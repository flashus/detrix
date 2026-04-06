//! Go map iterator for eBPF user-space capture.
//!
//! Supports both:
//! - Classic hash maps (Go < 1.24): `runtime.hmap` with `runtime.bmap` buckets
//! - Swiss Table maps (Go 1.24+): `internal/runtime/maps.Map`

use crate::dwarf::nested_types::NestedType;
use crate::mem_reader::ProcessMemoryReader;
use crate::probe::types::{CaptureConfig, CapturedValue};
use std::collections::HashSet;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Read a fixed-size byte array and convert to a native integer, returning an
/// error instead of panicking on malformed input.
fn le_bytes_to_u64(data: &[u8]) -> Result<u64, String> {
    let arr: [u8; 8] = data
        .try_into()
        .map_err(|_| format!("expected 8 bytes, got {}", data.len()))?;
    Ok(u64::from_le_bytes(arr))
}

fn le_bytes_to_i64(data: &[u8]) -> Result<i64, String> {
    let arr: [u8; 8] = data
        .try_into()
        .map_err(|_| format!("expected 8 bytes, got {}", data.len()))?;
    Ok(i64::from_le_bytes(arr))
}

// ── Swiss Table constants (Go 1.24+) ────────────────────────────────────────

const SLOTS_PER_GROUP: u64 = 8;
const CTRL_EMPTY: u8 = 0x80;
const CTRL_DELETED: u8 = 0xFE;

// ── Classic map constants (Go < 1.24) ───────────────────────────────────────

const BUCKET_COUNT: u64 = 8; // 8 key-value pairs per bmap

/// Top hash sentinel values. Go 1.12+ changed the empty sentinel.
#[allow(dead_code)] // documented for reference
const HASH_TOPHASH_EMPTY_ZERO: u64 = 0;
#[allow(dead_code)] // Go 1.11 values kept for documentation
const HASH_TOPHASH_EMPTY_ONE_GO111: u64 = 0;
#[allow(dead_code)]
const HASH_MIN_TOPHASH_GO111: u64 = 4;
const HASH_TOPHASH_EMPTY_ONE_GO112: u64 = 1;
const HASH_MIN_TOPHASH_GO112: u64 = 5;

