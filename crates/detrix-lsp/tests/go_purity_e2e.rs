//! E2E tests for Go LSP purity analysis
//!
//! These tests require gopls (Go language server) to be installed.
//! Run with: `cargo test -p detrix-lsp --test go_purity_e2e -- --ignored`
//!
//! Install gopls via: `go install golang.org/x/tools/gopls@latest`

use detrix_application::PurityAnalyzer;
use detrix_config::LspConfig;
use detrix_core::{PurityLevel, SourceLanguage};
use detrix_lsp::GoPurityAnalyzer;
use detrix_testing::e2e::{require_tool, ToolDependency};
use std::path::PathBuf;

/// Get path to Go test fixtures
fn go_fixtures_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go")
}

/// Create test config for gopls
fn test_config() -> LspConfig {
    LspConfig {
        enabled: true,
        command: "gopls".to_string(),
        args: vec!["serve".to_string()],
        max_call_depth: 10,
        analysis_timeout_ms: 30000,
        cache_enabled: false,
        working_directory: Some(go_fixtures_path()),
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
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        assert_eq!(analyzer.language(), SourceLanguage::Go);
    }

    #[test]
    fn test_gopls_availability_check() {
        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        // is_available() checks if gopls binary exists
        let _ = analyzer.is_available();
    }

    #[test]
    fn test_fixtures_exist() {
        let fixtures = go_fixtures_path();
        assert!(
            fixtures.join("pure_chain.go").exists(),
            "pure_chain.go fixture missing"
        );
        assert!(
            fixtures.join("impure_chain.go").exists(),
            "impure_chain.go fixture missing"
        );
        assert!(
            fixtures.join("mixed_chain.go").exists(),
            "mixed_chain.go fixture missing"
        );
        assert!(
            fixtures.join("transitive_impure.go").exists(),
            "transitive_impure.go fixture missing"
        );
    }

    #[tokio::test]
    async fn test_find_function_location_pure_chain() {
        let file = go_fixtures_path().join("pure_chain.go");

        // Read the file to verify functions exist
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("func CalculateTotal"));
        assert!(content.contains("func ComputeSubtotal"));
        assert!(content.contains("func ComputeItemPrice"));
    }

    #[tokio::test]
    async fn test_find_function_location_impure_chain() {
        let file = go_fixtures_path().join("impure_chain.go");

        // Read the file to verify functions exist
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("func ProcessOrder"));
        assert!(content.contains("func SaveToDatabase"));
        assert!(content.contains("func WriteLog"));
    }
}

// =============================================================================
// E2E Tests (require gopls)
// =============================================================================

mod e2e_tests {
    use super::*;

    // =============================================================================
    // Pure Function Chain Tests
    // =============================================================================

    /// Test pure function: CalculateTotal is a pure computation
    #[tokio::test]
    async fn test_pure_function_chain_3_levels() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("pure_chain.go");

        let result = analyzer.analyze_function("CalculateTotal", &file).await;

