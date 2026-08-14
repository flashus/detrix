//! Recursive type resolution for nested struct capture
//!
//! Implements depth-limited DWARF traversal following Delve's patterns.
//!
//! # References from Go Compiler and Delve
//!
//! ## Go Runtime Type Layouts
//!
//! **String** (`src/runtime/string.go:291-294`):
//! ```go
//! type stringStruct struct {
//!     str unsafe.Pointer  // offset 0, 8 bytes on amd64
//!     len int             // offset 8, 8 bytes on amd64
//! }
//! // unsafe.Sizeof(string("")) == 16 on amd64
//! ```
//!
//! **Slice** (`src/runtime/slice.go`):
//! ```go
//! type slice struct {
//!     array unsafe.Pointer  // offset 0
//!     len   int             // offset 8
//!     cap   int             // offset 16
//! }
//! // unsafe.Sizeof(slice{}) == 24 on amd64
//! ```
//!
//! ## Delve DWARF Field Resolution
//!
//! **StructField** (`pkg/dwarf/godwarf/type.go:268`):
//! ```go
//! type StructField struct {
//!     Name       string
//!     Type       Type
//!     ByteOffset int64   // offset from struct base address
//!     ByteSize   int64   // field size in bytes
//! }
//! ```
//!
//! **Field Access** (`pkg/proc/variables.go:849`):
//! ```go
//! func (v *Variable) toField(field *godwarf.StructField) (*Variable, error) {
//!     return v.newVariable(name, uint64(int64(v.Addr)+field.ByteOffset), field.Type, v.mem)
//! }
//! ```
//!
//! **Struct Field Loading** (`pkg/proc/variables.go:1418-1442`):
//! ```go
//! case reflect.Struct:
//!     for i, field := range t.Field {
//!         if cfg.MaxStructFields >= 0 && len(v.Children) >= cfg.MaxStructFields {
//!             break
//!         }
//!         f := v.toField(field)
//!         f.loadValueInternal(recurseLevel+1, cfg)
//!     }
//! ```

use crate::dwarf::typeinfo::{resolve_type_info, TypeInfo};
use crate::error::{Error, Result};
use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, Reader, Unit};

/// Hard safety cap on recursion depth (prevents cycles and stack overflow).
/// Always >= any user-configured max_capture_depth.
/// Shared with `probe::ringbuf::MAX_PARSE_DEPTH` — single source of truth.
pub const MAX_DEPTH: usize = 32;

/// Returns true for type tags that should be transparently followed (typedef chains).
///
/// Mirrors Delve's `SeekToType` which loops through typedefs/qualifiers:
/// `pkg/dwarf/reader/reader.go:SeekToType`
#[inline]
fn is_transparent_type_tag(tag: gimli::DwTag) -> bool {
    matches!(
        tag,
        gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
    )
}

/// Resolved struct field with DWARF-derived offset information.
#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    /// Field name (e.g., "Product", "Trader").
    pub name: String,
    /// Field type info.
    pub type_info: TypeInfo,
    /// Field byte offset from struct base (DW_AT_data_member_location).
    pub byte_offset: u64,
    /// Field byte size (DW_AT_byte_size).
    pub byte_size: u64,
    /// Resolved nested type for pointer fields.
    ///
    /// Populated when `type_info.is_pointer = true` and depth < max_depth.
    /// Contains the pointer's nested type chain ending in `NestedType::Struct`
    /// (after following typedef chains through the pointee).
    ///
    /// Used by the ring buffer parser to dereference pointers and capture
    /// the pointed-to struct's fields instead of the raw pointer value.
    pub nested_type: Option<NestedType>,
}

/// Resolved nested type structure with depth-limited recursion.
#[derive(Debug, Clone, PartialEq)]
pub enum NestedType {
    /// Scalar type (int, float, bool, string).
    Scalar(TypeInfo),
    /// Struct with fields.
    Struct {
        type_info: TypeInfo,
        fields: Vec<StructField>,
        depth: usize,
    },
    /// Array/slice.
    Array {
        type_info: TypeInfo,
        element_type: Box<NestedType>,
        count: Option<u64>,
    },
    /// Map.
    Map {
        type_info: TypeInfo,
        key_type: Box<NestedType>,
        value_type: Box<NestedType>,
    },
    /// Pointer.
    Pointer {
        type_info: TypeInfo,
        pointee: Box<NestedType>,
    },
    /// Interface.
    Interface {
        type_info: TypeInfo,
        concrete_type: Option<String>,
    },
    /// Unsupported or too-deep.
    Unsupported { type_info: TypeInfo, reason: String },
}

