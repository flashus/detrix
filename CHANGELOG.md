# Changelog

All notable changes to Detrix will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-02-10

### Added
- **Multi-expression metrics**: Single metric can observe multiple expressions
  simultaneously (`expressions: ["symbol", "quantity", "price"]`)
- `ExpressionValue` type with typed projections (numeric, string, boolean)
- GELF output includes per-expression custom fields (`_expr_N_name`, `_expr_N_value`)
- Configurable `max_expressions_per_metric` limit (default: 20)

### Breaking Changes
- **WebSocket API**: Field names changed from snake_case to camelCase
  (`metric_id` -> `metricId`, `stack_trace` -> `stackTrace`)
- **gRPC/Proto**: `expression` (singular) replaced with `expressions` (repeated)
- **REST API**: `expression` field replaced with `expressions` array
- **Database**: Schema consolidated. Requires clean install from v1.0.

### Fixed
- Expression safety validation now enforced on config-file metric imports

## [1.0.0] - 2026-01-27

### Added

#### Core Features
- **Dynamic observability platform** - Add metrics to any line of code without redeployment
- **Non-breaking observation points** - Logpoints that capture values without pausing execution
- **Multi-language support** - Python, Go, and Rust via DAP adapters
- **Clean Architecture** - Domain-Driven Design with strict layer separation (13 crates)

#### Language Support
- **Python support** via debugpy
  - DAP adapter with full logpoint support
  - AST analyzer for expression safety validation
  - LSP purity analysis via pylsp/pyright
  - Expression validator with whitelist/blacklist
- **Go support** via delve
  - DAP adapter with full logpoint support
  - AST analyzer for expression validation
  - LSP purity analysis via gopls
  - Expression validator
- **Rust support** via lldb-dap
  - DAP adapter with full logpoint support
  - AST analyzer for expression validation
  - LSP purity analysis via rust-analyzer
  - Expression validator

#### APIs (All Complete)
- **MCP (Model Context Protocol)** - 28 tools for LLM integration

  **Workflow Tools:**
  - `observe` - Simplest way to add metrics (auto-finds line, auto-generates name)
  - `enable_from_diff` - Parse git diff with print/log statements, create metrics automatically
  - `add_metric` - Create observation point at specific file:line

  **Metric Management:**
  - `list_metrics` - List all metrics with filtering (by group, enabled status)
  - `get_metric` - Get details about specific metric by name
  - `update_metric` - Modify metric properties (expression, group, enabled status)
  - `remove_metric` - Delete metric and historical data
  - `toggle_metric` - Enable/disable metric without removing

  **Group Management:**
  - `list_groups` - List all metric groups with counts
  - `enable_group` - Enable all metrics in a group
  - `disable_group` - Disable all metrics in a group

  **Event Querying:**
  - `query_metrics` - Query captured values by metric name or group
  - `query_system_events` - Query system-level events
  - `acknowledge_events` - Mark events as acknowledged

  **Connection Management:**
  - `create_connection` - Connect to debugger at host:port (returns connection_id)
  - `list_connections` - List all debugger connections (filter by active status)
  - `get_connection` - Get detailed information about specific connection
  - `close_connection` - Disconnect from debugger

  **Diagnostic & Validation:**
  - `validate_expression` - Check if expression is safe (prevents eval/exec/file I/O)
  - `inspect_file` - Find correct line for metric, list variables at line

  **Configuration:**
  - `get_config` - Get current Detrix configuration as TOML
  - `update_config` - Update configuration values
  - `validate_config` - Validate configuration before applying
  - `reload_config` - Reload configuration from file on disk

  **System Control:**
  - `wake` - Resume all connections from sleep mode
  - `sleep` - Stop all connections (zero overhead when sleeping)
  - `get_status` - Get system status (uptime, active connections, metrics count)
  - `get_mcp_usage` - Get MCP usage statistics and error patterns

  **Features:**
  - Bridge mode with daemon auto-spawn
  - Heartbeat keep-alive mechanism (10 seconds)
  - Automatic connection selection (when only one exists)
  - Auto-line detection (when expression found in file)
  - Auto-name generation (when metric name not specified)
  - Comprehensive error tracking and usage statistics

- **gRPC API** (port 50061)
  - Full metrics management
  - Connection management
  - Event streaming
  - Generated from protobuf definitions

- **REST API** (port 8090)
  - **Metrics**: CRUD, enable/disable, value/history retrieval
  - **Events**: Query with filters
  - **Groups**: List, manage, enable/disable all metrics in group
  - **Connections**: Create, list, get details, close
  - **Configuration**: Get, update, reload, validate
  - **Diagnostics**: Expression validation, file inspection, MCP usage stats
  - **System**: Health check, Prometheus metrics, wake/sleep/status
  - Rate limiting with configurable limits
  - CORS support
  - Organized by domain (8 handler modules)

- **WebSocket API** (port 8090)
  - Real-time event streaming
  - JSON message format
  - Includes introspection data (stack traces, memory snapshots)
  - Client lag handling with automatic recovery

