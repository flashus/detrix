//! Shared proptest configuration for consistent test behavior across crates.
//!
//! This module provides standardized configurations for property-based testing
//! that can be used across all Detrix crates.
//!
//! # Usage
//!
//! ```rust,ignore
//! use detrix_testing::proptest_config;
//!
//! proptest! {
//!     #![proptest_config(proptest_config::auto_config())]
//!
//!     #[test]
//!     fn my_property(x in 0..100i32) {
//!         // ...
//!     }
//! }
//! ```
//!
//! # CI Integration
//!
//! Set `PROPTEST_CASES` environment variable to control test thoroughness:
//! - PR/fast: `PROPTEST_CASES=64`
//! - Nightly: `PROPTEST_CASES=5000`

use proptest::prelude::*;

/// CI-optimized config: fast tests with small case count.
///
/// Use this for quick feedback in PR checks.
pub fn ci_config() -> ProptestConfig {
    ProptestConfig {
        cases: 64,
        max_shrink_iters: 100,
        ..ProptestConfig::default()
    }
}

/// Nightly config: thorough testing with many cases.
///
/// Use this for comprehensive nightly runs.
pub fn nightly_config() -> ProptestConfig {
    ProptestConfig {
        cases: 5000,
        max_shrink_iters: 10000,
        ..ProptestConfig::default()
    }
}

/// Get config based on PROPTEST_CASES env var.
///
/// Defaults to 256 cases if not set.
///
/// # Environment Variables
///
/// - `PROPTEST_CASES`: Number of test cases to run (default: 256)
pub fn auto_config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);

    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

/// Standard config for most property tests.
///
/// Uses 256 cases with reasonable shrinking limits.
pub fn standard_config() -> ProptestConfig {
    ProptestConfig {
        cases: 256,
        max_shrink_iters: 1000,
        ..ProptestConfig::default()
    }
}
