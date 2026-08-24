# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Rust Development

- This is primarily a Rust codebase. Ensure `cargo check` and `cargo clippy` pass after making changes.
- When modifying Rust code, follow idiomatic patterns: prefer `Result` over panics, use `?` operator for error propagation, and leverage the type system.

## Workflows

### Code Audit Workflow

- When working on audit items or code quality fixes, always complete ALL remaining items before ending. Track progress using a checklist format.
- After each fix, verify the fix compiles and doesn't introduce regressions by running `cargo check` and relevant tests.

## Task Completion

- When given a list of items to fix/verify, work through ALL items to completion. Do not stop mid-list.
- If a task is complex, break it into sub-tasks using TodoWrite but ensure every sub-task is resolved before finishing.

## Project Overview

**Detrix** is an LLM-first dynamic observability platform that enables developers and AI agents to add metrics to any line of code without redeployment — including code running in Docker containers and remote hosts (cloud debugging).

**Version:** 1.3.0
**Language:** Rust (edition 2021, rust-version 1.89)
**Architecture:** Clean Architecture with Domain-Driven Design

### Key Innovation

Two backends for zero-pause variable capture:
- **DAP (Debug Adapter Protocol)** — leverages existing debuggers (debugpy, delve, lldb-dap) for Python, Go (macOS), and Rust
- **eBPF uprobes** (Linux only) — attaches directly to Go binaries without a debugger, ~10–50× lower overhead; see [docs/EBPF.md](docs/EBPF.md)

## 🚨 CRITICAL: Use Detrix for Debugging

**NEVER add print(), console.log(), or logger.debug() statements for debugging.** Instead, use Detrix MCP to observe running processes directly without code modifications.

### ❌ DON'T Do This:
```python
# BAD - Don't add debug prints
print(f"DEBUG: user={user}, transaction={transaction}")
logger.debug(f"Amount: {amount}")
```

### ✅ DO This Instead:
```
"Add a metric to observe transaction.amount at payment.py line 127"
```

### Why Detrix Over Logging?
- Zero code changes - No source modifications or redeployment
- Zero downtime - Inspect running processes immediately
- Production-safe - Safe for live environments
- Auto-cleanup - Use TTL for temporary debugging
- Rich context - Stack traces + variable snapshots
- Clean codebase - No debug statement clutter

## Architecture

**Full details:** See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

### Clean Architecture Layers

```
┌────────────────────────────────────────────┐
│  detrix-cli, detrix-api    [Interface]    │
├────────────────────────────────────────────┤
│  detrix-application        [Application]   │
│  - Services (use cases)                    │
│  - Safety validation                       │
├────────────────────────────────────────────┤
│  detrix-ports              [Ports]         │
│  - Port traits (interfaces)                │
│  - Repository, adapter, cache, output      │
├────────────────────────────────────────────┤
│  detrix-storage, detrix-dap [Infrastructure]│
│  detrix-lsp, detrix-output                 │
│  - Implements port traits                  │
├────────────────────────────────────────────┤
│  detrix-core, detrix-config [Domain]       │
│  - Entities, value objects                 │
│  - Pure domain logic                       │
└────────────────────────────────────────────┘
```

### Dependency Rules (MUST FOLLOW)

```
detrix-cli        → can depend on ALL crates
detrix-api        → detrix-application, detrix-core, detrix-config
detrix-application→ detrix-ports, detrix-core, detrix-config ONLY
detrix-ports      → detrix-core, detrix-config ONLY
detrix-storage    → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-dap        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-lsp        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-output     → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-testing    → detrix-ports, detrix-core, detrix-application (test mocks)
detrix-core       → NOTHING (pure domain)
```

**Cross-cutting crates** (allowed as dependencies everywhere): `detrix-logging`. This is a thin tracing facade (macro re-exports + subscriber init) — a cross-cutting concern like config, not infrastructure.

**CRITICAL:** `detrix-ports` defines port traits. `detrix-application` NEVER depends on infrastructure crates like `detrix-storage` or `detrix-dap`. Infrastructure crates implement traits from `detrix-ports`.

