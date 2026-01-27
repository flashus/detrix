//! Language-specific test data for unified safety validator tests
//!
//! Each language provides equivalent expressions for the same test categories.
//! This ensures all languages have identical test coverage.

use super::LanguageTestData;
use detrix_core::SourceLanguage;

/// Python test data
pub struct PythonTestData;

impl LanguageTestData for PythonTestData {
    const LANGUAGE: SourceLanguage = SourceLanguage::Python;

    // Basic validation
    const SAFE_ATTRIBUTE: &'static str = "user.id";
    const SAFE_FUNCTION: &'static str = "len(items)";
    const SAFE_COMPLEX: &'static str = "sum([len(x) for x in items])";

    // Prohibited expressions
    const PROHIBITED_EVAL: Option<&'static str> = Some("eval('1+1')");
    const PROHIBITED_SYSCALL: &'static str = "os.system('ls')";
    const PROHIBITED_FILE_IO: &'static str = "open('/etc/passwd', 'r')";
    const PROHIBITED_NETWORK: Option<&'static str> = None; // Not in default blacklist
    const PROHIBITED_EXEC: &'static str = "subprocess.run(['ls'])";

    // Function classification
    const WHITELISTED_FUNC: &'static str = "len";
    const BLACKLISTED_FUNC: &'static str = "eval";
    const METHOD_CALL: &'static str = "user.get_name()";
    const NESTED_CALLS: &'static str = "str(int(float(x)))";

    // Sensitive variable tests
    const SENSITIVE_PASSWORD: &'static str = "user.password";
    const SENSITIVE_SECRET: &'static str = "secret_key";
    const SENSITIVE_API_KEY: &'static str = "api_key";
    const SENSITIVE_TOKEN: &'static str = "session.access_token";
    const SENSITIVE_CREDENTIAL: &'static str = "db_credentials";

    // Purity test expressions
    const PURE_EXPRESSION: &'static str = "len(str(x))";
    const IMPURE_EXPRESSION: &'static str = "eval('1+1')";
    const UNKNOWN_PURITY_STRICT: &'static str = "user.get_data()";
    const UNKNOWN_PURITY_TRUSTED: &'static str = "custom_process(x)";
    const ATTRIBUTE_ONLY: &'static str = "user.name";
    const DICT_METHOD: &'static str = "data.items()";

    // Feature flags
    const HAS_EVAL: bool = true;
    const HAS_SENSITIVE_VARS: bool = true;
}

/// Go test data
pub struct GoTestData;

impl LanguageTestData for GoTestData {
    const LANGUAGE: SourceLanguage = SourceLanguage::Go;

    // Basic validation
    const SAFE_ATTRIBUTE: &'static str = "user.ID";
    const SAFE_FUNCTION: &'static str = "len(items)";
    const SAFE_COMPLEX: &'static str = "fmt.Sprintf(\"%d\", len(items))";

    // Prohibited expressions
    const PROHIBITED_EVAL: Option<&'static str> = None; // Go doesn't have eval
    const PROHIBITED_SYSCALL: &'static str = "os.Exit(1)";
    const PROHIBITED_FILE_IO: &'static str = "os.Open(\"/etc/passwd\")";
    const PROHIBITED_NETWORK: Option<&'static str> = Some("http.Get(\"http://example.com\")");
    const PROHIBITED_EXEC: &'static str = "exec.Command(\"ls\")";

    // Function classification
    const WHITELISTED_FUNC: &'static str = "len";
    const BLACKLISTED_FUNC: &'static str = "exec.Command";
    const METHOD_CALL: &'static str = "user.GetName()";
    const NESTED_CALLS: &'static str = "strconv.Itoa(len(items))";

    // Sensitive variable tests (Go uses camelCase)
    const SENSITIVE_PASSWORD: &'static str = "user.Password";
    const SENSITIVE_SECRET: &'static str = "secretKey";
    const SENSITIVE_API_KEY: &'static str = "apiKey";
    const SENSITIVE_TOKEN: &'static str = "session.AccessToken";
    const SENSITIVE_CREDENTIAL: &'static str = "dbCredentials";

    // Purity test expressions
    const PURE_EXPRESSION: &'static str = "fmt.Sprintf(\"%d\", len(items))";
    const IMPURE_EXPRESSION: &'static str = "os.Exit(1)";
    const UNKNOWN_PURITY_STRICT: &'static str = "user.GetData()";
    const UNKNOWN_PURITY_TRUSTED: &'static str = "customProcess(x)";
    const ATTRIBUTE_ONLY: &'static str = "user.Name";
    const DICT_METHOD: &'static str = "len(m)"; // Go uses len() for map access

    // Feature flags
    const HAS_EVAL: bool = false;
    const HAS_SENSITIVE_VARS: bool = true;
}

/// Rust test data
pub struct RustTestData;

impl LanguageTestData for RustTestData {
    const LANGUAGE: SourceLanguage = SourceLanguage::Rust;

    // Basic validation (Rust syntax)
    const SAFE_ATTRIBUTE: &'static str = "user.id";
    const SAFE_FUNCTION: &'static str = "items.len()";
    const SAFE_COMPLEX: &'static str = "items.iter().map(|x| x.len()).sum::<usize>()";

