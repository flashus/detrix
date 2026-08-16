//! eBPF probe module — program generation, uprobe management, event reading
//!
//! # Architecture
//!
//! ```text
//! ProbePoint (from dwarf/) ──► program::generate_bpf_program()
//!                                    │
//!                                    ▼
//!                              BpfProgram (C source)
//!                                    │
//!                              [compile + load]  (Linux only)
//!                                    │
//!                                    ▼
//!                          uprobe::UprobeManager::attach()
//!                                    │
//!                              [ring buffer]
//!                                    │
//!                                    ▼
//!                          ringbuf::parse_ring_buffer_event()
//!                                    │
//!                                    ▼
//!                              ProbeEvent → MetricEvent
//! ```

pub mod loader;
pub mod map_iter;
pub mod program;
pub mod ringbuf;
pub mod types;
pub mod uprobe;

pub use loader::{
    check_clang_available, compile_bpf, compile_bpf_for_arch, sanitize_probe_name,
    Aarch64BpfCompiler, ArchitectureBpfCompiler, BpfCompileReport, ClangBpfCompiler,
    X86_64BpfCompiler,
};
pub use program::{
    generate_bpf_program, generate_bpf_program_with_envelope, BpfProgram, RawEnvelopeSpec,
    RAW_ENVELOPE_MAGIC, RAW_ENVELOPE_SCHEMA, RAW_ENVELOPE_SIZE,
};
pub use ringbuf::{
    parse_ring_buffer_event, parse_ring_buffer_event_with_envelope, RawEnvelopeExpectation,
};
pub use types::{CapturedValue, ProbeConfig, ProbeEvent};
pub use uprobe::{UprobeManager, RAW_EVENT_CHANNEL_CAPACITY};
