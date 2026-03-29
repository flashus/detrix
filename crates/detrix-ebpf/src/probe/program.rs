//! BPF program generation from DWARF-resolved variable locations
//!
//! Generates BPF C source code that reads variables at a specific
//! program counter and submits them to a ring buffer. Each logpoint
//! gets its own tiny BPF program with hardcoded register/stack offsets.

use crate::dwarf::types::{ResolvedVariable, VariableLocation};
use crate::error::{Error, Result};
use crate::probe::types::MAX_CAPTURE_VARS;

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
/// Returns error if too many variables are requested (max 8).
pub fn generate_bpf_program(
    variables: &[ResolvedVariable],
    capture_goid: bool,
) -> Result<BpfProgram> {
    if variables.len() > MAX_CAPTURE_VARS {
        return Err(Error::Ebpf(format!(
            "Too many variables: {} (max {MAX_CAPTURE_VARS})",
            variables.len()
        )));
    }

    let var_count = variables.len();
    let mut source = String::with_capacity(2048);

    // Header
    source.push_str(PROGRAM_HEADER);

    // Event struct
    generate_event_struct(&mut source, variables, capture_goid);

    // Ring buffer map
    source.push_str(RINGBUF_MAP);

    // Probe function
    generate_probe_function(&mut source, variables, capture_goid);

    Ok(BpfProgram {
        source,
        var_count,
        captures_goid: capture_goid,
    })
}

