# Adding a New Language to Detrix

This document tracks all places in the codebase that need to be updated when adding support for a new programming language/debugger.

**Last Updated:** 10 January 2026 

## Quick Reference: All Code Locations

When adding a new language, you need to update these specific locations:

### Domain Types (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-core/src/entities/language.rs` | `SourceLanguage` enum | Add enum variant (e.g., `JavaScript`) |
| `crates/detrix-core/src/entities/language.rs` | `SourceLanguage::as_str()` | Add string mapping |
| `crates/detrix-core/src/entities/language.rs` | `SourceLanguage::display_name()` | Add display name |
| `crates/detrix-core/src/entities/language.rs` | `SourceLanguage::DAP_SUPPORTED` | Add to supported list |
| `crates/detrix-core/src/entities/language.rs` | `SourceLanguage::from_extension()` | Add file extension mapping |
| `crates/detrix-core/src/entities/language.rs` | `FromStr` impl | Add string parsing |

### DAP Adapter (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-dap/src/{newlang}.rs` | New file | Implement `OutputParser` trait (~100 lines) |
| `crates/detrix-dap/src/lib.rs` | Module exports | `pub mod newlang; pub use newlang::NewLangAdapter;` |
| `crates/detrix-ports/src/adapter.rs` | `DapAdapterFactory` trait | Add `create_newlang_adapter()` method |
| `crates/detrix-dap/src/factory.rs` | `DapAdapterFactoryImpl` | Implement `create_newlang_adapter()` |
| `crates/detrix-application/src/services/adapter_lifecycle_manager.rs` | Language dispatch | Add `SourceLanguage::NewLang => create_newlang_adapter()` |

### Safety Analysis (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-config/src/safety/{newlang}.rs` | New file | Safety config + function classification constants |
| `crates/detrix-config/src/safety/mod.rs` | Re-exports | Add `pub mod newlang; pub use newlang::*;` |
| `crates/detrix-config/src/lib.rs` | SafetyConfig struct | Add `pub newlang: NewLangSafetyConfig` field |
| `crates/detrix-application/src/safety/{newlang}.rs` | New file | Implement `ExpressionValidator` trait |
| `crates/detrix-application/src/safety/mod.rs` | Module exports | `pub mod newlang; pub use newlang::NewLangValidator;` |
| `crates/detrix-application/src/safety/mod.rs` | `ValidatorRegistry::new()` | Register validator |
| `crates/detrix-application/src/safety/treesitter/{newlang}.rs` | New file | Tree-sitter AST analyzer |
| `crates/detrix-application/src/safety/treesitter/mod.rs` | Module exports | `pub use newlang::analyze_newlang;` |
| `crates/detrix-application/src/safety/treesitter/node_kinds.rs` | New module | Add node kind constants for the language |
| `Cargo.toml` (workspace) | Dependencies | Add `tree-sitter-{newlang}` crate |

### LSP Purity Analysis (Optional - for user-defined function analysis)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-lsp/src/{newlang}.rs` | New file | Implement `PurityAnalyzer` trait |
| `crates/detrix-lsp/src/lib.rs` | Module exports | `pub mod newlang;` |
| `crates/detrix-config/src/safety/{newlang}.rs` | `default_{newlang}_lsp_config()` | LSP config with server command |

### API Documentation (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-api/src/mcp/server.rs` | Schema descriptions | Update supported languages list in tool schemas |
| `crates/detrix-api/proto/connections.proto` | Comment | Document new language |

### Testing (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `crates/detrix-dap/tests/{newlang}_tests.rs` | New file | Adapter unit/integration tests |
| `crates/detrix-application/src/safety/unified_tests/{newlang}_unified.rs` | New file | Safety validation tests |
| `fixtures/{newlang}/` | New directory | Example program for testing |

### Documentation (Required)
| File | Section | What to Add |
|------|---------|-------------|
| `docs/ADD_LANGUAGE.md` | Current supported table | Add new language row |
| `CLAUDE.md` | Multi-language section | Update language support status |
| `README.md` | Features section | Update language list |

## Current Supported Languages

| Language | Adapter | Debugger | Connection Mode | Safety | LSP Purity | Status |
|----------|---------|----------|-----------------|--------|------------|--------|
| Python | debugpy | debugpy | attach | Tree-sitter | pyright/pylsp | Stable |
| Go | delve | dlv | attach | Tree-sitter | gopls | Stable |
| Rust | lldb-dap | lldb-dap | direct TCP or wrapper | Tree-sitter | rust-analyzer | Stable |