impl NestedType {
    pub fn type_info(&self) -> &TypeInfo {
        match self {
            Self::Scalar(t)
            | Self::Struct { type_info: t, .. }
            | Self::Array { type_info: t, .. }
            | Self::Map { type_info: t, .. }
            | Self::Pointer { type_info: t, .. }
            | Self::Interface { type_info: t, .. }
            | Self::Unsupported { type_info: t, .. } => t,
        }
    }

    pub fn depth(&self) -> usize {
        match self {
            Self::Struct { depth, .. } => *depth,
            _ => 0,
        }
    }

    pub fn should_recurse(&self, max_depth: usize) -> bool {
        self.depth() < max_depth
    }
}

/// Configuration for nested type resolution (matches EbpfConfig).
#[derive(Debug, Clone)]
pub struct NestedTypeConfig {
    pub max_depth: usize,
    pub max_struct_fields: i32,
    pub max_elements: usize,
}

impl Default for NestedTypeConfig {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_struct_fields: -1,
            max_elements: 64,
        }
    }
}

/// Resolve nested type with depth-limited DWARF traversal.
pub fn resolve_nested_type<R: Reader>(
    var_entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
) -> Result<NestedType> {
    resolve_nested_type_impl(var_entry, unit, dwarf, config, 0)
}

fn resolve_nested_type_impl<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    if depth >= MAX_DEPTH {
        let type_info = resolve_type_info(entry, unit, dwarf)?;
        return Ok(NestedType::Unsupported {
            type_info,
            reason: format!("max depth ({}) exceeded", MAX_DEPTH),
        });
    }

    if depth > config.max_depth {
        let type_info = resolve_type_info(entry, unit, dwarf)?;
        return Ok(NestedType::Unsupported {
            type_info,
            reason: format!("capture depth limit ({}) exceeded", config.max_depth),
        });
    }

    let type_info = resolve_type_info(entry, unit, dwarf)?;
    classify_type(entry, unit, dwarf, type_info, config, depth)
}

fn classify_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    if type_info.is_string {
        return Ok(NestedType::Scalar(type_info));
    }

    if type_info.is_slice {
        return resolve_slice_type(entry, unit, dwarf, type_info, config, depth);
    }

    if type_info.is_array {
        return resolve_array_type(entry, unit, dwarf, type_info, config, depth);
    }

    if type_info.is_struct {
        // Follow DW_AT_type typedef chain AND resolve fields in one step, keeping
        // the correct unit in scope. Using get_struct_type_entry then resolve_struct_fields
        // separately would drop the target unit before entries_tree can use it.
        detrix_logging::debug!(
            "[nested_types] classify_type: is_struct=true, resolving typedef chain + fields"
        );
        return resolve_struct_following_type_chain(
            entry, unit, dwarf, type_info, config, depth, 0,
        );
    }

    if type_info.name.starts_with("map[") || type_info.name.starts_with("map<") {
        return resolve_map_type(entry, unit, dwarf, type_info, config, depth);
    }

    if type_info.is_pointer {
        return resolve_pointer_type(entry, unit, dwarf, type_info, config, depth);
    }

    if type_info.name == "interface{}" || type_info.name.contains(".interface") {
        return resolve_interface_type(entry, unit, dwarf, type_info, config, depth);
    }

    Ok(NestedType::Scalar(type_info))
}

