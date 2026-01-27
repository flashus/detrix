//! E2E tests for LSP purity analysis
//!
//! These tests require a Python LSP server to be installed.
//! Run with: `cargo test -p detrix-lsp --test purity_e2e -- --ignored`
//!
//! **Recommended: pyright** (full call hierarchy support)
//! Install via: `npm install -g pyright` or `pip install pyright`
//!
//! **Alternative: pylsp** (static analysis fallback only)
//! Install via: `pip install python-lsp-server`
//! Note: pylsp does NOT support call hierarchy, falls back to static classification.
//!
//! To switch between servers, set the environment variable:
//! - `LSP_SERVER=pyright` (default)
//! - `LSP_SERVER=pylsp`

use detrix_application::PurityAnalyzer;
use detrix_config::LspConfig;
use detrix_core::PurityLevel;
use detrix_lsp::PythonPurityAnalyzer;
use detrix_testing::e2e::{require_tool, ToolDependency};
use std::path::PathBuf;

/// Get path to test fixtures
fn fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Create test config based on LSP_SERVER environment variable
/// Default: pyright (best call hierarchy support)
fn test_config() -> LspConfig {
    let server = std::env::var("LSP_SERVER").unwrap_or_else(|_| "pyright".to_string());

    match server.as_str() {
        "pylsp" => LspConfig {
            enabled: true,
            command: "pylsp".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 30000,
            cache_enabled: false,
            working_directory: Some(fixtures_path()),
        },
        "jedi" => LspConfig {
            enabled: true,
            command: "jedi-language-server".to_string(),
            args: vec![],
            max_call_depth: 10,
            analysis_timeout_ms: 30000,
            cache_enabled: false,
            working_directory: Some(fixtures_path()),
        },
        _ => LspConfig {
            // Default to pyright
            enabled: true,
            command: "pyright-langserver".to_string(),
            args: vec!["--stdio".to_string()],
            max_call_depth: 10,
            analysis_timeout_ms: 30000,
            cache_enabled: false,
            working_directory: Some(fixtures_path()),
        },
    }
}

/// Check if using an LSP server with full call hierarchy support
fn _uses_call_hierarchy() -> bool {
    let server = std::env::var("LSP_SERVER").unwrap_or_else(|_| "pyright".to_string());
    // pyright supports call hierarchy; pylsp does not
    server == "pyright"
}

// =============================================================================
// Unit Tests (no LSP server required)
// =============================================================================

mod unit_tests {
    use super::*;

    #[tokio::test]
    async fn test_classify_print_as_impure() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("impure_chain.py");

        let result = analyzer.analyze_function("write_log", &file).await;

