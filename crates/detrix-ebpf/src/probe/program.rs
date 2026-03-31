//! BPF program generation from DWARF-resolved variable locations
//!
//! Generates BPF C source code that reads variables at a specific
//! program counter and submits them to a ring buffer. Each logpoint
//! gets its own tiny BPF program with hardcoded register/stack offsets.

use crate::dwarf::types::{ResolvedVariable, VariableLocation};
use crate::error::{Error, Result};
use crate::probe::types::CaptureConfig;

/// The BPF C template with placeholders for dynamic sections.
/// Loaded at compile time from `probe_template.c` — edit that file to change
/// the template, then verify it compiles standalone with clang -target bpf.
const PROBE_TEMPLATE: &str = include_str!("probe_template.c");

/// Generated BPF program source (C code) ready for compilation.
#[derive(Debug, Clone)]
pub struct BpfProgram {
    /// The C source code for the BPF program.
    pub source: String,
    /// Number of variables this program captures.
    pub var_count: usize,
    /// Whether goroutine ID extraction is included.
    pub captures_goid: bool,
}

/// Generate a BPF C program that captures the given variables at a uprobe hit.
///
/// The generated program:
/// 1. Reads each variable from its register or stack location
/// 2. Packs values into a struct
/// 3. Submits to a ring buffer for user-space consumption
///
/// # Errors
/// Returns error if too many variables are requested (exceeds `config.max_capture_vars`).
pub fn generate_bpf_program(
    variables: &[ResolvedVariable],
    capture_goid: bool,
    config: &CaptureConfig,
) -> Result<BpfProgram> {
    if variables.len() > config.max_capture_vars {
        return Err(Error::Ebpf(format!(
            "Too many variables: {} (max {})",
            variables.len(),
            config.max_capture_vars,
        )));
    }

    let var_count = variables.len();

    // Build the two dynamic sections, then substitute into the template.
    let event_fields = build_event_fields(variables, capture_goid, config);
    let var_reads = build_var_reads(variables, capture_goid, config);

    let source = PROBE_TEMPLATE
        .replace("/*DETRIX_EVENT_FIELDS*/", &event_fields)
        .replace("/*DETRIX_VAR_READS*/", &var_reads);

    Ok(BpfProgram {
        source,
        var_count,
        captures_goid: capture_goid,
    })
}

/// Build the dynamic event struct fields for the `/*DETRIX_EVENT_FIELDS*/` placeholder.
///
/// Generates `u64 varN; // name: loc` lines plus extra fields for strings/slices.
/// Optional `goid` field prepended when `capture_goid` is true.
fn build_event_fields(
    variables: &[ResolvedVariable],
    capture_goid: bool,
    config: &CaptureConfig,
) -> String {
    let mut out = String::new();

    if capture_goid {
        out.push_str("    u64 goid;\n");
    }

    for (i, var) in variables.iter().enumerate() {
        out.push_str(&format!(
            "    u64 var{i}; // {name}: {loc}\n",
            name = var.name,
            loc = var.location
        ));

        match &var.location {
            VariableLocation::GoString { .. } => {
                out.push_str(&format!("    u64 var{i}_len;\n"));
                // Fixed-size buffer for string content — filled by bpf_probe_read_user.
                // Ring buffer parser reads exactly max_string_capture bytes after len.
                out.push_str(&format!(
                    "    u8  var{i}_str[{}];\n",
                    config.max_string_capture
                ));
            }
            VariableLocation::GoSlice { .. } => {
                out.push_str(&format!("    u64 var{i}_len;\n"));
                out.push_str(&format!("    u64 var{i}_cap;\n"));
            }
            VariableLocation::StackBlob { byte_size, .. } => {
                let capture = (*byte_size).min(config.max_blob_capture);
                out.push_str(&format!("    u8  var{i}_blob[{capture}];\n"));
            }
            _ => {}
        }
    }

    out
}

/// Build the variable read instructions for the `/*DETRIX_VAR_READS*/` placeholder.
///
/// Generates goroutine ID extraction (when requested) followed by per-variable
/// register or stack reads.
fn build_var_reads(
    variables: &[ResolvedVariable],
    capture_goid: bool,
    config: &CaptureConfig,
) -> String {
    let mut out = String::new();

    if capture_goid {
        out.push_str(GOID_EXTRACT);
    }

    for (i, var) in variables.iter().enumerate() {
        out.push_str(&format!("    // {}: {}\n", var.name, var.type_name));
        out.push_str(&generate_read_expr(var, i, config));
        out.push('\n');
    }

    out
}