#### Capture Features
- **Capture modes**
  - `stream` - Capture every hit
  - `sample` - Capture every Nth hit
  - `sample_interval` - Capture every N seconds
  - `first` - Capture only first hit
  - `throttle` - Maximum N events per second

- **Introspection**
  - **Stack trace capture** with configurable depth
  - **Memory snapshots** - Local and global variables
  - **Time-based sampling** - Periodic capture
  - **TTL auto-disable** - Temporary debugging with automatic cleanup

#### Safety & Security
- **Expression validation** (3-layer system)
  - AST analysis via external analyzers
  - Whitelist/blacklist enforcement
  - Sensitive pattern detection (passwords, tokens, keys)
- **LSP purity analysis** (optional)
  - Call hierarchy traversal for user-defined functions
  - Side-effect detection
  - Language-specific analyzers
- **Configurable safety rules**
  - Custom allowed/prohibited functions
  - Custom sensitive patterns

#### Storage & Persistence
- **SQLite backend** with schema migrations
- **Event storage** with efficient querying
- **Metric persistence** across daemon restarts
- **Connection state management**

#### Configuration
- **TOML configuration** (`detrix.toml`)
  - Language-specific adapter settings
  - API server configuration
  - Storage settings
  - Safety rules customization
  - Rate limiting
- **Configuration hot reload** via REST API
- **Configuration validation** before applying

#### Output & Observability
- **GELF output** for Graylog integration
- **Prometheus metrics** export
- **System metrics** (CPU, memory, uptime)
- **MCP usage tracking** and statistics

#### Daemon & Lifecycle
- **Daemon mode** with background service
- **PID file locking** (single instance)
- **Auto-spawn** when MCP connects
- **Heartbeat keep-alive** (10 seconds)
- **Auto-shutdown** when no clients
- **Adapter reconnection** with exponential backoff
- **Sleep/wake modes** for resource management

#### Developer Experience
- **File inspection** - Parse source files, extract functions/variables
- **Expression validation** - Test safety before setting metrics
- **Comprehensive CLI** with subcommands
- **Terminal UI** (experimental) - Interactive dashboard
- **Task runner** with pre-built workflows

#### Documentation
- Complete installation guide for multiple platforms (macOS, Linux, Windows)
- Architecture documentation with Clean Architecture principles
- Adding language support guide
- Contributing guidelines
- API reference for all protocols
- MCP integration guide for Claude Code

#### Testing
- 2,000+ tests with comprehensive coverage
- Unit tests for all crates
- Integration tests for DAP adapters
- E2E tests for Python/Go/Rust workflows
- LSP purity analysis tests
- Graceful skipping when tools unavailable
- Task runner for organized test execution

### Technical Details

#### Architecture
- **13 Rust crates** following Clean Architecture
  - `detrix-core` - Pure domain entities
  - `detrix-config` - Configuration types
  - `detrix-ports` - Port trait definitions (interfaces)
  - `detrix-application` - Business logic and services
  - `detrix-storage` - SQLite implementation
  - `detrix-dap` - DAP adapters
  - `detrix-lsp` - LSP purity analyzer
  - `detrix-output` - Event output routing (GELF)
  - `detrix-logging` - Structured logging infrastructure
  - `detrix-api` - API controllers (gRPC, MCP, HTTP, WS)
  - `detrix-cli` - CLI and composition root
  - `detrix-testing` - Test mocks and fixtures
  - `detrix-tui` - Terminal UI

#### Code Quality
- Rust edition 2021, minimum version 1.80
- Strict clippy linting (zero warnings)
- Consistent code formatting with rustfmt
- Test-Driven Development approach
- Dependency injection with trait objects
- Error handling via `From` trait implementations

#### Performance
- Async/await throughout with Tokio runtime
- Connection pooling for SQLite
- Broadcast channels for event streaming
- Efficient DAP protocol handling
- Low overhead in sleep mode

### Dependencies
- **Rust Core**: tokio, serde, sqlx, chrono, thiserror
- **APIs**: axum (REST/WS), tonic (gRPC), tower (middleware)
- **DAP**: Custom DAP client implementation
- **LSP**: tower-lsp for language server communication
- **Output**: GELF output for Graylog

### Installation
- Manual installation for Claude Code, Claude Desktop, Cursor, Windsurf
- Binary builds via Cargo

### Known Limitations
- REST API uses wide-open CORS for development (production should restrict)
- Terminal UI is experimental
- Single SQLite database

### Migration Notes
This is the initial release - no migrations needed.

---

## Roadmap

### Planned for Future Releases
- Node.js/TypeScript support via node-debug
- Web dashboard
- PostgreSQL support

---

## Links

- [Repository](https://github.com/flashus/detrix)
- [Issues](https://github.com/flashus/detrix/issues)
- [Documentation](https://github.com/flashus/detrix/tree/main/docs)

