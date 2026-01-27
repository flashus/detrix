//! Unified purity analyzer tests
//!
//! This module provides macro-based test generation for consistent
//! cross-language LSP purity analyzer testing. Each language should have
//! the same set of tests with language-specific expressions.
//!
//! # Architecture
//!
//! - `PurityAnalyzerTestData` trait: Defines test expressions for each language
//! - `purity_analyzer_tests!` macro: Generates standard test suite for any analyzer
//! - Language modules: Implement `PurityAnalyzerTestData` for Python, Go
//!
//! # Adding New Language Tests
//!
//! 1. Create a new impl of `PurityAnalyzerTestData` for your language
//! 2. Use `purity_analyzer_tests!` macro to generate tests
//! 3. All languages will have identical test coverage

/// Trait for language-specific purity analyzer test data
///
/// Each language implements this trait to provide equivalent test expressions.
/// This ensures all languages have the same test coverage.
pub trait PurityAnalyzerTestData {
    /// Language identifier (e.g., "python", "go")
    const LANGUAGE: &'static str;

    // =========================================================================
    // Function classification
    // =========================================================================

    /// Known pure functions for this language
    const PURE_FUNCTIONS: &'static [&'static str];

    /// Known impure functions for this language
    const IMPURE_FUNCTIONS: &'static [&'static str];

    /// Functions that are unknown (user-defined)
    const UNKNOWN_FUNCTIONS: &'static [&'static str];

    /// Acceptable impure functions (logging, printing)
    const ACCEPTABLE_IMPURE: &'static [&'static str];

    // =========================================================================
    // Scope-aware mutation detection
    // =========================================================================

    /// Example parameter names
    const PARAM_NAMES: &'static [&'static str] = &["items", "value", "data"];

    /// Example local variable names
    const LOCAL_NAMES: &'static [&'static str] = &["result", "temp", "counter"];

    /// Whether language has global state detection
    const HAS_GLOBAL_DETECTION: bool;

    /// Whether language has nonlocal (closure) state detection
    const HAS_NONLOCAL_DETECTION: bool;

    /// Whether language has receiver (method) state detection (Go-specific)
    const HAS_RECEIVER_DETECTION: bool;

    /// Whether language has package variable detection (Go-specific)
    const HAS_PACKAGE_VAR_DETECTION: bool;

    // =========================================================================
    // Implementation status
    // =========================================================================

    /// Whether this analyzer is fully implemented
    const IS_FULLY_IMPLEMENTED: bool = true;

    /// Whether LSP server is available
    const LSP_SERVER_NAME: &'static str;
}

/// Python purity analyzer test data
pub struct PythonPurityTestData;

impl PurityAnalyzerTestData for PythonPurityTestData {
    const LANGUAGE: &'static str = "python";

    const PURE_FUNCTIONS: &'static [&'static str] = &[
        "len",
        "math.sqrt",
        "json.loads",
        "sorted",
        "min",
        "max",
        "str",
        "int",
        "float",
    ];

    const IMPURE_FUNCTIONS: &'static [&'static str] = &[
        "open",
        "eval",
        "exec",
        "os.system",
        "subprocess.run",
        "os.remove",
    ];

    const UNKNOWN_FUNCTIONS: &'static [&'static str] =
        &["my_custom_func", "calculate_total", "user.process_data"];

    const ACCEPTABLE_IMPURE: &'static [&'static str] =
        &["print", "logging.info", "logging.debug", "logging.error"];

    const HAS_GLOBAL_DETECTION: bool = true;
    const HAS_NONLOCAL_DETECTION: bool = true;
    const HAS_RECEIVER_DETECTION: bool = false; // Python uses self but not like Go
    const HAS_PACKAGE_VAR_DETECTION: bool = false;

    const LSP_SERVER_NAME: &'static str = "pylsp";
}

/// Go purity analyzer test data
pub struct GoPurityTestData;

impl PurityAnalyzerTestData for GoPurityTestData {
    const LANGUAGE: &'static str = "go";