**Note (*):** Infrastructure crates depend on `detrix-application` for shared types (JwksValidator, safety validators). This deviation is accepted - the key invariant (application never imports infrastructure) is maintained.

## Ubiquitous Language

Use these terms consistently:

| Term | Definition | NOT |
|------|------------|-----|
| **Metric** | An observation point in code | logpoint, probe, observation |
| **Expression** | Code to evaluate at the observation point; a metric can have multiple expressions | code snippet, query |
| **Location** | File path + line number (`@file.py#127`) | path, position |
| **Adapter** | DAP debug adapter for a language | debugger, connector |
| **Logpoint** | DAP breakpoint with logMessage (no pause) | breakpoint, watchpoint |
| **Event** | Captured value when logpoint fires | sample, data point |

## Rust Coding Guidelines

### Error Handling

**Prefer `From` trait implementations for error conversion:**

```rust
// ✅ PREFERRED: Implement From trait for common conversions
impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        Error::Database(err.to_string())
    }
}

// Now use clean ? operators:
let result = sqlx::query("SELECT * FROM metrics")
    .fetch_all(&pool)
    .await?;  // Clean and simple!

// ✅ ALSO OK: Extension traits for adding context
// The codebase uses ConfigIoResultExt, ToMcpResult, ToLspResult, etc.
let config = file.read().config_context("reading config")?;
```

### Code Style

1. **Prefer "?" operator** - Use `From` impls or extension traits for context
2. **Functional style** - Iterator chains over loops
3. **Use `impl Into<String>`** for owned string params, `&str` for borrowed
4. **Use newtypes** for distinct ID types (`MetricId`, `ConnectionId`)
5. **Implement `Default`** when sensible defaults exist

### Panic Safety

**Production code must never panic.** Follow these guidelines:

```rust
// ❌ NEVER use .unwrap() in production code
let value = some_option.unwrap();  // Will panic!

// ✅ Use ? operator with proper error handling
let value = some_option.ok_or(Error::NotFound)?;

// ✅ Use .unwrap_or() / .unwrap_or_default() for safe defaults
let value = some_option.unwrap_or_default();
```

