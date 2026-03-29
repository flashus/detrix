//! DWARF debug info module for Go ELF binaries
//!
//! Provides the bridge between source-level locations (file:line) and
//! machine-level addresses (PC + variable locations in registers/stack).
//!
//! # Architecture
//!
//! ```text
//! file:line ──► DwarfInfo::resolve_probe_point() ──► ProbePoint
//!                  │                                    │
//!                  ├─ .debug_line → PC                  ├─ pc (uprobe addr)
//!                  ├─ .debug_info → function name       ├─ symbol_offset
//!                  └─ .debug_info → var locations        └─ variables[]
//!                                                           ├─ name
//!                                                           ├─ VariableLocation
//!                                                           └─ VariableSize
//! ```

pub mod parser;
pub mod typeinfo;
pub mod types;

pub use parser::DwarfInfo;
pub use typeinfo::{TypeInfo, canonicalize_go_type, is_go_bool_type, is_go_slice_type, is_go_string_type};
pub use types::{
    ProbePoint, ProgramCounter, Register, ResolvedVariable, VariableLocation, VariableSize,
};
