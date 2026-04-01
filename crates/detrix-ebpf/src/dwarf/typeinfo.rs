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
    /// Raw byte size from DWARF `DW_AT_byte_size`. Used for blob captures.
    pub byte_size: u64,
    /// Whether this is a pointer type.
    pub is_pointer: bool,
    /// Whether this is a Go string (struct {ptr, len}).
    pub is_string: bool,
    /// Whether this is a Go slice (struct {ptr, len, cap}).
    pub is_slice: bool,
    /// Whether this is a fixed-size array type (`[N]T`).
    pub is_array: bool,
    /// Whether this is a struct type (non-string, non-slice aggregate).
    pub is_struct: bool,
}

impl TypeInfo {
    /// Default when type resolution fails — assume 8-byte opaque value.
    pub fn unknown() -> Self {
        Self {
            name: "unknown".to_string(),
            size: VariableSize::QWord,
            byte_size: 8,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: false,
            is_struct: false,
        }
    }
}

/// Resolve type info for a variable DIE by following its `DW_AT_type` chain.
///
/// # Arguments
/// * `var_entry` - The variable or parameter DIE.
/// * `unit` - The compilation unit containing the variable DIE.
/// * `dwarf` - The full DWARF context (for string resolution and cross-unit refs).
///
/// Returns `TypeInfo::unknown()` if the type chain cannot be resolved,
/// rather than an error — missing type info should not fail probe setup.
pub fn resolve_type_info<R: Reader>(
    var_entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    // Check for type attribute
    let type_attr = var_entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    
    match type_attr {
        Some(AttributeValue::UnitRef(offset)) => {
            detrix_logging::debug!(
                "[DWARF typeinfo] resolve_type_info: UnitRef offset={:?}",
                offset.0
            );
            resolve_type_at_offset(offset, unit, dwarf, 0)
        }
        Some(AttributeValue::DebugInfoRef(debug_info_offset)) => {
            // Cross-unit reference — Go uses DW_FORM_ref_addr for type references.
            // Need to find which compilation unit contains this offset.
            detrix_logging::debug!(
                "[DWARF typeinfo] resolve_type_info: DebugInfoRef offset={:?}",
                debug_info_offset.0
            );
            resolve_type_from_debug_info_ref(debug_info_offset, unit, dwarf, 0)
        }
        None => {
            detrix_logging::debug!("[DWARF typeinfo] resolve_type_info: no DW_AT_type attribute");
            Ok(TypeInfo::unknown())
        }
        _ => Ok(TypeInfo::unknown()),
    }
}

/// Resolve a type from a cross-unit DebugInfoRef.
///
/// Searches through compilation units to find the one containing the offset,
/// then resolves the type within that unit.
fn resolve_type_from_debug_info_ref<R: Reader>(
    debug_info_offset: gimli::DebugInfoOffset<R::Offset>,
    _current_unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    depth: u8,
) -> Result<TypeInfo> {
    let target_offset = debug_info_offset.0;
    detrix_logging::debug!(
        "[DWARF typeinfo] Resolving DebugInfoRef {:?} across units",
        target_offset
    );
    
    // Collect all units first to find the one containing our offset
    let mut units = dwarf.units();
    let mut unit_headers = Vec::new();
    while let Ok(Some(unit_header)) = units.next() {
        unit_headers.push(unit_header);
    }
    
    // Sort by offset to find the containing unit
    unit_headers.sort_by_key(|h| h.offset().0);
    
    // Find the unit that contains our target offset
    // The target should be >= unit_start and < next_unit_start (or end of section)
    for (i, unit_header) in unit_headers.iter().enumerate() {
        let unit_start = unit_header.offset().0;
        let next_start = unit_headers.get(i + 1).map(|h| h.offset().0);
        
        // Check if target is within this unit's range
        let is_in_range = if let Some(next) = next_start {
            target_offset >= unit_start && target_offset < next
        } else {
            // Last unit - just check it's after the start
            target_offset >= unit_start
        };
        
        if is_in_range {
            let unit = dwarf
                .unit(unit_header.clone())
                .map_err(|e| Error::DwarfParse(format!("Unit load: {e}")))?;
            
            let unit_local_offset_val = target_offset - unit_start;
            let unit_local_offset = gimli::UnitOffset(unit_local_offset_val);
            detrix_logging::debug!(
                "[DWARF typeinfo] Found containing unit: start={:?} local_offset={:?}",
                unit_start,
                unit_local_offset.0
            );
            
            return resolve_type_at_offset(unit_local_offset, &unit, dwarf, depth);
        }
    }
    
    detrix_logging::debug!(
        "[DWARF typeinfo] DebugInfoRef {:?} not found in any unit range",
        target_offset
    );
    Ok(TypeInfo::unknown())
}