/// Follow DW_AT_type chain and resolve struct fields while the correct unit is in scope.
///
/// The critical property: `resolve_struct_fields` requires the unit that OWNS the struct
/// entry (so that `entries_tree(struct_offset)` is valid). When following DebugInfoRef
/// chains, the owning unit is `target_unit` — a local variable. We must call
/// `resolve_struct_fields` BEFORE `target_unit` drops.
///
/// This function keeps `target_unit` alive by recursing within its scope.
fn resolve_struct_following_type_chain<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
    chain_depth: usize,
) -> Result<NestedType> {
    if chain_depth >= 8 {
        return Err(Error::DwarfParse(format!(
            "type chain depth {chain_depth} exceeded (cycle?)"
        )));
    }

    let type_attr = match entry.attr_value(DwAt(gimli::constants::DW_AT_type.0)) {
        Some(v) => v,
        None => {
            return Err(Error::DwarfParse(
                "Entry has no DW_AT_type attribute".to_string(),
            ))
        }
    };

    match type_attr {
        AttributeValue::UnitRef(offset) => {
            let mut cursor = unit.entries_at_offset(offset)?;
            let Some(resolved) = cursor.next_dfs()? else {
                return Err(Error::DwarfParse("No entry at UnitRef offset".to_string()));
            };
            let resolved = resolved.clone();
            let tag = resolved.tag();
            detrix_logging::debug!(
                "[nested_types] type chain depth={} UnitRef({:?}) → tag={:?}",
                chain_depth,
                offset,
                tag
            );
            if is_transparent_type_tag(tag) {
                // Still same unit; follow further
                resolve_struct_following_type_chain(
                    &resolved,
                    unit,
                    dwarf,
                    type_info,
                    config,
                    depth,
                    chain_depth + 1,
                )
            } else {
                // Found the struct type entry — unit is correct (UnitRef stays in same CU)
                resolve_struct_fields(&resolved, unit, dwarf, type_info, config, depth)
            }
        }
        AttributeValue::DebugInfoRef(debug_info_offset) => {
            let target_offset = debug_info_offset.0;
            let mut units = dwarf.units();
            while let Some(header) = units.next()? {
                let unit_start = header.offset().0;
                let unit_end = unit_start + header.unit_length();
                if target_offset >= unit_start && target_offset < unit_end {
                    // target_unit lives in this scope — alive when resolve_struct_fields runs ✓
                    let target_unit = dwarf.unit(header)?;
                    let local_offset = gimli::UnitOffset(target_offset - unit_start);
                    let mut cursor = target_unit.entries_at_offset(local_offset)?;
                    let Some(resolved) = cursor.next_dfs()? else {
                        return Err(Error::DwarfParse("No entry in target unit".to_string()));
                    };
                    let resolved = resolved.clone();
                    let tag = resolved.tag();
                    detrix_logging::debug!(
                        "[nested_types] type chain depth={} DebugInfoRef({:?}) → tag={:?}",
                        chain_depth,
                        target_offset,
                        tag
                    );
                    if is_transparent_type_tag(tag) {
                        // Follow further within target_unit (while it's alive in this scope)
                        return resolve_struct_following_type_chain(
                            &resolved,
                            &target_unit,
                            dwarf,
                            type_info,
                            config,
                            depth,
                            chain_depth + 1,
                        );
                    }
                    // Found struct entry — pass target_unit (still alive here) to fields resolution
                    return resolve_struct_fields(
                        &resolved,
                        &target_unit,
                        dwarf,
                        type_info,
                        config,
                        depth,
                    );
                }
            }
            Err(Error::DwarfParse(format!(
                "DebugInfoRef {:?} not found in any unit",
                debug_info_offset
            )))
        }
        _ => Err(Error::DwarfParse(
            "DW_AT_type is not a UnitRef or DebugInfoRef".to_string(),
        )),
    }
}

