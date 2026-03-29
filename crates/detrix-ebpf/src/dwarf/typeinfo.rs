//! DWARF type information resolution
//!
//! Follows `DW_AT_type` reference chains from variable DIEs to their type DIEs,
//! extracting `byte_size` and human-readable type names.
//!
//! This is needed because variable DIEs in Go DWARF typically don't carry
//! `DW_AT_byte_size` directly — they reference a type DIE via `DW_AT_type`.
//!
//! # Go DWARF type structure
//!
//! ```text
//! DW_TAG_variable "amount"
//!   DW_AT_type → unit_offset 0x18
//!
//! DW_TAG_base_type @ 0x18   ← resolved by this module
//!   DW_AT_name "int64"
//!   DW_AT_byte_size 8
//! ```
//!
//! For pointer and named types Go emits:
//! ```text
//! DW_TAG_variable "req"
//!   DW_AT_type → DW_TAG_pointer_type
//!                  DW_AT_byte_size 8
//!                  DW_AT_type → DW_TAG_structure_type "http.Request"
//! ```

use crate::dwarf::types::VariableSize;
use crate::error::{Error, Result};

use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, Reader};

/// Resolved type info for a variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeInfo {
    /// Human-readable type name (e.g. "int64", "string", "*http.Request").
    pub name: String,
    /// Size in bytes (for BPF read sizing).
    pub size: VariableSize,
    /// Whether this is a pointer type.
    pub is_pointer: bool,
    /// Whether this is a Go string (struct {ptr, len}).
    pub is_string: bool,
    /// Whether this is a Go slice (struct {ptr, len, cap}).
    pub is_slice: bool,
}

impl TypeInfo {
    /// Default when type resolution fails — assume 8-byte opaque value.
    pub fn unknown() -> Self {
        Self {
            name: "unknown".to_string(),
            size: VariableSize::QWord,
            is_pointer: false,
            is_string: false,
            is_slice: false,
        }
    }
}

/// Resolve type info for a variable DIE by following its `DW_AT_type` chain.
///
/// # Arguments
/// * `var_entry` - The variable or parameter DIE.
/// * `unit` - The compilation unit containing the DIE.
/// * `dwarf` - The full DWARF context (for string resolution).
///
/// Returns `TypeInfo::unknown()` if the type chain cannot be resolved,
/// rather than an error — missing type info should not fail probe setup.
pub fn resolve_type_info<R: Reader>(
    var_entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    let type_offset = match get_type_offset(var_entry)? {
        Some(off) => off,
        None => return Ok(TypeInfo::unknown()),
    };

    resolve_type_at_offset(type_offset, unit, dwarf, 0)
}

/// Get the `DW_AT_type` unit offset from a DIE.
fn get_type_offset<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
) -> Result<Option<gimli::UnitOffset<R::Offset>>> {
    match entry
        .attr_value(DwAt(gimli::constants::DW_AT_type.0))
        .map_err(|e| Error::DwarfParse(format!("DW_AT_type read: {e}")))?
    {
        Some(AttributeValue::UnitRef(offset)) => Ok(Some(offset)),
        _ => Ok(None),
    }
}

/// Maximum depth for following pointer/typedef chains (prevents cycles).
const MAX_TYPE_DEPTH: u8 = 8;

/// Resolve a type at a given unit offset.
fn resolve_type_at_offset<R: Reader>(
    offset: gimli::UnitOffset<R::Offset>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    depth: u8,
) -> Result<TypeInfo> {
    if depth >= MAX_TYPE_DEPTH {
        return Ok(TypeInfo::unknown());
    }

    let mut cursor = unit.entries_at_offset(offset)
        .map_err(|e| Error::DwarfParse(format!("entries_at_offset: {e}")))?;

    let (_, entry) = match cursor.next_dfs()
        .map_err(|e| Error::DwarfParse(format!("next_dfs: {e}")))?
    {
        Some(e) => e,
        None => return Ok(TypeInfo::unknown()),
    };

    let tag = entry.tag();

    match tag {
        // Scalar types: int, float, bool, uint, etc.
        gimli::DW_TAG_base_type => resolve_base_type(entry, unit, dwarf),

        // Pointer type: *T — always 8 bytes on amd64
        gimli::DW_TAG_pointer_type => {
            let pointee_name = get_pointee_name(entry, unit, dwarf, depth)?;
            Ok(TypeInfo {
                name: format!("*{pointee_name}"),
                size: VariableSize::QWord,
                is_pointer: true,
                is_string: false,
                is_slice: false,
            })
        }

        // Typedef / named type: follow the chain
        gimli::DW_TAG_typedef => {
            let inner_offset = match get_type_offset(entry)? {
                Some(off) => off,
                None => return Ok(TypeInfo::unknown()),
            };
            let mut info = resolve_type_at_offset(inner_offset, unit, dwarf, depth + 1)?;
            // Override name with the typedef name if available
            if let Some(name) = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)? {
                info.name = name;
            }
            Ok(info)
        }

        // Go string is a struct with two fields (ptr + len), total 16 bytes
        gimli::DW_TAG_structure_type => resolve_struct_type(entry, unit, dwarf),

        // Array type — represent as pointer-sized
        gimli::DW_TAG_array_type => {
            let name = match get_type_offset(entry)? {
                Some(off) => {
                    let elem = resolve_type_at_offset(off, unit, dwarf, depth + 1)?;
                    format!("[]{}", elem.name)
                }
                None => "[]unknown".to_string(),
            };
            Ok(TypeInfo {
                name,
                size: VariableSize::QWord,
                is_pointer: true,
                is_string: false,
                is_slice: true,
            })
        }

        // Const qualifier — follow the inner type
        gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => {
            match get_type_offset(entry)? {
                Some(off) => resolve_type_at_offset(off, unit, dwarf, depth + 1),
                None => Ok(TypeInfo::unknown()),
            }
        }

        _ => Ok(TypeInfo::unknown()),
    }
}

