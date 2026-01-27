//! Source language types and parsing utilities

use serde::{Deserialize, Serialize};
use std::fmt;

/// Programming language for metrics and connections
///
/// This is a domain concept used by Metric and Connection entities.
/// It represents the language of the target application being instrumented.
///
/// NOTE: `Unknown` is intentionally NOT the default. Use explicit language specification
/// or handle the Result from `parse()`. The `Unknown` variant is used by `from_extension()`
/// for unrecognized file types and may be rejected at connection/metric creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLanguage {
    Python,
    Go,
    Rust,
    JavaScript,
    TypeScript,
    Java,
    Cpp,
    C,
    Ruby,
    Php,
    /// Unrecognized language - returned by `from_extension()` for unknown file types.
    /// This variant will be REJECTED by `Connection::new()` and similar constructors.
    Unknown,
}

impl SourceLanguage {
    /// Get the lowercase string representation for API/config usage
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Java => "java",
            Self::Cpp => "cpp",
            Self::C => "c",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Unknown => "unknown",
        }
    }

    /// Get display name for the language
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::Go => "Go",
            Self::Rust => "Rust",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Java => "Java",
            Self::Cpp => "C++",
            Self::C => "C",
            Self::Ruby => "Ruby",
            Self::Php => "PHP",
            Self::Unknown => "Unknown",
        }
    }

    /// Languages currently supported by DAP adapters.
    /// Update this when adding new language adapters.
    pub const DAP_SUPPORTED: &'static [SourceLanguage] = &[Self::Python, Self::Go, Self::Rust];

    /// Get comma-separated list of DAP-supported language names.
    /// Use this for error messages to keep them in sync with actual support.
    pub fn dap_supported_list() -> String {
        Self::DAP_SUPPORTED
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Check if this language has DAP adapter support
    pub fn is_dap_supported(&self) -> bool {
        Self::DAP_SUPPORTED.contains(self)
    }

    /// Error message for missing or unsupported language.
    /// Pass empty string for "language required" case.
    pub fn language_error(lang: &str) -> String {
        if lang.is_empty() {
            format!(
                "language is required. Supported: {}",
                Self::dap_supported_list()
            )
        } else {
            format!(
                "Unsupported language '{}'. Supported: {}",
                lang,
                Self::dap_supported_list()
            )
        }
    }

    /// Determine language from file extension
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "py" => Self::Python,
            "go" => Self::Go,
            "rs" => Self::Rust,
            "js" | "jsx" => Self::JavaScript,
            "ts" | "tsx" => Self::TypeScript,
            "java" => Self::Java,
            "cpp" | "cc" | "cxx" => Self::Cpp,
            "c" => Self::C,
            "rb" => Self::Ruby,
            "php" => Self::Php,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for SourceLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for SourceLanguage {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "python" => Ok(Self::Python),
            "go" | "golang" => Ok(Self::Go),
            "rust" => Ok(Self::Rust),
            "javascript" | "js" => Ok(Self::JavaScript),
            "typescript" | "ts" => Ok(Self::TypeScript),
            "java" => Ok(Self::Java),
            "cpp" | "c++" => Ok(Self::Cpp),
            "c" => Ok(Self::C),
            "ruby" => Ok(Self::Ruby),
            "php" => Ok(Self::Php),
            "unknown" => Ok(Self::Unknown),
            _ => Err(()),
        }
    }
}

/// Error type for SourceLanguage parsing failures
#[derive(Debug, Clone)]
pub struct ParseLanguageError {
    pub invalid_value: String,
    pub context: Option<String>,
}

impl ParseLanguageError {
    pub fn new(invalid_value: impl Into<String>) -> Self {
        Self {
            invalid_value: invalid_value.into(),
            context: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

impl std::fmt::Display for ParseLanguageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ctx) = &self.context {
            write!(
                f,
                "Invalid language '{}' {}. Valid options: python, go, rust, javascript, typescript, java, cpp, c, ruby, php",
                self.invalid_value, ctx
            )
        } else {
            write!(
                f,
                "Invalid language '{}'. Valid options: python, go, rust, javascript, typescript, java, cpp, c, ruby, php",
                self.invalid_value
            )
        }
    }
}

impl std::error::Error for ParseLanguageError {}

impl From<ParseLanguageError> for String {
    fn from(err: ParseLanguageError) -> Self {
        err.to_string()
    }
}

/// Extension trait for adding context to language parsing results
pub trait ParseLanguageResultExt<T> {
    /// Add context to a language parsing error
    fn with_language_context(
        self,
        context: impl Into<String>,
    ) -> std::result::Result<T, ParseLanguageError>;
}

impl<T> ParseLanguageResultExt<T> for std::result::Result<T, ParseLanguageError> {
    fn with_language_context(
        self,
        context: impl Into<String>,
    ) -> std::result::Result<T, ParseLanguageError> {
        self.map_err(|e| e.with_context(context))
    }
}

impl TryFrom<&str> for SourceLanguage {
    type Error = ParseLanguageError;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        s.parse().map_err(|_| ParseLanguageError::new(s))
    }
}

impl TryFrom<String> for SourceLanguage {
    type Error = ParseLanguageError;

    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        s.as_str().parse().map_err(|_| ParseLanguageError::new(s))
    }
}

// NOTE: We intentionally do NOT implement From<&str> or From<String>.
// These would silently convert parse errors to Unknown, hiding bugs.
// Instead, use the extension trait methods below.
//
// Example:
//   let lang = "python".parse_language()?;           // Returns Result
//   let lang = SourceLanguage::from_extension("py"); // Returns Unknown for unrecognized extensions

/// Extension trait for parsing language strings ergonomically
pub trait ParseLanguageExt {
    /// Parse to SourceLanguage with proper error type.
    /// Returns ParseLanguageError for invalid/unknown language strings.
    fn parse_language(&self) -> std::result::Result<SourceLanguage, ParseLanguageError>;

    /// Lossy parse for display-only contexts. Returns Unknown on failure.
    /// WARNING: Do NOT use for validation - use parse_language() instead.
    fn parse_language_lossy(&self) -> SourceLanguage;
}

impl ParseLanguageExt for str {
    fn parse_language(&self) -> std::result::Result<SourceLanguage, ParseLanguageError> {
        self.try_into()
    }

    fn parse_language_lossy(&self) -> SourceLanguage {
        self.parse().unwrap_or(SourceLanguage::Unknown)
    }
}

impl ParseLanguageExt for String {
    fn parse_language(&self) -> std::result::Result<SourceLanguage, ParseLanguageError> {
        self.as_str().try_into()
    }

    fn parse_language_lossy(&self) -> SourceLanguage {
        self.as_str().parse().unwrap_or(SourceLanguage::Unknown)
    }
}
