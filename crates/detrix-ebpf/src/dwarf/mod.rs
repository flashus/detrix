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

pub mod context;
pub mod evaluator;
pub mod nested_types;
pub mod parser;
pub mod slice_types;
pub mod typeinfo;
pub mod types;

pub use context::DwarfContext;
pub use evaluator::{evaluate_expression, LocationAtom, LocationPiece, LocationSemantics};
pub use nested_types::{NestedType, NestedTypeConfig, StructField};
pub use parser::DwarfInfo;
pub use slice_types::extract_slice_element_info;
pub use typeinfo::{
    canonicalize_go_type, is_go_bool_type, is_go_slice_type, is_go_string_type, EnumLayout,
    EnumVariantLayout, TypeInfo,
};
pub use types::{
    ProbePcCandidate, ProbePoint, ProbeResolutionDiagnostics, ProgramCounter, Register,
    ResolvedVariable, VariableLocation, VariableSize,
};
