//! BPF program generation from DWARF-resolved variable locations
//!
//! Generates BPF C source code that reads variables at a specific
//! program counter and submits them to a ring buffer. Each logpoint
//! gets its own tiny BPF program with hardcoded register/stack offsets.
//!
//! # Nested Type Capture (Hybrid Strategy)
//!
//! BPF has a 512-byte stack limit, so we use depth-limited capture:
//! - **Depth 0-1**: BPF reads struct fields inline into ring buffer
//! - **Depth 2+**: User-space follows pointers recursively (future)
//!
//! Configuration via `CaptureConfig`:
//! - `max_capture_depth`: Recursion depth (default: 2)
//! - `max_struct_fields`: Max fields per struct (-1 = all)
//! - `max_array_values`: Max array/slice elements (default: 64)

use crate::capture_plan::{CaptureField, CapturePlan, HeaderKind, ReadOp};
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
    /// TLS offset for reading the G pointer (x86_64 only; None for ARM64 or disabled).
    pub g_addr_offset: Option<i64>,
    /// Byte offset of goid field within runtime.g (from DWARF; None = use #ifndef default).
    pub goid_offset: Option<u64>,
    /// Whether this program emits the fixed-size DRX1 raw envelope header.
    pub versioned_envelope: bool,
}

/// Compact identity used in the fixed-size kernel record header. The full
/// profile/plan strings remain in the user-space `EventEnvelope`; the kernel
/// only needs bounded tags for stale-record rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEnvelopeSpec {
    pub profile_tag: u32,
    pub plan_tag: u64,
}

pub const RAW_ENVELOPE_MAGIC: u32 = u32::from_le_bytes(*b"DRX1");
pub const RAW_ENVELOPE_SCHEMA: u16 = 1;
pub const RAW_ENVELOPE_SIZE: usize = 24;

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
    g_addr_offset: Option<i64>,
    goid_offset: Option<u64>,
    config: &CaptureConfig,
) -> Result<BpfProgram> {
    generate_bpf_program_with_envelope(
        variables,
        capture_goid,
        g_addr_offset,
        goid_offset,
        config,
        None,
    )
}

pub fn generate_bpf_program_with_envelope(
    variables: &[ResolvedVariable],
    capture_goid: bool,
    g_addr_offset: Option<i64>,
    goid_offset: Option<u64>,
    config: &CaptureConfig,
    envelope: Option<RawEnvelopeSpec>,
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
    let event_fields = build_event_fields(variables, capture_goid, config, envelope);
    let var_reads = build_var_reads(variables, capture_goid, config, envelope);

    let source = PROBE_TEMPLATE
        .replace("/*DETRIX_EVENT_FIELDS*/", &event_fields)
        .replace("/*DETRIX_VAR_READS*/", &var_reads);

    Ok(BpfProgram {
        source,
        var_count,
        captures_goid: capture_goid,
        g_addr_offset,
        goid_offset,
        versioned_envelope: envelope.is_some(),
    })
}

/// Generate a BPF program directly from the validated language-neutral plan.
///
/// This is the authoritative renderer for plan-native captures.  It keeps
/// Go's legacy `generate_bpf_program` API available for compatibility, but
/// avoids converting a plan back into `ResolvedVariable` locations for live
/// Go/Rust probes.  Every operation is bounded by `CapturePlan::validate`.
pub fn generate_bpf_program_from_plan(
    plan: &CapturePlan,
    capture_goid: bool,
    g_addr_offset: Option<i64>,
    goid_offset: Option<u64>,
    config: &CaptureConfig,
    envelope: Option<RawEnvelopeSpec>,
) -> Result<BpfProgram> {
    plan.validate()
        .map_err(|error| Error::Ebpf(format!("invalid capture plan: {error}")))?;
    if plan.fields.len() > config.max_capture_vars {
        return Err(Error::Ebpf(format!(
            "Too many variables: {} (max {})",
            plan.fields.len(),
            config.max_capture_vars
        )));
    }

    let event_fields = build_plan_event_fields(&plan.fields, capture_goid, config, envelope);
    let var_reads = build_plan_var_reads(&plan.fields, capture_goid, config, envelope)?;
    let source = PROBE_TEMPLATE
        .replace("/*DETRIX_EVENT_FIELDS*/", &event_fields)
        .replace("/*DETRIX_VAR_READS*/", &var_reads);

    Ok(BpfProgram {
        source,
        var_count: plan.fields.len(),
        captures_goid: capture_goid,
        g_addr_offset,
        goid_offset,
        versioned_envelope: envelope.is_some(),
    })
}

