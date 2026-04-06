//! DWARF parser for Go ELF binaries
//!
//! Resolves source file:line to program counter (PC) and extracts
//! variable locations from DWARF debug info. This is the foundation
//! for generating eBPF uprobe programs that capture variable values.
//!
//! Uses `gimli` for DWARF parsing and `object` for ELF reading.

use super::typeinfo::{resolve_type_info, TypeInfo};
use super::types::{ProbePoint, ProgramCounter, ResolvedVariable, VariableLocation, VariableSize};
use crate::error::{Error, Result};

use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, EndianSlice, Reader};
use object::{CompressionFormat, Object, ObjectSection};
use std::path::{Path, PathBuf};

/// Parsed Go binary with DWARF debug info ready for probe point resolution.
#[derive(Debug)]
pub struct DwarfInfo {
    /// Path to the ELF binary.
    binary_path: PathBuf,
    /// Raw binary data (kept alive for gimli references).
    _data: Vec<u8>,
    /// VMA of the .text section — used to convert DWARF virtual PCs to offsets.
    text_base: u64,
    /// File offset of the .text section — aya uprobe attachment uses file offsets.
    /// symbol_offset = text_file_offset + (pc - text_base)
    text_file_offset: u64,
}

impl DwarfInfo {
    /// Parse a Go ELF binary and extract DWARF debug info.
    ///
    /// The binary must be compiled with `-gcflags=all=-N -l` for stable
    /// DWARF variable locations.
    pub fn parse(binary_path: impl AsRef<Path>) -> Result<Self> {
        let path = binary_path.as_ref().to_path_buf();
        let data = std::fs::read(&path).map_err(|e| {
            Error::DwarfParse(format!("Failed to read binary {}: {e}", path.display()))
        })?;

        let obj = object::File::parse(&*data).map_err(|e| {
            Error::DwarfParse(format!("Failed to parse ELF {}: {e}", path.display()))
        })?;

        let text_section = obj.section_by_name(".text");
        // VMA of .text — used to convert DWARF virtual addresses to offsets.
        let text_vma = text_section.as_ref().map(|s| s.address()).unwrap_or(0);
        // File offset of .text — aya uprobe attachment uses file offsets, not VMAs.
        // file_offset(pc) = text_file_offset + (pc - text_vma)
        let text_file_offset = text_section
            .as_ref()
            .and_then(|s| s.file_range())
            .map(|(off, _)| off)
            .unwrap_or(0);

        Ok(Self {
            binary_path: path,
            _data: data,
            text_base: text_vma,
            text_file_offset,
        })
    }

    /// Resolve a source location to a probe point with variable locations.
    ///
    /// This is the main entry point for the DWARF module. Given a file and line,
    /// it returns the PC to attach the uprobe and the variables available at that PC.
    ///
    /// `max_nested_depth` controls how many levels of nested struct fields are resolved.
    /// Follows Delve's `MaxVariableRecurse` semantics: pointer chasing is free, each
    /// struct field level consumes one depth unit. Default: 2.
    pub fn resolve_probe_point(
        &self,
        file: &str,
        line: u32,
        requested_vars: &[String],
        max_nested_depth: usize,
    ) -> Result<ProbePoint> {
        let obj = object::File::parse(&*self._data)
            .map_err(|e| Error::DwarfParse(format!("Failed to parse ELF: {e}")))?;

        let endian = if obj.is_little_endian() {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };

        let dwarf = load_dwarf(&obj, endian)?;

        // Step 1: Resolve file:line → PC via .debug_line
        let pc = resolve_line_to_pc(&dwarf, file, line)?;

        // Step 2: Find the containing function
        let function_name = find_function_at_pc(&dwarf, pc)?;

        // Step 3: Compute CFA-to-SP delta at the uprobe PC so that DW_OP_fbreg offsets
        // (CFA-relative) can be converted to SP-relative (what BPF ctx->sp refers to).
        // Returns 0 if .debug_frame is missing/unreadable, which keeps old behaviour.
        let cfa_sp_delta = get_cfa_sp_delta(&obj, endian, pc);
        detrix_logging::debug!(
            "[DWARF CFI] PC={:#x} cfa_sp_delta={} (CFA = SP + {})",
            pc,
            cfa_sp_delta,
            cfa_sp_delta
        );

        // Step 4: Resolve variable locations at this PC
        let variables =
            resolve_variables_at_pc(&dwarf, pc, requested_vars, cfa_sp_delta, max_nested_depth)?;

        // aya uprobe attach(fn_name=None, offset, ...) expects a file offset,
        // not a virtual address. Convert: file_offset = text_file_offset + (pc - text_vma).
        let symbol_offset = self
            .text_file_offset
            .saturating_add(pc.saturating_sub(self.text_base));

        Ok(ProbePoint {
            binary_path: self.binary_path.clone(),
            pc,
            symbol_offset,
            function_name,
            variables,
        })
    }
}

