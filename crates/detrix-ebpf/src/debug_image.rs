//! Debug-image discovery independent of any source language.
//!
//! This is intentionally small: embedded DWARF is the first provider.  The
//! same metadata contract can later be backed by GNU debuglink/build-id or
//! split-DWARF providers without changing profiles or probe runtimes.

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
        let has_variable_dwarf = file.section_by_name(".debug_info").is_some()
            || file.section_by_name(".zdebug_info").is_some();
        let source = if has_variable_dwarf {
            DebugImageSource::Embedded
        } else {
            DebugImageSource::Missing
        };
        Ok(DebugImageMetadata {
            path: path.to_path_buf(),
            format: format!("{:?}", file.format()),
            abi,
            source,
            has_symbols: file.symbols().next().is_some() || file.dynamic_symbols().next().is_some(),
            has_variable_dwarf,
        })
    }
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
}