**Rust Connection Modes:**
- **Direct TCP (preferred):** `RustAdapter::config_direct_launch()` connects to `lldb-dap --connection listen://localhost:<port>`
- **Wrapper (legacy):** `detrix lldb-serve ./binary --listen :4711` when direct TCP is unavailable

### Safety & Purity Analysis Support

| Language | Tree-sitter Parser | Expression Validator | LSP Purity Analyzer |
|----------|-------------------|---------------------|---------------------|
| Python | `tree-sitter-python` | `PythonValidator` | `PythonPurityAnalyzer` |
| Go | `tree-sitter-go` | `GoValidator` | `GoPurityAnalyzer` |
| Rust | `tree-sitter-rust` | `RustValidator` | `RustPurityAnalyzer` |

## Architecture Overview

### Function Classification

Function classification constants are defined in `crates/detrix-config/src/safety/{lang}.rs` and used by both:
- **Tree-sitter validators** (detrix-application) via `config.allowed_set()` / `config.prohibited_set()`
- **LSP purity analyzers** (detrix-lsp) via direct constant imports

```rust
// In crates/detrix-config/src/safety/{lang}.rs

// Constants define built-in function lists
pub const {LANG}_PURE_FUNCTIONS: &[&str] = &[...];
pub const {LANG}_IMPURE_FUNCTIONS: &[&str] = &[...];
pub const {LANG}_ACCEPTABLE_IMPURE: &[&str] = &[...];  // Logging/printing
pub const {LANG}_MUTATION_METHODS: &[&str] = &[...];   // Scope-aware

// Config struct allows user extensions
pub struct {Lang}SafetyConfig {
    pub user_allowed_functions: Vec<String>,    // Extends built-in PURE + ACCEPTABLE
    pub user_prohibited_functions: Vec<String>, // Extends built-in IMPURE
    pub sensitive_patterns: Vec<String>,
    pub lsp: LspConfig,
}

impl {Lang}SafetyConfig {
    /// Returns BUILTIN_PURE + BUILTIN_ACCEPTABLE_IMPURE + user extensions
    pub fn allowed_set(&self) -> HashSet<String> { ... }

    /// Returns BUILTIN_IMPURE + user extensions
    pub fn prohibited_set(&self) -> HashSet<String> { ... }
}
```

### AST Analysis: Tree-sitter (In-Process)

Detrix uses **tree-sitter** for all AST analysis.

**Location:** `crates/detrix-application/src/safety/treesitter/`

```
treesitter/
├── mod.rs              # TreeSitterResult type, node_text(), traverse_tree()
├── node_kinds.rs       # Language-specific node kind constants
├── python.rs           # analyze_python() function
├── go.rs               # analyze_go() function
├── rust_lang.rs        # analyze_rust() function
├── scope.rs            # analyze_scope() for variable discovery
├── cache.rs            # Parser caching for performance
└── comment_stripper.rs # Remove comments before analysis
```

### DAP Adapter Architecture

All adapters use a **generic base adapter** pattern for maximum code reuse (~80% shared code):

```
┌─────────────────────────────────────────────────────────────────────┐
│  BaseAdapter<P: OutputParser>                                       │
│  ├── Connection management (ensure_connected)                       │
│  ├── Metric operations (set_metric, remove_metric)                  │
│  ├── Event subscription (subscribe_events)                          │
│  └── Response parsing (100% shared)                                 │
├─────────────────────────────────────────────────────────────────────┤
│  OutputParser Trait (Language-Specific)                             │
│  ├── parse_output() - Parse debugger output into MetricEvent        │
│  ├── build_logpoint_message() - Build DETRICS:name={expr} format    │
│  ├── needs_continue_after_connect() - Go: true, others: false       │
│  └── language_name() - "python", "go", "rust"                       │
└─────────────────────────────────────────────────────────────────────┘
```

### Standard vs Wrapped Adapters

**Standard Adapters (Python, Go, Rust Direct):** Debug server runs independently, Detrix connects via TCP:

```
┌─────────────┐     DAP/TCP        ┌─────────────┐
│   Detrix    │◄────────────────►│  debugpy /  │
│   Daemon    │                    │  delve /    │
└─────────────┘                    │  lldb-dap   │
                                   └─────────────┘
```