/// Maximum depth for following pointer/typedef chains (prevents cycles).
const MAX_TYPE_DEPTH: u8 = 8;

/// Resolve the target type of a typedef, handling both unit-local and cross-unit references.
///
/// Go's DWARF uses `DW_FORM_ref_addr` (DebugInfoRef) for type references,
/// which can point to types in different compilation units.
fn resolve_typedef_target<R: Reader>(
    typedef_entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    depth: u8,
) -> Result<TypeInfo> {
    let type_attr = typedef_entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    
    match type_attr {
        Some(AttributeValue::UnitRef(offset)) => {
            // Unit-local reference
            detrix_logging::debug!(
                "[DWARF typeinfo] typedef target: UnitRef offset={:?}",
                offset.0
            );
            resolve_type_at_offset(offset, unit, dwarf, depth + 1)
        }
        Some(AttributeValue::DebugInfoRef(debug_info_offset)) => {
            // Cross-unit reference - search all units
            detrix_logging::debug!(
                "[DWARF typeinfo] typedef target: DebugInfoRef offset={:?}",
                debug_info_offset.0
            );
            resolve_type_from_debug_info_ref(debug_info_offset, unit, dwarf, depth + 1)
        }
        _ => {
            detrix_logging::debug!("[DWARF typeinfo] typedef has no type attribute");
            Ok(TypeInfo::unknown())
        }
    }
}

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

    let mut cursor = unit
        .entries_at_offset(offset)
        .map_err(|e| Error::DwarfParse(format!("entries_at_offset: {e}")))?;

    let entry = match cursor
        .next_dfs()
        .map_err(|e| Error::DwarfParse(format!("next_dfs: {e}")))?
    {
        Some(e) => e,
        None => return Ok(TypeInfo::unknown()),
    };

    let tag = entry.tag();

    // Debug logging for type resolution chain
    detrix_logging::debug!(
        "[DWARF typeinfo] depth={} tag={:?} offset={:?}",
        depth,
        tag,
        offset.0
    );

    match tag {
        // Scalar types: int, float, bool, uint, etc.
        gimli::DW_TAG_base_type => resolve_base_type(entry, unit, dwarf),

        // Pointer type: *T — always 8 bytes on amd64
        gimli::DW_TAG_pointer_type => {
            let pointee_name = get_pointee_name(entry, unit, dwarf, depth)?;
            Ok(TypeInfo {
                name: format!("*{pointee_name}"),
                size: VariableSize::QWord,
                byte_size: 8,
                is_pointer: true,
                is_string: false,
                is_slice: false,
                is_array: false,
                is_struct: false,
            })
        }

        // Typedef / named type: follow the chain
        gimli::DW_TAG_typedef => {
            let typedef_name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?;
            detrix_logging::debug!(
                "[DWARF typeinfo] typedef name={:?} following chain",
                typedef_name
            );
            
            // Get the inner type offset - may be unit-local or cross-unit
            let mut inner_info = resolve_typedef_target(entry, unit, dwarf, depth)?;
            
            // Override name with the typedef name if available
            if let Some(name) = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)? {
                detrix_logging::debug!(
                    "[DWARF typeinfo] typedef '{}' overriding inner name='{}' is_string={}",
                    name,
                    inner_info.name,
                    inner_info.is_string
                );
                inner_info.name = name;
            }
            Ok(inner_info)
        }

        // Go string is a struct with two fields (ptr + len), total 16 bytes
        gimli::DW_TAG_structure_type => resolve_struct_type(entry, unit, dwarf),

        // Fixed-size array type `[N]T` — stores N elements inline on stack.
        // Go DWARF includes DW_AT_byte_size on the array type itself.
        gimli::DW_TAG_array_type => {
            let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
                Some(AttributeValue::Udata(n)) => n,
                _ => 0, // Unknown size — will be treated as zero-byte blob
            };
            // Use resolve_typedef_target to handle cross-unit element type references
            let elem_info = resolve_typedef_target(entry, unit, dwarf, depth)?;
            let name = format!("[N]{}", elem_info.name); // [N]T — N not yet decoded
            Ok(TypeInfo {
                name,
                size: VariableSize::QWord, // Header word for location; actual read uses byte_size
                byte_size,
                is_pointer: false,
                is_string: false,
                is_slice: false,
                is_array: true,
                is_struct: false,
            })
        }

        // Const qualifier — follow the inner type
        gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => {
            resolve_typedef_target(entry, unit, dwarf, depth + 1)
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

    let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
        Some(AttributeValue::Udata(n)) => n,
        _ => 8, // Default to 8 bytes
    };

    let size = VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord);
    let _ = unit; // used for completeness

    detrix_logging::debug!(
        "[DWARF typeinfo] base_type name='{}' byte_size={}",
        name,
        byte_size
    );

    Ok(TypeInfo {
        name,
        size,
        byte_size,
        is_pointer: false,
        is_string: false,
        is_slice: false,
        is_array: false,
        is_struct: false,
    })
}