/// Load DWARF debug sections from an ELF object.
///
/// Returns an `EndianSlice`-backed Dwarf. The binary must have uncompressed DWARF
/// sections (i.e. built with `-ldflags="-compressdwarf=false"`) because
/// `EndianSlice` borrows directly from the ELF bytes — it cannot hold decompressed
/// (Cow::Owned) data. The Dockerfile.ebpf-test build disables compression.
fn load_dwarf<'a>(
    obj: &'a object::File<'a>,
    endian: gimli::RunTimeEndian,
) -> Result<gimli::Dwarf<EndianSlice<'a, gimli::RunTimeEndian>>> {
    // Check for compressed DWARF sections before attempting to load.
    // `EndianSlice` borrows directly from the ELF bytes and cannot hold
    // decompressed (Cow::Owned) data.
    for section in obj.sections() {
        let name = section.name().unwrap_or("");
        if name.starts_with(".debug_") || name.starts_with(".zdebug_") {
            if let Ok(compressed) = section.compressed_data() {
                if compressed.format != CompressionFormat::None {
                    return Err(Error::DwarfParse(
                        "Compressed DWARF is not supported. \
                         Build with -ldflags=\"-compressdwarf=false\" -gcflags=\"all=-N -l\" \
                         to produce uncompressed DWARF with stable variable locations."
                            .to_string(),
                    ));
                }
            }
        }
    }

    let load_section = |id: gimli::SectionId| -> std::result::Result<
        EndianSlice<'a, gimli::RunTimeEndian>,
        gimli::Error,
    > {
        let data = obj
            .section_by_name(id.name())
            .and_then(|s| s.data().ok())
            .unwrap_or(&[]);
        Ok(EndianSlice::new(data, endian))
    };

    gimli::Dwarf::load(load_section).map_err(|e| Error::DwarfParse(format!("Load DWARF: {e}")))
}

/// Parse `.debug_frame` to find CFA = SP + N at the given PC.
///
/// Returns the `N` (the `offset` in `CfaRule::RegisterAndOffset`), which is the
/// frame size added by the function prologue. A `DW_OP_fbreg: F` location means
/// the variable is at `CFA + F = SP + (N + F)`.
///
/// # Fallback behavior
///
/// Returns `0` when the section is absent or the PC is not covered. This leaves
/// `DW_OP_fbreg` offsets treated as SP-relative — which is **correct for leaf
/// functions** (no stack frame) but **incorrect for functions with stack frames**.
/// When this fallback is used, a warning is logged and captured variable values
/// may be wrong for non-leaf functions.
fn get_cfa_sp_delta(obj: &object::File<'_>, endian: gimli::RunTimeEndian, pc: u64) -> i64 {
    use gimli::{BaseAddresses, CfaRule, DebugFrame, EndianSlice, UnwindSection};

    let data = match obj
        .section_by_name(".debug_frame")
        .and_then(|s| s.data().ok())
    {
        Some(d) => d,
        None => {
            detrix_logging::warn!("[DWARF CFI] .debug_frame section not found — fbreg offsets will be SP-relative (may be wrong)");
            return 0;
        }
    };

    let debug_frame: DebugFrame<EndianSlice<gimli::RunTimeEndian>> =
        DebugFrame::from(EndianSlice::new(data, endian));
    let bases = BaseAddresses::default();
    // UnwindContext<R::Offset=usize, S=StoreOnHeap> — let the compiler infer the defaults.
    let mut ctx = gimli::UnwindContext::new();

    match debug_frame.unwind_info_for_address(&bases, &mut ctx, pc, DebugFrame::cie_from_offset) {
        Ok(row) => match row.cfa() {
            CfaRule::RegisterAndOffset { offset, .. } => {
                detrix_logging::info!("[DWARF CFI] PC={:#x} CFA = SP + {}", pc, offset);
                *offset
            }
            other => {
                detrix_logging::warn!("[DWARF CFI] PC={:#x} unsupported CFA rule: {:?}", pc, other);
                0
            }
        },
        Err(e) => {
            detrix_logging::warn!("[DWARF CFI] PC={:#x} unwind_info error: {}", pc, e);
            0
        }
    }
}