**Rust Direct Connection (New Preferred Way):**
- Start lldb-dap: `lldb-dap --connection listen://localhost:4711`
- Use `RustAdapter::config_direct_launch("/path/to/binary", 4711)`
- Type formatters are injected via `initCommands` in the launch request

**Wrapped Adapters (Rust/LLDB Fallback):** For environments where lldb-dap doesn't support `--connection`:

```
┌─────────────┐     DAP/TCP        ┌─────────────────┐     stdio        ┌──────────┐
│   Detrix    │◄────────────────►│ detrix          │◄──────────────►│ lldb-dap │
│   Daemon    │                    │ lldb-serve      │                  │          │
└─────────────┘                    └─────────────────┘                  └──────────┘
```

## Checklist for Adding a New Language

### 1. Add SourceLanguage Variant

**Location:** `crates/detrix-core/src/entities/language.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLanguage {
    Python,
    Go,
    Rust,
    JavaScript,  // Add new variant
    // ...
}

impl SourceLanguage {
    pub fn as_str(&self) -> &'static str {
        match self {
            // ...
            Self::JavaScript => "javascript",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            // ...
            Self::JavaScript => "JavaScript",
        }
    }

    // Add to DAP_SUPPORTED if adapter exists
    pub const DAP_SUPPORTED: &'static [SourceLanguage] = &[
        Self::Python, Self::Go, Self::Rust, Self::JavaScript
    ];

    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            // ...
            "js" | "jsx" => Self::JavaScript,
            _ => Self::Unknown,
        }
    }
}

impl std::str::FromStr for SourceLanguage {
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            // ...
            "javascript" | "js" => Ok(Self::JavaScript),
            _ => Err(()),
        }
    }
}
```

### 2. Implement OutputParser Trait (~100 lines)

**Location:** `crates/detrix-dap/src/{language}.rs`

```rust
// crates/detrix-dap/src/javascript.rs

pub struct JavaScriptOutputParser;

#[async_trait]
impl OutputParser for JavaScriptOutputParser {
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent> {
        // Parse DETRICS:metric_name=value from output
        // See python.rs or go.rs for reference
    }

    fn language_name() -> &'static str {
        "javascript"
    }

    // Override if debugger starts paused
    fn needs_continue_after_connect() -> bool {
        false
    }
}

// Type alias for the adapter
pub type JavaScriptAdapter = BaseAdapter<JavaScriptOutputParser>;

impl JavaScriptAdapter {
    pub fn new(config: AdapterConfig, base_path: PathBuf) -> Self {
        BaseAdapter::new(config, base_path)
    }

    pub fn default_config(port: u16) -> AdapterConfig {
        AdapterConfig {
            debugger_host: "127.0.0.1".to_string(),
            debugger_port: port,
            connection_mode: ConnectionMode::Attach { pid: None },
            timeout_ms: 10_000,
        }
    }
}
```

All logpoint formats use: `DETRICS:metric_name={expression}`

### 3. Create Safety Config with Function Classification

**Location:** `crates/detrix-config/src/safety/{language}.rs`