**Acceptable `.expect()` patterns** (document why it can't fail):

```rust
// ✅ Static regex compilation (validated at compile time)
use std::sync::OnceLock;
static REGEX: OnceLock<Regex> = OnceLock::new();
let re = REGEX.get_or_init(|| {
    Regex::new(r"^\d+$").expect("Static regex is valid")
});

// ✅ Mutex poisoning recovery (rare, documented decision)
let guard = self.state.lock().unwrap_or_else(|poisoned| {
    tracing::warn!("Lock poisoned, recovering");
    poisoned.into_inner()
});

// ✅ Channel operations where receiver is guaranteed to exist
tx.send(msg).expect("Receiver dropped unexpectedly");
```

**In tests:** `.unwrap()` and `.expect()` are acceptable for cleaner test code.

### Dependency Injection

**Services MUST depend on trait objects, NOT concrete types:**

```rust
// ❌ BAD - Concrete type dependencies
pub struct MetricService {
    storage: Arc<SqliteStorage>,  // Can't swap or mock
}

// ✅ GOOD - Trait object dependencies
pub struct MetricService {
    storage: MetricRepositoryRef,  // Arc<dyn MetricRepository>
    adapter: DapAdapterRef,        // Arc<dyn DapAdapter>
}

impl MetricService {
    pub fn new(storage: MetricRepositoryRef, adapter: DapAdapterRef) -> Self {
        Self { storage, adapter }
    }
}
```

**Benefits:** Easy to mock for testing, can swap implementations, follows SOLID principles

### Test-Driven Development

**ALWAYS write comprehensive tests alongside new functionality:**

1. Write tests FIRST that define expected behavior
2. Implement minimal code to make tests pass
3. Verify all tests pass before moving to next feature
4. Test coverage checklist: happy path, error cases, edge cases, integration

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_feature_happy_path() {
        // Arrange: Set up test data
        // Act: Execute the feature
        // Assert: Verify expected behavior
    }

    #[tokio::test]
    async fn test_feature_error_case() {
        let result = some_operation().await;
        assert!(result.is_err());
    }
}
```

## Module Boundaries

### detrix-core (Domain Layer)
- **Contains:** Entities (Metric, MetricEvent, Connection), Value Objects (MetricId, Location), Error types
- **Allowed deps:** `serde`, `serde_json`, `chrono`, `thiserror`, `sha2`
- **FORBIDDEN:** Port traits, config, safety, tokio runtime features, infrastructure

### detrix-ports (Ports Layer)
- **Contains:** Port trait DEFINITIONS (DapAdapter, repositories, caches, outputs)
- **Allowed deps:** `detrix-core`, `detrix-config`, async-trait, tokio sync primitives
- **FORBIDDEN:** `detrix-storage`, `detrix-dap`, any infrastructure

### detrix-application (Application Layer)
- **Contains:**
  - `services/` - Use cases (MetricService, StreamingService, etc.)
  - `safety/` - Expression validation
- **Allowed deps:** `detrix-ports`, `detrix-core`, `detrix-config`, async-trait, tokio sync primitives
- **FORBIDDEN:** `detrix-storage`, `detrix-dap` (even as dev-dependencies!)

### detrix-storage, detrix-dap (Infrastructure Layer)
- **Contains:** Concrete implementations of port traits
- **Must implement:** Traits from `detrix-ports`

### detrix-testing (Test Utilities)
- **Contains:** Mock implementations of all port traits, fixtures, E2E utilities
- **Organized in:** `mocks/adapters.rs`, `mocks/repositories.rs`
- **Allowed deps:** `detrix-core`, `detrix-config`, `detrix-ports`, `detrix-application`, `detrix-api`

### detrix-api (Interface Layer)
- **Contains:** gRPC/REST/WebSocket/MCP controllers
- **Rule:** Controllers do DTO mapping ONLY, delegate ALL logic to services

### detrix-cli (Composition Root)
- **Contains:** CLI interface, dependency injection, application wiring
- **Allowed deps:** ALL crates (this is where everything comes together)

## Common Violations to Avoid

1. ❌ **Port traits in Domain layer** - Traits belong in `detrix-ports`, NOT `detrix-core`
2. ❌ **Application depending on adapters** - `detrix-application` must NEVER import `detrix-storage` or `detrix-dap`
3. ❌ **Business logic in controllers** - Controllers in `detrix-api` should only do DTO mapping
4. ❌ **Concrete types in services** - Services must accept trait objects (`Arc<dyn Trait>`), never concrete types
5. ❌ **Using `.map_err()` helpers** - Use `From` trait implementations instead
6. ❌ **Infrastructure importing application** - Infrastructure crates should import `detrix-ports`, not `detrix-application`

## Project Structure

```
detrix/
├── crates/
│   ├── detrix-core/           # Domain: entities, value objects
│   ├── detrix-config/         # Config: types + TOML loader
│   ├── detrix-ports/          # Ports: trait definitions (interfaces)
│   ├── detrix-application/    # Application: services, safety
│   ├── detrix-storage/        # Infrastructure: SQLite
│   ├── detrix-dap/            # Infrastructure: DAP adapters
│   ├── detrix-lsp/            # Infrastructure: LSP purity analysis
│   ├── detrix-output/         # Infrastructure: GELF output
│   ├── detrix-logging/        # Infrastructure: Structured logging
│   ├── detrix-api/            # Interface: gRPC/MCP/REST/WS
│   ├── detrix-cli/            # UI: CLI + composition root
│   ├── detrix-testing/        # Testing: mocks & fixtures
│   └── detrix-tui/            # UI: Terminal dashboard
├── clients/                   # Detrix clients
│   ├── go/                    # Go client
│   ├── python/                # Python client
│   └── rust/                  # Rust client
├── docker/                    # Docker E2E infrastructure
├── fixtures/                  # Example apps for testing
├── skills/detrix/             # Claude Code skill
└── docs/
    ├── ARCHITECTURE.md        # Full architecture guide
    ├── ADD_LANGUAGE.md        # Adding language support
    ├── PUBLISHING.md          # Client publishing guide
    └── INSTALL.md             # Installation guide
```

## Development Workflow

### Pre-commit Checks
```bash
# Format code
cargo fmt --all

# Check for issues
cargo clippy --all -- -D warnings