/// Resolve file:line to a program counter using DWARF line tables.
///
/// Tries an exact match first. If the compiler omitted a DWARF entry for that
/// exact line (common for simple assignments in Go), falls back to the nearest
/// line whose number is >= the requested line in the same file.
///
/// # Path resolution (mirrors Delve's approach)
///
/// Go DWARF v4 stores source files with `dir_index=0` meaning "compilation directory"
/// (`DW_AT_comp_dir`). Gimli returns `None` for `directory(header)` in that case,
/// so we fall back to the compile unit's `comp_dir` attribute explicitly.
///
/// Priority:
/// 1. Absolute filename → used as-is (avoids double-path `/dir//dir/file.go`)
/// 2. Relative + explicit directory entry (DWARF v5 dir≥0, or v4 dir≥1) → join
/// 3. Relative + no dir entry (DWARF v4 `dir_index=0`) → join with `DW_AT_comp_dir`
/// 4. Fallback → filename as-is (suffix match via `ends_with` may still find it)
fn resolve_line_to_pc<R: Reader>(
    dwarf: &gimli::Dwarf<R>,
    file: &str,
    line: u32,
) -> Result<ProgramCounter> {
    if line == 0 {
        return Err(Error::DwarfParse("Line number must be > 0".to_string()));
    }
    let target_line = line as u64;

    // Collect all (line, pc) pairs for the matching file in one pass.
    // Then pick exact match or nearest >= target.
    let mut candidates: Vec<(u64, ProgramCounter)> = Vec::new();

    // Sample of DWARF file paths seen — included in error messages for diagnosis.
    let mut seen_paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let mut units = dwarf.units();
    while let Some(header) = units
        .next()
        .map_err(|e| Error::DwarfParse(format!("Unit iteration: {e}")))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|e| Error::DwarfParse(format!("Unit parse: {e}")))?;

        // DW_AT_comp_dir: needed for DWARF v4 files where dir_index=0 means comp_dir.
        // Gimli returns None for v4 dir index 0, so we read it explicitly from the unit.
        let comp_dir: Option<String> = unit
            .comp_dir
            .clone()
            .and_then(|cd| cd.to_string_lossy().ok().map(|s| s.into_owned()));

        if let Some(program) = unit.line_program.clone() {
            let mut rows = program.rows();
            while let Some((header, row)) = rows
                .next_row()
                .map_err(|e| Error::DwarfParse(format!("Line row: {e}")))?
            {
                let Some(row_line) = row.line().map(|n| n.get()) else {
                    continue;
                };
                if row.address() == 0 {
                    continue;
                }

                if let Some(file_entry) = row.file(header) {
                    let name_reader = dwarf
                        .attr_string(&unit, file_entry.path_name())
                        .map_err(|e| Error::DwarfParse(format!("File name: {e}")))?;
                    let name_s = name_reader
                        .to_string_lossy()
                        .map_err(|e| Error::DwarfParse(format!("Name UTF-8: {e}")))?;
                    let name_str: &str = &name_s;

                    let file_path: String = if name_str.starts_with('/') {
                        // Already absolute — use directly to avoid double-path like:
                        //   /dir/ + /dir/file.go  →  /dir//dir/file.go  (broken)
                        name_str.to_owned()
                    } else if let Some(dir) = file_entry.directory(header) {
                        // Explicit directory from line table header (DWARF v5 dir≥0, v4 dir≥1).
                        let dir_reader = dwarf
                            .attr_string(&unit, dir)
                            .map_err(|e| Error::DwarfParse(format!("Dir string: {e}")))?;
                        let dir_s = dir_reader
                            .to_string_lossy()
                            .map_err(|e| Error::DwarfParse(format!("Dir UTF-8: {e}")))?;
                        let dir_str: &str = &dir_s;
                        if dir_str.is_empty() {
                            name_str.to_owned()
                        } else {
                            format!("{dir_str}/{name_str}")
                        }
                    } else if let Some(ref cd) = comp_dir {
                        // DWARF v4 dir_index=0 → gimli returns None but means comp_dir.
                        if cd.is_empty() {
                            name_str.to_owned()
                        } else {
                            format!("{cd}/{name_str}")
                        }
                    } else {
                        name_str.to_owned()
                    };

                    // Collect a sample of paths for diagnostic error messages.
                    // Limit size to avoid unbounded allocation across large Go binaries.
                    if seen_paths.len() < 25 {
                        seen_paths.insert(file_path.clone());
                    }

                    if file_path.ends_with(file) || file.ends_with(&file_path) {
                        candidates.push((row_line, row.address()));
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        // Include a few sample DWARF paths to make mismatches immediately diagnosable.
        let sample: Vec<&str> = seen_paths.iter().take(5).map(String::as_str).collect();
        return Err(Error::DwarfParse(format!(
            "No PC found for {file}:{line} (file not found in DWARF; \
             sample DWARF paths: [{}])",
            sample.join(", ")
        )));
    }

    // Prefer exact match; fall back to smallest line >= target (next statement).
    if let Some(&(_, pc)) = candidates.iter().find(|&&(l, _)| l == target_line) {
        return Ok(pc);
    }

    candidates
        .iter()
        .filter(|&&(l, _)| l >= target_line)
        .min_by_key(|&&(l, _)| l)
        .map(|&(_, pc)| pc)
        .ok_or_else(|| {
            Error::DwarfParse(format!(
                "No PC found for {file}:{line} (no DWARF entry at or after that line)"
            ))
        })
}

/// Find the function name containing a given PC.
fn find_function_at_pc<R: Reader>(dwarf: &gimli::Dwarf<R>, pc: ProgramCounter) -> Result<String> {
    let mut units = dwarf.units();
    while let Some(header) = units
        .next()
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|e| Error::DwarfParse(format!("{e}")))?;
        let mut entries = unit.entries();

        while let Some(entry) = entries
            .next_dfs()
            .map_err(|e| Error::DwarfParse(format!("{e}")))?
        {
            if entry.tag() != gimli::DW_TAG_subprogram {
                continue;
            }

            if let Some(ranges) = entry_pc_range(entry) {
                if pc >= ranges.0 && pc < ranges.1 {
                    if let Some(name) = read_die_name(entry, dwarf)? {
                        return Ok(name);
                    }
                }
            }
        }
    }

    Err(Error::DwarfParse(format!(
        "No function found at PC {pc:#x}"
    )))
}

