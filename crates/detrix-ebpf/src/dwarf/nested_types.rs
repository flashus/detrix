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

/// Maximum depth to recurse (safety limit, matches Delve's default behavior).
const MAX_DEPTH: usize = 10;

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
    Unsupported {
        type_info: TypeInfo,
        reason: String,
    },
}

impl NestedType {
    pub fn type_info(&self) -> &TypeInfo {
        match self {
            Self::Scalar(t) | Self::Struct { type_info: t, .. } | Self::Array { type_info: t, .. }
            | Self::Map { type_info: t, .. } | Self::Pointer { type_info: t, .. }
            | Self::Interface { type_info: t, .. } | Self::Unsupported { type_info: t, .. } => t,
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
        return resolve_struct_following_type_chain(entry, unit, dwarf, type_info, config, depth, 0);
    }

    if type_info.is_pointer {
        return resolve_pointer_type(entry, unit, dwarf, type_info, config, depth);
    }

    if type_info.name.starts_with("map[") {
        return resolve_map_type(entry, unit, dwarf, type_info, config, depth);
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
                return Err(Error::DwarfParse(
                    "No entry at UnitRef offset".to_string(),
                ));
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
                    &resolved, unit, dwarf, type_info, config, depth, chain_depth + 1,
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
                        return Err(Error::DwarfParse(
                            "No entry in target unit".to_string(),
                        ));
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

        let field_name = read_die_name_string(child, dwarf)
            .unwrap_or_else(|| format!("_field{}", fields.len()));

        let byte_offset = match child.attr_value(DwAt(gimli::constants::DW_AT_data_member_location.0)) {
            Some(AttributeValue::Udata(n)) => n,
            Some(AttributeValue::Data1(n)) => n as u64,
            Some(AttributeValue::Data2(n)) => n as u64,
            Some(AttributeValue::Data4(n)) => n as u64,
            Some(AttributeValue::Data8(n)) => n,
            _ => 0,
        };

        let byte_size = match child.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
            Some(AttributeValue::Udata(n)) => n,
            _ => 8,
        };

        let field_type_info = resolve_type_info(child, unit, dwarf)?;

        detrix_logging::debug!(
            "[nested_types] field '{}' byte_offset={} byte_size={} type='{}'",
            field_name,
            byte_offset,
            byte_size,
            field_type_info.name
        );

        fields.push(StructField {
            name: field_name,
            type_info: field_type_info,
            byte_offset,
            byte_size,
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
    _entry: &DebuggingInformationEntry<R>,
    _unit: &Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    _config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    Ok(NestedType::Struct {
        type_info,
        fields: vec![],
        depth,
    })
}

fn resolve_array_type<R: Reader>(
    _entry: &DebuggingInformationEntry<R>,
    _unit: &Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    _config: &NestedTypeConfig,
    _depth: usize,
) -> Result<NestedType> {
    Ok(NestedType::Scalar(type_info))
}

fn resolve_map_type<R: Reader>(
    _entry: &DebuggingInformationEntry<R>,
    _unit: &Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    _config: &NestedTypeConfig,
    _depth: usize,
) -> Result<NestedType> {
    Ok(NestedType::Unsupported {
        type_info,
        reason: "map capture requires runtime introspection".to_string(),
    })
}

fn resolve_pointer_type<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    let type_offset = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    if let Some(AttributeValue::UnitRef(offset)) = type_offset {
        let mut cursor = unit.entries_at_offset(offset)?;
        if let Some(pointee_entry) = cursor.next_dfs()? {
            let pointee = resolve_nested_type_impl(pointee_entry, unit, dwarf, config, depth + 1)?;
            return Ok(NestedType::Pointer {
                type_info,
                pointee: Box::new(pointee),
            });
        }
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
        AttributeValue::String(ref s) => {
            match s.to_string_lossy() {
                Ok(cow) => Some(cow.into_owned()),
                Err(_) => Some("<invalid-utf8>".to_string()),
            }
        }
        _ => None,
    }
}
