//! Go Swiss Table map iterator for eBPF user-space capture.
//!
//! Implements iteration over `internal/runtime/maps.Map` (Go 1.24+).

use crate::dwarf::nested_types::NestedType;
use crate::mem_reader::ProcessMemoryReader;
use crate::probe::types::{CaptureConfig, CapturedValue};
use std::collections::HashSet;

const SLOTS_PER_GROUP: u64 = 8;
const CTRL_EMPTY: u8 = 0x80;
const CTRL_DELETED: u8 = 0xFE;

/// Read entries from a Go 1.24+ Swiss Table map at `map_ptr`.
pub fn read_go_swiss_map(
    map_ptr: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> CapturedValue {
    let key_type_name = key_nested
        .map(|n| n.type_info().name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let val_type_name = val_nested
        .map(|n| n.type_info().name.clone())
        .unwrap_or_else(|| "unknown".to_string());

    match read_swiss_map_inner(map_ptr, key_nested, val_nested, config, mem_reader, pid) {
        Ok(entries) => CapturedValue::Map {
            key_type: key_type_name,
            value_type: val_type_name,
            entries,
            reason: String::new(),
        },
        Err(reason) => CapturedValue::Map {
            key_type: key_type_name,
            value_type: val_type_name,
            entries: vec![],
            reason,
        },
    }
}

fn read_swiss_map_inner(
    map_ptr: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> Result<Vec<(CapturedValue, CapturedValue)>, String> {
    // maps.Map header (amd64):
    //   +0x00: used   (uint64)
    //   +0x08: seed   (uint64)  [unused]
    //   +0x10: dirPtr (uint64)  -> *table OR *[dirLen]*table
    //   +0x18: dirLen (int64)   0 = single table
    let header = mem_reader
        .read_bytes(pid, map_ptr, 32)
        .map_err(|e| format!("map header: {e}"))?;

    if header.len() < 32 {
        return Err(format!("map header too short: {} bytes", header.len()));
    }

    let used = u64::from_le_bytes(header[0..8].try_into().expect("8 bytes"));
    let dir_ptr = u64::from_le_bytes(header[16..24].try_into().expect("8 bytes"));
    let dir_len = i64::from_le_bytes(header[24..32].try_into().expect("8 bytes"));

    detrix_logging::debug!(
        "[map_iter] map={:#x} used={} dirPtr={:#x} dirLen={}",
        map_ptr,
        used,
        dir_ptr,
        dir_len
    );

    if used == 0 || dir_ptr == 0 {
        return Ok(vec![]);
    }

    let max_entries = config.max_array_values;
    let mut entries: Vec<(CapturedValue, CapturedValue)> = Vec::new();
    let mut seen_tables: HashSet<u64> = HashSet::new();

    if dir_len < 0 {
        // Negative dirLen indicates a nil or uninitialised map — return empty with reason.
        detrix_logging::debug!(
            "[map_iter] map={:#x} has negative dirLen={}, treating as nil map",
            map_ptr,
            dir_len
        );
        return Ok(vec![]);
    } else if dir_len == 0 {
        // Small-map mode (Go 1.24 dirLen==0): dirPtr points directly to the
        // raw groups array — [8 ctrl bytes][8 × slot_size bytes].  There is
        // NO table struct wrapper; just 1 group.
        let key_size = slot_type_size(key_nested);
        let val_size = slot_type_size(val_nested);
        let (val_offset, slot_size) = compute_slot_layout(key_size, val_size);
        detrix_logging::debug!(
            "[map_iter] small-map dirPtr={:#x} key_size={} val_size={} slot_size={}",
            dir_ptr,
            key_size,
            val_size,
            slot_size
        );
        read_groups(
            dir_ptr,
            1,
            key_nested,
            val_nested,
            key_size,
            val_size,
            val_offset,
            slot_size,
            max_entries,
            config,
            mem_reader,
            pid,
            &mut entries,
        );
    } else {
        // Directory mode (dirLen > 0): dirPtr → [dirLen]*table.
        // Each *table has a header followed by groups.
        for i in 0..(dir_len as u64) {
            if entries.len() >= max_entries {
                break;
            }
            let slot_addr = dir_ptr + i * 8;
            let table_ptr = match read_u64(mem_reader, pid, slot_addr) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if table_ptr == 0 || !seen_tables.insert(table_ptr) {
                continue;
            }
            if let Err(e) = read_table(
                table_ptr,
                key_nested,
                val_nested,
                max_entries - entries.len(),
                config,
                mem_reader,
                pid,
                &mut entries,
            ) {
                detrix_logging::warn!("[map_iter] read_table failed: {e}");
            }
        }
    }

    Ok(entries)
}

/// Read key/value entries from a table struct (directory mode, dirLen > 0).
///
/// maps.table layout (amd64):
///   +0x00: used (uint16), capacity (uint16), growthLeft (uint16), localDepth (uint8), pad (1)
///   +0x08: index (int64)
///   +0x10: groups.data       (uint64) -> *groups_array
///   +0x18: groups.lengthMask (uint64)  = num_groups - 1
fn read_table(
    table_ptr: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    max_entries: usize,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
    entries: &mut Vec<(CapturedValue, CapturedValue)>,
) -> Result<(), String> {
    let hdr = mem_reader
        .read_bytes(pid, table_ptr, 32)
        .map_err(|e| format!("table header: {e}"))?;

    if hdr.len() < 32 {
        return Err(format!("table header too short: {} bytes", hdr.len()));
    }

    let groups_data = u64::from_le_bytes(hdr[16..24].try_into().expect("8 bytes"));
    let length_mask = u64::from_le_bytes(hdr[24..32].try_into().expect("8 bytes"));
    let num_groups = length_mask.wrapping_add(1);

    if groups_data == 0 {
        return Ok(());
    }

    // Sanity check: more than 1024 groups is unreasonable
    if num_groups > 1024 {
        return Err(format!(
            "table@{table_ptr:#x} num_groups={num_groups} exceeds sanity limit"
        ));
    }

    let key_size = slot_type_size(key_nested);
    let val_size = slot_type_size(val_nested);
    let (val_offset, slot_size) = compute_slot_layout(key_size, val_size);

    detrix_logging::debug!(
        "[map_iter] table={:#x} groups={:#x} num_groups={} key_size={} val_size={} slot_size={}",
        table_ptr,
        groups_data,
        num_groups,
        key_size,
        val_size,
        slot_size
    );

    read_groups(
        groups_data,
        num_groups,
        key_nested,
        val_nested,
        key_size,
        val_size,
        val_offset,
        slot_size,
        max_entries,
        config,
        mem_reader,
        pid,
        entries,
    );
    Ok(())
}

/// Iterate over `num_groups` groups starting at `groups_ptr`.
///
/// Groups layout: [8 ctrl bytes][8 × slot_size slot bytes] repeated num_groups times.
#[allow(clippy::too_many_arguments)]
fn read_groups(
    groups_ptr: u64,
    num_groups: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    key_size: u64,
    val_size: u64,
    val_offset: u64,
    slot_size: u64,
    max_entries: usize,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
    entries: &mut Vec<(CapturedValue, CapturedValue)>,
) {
    let _ = val_size; // stored in slot_size / val_offset already
    let group_size = SLOTS_PER_GROUP + SLOTS_PER_GROUP * slot_size;

    for g in 0..num_groups {
        if entries.len() >= max_entries {
            break;
        }
        let group_ptr = groups_ptr + g * group_size;

        let ctrl = match mem_reader.read_bytes(pid, group_ptr, SLOTS_PER_GROUP as usize) {
            Ok(b) => b,
            Err(e) => {
                detrix_logging::warn!(
                    "[map_iter] Failed to read group ctrl at {:#x}: {e}",
                    group_ptr
                );
                continue;
            }
        };

        for s in 0..(SLOTS_PER_GROUP as usize) {
            if entries.len() >= max_entries {
                break;
            }
            let cb = ctrl[s];
            if cb == CTRL_EMPTY || cb == CTRL_DELETED {
                continue;
            }
            let slot_base = group_ptr + SLOTS_PER_GROUP + s as u64 * slot_size;
            let key_val = read_slot_value(slot_base, key_nested, key_size, config, mem_reader, pid);
            let val_val = read_slot_value(
                slot_base + val_offset,
                val_nested,
                val_size,
                config,
                mem_reader,
                pid,
            );
            entries.push((key_val, val_val));
        }
    }
}

/// Compute (val_offset_in_slot, slot_total_size) using Go's natural alignment rules.
///
/// slot[K,V] in Go has:
///   key at offset 0 (size = key_size)
///   val at offset align_up(key_size, min(val_size, 8))
///   slot_size = align_up(val_offset + val_size, max(key_align, val_align))
fn compute_slot_layout(key_size: u64, val_size: u64) -> (u64, u64) {
    let key_align = align_of(key_size);
    let val_align = align_of(val_size);
    let slot_align = key_align.max(val_align);
    let val_offset = align_up(key_size, val_align);
    let slot_size = align_up(val_offset + val_size, slot_align);
    (val_offset, slot_size)
}

/// Natural alignment for a type of `size` bytes (up to 8).
fn align_of(size: u64) -> u64 {
    match size {
        0 => 1,
        1 => 1,
        2 => 2,
        3 | 4 => 4,
        _ => 8, // 5..=8 and larger types align to 8
    }
}

/// Round `n` up to the next multiple of `align`.
fn align_up(n: u64, align: u64) -> u64 {
    if align == 0 {
        return n;
    }
    n.div_ceil(align) * align
}

/// Byte size to use when reading a slot field.
fn slot_type_size(nested: Option<&NestedType>) -> u64 {
    match nested {
        Some(n) => {
            let s = n.type_info().byte_size;
            if s > 0 {
                s
            } else {
                8
            }
        }
        None => 8,
    }
}

/// Read and decode a single key or value from a slot at `addr`.
fn read_slot_value(
    addr: u64,
    nested: Option<&NestedType>,
    size: u64,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> CapturedValue {
    if size == 0 {
        return CapturedValue::Scalar(0);
    }

    match nested {
        Some(NestedType::Scalar(ti)) if ti.is_string => {
            read_string_slot(addr, mem_reader, pid, config)
        }
        Some(NestedType::Struct {
            type_info, fields, ..
        }) => {
            // Recursively parse struct fields using the address-based parser.
            // We re-use parse_struct_fields_from_addr from ringbuf to avoid duplication.
            use crate::dwarf::nested_types::NestedType as NT;
            let wrapped = NT::Struct {
                type_info: type_info.clone(),
                fields: fields.clone(),
                depth: 0,
            };
            match super::ringbuf::parse_struct_fields_from_addr(
                addr,
                &type_info.name,
                config,
                Some(&wrapped),
                mem_reader,
                pid,
            ) {
                Ok(cv) => cv,
                Err(_) => read_raw_bytes(addr, size, mem_reader, pid),
            }
        }
        Some(NestedType::Scalar(ti)) => {
            // Scalar: read exactly `size` bytes, return as Scalar(u64)
            let bytes = mem_reader.read_bytes(pid, addr, size as usize);
            match bytes {
                Ok(b) => {
                    let val = read_le_u64(&b, size as usize);
                    match ti.name.as_str() {
                        "float64" => CapturedValue::Float(f64::from_bits(val)),
                        "float32" => CapturedValue::Float(f32::from_bits(val as u32) as f64),
                        _ => CapturedValue::Scalar(val),
                    }
                }
                Err(_) => CapturedValue::Error("read failed".to_string()),
            }
        }
        _ => {
            // Unknown or unsupported type: read as raw bytes
            read_raw_bytes(addr, size, mem_reader, pid)
        }
    }
}

/// Read a Go string from a slot (ptr u64, len u64) at `addr`.
fn read_string_slot(
    addr: u64,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
    config: &CaptureConfig,
) -> CapturedValue {
    let hdr = match mem_reader.read_bytes(pid, addr, 16) {
        Ok(b) => b,
        Err(_) => return CapturedValue::Error("string header read failed".to_string()),
    };
    if hdr.len() < 16 {
        return CapturedValue::Error("string header too short".to_string());
    }
    let ptr = u64::from_le_bytes(hdr[0..8].try_into().expect("8 bytes"));
    let len = u64::from_le_bytes(hdr[8..16].try_into().expect("8 bytes")) as usize;

    if ptr == 0 || len == 0 {
        return CapturedValue::String {
            data: Vec::new(),
            len: 0,
        };
    }

    let capped_len = len.min(config.max_string_capture);
    match mem_reader.read_string(pid, ptr, capped_len) {
        Ok(s) => CapturedValue::String {
            data: s.into_bytes(),
            len,
        },
        Err(_) => CapturedValue::Error("string data read failed".to_string()),
    }
}

fn read_raw_bytes(
    addr: u64,
    size: u64,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> CapturedValue {
    match mem_reader.read_bytes(pid, addr, size as usize) {
        Ok(b) => CapturedValue::Bytes(b),
        Err(_) => CapturedValue::Error("read failed".to_string()),
    }
}

fn read_u64(mem_reader: &dyn ProcessMemoryReader, pid: u32, addr: u64) -> Result<u64, String> {
    mem_reader
        .read_u64(pid, addr)
        .map_err(|e| format!("read_u64@{addr:#x}: {e}"))
}

/// Read up to 8 bytes from a little-endian byte slice as u64.
fn read_le_u64(bytes: &[u8], size: usize) -> u64 {
    let mut buf = [0u8; 8];
    let n = size.min(8).min(bytes.len());
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}