/// Resolve struct fields by traversing DW_TAG_member children.
///
/// Uses gimli's `entries_tree` API (analogous to Delve's `next()` helper in
/// `pkg/dwarf/godwarf/type.go:600`) which correctly limits traversal to
/// **direct children** of the struct entry only — no depth-tracking needed.
///
/// NOTE: `entry` MUST be the DW_TAG_structure_type entry, not a typedef.
/// Call `get_struct_type_entry()` first to resolve typedef chains.
fn resolve_struct_fields<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    detrix_logging::info!(
        "[nested_types] Resolving struct '{}' fields at offset {:?} tag={:?} has_children={}",
        type_info.name,
        entry.offset(),
        entry.tag(),
        entry.has_children()
    );

    if !entry.has_children() {
        // This means get_struct_type_entry returned a non-struct entry (e.g., still a typedef).
        // Return empty fields rather than silently failing.
        detrix_logging::warn!(
            "[nested_types] struct entry at {:?} has tag={:?} with has_children=false — \
             typedef chain may not have been fully resolved; returning 0 fields",
            entry.offset(),
            entry.tag()
        );
        return Ok(NestedType::Struct {
            type_info,
            fields: vec![],
            depth,
        });
    }

    // entries_tree(Some(offset)) starts a tree rooted at the entry at `offset`.
    // root.children() returns ONLY direct children — correct by construction.
    let mut tree = unit.entries_tree(Some(entry.offset()))?;
    let root = tree.root()?;

    detrix_logging::debug!(
        "[nested_types] entries_tree root: tag={:?} offset={:?}",
        root.entry().tag(),
        root.entry().offset()
    );

    let mut children_iter = root.children();
    let mut fields = Vec::new();
    let mut member_count = 0;

    while let Some(child_node) = children_iter.next()? {
        let child = child_node.entry();

        detrix_logging::debug!(
            "[nested_types] child tag={:?} name={:?}",
            child.tag(),
            read_die_name_string(child, dwarf)
        );

        if child.tag() != gimli::DW_TAG_member {
            continue;
        }

        member_count += 1;
        if config.max_struct_fields >= 0 && fields.len() >= config.max_struct_fields as usize {
            break;
        }

        let field_name =
            read_die_name_string(child, dwarf).unwrap_or_else(|| format!("_field{}", fields.len()));

        let byte_offset =
            match child.attr_value(DwAt(gimli::constants::DW_AT_data_member_location.0)) {
                Some(AttributeValue::Udata(n)) => n,
                Some(AttributeValue::Data1(n)) => n as u64,
                Some(AttributeValue::Data2(n)) => n as u64,
                Some(AttributeValue::Data4(n)) => n as u64,
                Some(AttributeValue::Data8(n)) => n,
                _ => 0,
            };

        let field_type_info = resolve_type_info(child, unit, dwarf)?;

        // Go DWARF does NOT emit DW_AT_byte_size on DW_TAG_member — size lives on the type.
        // Fall back to the type's byte_size (e.g. 40 for [5]float64, 312 for main.Order).
        let byte_size = match child.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
            Some(AttributeValue::Udata(n)) => n,
            _ => field_type_info.byte_size,
        };

        // For pointer, struct, slice, array, and map fields within depth limits, resolve nested type info so
        // the ring buffer parser can dereference pointers, recursively parse embedded structs,
        // read slice elements, iterate array elements, and iterate map buckets.
        let field_nested = if (field_type_info.is_pointer
            || field_type_info.is_struct
            || field_type_info.is_slice
            || field_type_info.is_array
            || field_type_info.is_map)
            && depth < config.max_depth
        {
            detrix_logging::debug!(
                "[nested_types] Resolving nested type for field '{}' (type={}, is_slice={}, is_array={}, depth={}/{})",
                field_name, field_type_info.name, field_type_info.is_slice, field_type_info.is_array, depth, config.max_depth
            );
            let result = resolve_nested_type_impl(child, unit, dwarf, config, depth + 1);
            detrix_logging::debug!(
                "[nested_types] Resolved nested type for field '{}': {:?}",
                field_name,
                result.as_ref().map(|n| match n {
                    NestedType::Array { element_type, .. } =>
                        format!("Array(element={})", element_type.type_info().name),
                    NestedType::Struct { type_info, .. } => format!("Struct({})", type_info.name),
                    NestedType::Scalar(t) => format!("Scalar({})", t.name),
                    NestedType::Pointer { .. } => "Pointer".to_string(),
                    NestedType::Map { .. } => "Map".to_string(),
                    NestedType::Unsupported { reason, .. } => format!("Unsupported({})", reason),
                    _ => "Other".to_string(),
                })
            );
            result.ok()
        } else {
            detrix_logging::debug!(
                "[nested_types] NOT resolving nested type for field '{}' (type={}, is_slice={}, depth={}/{})",
                field_name, field_type_info.name, field_type_info.is_slice, depth, config.max_depth
            );
            None
        };

        detrix_logging::debug!(
            "[nested_types] field '{}' byte_offset={} byte_size={} type='{}' has_nested={}",
            field_name,
            byte_offset,
            byte_size,
            field_type_info.name,
            field_nested.is_some()
        );

        fields.push(StructField {
            name: field_name,
            type_info: field_type_info,
            byte_offset,
            byte_size,
            nested_type: field_nested,
        });
    }

    detrix_logging::info!(
        "[nested_types] Resolved struct '{}' → {} fields (saw {} DW_TAG_member entries)",
        type_info.name,
        fields.len(),
        member_count
    );

    Ok(NestedType::Struct {
        type_info,
        fields,
        depth,
    })
}

