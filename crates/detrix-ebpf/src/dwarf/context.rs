//! Language-neutral debug-image/DWARF context facade.
//!
//! The parser still contains compatibility-oriented Go metadata helpers, but
//! all object/DWARF ownership is exposed through this facade so profiles do
//! not need to know how gimli/object storage is initialized. Go-only runtime
//! metadata remains available only through `DwarfInfo`'s explicit methods.

use super::parser::DwarfInfo;
use super::types::{ProbePoint, ProbeResolutionDiagnostics, TargetArchitecture};
use crate::error::Result;
use std::path::Path;

#[derive(Debug)]
pub struct DwarfContext {
    image: DwarfInfo,
}

impl DwarfContext {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            image: DwarfInfo::parse(path)?,
        })
    }

    pub fn architecture(&self) -> TargetArchitecture {
        self.image.target_architecture()
    }

    pub fn binary_path(&self) -> &Path {
        self.image.binary_path()
    }

    /// Whether the selected debug image contains at least one variable DIE
    /// with a usable location. Section presence alone is insufficient for
    /// probe readiness (stripped and split-DWARF images can retain headers).
    pub fn has_usable_variable_dwarf(&self) -> Result<bool> {
        self.image.has_usable_variable_dwarf()
    }

    /// Compute the Go runtime metadata only for callers that explicitly use
    /// the Go profile. Keeping these methods off the profile-neutral adapter
    /// path prevents Rust and future profiles from inheriting Go layout rules.
    pub fn go_g_addr_offset(&self) -> Result<Option<i64>> {
        self.image.g_addr_offset()
    }

    pub fn goid_field_offset(&self) -> Option<u64> {
        self.image.goid_field_offset()
    }

    pub fn resolve_probe_point(
        &self,
        file: &str,
        line: u32,
        variables: &[String],
    ) -> Result<ProbePoint> {
        self.image.resolve_probe_point(file, line, variables, 2)
    }

    pub fn resolve_probe_point_with_diagnostics(
        &self,
        file: &str,
        line: u32,
        variables: &[String],
    ) -> Result<(ProbePoint, ProbeResolutionDiagnostics)> {
        self.image
            .resolve_probe_point_with_diagnostics(file, line, variables, 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_image_is_reported_without_profile_logic() {
        assert!(DwarfContext::open("/definitely/missing/binary").is_err());
    }
}