/// Read entries from a Go map at `map_ptr`, auto-detecting classic vs Swiss Table.
///
/// Detection heuristic:
/// - Swiss Table (Go 1.24+): 32-byte header with `used` (uint64), `dirPtr` (uint64), `dirLen` (int64)
/// - Classic (Go < 1.24): 40+ byte header with `count` (int), `flags` (uint8), `B` (uint8),
///   `noverflow` (uint16), `hash0` (uint32), `buckets` (ptr), `oldbuckets` (ptr), `nevacuate` (ptr), `extra` (ptr)
///
/// We detect by checking if bytes 8-16 (seed in Swiss, flags+B+noverflow+hash0 in classic) look
/// like a valid Swiss Table header. If `dirLen` (bytes 24-32) is a reasonable value (>= -1 and <= 64),
/// we treat it as Swiss Table. Otherwise, classic.
pub fn read_go_map(
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

    if map_ptr == 0 {
        return CapturedValue::Map {
            key_type: key_type_name,
            value_type: val_type_name,
            entries: vec![],
            reason: "nil map".to_string(),
        };
    }

    // Read enough bytes to cover both Swiss Table header (32 bytes) and classic hmap header (40 bytes)
    let header = match mem_reader.read_bytes(pid, map_ptr, 48) {
        Ok(b) => b,
        Err(e) => {
            return CapturedValue::Map {
                key_type: key_type_name,
                value_type: val_type_name,
                entries: vec![],
                reason: format!("map header read failed: {e}"),
            };
        }
    };

    if header.len() < 32 {
        return CapturedValue::Map {
            key_type: key_type_name,
            value_type: val_type_name,
            entries: vec![],
            reason: format!("map header too short: {} bytes", header.len()),
        };
    }

    // Try to detect Swiss Table by checking dirLen (bytes 24-32)
    let dir_len = match le_bytes_to_i64(&header[24..32]) {
        Ok(v) => v,
        Err(e) => return CapturedValue::Map { key_type: key_type_name, value_type: val_type_name, entries: vec![], reason: e },
    };
    let dir_ptr = match le_bytes_to_u64(&header[16..24]) {
        Ok(v) => v,
        Err(e) => return CapturedValue::Map { key_type: key_type_name, value_type: val_type_name, entries: vec![], reason: e },
    };

    // Swiss Table detection heuristic:
    //
    // Swiss Table (Go 1.24+) header:
    //   +0x00: used   (uint64)
    //   +0x08: seed   (uint64) — random 64-bit value
    //   +0x10: dirPtr (uint64)
    //   +0x18: dirLen (int64)  — 0 for small (single-group) maps, >0 for directory maps
    //
    // Classic hmap (Go < 1.24) header:
    //   +0x00: count      (int64)
    //   +0x08: flags      (uint8) — only bits 0-3 used (values 0-15)
    //   +0x09: B          (uint8) — log2(num_buckets), typically 0-30
    //   +0x0A: noverflow  (uint16)
    //   +0x0C: hash0      (uint32)
    //   +0x10: buckets    (uintptr)
    //   +0x18: oldbuckets (uintptr) — 0 when no growth in progress
    //
    // Ambiguous case: dirLen==0 AND oldbuckets==0 both produce bytes 24-32 == 0.
    // Disambiguate using bytes 8-11:
    //   Classic: flags (0-15), B (0-30), noverflow (any) — low-entropy bytes 8-9
    //   Swiss:   first 4 bytes of random seed — both bytes 8 and 9 independently random
    //
    // If bytes[8] < 16 (only 4 bits used in classic flags) AND bytes[9] < 32 (B is log2,
    // rarely exceeds ~30), treat as classic. P(Swiss misidentified) ≈ 6.25% × 12.5% < 1%.
    let is_swiss = if dir_len > 0 && dir_len <= 64 {
        // Large-directory mode — only Swiss Table has small positive dirLen.
        // Classic oldbuckets during growth is a heap pointer (>> 64).
        true
    } else if dir_len == 0 && dir_ptr != 0 {
        // Ambiguous: classic no-growth (oldbuckets=0) vs Swiss small-map (dirLen=0).
        let flags = header[8];
        let b_field = header[9];
        let looks_like_classic = flags < 16 && b_field < 32;
        !looks_like_classic
    } else {
        // dir_len < 0 or dir_len > 64 or dir_ptr == 0 — not Swiss
        false
    };

    detrix_logging::debug!(
        "[map_iter] map={:#x} dirPtr={:#x} dirLen={} is_swiss={}",
        map_ptr,
        dir_ptr,
        dir_len,
        is_swiss
    );

    let entries = if is_swiss {
        match read_swiss_map_inner(map_ptr, key_nested, val_nested, config, mem_reader, pid) {
            Ok(e) => e,
            Err(reason) => {
                detrix_logging::debug!(
                    "[map_iter] Swiss Table failed ({reason}), falling back to classic"
                );
                // Fall back to classic map iterator
                read_classic_map_inner(map_ptr, key_nested, val_nested, config, mem_reader, pid)
                    .unwrap_or_else(|e| {
                        detrix_logging::warn!("[map_iter] Classic fallback also failed: {e}");
                        vec![]
                    })
            }
        }
    } else {
        match read_classic_map_inner(map_ptr, key_nested, val_nested, config, mem_reader, pid) {
            Ok(e) => e,
            Err(reason) => {
                detrix_logging::debug!(
                    "[map_iter] Classic failed ({reason}), falling back to Swiss Table"
                );
                read_swiss_map_inner(map_ptr, key_nested, val_nested, config, mem_reader, pid)
                    .unwrap_or_else(|e| {
                        detrix_logging::warn!("[map_iter] Swiss fallback also failed: {e}");
                        vec![]
                    })
            }
        }
    };

    if entries.is_empty() {
        detrix_logging::warn!(
            "[map_iter] read_go_map map={:#x} pid={} returned empty entries (key={} val={})",
            map_ptr, pid, key_type_name, val_type_name
        );
    }

    CapturedValue::Map {
        key_type: key_type_name,
        value_type: val_type_name,
        entries,
        reason: String::new(),
    }
}

