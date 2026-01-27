//! Rust-specific safety configuration and function classification constants

use super::LspConfig;
use crate::constants::{DEFAULT_ANALYSIS_TIMEOUT_MS, DEFAULT_MAX_CALL_DEPTH};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============================================================================
// Rust Safety Config
// ============================================================================

/// Rust-specific safety configuration
///
/// Built-in function lists come from the `RUST_PURE_FUNCTIONS`, `RUST_IMPURE_FUNCTIONS`,
/// and `RUST_ACCEPTABLE_IMPURE` constants. Users can extend these with additional functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustSafetyConfig {
    /// User's additional allowed functions (extend built-in PURE + ACCEPTABLE_IMPURE)
    #[serde(default)]
    pub user_allowed_functions: Vec<String>,

    /// User's additional prohibited functions (extend built-in IMPURE)
    #[serde(default)]
    pub user_prohibited_functions: Vec<String>,

    /// Sensitive variable patterns (substring patterns to block)
    #[serde(default = "default_rust_sensitive_patterns")]
    pub sensitive_patterns: Vec<String>,

    /// LSP-based purity analysis settings for Rust
    #[serde(default = "default_rust_lsp_config")]
    pub lsp: LspConfig,
}

impl Default for RustSafetyConfig {
    fn default() -> Self {
        Self {
            user_allowed_functions: Vec::new(),
            user_prohibited_functions: Vec::new(),
            sensitive_patterns: default_rust_sensitive_patterns(),
            lsp: default_rust_lsp_config(),
        }
    }
}

impl RustSafetyConfig {
    /// Get effective allowed functions: BUILTIN_PURE + BUILTIN_ACCEPTABLE_IMPURE + user extensions
    pub fn allowed_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = RUST_PURE_FUNCTIONS
            .iter()
            .chain(RUST_ACCEPTABLE_IMPURE.iter())
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_allowed_functions.iter().cloned());
        set
    }

    /// Get effective prohibited functions: BUILTIN_IMPURE + user extensions
    pub fn prohibited_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = RUST_IMPURE_FUNCTIONS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_prohibited_functions.iter().cloned());
        set
    }

    /// Get sensitive variable patterns
    pub fn sensitive_patterns(&self) -> &Vec<String> {
        &self.sensitive_patterns
    }
}

fn default_rust_lsp_config() -> LspConfig {
    LspConfig {
        enabled: false,
        command: "rust-analyzer".to_string(),
        args: Vec::new(),
        max_call_depth: DEFAULT_MAX_CALL_DEPTH,
        analysis_timeout_ms: DEFAULT_ANALYSIS_TIMEOUT_MS,
        cache_enabled: true,
        working_directory: None,
    }
}

