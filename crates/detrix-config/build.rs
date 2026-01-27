//! Build script for detrix-config
//!
//! Embeds config templates at compile time using CARGO_MANIFEST_DIR.
//! This is more robust than relative paths in include_str!() because:
//! 1. Cargo automatically tracks the files for rebuild
//! 2. Clear error messages if files are missing
//! 3. Works regardless of crate location in workspace

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let workspace_root = PathBuf::from(&manifest_dir)
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("Could not find workspace root")
        .to_path_buf();

    let full_config_path = workspace_root.join("detrix.toml");
    let minimal_config_path = workspace_root.join("detrix.minimal.toml");

    // Read config files
    let full_config = fs::read_to_string(&full_config_path).unwrap_or_else(|e| {
        panic!(
            "Failed to read full config at {}: {}",
            full_config_path.display(),
            e
        )
    });

    let minimal_config = fs::read_to_string(&minimal_config_path).unwrap_or_else(|e| {
        panic!(
            "Failed to read minimal config at {}: {}",
            minimal_config_path.display(),
            e
        )
    });

    // Generate a file with the embedded content
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let dest_path = PathBuf::from(&out_dir).join("embedded_configs.rs");

    // Escape the content for inclusion as a raw string literal
    let content = format!(
        r####"/// Full configuration template embedded at compile time
/// Source: detrix.toml (workspace root)
pub const FULL_CONFIG: &str = r###"{}"###;

/// Minimal configuration template embedded at compile time
/// Source: detrix.minimal.toml (workspace root)
pub const MINIMAL_CONFIG: &str = r###"{}"###;
"####,
        full_config, minimal_config
    );

    fs::write(&dest_path, content).expect("Could not write embedded_configs.rs");

    // Tell Cargo to rerun if the config files change
    println!("cargo:rerun-if-changed={}", full_config_path.display());
    println!("cargo:rerun-if-changed={}", minimal_config_path.display());
    println!("cargo:rerun-if-changed=build.rs");
}