```rust
// crates/detrix-config/src/safety/javascript.rs

use super::LspConfig;
use crate::constants::{DEFAULT_ANALYSIS_TIMEOUT_MS, DEFAULT_MAX_CALL_DEPTH};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ============================================================================
// JavaScript Safety Config
// ============================================================================

/// JavaScript-specific safety configuration
///
/// Built-in function lists come from the constants below.
/// Users can extend these with additional functions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JavaScriptSafetyConfig {
    /// User's additional allowed functions (extend built-in PURE + ACCEPTABLE_IMPURE)
    #[serde(default)]
    pub user_allowed_functions: Vec<String>,

    /// User's additional prohibited functions (extend built-in IMPURE)
    #[serde(default)]
    pub user_prohibited_functions: Vec<String>,

    /// Sensitive variable patterns (substring patterns to block)
    #[serde(default = "default_js_sensitive_patterns")]
    pub sensitive_patterns: Vec<String>,

    /// LSP-based purity analysis settings
    #[serde(default = "default_js_lsp_config")]
    pub lsp: LspConfig,
}

impl Default for JavaScriptSafetyConfig {
    fn default() -> Self {
        Self {
            user_allowed_functions: Vec::new(),
            user_prohibited_functions: Vec::new(),
            sensitive_patterns: default_js_sensitive_patterns(),
            lsp: default_js_lsp_config(),
        }
    }
}

impl JavaScriptSafetyConfig {
    /// Get effective allowed functions: BUILTIN_PURE + BUILTIN_ACCEPTABLE_IMPURE + user extensions
    pub fn allowed_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = JS_PURE_FUNCTIONS
            .iter()
            .chain(JS_ACCEPTABLE_IMPURE.iter())
            .map(|s| (*s).to_string())
            .collect();
        set.extend(self.user_allowed_functions.iter().cloned());
        set
    }

    /// Get effective prohibited functions: BUILTIN_IMPURE + user extensions
    pub fn prohibited_set(&self) -> HashSet<String> {
        let mut set: HashSet<String> = JS_IMPURE_FUNCTIONS
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

fn default_js_lsp_config() -> LspConfig {
    LspConfig {
        enabled: false,
        command: "typescript-language-server".to_string(),
        args: vec!["--stdio".to_string()],
        max_call_depth: DEFAULT_MAX_CALL_DEPTH,
        analysis_timeout_ms: DEFAULT_ANALYSIS_TIMEOUT_MS,
        cache_enabled: true,
        working_directory: None,
    }
}

fn default_js_sensitive_patterns() -> Vec<String> {
    vec![
        "password", "secret", "apiKey", "api_key", "token",
        "privateKey", "private_key", "credential",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

// ============================================================================
// JavaScript Function Classification Constants (Single Source of Truth)
// ============================================================================

/// "Acceptable" impure functions - I/O but safe for metrics (logging)
pub const JS_ACCEPTABLE_IMPURE: &[&str] = &[
    "console.log",
    "console.debug",
    "console.info",
    "console.warn",
    "console.error",
];

/// Pure functions (no side effects)
pub const JS_PURE_FUNCTIONS: &[&str] = &[
    // Array methods (non-mutating)
    "Array.isArray",
    "Array.from",
    "Array.of",
    "concat",
    "filter",
    "find",
    "findIndex",
    "flat",
    "flatMap",
    "includes",
    "indexOf",
    "join",
    "lastIndexOf",
    "map",
    "reduce",
    "reduceRight",
    "slice",
    "some",
    "every",
    // String methods
    "charAt",
    "charCodeAt",
    "concat",
    "includes",
    "indexOf",
    "lastIndexOf",
    "match",
    "replace",
    "search",
    "slice",
    "split",
    "substring",
    "toLowerCase",
    "toUpperCase",
    "trim",
    "trimStart",
    "trimEnd",
    // Object methods
    "Object.keys",
    "Object.values",
    "Object.entries",
    "Object.assign",
    "Object.freeze",
    // Math
    "Math.abs",
    "Math.floor",
    "Math.ceil",
    "Math.round",
    "Math.sqrt",
    "Math.pow",
    "Math.min",
    "Math.max",
    // JSON
    "JSON.stringify",
    "JSON.parse",
    // Type checking
    "typeof",
    "instanceof",
    "Number.isNaN",
    "Number.isFinite",
    "isNaN",
    "isFinite",
];

/// Impure functions (side effects)
pub const JS_IMPURE_FUNCTIONS: &[&str] = &[
    // Code execution
    "eval",
    "Function",
    // Module system
    "require",
    "import",
    // File system (Node.js)
    "fs.readFile",
    "fs.writeFile",
    "fs.readFileSync",
    "fs.writeFileSync",
    "fs.unlink",
    "fs.mkdir",
    // Process (Node.js)
    "process.exit",
    "child_process.exec",
    "child_process.spawn",
    // Network
    "fetch",
    "XMLHttpRequest",
    // Timers
    "setTimeout",
    "setInterval",
    // DOM (browser)
    "document.write",
    "window.open",
];

/// Mutation methods (scope-aware)
pub const JS_MUTATION_METHODS: &[&str] = &[
    // Array mutations
    "push",
    "pop",
    "shift",
    "unshift",
    "splice",
    "sort",
    "reverse",
    "fill",
    // Object mutations
    "delete",
];
```

**Update re-exports in `crates/detrix-config/src/safety/mod.rs`:**

```rust
pub mod javascript;
pub use javascript::*;
```

**Update SafetyConfig in `crates/detrix-config/src/lib.rs`:**

