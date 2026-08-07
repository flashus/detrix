//! DWARF-derived types for variable location resolution
//!
//! These types represent where a variable lives at a given program counter (PC):
//! either in a CPU register or at a stack offset. They are used to generate
//! eBPF programs that read variable values from `struct pt_regs` or via
//! `bpf_probe_read_user()`.

use std::fmt;
use std::path::PathBuf;

/// A program counter (instruction address) in the target binary.
pub type ProgramCounter = u64;

/// CPU register index (x86-64 DWARF register numbering).
///
/// Maps to `struct pt_regs` fields in the eBPF program.
/// See: DWARF register number assignments for x86-64 (AMD64 ABI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Register {
    Rax = 0,
    Rdx = 1,
    Rcx = 2,
    Rbx = 3,
    Rsi = 4,
    Rdi = 5,
    Rbp = 6,
    Rsp = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl Register {
    /// Convert a DWARF register number to a Register enum.
    ///
    /// Returns `None` for registers we don't handle (XMM, segment, etc.).
    pub fn from_dwarf(reg_num: u16) -> Option<Self> {
        match reg_num {
            0 => Some(Self::Rax),
            1 => Some(Self::Rdx),
            2 => Some(Self::Rcx),
            3 => Some(Self::Rbx),
            4 => Some(Self::Rsi),
            5 => Some(Self::Rdi),
            6 => Some(Self::Rbp),
            7 => Some(Self::Rsp),
            8 => Some(Self::R8),
            9 => Some(Self::R9),
            10 => Some(Self::R10),
            11 => Some(Self::R11),
            12 => Some(Self::R12),
            13 => Some(Self::R13),
            14 => Some(Self::R14),
            15 => Some(Self::R15),
            _ => None,
        }
    }

    /// Get the `pt_regs` field name for this register (for BPF C code generation).
    pub fn pt_regs_field(&self) -> &'static str {
        match self {
            Self::Rax => "rax",
            Self::Rdx => "rdx",
            Self::Rcx => "rcx",
            Self::Rbx => "rbx",
            Self::Rsi => "rsi",
            Self::Rdi => "rdi",
            Self::Rbp => "rbp",
            Self::Rsp => "rsp",
            Self::R8 => "r8",
            Self::R9 => "r9",
            Self::R10 => "r10",
            Self::R11 => "r11",
            Self::R12 => "r12",
            Self::R13 => "r13",
            Self::R14 => "r14",
            Self::R15 => "r15",
        }
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.pt_regs_field())
    }
}

/// Where a variable lives at a specific program counter.
///
/// Derived from DWARF location expressions (DW_OP_regX, DW_OP_fbreg, etc.).
/// The eBPF program generator uses this to emit the right read instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableLocation {
    /// Variable is in a CPU register.
    /// Read via `ctx->{register}` in BPF.
    Register(Register),

    /// Variable is at a stack offset relative to the frame base (RSP).
    /// Read via `bpf_probe_read_user` from the architecture-specific stack
    /// pointer (`ctx->rsp` on x86-64, `ctx->sp` on arm64) plus this offset.
    StackOffset {
        /// Offset from frame base (can be negative for locals below RSP).
        offset: i64,
    },

    /// Variable is a Go string (ptr + len at adjacent locations).
    /// Requires two reads: pointer and length.
    GoString {
        /// Location of the string data pointer.
        ptr: Box<VariableLocation>,
        /// Location of the string length.
        len: Box<VariableLocation>,
    },

    /// Variable is a Go slice (ptr + len + cap).
    GoSlice {
        ptr: Box<VariableLocation>,
        len: Box<VariableLocation>,
        cap: Box<VariableLocation>,
    },

    /// Variable is a fixed-size array or struct stored inline on the stack.
    ///
    /// Captured as raw bytes via `bpf_probe_read_user(buf, min(byte_size, limit), sp + offset)`.
    /// Used for `[N]T` arrays and user-defined struct types.
    StackBlob {
        /// Offset from frame base (RSP).
        offset: i64,
        /// Total byte size from DWARF `DW_AT_byte_size`. Clamped at runtime by `max_blob_capture`.
        byte_size: usize,
    },

    /// Variable's stack slot holds a POINTER to the actual struct on the heap.
    ///
    /// Produced when DWARF location contains `DW_OP_deref` (heap-escaped Go variables).
    /// BPF reads the 8-byte pointer from the stack slot, then user-space dereferences
    /// the pointer to read `byte_size` bytes of the actual struct from process memory.
    StackIndirect {
        /// Offset from frame base (RSP) where the pointer lives.
        offset: i64,
        /// Byte size of the pointed-to struct (from DWARF `DW_AT_byte_size`).
        byte_size: usize,
    },

    /// Variable is a Go map (pointer to runtime.hmap or Swiss Table Map).
    /// BPF captures the map pointer; user-space iterates the map structure.
    GoMap {
        /// Location of the map pointer (hmap* or Map*).
        ptr: Box<VariableLocation>,
    },
}