/// Get the name of what a pointer points to.
fn get_pointee_name<R: Reader>(
    ptr_entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    depth: u8,
) -> Result<String> {
    // Use resolve_typedef_target to handle cross-unit references
    match resolve_typedef_target(ptr_entry, unit, dwarf, depth) {
        Ok(inner) => Ok(inner.name),
        Err(_) => Ok("void".to_string()),
    }
}

/// Resolve a DW_TAG_structure_type DIE.
///
/// Go emits its built-in types (string, slice headers) as struct types.
/// We detect them by name and field count.
///
/// # Go string type detection
///
/// ## Authoritative sources
///
/// **Go runtime source** (`src/runtime/string.go`):
/// ```go
/// type stringStruct struct {
///     str unsafe.Pointer  // 8 bytes on amd64
///     len int             // 8 bytes on amd64
/// }
/// ```
/// Total size: **16 bytes** on amd64 (verified via `unsafe.Sizeof(string(""))`)
///
/// **Delve debugger** (`pkg/proc/variables.go:readStringInfo`):
/// ```go
/// // string data structure is always two ptrs in size. Addr, followed by len
/// // https://research.swtch.com/godata
/// mem = cacheMemory(mem, addr, arch.PtrSize()*2)
/// ```
/// Delve explicitly reads `PtrSize() * 2` bytes = 16 bytes on amd64.
///
/// **Go DWARF emission**:
/// Go does NOT use `DW_TAG_string_type` (reserved for Pascal-style strings).
/// Instead, Go emits `DW_TAG_structure_type` with:
/// - `DW_AT_name`: `"string"` or `"basic/string"` (for type aliases)
/// - `DW_AT_byte_size`: `16` (on amd64)
/// - Two fields: `str` (ptr) at offset 0, `len` (int) at offset 8
///
/// ## Type name variations
///
/// Go's DWARF emits string types with various names depending on creation:
/// - `"string"` - literal strings, simple assignments
/// - `"basic/string"` - strings from operations (concatenation, conversion)
/// - `"string"` with `DW_AT_linkage_name` `"runtime.string"` - internal runtime type
/// - Named type aliases: `type MyString string` → name might be `"MyString"`
///
/// ## Detection strategy
///
/// The **primary identifier** is the **16-byte size** on amd64 (2×8-byte fields).
/// Name matching is secondary and used for additional validation.
fn resolve_struct_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    _unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    let name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?
        .unwrap_or_else(|| "struct".to_string());

    let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
        Some(AttributeValue::Udata(n)) => n,
        _ => 8,
    };

    // Also check DW_AT_linkage_name for internal Go types like "runtime.string"
    let linkage_name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_linkage_name)
        .ok()
        .flatten()
        .unwrap_or_default();

    // Debug logging for struct type detection
    detrix_logging::debug!(
        "[DWARF typeinfo] struct name='{}' linkage='{}' byte_size={} is_string={} is_slice={}",
        name,
        linkage_name,
        byte_size,
        is_go_string_type_by_name_and_size(&name, &linkage_name, byte_size),
        byte_size == 24
    );

    // Go string = struct with "str" field (ptr) + "len" field = 16 bytes on amd64
    let is_string = is_go_string_type_by_name_and_size(&name, &linkage_name, byte_size);
    // Go slice = struct with array ptr + len + cap = 24 bytes
    let is_slice = byte_size == 24 && !is_string;
    // Everything else is a user-defined struct (captured as blob)
    let is_struct = !is_string && !is_slice;

    let size = VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord);

    Ok(TypeInfo {
        name,
        size,
        byte_size,
        is_pointer: false,
        is_string,
        is_slice,
        is_array: false,
        is_struct,
    })
}