fn default_rust_sensitive_patterns() -> Vec<String> {
    vec![
        // Credentials (snake_case - Rust convention)
        "password",
        "passwd",
        "secret",
        "api_key",
        "apikey",
        "auth_token",
        "access_token",
        "refresh_token",
        "bearer_token",
        "jwt",
        "token",
        // Database
        "db_password",
        "database_password",
        "connection_string",
        "conn_string",
        // Security
        "private_key",
        "ssh_key",
        "encryption_key",
        "signing_key",
        "credential",
        "credentials",
        // Personal data (GDPR/PII)
        "ssn",
        "social_security",
        "credit_card",
        "card_number",
        "cvv",
        "pin",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ============================================================================
// Rust Function Classification Constants
// ============================================================================
//
// These constants are the canonical source for Rust function classification,
// used by both tree-sitter validation (detrix-application) and LSP purity
// analysis (detrix-lsp). Import from here instead of duplicating.

/// "Acceptable" impure functions - I/O side effects but considered safe for metrics
/// These are kept separate from IMPURE so they can be treated as pure
/// for purity analysis purposes (logging/printing doesn't affect program state)
pub const RUST_ACCEPTABLE_IMPURE: &[&str] = &[
    // Standard library printing macros
    "println",
    "print",
    "eprintln",
    "eprint",
    "dbg",
    // Tracing/logging crate
    "tracing::info",
    "tracing::debug",
    "tracing::warn",
    "tracing::error",
    "tracing::trace",
    "tracing::event",
    "log::info",
    "log::debug",
    "log::warn",
    "log::error",
    "log::trace",
    // env_logger
    "env_logger::init",
    "env_logger::try_init",
];

/// Known pure functions (no side effects, no state modification)
/// These are safe to call from any context.
pub const RUST_PURE_FUNCTIONS: &[&str] = &[
    // Core type constructors
    "String::new",
    "String::from",
    "String::with_capacity",
    "Vec::new",
    "Vec::with_capacity",
    "Box::new",
    "Rc::new",
    "Arc::new",
    "Option::Some",
    "Option::None",
    "Result::Ok",
    "Result::Err",
    // Collection constructors
    "HashMap::new",
    "HashMap::with_capacity",
    "HashSet::new",
    "HashSet::with_capacity",
    "BTreeMap::new",
    "BTreeSet::new",
    "VecDeque::new",
    "VecDeque::with_capacity",
    "LinkedList::new",
    "BinaryHeap::new",
    "BinaryHeap::with_capacity",
    // Smart pointer constructors
    "Cow::Borrowed",
    "Cow::Owned",
    "Cell::new",
    "RefCell::new",
    "Mutex::new",
    "RwLock::new",
    "Once::new",
    // Iterator methods (pure, create new iterators)
    "iter",
    "into_iter",
    "iter_mut",
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
    "cycle",
    "cloned",
    "copied",
    "peekable",
    "fuse",
    "inspect",
    "by_ref",
    "sum",
    "product",
    "max",
    "min",
    "max_by",
    "min_by",
    "max_by_key",
    "min_by_key",
    "count",
    "last",
    "nth",
    "find",
    "find_map",
    "position",
    "rposition",
    "all",
    "any",
    "partition",
    "unzip",
    // String methods (non-mutating)
    "len",
    "is_empty",
    "as_str",
    "as_bytes",
    "chars",
    "bytes",
    "lines",
    "split",
    "rsplit",
    "split_whitespace",
    "split_ascii_whitespace",
    "trim",
    "trim_start",
    "trim_end",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "contains",
    "starts_with",
    "ends_with",
    "rfind",
    "replace",
    "replacen",
    "repeat",
    "parse",
    // Formatting
    "format",
    "format_args",
    "to_string",
    "Display::fmt",
    "Debug::fmt",
    // Option/Result methods (non-mutating)
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "as_ref",
    "as_mut",
    "as_deref",
    "as_deref_mut",
    "ok",
    "err",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "map_or",
    "map_or_else",
    "map_err",
    "and",
    "and_then",
    "or",
    "or_else",
    "transpose",
    // Clone and Copy
    "clone",
    // Default
    "default",
    "Default::default",
    // Hash
    "hash",
    "Hash::hash",
    // Comparison
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "Ord::cmp",
    "Ord::max",
    "Ord::min",
    "Ord::clamp",
    "PartialOrd::partial_cmp",
    "PartialOrd::lt",
    "PartialOrd::le",
    "PartialOrd::gt",
    "PartialOrd::ge",
    // Math operations
    "abs",
    "signum",
    "is_positive",
    "is_negative",
    "checked_add",
    "checked_sub",
    "checked_mul",
    "checked_div",
    "checked_rem",
    "saturating_add",
    "saturating_sub",
    "saturating_mul",
    "wrapping_add",
    "wrapping_sub",
    "wrapping_mul",
    "overflowing_add",
    "overflowing_sub",
    "overflowing_mul",
    "pow",
    "sqrt",
    "cbrt",
    "log",
    "log2",
    "log10",
    "ln",
    "exp",
    "exp2",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sinh",
    "cosh",
    "tanh",
    "asinh",
    "acosh",
    "atanh",
    "floor",
    "ceil",
    "round",
    "trunc",
    "fract",
    "is_nan",
    "is_infinite",
    "is_finite",
    "is_normal",
    "is_sign_positive",
    "is_sign_negative",
    // Type introspection
    "type_name",
    "type_id",
    "size_of",
    "align_of",
    "size_of_val",
    "align_of_val",
    "mem::size_of",
    "mem::align_of",
    "mem::size_of_val",
    "mem::align_of_val",
    "mem::discriminant",
    // Time (read-only)
    "Instant::now",
    "Instant::elapsed",
    "Instant::duration_since",
    "Instant::saturating_duration_since",
    "SystemTime::now",
    "SystemTime::elapsed",
    "SystemTime::duration_since",
    "Duration::new",
    "Duration::from_secs",
    "Duration::from_millis",
    "Duration::from_micros",
    "Duration::from_nanos",
    "Duration::as_secs",
    "Duration::as_millis",
    "Duration::as_micros",
    "Duration::as_nanos",
    "Duration::is_zero",
    // Path operations (non-mutating)
    "Path::new",
    "Path::as_os_str",
    "Path::to_str",
    "Path::to_string_lossy",
    "Path::is_absolute",
    "Path::is_relative",
    "Path::has_root",
    "Path::parent",
    "Path::file_name",
    "Path::file_stem",
    "Path::extension",
    "Path::join",
    "Path::with_file_name",
    "Path::with_extension",
    "Path::components",
    "Path::display",
    "PathBuf::new",
    "PathBuf::from",
    "PathBuf::as_path",
    // JSON (parsing only - no I/O)
    "serde_json::to_string",
    "serde_json::to_string_pretty",
    "serde_json::to_vec",
    "serde_json::to_vec_pretty",
    "serde_json::from_str",
    "serde_json::from_slice",
    "serde_json::from_value",
    "serde_json::to_value",
    // Error creation (pure)
    "Error::new",
    "anyhow::anyhow",
    "anyhow::bail",
    "anyhow::ensure",
    "thiserror::Error",
];

/// Known impure functions (side effects that matter)
/// These MUST trigger impurity flags.
pub const RUST_IMPURE_FUNCTIONS: &[&str] = &[
    // Process operations
    "std::process::exit",
    "std::process::abort",
    "std::process::Command::new",
    "std::process::Command::spawn",
    "std::process::Command::output",
    "std::process::Command::status",
    // File I/O
    "std::fs::read",
    "std::fs::read_to_string",
    "std::fs::write",
    "std::fs::create_dir",
    "std::fs::create_dir_all",
    "std::fs::remove_file",
    "std::fs::remove_dir",
    "std::fs::remove_dir_all",
    "std::fs::rename",
    "std::fs::copy",
    "std::fs::hard_link",
    "std::fs::soft_link",
    "std::fs::set_permissions",
    "std::fs::File::create",
    "std::fs::File::open",
    "std::fs::OpenOptions::open",
    "tokio::fs::read",
    "tokio::fs::read_to_string",
    "tokio::fs::write",
    "tokio::fs::create_dir",
    "tokio::fs::create_dir_all",
    "tokio::fs::remove_file",
    "tokio::fs::remove_dir",
    "tokio::fs::remove_dir_all",
    "tokio::fs::rename",
    "tokio::fs::copy",
    "tokio::fs::File::create",
    "tokio::fs::File::open",
    // Network I/O
    "std::net::TcpStream::connect",
    "std::net::TcpListener::bind",
    "std::net::UdpSocket::bind",
    "tokio::net::TcpStream::connect",
    "tokio::net::TcpListener::bind",
    "tokio::net::UdpSocket::bind",
    // HTTP clients
    "reqwest::get",
    "reqwest::Client::get",
    "reqwest::Client::post",
    "reqwest::Client::put",
    "reqwest::Client::delete",
    "reqwest::Client::patch",
    "hyper::Client::get",
    // Database
    "sqlx::query",
    "sqlx::query_as",
    "sqlx::Pool::connect",
    "diesel::Connection::establish",
    "rusqlite::Connection::open",
    // Unsafe operations
    "std::ptr::read",
    "std::ptr::write",
    "std::ptr::read_volatile",
    "std::ptr::write_volatile",
    "std::ptr::copy",
    "std::ptr::copy_nonoverlapping",
    "std::ptr::swap",
    "std::ptr::swap_nonoverlapping",
    "std::ptr::drop_in_place",
    "std::mem::transmute",
    "std::mem::transmute_copy",
    "std::mem::forget",
    "std::mem::zeroed",
    "std::mem::uninitialized",
    "std::mem::MaybeUninit::uninit",
    "std::mem::MaybeUninit::assume_init",
    // OS/System operations
    "std::env::set_var",
    "std::env::remove_var",
    "std::os::unix::process::CommandExt::exec",
    // Thread spawning (side effects)
    "std::thread::spawn",
    "std::thread::Builder::spawn",
    "tokio::spawn",
    "tokio::task::spawn",
    "tokio::task::spawn_blocking",
    // Panic/Abort macros
    "panic!",
    "unreachable!",
    "unimplemented!",
    "todo!",
    // FFI
    "std::ffi::CString::from_raw",
    "std::ffi::CStr::from_ptr",
    // Intrinsics
    "std::intrinsics::abort",
    "std::intrinsics::breakpoint",
];

/// LLDB type formatter commands for Rust types.
///
/// These commands configure LLDB to display Rust types in a readable format.
/// Based on the Rust compiler's LLDB formatters (rust-lang/rust/src/etc/lldb_commands)
/// but simplified to not require Python scripts.
///
/// Used by both:
/// - `lldb-serve` proxy command (injected into launch request)
/// - Direct lldb-dap connection via `initCommands` in launch request
///
/// Note: We don't use "settings set target.language rust" because:
/// 1. It's not supported on all LLDB versions (especially Windows lldb-dap)
/// 2. It's not strictly necessary for type formatters to work
pub const RUST_LLDB_TYPE_FORMATTERS: &[&str] = &[
    // String type: show the actual string content
    // alloc::string::String stores data in vec.buf.inner.ptr.pointer.pointer
    r#"type summary add -x "^alloc::([a-z_]+::)*String$" --summary-string "${var.vec.buf.inner.ptr.pointer.pointer%s}""#,
    // &str type: show the string content
    r#"type summary add -x "^&str$" --summary-string "${var.data_ptr%s}""#,
    // Vec<T>: show length and capacity
    r#"type summary add -x "^alloc::([a-z_]+::)*Vec<.+>$" --summary-string "len=${var.len}, cap=${var.buf.inner.cap}""#,
    // Option<T>: show variant
    r#"type summary add -x "^core::option::Option<.+>$" --summary-string "${var}""#,
    // Result<T, E>: show variant
    r#"type summary add -x "^core::result::Result<.+>$" --summary-string "${var}""#,
    // Box<T>: dereference the pointer
    r#"type summary add -x "^alloc::([a-z_]+::)*Box<.+>$" --summary-string "${*var.inner.pointer.pointer}""#,
    // Rc<T>: show the inner value
    r#"type summary add -x "^alloc::([a-z_]+::)*Rc<.+>$" --summary-string "${*var.inner.pointer.pointer}""#,
    // Arc<T>: show the inner value
    r#"type summary add -x "^alloc::([a-z_]+::)*Arc<.+>$" --summary-string "${*var.inner.pointer.pointer}""#,
];

/// Mutation methods - impure ONLY when called on non-local targets
/// When called on local variables (owned), these are PURE.
/// When called on &mut self, &mut parameters, or statics, these are IMPURE.
pub const RUST_MUTATION_METHODS: &[&str] = &[
    // Clone trait mutation
    "clone_from", // fn clone_from(&mut self, source: &Self)
    // Vec mutations
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
    "truncate",
    "resize",
    "extend",
    "append",
    "drain",
    "retain",
    "dedup",
    "sort",
    "sort_by",
    "sort_by_key",
    "reverse",
    // HashMap/HashSet mutations
    "entry",
    "get_mut",
    // String mutations
    "push_str",
    "insert_str",
    "replace_range",
    // I/O mutations
    "write",
    "write_all",
    "write_fmt",
    "flush",
    // Sync primitives
    "lock",
    "try_lock",
    "read",
    "try_read",
    "try_write",
];