/// Resolve variable locations at a given PC.
fn resolve_variables_at_pc<R: Reader>(
    dwarf: &gimli::Dwarf<R>,
    pc: ProgramCounter,
    requested_vars: &[String],
    cfa_sp_delta: i64,
    max_nested_depth: usize,
) -> Result<Vec<ResolvedVariable>> {
    let mut resolved = Vec::new();
    let mut units = dwarf.units();

    while let Some(header) = units
        .next()
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|e| Error::DwarfParse(format!("{e}")))?;
        let mut entries = unit.entries();

        while let Some(entry) = entries
            .next_dfs()
            .map_err(|e| Error::DwarfParse(format!("{e}")))?
        {
            if entry.tag() != gimli::DW_TAG_variable
                && entry.tag() != gimli::DW_TAG_formal_parameter
            {
                continue;
            }

            let raw_name = match read_die_name(entry, dwarf)? {
                Some(n) => n,
                None => continue,
            };

            // Go compiler emits heap-escaped local variables with a "&" prefix in DW_AT_name
            // (e.g. `order` that escapes becomes `&order` in DWARF).
            // Match both "order" (the canonical name) and "&order" (the heap-escaped form)
            // when the user requests "order" — this mirrors Delve's behaviour in eval.go.
            let (name, is_heap_escaped) = if let Some(stripped) = raw_name.strip_prefix('&') {
                (stripped.to_string(), true)
            } else {
                (raw_name.clone(), false)
            };

            if !requested_vars.is_empty() && !requested_vars.contains(&name) {
                continue;
            }

            if let Some(locations) = resolve_location_attr(entry, &unit, dwarf, pc, cfa_sp_delta)? {
                // Resolve size and type name via DW_AT_type chain.
                // Falls back to QWord / "unknown" on resolution failure.
                let type_offset_debug = entry.attr_value(DwAt(gimli::constants::DW_AT_type.0));
                detrix_logging::debug!(
                    "[DWARF resolve] variable '{}' (raw='{}' heap_escaped={}) type_attr={:?}",
                    name,
                    raw_name,
                    is_heap_escaped,
                    type_offset_debug
                );
                let type_info = resolve_type_info(entry, &unit, dwarf)?;
                let size = if type_info.size == VariableSize::QWord {
                    // Prefer direct DW_AT_byte_size if present (more reliable)
                    resolve_type_size(entry)?.unwrap_or(type_info.size)
                } else {
                    type_info.size
                };

                // Upgrade scalar/piece locations to compound types (GoString, GoSlice).
                // Multi-piece = Go register ABI; single-piece = stack-allocated.
                let location = {
                    let upgraded = upgrade_location_for_type(locations, &type_info);
                    // For heap-escaped variables (& prefix in DWARF name): the location
                    // expression gives the stack slot of a pointer to the heap value.
                    // upgrade_location_for_type sees a struct type + StackOffset and returns
                    // StackBlob — but the stack slot holds only the pointer, not the struct
                    // bytes. Convert to StackIndirect so BPF reads 8 bytes (the pointer),
                    // and user-space dereferences it to get the actual struct from the heap.
                    if is_heap_escaped {
                        match upgraded {
                            VariableLocation::StackBlob { offset, byte_size } => {
                                detrix_logging::info!(
                                    "[DWARF] '{}' heap-escaped (& prefix): \
                                     StackBlob→StackIndirect offset={} byte_size={}",
                                    name,
                                    offset,
                                    byte_size
                                );
                                VariableLocation::StackIndirect { offset, byte_size }
                            }
                            other => other,
                        }
                    } else {
                        upgraded
                    }
                };

                detrix_logging::debug!(
                    "DWARF resolved '{}': type={} is_string={} is_slice={} location={}",
                    name,
                    type_info.name,
                    type_info.is_string,
                    type_info.is_slice,
                    location,
                );

                // Resolve nested type structure for compound types only.
                // Go DWARF sometimes emits structs as typedefs or with package-qualified names
                // (e.g. "main.Order") without setting is_struct=true directly.
                // Primitive types (int, float64, bool, etc.) are intentionally excluded —
                // they have no nested fields and NestedType::Scalar in `_ =>` caused
                // parse_struct_fields_from_addr to be called with the scalar VALUE as an address.
                let should_resolve_nested = type_info.is_struct
                    || type_info.name.contains('.') // Package-qualified names like main.Order
                    || type_info.name.starts_with("map["); // Map types need nested type info for iteration

                let nested_type = if should_resolve_nested {
                    use crate::dwarf::nested_types::{resolve_nested_type, NestedTypeConfig};
                    let config = NestedTypeConfig {
                        max_depth: max_nested_depth,
                        max_struct_fields: -1,
                        max_elements: 64,
                    };
                    detrix_logging::debug!(
                        "[DWARF] Attempting nested type resolution for '{}' (type_name='{}', is_struct={})",
                        name, type_info.name, type_info.is_struct
                    );
                    let result = resolve_nested_type(entry, &unit, dwarf, &config);
                    match &result {
                        Ok(nested) => {
                            detrix_logging::debug!(
                                "[DWARF] Resolved nested type for '{}': {:?}",
                                name,
                                match nested {
                                    crate::dwarf::nested_types::NestedType::Struct {
                                        fields,
                                        ..
                                    } => format!("Struct with {} fields", fields.len()),
                                    _ => "Other".to_string(),
                                }
                            );
                        }
                        Err(e) => {
                            detrix_logging::debug!(
                                "[DWARF] Failed to resolve nested type for '{}': {}",
                                name,
                                e
                            );
                        }
                    }
                    result.ok()
                } else {
                    detrix_logging::debug!(
                        "[DWARF] Skipping nested type resolution for '{}' (type_name='{}')",
                        name,
                        type_info.name
                    );
                    None
                };

                resolved.push(ResolvedVariable {
                    name,
                    location,
                    size,
                    type_name: type_info.name,
                    nested_type,
                });
            }
        }
    }

    // Go DWARF may emit multiple DW_TAG_variable DIEs with the same name for
    // complex heap-string assignments (compiler-generated intermediates).
    // Keep only the FIRST occurrence per name: for `runtime.concatstring*`
    // (used by `+` concatenation) the first DIE is the sret output slot written
    // directly by the runtime call — it holds the correct heap ptr by the next
    // statement. Later DIEs are compiler temporaries at stale/wrong stack slots.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, var) in resolved.iter().enumerate() {
        seen.entry(var.name.clone()).or_insert(i);
    }
    let mut deduped: Vec<ResolvedVariable> = seen.values().map(|&i| resolved[i].clone()).collect();
    // Preserve the original relative order so BPF slot indices are stable.
    deduped.sort_by_key(|v| resolved.iter().position(|r| r.name == v.name).unwrap_or(0));

    Ok(deduped)
}