impl VariableLocation {
    /// Create a register location from a DWARF register number.
    pub fn from_register(dwarf_reg: u16) -> Option<Self> {
        Register::from_dwarf(dwarf_reg).map(Self::Register)
    }

    /// Create a stack offset location.
    pub fn stack(offset: i64) -> Self {
        Self::StackOffset { offset }
    }

    /// Returns true if this is a simple scalar location (register or stack).
    pub fn is_scalar(&self) -> bool {
        matches!(self, Self::Register(_) | Self::StackOffset { .. })
    }

    /// Number of BPF reads needed to capture this variable.
    pub fn read_count(&self) -> usize {
        match self {
            Self::Register(_) => 1,
            Self::StackOffset { .. } => 1,
            Self::GoString { .. } => 2,
            Self::GoSlice { .. } => 3,
            Self::StackBlob { .. } => 1,
            Self::StackIndirect { .. } => 1,
            Self::GoMap { .. } => 1,
        }
    }
}

impl fmt::Display for VariableLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(reg) => write!(f, "{reg}"),
            Self::StackOffset { offset } => {
                if *offset >= 0 {
                    write!(f, "[sp+{offset:#x}]")
                } else {
                    write!(f, "[sp-{:#x}]", offset.unsigned_abs())
                }
            }
            Self::GoString { .. } => write!(f, "go.string{{ptr, len}}"),
            Self::GoSlice { .. } => write!(f, "go.slice{{ptr, len, cap}}"),
            Self::StackBlob { offset, byte_size } => {
                if *offset >= 0 {
                    write!(f, "blob[{byte_size}b@sp+{offset:#x}]")
                } else {
                    write!(f, "blob[{byte_size}b@sp-{:#x}]", offset.unsigned_abs())
                }
            }
            Self::StackIndirect { offset, byte_size } => {
                if *offset >= 0 {
                    write!(f, "indirect[{byte_size}b@*sp+{offset:#x}]")
                } else {
                    write!(f, "indirect[{byte_size}b@*sp-{:#x}]", offset.unsigned_abs())
                }
            }
            Self::GoMap { .. } => write!(f, "go.map{{ptr}}"),
        }
    }
}

/// Size of a variable in bytes, needed for `bpf_probe_read_user`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableSize {
    Byte,  // 1 byte (bool)
    Word,  // 2 bytes
    DWord, // 4 bytes (int32, float32)
    QWord, // 8 bytes (int64, float64, pointer)
}

impl VariableSize {
    pub fn bytes(&self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::DWord => 4,
            Self::QWord => 8,
        }
    }

    /// Infer size from DWARF byte size attribute.
    pub fn from_byte_size(size: u64) -> Option<Self> {
        match size {
            1 => Some(Self::Byte),
            2 => Some(Self::Word),
            4 => Some(Self::DWord),
            8 => Some(Self::QWord),
            _ => None,
        }
    }
}

/// A resolved variable ready for eBPF capture.
///
/// Contains everything needed to generate a BPF read instruction:
/// the variable name, where it lives, and how big it is.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedVariable {
    /// Variable name as it appears in source code.
    pub name: String,
    /// Where the variable lives at the probe PC.
    pub location: VariableLocation,
    /// Size of the variable.
    pub size: VariableSize,
    /// Go type name (e.g., "int", "string", "*http.Request").
    pub type_name: String,
    /// Nested type structure for structs (depth-limited).
    /// Populated when type is a struct, contains field information.
    pub nested_type: Option<crate::dwarf::nested_types::NestedType>,
}