```rust
pub struct SafetyConfig {
    pub python: PythonSafetyConfig,
    pub go: GoSafetyConfig,
    pub rust: RustSafetyConfig,
    pub javascript: JavaScriptSafetyConfig,  // NEW
}
```

### 4. Add Tree-sitter Analyzer

**Location:** `crates/detrix-application/src/safety/treesitter/{language}.rs`

```rust
// crates/detrix-application/src/safety/treesitter/javascript.rs

use super::{node_text, traverse_tree, TreeSitterResult};
use super::node_kinds::javascript as nodes;
use detrix_core::SafetyLevel;
use std::collections::HashSet;

pub fn analyze_javascript(
    expression: &str,
    _safety_level: SafetyLevel,
    allowed_functions: &HashSet<String>,
) -> TreeSitterResult {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .expect("JavaScript grammar is valid");

    let source = expression.as_bytes();
    let Some(tree) = parser.parse(source, None) else {
        return TreeSitterResult::unsafe_with_violations(vec![
            "Failed to parse JavaScript expression".to_string()
        ]);
    };

    let mut result = TreeSitterResult::safe();
    traverse_tree(tree.root_node(), source, &mut |node, src| {
        analyze_javascript_node(node, src, allowed_functions, &mut result);
    });

    result
}

fn analyze_javascript_node(
    node: tree_sitter::Node,
    source: &[u8],
    allowed_functions: &HashSet<String>,
    result: &mut TreeSitterResult,
) {
    match node.kind() {
        nodes::ASSIGNMENT_EXPRESSION => {
            result.add_violation(format!(
                "Assignment detected: '{}'",
                node_text(&node, source)
            ));
        }
        nodes::CALL_EXPRESSION => {
            if let Some(func_node) = node.child_by_field_name("function") {
                let func_name = node_text(&func_node, source);
                result.add_function_call(func_name.clone());

                // Check if function is allowed
                if !allowed_functions.contains(&func_name) {
                    // Will be validated by the validator against prohibited list
                }
            }
        }
        // ... more patterns
        _ => {}
    }
}
```

**Add node kinds constants in `crates/detrix-application/src/safety/treesitter/node_kinds.rs`:**

```rust
pub mod javascript {
    pub const ASSIGNMENT_EXPRESSION: &str = "assignment_expression";
    pub const CALL_EXPRESSION: &str = "call_expression";
    pub const IDENTIFIER: &str = "identifier";
    pub const MEMBER_EXPRESSION: &str = "member_expression";
    pub const NEW_EXPRESSION: &str = "new_expression";
    pub const UPDATE_EXPRESSION: &str = "update_expression";
    pub const AWAIT_EXPRESSION: &str = "await_expression";
    // ... more node kinds
}
```

**Add tree-sitter crate to workspace:**

**Location:** `Cargo.toml` (workspace root)

```toml
[workspace.dependencies]
tree-sitter-javascript = "0.23"
```

**Location:** `crates/detrix-application/Cargo.toml`

```toml
[dependencies]
tree-sitter-javascript = { workspace = true }
```

### 5. Implement Expression Validator

**Location:** `crates/detrix-application/src/safety/{language}.rs`

```rust
// crates/detrix-application/src/safety/javascript.rs

use super::base_validator::BaseValidator;
use super::treesitter::analyze_javascript;
use super::validation_result::ValidationResult;
use super::ExpressionValidator;
use crate::error::Result;
use detrix_config::JavaScriptSafetyConfig;
use detrix_core::{PurityLevel, SafetyLevel, SourceLanguage};
use std::collections::HashSet;

/// JavaScript expression validator
#[derive(Debug, Clone)]
pub struct JavaScriptValidator {
    allowed_functions: HashSet<String>,
    prohibited_functions: HashSet<String>,
    sensitive_patterns: Vec<String>,
}

impl JavaScriptValidator {
    pub fn new(config: &JavaScriptSafetyConfig) -> Self {
        Self {
            allowed_functions: config.allowed_set(),
            prohibited_functions: config.prohibited_set(),
            sensitive_patterns: config.sensitive_patterns().clone(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(&JavaScriptSafetyConfig::default())
    }
}

impl BaseValidator for JavaScriptValidator {
    fn allowed_functions(&self) -> &HashSet<String> {
        &self.allowed_functions
    }

    fn prohibited_functions(&self) -> &HashSet<String> {
        &self.prohibited_functions
    }

    fn sensitive_patterns(&self) -> &[String] {
        &self.sensitive_patterns
    }
}

impl ExpressionValidator for JavaScriptValidator {
    fn language(&self) -> SourceLanguage {
        SourceLanguage::JavaScript
    }

    fn validate(&self, expression: &str, safety_level: SafetyLevel) -> Result<ValidationResult> {
        let ast_result = analyze_javascript(expression, safety_level, &self.allowed_functions);
        Ok(self.validate_ast_result(ast_result, safety_level, PurityLevel::Pure))
    }

    fn is_function_allowed(&self, func_name: &str, level: SafetyLevel) -> bool {
        self.is_function_allowed_base(func_name, level)
    }
}
```