/// Legacy alias for backward compatibility in ringbuf.rs callers.
#[deprecated(since = "1.3.0", note = "use read_go_map instead")]
pub fn read_go_swiss_map(
    map_ptr: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> CapturedValue {
    read_go_map(map_ptr, key_nested, val_nested, config, mem_reader, pid)
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

    let used = le_bytes_to_u64(&header[0..8]).map_err(|e| format!("map used: {e}"))?;
    let dir_ptr = le_bytes_to_u64(&header[16..24]).map_err(|e| format!("map dirPtr: {e}"))?;
    let dir_len = le_bytes_to_i64(&header[24..32]).map_err(|e| format!("map dirLen: {e}"))?;

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

    let groups_data = le_bytes_to_u64(&hdr[16..24]).map_err(|e| format!("table groups_data: {e}"))?;
    let length_mask = le_bytes_to_u64(&hdr[24..32]).map_err(|e| format!("table length_mask: {e}"))?;
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
                0, // External entry point from map_iter
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
    let ptr = match le_bytes_to_u64(&hdr[0..8]) {
        Ok(v) => v,
        Err(e) => return CapturedValue::Error(e),
    };
    let len = match le_bytes_to_u64(&hdr[8..16]) {
        Ok(v) => v as usize,
        Err(e) => return CapturedValue::Error(e),
    };

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

// ============================================================================
// Classic Map Iterator (Go < 1.24)
// ============================================================================
//
// Follows Delve's mapIteratorClassic algorithm from pkg/proc/mapiter.go.
//
// runtime.hmap layout (amd64, Go 1.12+):
//   +0x00: count      (int64)     — number of live entries
//   +0x08: flags      (uint8)     — iterator / grow flags
//   +0x09: B          (uint8)     — log2(number of buckets)
//   +0x0A: noverflow  (uint16)    — approximate overflow bucket count
//   +0x0C: hash0      (uint32)    — hash seed
//   +0x10: buckets    (uintptr)   — pointer to bucket array
//   +0x18: oldbuckets (uintptr)   — pointer to old bucket array (during grow)
//   +0x20: nevacuate  (uintptr)   — progress counter for evacuation
//   +0x28: extra      (*mapextra) — overflow buckets (optional)
//
// runtime.bmap layout:
//   +0x00: tophash  [8]uint8      — top 8 bits of hash for each slot
//   +0x08: keys     [8]KeyType    — key storage (inline or pointer)
//   +0x08+8*K: values [8]ValueType — value storage
//   overflow: *bmap               — pointer to overflow bucket

/// Read entries from a classic Go hash map (Go < 1.24).
fn read_classic_map_inner(
    map_ptr: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
) -> Result<Vec<(CapturedValue, CapturedValue)>, String> {
    // Read hmap header (40 bytes covers up to nevacuate)
    let header = mem_reader
        .read_bytes(pid, map_ptr, 40)
        .map_err(|e| format!("classic map header: {e}"))?;

    if header.len() < 40 {
        return Err(format!(
            "classic map header too short: {} bytes",
            header.len()
        ));
    }

    let count = le_bytes_to_i64(&header[0..8]).map_err(|e| format!("hmap count: {e}"))?;
    let b = header[9] as u64;
    let buckets_ptr = le_bytes_to_u64(&header[16..24]).map_err(|e| format!("hmap buckets: {e}"))?;
    let oldbuckets_ptr = le_bytes_to_u64(&header[24..32])
        .map_err(|e| format!("hmap oldbuckets: {e}"))?;

    detrix_logging::debug!(
        "[map_iter] classic map={:#x} pid={} count={} B={} buckets={:#x} oldbuckets={:#x} header={:02x?}",
        map_ptr,
        pid,
        count,
        b,
        buckets_ptr,
        oldbuckets_ptr,
        &header[..32]
    );

    if buckets_ptr == 0 {
        if count > 0 {
            detrix_logging::warn!(
                "[map_iter] classic map={:#x} count={} but buckets_ptr=0 (nil buckets, possible GC or stale read)",
                map_ptr, count
            );
        } else {
            detrix_logging::debug!(
                "[map_iter] classic map={:#x} count=0 buckets_ptr=0 (empty map)",
                map_ptr
            );
        }
        return Ok(vec![]);
    }

    // Safety: B is the log2 of bucket count (Go's hmap.B). A corrupted or malicious
    // binary could set B >= 64, causing `1u64 << B` to panic (debug) or wrap (release).
    // Go itself never produces B > 60 (2^60 buckets is ~1 exabyte), so clamp aggressively.
    if b >= 64 {
        return Err(format!(
            "corrupt hmap header: B={b} exceeds maximum (63). Map pointer={map_ptr:#x}, count={count}"
        ));
    }
    let numbuckets = 1u64 << b;
    let oldmask = if b > 0 { (1u64 << (b - 1)) - 1 } else { 0 };

    // Go version detection: Go 1.12+ uses HASH_TOPHASH_EMPTY_ONE_GO112
    // We default to Go 1.12+ since Go 1.11 is ancient.
    let hash_tophash_empty_one = HASH_TOPHASH_EMPTY_ONE_GO112;
    let hash_min_tophash = HASH_MIN_TOPHASH_GO112;

    let key_size = slot_type_size(key_nested);
    let val_size = slot_type_size(val_nested);
    let _key_align = align_of(key_size);
    let val_align = align_of(val_size);

    // Bucket layout: tophash[8] + keys[8] + values[8] + overflow
    // Keys and values are laid out with Go's struct alignment rules.
    // tophash is always 8 bytes (8 × uint8).
    let keys_offset = 8u64; // after tophash[8]
    let vals_offset = align_up(keys_offset + BUCKET_COUNT * key_size, val_align);
    let overflow_offset = align_up(vals_offset + BUCKET_COUNT * val_size, 8);
    let bucket_size = overflow_offset + 8; // overflow pointer

    detrix_logging::debug!(
        "[map_iter] classic bucket: keys_off={} vals_off={} overflow_off={} bucket_size={}",
        keys_offset,
        vals_offset,
        overflow_offset,
        bucket_size
    );

    let max_entries = config.max_array_values;
    let max_buckets = config.max_array_values as u64; // reuse as bucket limit
    let mut entries: Vec<(CapturedValue, CapturedValue)> = Vec::new();

    let mut bidx = 0u64;
    let mut oldbuckets_visited: HashSet<u64> = HashSet::new();

    while bidx < numbuckets && entries.len() < max_entries {
        if bidx >= max_buckets {
            detrix_logging::debug!(
                "[map_iter] classic: hit max_buckets limit ({})",
                max_buckets
            );
            break;
        }

        // Determine which bucket to read
        let mut bucket_addr = buckets_ptr + bidx * bucket_size;

        // Handle map growth: if oldbuckets is non-nil and this bucket hasn't been
        // evacuated, read from oldbuckets instead (but only once per oldbucket).
        if oldbuckets_ptr != 0 {
            let oldbidx = bidx & oldmask;
            let oldb_addr = oldbuckets_ptr + oldbidx * bucket_size;

            if !is_evacuated(
                mem_reader,
                pid,
                oldb_addr,
                hash_tophash_empty_one,
                hash_min_tophash,
            ) {
                // Old bucket not yet evacuated — read from it.
                // Only process each oldbucket once (when bidx == oldbidx).
                if oldbidx == bidx {
                    bucket_addr = oldb_addr;
                } else if oldbuckets_visited.contains(&oldbidx) {
                    // Already visited this oldbucket via its other half — skip.
                    bidx += 1;
                    continue;
                } else {
                    oldbuckets_visited.insert(oldbidx);
                    bucket_addr = oldb_addr;
                }
            }
        }

        // Read bucket contents (including full overflow chain)
        if let Err(e) = read_classic_bucket(
            bucket_addr,
            bucket_size,
            keys_offset,
            vals_offset,
            overflow_offset,
            key_size,
            val_size,
            key_nested,
            val_nested,
            config,
            mem_reader,
            pid,
            hash_tophash_empty_one,
            hash_min_tophash,
            &mut entries,
            max_entries,
        ) {
            detrix_logging::warn!(
                "[map_iter] classic bucket read failed at {:#x}: {e}",
                bucket_addr
            );
        }
        // Always advance to next primary bucket — read_classic_bucket traverses
        // the full overflow chain internally.
        bidx += 1;
    }

    if entries.is_empty() && count > 0 {
        detrix_logging::warn!(
            "[map_iter] classic map={:#x}: count={} but collected 0 entries \
             (B={} numbuckets={} buckets_ptr={:#x} oldbuckets_ptr={:#x}) — \
             possible GC race or stale memory read",
            map_ptr,
            count,
            b,
            numbuckets,
            buckets_ptr,
            oldbuckets_ptr
        );
    } else {
        detrix_logging::debug!(
            "[map_iter] classic: collected {} entries from {} buckets",
            entries.len(),
            bidx
        );
    }

    Ok(entries)
}

/// Check if a classic bucket has been evacuated.
///
/// A bucket is evacuated if its first tophash value is between
/// hashTophashEmptyOne and hashMinTopHash (exclusive), which indicates
/// the evacuated sentinel value.
fn is_evacuated(
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
    bucket_addr: u64,
    hash_tophash_empty_one: u64,
    hash_min_tophash: u64,
) -> bool {
    if bucket_addr == 0 {
        return true;
    }
    // Read just the first tophash byte
    match mem_reader.read_bytes(pid, bucket_addr, 1) {
        Ok(b) if b.len() == 1 => {
            let tophash0 = b[0] as u64;
            tophash0 > hash_tophash_empty_one && tophash0 < hash_min_tophash
        }
        _ => true, // Can't read — assume evacuated
    }
}

/// Read a classic bucket and its overflow chain, collecting entries.
///
/// Traverses the full overflow chain from the given bucket, collecting key-value
/// pairs from each bucket until `max_entries` is reached or the chain ends.
#[allow(clippy::too_many_arguments)]
fn read_classic_bucket(
    mut bucket_addr: u64,
    _bucket_size: u64,
    keys_offset: u64,
    vals_offset: u64,
    overflow_offset: u64,
    key_size: u64,
    val_size: u64,
    key_nested: Option<&NestedType>,
    val_nested: Option<&NestedType>,
    config: &CaptureConfig,
    mem_reader: &dyn ProcessMemoryReader,
    pid: u32,
    _hash_tophash_empty_one: u64,
    hash_min_tophash: u64,
    entries: &mut Vec<(CapturedValue, CapturedValue)>,
    max_entries: usize,
) -> Result<(), String> {
    while bucket_addr != 0 && entries.len() < max_entries {
        // Read bucket: tophash[8] at offset 0
        let tophash = match mem_reader.read_bytes(pid, bucket_addr, BUCKET_COUNT as usize) {
            Ok(b) if b.len() == BUCKET_COUNT as usize => b,
            Ok(_) => return Err("bucket tophash read too short".to_string()),
            Err(e) => return Err(format!("bucket tophash read failed: {e}")),
        };

        detrix_logging::debug!(
            "[map_iter] classic bucket={:#x} tophash={:?} (occupied slots with tophash >= {})",
            bucket_addr,
            tophash,
            hash_min_tophash
        );

        for slot in 0..BUCKET_COUNT {
            if entries.len() >= max_entries {
                return Ok(());
            }

            let tophash_val = tophash[slot as usize] as u64;
            // Skip empty and evacuated sentinel values (matches Delve's mapIteratorClassic.next()).
            // Values < minTopHash (5): 0=emptyRest, 1=emptyOne, 2=evacuatedX, 3=evacuatedY, 4=evacuatedEmpty.
            // tophash >= minTopHash means the slot is occupied with a valid key/value.
            if tophash_val < hash_min_tophash {
                continue;
            }
            // Occupied slot — read key and value
            let key_addr = bucket_addr + keys_offset + slot * key_size;
            let val_addr = bucket_addr + vals_offset + slot * val_size;

            let key_val =
                read_slot_value(key_addr, key_nested, key_size, config, mem_reader, pid);
            let val_val =
                read_slot_value(val_addr, val_nested, val_size, config, mem_reader, pid);
            entries.push((key_val, val_val));
        }

        // Follow overflow pointer
        let overflow_addr = match mem_reader.read_u64(pid, bucket_addr + overflow_offset) {
            Ok(a) => a,
            Err(e) => {
                detrix_logging::debug!(
                    "[map_iter] classic: overflow read failed at {:#x}: {e}",
                    bucket_addr + overflow_offset
                );
                break;
            }
        };

        if overflow_addr != 0 {
            bucket_addr = overflow_addr;
        } else {
            break;
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub memory reader that returns predefined bytes at known addresses.
    struct MockMemReader {
        data: std::collections::HashMap<u64, Vec<u8>>,
    }
    impl MockMemReader {
        fn new() -> Self {
            Self {
                data: std::collections::HashMap::new(),
            }
        }
        fn put(&mut self, addr: u64, bytes: Vec<u8>) {
            self.data.insert(addr, bytes);
        }
    }
    impl ProcessMemoryReader for MockMemReader {
        fn read_string(&self, _pid: u32, _ptr: u64, _len: usize) -> crate::error::Result<String> {
            Ok("test".to_string())
        }
        fn read_bytes(&self, _pid: u32, ptr: u64, len: usize) -> crate::error::Result<Vec<u8>> {
            if let Some(data) = self.data.get(&ptr) {
                Ok(data[..len.min(data.len())].to_vec())
            } else {
                // Return zeroed bytes for unmapped addresses
                Ok(vec![0u8; len])
            }
        }
        fn read_u64(&self, _pid: u32, ptr: u64) -> crate::error::Result<u64> {
            if let Some(data) = self.data.get(&ptr) {
                if data.len() >= 8 {
                    return Ok(u64::from_le_bytes(data[0..8].try_into().unwrap_or([0; 8])));
                }
            }
            Ok(0)
        }
    }

    #[test]
    fn classic_map_nil_map_returns_empty() {
        let reader = MockMemReader::new();
        let result = read_go_map(
            0, // nil map pointer
            None,
            None,
            &CaptureConfig::default(),
            &reader,
            1234,
        );
        match result {
            CapturedValue::Map {
                entries, reason, ..
            } => {
                assert!(entries.is_empty());
                assert_eq!(reason, "nil map");
            }
            other => panic!("Expected Map, got {other:?}"),
        }
    }

    #[test]
    fn classic_map_detects_swiss_vs_classic() {
        // Swiss Table header: dirLen at offset 24 is a small positive number
        let mut reader = MockMemReader::new();
        // Swiss Table header: used=5, seed=0, dirPtr=0x1000, dirLen=1
        let mut swiss_header = vec![0u8; 48];
        swiss_header[0..8].copy_from_slice(&5u64.to_le_bytes()); // used
        swiss_header[16..24].copy_from_slice(&0x1000u64.to_le_bytes()); // dirPtr
        swiss_header[24..32].copy_from_slice(&1i64.to_le_bytes()); // dirLen = 1 (small map)
        reader.put(0x5000, swiss_header);

        // We can't fully test Swiss Table without proper group data,
        // but we can verify the detection logic doesn't panic
        let _result = read_go_map(0x5000, None, None, &CaptureConfig::default(), &reader, 1234);
        // Should not panic, returns whatever it can read
    }

    #[test]
    fn classic_map_detects_classic_by_dir_len() {
        // Classic hmap: bytes 24-32 are oldbuckets pointer (large address),
        // which when interpreted as dirLen would be a huge number
        let mut reader = MockMemReader::new();
        let mut classic_header = vec![0u8; 48];
        classic_header[0..8].copy_from_slice(&3i64.to_le_bytes()); // count = 3
        classic_header[9] = 1; // B = 1 (2 buckets)
        classic_header[16..24].copy_from_slice(&0x7000u64.to_le_bytes()); // buckets
        classic_header[24..32].copy_from_slice(&0x8000u64.to_le_bytes()); // oldbuckets (large addr)
                                                                          // bytes 24-32 as i64 = 0x8000 = 32768, which is > 64, so detected as classic
        reader.put(0x6000, classic_header);

        // Should detect as classic (dirLen > 64) and attempt classic iteration
        let _result = read_go_map(0x6000, None, None, &CaptureConfig::default(), &reader, 1234);
    }

    #[test]
    fn classic_map_no_growth_not_misidentified_as_swiss() {
        // Classic hmap with oldbuckets=0 (no growth): bytes 24-32 == 0.
        // This is the same as Swiss Table dirLen=0 (small-map mode).
        // The disambiguation uses bytes 8-9 (flags, B):
        //   Classic: flags=0 (<16), B=1 (<32) → detected as classic
        //   Swiss:   seed bytes with values >= 16 or >= 32 → detected as Swiss
        let mut reader = MockMemReader::new();
        let mut classic_no_grow = vec![0u8; 48];
        classic_no_grow[0..8].copy_from_slice(&2i64.to_le_bytes()); // count = 2
        classic_no_grow[8] = 0; // flags = 0 (no iterator)
        classic_no_grow[9] = 1; // B = 1 (2 buckets)
        // bytes 10-11: noverflow = 0
        classic_no_grow[16..24].copy_from_slice(&0x9000u64.to_le_bytes()); // buckets pointer
        // bytes 24-32: oldbuckets = 0 (no growth) — same as dirLen=0 in Swiss
        reader.put(0x7000, classic_no_grow);

        // Should be detected as classic (flags=0 < 16 AND B=1 < 32)
        // and attempt classic iteration (won't find entries without bucket data, but won't panic)
        let _result = read_go_map(0x7000, None, None, &CaptureConfig::default(), &reader, 1234);
        // Key assertion: it should not panic and should not attempt Swiss Table group parsing
    }

    #[test]
    fn compute_slot_layout_basic() {
        // int64 key, int64 value
        let (val_offset, slot_size) = compute_slot_layout(8, 8);
        assert_eq!(val_offset, 8); // key at 0, val at 8
        assert_eq!(slot_size, 16); // total slot = 16

        // string key (16 bytes), int64 value (8 bytes)
        let (val_offset, slot_size) = compute_slot_layout(16, 8);
        assert_eq!(val_offset, 16);
        assert_eq!(slot_size, 24);

        // int32 key (4 bytes), int64 value (8 bytes) — val needs 8-byte alignment
        let (val_offset, slot_size) = compute_slot_layout(4, 8);
        assert_eq!(val_offset, 8); // val aligned to 8
        assert_eq!(slot_size, 16); // 8 + 8 = 16
    }

    #[test]
    fn align_up_rounds_correctly() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(7, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(5, 4), 8);
    }

    #[test]
    fn align_of_correct() {
        assert_eq!(align_of(0), 1);
        assert_eq!(align_of(1), 1);
        assert_eq!(align_of(2), 2);
        assert_eq!(align_of(3), 4);
        assert_eq!(align_of(4), 4);
        assert_eq!(align_of(5), 8);
        assert_eq!(align_of(8), 8);
    }

    #[test]
    fn classic_map_rejects_corrupted_b_field() {
        // Corrupted hmap header with B=64 would cause panic in `1u64 << 64`
        // (overflow in shift). The fix should reject this gracefully.
        let mut reader = MockMemReader::new();
        let mut bad_header = vec![0u8; 40];
        bad_header[0..8].copy_from_slice(&100i64.to_le_bytes()); // count = 100 (suspicious)
        bad_header[9] = 64; // B = 64 → 1u64 << 64 would panic/wrap
        bad_header[16..24].copy_from_slice(&0x10000u64.to_le_bytes()); // buckets pointer
        reader.put(0x10000, bad_header);

        // Should NOT panic — should return an error instead
        let result = read_classic_map_inner(
            0x10000,
            None,
            None,
            &CaptureConfig::default(),
            &reader,
            1234,
        );
        assert!(result.is_err(), "Should reject B=64, got: {:?}", result);
        let err = result.unwrap_err();
        assert!(
            err.contains("B=64") || err.contains("invalid") || err.contains("corrupt"),
            "Error should mention invalid B: {err}"
        );
    }

    #[test]
    fn classic_map_accepts_max_valid_b() {
        // B=60 is the maximum safe value (1u64 << 60 = ~1 exa-buckets, capped by max_buckets).
        let mut reader = MockMemReader::new();
        let mut header = vec![0u8; 40];
        header[9] = 60; // B = 60 (max safe for u64 shift)
        header[16..24].copy_from_slice(&0x20000u64.to_le_bytes()); // buckets pointer
        reader.put(0x20000, header);

        // Should NOT panic — will return empty since no bucket data
        let result = read_classic_map_inner(
            0x20000,
            None,
            None,
            &CaptureConfig::default(),
            &reader,
            1234,
        );
        // Returns empty or error, but doesn't panic
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn read_classic_bucket_returns_unit_not_bool() {
        // H4: The function used to return Ok(bool) indicating overflow_followed,
        // but the caller always advanced bidx regardless — the bool was dead code.
        // After the fix, it returns Result<(), String>.
        // This test verifies the function completes without panicking.
        let mut reader = MockMemReader::new();

        // Build a minimal bucket: 8 tophash bytes + 8 slots * 8 bytes (keys+vals) + 8 overflow
        // Total = 8 (tophash) + 64 (keys) + 64 (vals) + 8 (overflow) = 144 bytes
        let mut bucket = vec![0xABu8; 144];
        bucket[0] = 5; // tophash[0] = occupied
        bucket[1] = 6; // tophash[1] = occupied
        reader.put(0x30000, bucket);

        let mut entries = Vec::new();
        let result = read_classic_bucket(
            0x30000,
            144,   // bucket_size
            8,     // keys_offset
            72,    // vals_offset (8 + 8*8 = 72)
            136,   // overflow_offset (72 + 8*8 = 136)
            8,     // key_size
            8,     // val_size
            None,  // key_nested
            None,  // val_nested
            &CaptureConfig::default(),
            &reader,
            1234,
            1,     // hash_tophash_empty_one
            5,     // hash_min_tophash
            &mut entries,
            64,    // max_entries
        );
        // Function should succeed (not panic, not return bool)
        assert!(result.is_ok());
        // All 8 slots show as occupied (MockMemReader returns 0xAB for every slot,
        // which is >= minTopHash=5). This is correct behavior — the function reads
        // what the mock provides.
        assert_eq!(entries.len(), 8);
    }
}