        match result {
            Ok(analysis) => {
                assert_ne!(
                    analysis.level,
                    PurityLevel::Impure,
                    "write_log should not be classified as Impure just because it uses print()"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    #[test]
    fn test_fixtures_exist() {
        let fixtures = fixtures_path();
        assert!(fixtures.join("pure_chain.py").exists());
        assert!(fixtures.join("impure_chain.py").exists());
        assert!(fixtures.join("mixed_chain.py").exists());
    }
}

// =============================================================================
// E2E Tests (require pylsp)
// =============================================================================

mod e2e_tests {
    use super::*;

    // =============================================================================
    // Static Fallback Tests (pylsp without call hierarchy support)
    // =============================================================================
    // These tests verify that the analyzer works correctly when the LSP server
    // doesn't support call hierarchy. The analyzer falls back to static
    // classification based on function names only.

    /// Test custom function: calculate_total is not in pure/impure lists -> Unknown
    /// Since pylsp doesn't support call hierarchy, we get static fallback
    #[tokio::test]
    async fn test_pure_function_chain_3_levels() {
        if !require_tool(ToolDependency::Pylsp).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("pure_chain.py");

        // Analyze the top-level function
        let result = analyzer.analyze_function("calculate_total", &file).await;

        match result {
            Ok(analysis) => {
                println!("Pure chain analysis result: {:?}", analysis);
                // With pylsp (no call hierarchy), custom functions are Unknown
                // because we can only classify by name, not by call graph
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "calculate_total should be Unknown (static fallback) or Pure, got: {:?}",
                    analysis
                );
                assert!(!analysis.max_depth_reached, "Should not hit max depth");
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test pure helper function - uses static fallback with pylsp
    #[tokio::test]
    async fn test_pure_helper_function() {
        if !require_tool(ToolDependency::Pylsp).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("pure_chain.py");

        let result = analyzer.analyze_function("format_currency", &file).await;

        match result {
            Ok(analysis) => {
                println!("format_currency analysis: {:?}", analysis);
                // format_currency is not in pure/impure lists -> Unknown with static fallback
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "format_currency should be Unknown or Pure with static fallback"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test impure function chain: process_order uses 'global' statement
    /// STRICT: Should be Impure because it modifies global state
    #[tokio::test]
    async fn test_impure_function_chain_3_levels() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("impure_chain.py");

        let result = analyzer.analyze_function("process_order", &file).await;

        match result {
            Ok(analysis) => {
                println!("Impure chain analysis result: {:?}", analysis);
                // process_order has 'global _order_count' - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "process_order should be Impure (uses global statement)"
                );
                // Check that global variable mutation was detected
                // New scope-aware format: reason contains "global variable"
                // Old format: function_name == "global"
                let has_global_detection = analysis.impure_calls.iter().any(|c| {
                    c.function_name == "global" || c.reason.to_lowercase().contains("global")
                });
                assert!(
                    has_global_detection,
                    "Should detect global variable mutation as impure. Got: {:?}",
                    analysis.impure_calls
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test write_log function - directly contains print() call
    /// print() is now considered "acceptable impurity" (doesn't affect program state)
    /// so write_log should be Pure
    #[tokio::test]
    async fn test_function_with_print() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("impure_chain.py");

        let result = analyzer.analyze_function("write_log", &file).await;

        match result {
            Ok(analysis) => {
                println!("write_log analysis: {:?}", analysis);
                // write_log calls print() which is "acceptable impurity" - should be Pure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Pure,
                    "write_log should be Pure (print is acceptable impurity)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test execute_command function - directly contains os.system() call
    /// STRICT: Should always be Impure because os.system() is in the function body
    #[tokio::test]
    async fn test_direct_impure_function() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("impure_chain.py");

        let result = analyzer.analyze_function("execute_command", &file).await;

        match result {
            Ok(analysis) => {
                println!("execute_command analysis: {:?}", analysis);
                // execute_command directly calls os.system() - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "execute_command should be Impure (contains os.system())"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test read_config function - directly contains open() and read() calls
    /// STRICT: Should always be Impure because open() is in the function body
    #[tokio::test]
    async fn test_file_io_impure() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("impure_chain.py");

        let result = analyzer.analyze_function("read_config", &file).await;

        match result {
            Ok(analysis) => {
                println!("read_config analysis: {:?}", analysis);
                // read_config directly calls open() - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "read_config should be Impure (contains open())"
                );
                assert!(
                    analysis
                        .impure_calls
                        .iter()
                        .any(|c| c.function_name == "open"),
                    "Should detect open() as impure call"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test mixed purity chain: analyze_data -> process_items -> log_result -> print + json.dumps
    ///
    /// This should consistently return Pure because:
    /// - print() is acceptable impurity (logging)
    /// - json.dumps is in PURE_PREFIXES
    /// - stdlib/typeshed internals are skipped during analysis
    #[tokio::test]
    async fn test_mixed_purity_chain() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("mixed_chain.py");

        let result = analyzer.analyze_function("analyze_data", &file).await;

        match result {
            Ok(analysis) => {
                println!("Mixed chain analysis: {:?}", analysis);
                // analyze_data -> process_items -> log_result -> print() + json.dumps()
                // Should be Pure because:
                // - print is acceptable impurity
                // - json.dumps is in PURE_PREFIXES
                // - stdlib internals (c_make_encoder, etc.) are skipped
                assert_eq!(
                    analysis.level,
                    PurityLevel::Pure,
                    "analyze_data should be Pure (print is acceptable, json.dumps is pure), got {:?}",
                    analysis.level
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test compute_stats - pure function using only pure operations (min, max, sum)
    /// STRICT: Should be Pure
    #[tokio::test]
    async fn test_pure_in_mixed_file() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("mixed_chain.py");

        let result = analyzer.analyze_function("compute_stats", &file).await;

        match result {
            Ok(analysis) => {
                println!("compute_stats analysis: {:?}", analysis);
                // compute_stats uses only pure operations (min, max, sum)
                assert_eq!(
                    analysis.level,
                    PurityLevel::Pure,
                    "compute_stats should be Pure (only uses pure operations)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test max depth handling - with shallow depth, should hit max_depth_reached
    #[tokio::test]
    async fn test_max_depth_handling() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let mut config = test_config();
        config.max_call_depth = 1; // Very shallow depth

        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("pure_chain.py");

        let result = analyzer.analyze_function("calculate_total", &file).await;

        match result {
            Ok(analysis) => {
                println!("Max depth test result: {:?}", analysis);
                // With shallow depth of 1, if call chain is deeper it should either:
                // 1. Hit max_depth_reached and mark Unknown
                // 2. Or return Pure if it doesn't find impure calls in shallow analysis
                // Either is acceptable for this test
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "Should get Unknown (max depth) or Pure (no impure calls found)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test transitive impure chain: entry_point -> middle_layer -> read_file (has open())
    /// STRICT: Should be Impure because the call chain eventually reaches open()
    #[tokio::test]
    async fn test_transitive_impure_chain() {
        if !require_tool(ToolDependency::Pyright).await {
            return;
        }

        let config = test_config();
        let analyzer = PythonPurityAnalyzer::new(&config).unwrap();
        let file = fixtures_path().join("transitive_impure.py");

        let result = analyzer.analyze_function("entry_point", &file).await;

        match result {
            Ok(analysis) => {
                println!("entry_point analysis: {:?}", analysis);
                // entry_point -> middle_layer -> read_file -> open()
                // Should be Impure because transitive call to open()
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "entry_point should be Impure (transitive call to open())"
                );
                // Check that the impure call path includes the chain
                assert!(
                    analysis
                        .impure_calls
                        .iter()
                        .any(|c| c.function_name == "open"),
                    "Should detect open() as impure call through the chain"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }
}

// (classification_tests removed: these were print-only and did not assert behavior)