fn resolve_slice_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    // For slices, we need to resolve the element type
    // Following Delve's loadSliceInfo which gets ElemType from the array field
    //
    // The 'entry' here is the DW_TAG_member field entry, not the slice struct type.
    // We need to follow the DW_AT_type attribute to get to the slice struct type entry.
    let slice_struct_entry = match entry.attr_value(DwAt(gimli::constants::DW_AT_type.0)) {
        Some(type_attr) => {
            // Follow the type reference to get the slice struct type
            match type_attr {
                AttributeValue::UnitRef(offset) => {
                    let mut cursor = unit.entries_at_offset(offset)?;
                    cursor.next_dfs()?.cloned()
                }
                AttributeValue::DebugInfoRef(debug_info_offset) => {
                    // Cross-unit reference - find the unit and get the entry
                    let target_offset = debug_info_offset.0;
                    let mut units = dwarf.units();
                    let mut result = None;
                    while let Some(header) = units.next()? {
                        let unit_start = header.offset().0;
                        let unit_end = unit_start + header.unit_length();
                        if target_offset >= unit_start && target_offset < unit_end {
                            let target_unit = dwarf.unit(header)?;
                            let local_offset = gimli::UnitOffset(target_offset - unit_start);
                            let mut cursor = target_unit.entries_at_offset(local_offset)?;
                            result = cursor.next_dfs()?.cloned();
                            break;
                        }
                    }
                    result
                }
                _ => None,
            }
        }
        None => None,
    };

    let element_nested_type = if let Some(slice_entry) = slice_struct_entry {
        resolve_slice_element_type(&slice_entry, unit, dwarf, config, depth + 1)?
    } else {
        // Fallback: return unknown type
        NestedType::Scalar(TypeInfo::unknown())
    };

    Ok(NestedType::Array {
        type_info,
        element_type: Box::new(element_nested_type),
        count: None, // Slices have dynamic length
    })
}

/// Resolve the element type of a slice by following the 'array' field pointer.
fn resolve_slice_element_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    detrix_logging::debug!(
        "[nested_types] Resolving slice element type at offset {:?}",
        entry.offset()
    );

    // Iterate over children to find the 'array' field (pointer to element type)
    let mut cursor = unit.entries_at_offset(entry.offset())?;
    let _ = cursor.next_dfs()?; // Skip the struct entry itself

    while let Ok(Some(child)) = cursor.next_dfs() {
        if child.tag() != gimli::DW_TAG_member {
            continue;
        }

        let field_name = read_die_name_string(child, dwarf);
        if field_name.as_deref() != Some("array") {
            continue;
        }

        detrix_logging::debug!("[nested_types] Found slice 'array' field, resolving element type");

        // Get the type of the array field (should be a pointer to element type)
        let type_attr = child.attr_value(DwAt(gimli::constants::DW_AT_type.0));
        if let Some(type_attr) = type_attr {
            // Follow the pointer to get the element type
            let element_type = resolve_pointer_target_type(&type_attr, unit, dwarf, config, depth)?;
            detrix_logging::debug!(
                "[nested_types] Slice element type resolved: {:?}",
                match &element_type {
                    NestedType::Struct { type_info, .. } => format!("Struct({})", type_info.name),
                    NestedType::Scalar(type_info) => format!("Scalar({})", type_info.name),
                    NestedType::Array { type_info, .. } => format!("Array({})", type_info.name),
                    _ => "Other".to_string(),
                }
            );
            return Ok(element_type);
        }
    }

    // Fallback: return unknown scalar type
    detrix_logging::debug!("[nested_types] Slice 'array' field not found, returning unknown type");
    Ok(NestedType::Scalar(TypeInfo::unknown()))
}

/// Resolve the target type of a pointer type attribute.
fn resolve_pointer_target_type<R: Reader>(
    type_attr: &gimli::AttributeValue<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    match type_attr {
        AttributeValue::UnitRef(offset) => {
            let mut cursor = unit.entries_at_offset(*offset)?;
            if let Ok(Some(ptr_entry)) = cursor.next_dfs() {
                if ptr_entry.tag() == gimli::DW_TAG_pointer_type {
                    // Follow the pointer's DW_AT_type
                    if let Some(elem_attr) =
                        ptr_entry.attr_value(DwAt(gimli::constants::DW_AT_type.0))
                    {
                        return resolve_pointer_target_type(&elem_attr, unit, dwarf, config, depth);
                    }
                } else {
                    // The offset points directly to the element type
                    // First resolve type info, then classify
                    let type_info =
                        crate::dwarf::typeinfo::resolve_type_info(ptr_entry, unit, dwarf)?;
                    return classify_type(ptr_entry, unit, dwarf, type_info, config, depth);
                }
            }
        }
        AttributeValue::DebugInfoRef(debug_info_offset) => {
            // Cross-unit reference - find the unit and resolve
            return resolve_cross_unit_type(*debug_info_offset, unit, dwarf, config, depth);
        }
        _ => {}
    }

    Ok(NestedType::Scalar(TypeInfo::unknown()))
}

