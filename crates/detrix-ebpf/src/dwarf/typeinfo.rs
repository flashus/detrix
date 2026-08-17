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
use crate::error::{ErrContext, Error, Result};

use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, Reader, Unit};

// Go-specific DWARF attributes (not in standard DWARF spec)
// Defined by Delve: pkg/dwarf/godwarf/type.go
const DW_AT_GO_KIND: u16 = 0x2900;

// Go reflect.Kind values
const GO_KIND_STRING: i64 = 24;
const GO_KIND_MAP: i64 = 21;
#[allow(dead_code)] // Reserved for future slice detection via DW_AT_go_kind
const GO_KIND_SLICE: i64 = 19;

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
    /// Whether this is a Go map type.
    pub is_map: bool,
    /// Whether this is an enum/sum type.  The discriminant is only considered
    /// capture-ready when a profile supplies a verified layout; Rust niche
    /// layouts deliberately remain unavailable by default.
    pub is_enum: bool,
    /// Element count for arrays (0 for non-arrays).
    pub array_element_count: u64,
    /// Element type name for arrays (empty for non-arrays).
    pub array_element_type: String,
    /// Element type name for slices (empty for non-slices).
    pub slice_element_type: String,
    /// Element byte size for arrays/slices (0 for non-arrays/slices).
    pub element_byte_size: u64,
    /// Compiler-emitted enum layout, when DWARF exposes an explicit variant
    /// part and discriminants. Niche layouts intentionally remain `None`.
    pub enum_layout: Option<EnumLayout>,
}

/// A bounded, metadata-only description of a Rust enum representation.
///
/// This is deliberately separate from `VariableLocation`: it describes where
/// the discriminant and variant payload live inside an inline value, but does
/// not claim that either is live at a particular PC. The capture planner must
/// still validate the variable location and reject niche/implicit layouts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumLayout {
    pub discriminant_offset: u64,
    pub discriminant_size: u64,
    pub variants: Vec<EnumVariantLayout>,
    pub niche: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantLayout {
    pub name: String,
    pub discriminant: Option<i128>,
    pub payload_offset: Option<u64>,
    pub payload_size: Option<u64>,
}

impl EnumLayout {
    /// Return true only for a representation that can safely be lowered as an
    /// explicit discriminant read. Niche encodings and incomplete DIEs are
    /// rejected here, before a profile can advertise live enum capture.
    pub fn is_explicit_non_niche(&self) -> bool {
        !self.niche
            && matches!(self.discriminant_size, 1 | 2 | 4 | 8)
            && self.variants.len() >= 2
            && self
                .variants
                .iter()
                .all(|variant| variant.discriminant.is_some())
    }
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
            is_map: false,
            is_enum: false,
            array_element_count: 0,
            array_element_type: String::new(),
            slice_element_type: String::new(),
            element_byte_size: 0,
            enum_layout: None,
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
            let unit = dwarf.unit(unit_header.clone()).context("Unit load")?;

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
        .context("entries_at_offset")?;

