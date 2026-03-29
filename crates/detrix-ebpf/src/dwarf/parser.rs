//! DWARF parser for Go ELF binaries
//!
//! Resolves source file:line to program counter (PC) and extracts
//! variable locations from DWARF debug info. This is the foundation
//! for generating eBPF uprobe programs that capture variable values.
//!
//! Uses `gimli` for DWARF parsing and `object` for ELF reading.

use super::typeinfo::resolve_type_info;
use super::types::{
    ProbePoint, ProgramCounter, ResolvedVariable, VariableLocation, VariableSize,
};
use crate::error::{Error, Result};

use gimli::{AttributeValue, DebuggingInformationEntry, DwAt, EndianSlice, Reader};
use object::{Object, ObjectSection};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Parsed Go binary with DWARF debug info ready for probe point resolution.
#[derive(Debug)]
pub struct DwarfInfo {
    /// Path to the ELF binary.
    binary_path: PathBuf,
    /// Raw binary data (kept alive for gimli references).
    _data: Vec<u8>,
    /// Base address for symbol offset calculation.
    text_base: u64,
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

        let text_base = obj
            .section_by_name(".text")
            .map(|s| s.address())
            .unwrap_or(0);

        Ok(Self {
            binary_path: path,
            _data: data,
            text_base,
        })
    }

    /// Resolve a source location to a probe point with variable locations.
    ///
    /// This is the main entry point for the DWARF module. Given a file and line,
    /// it returns the PC to attach the uprobe and the variables available at that PC.
    pub fn resolve_probe_point(
        &self,
        file: &str,
        line: u32,
        requested_vars: &[String],
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

        // Step 3: Resolve variable locations at this PC
        let variables = resolve_variables_at_pc(&dwarf, pc, requested_vars)?;

        let symbol_offset = pc.saturating_sub(self.text_base);

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
fn load_dwarf<'a>(
    obj: &'a object::File<'a>,
    endian: gimli::RunTimeEndian,
) -> Result<gimli::Dwarf<EndianSlice<'a, gimli::RunTimeEndian>>> {
    let load_section =
        |id: gimli::SectionId| -> std::result::Result<EndianSlice<'a, gimli::RunTimeEndian>, gimli::Error> {
            let data = obj
                .section_by_name(id.name())
                .and_then(|s| s.uncompressed_data().ok())
                .unwrap_or(Cow::Borrowed(&[]));
            // For uncompressed ELF sections, this is always Borrowed.
            Ok(EndianSlice::new(
                match &data {
                    Cow::Borrowed(b) => b,
                    Cow::Owned(_) => &[],
                },
                endian,
            ))
        };

    gimli::Dwarf::load(load_section).map_err(|e| Error::DwarfParse(format!("Load DWARF: {e}")))
}

/// Resolve file:line to a program counter using DWARF line tables.
fn resolve_line_to_pc<R: Reader>(
    dwarf: &gimli::Dwarf<R>,
    file: &str,
    line: u32,
) -> Result<ProgramCounter> {
    let target_line = std::num::NonZeroU64::new(line as u64)
        .ok_or_else(|| Error::DwarfParse("Line number must be > 0".to_string()))?;

    let mut units = dwarf.units();
    while let Some(header) = units
        .next()
        .map_err(|e| Error::DwarfParse(format!("Unit iteration: {e}")))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|e| Error::DwarfParse(format!("Unit parse: {e}")))?;

        if let Some(program) = unit.line_program.clone() {
            let mut rows = program.rows();
            while let Some((header, row)) = rows
                .next_row()
                .map_err(|e| Error::DwarfParse(format!("Line row: {e}")))?
            {
                if row.line() != Some(target_line) {
                    continue;
                }

                if let Some(file_entry) = row.file(header) {
                    let mut file_path = String::new();

                    if let Some(dir) = file_entry.directory(header) {
                        let dir_reader = dwarf
                            .attr_string(&unit, dir)
                            .map_err(|e| Error::DwarfParse(format!("Dir string: {e}")))?;
                        let dir_s = dir_reader.to_string_lossy()
                            .map_err(|e| Error::DwarfParse(format!("Dir UTF-8: {e}")))?;
                        if !dir_s.is_empty() {
                            file_path.push_str(&dir_s);
                            file_path.push('/');
                        }
                    }

                    let name_reader = dwarf
                        .attr_string(&unit, file_entry.path_name())
                        .map_err(|e| Error::DwarfParse(format!("File name: {e}")))?;
                    let name_s = name_reader.to_string_lossy()
                        .map_err(|e| Error::DwarfParse(format!("Name UTF-8: {e}")))?;
                    file_path.push_str(&name_s);

                    if file_path.ends_with(file) || file.ends_with(&*file_path) {
                        return Ok(row.address());
                    }
                }
            }
        }
    }

    Err(Error::DwarfParse(format!("No PC found for {file}:{line}")))
}

