# Contributing to Detrix

Thank you for your interest in contributing to Detrix! This guide will help you get started.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Architecture Principles](#architecture-principles)
- [Coding Guidelines](#coding-guidelines)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Adding Language Support](#adding-language-support)
- [Getting Help](#getting-help)

## Code of Conduct

We are committed to providing a welcoming and inclusive environment. Please be respectful and constructive in all interactions.

## Getting Started

1. **Fork the repository** on GitHub
2. **Clone your fork** locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/detrix.git
   cd detrix
   ```
3. **Add upstream remote**:
   ```bash
   git remote add upstream https://github.com/flashus/detrix.git
   ```

## Development Setup

### Prerequisites

- **Rust 1.80+** - [Install Rust](https://rustup.rs/)
- **Git** for version control
- **Task** (optional) - [Install Task](https://taskfile.dev/) for build automation

For testing language adapters:
- **Python 3.8+** with `debugpy` - `pip install debugpy`
- **Go 1.19+** with `delve` - `go install github.com/go-delve/delve/cmd/dlv@latest`
- **Rust toolchain** with `lldb-dap` (usually included with LLVM)

### Building from Source

```bash
# Build all crates
cargo build

# Build in release mode
cargo build --release
```

### Running Tests

```bash
# Run all tests
cargo test --all

# Run tests for specific crate
cargo test -p detrix-core

# Run with output
cargo test --all -- --nocapture
# OR
task test
```

### Pre-commit Checks

Before submitting a PR, run:

```bash
# Format code
cargo fmt --all

# Check for issues
cargo clippy --all -- -D warnings

# Run tests
cargo test --all

# Or use task runner (if installed)
task pre-commit
```

## Architecture Principles

Detrix follows **Clean Architecture** with strict dependency rules. **Read [ARCHITECTURE.md](ARCHITECTURE.md) before contributing.**

### Dependency Rules (CRITICAL)

```
detrix-cli        → can depend on ALL crates
detrix-api        → detrix-application, detrix-ports, detrix-core
detrix-application→ detrix-ports, detrix-core, detrix-config ONLY
detrix-ports      → detrix-core, detrix-config ONLY
detrix-storage    → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-dap        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-lsp        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-testing    → detrix-ports, detrix-core, detrix-application (test mocks)
detrix-core       → NOTHING (pure domain)
```

**Key Rule:** `detrix-ports` defines port traits. `detrix-application` NEVER depends on infrastructure crates like `detrix-storage` or `detrix-dap`. Infrastructure crates implement traits from `detrix-ports`.

**Note (*):** Infrastructure crates depend on `detrix-application` for shared types (safety validators, JwksValidator). The key invariant is maintained: application never imports infrastructure.

### Layer Responsibilities

- **detrix-core** - Pure domain entities, value objects, no dependencies
- **detrix-ports** - Port trait definitions (interfaces for repositories, adapters)
- **detrix-application** - Business logic, use cases, safety validation
- **detrix-storage, detrix-dap** - Infrastructure implementations of port traits
- **detrix-api** - Thin controllers that only do DTO mapping
- **detrix-cli** - Composition root, dependency injection

## Coding Guidelines

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
let result = query.fetch_all(&pool).await?;

// ✅ ALSO OK: Use extension traits with .map_err() for context
// The codebase uses patterns like ConfigIoResultExt, ToMcpResult, etc.
let result = file.read().config_context("reading config file")?;
```

### Code Style

1. **Functional style** - Prefer iterator chains over loops
2. **Type safety** - Use newtypes for distinct IDs (`MetricId`, `ConnectionId`)
3. **Dependency injection** - Services accept trait objects, not concrete types
4. **Async traits** - Use `#[async_trait]` for async trait methods
5. **Documentation** - Add doc comments for public APIs

### Naming Conventions

- **Ubiquitous Language** - Use domain terms consistently (see [CLAUDE.md](../CLAUDE.md#ubiquitous-language))
- **Port traits** - End with trait name: `MetricRepository`, `DapAdapter`
- **Services** - End with `Service`: `MetricService`, `ConnectionService`
- **Value Objects** - Descriptive names: `Location`, `MetricId`

### Example: Good Service Structure

```rust
// ✅ GOOD - Service with trait dependencies
use detrix_ports::{MetricRepository, DapAdapter};

pub struct MetricService {
    storage: Arc<dyn MetricRepository + Send + Sync>,
    adapter: Arc<dyn DapAdapter + Send + Sync>,
}

impl MetricService {
    pub fn new(
        storage: Arc<dyn MetricRepository + Send + Sync>,
        adapter: Arc<dyn DapAdapter + Send + Sync>,
    ) -> Self {
        Self { storage, adapter }
    }

    pub async fn add_metric(&self, metric: Metric) -> Result<MetricId> {
        // Business logic here
        let id = self.storage.save(&metric).await?;
        if metric.enabled {
            self.adapter.set_metric(&metric).await?;
        }
        Ok(id)
    }
}
```

## Testing

### Test-Driven Development

We follow TDD approach:

1. **Write test first** - Define expected behavior
2. **Implement** - Make the test pass
3. **Refactor** - Clean up while keeping tests green

### Test Structure

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_metric_success() {
        // Arrange - Set up test data (simplified example)
        let mock_storage = Arc::new(MockMetricRepository::new());
        let adapter_manager = create_test_adapter_manager();
        let (tx, _) = tokio::sync::broadcast::channel(100);
        let service = MetricService::builder(mock_storage, adapter_manager, tx).build();

        // Act - Execute the feature
        let metric = Metric::new("test", "@file.py#42", "user.id");
        let result = service.add_metric(metric).await;

        // Assert - Verify expected behavior
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_metric_storage_error() {
        // Test error handling
        let mock_storage = Arc::new(MockMetricRepository::with_error());
        let (tx, _) = tokio::sync::broadcast::channel(100);
        let service = MetricService::builder(mock_storage, adapter_manager, tx).build();

        let result = service.add_metric(metric).await;
        assert!(result.is_err());
    }
}
```

### Test Coverage

Every PR should include tests for:
- ✅ Happy path (valid inputs)
- ✅ Error cases (invalid inputs, failures)
- ✅ Edge cases (empty, null, boundaries)
- ✅ Integration scenarios (multiple components)

## Pull Request Process

### Before Submitting

1. **Sync with upstream**:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

2. **Run pre-commit checks**:
   ```bash
   cargo fmt --all
   cargo clippy --all -- -D warnings
   cargo test --all
   ```

3. **Update documentation** if you changed:
   - Public APIs
   - Configuration options
   - Architecture
   - Installation process

### PR Guidelines

1. **Create a feature branch**:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Write clear commit messages**:
   ```
   Add Go expression validator for safety system

   - Implement GoValidator with AST analysis
   - Add whitelist/blacklist for Go functions
   - Include comprehensive tests
   - Update ARCHITECTURE.md with Go safety details
   ```

3. **Keep PRs focused** - One feature/fix per PR

4. **Add tests** - All new code must have tests

5. **Update CHANGELOG.md** - Add entry for your change

### PR Template

When you create a PR, include:

```markdown
## Description
Brief description of what this PR does.

## Motivation
Why is this change needed? What problem does it solve?

## Changes
- Added X feature
- Fixed Y bug
- Updated Z documentation

## Testing
How was this tested?
- [ ] Unit tests added
- [ ] Integration tests added
- [ ] Manually tested with Python/Go/Rust

## Checklist
- [ ] Code follows project style guidelines
- [ ] Tests pass locally
- [ ] Documentation updated
- [ ] No breaking changes (or clearly documented)
```

### Review Process

1. Maintainers will review your PR within 3-5 days
2. Address feedback by pushing new commits
3. Once approved, a maintainer will merge

## Adding Language Support

Want to add support for a new language? See [ADD_LANGUAGE.md](ADD_LANGUAGE.md) for detailed guide.

**Quick overview:**

1. Add `SourceLanguage` variant to `detrix-core`
2. Add safety config in `detrix-config/src/safety/{lang}.rs` with function constants
3. Implement `OutputParser` trait in `detrix-dap` (~100 lines)
4. Add to `DapAdapterFactory` and `AdapterLifecycleManager`
5. Implement tree-sitter analyzer in `safety/treesitter/{lang}.rs`
6. Implement `ExpressionValidator` (via `BaseValidator`) in `safety/{lang}.rs`
7. Add tests and documentation

## Common Pitfalls to Avoid

### ❌ Dependency Violations

```rust
// ❌ WRONG - Application depending on infrastructure
// In detrix-application/Cargo.toml
[dependencies]
detrix-storage = { path = "../detrix-storage" }  // NEVER DO THIS!
```

### ❌ Business Logic in Controllers

```rust
// ❌ WRONG - Logic in controller
async fn add_metric(&self, req: Request<AddMetricRequest>) -> Result<Response<...>> {
    let metric = req.into_inner();
    let id = self.storage.save(&metric).await?;  // Business logic!
    self.adapter.set_metric(&metric).await?;     // Business logic!
    Ok(Response::new(...))
}

// ✅ CORRECT - Delegate to service
async fn add_metric(&self, req: Request<AddMetricRequest>) -> Result<Response<...>> {
    let metric = convert_request(req)?;  // DTO mapping only
    let id = self.metric_service.add_metric(metric).await?;  // Delegate
    Ok(Response::new(convert_response(id)))  // DTO mapping only
}
```

### ❌ Concrete Types in Services

```rust
// ❌ WRONG
pub struct MetricService {
    storage: Arc<SqliteStorage>,  // Concrete type!
}

// ✅ CORRECT
pub struct MetricService {
    storage: Arc<dyn MetricRepository + Send + Sync>,  // Trait object
}
```

## API Documentation

Detrix provides multiple APIs for different use cases:

- **gRPC (port 50061)** - High-performance RPC, full metrics management
- **MCP (stdio)** - LLM integration
- **REST (port 8090)** - HTTP/JSON API
- **WebSocket (port 8090)** - Real-time event streaming

All APIs are fully implemented and production-ready. See [ARCHITECTURE.md](ARCHITECTURE.md#api-protocols) for endpoint details.

## Project-Specific Notes

### Build Configuration

The project uses a custom build directory (see `.cargo/config.toml`). Binary location is resolved in this order:

1. `DETRIX_BIN` environment variable
2. `CARGO_TARGET_DIR` environment variable
3. `~/detrix/target` (config default)
4. `workspace/target` (Cargo default)

In CI environments, set `CARGO_TARGET_DIR` or `DETRIX_BIN` to override.

### Safety Validation

Three-layer expression safety validation:

1. **Tree-sitter AST Analysis** - In-process parsing
   - Location: `crates/detrix-application/src/safety/treesitter/`
   - Languages: Python, Go, Rust via `tree-sitter-{python,go,rust}` crates
   - Scope analysis: `scope.rs` for local vs global mutation detection

2. **Function Classification** - Whitelist/blacklist enforcement
   - Constants: `crates/detrix-config/src/safety/{python,go,rust}.rs`
   - Categories: PURE, IMPURE, ACCEPTABLE_IMPURE, MUTATION_METHODS
   - User extension: `user_allowed_functions` / `user_prohibited_functions`
   - Validators: `crates/detrix-application/src/safety/{python,go,rust}.rs`

3. **LSP Purity Analyzer** (optional) - Call hierarchy analysis
   - Location: `crates/detrix-lsp/src/`

### Database Migrations

SQLite schema migrations are in `crates/detrix-storage/migrations/`.

When adding migrations:
1. Create new migration file: `NNNN_description.sql`
2. Test up and down migrations
3. Update schema documentation

## Getting Help

- **Bug report?** Create an [Issue](https://github.com/flashus/detrix/issues)
- **Architecture questions?** Read [ARCHITECTURE.md](ARCHITECTURE.md)
- **Language support?** Read [ADD_LANGUAGE.md](ADD_LANGUAGE.md)

## Recognition

All contributors will be:
- Listed in the project's contributors page
- Mentioned in release notes for significant contributions
- Thanked in commit messages and PR descriptions

## License

By contributing to Detrix, you agree that your contributions will be licensed under the MIT License.

---

**Thank you for contributing to Detrix!** 🎉