fn build_plan_event_fields(
    fields: &[CaptureField],
    capture_goid: bool,
    config: &CaptureConfig,
    envelope: Option<RawEnvelopeSpec>,
) -> String {
    let mut out = String::new();
    if envelope.is_some() {
        out.push_str("    u32 drx_magic;\n");
        out.push_str("    u16 drx_schema;\n");
        out.push_str("    u16 drx_field_count;\n");
        out.push_str("    u32 drx_payload_len;\n");
        out.push_str("    u32 drx_profile_tag;\n");
        out.push_str("    u64 drx_plan_tag;\n");
    }
    if capture_goid {
        out.push_str("    u64 goid;\n");
    }
    for (index, field) in fields.iter().enumerate() {
        out.push_str(&format!("    u64 var{index}; // {}: plan-op\n", field.name));
        match &field.op {
            ReadOp::Header { kind, .. } => match kind {
                HeaderKind::String | HeaderKind::RustString | HeaderKind::BorrowedStr => {
                    out.push_str(&format!("    u64 var{index}_len;\n"));
                    let size = config.max_string_capture.min(255);
                    out.push_str(&format!("    u8 var{index}_str[{size}];\n"));
                }
                HeaderKind::Slice | HeaderKind::RustVec | HeaderKind::BorrowedSlice => {
                    out.push_str(&format!("    u64 var{index}_len;\n"));
                    out.push_str(&format!("    u64 var{index}_cap;\n"));
                }
            },
            ReadOp::Blob { size, .. } => {
                let size = (*size).min(config.max_blob_capture);
                out.push_str(&format!("    u8 var{index}_blob[{size}];\n"));
            }
            ReadOp::Piecewise { .. } => {
                let size = field.size.min(config.max_blob_capture);
                out.push_str(&format!("    u8 var{index}_blob[{size}];\n"));
            }
            ReadOp::Piece { .. } => {
                let size = field.size.min(config.max_blob_capture);
                out.push_str(&format!("    u8 var{index}_blob[{size}];\n"));
            }
            _ => {}
        }
    }
    out
}

fn build_plan_var_reads(
    fields: &[CaptureField],
    capture_goid: bool,
    config: &CaptureConfig,
    envelope: Option<RawEnvelopeSpec>,
) -> Result<String> {
    let mut out = String::new();
    if let Some(envelope) = envelope {
        out.push_str(&format!(
            "    event->drx_magic = {:#x};\n    event->drx_schema = {};\n    event->drx_field_count = {};\n    event->drx_payload_len = sizeof(*event) - {};\n    event->drx_profile_tag = {:#x};\n    event->drx_plan_tag = {:#x};\n",
            RAW_ENVELOPE_MAGIC,
            RAW_ENVELOPE_SCHEMA,
            fields.len(),
            24 + RAW_ENVELOPE_SIZE,
            envelope.profile_tag,
            envelope.plan_tag,
        ));
    }
    if capture_goid {
        out.push_str(GOID_EXTRACT);
    }
    for (index, field) in fields.iter().enumerate() {
        out.push_str(&format!("    // {}: plan-op\n", field.name));
        out.push_str(&generate_plan_read_expr(
            &field.op, index, field.size, config,
        )?);
        out.push('\n');
    }
    Ok(out)
}

fn generate_plan_read_expr(
    op: &ReadOp,
    index: usize,
    field_size: usize,
    config: &CaptureConfig,
) -> Result<String> {
    let value = format!("event->var{index}");
    match op {
        ReadOp::Register { register, .. } => {
            Ok(format!("    {value} = (u64){};", register.pt_regs_access()))
        }
        ReadOp::Stack { offset, size, .. } => {
            Ok(plan_memory_read(&value, "DETRIX_STACK_PTR", *offset, *size))
        }
        ReadOp::Frame {
            register,
            offset,
            size,
            ..
        } => Ok(plan_memory_read(
            &value,
            &register.pt_regs_access(),
            *offset,
            *size,
        )),
        ReadOp::Blob { offset, size, .. } => {
            let size = (*size).min(config.max_blob_capture);
            Ok(format!(
                "    bpf_probe_read_user(event->var{index}_blob, {size}, (void *){});",
                plan_address("DETRIX_STACK_PTR", *offset)
            ))
        }
        ReadOp::Indirect { base, .. } | ReadOp::Map { base } => {
            Ok(generate_plan_read_into(base, &value, 8)?)
        }
        ReadOp::Header { base, kind, .. } => {
            let (ptr_delta, len_delta, cap_delta) = match kind {
                HeaderKind::String | HeaderKind::BorrowedStr => (0, 8, None),
                HeaderKind::RustString => (8, 16, None),
                HeaderKind::Slice => (0, 8, Some(16)),
                HeaderKind::RustVec => (0, 16, Some(16)),
                HeaderKind::BorrowedSlice => (0, 8, Some(8)),
            };
            let mut out = String::new();
            out.push_str(&generate_plan_read_into(
                &shift_plan_op(base, ptr_delta),
                &value,
                8,
            )?);
            out.push('\n');
            let len = format!("event->var{index}_len");
            out.push_str(&generate_plan_read_into(
                &shift_plan_op(base, len_delta),
                &len,
                8,
            )?);
            if let Some(cap_delta) = cap_delta {
                out.push('\n');
                let cap = format!("event->var{index}_cap");
                out.push_str(&generate_plan_read_into(
                    &shift_plan_op(base, cap_delta),
                    &cap,
                    8,
                )?);
            } else {
                let max = config.max_string_capture.min(255);
                out.push_str(&format!(
                    "\n    __builtin_memset(event->var{index}_str, 0, {max});\n    {{\n        u32 _len{index} = (u32)event->var{index}_len;\n        _len{index} &= 0xFF;\n        if (event->var{index} && _len{index} > 0 && _len{index} <= {max}) {{\n            bpf_probe_read_user(event->var{index}_str, _len{index}, (void *)event->var{index});\n        }}\n    }}"
                ));
            }
            Ok(out)
        }
        ReadOp::Piecewise { pieces } => {
            let capture = field_size.min(config.max_blob_capture);
            let mut out = format!("    __builtin_memset(event->var{index}_blob, 0, {capture});");
            for piece in pieces {
                if piece.offset >= capture {
                    continue;
                }
                let size = piece.size.min(capture - piece.offset);
                if let Some(piece_op) = &piece.op {
                    out.push('\n');
                    out.push_str(&generate_plan_read_into(
                        piece_op,
                        &format!("event->var{index}_blob[{}]", piece.offset),
                        size,
                    )?);
                }
            }
            Ok(out)
        }
        ReadOp::Piece { offset, size, op } => {
            let size = (*size).min(field_size.saturating_sub(*offset));
            generate_plan_read_into(op, &format!("event->var{index}_blob[{offset}]"), size)
        }
        ReadOp::Unavailable { .. } => Ok(format!("    {value} = 0;")),
        ReadOp::Constant { bytes } => {
            let mut constant = 0u64;
            for (shift, byte) in bytes.iter().take(8).enumerate() {
                constant |= u64::from(*byte) << (shift * 8);
            }
            Ok(format!("    {value} = {constant:#x};"))
        }
        ReadOp::Cfa { .. } | ReadOp::UserBytes { .. } => Err(Error::Ebpf(
            "capture plan operation requires user-space lowering".into(),
        )),
    }
}