**Register in ValidatorRegistry:**

**Location:** `crates/detrix-application/src/safety/mod.rs`

```rust
mod javascript;
pub use javascript::JavaScriptValidator;

impl ValidatorRegistry {
    pub fn new(config: &SafetyConfig) -> Self {
        let mut validators: HashMap<SourceLanguage, Arc<dyn ExpressionValidator>> = HashMap::new();

        // ... existing validators ...

        // Register JavaScript validator
        let js_validator = JavaScriptValidator::new(&config.javascript);
        validators.insert(SourceLanguage::JavaScript, Arc::new(js_validator));

        Self { validators }
    }
}
```

### 6. Update Adapter Factory

**Location:** `crates/detrix-ports/src/adapter.rs`

```rust
#[async_trait]
pub trait DapAdapterFactory: Send + Sync {
    // ... existing methods ...
    async fn create_javascript_adapter(&self, host: &str, port: u16) -> Result<DapAdapterRef>;
}
```

**Location:** `crates/detrix-dap/src/factory.rs`

```rust
impl DapAdapterFactory for DapAdapterFactoryImpl {
    async fn create_javascript_adapter(&self, _host: &str, port: u16) -> Result<DapAdapterRef> {
        let config = JavaScriptAdapter::default_config(port);
        let adapter = JavaScriptAdapter::new(config, self.base_path.clone());
        Ok(Arc::new(adapter))
    }
}
```

### 7. Update Adapter Lifecycle Manager

**Location:** `crates/detrix-application/src/services/adapter_lifecycle_manager.rs`

```rust
let adapter = match language {
    SourceLanguage::Python => self.adapter_factory.create_python_adapter(host, port).await?,
    SourceLanguage::Go => self.adapter_factory.create_go_adapter(host, port).await?,
    SourceLanguage::Rust => self.adapter_factory.create_rust_adapter(host, port).await?,
    SourceLanguage::JavaScript => self.adapter_factory.create_javascript_adapter(host, port).await?,
    _ => {
        return Err(detrix_core::Error::InvalidConfig(
            SourceLanguage::language_error(language.as_str()),
        ));
    }
};
```

### 8. Create Wrapper Command (if needed)

If the debugger doesn't support network listening (like lldb-dap), create a wrapper:

**Location:** `crates/detrix-cli/src/commands/{lang}_serve.rs`

- Implement TCP listener that proxies to stdio
- Inject launch/attach requests
- Add type formatters for readable variable display
- Register in `commands/mod.rs` and `main.rs`

### 9. Add Tests

**DAP adapter tests:**
```rust
// crates/detrix-dap/tests/javascript_tests.rs

#[tokio::test]
async fn test_javascript_adapter_connection() {
    // Test connection to JavaScript debugger
}

#[tokio::test]
async fn test_javascript_logpoint_parsing() {
    // Test DETRICS: output parsing
}
```

**Safety validation tests:**
```rust
// crates/detrix-application/src/safety/unified_tests/javascript_unified.rs

#[test]
fn test_javascript_safe_expressions() {
    let validator = JavaScriptValidator::with_defaults();
    let result = validator.validate("user.name", SafetyLevel::Strict).unwrap();
    assert!(result.is_safe);
}

#[test]
fn test_javascript_unsafe_eval() {
    let validator = JavaScriptValidator::with_defaults();
    let result = validator.validate("eval('code')", SafetyLevel::Strict).unwrap();
    assert!(!result.is_safe);
}
```

### 10. Add LSP Purity Analyzer (Optional)

**Location:** `crates/detrix-lsp/src/{language}.rs`