/// Resolve a type from a cross-unit DebugInfoRef.
fn resolve_cross_unit_type<R: Reader>(
    debug_info_offset: gimli::DebugInfoOffset<R::Offset>,
    _current_unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    let target_offset = debug_info_offset.0;
    let mut units = dwarf.units();

    while let Ok(Some(unit_header)) = units.next() {
        let unit_start = unit_header.offset().0;
        let unit_end = unit_start + unit_header.unit_length();

        if target_offset >= unit_start && target_offset < unit_end {
            let target_unit = dwarf.unit(unit_header)?;
            let local_offset = gimli::UnitOffset(target_offset - unit_start);
            let mut cursor = target_unit.entries_at_offset(local_offset)?;

            if let Ok(Some(entry)) = cursor.next_dfs() {
                // First resolve the type info, then classify
                let type_info =
                    crate::dwarf::typeinfo::resolve_type_info(entry, &target_unit, dwarf)?;
                return classify_type(entry, &target_unit, dwarf, type_info, config, depth);
            }
        }
    }

    Ok(NestedType::Scalar(TypeInfo::unknown()))
}

fn resolve_array_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    // For fixed-size arrays, resolve the element type
    // Following Delve's approach for array element resolution
    let element_nested_type = resolve_array_element_type(entry, unit, dwarf, config, depth + 1)?;

    Ok(NestedType::Array {
        type_info: type_info.clone(),
        element_type: Box::new(element_nested_type),
        count: Some(type_info.array_element_count),
    })
}

/// Resolve the element type of a fixed-size array from DW_TAG_subrange_type.
fn resolve_array_element_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    // Arrays have DW_TAG_subrange_type child that describes the index type
    // The element type comes from DW_AT_type of the array itself
    let type_attr = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    if let Some(type_attr) = type_attr {
        return resolve_pointer_target_type(&type_attr, unit, dwarf, config, depth);
    }

    // Fallback
    Ok(NestedType::Scalar(TypeInfo::unknown()))
}

fn resolve_map_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    // Go custom DWARF attributes for map key and element types.
    // Emitted by the Go compiler on the map typedef DIE.
    const DW_AT_GO_KEY: u16 = 0x2901;
    const DW_AT_GO_ELEM: u16 = 0x2902;

    let key_nested =
        try_resolve_go_map_component(entry, unit, dwarf, config, depth, DwAt(DW_AT_GO_KEY))
            .unwrap_or_else(|| infer_nested_from_map_name(&type_info.name, 0));

    let val_nested =
        try_resolve_go_map_component(entry, unit, dwarf, config, depth, DwAt(DW_AT_GO_ELEM))
            .unwrap_or_else(|| infer_nested_from_map_name(&type_info.name, 1));

    Ok(NestedType::Map {
        type_info,
        key_type: Box::new(key_nested),
        value_type: Box::new(val_nested),
    })
}