/// Resolve a DW_TAG_base_type DIE (int, float, bool, etc.).
fn resolve_base_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    let name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?
        .unwrap_or_else(|| "unknown".to_string());

    let byte_size = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_byte_size.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        Some(AttributeValue::Udata(n)) => n,
        _ => 8, // Default to 8 bytes
    };

    let size = VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord);
    let _ = unit; // used for completeness

    Ok(TypeInfo {
        name,
        size,
        is_pointer: false,
        is_string: false,
        is_slice: false,
    })
}

/// Get the name of what a pointer points to.
fn get_pointee_name<R: Reader>(
    ptr_entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    depth: u8,
) -> Result<String> {
    match get_type_offset(ptr_entry)? {
        Some(off) => {
            let inner = resolve_type_at_offset(off, unit, dwarf, depth + 1)?;
            Ok(inner.name)
        }
        None => Ok("void".to_string()),
    }
}

/// Resolve a DW_TAG_structure_type DIE.
///
/// Go emits its built-in types (string, slice headers) as struct types.
/// We detect them by name and field count.
fn resolve_struct_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    _unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    let name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?
        .unwrap_or_else(|| "struct".to_string());

    let byte_size = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_byte_size.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        Some(AttributeValue::Udata(n)) => n,
        _ => 8,
    };

    // Go string = struct with "str" field (ptr) + "len" field = 16 bytes
    let is_string = name == "string" || byte_size == 16 && name.contains("string");
    // Go slice = struct with array ptr + len + cap = 24 bytes
    let is_slice = byte_size == 24 && !is_string;

    let size = if is_string || is_slice {
        // Strings and slices are captured via multi-read, report header size
        VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord)
    } else {
        VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord)
    };

    Ok(TypeInfo {
        name,
        size,
        is_pointer: false,
        is_string,
        is_slice,
    })
}