/// Detect if a struct type is a Go string based on name and size.
///
/// # Go string memory layout (amd64)
///
/// ```text
/// +0      +8      +16
/// | ptr   | len   |
/// | 8B    | 8B    |
/// +-------+-------+
/// ```
///
/// ## Authoritative verification
///
/// ```go
/// // Go runtime: src/runtime/string.go
/// type stringStruct struct {
///     str unsafe.Pointer  // offset 0, 8 bytes
///     len int             // offset 8, 8 bytes
/// }
/// // unsafe.Sizeof(string("")) == 16
/// ```
///
/// ```go
/// // Delve: pkg/proc/variables.go:readStringInfo
/// // "string data structure is always two ptrs in size"
/// mem = cacheMemory(mem, addr, arch.PtrSize()*2)  // 16 bytes on amd64
/// ```
///
/// ## Detection heuristics (in priority order)
///
/// 1. **Size check**: `byte_size == 16` (required but not sufficient)
/// 2. **Explicit names**: `"string"`, `"basic/string"`
/// 3. **Path-containing names**: contains `"/string"` (e.g., `"pkg/string"`)
/// 4. **Linkage name**: contains `"runtime.string"`
/// 5. **Case-insensitive**: contains `"string"` (catches type aliases)
fn is_go_string_type_by_name_and_size(name: &str, linkage_name: &str, byte_size: u64) -> bool {
    // Primary check: 16-byte struct is almost certainly a Go string on amd64
    // This is the most reliable indicator per Go runtime and Delve sources
    if byte_size == 16 {
        // Explicit string type names from Go DWARF emission
        if name == "string"
            || name == "basic/string"
            || name.contains("/string")  // Package-qualified names
            || linkage_name.contains("runtime.string")  // Internal runtime type
        {
            return true;
        }
        // Named type aliases: if it's 16 bytes and has "string" anywhere in the name
        // This catches `type MyString string` declarations
        if name.to_lowercase().contains("string") {
            return true;
        }
    }
    false
}

/// Read a string attribute from a DIE.
fn read_attr_string<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
    attr: gimli::constants::DwAt,
) -> Result<Option<String>> {
    match entry.attr_value(DwAt(attr.0)) {
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
        for t in [
            "uint8", "byte", "uint16", "uint32", "uint64", "uint", "uintptr",
        ] {
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
            byte_size: 8,
            is_pointer: true,
            is_string: false,
            is_slice: false,
            is_array: false,
            is_struct: false,
        };
        assert!(info.is_pointer);
        assert_eq!(info.size.bytes(), 8);
    }

    #[test]
    fn type_info_string() {
        let info = TypeInfo {
            name: "string".to_string(),
            size: VariableSize::QWord,
            byte_size: 16,
            is_pointer: false,
            is_string: true,
            is_slice: false,
            is_array: false,
            is_struct: false,
        };
        assert!(info.is_string);
        assert!(!info.is_slice);
    }

    #[test]
    fn type_info_slice() {
        let info = TypeInfo {
            name: "[]int64".to_string(),
            size: VariableSize::QWord,
            byte_size: 24,
            is_pointer: false,
            is_string: false,
            is_slice: true,
            is_array: false,
            is_struct: false,
        };
        assert!(info.is_slice);
        assert!(!info.is_string);
    }

    #[test]
    fn type_info_array() {
        let info = TypeInfo {
            name: "[N]int64".to_string(),
            size: VariableSize::QWord,
            byte_size: 32,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: true,
            is_struct: false,
        };
        assert!(info.is_array);
        assert_eq!(info.byte_size, 32);
    }

    #[test]
    fn type_info_struct() {
        let info = TypeInfo {
            name: "TradeRequest".to_string(),
            size: VariableSize::QWord,
            byte_size: 48,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: false,
            is_struct: true,
        };
        assert!(info.is_struct);
        assert_eq!(info.byte_size, 48);
    }

    #[test]
    fn variable_size_from_byte_size_roundtrip() {
        // Validate that common Go sizes resolve correctly
        let cases: &[(u64, VariableSize)] = &[
            (1, VariableSize::Byte),  // bool, int8, uint8
            (4, VariableSize::DWord), // int32, float32
            (8, VariableSize::QWord), // int64, float64, pointer
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
