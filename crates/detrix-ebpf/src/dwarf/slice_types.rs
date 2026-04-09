//! Slice type resolution for Go DWARF.
//!
//! Following Delve's pkg/proc/variables.go:loadSliceInfo implementation.

use gimli::{DebuggingInformationEntry, Reader};

/// Extract slice element type information from DWARF.
///
/// Go slices have the structure: { array *T, len int, cap int }
/// The 'array' field points to the underlying array, which contains the element type.
pub fn extract_slice_element_info<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    _unit: &gimli::Unit<R>,
    _dwarf: &gimli::Dwarf<R>,
) -> Option<(String, u64)> {
    // Get the slice type name from DW_AT_name attribute (e.g., "[]OrderItem").
    let slice_type_name = match entry.attr_value(gimli::DwAt(gimli::constants::DW_AT_name.0)) {
        Some(gimli::AttributeValue::DebugStrRef(offset)) => {
            // The offset points into the .debug_str section.
            // Full implementation would read the actual string via dwarf.debug_str.get_str().
            // For now, we use the offset as a unique identifier.
            format!("<str@{:?}>", offset)
        }
        _ => return None,
    };

    // Extract element type from slice name (e.g., "[]OrderItem" → "OrderItem").
    let element_type = if let Some(inner) = slice_type_name.strip_prefix("[]") {
        if inner.is_empty() {
            "unknown".to_string()
        } else {
            inner.to_string()
        }
    } else if slice_type_name.starts_with("<str@") {
        // Placeholder from DebugStrRef offset — can't extract element name.
        "unknown".to_string()
    } else {
        slice_type_name
    };

    // Estimate element size based on type name.
    let element_size = estimate_element_size(&element_type);

    detrix_logging::debug!(
        "[DWARF typeinfo] Slice element type: '{}', estimated byte_size: {}",
        element_type,
        element_size
    );

    Some((element_type, element_size))
}

/// Estimate element size based on type name.
/// Only handles primitive Go types; struct types return a generic default.
/// For accurate struct sizes, callers should rely on DWARF type resolution
/// rather than name-based heuristics.
fn estimate_element_size(type_name: &str) -> u64 {
    match type_name {
        "int" | "int64" | "uint64" | "float64" => 8,
        "int32" | "uint32" | "float32" => 4,
        "int16" | "uint16" => 2,
        "int8" | "uint8" | "byte" => 1,
        "bool" => 1,
        "string" => 16, // string header (ptr + len)
        "uintptr" | "unsafe.Pointer" => 8,
        _ => {
            // Unknown/struct types: use default pointer size.
            // Callers should prefer DWARF-based type resolution for accurate sizes.
            8
        }
    }
}
