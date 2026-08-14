//! Debug-image discovery independent of any source language.
//!
//! This is intentionally small: embedded DWARF is the first provider.  The
//! same metadata contract can later be backed by GNU debuglink/build-id or
//! split-DWARF providers without changing profiles or probe runtimes.

use crate::dwarf::DwarfInfo;
use object::{BinaryFormat, Object};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugImageSource {
    Embedded,
    External,
    Split,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetAbi {
    X86_64,
    Aarch64,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugImageMetadata {
    pub path: PathBuf,
    pub format: String,
    pub abi: TargetAbi,
    pub source: DebugImageSource,
    pub has_symbols: bool,
    pub has_variable_dwarf: bool,
    /// The ELF file that supplies debug sections. This differs from `path`
    /// when an external `.debug` or build-id file is selected.
    pub debug_path: PathBuf,
    /// ELF GNU build-id, when present, used to attribute external debug files.
    pub build_id: Option<String>,
}

pub trait DebugImageProvider: Send + Sync {
    fn load(&self, path: &Path) -> Result<DebugImageMetadata, DebugImageError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddedDebugImageProvider;

impl DebugImageProvider for EmbeddedDebugImageProvider {
    fn load(&self, path: &Path) -> Result<DebugImageMetadata, DebugImageError> {
        let bytes = std::fs::read(path).map_err(|source| DebugImageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file = object::File::parse(bytes.as_slice())
            .map_err(|source| DebugImageError::Object(source.to_string()))?;
        if file.format() != BinaryFormat::Elf {
            return Err(DebugImageError::UnsupportedFormat(format!(
                "{:?}; eBPF requires ELF",
                file.format()
            )));
        }
        let abi = match file.architecture() {
            object::Architecture::X86_64 => TargetAbi::X86_64,
            object::Architecture::Aarch64 => TargetAbi::Aarch64,
            _ => TargetAbi::Unknown,
        };
        let has_sections = file.section_by_name(".debug_info").is_some()
            || file.section_by_name(".zdebug_info").is_some();
        let split = file.section_by_name(".gnu_debugaltlink").is_some()
            || file.section_by_name(".debug_sup").is_some();
        let has_variable_dwarf = has_sections
            && DwarfInfo::parse(path)
                .and_then(|info| info.has_usable_variable_dwarf())
                .unwrap_or(false);
        let source = if has_variable_dwarf {
            DebugImageSource::Embedded
        } else if split {
            DebugImageSource::Split
        } else {
            DebugImageSource::Missing
        };
        let build_id = file.build_id().ok().flatten().map(|id| {
            id.iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
        Ok(DebugImageMetadata {
            path: path.to_path_buf(),
            format: format!("{:?}", file.format()),
            abi,
            source,
            has_symbols: file.symbols().next().is_some() || file.dynamic_symbols().next().is_some(),
            has_variable_dwarf,
            debug_path: path.to_path_buf(),
            build_id,
        })
    }
}

/// Provider that resolves embedded DWARF first, then GNU debuglink/build-id
/// files. It only selects and validates the debug image; DWARF evaluation
/// remains in the shared gimli layer.
#[derive(Debug, Clone, Default)]
pub struct ExternalDebugImageProvider {
    search_paths: Vec<PathBuf>,
}

impl ExternalDebugImageProvider {
    pub fn new(search_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            search_paths: search_paths.into_iter().collect(),
        }
    }

    fn candidates(&self, path: &Path, file: &object::File<'_>) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(Some((name, crc))) = file.gnu_debuglink() {
            if let Ok(name) = std::str::from_utf8(name) {
                if let Some(parent) = path.parent() {
                    candidates.push(parent.join(name));
                    candidates.push(parent.join(format!("{name}.debug")));
                }
                candidates.push(path.with_extension("debug"));
                for root in &self.search_paths {
                    candidates.push(root.join(name));
                    if let Ok(absolute) = path.canonicalize() {
                        if let Ok(relative) = absolute.strip_prefix(Path::new("/")) {
                            candidates.push(root.join(relative).with_extension("debug"));
                        }
                    }
                }
                // Keep only candidates whose GNU debuglink CRC matches. The
                // check is performed by `valid_debug_candidate` below.
                let debuglink_candidates = candidates
                    .iter()
                    .filter(|candidate| Self::valid_debug_candidate(candidate, crc))
                    .cloned()
                    .collect::<Vec<_>>();
                if !debuglink_candidates.is_empty() {
                    return debuglink_candidates;
                }
            }
        }
        if let Ok(Some(build_id)) = file.build_id() {
            if build_id.len() >= 2 {
                let hex = build_id
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let (prefix, suffix) = hex.split_at(2);
                for root in &self.search_paths {
                    candidates.push(
                        root.join(".build-id")
                            .join(prefix)
                            .join(format!("{suffix}.debug")),
                    );
                }
                candidates.push(
                    PathBuf::from("/usr/lib/debug/.build-id")
                        .join(prefix)
                        .join(format!("{suffix}.debug")),
                );
            }
        }
        candidates
    }

    fn valid_debug_candidate(path: &Path, expected_crc: u32) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        gnu_debuglink_crc32(&bytes) == expected_crc
    }
}

impl DebugImageProvider for ExternalDebugImageProvider {
    fn load(&self, path: &Path) -> Result<DebugImageMetadata, DebugImageError> {
        let embedded = EmbeddedDebugImageProvider.load(path)?;
        if embedded.has_variable_dwarf {
            return Ok(embedded);
        }
        let bytes = std::fs::read(path).map_err(|source| DebugImageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file = object::File::parse(bytes.as_slice())
            .map_err(|source| DebugImageError::Object(source.to_string()))?;
        for candidate in self.candidates(path, &file) {
            if let Ok(metadata) = EmbeddedDebugImageProvider.load(&candidate) {
                if metadata.has_variable_dwarf {
                    return Ok(DebugImageMetadata {
                        path: path.to_path_buf(),
                        source: DebugImageSource::External,
                        debug_path: candidate,
                        ..metadata
                    });
                }
            }
        }
        Ok(embedded)
    }
}

fn gnu_debuglink_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[derive(Debug, thiserror::Error)]
pub enum DebugImageError {
    #[error("failed to read debug image {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid object file: {0}")]
    Object(String),
    #[error("unsupported debug-image format: {0}")]
    UnsupportedFormat(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_path_is_diagnosable() {
        let error = EmbeddedDebugImageProvider
            .load(Path::new("/definitely/missing/detrix-binary"))
            .unwrap_err();
        assert!(error.to_string().contains("failed to read debug image"));
    }

    #[test]
    fn invalid_object_is_not_reported_ready() {
        let path = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(path.path(), b"not an executable").unwrap();
        let error = EmbeddedDebugImageProvider.load(path.path()).unwrap_err();
        assert!(error.to_string().contains("invalid object file"));
    }

    #[test]
    fn crc32_matches_gnu_debuglink_reference() {
        assert_eq!(gnu_debuglink_crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    #[ignore = "requires ELF fixture paths in DETRIX_EXTERNAL_DEBUG_BINARY and DETRIX_EXTERNAL_DEBUG_IMAGE"]
    fn external_provider_selects_gnu_debuglink_image() {
        let binary = std::env::var("DETRIX_EXTERNAL_DEBUG_BINARY")
            .expect("DETRIX_EXTERNAL_DEBUG_BINARY must point to stripped ELF");
        let debug = std::env::var("DETRIX_EXTERNAL_DEBUG_IMAGE")
            .expect("DETRIX_EXTERNAL_DEBUG_IMAGE must point to .debug ELF");
        let metadata = ExternalDebugImageProvider::default()
            .load(Path::new(&binary))
            .expect("external provider should load the debuglink pair");
        assert_eq!(metadata.source, DebugImageSource::External);
        assert_eq!(metadata.debug_path, PathBuf::from(debug));
        assert!(metadata.has_variable_dwarf);
        assert!(metadata.build_id.is_some());
    }
}
