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
use crate::error::Result;
use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, Reader, Unit};

/// Maximum depth to recurse (safety limit, matches Delve's default behavior).
const MAX_DEPTH: usize = 10;

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
        return resolve_struct_fields(entry, unit, dwarf, type_info, config, depth);
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

/// Resolve struct fields by traversing DW_TAG_member children.
fn resolve_struct_fields<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    type_info: TypeInfo,
    config: &NestedTypeConfig,
    depth: usize,
) -> Result<NestedType> {
    let mut fields = Vec::new();

    let type_offset = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
    let struct_offset = match type_offset {
        Some(AttributeValue::UnitRef(offset)) => offset,
        Some(AttributeValue::DebugInfoRef(offset)) => {
            return Ok(NestedType::Unsupported {
                type_info,
                reason: format!("cross-unit type reference at offset {:?}", offset.0),
            });
        }
        _ => {
            return Ok(NestedType::Unsupported {
                type_info,
                reason: "no DW_AT_type attribute found".to_string(),
            });
        }
    };

    // Iterate over children of the struct type to find DW_TAG_member entries
    // We need to skip the first entry (the struct type itself) and only process its direct children
    let mut cursor = unit.entries_at_offset(struct_offset)?;
    
    // Skip the struct type entry itself
    cursor.next_dfs()?;
    
    // Now iterate over children
    while let Some(child) = cursor.next_dfs()? {
        if child.tag() != gimli::DW_TAG_member {
            continue;
        }

        if config.max_struct_fields >= 0 && fields.len() >= config.max_struct_fields as usize {
            break;
        }

        let field_name = read_die_name_string(child, dwarf).unwrap_or_else(|| format!("_field{}", fields.len()));

        let byte_offset = match child.attr_value(DwAt(gimli::constants::DW_AT_data_member_location.0)) {
            Some(attr) => {
                match attr {
                    AttributeValue::Udata(n) => n,
                    AttributeValue::Data1(n) => n as u64,
                    AttributeValue::Data2(n) => n as u64,
                    AttributeValue::Data4(n) => n as u64,
                    AttributeValue::Data8(n) => n,
                    _ => 0,
                }
            },
            None => 0,
        };

        let byte_size = match child.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0)) {
            Some(attr) => {
                match attr {
                    AttributeValue::Udata(n) => n,
                    _ => 8,
                }
            },
            None => 8,
        };

        let field_type_info = resolve_type_info(child, unit, dwarf)?;

        fields.push(StructField {
            name: field_name,
            type_info: field_type_info,
            byte_offset,
            byte_size,
        });
    }

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