```rust
use detrix_config::safety::javascript::{
    JS_PURE_FUNCTIONS, JS_IMPURE_FUNCTIONS, JS_ACCEPTABLE_IMPURE, JS_MUTATION_METHODS
};

pub struct JavaScriptPurityAnalyzer {
    config: LspConfig,
    manager: Arc<LspManager>,
    cache: Arc<RwLock<HashMap<String, PurityAnalysis>>>,
}

#[async_trait]
impl PurityAnalyzer for JavaScriptPurityAnalyzer {
    async fn analyze_function(&self, function_name: &str, file_path: &Path) -> Result<PurityAnalysis> {
        // Use typescript-language-server for call hierarchy analysis
    }

    fn is_available(&self) -> bool {
        // Check if tsserver is installed
    }

    fn language(&self) -> &str { "javascript" }

    fn is_pure_function(name: &str) -> bool {
        JS_PURE_FUNCTIONS.contains(&name) || JS_ACCEPTABLE_IMPURE.contains(&name)
    }

    fn is_impure_function(name: &str) -> bool {
        JS_IMPURE_FUNCTIONS.contains(&name)
    }
}
```

### 11. Update Documentation

- [ ] Update this file with new language row
- [ ] Update `CLAUDE.md` multi-language section
- [ ] Update `README.md` features section
- [ ] Add fixture example in `fixtures/{language}/`

## Files Modified Reference

### When Adding Go Support (Reference):

1. `crates/detrix-core/src/entities/language.rs` - Added Go variant
2. `crates/detrix-dap/src/go.rs` - Go adapter implementation
3. `crates/detrix-dap/src/lib.rs` - Export module
4. `crates/detrix-ports/src/adapter.rs` - Factory trait method
5. `crates/detrix-dap/src/factory.rs` - Factory implementation
6. `crates/detrix-application/src/services/adapter_lifecycle_manager.rs` - Language dispatch
7. `crates/detrix-config/src/safety/go.rs` - GoSafetyConfig + function constants
8. `crates/detrix-application/src/safety/go.rs` - GoValidator
9. `crates/detrix-application/src/safety/treesitter/go.rs` - Tree-sitter analyzer
10. `crates/detrix-application/src/safety/treesitter/node_kinds.rs` - Go node kinds
11. `crates/detrix-application/src/safety/mod.rs` - Register validator
12. `crates/detrix-lsp/src/go.rs` - GoPurityAnalyzer
13. `crates/detrix-api/src/mcp/server.rs` - Update schemas
14. `fixtures/go/` - Example Go program

### When Adding Rust Support (Reference):

Same pattern as Go, plus:
- `crates/detrix-cli/src/commands/lldb_serve.rs` - Wrapper command (lldb-dap doesn't support network listening)
- Type formatters for LLDB to display Rust types readably

## Known Issues and Fixes

### DAP setBreakpoints Race Condition (Fixed)

**Issue:** DAP `setBreakpoints` replaces ALL breakpoints in a file, not adds to them.

**Fix:** `set_metric()` in `base.rs` stores metric first, then collects ALL breakpoints for the file before sending a single `setBreakpoints` with all breakpoints.

### MCP Connection Selection (Fixed)

**Issue:** When multiple connections existed, `add_metric` might use disconnected adapter.

**Fix:** Filter for connected adapters, sort by most recently started, use most recent.

## Code Statistics

### DAP Adapters:

| Component | Notes |
|-----------|-------|
| `BaseAdapter<P>` | Shared DAP logic |
| `PythonOutputParser` | Python-specific parsing |
| `GoOutputParser` | Go-specific parsing |
| `RustOutputParser` | Rust-specific parsing |


### Safety Analysis (Tree-sitter based):

| Component |  Notes |
|-----------|-------|
| `treesitter/mod.rs` | Core types and helpers |
| `treesitter/python.rs` | Python AST analysis |
| `treesitter/go.rs` | Go AST analysis |
| `treesitter/rust_lang.rs` | Rust AST analysis |
| `treesitter/node_kinds.rs` | Node kind constants |
| `treesitter/cache.rs` | Parser caching |
| `treesitter/comment_stripper.rs` | Comment removal |
| `PythonValidator` | Python validation |
| `GoValidator` | Go validation |
| `RustValidator` | Rust validation |

### LSP Purity Analyzers:

| Component | Notes |
|-----------|-------|
| `PythonPurityAnalyzer` | Python call hierarchy |
| `GoPurityAnalyzer` | Go call hierarchy |
| `RustPurityAnalyzer` | Rust call hierarchy |