/// Generate the read instruction for a single variable location.
///
/// Returns the C expression to read the variable value.
pub fn generate_read_expr(var: &ResolvedVariable, idx: usize) -> String {
    match &var.location {
        VariableLocation::Register(reg) => {
            format!("    event->var{idx} = (u64)ctx->{field};",
                field = reg.pt_regs_field())
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
            // Read pointer and length separately
            let ptr_read = match ptr.as_ref() {
                VariableLocation::Register(reg) => {
                    format!("    event->var{idx} = (u64)ctx->{};", reg.pt_regs_field())
                }
                VariableLocation::StackOffset { offset } => {
                    format!("    bpf_probe_read_user(&event->var{idx}, 8, (void *)(ctx->sp + ({offset})));")
                }
                _ => format!("    event->var{idx} = 0; // unsupported ptr location"),
            };
            let len_read = match len.as_ref() {
                VariableLocation::Register(reg) => {
                    format!("    event->var{}_len = (u64)ctx->{};", idx, reg.pt_regs_field())
                }
                VariableLocation::StackOffset { offset } => {
                    format!("    bpf_probe_read_user(&event->var{}_len, 8, (void *)(ctx->sp + ({offset})));", idx)
                }
                _ => format!("    event->var{}_len = 0; // unsupported len location", idx),
            };
            format!("{ptr_read}\n{len_read}")
        }
        VariableLocation::GoSlice { ptr, len, cap } => {
            // Similar to GoString but with cap
            let ptr_expr = simple_read_expr(ptr, &format!("var{idx}"));
            let len_expr = simple_read_expr(len, &format!("var{idx}_len"));
            let cap_expr = simple_read_expr(cap, &format!("var{idx}_cap"));
            format!("    {ptr_expr}\n    {len_expr}\n    {cap_expr}")
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

fn generate_event_struct(source: &mut String, variables: &[ResolvedVariable], capture_goid: bool) {
    source.push_str("struct probe_event {\n");
    source.push_str("    u32 pid;\n");
    source.push_str("    u32 tid;\n");
    if capture_goid {
        source.push_str("    u64 goid;\n");
    }
    source.push_str("    u64 timestamp;\n");

    for (i, var) in variables.iter().enumerate() {
        source.push_str(&format!("    u64 var{i}; // {name}: {loc}\n",
            name = var.name, loc = var.location));

        match &var.location {
            VariableLocation::GoString { .. } => {
                source.push_str(&format!("    u64 var{i}_len;\n"));
            }
            VariableLocation::GoSlice { .. } => {
                source.push_str(&format!("    u64 var{i}_len;\n"));
                source.push_str(&format!("    u64 var{i}_cap;\n"));
            }
            _ => {}
        }
    }

    source.push_str("};\n\n");
}

fn generate_probe_function(source: &mut String, variables: &[ResolvedVariable], capture_goid: bool) {
    source.push_str("SEC(\"uprobe\")\n");
    source.push_str("int detrix_capture(struct pt_regs *ctx) {\n");
    source.push_str("    struct probe_event *event;\n");
    source.push_str("    event = bpf_ringbuf_reserve(&events, sizeof(*event), 0);\n");
    source.push_str("    if (!event) return 0;\n\n");

    // PID/TID
    source.push_str("    u64 pid_tgid = bpf_get_current_pid_tgid();\n");
    source.push_str("    event->pid = pid_tgid >> 32;\n");
    source.push_str("    event->tid = (u32)pid_tgid;\n");
    source.push_str("    event->timestamp = bpf_ktime_get_ns();\n\n");

    if capture_goid {
        source.push_str(GOID_EXTRACT);
    }

    // Variable reads
    for (i, var) in variables.iter().enumerate() {
        source.push_str(&format!("    // {}: {}\n", var.name, var.type_name));
        source.push_str(&generate_read_expr(var, i));
        source.push('\n');
    }

    source.push_str("\n    bpf_ringbuf_submit(event, 0);\n");
    source.push_str("    return 0;\n");
    source.push_str("}\n");
}

const PROGRAM_HEADER: &str = r#"// Auto-generated by detrix-ebpf — do not edit
#include <linux/bpf.h>
#include <linux/ptrace.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "Dual MIT/GPL";

"#;

const RINGBUF_MAP: &str = r#"struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024); // 256 KB
} events SEC(".maps");

"#;

const GOID_EXTRACT: &str = r#"    // Extract goroutine ID from runtime.g struct
    // goid offset is version-dependent — baked in at load time
    u64 goid = 0;
    void *g_ptr = (void *)ctx->r14; // Go stores g in R14 (Go 1.17+)
    if (g_ptr) {
        bpf_probe_read_user(&goid, sizeof(goid), g_ptr + GOID_OFFSET);
    }
    event->goid = goid;

"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{Register, VariableLocation, VariableSize};

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
        let prog = generate_bpf_program(&[], false).unwrap();
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
        let prog = generate_bpf_program(&vars, false).unwrap();
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
        let prog = generate_bpf_program(&vars, false).unwrap();
        assert!(prog.source.contains("bpf_probe_read_user"));
        assert!(prog.source.contains("ctx->sp"));
    }

    #[test]
    fn generate_with_goid() {
        let prog = generate_bpf_program(&[], true).unwrap();
        assert!(prog.captures_goid);
        assert!(prog.source.contains("goid"));
        assert!(prog.source.contains("GOID_OFFSET"));
        assert!(prog.source.contains("r14"));
    }

    #[test]
    fn generate_multiple_vars() {
        let vars = vec![
            make_var("a", VariableLocation::Register(Register::Rax), VariableSize::QWord),
            make_var("b", VariableLocation::Register(Register::Rbx), VariableSize::QWord),
            make_var("c", VariableLocation::stack(8), VariableSize::DWord),
        ];
        let prog = generate_bpf_program(&vars, false).unwrap();
        assert_eq!(prog.var_count, 3);
        assert!(prog.source.contains("var0"));
        assert!(prog.source.contains("var1"));
        assert!(prog.source.contains("var2"));
    }

    #[test]
    fn generate_too_many_vars_fails() {
        let vars: Vec<_> = (0..=MAX_CAPTURE_VARS)
            .map(|i| make_var(
                &format!("v{i}"),
                VariableLocation::Register(Register::Rax),
                VariableSize::QWord,
            ))
            .collect();
        let result = generate_bpf_program(&vars, false);
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
        let prog = generate_bpf_program(&vars, false).unwrap();
        assert!(prog.source.contains("var0")); // ptr
        assert!(prog.source.contains("var0_len")); // len
    }

    #[test]
    fn generate_read_expr_register() {
        let var = make_var("x", VariableLocation::Register(Register::Rdx), VariableSize::QWord);
        let expr = generate_read_expr(&var, 0);
        assert_eq!(expr, "    event->var0 = (u64)ctx->rdx;");
    }

    #[test]
    fn generate_read_expr_stack_positive() {
        let var = make_var("x", VariableLocation::stack(16), VariableSize::DWord);
        let expr = generate_read_expr(&var, 2);
        assert!(expr.contains("ctx->sp + 16"));
        assert!(expr.contains("var2"));
        assert!(expr.contains(", 4,")); // DWord = 4 bytes
    }

    #[test]
    fn generate_read_expr_stack_negative() {
        let var = make_var("x", VariableLocation::stack(-8), VariableSize::QWord);
        let expr = generate_read_expr(&var, 0);
        assert!(expr.contains("ctx->sp - 8"));
    }

    #[test]
    fn program_has_license() {
        let prog = generate_bpf_program(&[], false).unwrap();
        assert!(prog.source.contains("LICENSE"));
        assert!(prog.source.contains("Dual MIT/GPL"));
    }

    #[test]
    fn program_has_ringbuf_map() {
        let prog = generate_bpf_program(&[], false).unwrap();
        assert!(prog.source.contains("BPF_MAP_TYPE_RINGBUF"));
        assert!(prog.source.contains("events"));
    }
}
