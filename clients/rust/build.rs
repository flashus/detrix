//! Build script for detrix-rs client
//! Generates build_info.rs with version and git metadata
#![allow(clippy::panic, clippy::expect_used)]

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Get version from Cargo.toml
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());

    // Try to get build commit from CI environment
    let build_commit = env::var("DETRIX_BUILD_COMMIT")
        .or_else(|_| env::var("GIT_COMMIT"))
        .or_else(|_| env::var("CI_COMMIT_SHA"))
        .or_else(|_| env::var("GITHUB_SHA"))
        .unwrap_or_else(|_| "unknown".to_string());

    let build_tag = env::var("DETRIX_BUILD_TAG")
        .or_else(|_| env::var("GIT_TAG"))
        .or_else(|_| env::var("CI_COMMIT_TAG"))
        .or_else(|_| env::var("GITHUB_REF_NAME"))
        .unwrap_or_else(|_| version.clone());

    // Generate build_info.rs
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let dest_path = Path::new(&out_dir).join("build_info.rs");

    let build_info = format!(
        r#"/// Client version from Cargo.toml
pub const VERSION: &str = "{}";

/// Git commit SHA at build time (or "unknown")
pub const BUILD_COMMIT: &str = "{}";

/// Build tag/version (or VERSION if not set)
pub const BUILD_TAG: &str = "{}";"#,
        version, build_commit, build_tag
    );

    fs::write(&dest_path, build_info).expect("Could not write build_info.rs");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DETRIX_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=CI_COMMIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=DETRIX_BUILD_TAG");
    println!("cargo:rerun-if-env-changed=GIT_TAG");
    println!("cargo:rerun-if-env-changed=CI_COMMIT_TAG");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
}