/// Read DW_AT_name from a DIE, handling both inline strings and .debug_str refs.
fn read_die_name<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<Option<String>> {
    let attr = match entry.attr_value(DwAt(gimli::constants::DW_AT_name.0)) {
        Some(a) => a,
        None => return Ok(None),
    };

    match attr {
        AttributeValue::DebugStrRef(offset) => {
            let s = dwarf
                .string(offset)
                .map_err(|e| Error::DwarfParse(format!("{e}")))?;
            let name = s
                .to_string_lossy()
                .map_err(|e| Error::DwarfParse(format!("UTF-8: {e}")))?;
            Ok(Some(name.to_string()))
        }
        AttributeValue::String(ref s) => {
            let name = s
                .to_string_lossy()
                .map_err(|e| Error::DwarfParse(format!("UTF-8: {e}")))?;
            Ok(Some(name.to_string()))
        }
        _ => Ok(None),
    }
}

/// Extract the PC range (low_pc, high_pc) from a DWARF entry.
fn entry_pc_range<R: Reader>(entry: &DebuggingInformationEntry<R>) -> Option<(u64, u64)> {
    let low = match entry.attr_value(DwAt(gimli::constants::DW_AT_low_pc.0)) {
        Some(AttributeValue::Addr(addr)) => addr,
        _ => return None,
    };
    let high = match entry.attr_value(DwAt(gimli::constants::DW_AT_high_pc.0)) {
        Some(AttributeValue::Addr(addr)) => addr,
        Some(AttributeValue::Udata(len)) => low + len,
        _ => return None,
    };
    Some((low, high))
}