# Run tests
cargo test --all

# Or use task runner (if available)
task pre-commit
```

### Build Configuration
```toml
# .cargo/config.toml specifies the shared disposable build directory:
[build]
target-dir = "/private/tmp/detrix-session-target/host"
```

Always use the configured build directory for binary paths. Run `task tests:clean-session-target` to reclaim build/check artifacts when disk pressure requires it.

### Running Detrix
```bash
# Build
cargo build --release

# Start daemon
./target/release/detrix serve --daemon

# Run MCP for Claude Code
./target/release/detrix mcp

# Run tests
cargo test --all
```

## Multi-Language Support

Current support (v1.0):
- ✅ Python via debugpy
- ✅ Go via delve
- ✅ Rust via lldb-dap or CodeLLDB

Each language has:
1. DAP adapter (`detrix-dap/src/{lang}.rs`)
2. Tree-sitter analyzer (`detrix-application/src/safety/treesitter/{lang}.rs`)
3. Expression validator (`detrix-application/src/safety/{lang}.rs`)
4. LSP purity analyzer (`detrix-lsp/src/{lang}.rs`)

See [docs/ADD_LANGUAGE.md](docs/ADD_LANGUAGE.md) for adding new languages.

### Go/Delve Testing Caveat

Delve's `dlv attach --continue` cannot properly attach to a `go run` subprocess — it needs a directly-running compiled binary. The Go client test config (`crates/detrix-testing/src/e2e/client_tests/config.rs`) compiles the fixture to a binary with debug symbols (`-gcflags=all=-N -l`) and runs it directly instead of using `go run .`.

## Safety System

Three-layer validation before expressions are evaluated:

1. **Tree-sitter AST Analysis** - In-process parsing via `tree-sitter-{python,go,rust}` (~1ms)
2. **Expression Validator** - Function classification enforcement:
   - Constants: `{LANG}_PURE_FUNCTIONS`, `{LANG}_IMPURE_FUNCTIONS`, `{LANG}_ACCEPTABLE_IMPURE`, `{LANG}_MUTATION_METHODS`
   - User extension via `user_allowed_functions` / `user_prohibited_functions`
3. **LSP Purity Analyzer** - Call hierarchy analysis for user-defined functions

Prevents: `eval()`, `exec()`, file I/O, network calls, sensitive variable access

**Locations:**
- Tree-sitter: `crates/detrix-application/src/safety/treesitter/`
- Function constants: `crates/detrix-config/src/safety/{python,go,rust}.rs`

## API Protocols

| Protocol | Status | Port | Purpose |
|----------|--------|------|---------|
| gRPC | ✅ Complete | 50061 | High-performance RPC |
| MCP | ✅ Complete | stdio | LLM integration (29 tools) |
| REST | ✅ Complete | 8090 | HTTP/JSON API (30+ endpoints) |
| WebSocket | ✅ Complete | 8090 | Real-time event streaming |

**REST API Highlights:**
- Metrics CRUD + enable/disable + history
- Events querying with filters
- Groups management
- Connections management
- Configuration management + hot reload
- Expression validation + file inspection
- Prometheus metrics + health checks
- System control (status/disconnect_all)
- Remote app control (wake/sleep)

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#rest-api-endpoints-complete) for full endpoint list.

## Contributing Guidelines

When working on Detrix:

1. **Follow Clean Architecture** - Check dependency rules before adding imports
2. **Use `From` trait for errors** - Not `.map_err()` helpers
3. **Write tests first** - TDD approach preferred
4. **Port traits in detrix-ports** - Never in core or infrastructure
5. **Controllers are thin** - Business logic belongs in services
6. **Run pre-commit checks** - Format, clippy, tests
7. **Use Detrix for debugging** - Don't add print statements!

## Resources

- **Architecture Guide:** [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- **Authentication:** [docs/AUTH.md](docs/AUTH.md)
- **Installation:** [docs/INSTALL.md](docs/INSTALL.md)
- **Adding Languages:** [docs/ADD_LANGUAGE.md](docs/ADD_LANGUAGE.md)
- **GitHub:** [https://github.com/flashus/detrix](https://github.com/flashus/detrix)