/// Try to resolve a map key or value type using a Go-specific DWARF attribute
/// (`DW_AT_go_key` = 0x2901, `DW_AT_go_elem` = 0x2902).
///
/// Searches the member entry itself, then follows one level of `DW_AT_type`
/// (the typedef or pointer) to locate the attribute.
fn try_resolve_go_map_component<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
    attr: DwAt,
) -> Option<NestedType> {
    // Check the entry itself first
    if let Some(val) = entry.attr_value(attr) {
        if let Ok(nt) = resolve_nested_from_attr_value(val, unit, dwarf, config, depth) {
            if nt.type_info().name != "unknown" {
                return Some(nt);
            }
        }
    }

    // Follow DW_AT_type one level (member → typedef or pointer_type)
    let type_attr = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0))?;
    match type_attr {
        AttributeValue::UnitRef(offset) => {
            let mut cursor = unit.entries_at_offset(offset).ok()?;
            let typedef_entry = cursor.next_dfs().ok()??;
            if let Some(val) = typedef_entry.attr_value(attr) {
                if let Ok(nt) = resolve_nested_from_attr_value(val, unit, dwarf, config, depth) {
                    if nt.type_info().name != "unknown" {
                        return Some(nt);
                    }
                }
            }
            // Try one more hop (typedef → pointer → struct)
            if let Some(AttributeValue::UnitRef(inner_offset)) =
                typedef_entry.attr_value(DwAt(gimli::constants::DW_AT_type.0))
            {
                let mut inner_cursor = unit.entries_at_offset(inner_offset).ok()?;
                let inner_entry = inner_cursor.next_dfs().ok()??;
                if let Some(val) = inner_entry.attr_value(attr) {
                    if let Ok(nt) = resolve_nested_from_attr_value(val, unit, dwarf, config, depth)
                    {
                        if nt.type_info().name != "unknown" {
                            return Some(nt);
                        }
                    }
                }
            }
        }
        AttributeValue::DebugInfoRef(di_off) => {
            // Inline cross-unit resolution (can't return entry+unit together due to lifetimes)
            let target = di_off.0;
            let mut units = dwarf.units();
            while let Ok(Some(header)) = units.next() {
                let start = header.offset().0;
                let end = start + header.unit_length();
                if target >= start && target < end {
                    if let Ok(target_unit) = dwarf.unit(header) {
                        let local = gimli::UnitOffset(target - start);
                        if let Ok(mut cursor) = target_unit.entries_at_offset(local) {
                            if let Ok(Some(target_entry)) = cursor.next_dfs() {
                                if let Some(val) = target_entry.attr_value(attr) {
                                    if let Ok(nt) = resolve_nested_from_attr_value(
                                        val,
                                        &target_unit,
                                        dwarf,
                                        config,
                                        depth,
                                    ) {
                                        if nt.type_info().name != "unknown" {
                                            return Some(nt);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
        _ => {}
    }
    None
}

/// Resolve a `NestedType` from a gimli `AttributeValue` that is a type reference.
fn resolve_nested_from_attr_value<R: Reader>(
    val: AttributeValue<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    match val {
        AttributeValue::UnitRef(offset) => {
            let mut cursor = unit.entries_at_offset(offset)?;
            if let Some(e) = cursor.next_dfs()? {
                let ti = resolve_type_info(e, unit, dwarf)?;
                return classify_type(e, unit, dwarf, ti, config, depth);
            }
        }
        AttributeValue::DebugInfoRef(di_off) => {
            return resolve_cross_unit_type(di_off, unit, dwarf, config, depth);
        }
        _ => {}
    }
    Ok(NestedType::Scalar(TypeInfo::unknown()))
}

/// Infer a `NestedType::Scalar` for a map key (component=0) or value (component=1)
/// by parsing the map type name string.
///
/// Handles both `map[K]V` and `map<K,V>` formats.
/// Returns `NestedType::Scalar(TypeInfo::unknown())` when the size cannot be determined.
fn infer_nested_from_map_name(type_name: &str, component: usize) -> NestedType {
    let (key_name, val_name) = match split_map_type_name(type_name) {
        Some(pair) => pair,
        None => return NestedType::Scalar(TypeInfo::unknown()),
    };
    let name = if component == 0 { key_name } else { val_name };
    let size = known_go_type_byte_size(name);
    NestedType::Scalar(TypeInfo {
        name: name.to_string(),
        size: crate::dwarf::types::VariableSize::from_byte_size(size)
            .unwrap_or(crate::dwarf::types::VariableSize::QWord),
        byte_size: size,
        is_pointer: name.starts_with('*'),
        is_string: name == "string",
        is_slice: name.starts_with("[]"),
        is_array: false,
        is_struct: name.contains('.') && !name.starts_with('*') && !name.starts_with("[]"),
        is_map: name.starts_with("map["),
        is_enum: false,
        array_element_count: 0,
        array_element_type: String::new(),
        slice_element_type: String::new(),
        element_byte_size: 0,
        enum_layout: None,
    })
}

/// Parse `map[K]V` or `map<K,V>` into (key_str, val_str).
fn split_map_type_name(s: &str) -> Option<(&str, &str)> {
    if let Some(inner) = s.strip_prefix("map[") {
        let mut depth = 1usize;
        for (i, c) in inner.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&inner[..i], &inner[i + 1..]));
                    }
                }
                _ => {}
            }
        }
    } else if let Some(inner) = s.strip_prefix("map<") {
        let inner = inner.trim_end_matches('>');
        let mut depth = 0usize;
        for (i, c) in inner.char_indices() {
            match c {
                '<' | '[' => depth += 1,
                '>' | ']' => depth -= 1,
                ',' if depth == 0 => {
                    return Some((&inner[..i], inner[i + 1..].trim_start()));
                }
                _ => {}
            }
        }
    }
    None
}

/// Return the known in-memory byte size for common Go types.
/// Returns 8 as a conservative default for unknown/struct types.
fn known_go_type_byte_size(name: &str) -> u64 {
    match name {
        "string" => 16,
        "bool" | "int8" | "uint8" | "byte" => 1,
        "int16" | "uint16" => 2,
        "int32" | "uint32" | "float32" | "rune" => 4,
        "int" | "int64" | "uint" | "uint64" | "uintptr" | "float64" | "complex64" | "error" => 8,
        "complex128" => 16,
        _ if name.starts_with('*') => 8,   // pointer
        _ if name.starts_with("[]") => 24, // slice header
        _ => 8,                            // struct or unknown — conservative
    }
}

fn resolve_pointer_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    let type_attr = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    match type_attr {
        Some(AttributeValue::UnitRef(offset)) => {
            let mut cursor = unit.entries_at_offset(offset)?;
            if let Some(pointee_entry) = cursor.next_dfs()? {
                // Following a pointer does NOT consume depth budget — only entering
                // struct fields does (matching Delve's MaxVariableRecurse semantics).
                let pointee = resolve_nested_type_impl(pointee_entry, unit, dwarf, config, depth)?;
                return Ok(NestedType::Pointer {
                    type_info,
                    pointee: Box::new(pointee),
                });
            }
        }
        Some(AttributeValue::DebugInfoRef(debug_info_offset)) => {
            // Go emits cross-CU type references as DebugInfoRef (absolute DWARF offset).
            // Find the compilation unit that owns this offset, load it, then recurse.
            let target_offset = debug_info_offset.0;
            let mut units = dwarf.units();
            while let Some(header) = units.next()? {
                let unit_start = header.offset().0;
                let unit_end = unit_start + header.unit_length();
                if target_offset >= unit_start && target_offset < unit_end {
                    let target_unit = dwarf.unit(header)?;
                    let local_offset = gimli::UnitOffset(target_offset - unit_start);
                    let mut cursor = target_unit.entries_at_offset(local_offset)?;
                    if let Some(pointee_entry) = cursor.next_dfs()? {
                        detrix_logging::debug!(
                            "[nested_types] resolve_pointer_type DebugInfoRef → tag={:?}",
                            pointee_entry.tag()
                        );
                        // Following a pointer does NOT consume depth budget.
                        let pointee = resolve_nested_type_impl(
                            pointee_entry,
                            &target_unit,
                            dwarf,
                            config,
                            depth, // not depth+1
                        )?;
                        return Ok(NestedType::Pointer {
                            type_info,
                            pointee: Box::new(pointee),
                        });
                    }
                    break;
                }
            }
        }
        _ => {}
    }

    Ok(NestedType::Pointer {
        type_info,
        pointee: Box::new(NestedType::Unsupported {
            type_info: TypeInfo::unknown(),
            reason: "could not resolve pointee type".to_string(),
        }),
    })
}

fn resolve_interface_type<R: Reader>(
    _entry: &DebuggingInformationEntry<R>,
    _unit: &Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    _config: &NestedTypeConfig,
    _depth: usize,
) -> Result<NestedType> {
    Ok(NestedType::Interface {
        type_info,
        concrete_type: None,
    })
}

/// Helper to read a DIE name as a String.
fn read_die_name_string<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Option<String> {
    let attr = entry.attr_value(DwAt(gimli::constants::DW_AT_name.0))?;

    match attr {
        AttributeValue::DebugStrRef(offset) => {
            let s = dwarf.string(offset).ok()?;
            match s.to_string_lossy() {
                Ok(cow) => Some(cow.into_owned()),
                Err(_) => Some("<invalid-utf8>".to_string()),
            }
        }
        AttributeValue::String(ref s) => match s.to_string_lossy() {
            Ok(cow) => Some(cow.into_owned()),
            Err(_) => Some("<invalid-utf8>".to_string()),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_type_config_defaults() {
        let config = NestedTypeConfig::default();
        assert_eq!(config.max_depth, 2);
        assert_eq!(config.max_struct_fields, -1);
        assert_eq!(config.max_elements, 64);
    }

    #[test]
    fn nested_type_depth_tracking() {
        let type_info = TypeInfo::unknown();
        let root = NestedType::Struct {
            type_info: type_info.clone(),
            fields: vec![],
            depth: 0,
        };
        assert_eq!(root.depth(), 0);
        assert!(root.should_recurse(2));

        let deep = NestedType::Struct {
            type_info,
            fields: vec![],
            depth: 5,
        };
        assert_eq!(deep.depth(), 5);
        assert!(!deep.should_recurse(2));
    }
}