    const PURE_FUNCTIONS: &'static [&'static str] = &[
        "len",
        "cap",
        "append",
        "math.Sqrt",
        "math.Abs",
        "strings.ToLower",
        "strings.Contains",
        "fmt.Sprintf",
    ];

    const IMPURE_FUNCTIONS: &'static [&'static str] = &[
        "os.Exit",
        "os.Open",
        "os.WriteFile",
        "http.Get",
        "net.Dial",
        "exec.Command",
    ];

    const UNKNOWN_FUNCTIONS: &'static [&'static str] =
        &["mypackage.ProcessData", "CustomHandler", "user.getData"];

    const ACCEPTABLE_IMPURE: &'static [&'static str] =
        &["fmt.Println", "fmt.Printf", "log.Print", "log.Printf"];

    const HAS_GLOBAL_DETECTION: bool = false; // Go uses package vars
    const HAS_NONLOCAL_DETECTION: bool = false;
    const HAS_RECEIVER_DETECTION: bool = true;
    const HAS_PACKAGE_VAR_DETECTION: bool = true;

    const LSP_SERVER_NAME: &'static str = "gopls";
}

/// Rust purity analyzer test data
pub struct RustPurityTestData;

impl PurityAnalyzerTestData for RustPurityTestData {
    const LANGUAGE: &'static str = "rust";