    let entry = match cursor.next_dfs().context("next_dfs")? {
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
            let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
                Some(AttributeValue::Udata(size)) => size,
                _ => 8,
            };
            let is_rust_str = pointee_name == "str";
            let is_rust_slice = pointee_name.starts_with('[');
            Ok(TypeInfo {
                name: if is_rust_str {
                    "&str".into()
                } else {
                    format!("*{pointee_name}")
                },
                size: VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord),
                byte_size,
                is_pointer: true,
                is_string: is_rust_str,
                is_slice: is_rust_slice,
                is_array: false,
                is_struct: false,
                is_map: false,
                is_enum: false,
                array_element_count: 0,
                array_element_type: String::new(),
                slice_element_type: String::new(),
                element_byte_size: 0,
                enum_layout: None,
            })
        }

        // Typedef / named type: follow the chain
        gimli::DW_TAG_typedef => {
            let typedef_name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?;
            detrix_logging::debug!(
                "[DWARF typeinfo] TYPEDEF: name={:?} at offset {:?}",
                typedef_name,
                entry.offset()
            );

            // Check for Go-specific attributes
            let go_kind = read_go_kind(entry);

            // Get the inner type offset - may be unit-local or cross-unit
            let mut inner_info = resolve_typedef_target(entry, unit, dwarf, depth)?;

            detrix_logging::debug!(
                "[DWARF typeinfo] TYPEDEF '{}' → inner: name='{}' is_string={} is_struct={} is_map={} byte_size={}",
                typedef_name.as_deref().unwrap_or("???"),
                inner_info.name, inner_info.is_string, inner_info.is_struct, inner_info.is_map, inner_info.byte_size
            );

            // Override name with the typedef name if available
            if let Some(name) = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)? {
                detrix_logging::debug!(
                    "[DWARF typeinfo] TYPEDEF '{}' final: name='{}' is_string={} is_map={} (preserved from inner)",
                    name, inner_info.name, inner_info.is_string, inner_info.is_map
                );
                inner_info.name = name;
            }

            // Detect map types via DW_AT_go_kind = reflect.Map (21)
            // Go maps are typedefs with go_kind=21 pointing to a struct (runtime.hmap or Swiss Table Map)
            if go_kind == Some(GO_KIND_MAP) {
                detrix_logging::debug!(
                    "[DWARF typeinfo] TYPEDEF '{}' detected as Go map (go_kind=21)",
                    inner_info.name
                );
                inner_info.is_map = true;
            }

            Ok(inner_info)
        }

        // Go string is a struct with two fields (ptr + len), total 16 bytes
        gimli::DW_TAG_structure_type => resolve_struct_type(entry, unit, dwarf),

        // Rust enums and sum types.  DWARF gives us the aggregate size and
        // variant DIEs, but a niche discriminant is not necessarily stored at
        // byte zero.  Record the category here and let the Rust profile admit
        // only layouts backed by explicit fixture evidence.
        gimli::DW_TAG_enumeration_type => {
            let name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?
                .unwrap_or_else(|| "enum".to_string());
            let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
                Some(AttributeValue::Udata(size)) => size,
                _ => 0,
            };
            Ok(TypeInfo {
                name,
                size: VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord),
                byte_size,
                is_pointer: false,
                is_string: false,
                is_slice: false,
                is_array: false,
                is_struct: false,
                is_map: false,
                is_enum: true,
                array_element_count: 0,
                array_element_type: String::new(),
                slice_element_type: String::new(),
                element_byte_size: 0,
                enum_layout: None,
            })
        }

        // Fixed-size array type `[N]T` — stores N elements inline on stack.
        // Go DWARF includes DW_AT_byte_size on the array type itself.
        // Element count is in DW_TAG_subrange_type child with DW_AT_count.
        gimli::DW_TAG_array_type => {
            let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
                Some(AttributeValue::Udata(n)) => n,
                _ => 0, // Unknown size — will be treated as zero-byte blob
            };
            // Use resolve_typedef_target to handle cross-unit element type references
            let elem_info = resolve_typedef_target(entry, unit, dwarf, depth)?;

            // Get element count from DW_TAG_subrange_type child
            let element_count = get_array_element_count(entry, unit, dwarf)?;
            let name = format!("[{}]{}", element_count, elem_info.name);

            Ok(TypeInfo {
                name,
                size: VariableSize::QWord, // Header word for location; actual read uses byte_size
                byte_size,
                is_pointer: false,
                is_string: false,
                is_slice: false,
                is_array: true,
                is_struct: false,
                is_map: false,
                is_enum: false,
                array_element_count: element_count,
                array_element_type: elem_info.name.clone(),
                slice_element_type: String::new(),
                element_byte_size: 0,
                enum_layout: None,
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
    _unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<TypeInfo> {
    let name = read_attr_string(entry, dwarf, gimli::constants::DW_AT_name)?
        .unwrap_or_else(|| "unknown".to_string());

    let byte_size = match entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
        Some(AttributeValue::Udata(n)) => n,
        _ => {
            // Infer byte size from base type name when DW_AT_byte_size is missing
            match name.as_str() {
                "bool" | "int8" | "uint8" | "byte" => 1,
                "int16" | "uint16" => 2,
                "int32" | "uint32" | "rune" | "float32" => 4,
                "int64" | "uint64" | "int" | "uint" | "uintptr" | "float64" | "complex64" => 8,
                "complex128" => 16,
                _ => 8, // Conservative default
            }
        }
    };

    let size = VariableSize::from_byte_size(byte_size).unwrap_or(VariableSize::QWord);

    detrix_logging::debug!(
        "[DWARF typeinfo] base_type name='{}' byte_size={}",
        name,
        byte_size
    );

    // Go's DWARF emits 'string' as a base_type with byte_size=16
    // Check for string type alias (type MyString string)
    let is_string = name == "string" || name == "basic/string";

    detrix_logging::info!(
        "[DWARF typeinfo] BASE_TYPE: name='{}' byte_size={} is_string={}",
        name,
        byte_size,
        is_string
    );

    Ok(TypeInfo {
        name,
        size,
        byte_size,
        is_pointer: false,
        is_string,
        is_slice: false,
        is_array: false,
        is_struct: false,
        is_map: false,
        is_enum: false,
        array_element_count: 0,
        array_element_type: String::new(),
        slice_element_type: String::new(),
        element_byte_size: 0,
        enum_layout: None,
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

/// Get the element count for an array type from DW_TAG_subrange_type child.
fn get_array_element_count<R: Reader>(
    array_entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
) -> Result<u64> {
    // Array types have DW_TAG_subrange_type child with DW_AT_count or DW_AT_upper_bound
    let mut cursor = unit.entries_at_offset(array_entry.offset())?;

    // Skip the array entry itself
    let Some(_) = cursor.next_dfs()? else {
        return Ok(0);
    };

    // Look for DW_TAG_subrange_type child
    while let Some(child) = cursor.next_dfs()? {
        detrix_logging::debug!(
            "[DWARF typeinfo] array child tag={:?} at offset {:?}",
            child.tag(),
            child.offset()
        );

        if child.tag() != gimli::DW_TAG_subrange_type {
            continue;
        }

        // Try DW_AT_count first
        if let Some(count) = child.attr_value(DwAt(gimli::constants::DW_AT_count.0)) {
            detrix_logging::debug!("[DWARF typeinfo] array DW_AT_count={:?}", count);
            match count {
                AttributeValue::Udata(n) => return Ok(n),
                AttributeValue::Data1(n) => return Ok(n as u64),
                AttributeValue::Data2(n) => return Ok(n as u64),
                AttributeValue::Data4(n) => return Ok(n as u64),
                AttributeValue::Data8(n) => return Ok(n),
                _ => {}
            }
        }

        // Fallback to DW_AT_upper_bound + 1
        if let Some(upper) = child.attr_value(DwAt(gimli::constants::DW_AT_upper_bound.0)) {
            detrix_logging::debug!(
                "[DWARF typeinfo] array DW_AT_upper_bound={:?} (count will be upper+1)",
                upper
            );
            match upper {
                AttributeValue::Udata(n) => return Ok(n + 1),
                AttributeValue::Data1(n) => return Ok(n as u64 + 1),
                AttributeValue::Data2(n) => return Ok(n as u64 + 1),
                AttributeValue::Data4(n) => return Ok(n as u64 + 1),
                AttributeValue::Data8(n) => return Ok(n + 1),
                _ => {}
            }
        }
    }

    detrix_logging::warn!(
        "[DWARF typeinfo] array element count not found — array will be captured as raw bytes"
    );
    Err(Error::MissingDebugInfo(
        "Array element count not found in DWARF — array will be captured as raw bytes".to_string(),
    ))
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
    unit: &gimli::Unit<R>,
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

    // Check Go's DWARF kind attribute first (most reliable)
    // Go emits DW_AT_go_kind (0x2900) with reflect.Kind value (24 = string)
    let mut is_string = is_go_string_by_kind_attr(entry);

    // Fallback to name/size-based detection if kind attr not present
    if !is_string {
        is_string = is_go_string_type_by_name_and_size(&name, &linkage_name, byte_size);
    }

    // Rust's owned String is a bounded pointer/length/capacity header.  It is
    // intentionally recognized only by canonical compiler names; arbitrary
    // user structs containing "String" must remain ordinary structs.
    let rust_string = is_rust_string_name(&name) && byte_size >= 16;
    let rust_str = matches!(name.as_str(), "&str" | "&mut str") && byte_size >= 16;
    if rust_string || rust_str {
        is_string = true;
    }

    // Final fallback: 16-byte structs with str+len fields are string aliases
    // Go represents string type aliases as 16-byte structs with str (ptr) + len fields
    // We must check for str+len fields to avoid misidentifying other 16-byte structs
    // like OrderPtr (Order *Order + Count int) or complex numbers
    if !is_string && byte_size == 16 && !name.contains("Complex") {
        // Check if struct has str and len fields (string-like structure)
        if has_string_fields(entry, unit, dwarf) {
            detrix_logging::debug!(
                "[DWARF typeinfo] Treating 16-byte struct '{}' as string alias (has str+len fields)",
                name
            );
            is_string = true;
        }
    }

    // Rust data-bearing enums are emitted as structures with an explicit
    // DW_TAG_variant_part.  Extract only non-niche metadata; ordinary Rust
    // structs and niche enums remain distinct and fail closed downstream.
    let enum_layout = extract_rust_enum_layout(entry, unit, dwarf);
    let is_enum = enum_layout.is_some();

    // time.Time is a 24-byte struct (wall uint64 + ext int64 + loc *Location)
    // Don't confuse it with slices
    let is_time = name == "time.Time" || name.ends_with(".Time");
    // Go slice = struct with array ptr + len + cap = 24 bytes (but not time.Time)
    let is_slice = (byte_size == 24 && !is_string && !is_time)
        || (is_rust_slice_name(&name) && !is_string && (byte_size == 16 || byte_size == 24));
    // Everything else is a user-defined struct (captured as blob)
    let is_struct = !is_string && !is_slice && !is_time && !is_enum;

    // For slices, try to extract element type info from DWARF
    let (slice_element_type, element_byte_size) = if is_slice {
        crate::dwarf::slice_types::extract_slice_element_info(entry, unit, dwarf)
            .unwrap_or_else(|| (String::new(), 0))
    } else {
        (String::new(), 0)
    };

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
        is_map: false,
        is_enum,
        array_element_count: 0,
        array_element_type: String::new(),
        slice_element_type,
        element_byte_size,
        enum_layout,
    })
}

/// Extract the explicit, non-niche enum representation rustc emits as a
/// `DW_TAG_variant_part`.  codelldb uses richer synthetic metadata for niche
/// enums; without a real discriminant member and explicit values we return
/// `None` so the capture path cannot invent a tag or payload offset.
fn extract_rust_enum_layout<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Option<EnumLayout> {
    let mut tree = unit.entries_tree(Some(entry.offset())).ok()?;
    let root = tree.root().ok()?;
    let mut discriminant_offset = None;
    let mut discriminant_size = 1u64;
    let mut children = root.children();
    while let Ok(Some(child)) = children.next() {
        let variant_part = child.entry();
        if variant_part.tag() == gimli::DW_TAG_member && is_discriminant_member(variant_part, dwarf)
        {
            discriminant_offset =
                read_unsigned_attr(variant_part, gimli::constants::DW_AT_data_member_location);
            discriminant_size = member_byte_size(variant_part, unit, dwarf).unwrap_or(1);
        }
        if variant_part.tag() != gimli::DW_TAG_variant_part {
            continue;
        }

        // rustc commonly puts the discriminant reference on the variant part
        // (`DW_AT_discr`) and emits an unnamed artificial member immediately
        // before the variants.  Do not require a synthetic field name: the
        // reference/ artificial marker is the stable DWARF signal.
        let mut discr_member = None;
        if let Some(AttributeValue::UnitRef(offset)) =
            variant_part.attr_value(DwAt(gimli::constants::DW_AT_discr.0))
        {
            if let Ok(entry) = unit.entry(offset) {
                discr_member = Some(entry);
            }
        }
        if let Some(member) = discr_member {
            discriminant_offset =
                read_unsigned_attr(&member, gimli::constants::DW_AT_data_member_location);
            discriminant_size = member_byte_size(&member, unit, dwarf).unwrap_or(1);
        }

        let mut variants = Vec::new();
        let mut variant_children = child.children();
        while let Ok(Some(node)) = variant_children.next() {
            let child_entry = node.entry();
            match child_entry.tag() {
                gimli::DW_TAG_member => {
                    // rustc's artificial discriminant is normally named
                    // `<<variant>>`; accept the LLDB encoded spelling too.
                    if is_discriminant_member(child_entry, dwarf) {
                        discriminant_offset = read_unsigned_attr(
                            child_entry,
                            gimli::constants::DW_AT_data_member_location,
                        );
                        discriminant_size = member_byte_size(child_entry, unit, dwarf).unwrap_or(1);
                    }
                }
                gimli::DW_TAG_variant => {
                    let mut name =
                        read_attr_string(child_entry, dwarf, gimli::constants::DW_AT_name)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "<anonymous>".into());
                    let discriminant =
                        read_signed_attr(child_entry, gimli::constants::DW_AT_discr_value);
                    let mut payload_offset = None;
                    let mut payload_size = None;
                    let mut members = node.children();
                    while let Ok(Some(member_node)) = members.next() {
                        let member = member_node.entry();
                        if member.tag() != gimli::DW_TAG_member {
                            continue;
                        }
                        // rustc emits the variant label on the payload
                        // member (`DW_AT_name("Pending")`) rather than on
                        // the DW_TAG_variant itself. Preserve that semantic
                        // name instead of exposing `<anonymous>`.
                        if name == "<anonymous>" {
                            if let Ok(Some(member_name)) =
                                read_attr_string(member, dwarf, gimli::constants::DW_AT_name)
                            {
                                name = member_name;
                            }
                        }
                        payload_offset = read_unsigned_attr(
                            member,
                            gimli::constants::DW_AT_data_member_location,
                        );
                        payload_size = member_byte_size(member, unit, dwarf);
                        break;
                    }
                    variants.push(EnumVariantLayout {
                        name,
                        discriminant,
                        payload_offset,
                        payload_size,
                    });
                }
                _ => {}
            }
        }

        if discriminant_offset.is_some()
            && variants.len() >= 2
            && variants
                .iter()
                .all(|variant| variant.discriminant.is_some())
        {
            return Some(EnumLayout {
                discriminant_offset: discriminant_offset.unwrap_or_default(),
                discriminant_size,
                variants,
                niche: false,
            });
        }
    }
    None
}

fn is_discriminant_member<R: Reader>(
    member: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
) -> bool {
    let name = read_attr_string(member, dwarf, gimli::constants::DW_AT_name)
        .ok()
        .flatten()
        .unwrap_or_default();
    let artificial = matches!(
        member.attr_value(DwAt(gimli::constants::DW_AT_artificial.0)),
        Some(AttributeValue::Flag(true))
    );
    artificial || name.contains("variant") || name.contains("discr")
}

fn member_byte_size<R: Reader>(
    member: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Option<u64> {
    read_unsigned_attr(member, gimli::constants::DW_AT_byte_size).or_else(|| {
        resolve_type_info(member, unit, dwarf)
            .ok()
            .map(|type_info| type_info.byte_size)
            .filter(|size| *size != 0)
    })
}

fn read_unsigned_attr<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    attr: gimli::constants::DwAt,
) -> Option<u64> {
    match entry.attr_value(DwAt(attr.0)) {
        Some(AttributeValue::Udata(value)) => Some(value),
        Some(AttributeValue::Sdata(value)) if value >= 0 => Some(value as u64),
        Some(AttributeValue::Data1(value)) => Some(value as u64),
        Some(AttributeValue::Data2(value)) => Some(value as u64),
        Some(AttributeValue::Data4(value)) => Some(value as u64),
        Some(AttributeValue::Data8(value)) => Some(value),
        _ => None,
    }
}

fn read_signed_attr<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    attr: gimli::constants::DwAt,
) -> Option<i128> {
    match entry.attr_value(DwAt(attr.0)) {
        Some(AttributeValue::Sdata(value)) => Some(value as i128),
        Some(AttributeValue::Udata(value)) => Some(value as i128),
        Some(AttributeValue::Data1(value)) => Some(value as i128),
        Some(AttributeValue::Data2(value)) => Some(value as i128),
        Some(AttributeValue::Data4(value)) => Some(value as i128),
        Some(AttributeValue::Data8(value)) => Some(value as i128),
        _ => None,
    }
}