fn generate_plan_read_into(op: &ReadOp, destination: &str, size: usize) -> Result<String> {
    if size == 0 {
        return Err(Error::Ebpf(
            "capture plan contains a zero-sized read".into(),
        ));
    }
    match op {
        ReadOp::Register { register, .. } => Ok(format!(
            "    {destination} = (u64){};",
            register.pt_regs_access()
        )),
        ReadOp::Stack { offset, .. } => Ok(plan_memory_read(
            destination,
            "DETRIX_STACK_PTR",
            *offset,
            size,
        )),
        ReadOp::Frame {
            register, offset, ..
        } => Ok(plan_memory_read(
            destination,
            &register.pt_regs_access(),
            *offset,
            size,
        )),
        ReadOp::Indirect { base, .. } | ReadOp::Map { base } => {
            generate_plan_read_into(base, destination, size)
        }
        _ => Err(Error::Ebpf(format!(
            "unsupported nested plan read operation: {op:?}"
        ))),
    }
}

fn plan_memory_read(destination: &str, base: &str, offset: i64, size: usize) -> String {
    format!(
        "    {destination} = 0;\n    bpf_probe_read_user(&{destination}, {size}, (void *){});",
        plan_address(base, offset)
    )
}

fn plan_address(base: &str, offset: i64) -> String {
    if offset >= 0 {
        format!("({base} + {offset})")
    } else {
        format!("({base} - {})", offset.unsigned_abs())
    }
}

fn shift_plan_op(op: &ReadOp, delta: i64) -> ReadOp {
    match op {
        ReadOp::Stack {
            offset,
            size,
            semantics,
        } => ReadOp::Stack {
            offset: offset.saturating_add(delta),
            size: *size,
            semantics: *semantics,
        },
        ReadOp::Frame {
            register,
            offset,
            size,
            semantics,
        } => ReadOp::Frame {
            register: *register,
            offset: offset.saturating_add(delta),
            size: *size,
            semantics: *semantics,
        },
        other => other.clone(),
    }
}

