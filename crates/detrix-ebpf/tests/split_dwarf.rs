//! Integration coverage for Rust split-DWARF sidecars.
//!
//! The test is intentionally environment-driven: Linux fixture generation is
//! performed by the Docker task so the host does not need an ELF toolchain.

use detrix_ebpf::dwarf::DwarfInfo;
use std::env;
use std::path::PathBuf;

fn required(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    let path = PathBuf::from(value);
    path.exists().then_some(path)
}

#[test]
fn rust_split_dwarf_dwo_and_dwp_load_variable_units() {
    let Some(binary) = required("DETRIX_SPLIT_DWARF_BINARY") else {
        eprintln!("skipping split-DWARF fixture test: DETRIX_SPLIT_DWARF_BINARY unset");
        return;
    };
    let Some(dwo) = required("DETRIX_SPLIT_DWARF_DWO") else {
        eprintln!("skipping split-DWARF fixture test: DETRIX_SPLIT_DWARF_DWO unset");
        return;
    };
    let Some(dwp) = required("DETRIX_SPLIT_DWARF_DWP") else {
        eprintln!("skipping split-DWARF fixture test: DETRIX_SPLIT_DWARF_DWP unset");
        return;
    };

    for (label, sidecar) in [("dwo", dwo), ("dwp", dwp)] {
        let info = DwarfInfo::parse_with_debug_path(&binary, Some(&sidecar))
            .unwrap_or_else(|error| panic!("{label} sidecar should parse: {error}"));
        assert!(
            info.has_usable_variable_dwarf()
                .unwrap_or_else(|error| panic!("{label} variable-DWARF scan failed: {error}")),
            "{label} sidecar must expose at least one usable variable location"
        );
    }
}
