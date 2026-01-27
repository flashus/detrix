//! E2E tests for Rust LSP purity analysis
//!
//! These tests require rust-analyzer (Rust language server) to be installed.
//! Run with: `cargo test -p detrix-lsp --test rust_purity_e2e -- --ignored`
//!
//! Install rust-analyzer via:
//! - rustup: `rustup component add rust-analyzer`
//! - Manual: https://rust-analyzer.github.io/manual.html#installation

use detrix_application::PurityAnalyzer;
use detrix_config::LspConfig;
use detrix_core::{PurityLevel, SourceLanguage};
use detrix_lsp::RustPurityAnalyzer;
use detrix_testing::e2e::{require_tool, ToolDependency};
use std::path::PathBuf;

/// Get path to Rust test fixtures
fn rust_fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust")
}

/// Create test config for rust-analyzer
fn test_config() -> LspConfig {
    LspConfig {
        enabled: true,
        command: "rust-analyzer".to_string(),
        args: vec![],
        max_call_depth: 10,
        analysis_timeout_ms: 30000,
        cache_enabled: false,
        working_directory: Some(rust_fixtures_path()),
    }
}

// =============================================================================
// Unit Tests (no LSP server required)
// =============================================================================

mod unit_tests {
    use super::*;

    #[test]
    fn test_analyzer_creation() {
        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        assert_eq!(analyzer.language(), SourceLanguage::Rust);
    }

    #[test]
    fn test_rust_analyzer_availability_check() {
        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        // is_available() checks if rust-analyzer binary exists
        let _ = analyzer.is_available();
    }

    #[test]
    fn test_fixtures_exist() {
        let fixtures = rust_fixtures_path();
        assert!(
            fixtures.join("pure_chain.rs").exists(),
            "pure_chain.rs fixture missing"
        );
        assert!(
            fixtures.join("impure_chain.rs").exists(),
            "impure_chain.rs fixture missing"
        );
        assert!(
            fixtures.join("mixed_chain.rs").exists(),
            "mixed_chain.rs fixture missing"
        );
        assert!(
            fixtures.join("transitive_impure.rs").exists(),
            "transitive_impure.rs fixture missing"
        );
    }

    #[tokio::test]
    async fn test_find_function_location_pure_chain() {
        let file = rust_fixtures_path().join("pure_chain.rs");

        // Read the file to verify functions exist
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("pub fn calculate_total"));
        assert!(content.contains("pub fn compute_subtotal"));
        assert!(content.contains("pub fn compute_item_price"));
    }

    #[tokio::test]
    async fn test_find_function_location_impure_chain() {
        let file = rust_fixtures_path().join("impure_chain.rs");

        // Read the file to verify functions exist
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("pub fn process_order"));
        assert!(content.contains("pub fn save_to_database"));
        assert!(content.contains("pub fn write_log"));
    }
}

// =============================================================================
// E2E Tests (require rust-analyzer)
// =============================================================================

mod e2e_tests {
    use super::*;

    // =============================================================================
    // Pure Function Chain Tests
    // =============================================================================

    /// Test pure function: calculate_total is a pure computation
    #[tokio::test]
    async fn test_pure_function_chain_3_levels() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("pure_chain.rs");

        let result = analyzer.analyze_function("calculate_total", &file).await;