    // Prohibited expressions (Rust equivalents)
    const PROHIBITED_EVAL: Option<&'static str> = None; // Rust doesn't have eval
    const PROHIBITED_SYSCALL: &'static str = "std::process::exit(1)";
    const PROHIBITED_FILE_IO: &'static str = "std::fs::File::open(\"/etc/passwd\")";
    const PROHIBITED_NETWORK: Option<&'static str> = None;
    const PROHIBITED_EXEC: &'static str = "std::process::Command::new(\"ls\")";

    // Function classification
    const WHITELISTED_FUNC: &'static str = "len";
    const BLACKLISTED_FUNC: &'static str = "std::process::exit";
    const METHOD_CALL: &'static str = "user.get_name()";
    const NESTED_CALLS: &'static str = "items.len().to_string()";

    // Sensitive variable tests (Rust uses snake_case)
    const SENSITIVE_PASSWORD: &'static str = "user.password";
    const SENSITIVE_SECRET: &'static str = "secret_key";
    const SENSITIVE_API_KEY: &'static str = "api_key";
    const SENSITIVE_TOKEN: &'static str = "session.access_token";
    const SENSITIVE_CREDENTIAL: &'static str = "db_credentials";

    // Purity test expressions
    const PURE_EXPRESSION: &'static str = "items.len()";
    const IMPURE_EXPRESSION: &'static str = "std::process::exit(1)";
    const UNKNOWN_PURITY_STRICT: &'static str = "user.get_data()";
    const UNKNOWN_PURITY_TRUSTED: &'static str = "custom_process(x)";
    const ATTRIBUTE_ONLY: &'static str = "user.name";
    const DICT_METHOD: &'static str = "map.len()";

    // Feature flags
    const HAS_EVAL: bool = false;
    const HAS_SENSITIVE_VARS: bool = true;

    const HAS_SAFETY_VALIDATOR: bool = true;
    const HAS_LSP_PURITY: bool = true;
    const HAS_SCOPE_ANALYSIS: bool = true;
    const HAS_DAP_ADAPTER: bool = true; // Via CodeLLDB
    const HAS_INTROSPECTION: bool = true; // Basic introspection works
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_test_data() {
        assert_eq!(PythonTestData::LANGUAGE, SourceLanguage::Python);
        assert!(PythonTestData::HAS_EVAL);
        assert!(PythonTestData::PROHIBITED_EVAL.is_some());
    }

    #[test]
    fn test_go_test_data() {
        assert_eq!(GoTestData::LANGUAGE, SourceLanguage::Go);
        assert!(!GoTestData::HAS_EVAL);
        assert!(GoTestData::PROHIBITED_EVAL.is_none());
    }

    #[test]
    fn test_rust_test_data() {
        assert_eq!(RustTestData::LANGUAGE, SourceLanguage::Rust);
        assert!(!RustTestData::HAS_EVAL);
    }

    // =========================================================================
    // Implementation status tests
    // =========================================================================

    #[test]
    fn test_python_has_full_implementation() {
        // Python has full implementation
        assert!(PythonTestData::HAS_SAFETY_VALIDATOR);
        assert!(PythonTestData::HAS_LSP_PURITY);
        assert!(PythonTestData::HAS_SCOPE_ANALYSIS);
        assert!(PythonTestData::HAS_DAP_ADAPTER);
        assert!(PythonTestData::HAS_INTROSPECTION);
    }

    #[test]
    fn test_go_has_full_implementation() {
        // Go has full implementation
        assert!(GoTestData::HAS_SAFETY_VALIDATOR);
        assert!(GoTestData::HAS_LSP_PURITY);
        assert!(GoTestData::HAS_SCOPE_ANALYSIS);
        assert!(GoTestData::HAS_DAP_ADAPTER);
        assert!(GoTestData::HAS_INTROSPECTION);
    }

    #[test]
    fn test_rust_has_full_implementation() {
        // Rust has full implementation
        assert!(RustTestData::HAS_SAFETY_VALIDATOR);
        assert!(RustTestData::HAS_LSP_PURITY);
        assert!(RustTestData::HAS_SCOPE_ANALYSIS);
        assert!(RustTestData::HAS_DAP_ADAPTER);
        assert!(RustTestData::HAS_INTROSPECTION);
    }

    /// Helper to check if a language supports safety tests
    fn supports_safety_tests<T: LanguageTestData>() -> bool {
        T::HAS_SAFETY_VALIDATOR
    }

    /// Helper to check if a language supports LSP purity tests
    fn supports_lsp_tests<T: LanguageTestData>() -> bool {
        T::HAS_LSP_PURITY
    }

    /// Helper to check if a language supports DAP tests
    fn supports_dap_tests<T: LanguageTestData>() -> bool {
        T::HAS_DAP_ADAPTER
    }

    #[test]
    fn test_capability_helpers() {
        // Python and Go support all tests
        assert!(supports_safety_tests::<PythonTestData>());
        assert!(supports_safety_tests::<GoTestData>());
        assert!(supports_safety_tests::<RustTestData>());

        assert!(supports_lsp_tests::<PythonTestData>());
        assert!(supports_lsp_tests::<GoTestData>());
        assert!(supports_lsp_tests::<RustTestData>());

        // All support DAP
        assert!(supports_dap_tests::<PythonTestData>());
        assert!(supports_dap_tests::<GoTestData>());
        assert!(supports_dap_tests::<RustTestData>());
    }
}