/// Generate the read instruction for a single variable location.
///
/// Returns the C expression to read the variable value.
pub fn generate_read_expr(var: &ResolvedVariable, idx: usize, config: &CaptureConfig) -> String {
    match &var.location {
        VariableLocation::Register(reg) => {
            format!(
                "    event->var{idx} = (u64)ctx->{field};",
                field = reg.pt_regs_field()
            )
        }
        VariableLocation::StackOffset { offset } => {
            let size = var.size.bytes();
            if *offset >= 0 {
                format!(
                    "    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(ctx->sp + {offset}));"
                )
            } else {
                format!(
                    "    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(ctx->sp - {}));",
                    offset.unsigned_abs()
                )
            }
        }
        VariableLocation::GoString { ptr, len } => {
            // Go string struct {ptr uintptr, len int} lives on the stack.
            // Step 1: read the ptr and len fields from their DWARF locations.
            // Step 2: dereference ptr to read the actual string bytes.
            //   - For string literals: ptr points into .rodata, accessible via bpf_probe_read_user.
            //   - For heap strings: ptr points into Go's heap; BPF may fail; user-space
            //     mem_reader will fall back to process_vm_readv when var{idx}_str is empty.

            let ptr_location = ptr.as_ref();
            let len_location = len.as_ref();

            // Log the exact offsets so we can diagnose wrong stack reads.
            detrix_logging::info!(
                "[BPF gen] GoString var{idx} ({}): ptr={ptr_location:?} len={len_location:?}",
                var.name,
            );

            // Read pointer from its location
            let ptr_read = match ptr_location {
                VariableLocation::StackOffset { offset } => {
                    format!("    bpf_probe_read_user(&event->var{idx}, 8, (void *)(ctx->sp + ({offset})));")
                }
                VariableLocation::Register(reg) => {
                    format!("    event->var{idx} = (u64)ctx->{};", reg.pt_regs_field())
                }
                _ => format!("    event->var{idx} = 0;"),
            };

            // Read length from its location
            let len_read = match len_location {
                VariableLocation::StackOffset { offset } => {
                    format!("    bpf_probe_read_user(&event->var{idx}_len, 8, (void *)(ctx->sp + ({offset})));")
                }
                VariableLocation::Register(reg) => {
                    format!("    event->var{idx}_len = (u64)ctx->{};", reg.pt_regs_field())
                }
                _ => format!("    event->var{idx}_len = 0;"),
            };

            // Dereference ptr → fill the content buffer.
            //
            // BPF verifier requires the `size` argument of bpf_probe_read_user to be
            // provably bounded.  The `var &= const` pattern is the canonical way:
            //   1. mask _len with a power-of-two - 1 to give the verifier an upper bound,
            //   2. then guard with > 0 so we don't call with size 0.
            // We use mask = MAX_STRING_CAPTURE (which must be a power of two) so that
            // masking is both the truncation AND the verifier hint in one step.
            let max = config.max_string_capture;
            let content_read = format!(
                "    __builtin_memset(event->var{idx}_str, 0, {max});\
\n    {{\
\n        u32 _len{idx} = (u32)event->var{idx}_len;\
\n        _len{idx} &= 0xFF;  /* bound to [0,255] — verifier hint */\
\n        if (event->var{idx} && _len{idx} > 0 && _len{idx} <= {max}) {{\
\n            bpf_probe_read_user(event->var{idx}_str, _len{idx}, (void *)event->var{idx});\
\n        }}\
\n    }}"
            );

            format!("{ptr_read}\n{len_read}\n{content_read}")
        }
        VariableLocation::GoSlice { ptr, len, cap } => {
            // Read ptr, len, cap from their sub-locations.
            let ptr_expr = simple_read_expr(ptr, &format!("var{idx}"));
            let len_expr = simple_read_expr(len, &format!("var{idx}_len"));
            let cap_expr = simple_read_expr(cap, &format!("var{idx}_cap"));
            format!("    {ptr_expr}\n    {len_expr}\n    {cap_expr}")
        }
        VariableLocation::StackBlob { offset, byte_size } => {
            // Read raw bytes from stack into the fixed blob buffer.
            // var{idx} (u64) is left as 0; actual data goes in var{idx}_blob[N].
            let capture = (*byte_size).min(config.max_blob_capture);
            if *offset >= 0 {
                format!("    bpf_probe_read_user(event->var{idx}_blob, {capture}, (void *)(ctx->sp + {offset}));")
            } else {
                format!(
                    "    bpf_probe_read_user(event->var{idx}_blob, {capture}, (void *)(ctx->sp - {}));",
                    offset.unsigned_abs()
                )
            }
        }
    }
}

fn simple_read_expr(loc: &VariableLocation, field: &str) -> String {
    match loc {
        VariableLocation::Register(reg) => {
            format!("event->{field} = (u64)ctx->{};", reg.pt_regs_field())
        }
        VariableLocation::StackOffset { offset } => {
            format!("bpf_probe_read_user(&event->{field}, 8, (void *)(ctx->sp + ({offset})));")
        }
        _ => format!("event->{field} = 0; // unsupported"),
    }
}