/// Result of resolving a source file:line to a probe point.
///
/// Contains the PC to attach the uprobe and the available variables.
#[derive(Debug, Clone)]
pub struct ProbePoint {
    /// Binary path (ELF).
    pub binary_path: PathBuf,
    /// Program counter (instruction address) for the uprobe.
    pub pc: ProgramCounter,
    /// Offset from the symbol start (what aya/libbpf needs for uprobe attachment).
    pub symbol_offset: u64,
    /// Function name containing this PC.
    pub function_name: String,
    /// Variables available at this PC with their locations.
    pub variables: Vec<ResolvedVariable>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_from_dwarf_valid() {
        assert_eq!(Register::from_dwarf(0), Some(Register::Rax));
        assert_eq!(Register::from_dwarf(7), Some(Register::Rsp));
        assert_eq!(Register::from_dwarf(15), Some(Register::R15));
    }

    #[test]
    fn register_from_dwarf_invalid() {
        // XMM0 = 17 in DWARF, not supported
        assert_eq!(Register::from_dwarf(17), None);
        assert_eq!(Register::from_dwarf(100), None);
    }

    #[test]
    fn register_pt_regs_field() {
        assert_eq!(Register::Rax.pt_regs_field(), "rax");
        assert_eq!(Register::R15.pt_regs_field(), "r15");
    }

    #[test]
    fn variable_location_from_register() {
        let loc = VariableLocation::from_register(0);
        assert_eq!(loc, Some(VariableLocation::Register(Register::Rax)));

        let loc = VariableLocation::from_register(99);
        assert_eq!(loc, None);
    }

    #[test]
    fn variable_location_stack() {
        let loc = VariableLocation::stack(-8);
        assert_eq!(loc, VariableLocation::StackOffset { offset: -8 });
        assert!(loc.is_scalar());
    }

    #[test]
    fn variable_location_read_count() {
        assert_eq!(VariableLocation::Register(Register::Rax).read_count(), 1);
        assert_eq!(VariableLocation::stack(0).read_count(), 1);
        assert_eq!(
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
            }
            .read_count(),
            2
        );
        assert_eq!(
            VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::stack(0)),
                len: Box::new(VariableLocation::stack(8)),
                cap: Box::new(VariableLocation::stack(16)),
            }
            .read_count(),
            3
        );
    }

    #[test]
    fn variable_location_is_scalar() {
        assert!(VariableLocation::Register(Register::Rax).is_scalar());
        assert!(VariableLocation::stack(-16).is_scalar());
        assert!(!VariableLocation::GoString {
            ptr: Box::new(VariableLocation::stack(0)),
            len: Box::new(VariableLocation::stack(8)),
        }
        .is_scalar());
    }

    #[test]
    fn variable_size_from_byte_size() {
        assert_eq!(VariableSize::from_byte_size(1), Some(VariableSize::Byte));
        assert_eq!(VariableSize::from_byte_size(4), Some(VariableSize::DWord));
        assert_eq!(VariableSize::from_byte_size(8), Some(VariableSize::QWord));
        assert_eq!(VariableSize::from_byte_size(3), None);
        assert_eq!(VariableSize::from_byte_size(16), None);
    }

    #[test]
    fn variable_size_bytes() {
        assert_eq!(VariableSize::Byte.bytes(), 1);
        assert_eq!(VariableSize::Word.bytes(), 2);
        assert_eq!(VariableSize::DWord.bytes(), 4);
        assert_eq!(VariableSize::QWord.bytes(), 8);
    }

    #[test]
    fn display_register() {
        assert_eq!(format!("{}", Register::Rax), "%rax");
        assert_eq!(format!("{}", Register::R15), "%r15");
    }

    #[test]
    fn display_variable_location() {
        assert_eq!(
            format!("{}", VariableLocation::Register(Register::Rax)),
            "%rax"
        );
        assert_eq!(format!("{}", VariableLocation::stack(16)), "[sp+0x10]");
        assert_eq!(format!("{}", VariableLocation::stack(-8)), "[sp-0x8]");
    }

    #[test]
    fn resolved_variable_construction() {
        let var = ResolvedVariable {
            name: "amount".to_string(),
            location: VariableLocation::Register(Register::Rax),
            size: VariableSize::QWord,
            type_name: "int64".to_string(),
            nested_type: None,
        };
        assert_eq!(var.name, "amount");
        assert!(var.location.is_scalar());
        assert_eq!(var.size.bytes(), 8);
    }
}
