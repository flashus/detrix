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
pub mod program;
pub mod ringbuf;
pub mod types;
pub mod uprobe;

pub use loader::{check_clang_available, compile_bpf, sanitize_probe_name};
pub use program::{generate_bpf_program, BpfProgram};
pub use ringbuf::parse_ring_buffer_event;
pub use types::{CapturedValue, ProbeConfig, ProbeEvent};
pub use uprobe::UprobeManager;