    const PURE_FUNCTIONS: &'static [&'static str] = &[
        // Iterator methods
        "len",
        "iter",
        "into_iter",
        "map",
        "filter",
        "fold",
        "reduce",
        "collect",
        "enumerate",
        "zip",
        "take",
        "skip",
        "chain",
        "flatten",
        "flat_map",
        // String methods
        "to_string",
        "to_owned",
        "as_str",
        "as_bytes",
        "chars",
        "trim",
        "split",
        "contains",
        "starts_with",
        "ends_with",
        "to_lowercase",
        "to_uppercase",
        // Option/Result
        "unwrap",
        "unwrap_or",
        "unwrap_or_else",
        "unwrap_or_default",
        "map",
        "and_then",
        "ok_or",
        "is_some",
        "is_none",
        "is_ok",
        "is_err",
        // Clone/Copy
        "clone",
        "to_owned",
        // Math
        "abs",
        "sqrt",
        "pow",
        "sin",
        "cos",
        "min",
        "max",
    ];

    const IMPURE_FUNCTIONS: &'static [&'static str] = &[
        // Process
        "std::process::exit",
        "std::process::abort",
        "std::process::Command::new",
        // Filesystem
        "std::fs::File::open",
        "std::fs::File::create",
        "std::fs::read",
        "std::fs::read_to_string",
        "std::fs::write",
        "std::fs::remove_file",
        "std::fs::create_dir",
        // Network
        "std::net::TcpStream::connect",
        "std::net::TcpListener::bind",
        "std::net::UdpSocket::bind",
        // Environment
        "std::env::var",
        "std::env::set_var",
        "std::env::remove_var",
        "std::env::current_dir",
        // Thread
        "std::thread::spawn",
        "std::thread::sleep",
        // Sync primitives (often impure)
        "Mutex::lock",
        "RwLock::read",
        "RwLock::write",
        // Unsafe
        "transmute",
        "transmute_copy",
    ];

    const UNKNOWN_FUNCTIONS: &'static [&'static str] = &[
        "my_custom_func",
        "process_data",
        "MyStruct::new",
        "calculate_total",
    ];

    const ACCEPTABLE_IMPURE: &'static [&'static str] = &[
        "println",
        "print",
        "eprintln",
        "eprint",
        "dbg",
        "tracing::info",
        "tracing::debug",
        "tracing::warn",
        "tracing::error",
        "log::info",
        "log::debug",
        "log::warn",
        "log::error",
    ];

    const HAS_GLOBAL_DETECTION: bool = true; // static mut detection
    const HAS_NONLOCAL_DETECTION: bool = false;
    const HAS_RECEIVER_DETECTION: bool = true; // &self, &mut self
    const HAS_PACKAGE_VAR_DETECTION: bool = false;

    // Rust purity analyzer is now fully implemented
    const IS_FULLY_IMPLEMENTED: bool = true;

    const LSP_SERVER_NAME: &'static str = "rust-analyzer";
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn assert_no_overlap(left: &[&'static str], right: &[&'static str], label: &str) {
        let left_set: HashSet<&'static str> = left.iter().copied().collect();
        let right_set: HashSet<&'static str> = right.iter().copied().collect();

        let overlap: Vec<&'static str> = left_set.intersection(&right_set).copied().collect();
        assert!(overlap.is_empty(), "{} overlap: {:?}", label, overlap);
    }

    // =========================================================================
    // Test data verification tests
    // =========================================================================

    #[test]
    fn test_no_overlap_pure_and_impure() {
        assert_no_overlap(
            PythonPurityTestData::PURE_FUNCTIONS,
            PythonPurityTestData::IMPURE_FUNCTIONS,
            "python PURE_FUNCTIONS vs IMPURE_FUNCTIONS",
        );
        assert_no_overlap(
            GoPurityTestData::PURE_FUNCTIONS,
            GoPurityTestData::IMPURE_FUNCTIONS,
            "go PURE_FUNCTIONS vs IMPURE_FUNCTIONS",
        );
        assert_no_overlap(
            RustPurityTestData::PURE_FUNCTIONS,
            RustPurityTestData::IMPURE_FUNCTIONS,
            "rust PURE_FUNCTIONS vs IMPURE_FUNCTIONS",
        );
    }

    #[test]
    fn test_no_overlap_acceptable_impure_and_impure() {
        assert_no_overlap(
            PythonPurityTestData::ACCEPTABLE_IMPURE,
            PythonPurityTestData::IMPURE_FUNCTIONS,
            "python ACCEPTABLE_IMPURE vs IMPURE_FUNCTIONS",
        );
        assert_no_overlap(
            GoPurityTestData::ACCEPTABLE_IMPURE,
            GoPurityTestData::IMPURE_FUNCTIONS,
            "go ACCEPTABLE_IMPURE vs IMPURE_FUNCTIONS",
        );
        assert_no_overlap(
            RustPurityTestData::ACCEPTABLE_IMPURE,
            RustPurityTestData::IMPURE_FUNCTIONS,
            "rust ACCEPTABLE_IMPURE vs IMPURE_FUNCTIONS",
        );
    }

    // =========================================================================
    // Cross-language consistency tests
    // =========================================================================

    #[test]
    fn test_all_languages_have_len() {
        // All languages should have 'len' as a pure function
        assert!(PythonPurityTestData::PURE_FUNCTIONS.contains(&"len"));
        assert!(GoPurityTestData::PURE_FUNCTIONS.contains(&"len"));
        assert!(RustPurityTestData::PURE_FUNCTIONS.contains(&"len"));
    }

    #[test]
    fn test_all_languages_have_file_io_impure() {
        // All languages should consider file I/O impure
        assert!(PythonPurityTestData::IMPURE_FUNCTIONS.contains(&"open"));
        assert!(GoPurityTestData::IMPURE_FUNCTIONS.contains(&"os.Open"));
        assert!(RustPurityTestData::IMPURE_FUNCTIONS.contains(&"std::fs::File::open"));
    }

    #[test]
    fn test_all_languages_have_unknown_functions() {
        // All languages should have at least some unknown functions
        assert!(!PythonPurityTestData::UNKNOWN_FUNCTIONS.is_empty());
        assert!(!GoPurityTestData::UNKNOWN_FUNCTIONS.is_empty());
        assert!(!RustPurityTestData::UNKNOWN_FUNCTIONS.is_empty());
    }

    #[test]
    fn test_acceptable_impure_logging() {
        // Logging/printing is acceptable impurity in all languages
        assert!(PythonPurityTestData::ACCEPTABLE_IMPURE
            .iter()
            .any(|f| f.contains("print") || f.contains("logging")));
        assert!(GoPurityTestData::ACCEPTABLE_IMPURE
            .iter()
            .any(|f| f.contains("Print") || f.contains("log")));
    }

    // =========================================================================
    // Scope detection capability tests
    // =========================================================================

    #[test]
    fn test_python_scope_detection_capabilities() {
        // Python has global and nonlocal
        const { assert!(PythonPurityTestData::HAS_GLOBAL_DETECTION) };
        const { assert!(PythonPurityTestData::HAS_NONLOCAL_DETECTION) };
        // But not Go-specific features
        const { assert!(!PythonPurityTestData::HAS_RECEIVER_DETECTION) };
        const { assert!(!PythonPurityTestData::HAS_PACKAGE_VAR_DETECTION) };
    }

    #[test]
    fn test_go_scope_detection_capabilities() {
        // Go has receiver and package vars
        const { assert!(GoPurityTestData::HAS_RECEIVER_DETECTION) };
        const { assert!(GoPurityTestData::HAS_PACKAGE_VAR_DETECTION) };
        // But not Python-specific features
        const { assert!(!GoPurityTestData::HAS_GLOBAL_DETECTION) };
        const { assert!(!GoPurityTestData::HAS_NONLOCAL_DETECTION) };
    }

    // =========================================================================
    // Implementation status tests
    // =========================================================================
}