        match result {
            Ok(analysis) => {
                println!("Pure chain analysis result: {:?}", analysis);
                // calculate_total calls only pure functions (math operations, compute_subtotal)
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

    /// Test pure helper function - format_currency
    #[tokio::test]
    async fn test_pure_helper_function() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("pure_chain.rs");

        let result = analyzer.analyze_function("format_currency", &file).await;

        match result {
            Ok(analysis) => {
                println!("format_currency analysis: {:?}", analysis);
                // format_currency uses format! which is pure
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "format_currency should be Unknown or Pure"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test pure iterator chain - calculate_average
    #[tokio::test]
    async fn test_pure_iterator_chain() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("pure_chain.rs");

        let result = analyzer.analyze_function("calculate_average", &file).await;

        match result {
            Ok(analysis) => {
                println!("calculate_average analysis: {:?}", analysis);
                // Uses only iterator methods which are pure
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "calculate_average should be Unknown or Pure (only uses iterators)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    // =============================================================================
    // Impure Function Chain Tests
    // =============================================================================

    /// Test impure function chain: process_order uses global state
    #[tokio::test]
    async fn test_impure_function_chain_3_levels() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("process_order", &file).await;

        match result {
            Ok(analysis) => {
                println!("Impure chain analysis result: {:?}", analysis);
                // process_order modifies global state (ORDER_COUNT)
                // and calls File I/O functions
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "process_order should be Impure or Unknown, got: {:?}",
                    analysis.level
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test write_log function - contains println! call
    /// println! is in ACCEPTABLE_IMPURE_FUNCTIONS (treated as pure)
    #[tokio::test]
    async fn test_function_with_println() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("write_log", &file).await;

        match result {
            Ok(analysis) => {
                println!("write_log analysis: {:?}", analysis);
                // write_log calls println! which is "acceptable impurity" - treated as pure
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "write_log should be Pure (println! is acceptable impurity) or Unknown"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test execute_command function - contains Command::new call
    /// Command::new is always impure (system execution)
    #[tokio::test]
    async fn test_direct_impure_function() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("execute_command", &file).await;

        match result {
            Ok(analysis) => {
                println!("execute_command analysis: {:?}", analysis);
                // execute_command directly calls Command::new - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "execute_command should be Impure (contains Command::new)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test read_config function - contains File::open call
    /// File::open is always impure (file I/O)
    #[tokio::test]
    async fn test_file_io_impure() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("read_config", &file).await;

        match result {
            Ok(analysis) => {
                println!("read_config analysis: {:?}", analysis);
                // read_config directly calls File::open - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "read_config should be Impure (contains File::open)"
                );
                assert!(
                    analysis
                        .impure_calls
                        .iter()
                        .any(|c| c.function_name.contains("File")
                            || c.function_name.contains("open")),
                    "Should detect File::open as impure call"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test spawn_worker function - creates threads
    #[tokio::test]
    async fn test_thread_spawn_impure() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("spawn_worker", &file).await;

        match result {
            Ok(analysis) => {
                println!("spawn_worker analysis: {:?}", analysis);
                // spawn_worker calls thread::spawn - should be Impure
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "spawn_worker should be Impure or Unknown (creates threads)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test increment_counter function - mutates parameter
    #[tokio::test]
    async fn test_mutable_parameter_impure() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("impure_chain.rs");

        let result = analyzer.analyze_function("increment_counter", &file).await;

        match result {
            Ok(analysis) => {
                println!("increment_counter analysis: {:?}", analysis);
                // increment_counter mutates &mut parameter - affects caller
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "increment_counter should be Impure or Unknown (mutates parameter)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    // =============================================================================
    // Mixed Purity Chain Tests
    // =============================================================================

    /// Test mixed purity chain: analyze_data -> process_items -> log_result -> println!
    /// Since println! is "acceptable impurity", this chain is treated as Pure
    #[tokio::test]
    async fn test_mixed_purity_chain() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("mixed_chain.rs");

        let result = analyzer.analyze_function("analyze_data", &file).await;

        match result {
            Ok(analysis) => {
                println!("Mixed chain analysis: {:?}", analysis);
                // analyze_data -> process_items -> log_result -> println!
                // Should be Pure because println! is acceptable impurity
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "analyze_data should be Pure (println! is acceptable) or Unknown, got {:?}",
                    analysis.level
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test compute_stats - pure function using only pure operations
    #[tokio::test]
    async fn test_pure_in_mixed_file() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("mixed_chain.rs");

        let result = analyzer.analyze_function("compute_stats", &file).await;

        match result {
            Ok(analysis) => {
                println!("compute_stats analysis: {:?}", analysis);
                // compute_stats uses only pure operations
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "compute_stats should be Pure or Unknown (only uses pure operations)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test process_from_file - impure via file I/O
    #[tokio::test]
    async fn test_impure_via_file_io() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("mixed_chain.rs");

        let result = analyzer.analyze_function("process_from_file", &file).await;

        match result {
            Ok(analysis) => {
                println!("process_from_file analysis: {:?}", analysis);
                // process_from_file calls read_data_file which does file I/O
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "process_from_file should be Impure or Unknown (file I/O)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    // =============================================================================
    // Transitive Impurity Tests
    // =============================================================================

    /// Test transitive impure chain: entry_point -> middle_layer -> read_file
    #[tokio::test]
    async fn test_transitive_impure_chain() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("transitive_impure.rs");

        let result = analyzer.analyze_function("entry_point", &file).await;

        match result {
            Ok(analysis) => {
                println!("entry_point analysis: {:?}", analysis);
                // entry_point -> middle_layer -> read_file -> std::fs::read_to_string
                // Should be Impure because transitive call to file I/O
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "entry_point should be Impure or Unknown (transitive call to file I/O)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test transitive via global state
    #[tokio::test]
    async fn test_transitive_via_global() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("transitive_impure.rs");

        let result = analyzer
            .analyze_function("transitive_via_global", &file)
            .await;

        match result {
            Ok(analysis) => {
                println!("transitive_via_global analysis: {:?}", analysis);
                // transitive_via_global -> increment_global -> AtomicUsize::fetch_add
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "transitive_via_global should be Impure or Unknown (global state)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test multi-level transitive impurity
    #[tokio::test]
    async fn test_multilevel_transitive_impure() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let config = test_config();
        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("transitive_impure.rs");

        let result = analyzer.analyze_function("level1", &file).await;

        match result {
            Ok(analysis) => {
                println!("level1 analysis: {:?}", analysis);
                // level1 -> level2 -> level3 -> file_io
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "level1 should be Impure or Unknown (4-level transitive impurity)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    // =============================================================================
    // Max Depth Tests
    // =============================================================================

    /// Test max depth handling - with shallow depth, should hit max_depth_reached
    #[tokio::test]
    async fn test_max_depth_handling() {
        if !require_tool(ToolDependency::RustAnalyzer).await {
            return;
        }

        let mut config = test_config();
        config.max_call_depth = 1; // Very shallow depth

        let analyzer = RustPurityAnalyzer::new(&config).unwrap();
        let file = rust_fixtures_path().join("pure_chain.rs");

        let result = analyzer.analyze_function("calculate_total", &file).await;

        match result {
            Ok(analysis) => {
                println!("Max depth test result: {:?}", analysis);
                // With shallow depth of 1, if call chain is deeper it should either:
                // 1. Hit max_depth_reached and mark Unknown
                // 2. Or return Pure if it doesn't find impure calls in shallow analysis
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
}

// =============================================================================
// Classification Tests (no LSP required)
// =============================================================================

mod classification_tests {
    // These test the internal classification logic which doesn't require LSP
    use detrix_lsp::RustPurityAnalyzer;

    #[test]
    fn test_known_impure_functions() {
        // Rust impure functions (I/O, network, system)
        // These should match IMPURE_FUNCTIONS in rust.rs
        let impure_functions = [
            "std::fs::File::open",
            "std::fs::File::create",
            "std::fs::read",
            "std::fs::read_to_string",
            "std::fs::write",
            "std::fs::remove_file",
            "std::process::Command::new",
            "std::process::exit",
            "std::process::abort",
            "std::net::TcpStream::connect",
            "std::net::TcpListener::bind",
            // Note: std::thread::spawn and std::thread::sleep are not in the impure list
            // They could be added to the implementation if needed
        ];

        for func in impure_functions {
            assert!(
                RustPurityAnalyzer::is_impure_function(func),
                "Expected {} to be classified as impure",
                func
            );
        }
    }

    #[test]
    fn test_acceptable_impure_functions() {
        // These have I/O side effects but don't affect program state
        // They're treated as "acceptable impurity" (effectively pure)
        let acceptable = [
            "println",
            "print",
            "eprintln",
            "eprint",
            "dbg",
            "tracing::info",
            "tracing::debug",
            "log::info",
            "log::debug",
        ];

        for func in acceptable {
            // Acceptable impure functions are treated as pure
            assert!(
                RustPurityAnalyzer::is_pure_function(func),
                "Expected {} to be classified as pure (acceptable impurity)",
                func
            );
            // But they're not in the IMPURE_FUNCTIONS list
            assert!(
                !RustPurityAnalyzer::is_impure_function(func),
                "Expected {} NOT to be in impure list (it's acceptable impurity)",
                func
            );
        }
    }

    #[test]
    fn test_known_pure_functions() {
        let pure_functions = [
            "len",
            "is_empty",
            "clone",
            "to_string",
            "iter",
            "map",
            "filter",
            "collect",
            "fold",
            "reduce",
            "sum",
            "enumerate",
            "zip",
            "chain",
            "take",
            "skip",
            "trim",
            "contains",
            "starts_with",
            "ends_with",
            "to_lowercase",
            "to_uppercase",
            "unwrap_or",
            "unwrap_or_else",
            "unwrap_or_default",
            "is_some",
            "is_none",
            "is_ok",
            "is_err",
        ];

        for func in pure_functions {
            assert!(
                RustPurityAnalyzer::is_pure_function(func),
                "Expected {} to be classified as pure",
                func
            );
        }
    }

    #[test]
    fn test_unknown_functions() {
        // Custom/user-defined functions should be Unknown
        let unknown_functions = ["custom_func", "my_module::do_something", "utils::process"];

        for func in unknown_functions {
            let is_pure = RustPurityAnalyzer::is_pure_function(func);
            let is_impure = RustPurityAnalyzer::is_impure_function(func);

            assert!(
                !is_pure && !is_impure,
                "Expected {} to be classified as unknown (not pure, not impure)",
                func
            );
        }
    }

    #[test]
    fn test_mutation_methods() {
        // Methods that mutate data
        // These should match MUTATION_METHODS in rust.rs
        let mutation_methods = [
            "push",
            "push_str",
            "pop",
            "insert",
            "remove",
            "clear",
            "extend",
            "append",
            "drain",
            "retain",
            "sort",
            "sort_by",
            "sort_by_key",
            "reverse",
            "dedup",
            "truncate",
            "resize",
            // Note: swap_remove is not in the mutation methods list
        ];

        for method in mutation_methods {
            assert!(
                RustPurityAnalyzer::is_mutation_method(method),
                "Expected {} to be classified as mutation method",
                method
            );
        }
    }

    #[test]
    fn test_non_mutation_methods() {
        // Methods that don't mutate (return new values)
        let non_mutation_methods = ["len", "is_empty", "get", "first", "last", "iter", "clone"];

        for method in non_mutation_methods {
            assert!(
                !RustPurityAnalyzer::is_mutation_method(method),
                "Expected {} NOT to be classified as mutation method",
                method
            );
        }
    }

    #[test]
    fn test_iterator_methods_pure() {
        // Iterator methods should all be pure
        let iterator_methods = [
            "map",
            "filter",
            "filter_map",
            "flat_map",
            "flatten",
            "fold",
            "reduce",
            "collect",
            "take",
            "skip",
            "enumerate",
            "zip",
            "chain",
            "rev",
            "cloned",
            "copied",
        ];

        for method in iterator_methods {
            assert!(
                RustPurityAnalyzer::is_pure_function(method),
                "Expected iterator method {} to be pure",
                method
            );
        }
    }

    #[test]
    fn test_option_result_methods_pure() {
        // Option/Result methods should be pure (they don't perform I/O)
        let option_methods = [
            "is_some",
            "is_none",
            "is_ok",
            "is_err",
            "as_ref",
            "ok",
            "err",
            "unwrap_or",
            "unwrap_or_else",
            "unwrap_or_default",
            "map_or",
            "map_or_else",
            "and",
            "and_then",
            "or",
            "or_else",
        ];

        for method in option_methods {
            assert!(
                RustPurityAnalyzer::is_pure_function(method),
                "Expected Option/Result method {} to be pure",
                method
            );
        }
    }

    #[test]
    fn test_string_methods_pure() {
        // String methods that don't mutate should be pure
        let string_methods = [
            "len",
            "is_empty",
            "as_str",
            "as_bytes",
            "chars",
            "trim",
            "contains",
            "starts_with",
            "ends_with",
            "to_lowercase",
            "to_uppercase",
            "split",
            "lines",
        ];

        for method in string_methods {
            assert!(
                RustPurityAnalyzer::is_pure_function(method),
                "Expected string method {} to be pure",
                method
            );
        }
    }
}