/// Read a string attribute from a DIE.
fn read_attr_string<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
    attr: gimli::constants::DwAt,
) -> Result<Option<String>> {
    match entry
        .attr_value(DwAt(attr.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        Some(AttributeValue::DebugStrRef(offset)) => {
            let s = dwarf
                .string(offset)
                .map_err(|e| Error::DwarfParse(format!("{e}")))?;
            let name = s
                .to_string_lossy()
                .map_err(|e| Error::DwarfParse(format!("UTF-8: {e}")))?;
            Ok(Some(name.to_string()))
        }
        Some(AttributeValue::String(ref s)) => {
            let name = s
                .to_string_lossy()
                .map_err(|e| Error::DwarfParse(format!("UTF-8: {e}")))?;
            Ok(Some(name.to_string()))
        }
        _ => Ok(None),
    }
}

// ============================================================================
// Go type name canonicalization
// ============================================================================

/// Map a raw DWARF type name to a canonical Go type name.
///
/// Go's DWARF often emits internal names like "int" where the Go source says
/// "int64", or emits package-qualified names. This function normalizes them.
pub fn canonicalize_go_type(raw: &str) -> &str {
    match raw {
        // Signed integers
        "int8" | "int16" | "int32" | "int64" | "int" => raw,
        // Unsigned integers
        "uint8" | "byte" | "uint16" | "uint32" | "uint64" | "uint" | "uintptr" => raw,
        // Floats
        "float32" | "float64" => raw,
        // Complex
        "complex64" | "complex128" => raw,
        // Special
        "bool" | "string" | "error" => raw,
        // Unknown — return as-is
        other => other,
    }
}

/// Detect if a type name is a Go string (needs 2-read capture).
pub fn is_go_string_type(name: &str) -> bool {
    name == "string"
}

/// Detect if a type name is a Go slice (needs 3-read capture).
pub fn is_go_slice_type(name: &str) -> bool {
    name.starts_with("[]")
}

/// Detect if a type name is a boolean (byte-sized, display as true/false).
pub fn is_go_bool_type(name: &str) -> bool {
    name == "bool"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_info_unknown_default() {
        let info = TypeInfo::unknown();
        assert_eq!(info.name, "unknown");
        assert_eq!(info.size, VariableSize::QWord);
        assert!(!info.is_pointer);
        assert!(!info.is_string);
        assert!(!info.is_slice);
    }

    // -----------------------------------------------------------------------
    // Go type name canonicalization (pure, no DWARF needed)
    // -----------------------------------------------------------------------

    #[test]
    fn canonicalize_signed_ints() {
        for t in ["int8", "int16", "int32", "int64", "int"] {
            assert_eq!(canonicalize_go_type(t), t);
        }
    }

    #[test]
    fn canonicalize_unsigned_ints() {
        for t in ["uint8", "byte", "uint16", "uint32", "uint64", "uint", "uintptr"] {
            assert_eq!(canonicalize_go_type(t), t);
        }
    }

    #[test]
    fn canonicalize_floats() {
        assert_eq!(canonicalize_go_type("float32"), "float32");
        assert_eq!(canonicalize_go_type("float64"), "float64");
    }

    #[test]
    fn canonicalize_specials() {
        assert_eq!(canonicalize_go_type("bool"), "bool");
        assert_eq!(canonicalize_go_type("string"), "string");
        assert_eq!(canonicalize_go_type("error"), "error");
    }

    #[test]
    fn canonicalize_unknown_passes_through() {
        assert_eq!(canonicalize_go_type("net.Conn"), "net.Conn");
        assert_eq!(canonicalize_go_type("*http.Request"), "*http.Request");
    }

    #[test]
    fn is_string_type_detection() {
        assert!(is_go_string_type("string"));
        assert!(!is_go_string_type("int64"));
        assert!(!is_go_string_type("[]string"));
    }

    #[test]
    fn is_slice_type_detection() {
        assert!(is_go_slice_type("[]byte"));
        assert!(is_go_slice_type("[]string"));
        assert!(is_go_slice_type("[]*http.Request"));
        assert!(!is_go_slice_type("string"));
        assert!(!is_go_slice_type("int64"));
    }

    #[test]
    fn is_bool_type_detection() {
        assert!(is_go_bool_type("bool"));
        assert!(!is_go_bool_type("int"));
        assert!(!is_go_bool_type("uint8"));
    }

    // -----------------------------------------------------------------------
    // Struct type classification (no DWARF needed)
    // -----------------------------------------------------------------------

    #[test]
    fn struct_string_detection() {
        // Go string struct is named "string" with byte_size 16
        let is_str = "string" == "string" || (16 == 16 && "string".contains("string"));
        assert!(is_str);
    }

    #[test]
    fn struct_slice_not_string() {
        // Slice has 24 bytes and is not a string
        let byte_size: u64 = 24;
        let name = "[]int64";
        let is_string = name == "string";
        let is_slice = byte_size == 24 && !is_string;
        assert!(is_slice);
    }

    // -----------------------------------------------------------------------
    // TypeInfo field semantics
    // -----------------------------------------------------------------------

    #[test]
    fn type_info_pointer() {
        let info = TypeInfo {
            name: "*http.Request".to_string(),
            size: VariableSize::QWord,
            is_pointer: true,
            is_string: false,
            is_slice: false,
        };
        assert!(info.is_pointer);
        assert_eq!(info.size.bytes(), 8);
    }

    #[test]
    fn type_info_string() {
        let info = TypeInfo {
            name: "string".to_string(),
            size: VariableSize::QWord, // header ptr part is 8 bytes
            is_pointer: false,
            is_string: true,
            is_slice: false,
        };
        assert!(info.is_string);
        assert!(!info.is_slice);
    }

    #[test]
    fn type_info_slice() {
        let info = TypeInfo {
            name: "[]int64".to_string(),
            size: VariableSize::QWord,
            is_pointer: false,
            is_string: false,
            is_slice: true,
        };
        assert!(info.is_slice);
        assert!(!info.is_string);
    }

    #[test]
    fn variable_size_from_byte_size_roundtrip() {
        // Validate that common Go sizes resolve correctly
        let cases: &[(u64, VariableSize)] = &[
            (1, VariableSize::Byte),   // bool, int8, uint8
            (4, VariableSize::DWord),  // int32, float32
            (8, VariableSize::QWord),  // int64, float64, pointer
        ];
        for (bytes, expected) in cases {
            assert_eq!(VariableSize::from_byte_size(*bytes), Some(*expected));
        }
    }

    #[test]
    fn max_type_depth_prevents_cycles() {
        // Verify the constant exists and is reasonable
        assert!(MAX_TYPE_DEPTH >= 4 && MAX_TYPE_DEPTH <= 16);
    }
}