/// Resolve a DWARF location attribute to a list of piece locations.
///
/// Returns `Some(vec)` with one entry for simple locations, or multiple entries
/// for composite `DW_OP_piece` expressions (Go 1.17+ register ABI).
fn resolve_location_attr<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    pc: ProgramCounter,
    cfa_sp_delta: i64,
) -> Result<Option<Vec<VariableLocation>>> {
    let loc_attr = match entry.attr_value(DwAt(gimli::constants::DW_AT_location.0)) {
        Some(attr) => attr,
        None => return Ok(None),
    };

    match loc_attr {
        AttributeValue::Exprloc(expr) => {
            evaluate_location_expr(expr, unit.encoding(), cfa_sp_delta)
        }
        AttributeValue::LocationListsRef(offset) => {
            let mut loclists = dwarf
                .locations(unit, offset)
                .map_err(|e| Error::DwarfParse(format!("Location list: {e}")))?;

            while let Some(entry) = loclists
                .next()
                .map_err(|e| Error::DwarfParse(format!("Location entry: {e}")))?
            {
                let gimli::LocationListEntry { range, data, .. } = entry;
                if pc >= range.begin && pc < range.end {
                    return evaluate_location_expr(data, unit.encoding(), cfa_sp_delta);
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Evaluate a DWARF location expression, collecting all piece locations.
///
/// Supports subset needed for Go with -N -l:
/// - `DW_OP_regX`: variable in register X (standalone or start of composite)
/// - `DW_OP_fbreg N`: variable at frame_base + N
/// - `DW_OP_bregX N` (`RegisterOffset`): value at base_register_X + N — Go's primary form
///   for stack-local variables (x86-64: `breg7` = RSP+N; ARM64: `breg31` = SP+N)
/// - `DW_OP_piece N`: composite piece separator (Go 1.17+ register ABI)
///
/// Returns a `Vec` with one entry for simple locations, or one entry per piece for
/// composite expressions such as `DW_OP_reg0 DW_OP_piece 8 DW_OP_reg3 DW_OP_piece 8`
/// (Go string passed in two registers: ptr in RAX, len in RBX) or
/// `DW_OP_breg31 -32 DW_OP_piece 8 DW_OP_breg31 -24 DW_OP_piece 8`
/// (Go string spilled to stack: ptr at SP-32, len at SP-24 on ARM64).
fn evaluate_location_expr<R: Reader>(
    expr: gimli::Expression<R>,
    encoding: gimli::Encoding,
    cfa_sp_delta: i64,
) -> Result<Option<Vec<VariableLocation>>> {
    let mut ops = expr.operations(encoding);
    let mut pieces: Vec<VariableLocation> = Vec::new();
    let mut pending: Option<VariableLocation> = None;

    detrix_logging::debug!("[DWARF eval] Starting location expression evaluation");

    loop {
        match ops
            .next()
            .map_err(|e| Error::DwarfParse(format!("Op parse: {e}")))?
        {
            None => break,
            Some(gimli::Operation::Register { register }) => {
                detrix_logging::debug!("[DWARF eval] Register {:?}", register);
                if let Some(prev) = pending.take() {
                    pieces.push(prev);
                }
                pending = VariableLocation::from_register(register.0);
            }
            Some(gimli::Operation::FrameOffset { offset }) => {
                // DW_OP_fbreg N: variable at CFA + N.
                // CFA = SP + cfa_sp_delta, so the SP-relative offset is cfa_sp_delta + N.
                // Example: fbreg -376 with cfa_sp_delta=408 → SP + (408 - 376) = SP + 32.
                let sp_offset = cfa_sp_delta + offset;
                detrix_logging::info!(
                    "[DWARF eval] FrameOffset fbreg={} + cfa_sp_delta={} = sp_offset={}",
                    offset,
                    cfa_sp_delta,
                    sp_offset
                );
                if let Some(prev) = pending.take() {
                    pieces.push(prev);
                }
                pending = Some(VariableLocation::stack(sp_offset));
            }
            Some(gimli::Operation::RegisterOffset { offset, .. }) => {
                // DW_OP_bregX N: value at address (register_X + offset).
                // Go emits this for stack-spilled locals:
                //   x86-64: DW_OP_breg7 N  (RSP = DWARF reg 7)
                //   ARM64:  DW_OP_breg31 N (SP  = DWARF reg 31)
                // We model all base-register+offset accesses as StackOffset so
                // the BPF program can use ctx->sp. For Go local variables this is
                // always correct because Go only uses SP/RSP as the base register.
                detrix_logging::info!("[DWARF eval] RegisterOffset breg7={}", offset);
                if let Some(prev) = pending.take() {
                    pieces.push(prev);
                }
                pending = Some(VariableLocation::stack(offset));
            }
            Some(gimli::Operation::Piece { .. }) => {
                detrix_logging::debug!("[DWARF eval] Piece");
                // DW_OP_piece follows a location op for composite types.
                // Flush pending location into the pieces list.
                if let Some(loc) = pending.take() {
                    pieces.push(loc);
                }
            }
            Some(gimli::Operation::Deref { .. }) => {
                // DW_OP_deref: treat the pending address as a pointer and dereference.
                // Go heap-escaped variables: the stack slot holds a pointer to the heap struct.
                // We convert StackOffset → StackIndirect so the ring buffer parser knows to
                // read 8 bytes (the pointer) from BPF and dereference them in user-space.
                detrix_logging::debug!("[DWARF eval] Deref: converting pending to StackIndirect");
                if let Some(VariableLocation::StackOffset { offset }) = pending.take() {
                    pending = Some(VariableLocation::StackIndirect {
                        offset,
                        byte_size: 0,
                    });
                } else {
                    detrix_logging::debug!("[DWARF eval] Deref on non-StackOffset — discarding");
                    pending = None;
                }
            }
            other => {
                // Unrecognised operation — discard pending; we can't interpret it.
                detrix_logging::debug!("[DWARF eval] Unhandled operation: {:?}", other);
                pending = None;
            }
        }
    }

    // Non-composite case: single location without DW_OP_piece.
    if let Some(loc) = pending.take() {
        pieces.push(loc);
    }

    detrix_logging::debug!("[DWARF eval] Result: {:?} pieces", pieces.len());

    if pieces.is_empty() {
        Ok(None)
    } else {
        Ok(Some(pieces))
    }
}

/// Upgrade raw DWARF piece locations to a compound Go type location.
///
/// **Single-piece (stack-allocated):** upgrades scalar stack locations:
///
/// | Go type  | In-memory layout        | Upgraded to  |
/// |----------|-------------------------|--------------|
/// | `string` | `{ptr uintptr, len int}` (16 B) | `GoString { ptr: off, len: off+8 }` |
/// | `[]T`    | `{ptr, len, cap}` (24 B)        | `GoSlice  { ptr: off, len: off+8, cap: off+16 }` |
///
/// **Multi-piece (Go 1.17+ register ABI):** composite `DW_OP_piece` expressions
/// encode each component in a separate register. Matched by piece count:
///
/// | Pieces | Type match      | Upgraded to  |
/// |--------|-----------------|--------------|
/// | 2      | `is_string`     | `GoString { ptr: pieces[0], len: pieces[1] }` |
/// | 3      | `is_slice`      | `GoSlice { ptr: pieces[0], len: pieces[1], cap: pieces[2] }` |
fn upgrade_location_for_type(
    locations: Vec<VariableLocation>,
    type_info: &TypeInfo,
) -> VariableLocation {
    // Debug logging for upgrade decision
    detrix_logging::debug!(
        "[DWARF upgrade] type={} is_string={} is_slice={} locations.len={} locations={:?}",
        type_info.name,
        type_info.is_string,
        type_info.is_slice,
        locations.len(),
        locations
    );

    // ── Multi-piece: Go register ABI composite types ──────────────────────────
    if locations.len() == 2 && type_info.is_string {
        // Guarded by len() == 2 above — pattern match eliminates unwrap.
        let [ptr, len]: [VariableLocation; 2] = locations
            .try_into()
            .unwrap_or_else(|_| unreachable!("len() == 2 guarantees exactly 2 elements"));
        return VariableLocation::GoString {
            ptr: Box::new(ptr),
            len: Box::new(len),
        };
    }
    if locations.len() == 3 && type_info.is_slice {
        // Guarded by len() == 3 above — pattern match eliminates unwrap.
        let [ptr, len, cap]: [VariableLocation; 3] = locations
            .try_into()
            .unwrap_or_else(|_| unreachable!("len() == 3 guarantees exactly 3 elements"));
        return VariableLocation::GoSlice {
            ptr: Box::new(ptr),
            len: Box::new(len),
            cap: Box::new(cap),
        };
    }

    // ── Single-piece: stack-based or register scalar ──────────────────────────
    // Safe: callers only reach this when locations is non-empty (resolve_location_attr
    // returns None for empty, which is filtered before calling upgrade_location_for_type).
    let location = locations
        .into_iter()
        .next()
        .unwrap_or(VariableLocation::Register(super::types::Register::Rax));

    detrix_logging::debug!(
        "[DWARF upgrade] single-piece location={:?} is_string={}",
        location,
        type_info.is_string
    );

    if type_info.is_string {
        if let VariableLocation::StackOffset { offset } = location {
            return VariableLocation::GoString {
                ptr: Box::new(VariableLocation::StackOffset { offset }),
                len: Box::new(VariableLocation::StackOffset { offset: offset + 8 }),
            };
        }
        // Register-based string with only one piece — can't reconstruct GoString.
        // Fall through as scalar (captures raw ptr word).
    } else if type_info.is_slice {
        if let VariableLocation::StackOffset { offset } = location {
            return VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::StackOffset { offset }),
                len: Box::new(VariableLocation::StackOffset { offset: offset + 8 }),
                cap: Box::new(VariableLocation::StackOffset {
                    offset: offset + 16,
                }),
            };
        }
    } else if type_info.is_map || type_info.name.starts_with("map[") {
        // Go map: single pointer to hmap (classic) or Map (Swiss Table).
        // Wrap in GoMap so the ringbuf parser knows to iterate the map.
        // Detection: go_kind=21 (DWARF) or name prefix "map[" (fallback for stripped DWARF).
        return VariableLocation::GoMap {
            ptr: Box::new(location),
        };
    } else if (type_info.is_array || type_info.is_struct) && type_info.byte_size > 0 {
        match location {
            VariableLocation::StackOffset { offset } => {
                return VariableLocation::StackBlob {
                    offset,
                    byte_size: type_info.byte_size as usize,
                };
            }
            VariableLocation::StackIndirect { offset, .. } => {
                // Heap-escaped struct: BPF reads 8-byte pointer from stack,
                // user-space dereferences the pointer to get actual struct bytes.
                return VariableLocation::StackIndirect {
                    offset,
                    byte_size: type_info.byte_size as usize,
                };
            }
            _ => {} // Register-based arrays/structs are unusual; fall through as scalar.
        }
    }
    location
}

/// Resolve byte size from a variable's DW_AT_byte_size.
fn resolve_type_size<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
) -> Result<Option<VariableSize>> {
    if let Some(AttributeValue::Udata(size)) =
        entry.attr_value(DwAt(gimli::constants::DW_AT_byte_size.0))
    {
        return Ok(VariableSize::from_byte_size(size));
    }
    // TODO: Follow DW_AT_type reference to get byte size from type DIE.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::typeinfo::TypeInfo;
    use crate::dwarf::types::{Register, VariableSize};

    fn string_type() -> TypeInfo {
        TypeInfo {
            name: "string".to_string(),
            size: VariableSize::QWord,
            byte_size: 16,
            is_pointer: false,
            is_string: true,
            is_slice: false,
            is_array: false,
            is_struct: false,
            ..TypeInfo::unknown()
        }
    }

    fn slice_type() -> TypeInfo {
        TypeInfo {
            name: "[]int64".to_string(),
            size: VariableSize::QWord,
            byte_size: 24,
            is_pointer: false,
            is_string: false,
            is_slice: true,
            is_array: false,
            is_struct: false,
            ..TypeInfo::unknown()
        }
    }

    fn array_type(byte_size: u64) -> TypeInfo {
        TypeInfo {
            name: "[N]int64".to_string(),
            size: VariableSize::QWord,
            byte_size,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: true,
            is_struct: false,
            ..TypeInfo::unknown()
        }
    }

    fn struct_type(byte_size: u64) -> TypeInfo {
        TypeInfo {
            name: "TradeRequest".to_string(),
            size: VariableSize::QWord,
            byte_size,
            is_pointer: false,
            is_string: false,
            is_slice: false,
            is_array: false,
            is_struct: true,
            ..TypeInfo::unknown()
        }
    }

    fn scalar_type() -> TypeInfo {
        TypeInfo::unknown()
    }

    // ── upgrade_location_for_type ─────────────────────────────────────────────

    #[test]
    fn upgrade_stack_offset_to_go_string() {
        let loc = vec![VariableLocation::stack(-240)];
        let result = upgrade_location_for_type(loc, &string_type());
        assert_eq!(
            result,
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(-240)),
                len: Box::new(VariableLocation::stack(-232)),
            }
        );
    }

    #[test]
    fn upgrade_stack_offset_to_go_slice() {
        let loc = vec![VariableLocation::stack(-64)];
        let result = upgrade_location_for_type(loc, &slice_type());
        assert_eq!(
            result,
            VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::stack(-64)),
                len: Box::new(VariableLocation::stack(-56)),
                cap: Box::new(VariableLocation::stack(-48)),
            }
        );
    }

    #[test]
    fn upgrade_positive_stack_offset_to_go_string() {
        let loc = vec![VariableLocation::stack(16)];
        let result = upgrade_location_for_type(loc, &string_type());
        assert_eq!(
            result,
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(16)),
                len: Box::new(VariableLocation::stack(24)),
            }
        );
    }

    #[test]
    fn scalar_type_location_unchanged() {
        let loc = VariableLocation::stack(-16);
        let result = upgrade_location_for_type(vec![loc.clone()], &scalar_type());
        assert_eq!(result, loc);
    }

    #[test]
    fn register_string_not_upgraded_falls_through() {
        // Single-register string (1 piece) — can't reconstruct GoString without len.
        let loc = VariableLocation::Register(Register::Rax);
        let result = upgrade_location_for_type(vec![loc.clone()], &string_type());
        assert_eq!(
            result, loc,
            "single-register strings should pass through unchanged"
        );
    }

    /// Go 1.17+ register ABI: string passed as two register pieces
    /// (DW_OP_reg0 DW_OP_piece 8 DW_OP_reg3 DW_OP_piece 8 → ptr=RAX, len=RBX).
    #[test]
    fn upgrade_two_register_pieces_to_go_string() {
        let pieces = vec![
            VariableLocation::Register(Register::Rax),
            VariableLocation::Register(Register::Rbx),
        ];
        let result = upgrade_location_for_type(pieces, &string_type());
        assert_eq!(
            result,
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
            }
        );
    }

    /// Go 1.17+ register ABI: slice passed as three register pieces
    /// (ptr=RAX, len=RBX, cap=RCX).
    #[test]
    fn upgrade_three_register_pieces_to_go_slice() {
        let pieces = vec![
            VariableLocation::Register(Register::Rax),
            VariableLocation::Register(Register::Rbx),
            VariableLocation::Register(Register::Rcx),
        ];
        let result = upgrade_location_for_type(pieces, &slice_type());
        assert_eq!(
            result,
            VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::Register(Register::Rax)),
                len: Box::new(VariableLocation::Register(Register::Rbx)),
                cap: Box::new(VariableLocation::Register(Register::Rcx)),
            }
        );
    }

    #[test]
    fn upgrade_stack_offset_to_stack_blob_array() {
        let loc = vec![VariableLocation::stack(-48)];
        let result = upgrade_location_for_type(loc, &array_type(32));
        assert_eq!(
            result,
            VariableLocation::StackBlob {
                offset: -48,
                byte_size: 32
            }
        );
    }

    #[test]
    fn upgrade_stack_offset_to_stack_blob_struct() {
        let loc = vec![VariableLocation::stack(-80)];
        let result = upgrade_location_for_type(loc, &struct_type(40));
        assert_eq!(
            result,
            VariableLocation::StackBlob {
                offset: -80,
                byte_size: 40
            }
        );
    }

    #[test]
    fn array_type_with_zero_byte_size_falls_through() {
        // byte_size=0 means we couldn't read DW_AT_byte_size — don't upgrade.
        let loc = VariableLocation::stack(-8);
        let result = upgrade_location_for_type(vec![loc.clone()], &array_type(0));
        assert_eq!(result, loc, "zero byte_size should not produce a StackBlob");
    }

    #[test]
    fn register_array_falls_through() {
        // Register-based arrays are unusual; fall through as scalar.
        let loc = VariableLocation::Register(Register::Rax);
        let result = upgrade_location_for_type(vec![loc.clone()], &array_type(16));
        assert_eq!(result, loc);
    }

    // ── RegisterOffset (DW_OP_bregX) handling ────────────────────────────────

    /// Go uses DW_OP_breg31 N (ARM64) / DW_OP_breg7 N (x86-64) for stack locals.
    /// RegisterOffset with an SP-like base register must resolve to StackOffset.
    #[test]
    fn register_offset_single_piece_upgrades_to_go_string() {
        // Simulates: DW_OP_breg31 -32  (string struct at SP-32 on ARM64)
        // After RegisterOffset fix → StackOffset{-32} → GoString{ptr:-32, len:-24}
        let loc = vec![VariableLocation::stack(-32)]; // what RegisterOffset{offset:-32} produces
        let result = upgrade_location_for_type(loc, &string_type());
        assert_eq!(
            result,
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(-32)),
                len: Box::new(VariableLocation::stack(-24)),
            }
        );
    }

    /// Two-piece RegisterOffset: Go string spilled to stack as two adjacent words
    /// (DW_OP_breg31 -32 DW_OP_piece 8 DW_OP_breg31 -24 DW_OP_piece 8).
    #[test]
    fn two_register_offset_pieces_upgrade_to_go_string() {
        let pieces = vec![
            VariableLocation::stack(-32), // ptr piece: what RegisterOffset{-32} yields
            VariableLocation::stack(-24), // len piece: what RegisterOffset{-24} yields
        ];
        let result = upgrade_location_for_type(pieces, &string_type());
        assert_eq!(
            result,
            VariableLocation::GoString {
                ptr: Box::new(VariableLocation::stack(-32)),
                len: Box::new(VariableLocation::stack(-24)),
            }
        );
    }

    /// Three-piece RegisterOffset: Go slice spilled to stack as three words.
    #[test]
    fn three_register_offset_pieces_upgrade_to_go_slice() {
        let pieces = vec![
            VariableLocation::stack(-48), // ptr
            VariableLocation::stack(-40), // len
            VariableLocation::stack(-32), // cap
        ];
        let result = upgrade_location_for_type(pieces, &slice_type());
        assert_eq!(
            result,
            VariableLocation::GoSlice {
                ptr: Box::new(VariableLocation::stack(-48)),
                len: Box::new(VariableLocation::stack(-40)),
                cap: Box::new(VariableLocation::stack(-32)),
            }
        );
    }

    #[test]
    fn parse_nonexistent_binary_returns_error() {
        let result = DwarfInfo::parse("/nonexistent/binary");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::DwarfParse(_)));
    }

    #[test]
    fn probe_point_symbol_offset_calculation() {
        let point = ProbePoint {
            binary_path: PathBuf::from("/test/binary"),
            pc: 0x401100,
            symbol_offset: 0x401100 - 0x401000,
            function_name: "main.handleOrder".to_string(),
            variables: vec![],
        };
        assert_eq!(point.symbol_offset, 0x100);
    }
}