/// Find the function name containing a given PC.
fn find_function_at_pc<R: Reader>(
    dwarf: &gimli::Dwarf<R>,
    pc: ProgramCounter,
) -> Result<String> {
    let mut units = dwarf.units();
    while let Some(header) = units
        .next()
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        let unit = dwarf
            .unit(header)
            .map_err(|e| Error::DwarfParse(format!("{e}")))?;
        let mut entries = unit.entries();

        while let Some((_, entry)) = entries
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

    Err(Error::DwarfParse(format!("No function found at PC {pc:#x}")))
}

/// Resolve variable locations at a given PC.
fn resolve_variables_at_pc<R: Reader>(
    dwarf: &gimli::Dwarf<R>,
    pc: ProgramCounter,
    requested_vars: &[String],
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

        while let Some((_, entry)) = entries
            .next_dfs()
            .map_err(|e| Error::DwarfParse(format!("{e}")))?
        {
            if entry.tag() != gimli::DW_TAG_variable
                && entry.tag() != gimli::DW_TAG_formal_parameter
            {
                continue;
            }

            let name = match read_die_name(entry, dwarf)? {
                Some(n) => n,
                None => continue,
            };

            if !requested_vars.is_empty() && !requested_vars.contains(&name) {
                continue;
            }

            if let Some(location) = resolve_location_attr(entry, &unit, dwarf, pc)? {
                // Resolve size and type name via DW_AT_type chain.
                // Falls back to QWord / "unknown" on resolution failure.
                let type_info = resolve_type_info(entry, &unit, dwarf)?;
                let size = if type_info.size == VariableSize::QWord {
                    // Prefer direct DW_AT_byte_size if present (more reliable)
                    resolve_type_size(entry)?.unwrap_or(type_info.size)
                } else {
                    type_info.size
                };

                resolved.push(ResolvedVariable {
                    name,
                    location,
                    size,
                    type_name: type_info.name,
                });
            }
        }
    }

    Ok(resolved)
}

/// Read DW_AT_name from a DIE, handling both inline strings and .debug_str refs.
fn read_die_name<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    dwarf: &gimli::Dwarf<R>,
) -> Result<Option<String>> {
    let attr = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_name.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
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
    let low = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_low_pc.0))
        .ok()?
    {
        Some(AttributeValue::Addr(addr)) => addr,
        _ => return None,
    };
    let high = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_high_pc.0))
        .ok()?
    {
        Some(AttributeValue::Addr(addr)) => addr,
        Some(AttributeValue::Udata(len)) => low + len,
        _ => return None,
    };
    Some((low, high))
}

/// Resolve a DWARF location attribute to a VariableLocation.
fn resolve_location_attr<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
    unit: &gimli::Unit<R>,
    dwarf: &gimli::Dwarf<R>,
    pc: ProgramCounter,
) -> Result<Option<VariableLocation>> {
    let loc_attr = match entry
        .attr_value(DwAt(gimli::constants::DW_AT_location.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        Some(attr) => attr,
        None => return Ok(None),
    };

    match loc_attr {
        AttributeValue::Exprloc(expr) => evaluate_location_expr(expr, unit.encoding()),
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
                    return evaluate_location_expr(data, unit.encoding());
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Evaluate a simple DWARF location expression.
///
/// Supports subset needed for Go with -N -l:
/// - DW_OP_regX: variable in register X
/// - DW_OP_fbreg N: variable at frame_base + N
fn evaluate_location_expr<R: Reader>(
    expr: gimli::Expression<R>,
    encoding: gimli::Encoding,
) -> Result<Option<VariableLocation>> {
    let mut ops = expr.operations(encoding);

    match ops
        .next()
        .map_err(|e| Error::DwarfParse(format!("Op parse: {e}")))?
    {
        Some(gimli::Operation::Register { register }) => {
            Ok(VariableLocation::from_register(register.0))
        }
        Some(gimli::Operation::FrameOffset { offset }) => {
            Ok(Some(VariableLocation::stack(offset)))
        }
        _ => Ok(None),
    }
}

/// Resolve byte size from a variable's DW_AT_byte_size.
fn resolve_type_size<R: Reader>(
    entry: &DebuggingInformationEntry<R>,
) -> Result<Option<VariableSize>> {
    if let Some(AttributeValue::Udata(size)) = entry
        .attr_value(DwAt(gimli::constants::DW_AT_byte_size.0))
        .map_err(|e| Error::DwarfParse(format!("{e}")))?
    {
        return Ok(VariableSize::from_byte_size(size));
    }
    // TODO: Follow DW_AT_type reference to get byte size from type DIE.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

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