        match result {
            Ok(analysis) => {
                println!("Pure chain analysis result: {:?}", analysis);
                // CalculateTotal calls only pure functions (math operations, ComputeSubtotal)
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "CalculateTotal should be Unknown (static fallback) or Pure, got: {:?}",
                    analysis
                );
                assert!(!analysis.max_depth_reached, "Should not hit max depth");
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test pure helper function - FormatCurrency
    #[tokio::test]
    async fn test_pure_helper_function() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("pure_chain.go");

        let result = analyzer.analyze_function("FormatCurrency", &file).await;

        match result {
            Ok(analysis) => {
                println!("FormatCurrency analysis: {:?}", analysis);
                // FormatCurrency uses fmt.Sprintf which is pure
                assert!(
                    analysis.level == PurityLevel::Unknown || analysis.level == PurityLevel::Pure,
                    "FormatCurrency should be Unknown or Pure"
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

    /// Test impure function chain: ProcessOrder uses global state
    #[tokio::test]
    async fn test_impure_function_chain_3_levels() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("impure_chain.go");

        let result = analyzer.analyze_function("ProcessOrder", &file).await;

        match result {
            Ok(analysis) => {
                println!("Impure chain analysis result: {:?}", analysis);
                // ProcessOrder modifies global state (orderCount, orderHistory)
                // and calls fmt.Printf which is impure
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "ProcessOrder should be Impure or Unknown, got: {:?}",
                    analysis.level
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test WriteLog function - directly contains fmt.Printf call
    /// fmt.Printf is in ACCEPTABLE_IMPURE_FUNCTIONS (treated as pure)
    /// Similar to Python's treatment of print()
    #[tokio::test]
    async fn test_function_with_print() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("impure_chain.go");

        let result = analyzer.analyze_function("WriteLog", &file).await;

        match result {
            Ok(analysis) => {
                println!("WriteLog analysis: {:?}", analysis);
                // WriteLog calls fmt.Printf which is "acceptable impurity" - treated as pure
                // Similar to Python's treatment of print()
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "WriteLog should be Pure (fmt.Printf is acceptable impurity) or Unknown"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test ExecuteCommand function - directly contains exec.Command call
    /// exec.Command is always impure (system execution)
    #[tokio::test]
    async fn test_direct_impure_function() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("impure_chain.go");

        let result = analyzer.analyze_function("ExecuteCommand", &file).await;

        match result {
            Ok(analysis) => {
                println!("ExecuteCommand analysis: {:?}", analysis);
                // ExecuteCommand directly calls exec.Command - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "ExecuteCommand should be Impure (contains exec.Command)"
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test ReadConfig function - directly contains os.ReadFile call
    /// os.ReadFile is always impure (file I/O)
    #[tokio::test]
    async fn test_file_io_impure() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("impure_chain.go");

        let result = analyzer.analyze_function("ReadConfig", &file).await;

        match result {
            Ok(analysis) => {
                println!("ReadConfig analysis: {:?}", analysis);
                // ReadConfig directly calls os.ReadFile - should be Impure
                assert_eq!(
                    analysis.level,
                    PurityLevel::Impure,
                    "ReadConfig should be Impure (contains os.ReadFile)"
                );
                assert!(
                    analysis
                        .impure_calls
                        .iter()
                        .any(|c| c.function_name.contains("os.")
                            || c.function_name.contains("ReadFile")),
                    "Should detect os.ReadFile as impure call"
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

    /// Test mixed purity chain: AnalyzeData -> ProcessItems -> LogResult -> fmt.Printf
    /// Since fmt.Printf is "acceptable impurity", this chain is treated as Pure
    #[tokio::test]
    async fn test_mixed_purity_chain() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("mixed_chain.go");

        let result = analyzer.analyze_function("AnalyzeData", &file).await;

        match result {
            Ok(analysis) => {
                println!("Mixed chain analysis: {:?}", analysis);
                // AnalyzeData -> ProcessItems -> LogResult -> fmt.Printf
                // Should be Pure because fmt.Printf is acceptable impurity
                // Similar to Python's test_mixed_purity_chain where print is acceptable
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "AnalyzeData should be Pure (fmt.Printf is acceptable) or Unknown, got {:?}",
                    analysis.level
                );
            }
            Err(e) => {
                panic!("Analysis failed: {:?}", e);
            }
        }
    }

    /// Test ComputeStats - pure function using only pure operations (sort, iteration)
    #[tokio::test]
    async fn test_pure_in_mixed_file() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("mixed_chain.go");

        let result = analyzer.analyze_function("ComputeStats", &file).await;

        match result {
            Ok(analysis) => {
                println!("ComputeStats analysis: {:?}", analysis);
                // ComputeStats uses only pure operations
                assert!(
                    analysis.level == PurityLevel::Pure || analysis.level == PurityLevel::Unknown,
                    "ComputeStats should be Pure or Unknown (only uses pure operations)"
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

    /// Test transitive impure chain: EntryPoint -> MiddleLayer -> ReadFile (has os.ReadFile)
    #[tokio::test]
    async fn test_transitive_impure_chain() {
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let config = test_config();
        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("transitive_impure.go");

        let result = analyzer.analyze_function("EntryPoint", &file).await;

        match result {
            Ok(analysis) => {
                println!("EntryPoint analysis: {:?}", analysis);
                // EntryPoint -> MiddleLayer -> ReadFile -> os.ReadFile
                // Should be Impure because transitive call to os.ReadFile
                assert!(
                    analysis.level == PurityLevel::Impure || analysis.level == PurityLevel::Unknown,
                    "EntryPoint should be Impure or Unknown (transitive call to os.ReadFile)"
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
        if !require_tool(ToolDependency::Gopls).await {
            return;
        }

        let mut config = test_config();
        config.max_call_depth = 1; // Very shallow depth

        let analyzer = GoPurityAnalyzer::new(&config).unwrap();
        let file = go_fixtures_path().join("pure_chain.go");

        let result = analyzer.analyze_function("CalculateTotal", &file).await;

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
    use detrix_lsp::GoPurityAnalyzer;

    #[test]
    fn test_known_impure_functions() {
        // Go impure functions (I/O, network, system)
        // Note: fmt.Print* and log.* are in ACCEPTABLE_IMPURE_FUNCTIONS (treated as pure)
        let impure_functions = [
            "os.Exit",
            "os.Open",
            "os.Create",
            "os.ReadFile",
            "os.WriteFile",
            "exec.Command",
            "http.Get",
            "http.Post",
            "net.Dial",
        ];

        for func in impure_functions {
            assert!(
                GoPurityAnalyzer::is_impure_function(func),
                "Expected {} to be classified as impure",
                func
            );
        }
    }

    #[test]
    fn test_acceptable_impure_functions() {
        // These have I/O side effects but don't affect program state
        // They're treated as "acceptable impurity" (effectively pure)
        // Similar to Python's treatment of print()
        let acceptable = [
            "fmt.Print",
            "fmt.Printf",
            "fmt.Println",
            "log.Print",
            "log.Printf",
            "log.Println",
        ];

        for func in acceptable {
            // Acceptable impure functions are treated as pure
            assert!(
                GoPurityAnalyzer::is_pure_function(func),
                "Expected {} to be classified as pure (acceptable impurity)",
                func
            );
            // But they're not in the IMPURE_FUNCTIONS list
            assert!(
                !GoPurityAnalyzer::is_impure_function(func),
                "Expected {} NOT to be in impure list (it's acceptable impurity)",
                func
            );
        }
    }

    #[test]
    fn test_known_pure_functions() {
        // Note: copy, delete, clear are in GO_MUTATION_OPERATIONS (scope-aware)
        // They modify their first argument, so they're not in the pure list
        let pure_functions = [
            "len",
            "cap",
            "make",
            "new",
            "append", // Returns new slice, original unchanged if cap exceeded
            "fmt.Sprintf",
            "fmt.Sprint",
            "fmt.Errorf",
            "strings.ToLower",
            "strings.ToUpper",
            "strings.Contains",
            "strings.Split",
            "strconv.Itoa",
            "strconv.Atoi",
            "math.Sqrt",
            "math.Floor",
            "math.Ceil",
            "sort.Ints",
            "sort.Strings",
        ];

        for func in pure_functions {
            assert!(
                GoPurityAnalyzer::is_pure_function(func),
                "Expected {} to be classified as pure",
                func
            );
        }
    }

    #[test]
    fn test_unknown_functions() {
        // Custom/user-defined functions should be Unknown
        let unknown_functions = ["customFunc", "mypackage.DoSomething", "utils.Process"];

        for func in unknown_functions {
            let is_pure = GoPurityAnalyzer::is_pure_function(func);
            let is_impure = GoPurityAnalyzer::is_impure_function(func);

            assert!(
                !is_pure && !is_impure,
                "Expected {} to be classified as unknown (not pure, not impure)",
                func
            );
        }
    }
}