/// Build the dynamic event struct fields for the `/*DETRIX_EVENT_FIELDS*/` placeholder.
///
/// Generates `u64 varN; // name: loc` lines plus extra fields for strings/slices.
/// Optional `goid` field prepended when `capture_goid` is true.
fn build_event_fields(
    variables: &[ResolvedVariable],
    capture_goid: bool,
    config: &CaptureConfig,
    envelope: Option<RawEnvelopeSpec>,
) -> String {
    let mut out = String::new();

    if envelope.is_some() {
        out.push_str("    u32 drx_magic;\n");
        out.push_str("    u16 drx_schema;\n");
        out.push_str("    u16 drx_field_count;\n");
        out.push_str("    u32 drx_payload_len;\n");
        out.push_str("    u32 drx_profile_tag;\n");
        out.push_str("    u64 drx_plan_tag;\n");
    }

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
            VariableLocation::GoString { .. } | VariableLocation::StringHeader { .. } => {
                out.push_str(&format!("    u64 var{i}_len;\n"));
                // Fixed-size buffer for string content — filled by bpf_probe_read_user.
                // Capped at 255 to match the verifier hint (_len &= 0xFF) in build_var_reads.
                // NOTE: Buffer boundary may split multi-byte UTF-8 characters.
                // User-space rejects invalid UTF-8 via String::from_utf8 (returns <read-failed>).
                // For full UTF-8 safety, keep max_string_capture as a multiple of 4
                // (max UTF-8 sequence is 4 bytes).
                let buf_size = config.max_string_capture.min(255);
                out.push_str(&format!("    u8  var{i}_str[{}];\n", buf_size));
            }
            VariableLocation::GoSlice { .. } | VariableLocation::SliceHeader { .. } => {
                out.push_str(&format!("    u64 var{i}_len;\n"));
                out.push_str(&format!("    u64 var{i}_cap;\n"));
            }
            VariableLocation::GoMap { .. } => {
                // Map pointer is already captured in var{i}. No extra fields needed.
                // User-space iterates the map structure via process_vm_readv.
            }
            VariableLocation::StackBlob { byte_size, .. }
            | VariableLocation::PiecewiseBlob { byte_size, .. } => {
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
    envelope: Option<RawEnvelopeSpec>,
) -> String {
    let mut out = String::new();

    if let Some(envelope) = envelope {
        out.push_str(&format!(
            "    event->drx_magic = {:#x};\n    event->drx_schema = {};\n    event->drx_field_count = {};\n    event->drx_payload_len = sizeof(*event) - {};\n    event->drx_profile_tag = {:#x};\n    event->drx_plan_tag = {:#x};\n",
            RAW_ENVELOPE_MAGIC,
            RAW_ENVELOPE_SCHEMA,
            variables.len(),
            24 + RAW_ENVELOPE_SIZE,
            envelope.profile_tag,
            envelope.plan_tag,
        ));
    }

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
            format!("    event->var{idx} = (u64){};", reg.pt_regs_access())
        }
        VariableLocation::StackOffset { offset } => {
            let size = var.size.bytes();
            // Zero-fill before read so failures leave zeros, not garbage.
            let zero_fill = format!("    event->var{idx} = 0;");
            let read = if *offset >= 0 {
                format!(
                    "    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(DETRIX_STACK_PTR + {offset}));"
                )
            } else {
                format!(
                    "    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(DETRIX_STACK_PTR - {}));",
                    offset.unsigned_abs()
                )
            };
            format!("{zero_fill}\n{read}")
        }
        VariableLocation::FrameOffset { register, offset } => {
            let size = var.size.bytes();
            let zero_fill = format!("    event->var{idx} = 0;");
            let base = register.pt_regs_access();
            let address = if *offset >= 0 {
                format!("({base} + {offset})")
            } else {
                format!("({base} - {})", offset.unsigned_abs())
            };
            let read =
                format!("    bpf_probe_read_user(&event->var{idx}, {size}, (void *){address});");
            format!("{zero_fill}\n{read}")
        }
        VariableLocation::GoString { ptr, len } | VariableLocation::StringHeader { ptr, len } => {
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
                    format!(
                        "    event->var{idx} = 0;\
\n    bpf_probe_read_user(&event->var{idx}, 8, (void *)(DETRIX_STACK_PTR + ({offset})));"
                    )
                }
                VariableLocation::Register(reg) => {
                    format!("    event->var{idx} = (u64){};", reg.pt_regs_access())
                }
                VariableLocation::FrameOffset { register, offset } => {
                    let base = register.pt_regs_access();
                    let address = if *offset >= 0 {
                        format!("({base} + {offset})")
                    } else {
                        format!("({base} - {})", offset.unsigned_abs())
                    };
                    format!("    event->var{idx} = 0;\n    bpf_probe_read_user(&event->var{idx}, 8, (void *){address});")
                }
                _ => format!("    event->var{idx} = 0;"),
            };

            // Read length from its location
            let len_read = match len_location {
                VariableLocation::StackOffset { offset } => {
                    format!(
                        "    event->var{idx}_len = 0;\
\n    bpf_probe_read_user(&event->var{idx}_len, 8, (void *)(DETRIX_STACK_PTR + ({offset})));"
                    )
                }
                VariableLocation::Register(reg) => {
                    format!("    event->var{idx}_len = (u64){};", reg.pt_regs_access())
                }
                VariableLocation::FrameOffset { register, offset } => {
                    let base = register.pt_regs_access();
                    let address = if *offset >= 0 {
                        format!("({base} + {offset})")
                    } else {
                        format!("({base} - {})", offset.unsigned_abs())
                    };
                    format!("    event->var{idx}_len = 0;\n    bpf_probe_read_user(&event->var{idx}_len, 8, (void *){address});")
                }
                _ => format!("    event->var{idx}_len = 0;"),
            };

            // Dereference ptr → fill the content buffer.
            //
            // BPF verifier requires the `size` argument of bpf_probe_read_user to be
            // provably bounded.  The `var &= const` pattern is the canonical way:
            //   1. mask _len with a power-of-two - 1 to give the verifier an upper bound,
            //   2. then guard with > 0 so we don't call with size 0.
            //
            // The mask is capped at 255 (0xFF) because BPF has limited stack space.
            // The ring buffer parser also enforces this limit, so strings longer than
            // 255 bytes are safely truncated at parse time.
            let max = config.max_string_capture.min(255);
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
        VariableLocation::GoSlice { ptr, len, cap }
        | VariableLocation::SliceHeader { ptr, len, cap } => {
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
                format!("    bpf_probe_read_user(event->var{idx}_blob, {capture}, (void *)(DETRIX_STACK_PTR + {offset}));")
            } else {
                format!(
                    "    bpf_probe_read_user(event->var{idx}_blob, {capture}, (void *)(DETRIX_STACK_PTR - {}));",
                    offset.unsigned_abs()
                )
            }
        }
        VariableLocation::PiecewiseBlob { pieces, byte_size } => {
            let capture = (*byte_size).min(config.max_blob_capture);
            let mut out = format!("    __builtin_memset(event->var{idx}_blob, 0, {capture});");
            let mut written = 0usize;

            for piece in pieces {
                if written >= capture {
                    break;
                }
                let piece_size = (capture - written).min(piece.byte_size);
                let Some(piece) = piece.location.as_ref() else {
                    written += piece.byte_size;
                    continue;
                };
                match piece {
                    VariableLocation::Register(reg) => {
                        for byte in 0..piece_size {
                            out.push_str(&format!(
                                "\n    event->var{idx}_blob[{}] = (u8)(((u64){}) >> {});",
                                written + byte,
                                reg.pt_regs_access(),
                                byte * 8
                            ));
                        }
                    }
                    VariableLocation::StackOffset { offset } => {
                        let address = if *offset >= 0 {
                            format!("DETRIX_STACK_PTR + {offset}")
                        } else {
                            format!("DETRIX_STACK_PTR - {}", offset.unsigned_abs())
                        };
                        out.push_str(&format!(
                            "\n    bpf_probe_read_user(&event->var{idx}_blob[{written}], {piece_size}, (void *)({address}));"
                        ));
                    }
                    VariableLocation::FrameOffset { register, offset } => {
                        let base = register.pt_regs_access();
                        let address = if *offset >= 0 {
                            format!("{base} + {offset}")
                        } else {
                            format!("{base} - {}", offset.unsigned_abs())
                        };
                        out.push_str(&format!(
                            "\n    bpf_probe_read_user(&event->var{idx}_blob[{written}], {piece_size}, (void *)({address}));"
                        ));
                    }
                    _ => unreachable!(
                        "PiecewiseBlob is constructed only from scalar register/stack/frame pieces"
                    ),
                }
                written += piece_size;
            }
            out
        }
        VariableLocation::StackIndirect { offset, .. } => {
            // Heap-escaped struct: read the 8-byte pointer from the stack slot.
            // User-space dereferences the pointer to read the actual struct bytes.
            // This produces the same BPF code as a plain StackOffset scalar read.
            let size = 8usize;
            if *offset >= 0 {
                format!("    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(DETRIX_STACK_PTR + {offset}));")
            } else {
                format!(
                    "    bpf_probe_read_user(&event->var{idx}, {size}, (void *)(DETRIX_STACK_PTR - {}));",
                    offset.unsigned_abs()
                )
            }
        }
        VariableLocation::GoMap { ptr } => {
            // Go map: capture the hmap pointer. User-space iterates the map.
            simple_read_expr(ptr, &format!("var{idx}"))
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Generate struct field capture for depth 0-1.
///
/// For struct-typed variables, we capture only the struct base address.
/// User-space will read the actual field values via process_vm_readv.
///
/// This follows the same pattern as string capture:
/// 1. BPF captures base address from stack
/// 2. User-space uses DWARF to get field offsets
/// 3. User-space reads each field: process_vm_readv(base_addr + field_offset)
#[allow(dead_code)]
fn generate_struct_field_reads(
    _var: &ResolvedVariable,
    _idx: usize,
    _config: &CaptureConfig,
    base_offset: i64,
) -> String {
    // For structs, we just capture the base address as a u64
    // User-space will read the actual fields
    format!("    // struct base address captured as u64 (user-space reads fields)\n    // Base offset from stack: {}\n", base_offset)
}

/// Generate struct field definitions for event struct.
///
/// For structs, we just store the base address (u64).
/// User-space reconstructs the full struct from DWARF info.
#[allow(dead_code)]
fn build_struct_field_definitions(
    _var: &ResolvedVariable,
    _idx: usize,
    _config: &CaptureConfig,
) -> String {
    // Structs are captured as a single u64 (base address)
    // The var{idx} field already exists from the main variable capture
    "    // struct captured as base address (user-space expands fields)\n".to_string()
}

fn simple_read_expr(loc: &VariableLocation, field: &str) -> String {
    match loc {
        VariableLocation::Register(reg) => {
            format!("event->{field} = (u64){};", reg.pt_regs_access())
        }
        VariableLocation::StackOffset { offset } => {
            format!(
                "bpf_probe_read_user(&event->{field}, 8, (void *)(DETRIX_STACK_PTR + ({offset})));"
            )
        }
        VariableLocation::FrameOffset { register, offset } => {
            let base = register.pt_regs_access();
            let address = if *offset >= 0 {
                format!("({base} + {offset})")
            } else {
                format!("({base} - {})", offset.unsigned_abs())
            };
            format!("bpf_probe_read_user(&event->{field}, 8, (void *){address});")
        }
        _ => format!("event->{field} = 0; // unsupported"),
    }
}

const GOID_EXTRACT: &str = r#"    // Extract goroutine ID (goid) from runtime.g struct.
    // ARM64 (non-cgo): X28 register holds G pointer directly (callee-saved, reliable).
    // x86-64: TLS-based approach using BPF CO-RE to read thread.fsbase from task_struct,
    //         then read G pointer from TLS + G_ADDR_OFFSET, then goid from G struct.
    // This mirrors Delve's approach — reliable at any instruction, not register-dependent.
#ifndef GOID_OFFSET
#define GOID_OFFSET 160  // Go 1.17+ default (param field added before atomicstatus/goid); override via -DGOID_OFFSET=N
#endif
#ifndef G_ADDR_OFFSET
#define G_ADDR_OFFSET -8  // fallback for pure Go (no PT_TLS); override via -DG_ADDR_OFFSET=N
#endif
    u64 goid = 0;
#if defined(__TARGET_ARCH_arm64)
    {
        // ARM64: Go uses X28 as callee-saved goroutine register.
        void *g_ptr = (void *)ctx->regs[28];
        if (g_ptr) {
            bpf_probe_read_user(&goid, sizeof(goid), g_ptr + GOID_OFFSET);
        }
    }
#else
    {
        // x86-64: TLS-based approach via BPF CO-RE.
        // Minimal struct definitions for CO-RE field relocation — the actual
        // offsets are resolved at load time from the kernel's BTF vmlinux.
        struct thread_struct___detrix { unsigned long fsbase; };
        struct task_struct___detrix   { struct thread_struct___detrix thread; };

        struct task_struct *task = (struct task_struct *)bpf_get_current_task();
        u64 fsbase = 0;
        if (task) {
            bpf_core_read(&fsbase, sizeof(fsbase),
                          &((struct task_struct___detrix *)task)->thread.fsbase);
        }
        if (fsbase) {
            void *g_ptr = NULL;
            bpf_probe_read_user(&g_ptr, sizeof(g_ptr), (void *)(fsbase + G_ADDR_OFFSET));
            if (g_ptr) {
                bpf_probe_read_user(&goid, sizeof(goid), (u8 *)g_ptr + GOID_OFFSET);
            }
        }
    }
#endif
    event->goid = goid;

"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_plan::{
        CaptureField, CapturePiece, CapturePlan, HeaderKind, ReadOp, ValueSemantics,
        CAPTURE_PLAN_SCHEMA_VERSION,
    };
    use crate::dwarf::types::TargetArchitecture;
    use crate::dwarf::types::{Register, VariableLocation, VariablePiece, VariableSize};
    use crate::probe::types::{CaptureConfig, MAX_CAPTURE_VARS, MAX_STRING_CAPTURE};

    fn make_var(name: &str, loc: VariableLocation, size: VariableSize) -> ResolvedVariable {
        ResolvedVariable {
            name: name.to_string(),
            location: loc,
            size,
            type_name: "int64".to_string(),
            nested_type: None,
        }
    }

    #[test]
    fn generate_empty_program() {
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();
        assert_eq!(prog.var_count, 0);
        assert!(!prog.captures_goid);
        assert!(prog.source.contains("detrix_capture"));
        assert!(prog.source.contains("bpf_ringbuf_submit"));
    }

    #[test]
    fn generate_versioned_envelope_program() {
        let vars = vec![make_var(
            "amount",
            VariableLocation::Register(Register::Rax),
            VariableSize::QWord,
        )];
        let prog = generate_bpf_program_with_envelope(
            &vars,
            false,
            None,
            None,
            &CaptureConfig::default(),
            Some(RawEnvelopeSpec {
                profile_tag: 0x7275,
                plan_tag: 0x1234,
            }),
        )
        .unwrap();
        assert!(prog.versioned_envelope);
        assert!(prog.source.contains("drx_magic"));
        assert!(prog.source.contains("drx_plan_tag = 0x1234"));
    }

    #[test]
    fn direct_plan_renderer_preserves_composite_operations() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "rust".into(),
            plan_hash: "sha256:composite".into(),
            probe_pc: 0x44,
            fields: vec![
                CaptureField {
                    name: "map".into(),
                    offset: 0,
                    size: 8,
                    op: ReadOp::Map {
                        base: Box::new(ReadOp::Stack {
                            offset: -40,
                            size: 8,
                            semantics: ValueSemantics::Value,
                        }),
                    },
                },
                CaptureField {
                    name: "header".into(),
                    offset: 8,
                    size: 16,
                    op: ReadOp::Header {
                        base: Box::new(ReadOp::Frame {
                            register: Register::Rbp,
                            offset: -32,
                            size: 8,
                            semantics: ValueSemantics::Address,
                        }),
                        size: 16,
                        kind: HeaderKind::BorrowedStr,
                    },
                },
                CaptureField {
                    name: "pieces".into(),
                    offset: 24,
                    size: 12,
                    op: ReadOp::Piecewise {
                        pieces: vec![
                            CapturePiece {
                                offset: 0,
                                size: 8,
                                op: Some(ReadOp::Register {
                                    register: Register::Rax,
                                    semantics: ValueSemantics::Value,
                                }),
                            },
                            CapturePiece {
                                offset: 8,
                                size: 4,
                                op: Some(ReadOp::Stack {
                                    offset: -16,
                                    size: 4,
                                    semantics: ValueSemantics::Value,
                                }),
                            },
                        ],
                    },
                },
            ],
            max_payload_bytes: 36,
        };

        let source = generate_bpf_program_from_plan(
            &plan,
            false,
            None,
            None,
            &CaptureConfig::default(),
            None,
        )
        .unwrap()
        .source;
        assert!(source.contains("DETRIX_STACK_PTR - 40"));
        assert!(source.contains("ctx->rbp - 32"));
        assert!(source.contains("ctx->rax"));
        assert!(source.contains("DETRIX_STACK_PTR - 16"));
    }

    #[test]
    fn direct_plan_renderer_covers_each_kernel_operation_family() {
        // Keep one field per IR operation so this test checks the generated
        // reads/fields rather than merely proving that a plan validates.
        let fields = vec![
            CaptureField {
                name: "register".into(),
                offset: 0,
                size: 8,
                op: ReadOp::Register {
                    register: Register::Rdi,
                    semantics: ValueSemantics::Value,
                },
            },
            CaptureField {
                name: "stack".into(),
                offset: 8,
                size: 4,
                op: ReadOp::Stack {
                    offset: -8,
                    size: 4,
                    semantics: ValueSemantics::Value,
                },
            },
            CaptureField {
                name: "frame".into(),
                offset: 12,
                size: 8,
                op: ReadOp::Frame {
                    register: Register::Rbp,
                    offset: -16,
                    size: 8,
                    semantics: ValueSemantics::Value,
                },
            },
            CaptureField {
                name: "blob".into(),
                offset: 20,
                size: 4,
                op: ReadOp::Blob {
                    offset: -32,
                    size: 4,
                    semantics: ValueSemantics::Value,
                },
            },
            CaptureField {
                name: "indirect".into(),
                offset: 24,
                size: 8,
                op: ReadOp::Indirect {
                    base: Box::new(ReadOp::Stack {
                        offset: -40,
                        size: 8,
                        semantics: ValueSemantics::Address,
                    }),
                    size: 16,
                },
            },
            CaptureField {
                name: "map".into(),
                offset: 32,
                size: 8,
                op: ReadOp::Map {
                    base: Box::new(ReadOp::Frame {
                        register: Register::Rbp,
                        offset: -48,
                        size: 8,
                        semantics: ValueSemantics::Address,
                    }),
                },
            },
            CaptureField {
                name: "header".into(),
                offset: 40,
                size: 16,
                op: ReadOp::Header {
                    base: Box::new(ReadOp::Stack {
                        offset: -56,
                        size: 8,
                        semantics: ValueSemantics::Address,
                    }),
                    size: 16,
                    kind: HeaderKind::BorrowedStr,
                },
            },
            CaptureField {
                name: "constant".into(),
                offset: 56,
                size: 8,
                op: ReadOp::Constant {
                    bytes: vec![0x78, 0x56, 0x34, 0x12],
                },
            },
            CaptureField {
                name: "unavailable".into(),
                offset: 64,
                size: 8,
                op: ReadOp::Unavailable {
                    reason: "optimized out".into(),
                },
            },
            CaptureField {
                name: "piece".into(),
                offset: 72,
                size: 8,
                op: ReadOp::Piece {
                    offset: 0,
                    size: 8,
                    op: Box::new(ReadOp::Register {
                        register: Register::Rax,
                        semantics: ValueSemantics::Value,
                    }),
                },
            },
            CaptureField {
                name: "piecewise".into(),
                offset: 80,
                size: 12,
                op: ReadOp::Piecewise {
                    pieces: vec![
                        CapturePiece {
                            offset: 0,
                            size: 8,
                            op: Some(ReadOp::Register {
                                register: Register::Rsi,
                                semantics: ValueSemantics::Value,
                            }),
                        },
                        CapturePiece {
                            offset: 8,
                            size: 4,
                            op: Some(ReadOp::Stack {
                                offset: -64,
                                size: 4,
                                semantics: ValueSemantics::Value,
                            }),
                        },
                    ],
                },
            },
        ];
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "go".into(),
            plan_hash: "sha256:all-ops".into(),
            probe_pc: 0x45,
            fields,
            max_payload_bytes: 92,
        };
        let mut config = CaptureConfig::default();
        config.max_capture_vars = 16;
        let source = generate_bpf_program_from_plan(
            &plan,
            false,
            None,
            None,
            &config,
            Some(RawEnvelopeSpec {
                profile_tag: 0x676f,
                plan_tag: 0x45,
            }),
        )
        .unwrap()
        .source;

        for marker in [
            "ctx->rdi",
            "DETRIX_STACK_PTR - 8",
            "ctx->rbp - 16",
            "DETRIX_STACK_PTR - 32",
            "DETRIX_STACK_PTR - 40",
            "ctx->rbp - 48",
            "DETRIX_STACK_PTR - 56",
            "0x12345678",
            "event->var8 = 0",
            "ctx->rax",
            "ctx->rsi",
            "DETRIX_STACK_PTR - 64",
            "drx_plan_tag = 0x45",
        ] {
            assert!(
                source.contains(marker),
                "generated plan missing marker: {marker}"
            );
        }
        assert!(source.contains("var3_blob[4]"));
        assert!(source.contains("var10_blob[12]"));
        assert!(source.contains("var6_str["));
    }

    #[test]
    fn direct_plan_renderer_rejects_user_space_only_operations() {
        let plan = CapturePlan {
            schema_version: CAPTURE_PLAN_SCHEMA_VERSION,
            architecture: TargetArchitecture::X86_64,
            profile_id: "go".into(),
            plan_hash: "sha256:user-bytes".into(),
            probe_pc: 1,
            fields: vec![CaptureField {
                name: "heap".into(),
                offset: 0,
                size: 8,
                op: ReadOp::UserBytes {
                    address_field: 0,
                    size: 8,
                },
            }],
            max_payload_bytes: 8,
        };
        let error = generate_bpf_program_from_plan(
            &plan,
            false,
            None,
            None,
            &CaptureConfig::default(),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires user-space lowering"));
    }

    #[test]
    fn generate_single_register_var() {
        let vars = vec![make_var(
            "amount",
            VariableLocation::Register(Register::Rax),
            VariableSize::QWord,
        )];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
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
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("bpf_probe_read_user"));
        assert!(prog.source.contains("DETRIX_STACK_PTR"));
    }

    #[test]
    fn generate_with_goid() {
        let prog = generate_bpf_program(&[], true, None, None, &CaptureConfig::default()).unwrap();
        assert!(prog.captures_goid);
        assert!(prog.source.contains("goid"));
        assert!(prog.source.contains("GOID_OFFSET"));
        // x86-64: TLS-based via bpf_core_read(fsbase); ARM64: X28 register.
        // Both branches are present in the source as text (C preprocessor, not Rust).
        assert!(prog.source.contains("fsbase")); // x86-64 TLS path
        assert!(prog.source.contains("regs[28]")); // ARM64 register path
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
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
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
        let result = generate_bpf_program(&vars, false, None, None, &CaptureConfig::default());
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
            nested_type: None,
        }];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("var0")); // ptr field
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
            nested_type: None,
        }];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
        // Struct must have the fixed-size string buffer
        assert!(
            prog.source.contains("var0_str["),
            "event struct missing var0_str[] buffer"
        );
        // Size must match MAX_STRING_CAPTURE
        assert!(
            prog.source
                .contains(&format!("var0_str[{}]", MAX_STRING_CAPTURE)),
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
            nested_type: None,
        }];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
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
        assert!(expr.contains("DETRIX_STACK_PTR + 16"));
        assert!(expr.contains("var2"));
        assert!(expr.contains(", 4,")); // DWord = 4 bytes
    }

    #[test]
    fn generate_read_expr_stack_negative() {
        let var = make_var("x", VariableLocation::stack(-8), VariableSize::QWord);
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(expr.contains("DETRIX_STACK_PTR - 8"));
    }

    #[test]
    fn program_has_license() {
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("LICENSE"));
        assert!(prog.source.contains("Dual MIT/GPL"));
    }

    #[test]
    fn program_has_ringbuf_map() {
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();
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
            nested_type: None,
        }];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
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
            nested_type: None,
        }];
        let config = CaptureConfig {
            max_capture_vars: 8,
            max_string_capture: 64,
            max_blob_capture: 64,
            ..CaptureConfig::default()
        };
        let prog = generate_bpf_program(&vars, false, None, None, &config).unwrap();
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
            nested_type: None,
        };
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(
            expr.contains("bpf_probe_read_user(event->var0_blob"),
            "missing bpf_probe_read_user for blob"
        );
        assert!(expr.contains("DETRIX_STACK_PTR - 32"));
        assert!(expr.contains(", 16,"), "capture size should be 16");
    }

    #[test]
    fn generate_read_expr_piecewise_blob_copies_partial_final_word() {
        let var = ResolvedVariable {
            name: "address".to_string(),
            location: VariableLocation::PiecewiseBlob {
                pieces: vec![
                    VariableLocation::Register(Register::Rax),
                    VariableLocation::Register(Register::Rbx),
                    VariableLocation::Register(Register::Rcx),
                ]
                .into_iter()
                .map(|location| VariablePiece {
                    location: Some(location),
                    byte_size: 8,
                })
                .collect(),
                byte_size: 20,
            },
            size: VariableSize::QWord,
            type_name: "[20]uint8".to_string(),
            nested_type: None,
        };

        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());

        assert!(expr.contains("var0_blob[0]"));
        assert!(expr.contains("ctx->rax"));
        assert!(expr.contains("var0_blob[8]"));
        assert!(expr.contains("ctx->rbx"));
        assert!(expr.contains("var0_blob[16]"));
        assert!(expr.contains("var0_blob[19]"));
        assert!(expr.contains("ctx->rcx"));
        assert!(!expr.contains("var0_blob[20]"));
    }

    #[test]
    fn generate_piecewise_blob_uses_arm64_register_access() {
        let var = make_var(
            "address",
            VariableLocation::PiecewiseBlob {
                pieces: vec![
                    VariableLocation::Register(Register::Arm64(0)),
                    VariableLocation::Register(Register::Arm64(1)),
                    VariableLocation::Register(Register::Arm64(2)),
                ]
                .into_iter()
                .map(|location| VariablePiece {
                    location: Some(location),
                    byte_size: 8,
                })
                .collect(),
                byte_size: 20,
            },
            VariableSize::QWord,
        );
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(expr.contains("ctx->regs[0]"));
        assert!(expr.contains("ctx->regs[1]"));
        assert!(expr.contains("ctx->regs[2]"));
        assert!(!expr.contains("ctx->rax"));
        assert!(!expr.contains("ctx->ctx->"));
    }

    #[test]
    fn generate_piecewise_blob_honors_dwarf_piece_sizes_and_undefined_gaps() {
        let var = make_var(
            "hash",
            VariableLocation::PiecewiseBlob {
                pieces: vec![
                    VariablePiece {
                        location: None,
                        byte_size: 2,
                    },
                    VariablePiece {
                        location: Some(VariableLocation::Register(Register::Rax)),
                        byte_size: 4,
                    },
                    VariablePiece {
                        location: Some(VariableLocation::stack(-16)),
                        byte_size: 6,
                    },
                ],
                byte_size: 12,
            },
            VariableSize::QWord,
        );
        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());
        assert!(expr.contains("var0_blob[2] ="));
        assert!(expr.contains("var0_blob[5] ="));
        assert!(expr.contains("var0_blob[6]"));
        assert!(!expr.contains("var0_blob[0] ="));
    }

    #[test]
    fn generate_go_string_length_uses_complete_register_access_expression() {
        let var = ResolvedVariable {
            name: "value".to_string(),
            location: VariableLocation::GoString {
                ptr: Box::new(VariableLocation::Register(Register::Arm64(0))),
                len: Box::new(VariableLocation::Register(Register::Arm64(1))),
            },
            size: VariableSize::QWord,
            type_name: "string".to_string(),
            nested_type: None,
        };

        let expr = generate_read_expr(&var, 0, &CaptureConfig::default());

        assert!(expr.contains("event->var0 = (u64)ctx->regs[0]"));
        assert!(expr.contains("event->var0_len = (u64)ctx->regs[1]"));
        assert!(!expr.contains("ctx->ctx->"));
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
            nested_type: None,
        }];
        let prog =
            generate_bpf_program(&vars, false, None, None, &CaptureConfig::default()).unwrap();
        assert!(prog.source.contains("var0_len"));
        assert!(prog.source.contains("var0_cap"));
    }

    #[test]
    fn program_has_drop_counter_map() {
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();
        assert!(
            prog.source.contains("DETRIX_DROP_CNT"),
            "generated program missing DETRIX_DROP_CNT map"
        );
        assert!(
            prog.source.contains("BPF_MAP_TYPE_PERCPU_ARRAY"),
            "drop counter should use PERCPU_ARRAY for zero-contention increments"
        );
    }

    #[test]
    fn program_has_drop_increment_logic() {
        let prog = generate_bpf_program(&[], false, None, None, &CaptureConfig::default()).unwrap();
        // Check drop counter increment on ring buffer reserve failure
        assert!(
            prog.source.contains("bpf_map_lookup_elem(&DETRIX_DROP_CNT"),
            "missing drop counter lookup in ringbuf reserve failure path"
        );
        assert!(
            prog.source.contains("(*cnt)++"),
            "missing drop counter increment"
        );
    }
}