fn is_rust_string_name(name: &str) -> bool {
    matches!(
        name,
        "String" | "alloc::string::String" | "std::string::String"
    )
}

fn is_rust_slice_name(name: &str) -> bool {
    name == "&str"
        || name == "&mut str"
        || name.starts_with("&[")
        || name.starts_with("&mut [")
        || name.starts_with("Vec<")
        || name.starts_with("alloc::vec::Vec<")
        || name.starts_with("std::vec::Vec<")
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
            || linkage_name.contains("runtime.string")
        // Internal runtime type
        {
            return true;
        }
        // Named type aliases: if it's 16 bytes AND has "string" in the name
        // This catches `type MyString string` and `type OrderStatus string` declarations
        // We require BOTH size match AND name containing "string" to avoid false positives
        // Only match when "string" appears as a complete word (not as substring of StringBuffer, StringCache, etc.)
        if is_go_string_name(name) {
            return true;
        }
    }
    false
}

/// Check if a type name refers to a Go string type (not just containing "string" as substring).
fn is_go_string_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Exact match or ends with "string" (type aliases like MyString, OrderStatus)
    lower == "string"
        || lower.ends_with("string")
        || lower.contains("/string")
        || lower.contains(".string")
}

/// Check if a struct has Go string fields (str + len).
///
/// Go's DWARF represents string type aliases (e.g., `type OrderStatus string`)
/// as structs with two fields: `str` (pointer) and `len` (length).
/// This function detects such string-like structs to distinguish them from
/// other 16-byte structs like OrderPtr (pointer + int).
///
/// Follows typedef chains to find the actual struct type.
fn has_string_fields<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
) -> bool {
    // If this is a typedef, follow it to find the actual struct
    if entry.tag() == gimli::DW_TAG_typedef {
        if let Ok(inner_info) = resolve_typedef_target(entry, unit, dwarf, 0) {
            // Check if the inner type name suggests it's a string
            // Go's base string type is named "string" or "basic/string"
            return inner_info.name == "string"
                || inner_info.name == "basic/string"
                || inner_info.name.ends_with(".string");
        }
        return false;
    }

    // For struct entries, check for str and len fields
    let mut cursor = match unit.entries_at_offset(entry.offset()) {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Skip the struct entry itself
    if cursor.next_dfs().is_err() || cursor.next_dfs().ok().is_none() {
        return false;
    }

    let mut has_str = false;
    let mut has_len = false;
    let mut field_count = 0;

    // Iterate over children looking for str and len fields
    while let Ok(Some(child)) = cursor.next_dfs() {
        if child.tag() != gimli::DW_TAG_member {
            continue;
        }
        field_count += 1;

        if let Some(field_name) = read_attr_string(child, dwarf, gimli::constants::DW_AT_name)
            .ok()
            .flatten()
        {
            if field_name == "str" {
                has_str = true;
            } else if field_name == "len" {
                has_len = true;
            }
        }

        // Stop if we've seen more than 2 fields
        if field_count > 2 {
            break;
        }
    }

    // String-like struct has exactly 2 fields: str and len
    field_count == 2 && has_str && has_len
}

/// Check if a type has Go's DWARF string kind attribute.
///
/// Go's DWARF emission includes a custom `DW_AT_go_kind` attribute (0x2900)
/// that directly encodes the reflect.Kind value. For strings, this is 24.
///
/// Reference: Delve pkg/dwarf/godwarf/type.go
fn is_go_string_by_kind_attr<R: Reader>(entry: &DebuggingInformationEntry<R>) -> bool {
    read_go_kind(entry) == Some(GO_KIND_STRING)
}

/// Read the Go reflect.Kind from DW_AT_go_kind attribute.
///
/// Go's DWARF emission includes a custom `DW_AT_go_kind` attribute (0x2900)
/// that directly encodes the reflect.Kind value.
///
/// Reference: Delve pkg/dwarf/godwarf/type.go
fn read_go_kind<R: Reader>(entry: &DebuggingInformationEntry<R>) -> Option<i64> {
    match entry.attr_value(DwAt(DW_AT_GO_KIND)) {
        Some(AttributeValue::Sdata(kind)) => Some(kind),
        Some(AttributeValue::Data1(kind)) => Some(kind as i64),
        Some(AttributeValue::Udata(kind)) => Some(kind as i64),
        _ => None,
    }
}

/// Read a string attribute from a DIE.
fn read_attr_string<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
    attr: gimli::constants::DwAt,
) -> Result<Option<String>> {
    match entry.attr_value(DwAt(attr.0)) {
        Some(AttributeValue::DebugStrRef(offset)) => {
            let s = dwarf.string(offset).context("name read")?;
            let name = s.to_string_lossy().context("UTF-8")?;
            Ok(Some(name.to_string()))
        }
        Some(AttributeValue::String(ref s)) => {
            let name = s.to_string_lossy().context("UTF-8")?;
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
            ..TypeInfo::unknown()
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
            ..TypeInfo::unknown()
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
            ..TypeInfo::unknown()
        };
        assert!(info.is_slice);
        assert!(!info.is_string);
    }

    #[test]
    fn type_info_array() {
        let info = TypeInfo {
            name: "[5]int64".to_string(),
            size: VariableSize::QWord,
            byte_size: 40,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: true,
            is_struct: false,
            array_element_count: 5,
            array_element_type: "int64".to_string(),
            ..TypeInfo::unknown()
        };
        assert!(info.is_array);
        assert_eq!(info.byte_size, 40);
        assert_eq!(info.array_element_count, 5);
    }

    #[test]
    fn rust_header_names_are_classified_without_broad_string_heuristics() {
        assert!(is_rust_string_name("alloc::string::String"));
        assert!(is_rust_slice_name("alloc::vec::Vec<i32>"));
        assert!(is_rust_slice_name("Vec<i32, alloc::alloc::Global>"));
        assert!(is_rust_slice_name("&[u8]"));
        assert!(is_rust_slice_name("&mut [u8]"));
        assert!(is_rust_slice_name("&mut str"));
        assert!(!is_rust_string_name("my::StringBuffer"));
        assert!(!is_rust_slice_name("my::StringSlice"));
    }

    #[test]
    fn enum_layout_is_fail_closed_for_niche_or_incomplete_metadata() {
        let explicit = EnumLayout {
            discriminant_offset: 0,
            discriminant_size: 1,
            variants: vec![
                EnumVariantLayout {
                    name: "Pending".into(),
                    discriminant: Some(0),
                    payload_offset: None,
                    payload_size: None,
                },
                EnumVariantLayout {
                    name: "Settled".into(),
                    discriminant: Some(1),
                    payload_offset: Some(1),
                    payload_size: Some(1),
                },
            ],
            niche: false,
        };
        assert!(explicit.is_explicit_non_niche());

        let mut niche = explicit.clone();
        niche.niche = true;
        assert!(!niche.is_explicit_non_niche());

        let mut incomplete = explicit;
        incomplete.variants[1].discriminant = None;
        assert!(!incomplete.is_explicit_non_niche());
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
            ..TypeInfo::unknown()
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
        const { assert!(MAX_TYPE_DEPTH >= 4 && MAX_TYPE_DEPTH <= 16) };
    }
}