const GOID_EXTRACT: &str = r#"    // Extract goroutine ID from runtime.g struct.
    // goid offset is Go-version-dependent — define GOID_OFFSET via -D at compile time.
    // Go stores g in R14 (x86-64) or X28/R28 (arm64) per runtime/asm_ARCH.s.
#ifndef GOID_OFFSET
#define GOID_OFFSET 152  // Go 1.21+ default; override via -DGOID_OFFSET=N
#endif
    u64 goid = 0;
#if defined(__TARGET_ARCH_arm64)
    void *g_ptr = (void *)ctx->regs[28]; // arm64: Go uses X28 for g
#else
    void *g_ptr = (void *)ctx->r14;      // x86-64: Go uses R14 for g
#endif
    if (g_ptr) {
        bpf_probe_read_user(&goid, sizeof(goid), g_ptr + GOID_OFFSET);
    }
    event->goid = goid;

"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{Register, VariableLocation, VariableSize};
    use crate::probe::types::{CaptureConfig, MAX_CAPTURE_VARS, MAX_STRING_CAPTURE};

    fn make_var(name: &str, loc: VariableLocation, size: VariableSize) -> ResolvedVariable {
        ResolvedVariable {
            name: name.to_string(),
            location: loc,
            size,
            type_name: "int64".to_string(),
        }
    }

    #[test]
    fn generate_empty_program() {
        let prog = generate_bpf_program(&[], false, &CaptureConfig::default()).unwrap();
        assert_eq!(prog.var_count, 0);
        assert!(!prog.captures_goid);
        assert!(prog.source.contains("detrix_capture"));
        assert!(prog.source.contains("bpf_ringbuf_submit"));
    }

    #[test]
    fn generate_single_register_var() {
        let vars = vec![make_var(
            "amount",
            VariableLocation::Register(Register::Rax),
            VariableSize::QWord,
        )];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        assert_eq!(prog.var_count, 1);
        assert!(prog.source.contains("ctx->rax"));
        assert!(prog.source.contains("var0"));
        assert!(prog.source.contains("amount: int64"));
    }

    #[test]
    fn generate_stack_var() {
        let vars = vec![make_var(
            "count",
            VariableLocation::stack(-16),
            VariableSize::DWord,
        )];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("bpf_probe_read_user"));
        assert!(prog.source.contains("ctx->sp"));
    }

    #[test]
    fn generate_with_goid() {
        let prog = generate_bpf_program(&[], true, &CaptureConfig::default()).unwrap();
        assert!(prog.captures_goid);
        assert!(prog.source.contains("goid"));
        assert!(prog.source.contains("GOID_OFFSET"));
        // g register is arch-specific; check both are present in the arch-guarded block
        assert!(prog.source.contains("r14") || prog.source.contains("regs[28]"));
    }

    #[test]
    fn generate_multiple_vars() {
        let vars = vec![
            make_var(
                "a",
                VariableLocation::Register(Register::Rax),
                VariableSize::QWord,
            ),
            make_var(
                "b",
                VariableLocation::Register(Register::Rbx),
                VariableSize::QWord,
            ),
            make_var("c", VariableLocation::stack(8), VariableSize::DWord),
        ];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        assert_eq!(prog.var_count, 3);
        assert!(prog.source.contains("var0"));
        assert!(prog.source.contains("var1"));
        assert!(prog.source.contains("var2"));
    }

    #[test]
    fn generate_too_many_vars_fails() {
        let vars: Vec<_> = (0..=MAX_CAPTURE_VARS)
            .map(|i| {
                make_var(
                    &format!("v{i}"),
                    VariableLocation::Register(Register::Rax),
                    VariableSize::QWord,
                )
            })
            .collect();
        let result = generate_bpf_program(&vars, false, &CaptureConfig::default());
        assert!(result.is_err());
    }

    #[test]
    fn generate_go_string_var() {
        let vars = vec![ResolvedVariable {
            name: "name".to_string(),
            location: VariableLocation::GoString {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
            },
            size: VariableSize::QWord,
            type_name: "string".to_string(),
        }];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("var0"));     // ptr field
        assert!(prog.source.contains("var0_len")); // len field
        assert!(prog.source.contains("var0_str")); // content buffer field
        assert!(prog.source.contains("bpf_probe_read_user")); // content read
    }

    #[test]
    fn go_string_event_struct_has_str_buffer() {
        let vars = vec![ResolvedVariable {
            name: "symbol".to_string(),
            location: VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(-240)),
                len: Box::new(VariableLocation::stack(-232)),
            },
            size: VariableSize::QWord,
            type_name: "string".to_string(),
        }];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        // Struct must have the fixed-size string buffer
        assert!(
            prog.source.contains("var0_str["),
            "event struct missing var0_str[] buffer"
        );
        // Size must match MAX_STRING_CAPTURE
        assert!(
            prog.source.contains(&format!("var0_str[{}]", MAX_STRING_CAPTURE)),
            "str buffer size does not match MAX_STRING_CAPTURE"
        );
    }

    #[test]
    fn go_string_bpf_reads_content_via_probe_read() {
        let vars = vec![ResolvedVariable {
            name: "symbol".to_string(),
            location: VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(-240)),
                len: Box::new(VariableLocation::stack(-232)),
            },
            size: VariableSize::QWord,
            type_name: "string".to_string(),
        }];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        // The probe function must dereference the ptr to fill the str buffer
        assert!(
            prog.source.contains("bpf_probe_read_user(event->var0_str"),
            "missing bpf_probe_read_user for string content"
        );
    }

    #[test]
    fn generate_read_expr_register() {
        let var = make_var(
            "x",
            VariableLocation::Register(Register::Rdx),
            VariableSize::QWord,
        );
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert_eq!(expr, "    event->var0 = (u64)ctx->rdx;");
    }

    #[test]
    fn generate_read_expr_stack_positive() {
        let var = make_var("x", VariableLocation::stack(16), VariableSize::DWord);
        let expr = generate_read_expr(&var, 2, &CaptureConfig::default());
        assert!(expr.contains("ctx->sp + 16"));
        assert!(expr.contains("var2"));
        assert!(expr.contains(", 4,")); // DWord = 4 bytes
    }

    #[test]
    fn generate_read_expr_stack_negative() {
        let var = make_var("x", VariableLocation::stack(-8), VariableSize::QWord);
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(expr.contains("ctx->sp - 8"));
    }

    #[test]
    fn program_has_license() {
        let prog = generate_bpf_program(&[], false, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("LICENSE"));
        assert!(prog.source.contains("Dual MIT/GPL"));
    }

    #[test]
    fn program_has_ringbuf_map() {
        let prog = generate_bpf_program(&[], false, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("BPF_MAP_TYPE_RINGBUF"));
        assert!(prog.source.contains("DETRIX_EVENTS"));
    }

    #[test]
    fn generate_stack_blob_event_struct_has_blob_buffer() {
        let vars = vec![ResolvedVariable {
            name: "req".to_string(),
            location: VariableLocation::StackBlob {
                offset: -48,
                byte_size: 32,
            },
            size: VariableSize::QWord,
            type_name: "TradeRequest".to_string(),
        }];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        // Struct must have the fixed-size blob buffer
        assert!(
            prog.source.contains("var0_blob["),
            "event struct missing var0_blob[] buffer"
        );
        // Size must be min(byte_size, max_blob_capture) = min(32, 64) = 32
        assert!(
            prog.source.contains("var0_blob[32]"),
            "blob buffer size should be 32 (byte_size)"
        );
    }

    #[test]
    fn generate_stack_blob_clamped_to_max_blob_capture() {
        let vars = vec![ResolvedVariable {
            name: "huge".to_string(),
            location: VariableLocation::StackBlob {
                offset: -256,
                byte_size: 256,
            },
            size: VariableSize::QWord,
            type_name: "[32]int64".to_string(),
        }];
        let config = CaptureConfig {
            max_capture_vars: 8,
            max_string_capture: 64,
            max_blob_capture: 64,
        };
        let prog = generate_bpf_program(&vars, false, &config).unwrap();
        // Clamped: min(256, 64) = 64
        assert!(prog.source.contains("var0_blob[64]"));
    }

    #[test]
    fn generate_read_expr_stack_blob() {
        let var = ResolvedVariable {
            name: "req".to_string(),
            location: VariableLocation::StackBlob {
                offset: -32,
                byte_size: 16,
            },
            size: VariableSize::QWord,
            type_name: "TradeRequest".to_string(),
        };
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(
            expr.contains("bpf_probe_read_user(event->var0_blob"),
            "missing bpf_probe_read_user for blob"
        );
        assert!(expr.contains("ctx->sp - 32"));
        assert!(expr.contains(", 16,"), "capture size should be 16");
    }

    #[test]
    fn generate_go_slice_var() {
        let vars = vec![ResolvedVariable {
            name: "items".to_string(),
            location: VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
                cap: Box::new(VariableLocation::Register(Register::Rcx)),
            },
            size: VariableSize::QWord,
            type_name: "[]int64".to_string(),
        }];
        let prog = generate_bpf_program(&vars, false, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("var0_len"));
        assert!(prog.source.contains("var0_cap"));
    }
}
