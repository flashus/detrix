# Detrix Architecture

**Version:** 1.2.0 | **Last Updated:** March 2026

Detrix is an LLM-first dynamic observability platform that enables developers and AI agents to add metrics to any line of code without redeployment or code changes.

## How It Works

Detrix uses **debugger protocols (DAP - Debug Adapter Protocol)** to set **non-breaking observation points** (logpoints) that capture metrics without modifying source code or pausing execution.

```
                              stdio (JSON-RPC)              HTTP POST /mcp
┌─────────────────┐         ┌─────────────────┐         ┌──────────────────┐
│   Claude Code   │────────▶│  detrix mcp     │────────▶│  Detrix Daemon   │
│   (AI Agent)    │◀────────│  (MCP Bridge)   │◀────────│                  │
└─────────────────┘         └─────────────────┘         └────────┬─────────┘
                                                                 │
                                                                 │ DAP Protocol
                                                                 │
                         ┌───────────────────────────────────────┼───────────────┐
                         │                                       │               │
                         ▼                                       ▼               ▼
                    ┌─────────┐                             ┌─────────┐    ┌─────────┐
                    │ debugpy │                             │  delve  │    │lldb-dap │
                    │(Python) │                             │  (Go)   │    │ (Rust)  │
                    └────┬────┘                             └────┬────┘    └────┬────┘
                         │                                       │               │
                         ▼                                       ▼               ▼
                    Your Python                             Your Go App   Your Rust App
```

**Flow (Bridge Mode - Default):**
1. **Claude Code** calls MCP tools (e.g., `add_metric`) via stdio
2. **detrix mcp** receives JSON-RPC, forwards as HTTP POST to daemon's `/mcp` endpoint
3. **Detrix Daemon** executes the request via Services → DAP Broker
4. **DAP Adapters** set logpoints in your apps, capture events
5. Response flows back: Daemon → Bridge → Claude Code

**Alternative: Direct Mode** (`detrix mcp --no-daemon`)
- Runs full MCP server without daemon, useful for testing

### Cloud Mode

For Docker containers and remote hosts, the daemon runs alongside your service and the AI agent connects to it remotely:

```
  AI Agent              Detrix Daemon               Your Container
  (local machine)       (Docker/remote)             (Go/Python/Rust)
       │                       │                           │
       │── MCP over HTTP ─────▶│                           │
       │                       │── DAP logpoint ──────────▶│
       │                       │◀─ captured values ────────│
       │◀── structured events ─│                           │
       │                       │                           │
       │          VFS fetches source files automatically   │
```

**Cloud mode components:**
- **`advertise_url`** — Set `DETRIX_ADVERTISE_URL` on the daemon so the MCP bridge knows where to connect
- **Detrix client** — Embed in your app (`detrix.Init(...)`); it registers with the daemon and exposes a control plane HTTP server. Enables on-demand debugger wake/sleep
- **Virtual File System (VFS)** — Daemon fetches source files transparently so the agent can suggest correct line numbers. Sources (configurable priority): `bridge` (agent workspace), `control_plane` (app container), `disk` (local path)
- **Token auth** — Set `DETRIX_TOKEN` on both daemon and client for secure multi-tenant deployments

See `examples/docker-demo/` for a complete working Docker example.

## Clean Architecture

Detrix follows **Clean Architecture** principles with strict dependency rules:

```
┌──────────────────────────────────────────────────────────────────┐
│  Interface Layer (detrix-cli, detrix-api)                       │
│  - CLI commands, gRPC/REST/WebSocket/MCP controllers            │
│  - Depends on: Application layer                                │
├──────────────────────────────────────────────────────────────────┤
│  Application Layer (detrix-application)                         │
│  - Use cases (services): MetricService, ConnectionService, etc. │
│  - Safety validation logic                                      │
│  - Depends on: Ports + Domain + Config                          │
├──────────────────────────────────────────────────────────────────┤
│  Ports Layer (detrix-ports)                                     │
│  - Port trait definitions (interfaces for repositories,         │
│    adapters, caches, outputs)                                   │
│  - Depends on: Domain + Config only                             │
├──────────────────────────────────────────────────────────────────┤
│  Infrastructure Layer (adapters)                                │
│  - detrix-storage: SQLite repository implementations            │
│  - detrix-dap: DAP adapter implementations                      │
│  - detrix-lsp: LSP purity analyzer                              │
│  - detrix-output: GELF/log output                               │
│  - Depends on: Ports (implements traits) + Domain               │
├──────────────────────────────────────────────────────────────────┤
│  Domain Layer (detrix-core, detrix-config)                      │
│  - Entities: Metric, MetricEvent, Connection                    │
│  - Value Objects: MetricId, Location                            │
│  - Config types                                                  │
│  - Depends on: NOTHING (pure domain)                            │
└──────────────────────────────────────────────────────────────────┘
```

### Dependency Rules (CRITICAL)

```
detrix-cli        → ALL crates (composition root)
detrix-api        → detrix-application, detrix-core, detrix-config
detrix-application→ detrix-ports, detrix-core, detrix-config ONLY (NO infrastructure!)
detrix-ports      → detrix-core, detrix-config ONLY (port definitions)
detrix-storage    → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-dap        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-lsp        → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-output     → detrix-ports, detrix-core, detrix-application* (implements traits)
detrix-core       → NOTHING (pure domain)
detrix-config     → NOTHING (standalone)
detrix-testing    → detrix-ports, detrix-core, detrix-application (test mocks)
```

**Key Rule:** `detrix-ports` defines port traits. `detrix-application` NEVER depends on infrastructure crates like `detrix-storage` or `detrix-dap`. Infrastructure crates implement traits from `detrix-ports`.

**Note (*):** Infrastructure crates currently depend on `detrix-application` for shared types like `JwksValidator` and safety validators. This is a pragmatic deviation from strict Clean Architecture - extracting these to ports would require significant refactoring. The critical invariant (application never imports infrastructure) is maintained.

## Crate Overview

### Domain Layer

**detrix-core** - Pure domain entities and value objects
- **Entities:**
  - `Metric` - Observation point definition
  - `MetricEvent` - Captured value at observation point
  - `Connection` - DAP adapter connection state
  - `MetricAnchor` - Location tracking for code changes
- **Value Objects:**
  - `Location` - File path + line number
  - `MetricId`, `ConnectionId` - Typed identifiers
  - `SourceLanguage` - Supported programming languages
  - `CapturedStackTrace`, `MemorySnapshot` - Introspection data
  - `PurityAnalysis` - Expression purity classification
- No external dependencies except serialization

**detrix-config** - Configuration types and loading
- `DetrixConfig` - Main config structure
- TOML parsing and validation
- Language-specific adapter configs
- Safety configuration per language (`safety/{python,go,rust}.rs`):
  - Function classification constants (PURE, IMPURE, ACCEPTABLE_IMPURE, MUTATION)
  - User-extensible allowed/prohibited function lists
  - Sensitive variable patterns

### Ports Layer

**detrix-ports** - Port trait definitions (Clean Architecture interfaces)
- **Repository Ports:**
  - `MetricRepository` - Metric storage interface
  - `EventRepository` - Event storage interface
  - `ConnectionRepository` - Connection storage interface
  - `SystemEventRepository` - System event storage interface
  - `McpUsageRepository` - MCP usage statistics storage
  - `DlqRepository` - Dead-letter queue storage
- **Adapter Ports:**
  - `DapAdapter` - Interface for DAP adapters
  - `DapAdapterFactory` - Factory for creating language-specific adapters
- **Service Ports:**
  - `AnchorService` - Metric location tracking port
  - `PurityAnalyzer` - LSP-based purity analysis port
  - `PurityCache` - Purity result caching port
- **Output Ports:**
  - `EventOutput` - Event output routing port (GELF, etc.)
  - `FileWatcher` - File system monitoring port
  - `RemoteAppControl` - Remote application wake/sleep control port

### Application Layer

**detrix-application** - Business logic (use cases and services)
- **Services (Use Cases):**
  - `MetricService` - CRUD for metrics, file change handling
  - `ConnectionService` - DAP connection management
  - `StreamingService` - Event querying and streaming
  - `EventCaptureService` - Event persistence
  - `AdapterLifecycleManager` - Adapter lifecycle management
  - `ConfigService` - Configuration CRUD and hot reload
  - `SystemEventService` - System event logging and querying
  - `McpUsageService` - MCP tool usage tracking and analytics
  - `FileInspectionService` - Source file analysis for IDE integration
  - `AnchorService` - Metric location tracking across code changes
  - `EnvironmentService` - Environment detection (debuggers, LSP servers)
  - `DlqRecoveryService` - Dead-letter queue recovery for failed events
  - `FileWatcherService` - File system monitoring for auto-relocation
  - `RemoteAppService` - Remote application wake/sleep orchestration
  - `JwksValidator` - JWT validation for external auth mode

- **Access Control:**
  - `MetricScope` enum - Multi-tenant access control (Admin/User/Agent)
  - `extract_scope()` - Builds scope from authenticated user + agent identity

- **Safety:**
  - `ExpressionValidator` trait - Expression safety validation interface
  - `BaseValidator` trait - Shared validation logic (purity classification)
  - `ValidatorRegistry` - Language-specific validator dispatch
  - `PythonValidator`, `GoValidator`, `RustValidator` - Language implementations
  - Tree-sitter analyzers in `safety/treesitter/` module:
    - `python.rs`, `go.rs`, `rust_lang.rs` - Language parsers
    - `scope.rs` - Scope-aware mutation analysis
    - `cache.rs` - Parser cache for performance
    - `comment_stripper.rs` - Comment removal utilities

### Infrastructure Layer

**detrix-storage** - SQLite persistence
- Implements repository traits from `detrix-ports`
- Schema migrations
- Transaction management
- Optimized queries for metrics and events

**detrix-dap** - DAP adapter implementations
- `BaseAdapter<P>` - Generic adapter with 80% shared code
- `PythonOutputParser` - Python/debugpy specifics
- `GoOutputParser` - Go/delve specifics
- `RustOutputParser` - Rust/lldb-dap specifics
- All use same logpoint format: `DETRICS:name={expr1}\x1F{expr2}\x1F...` (expressions delimited by ASCII Unit Separator)

**detrix-lsp** - LSP-based purity analysis (optional)
- Call hierarchy traversal for user-defined functions
- Determines if functions have side effects
- Language-specific analyzers (pyright, gopls, rust-analyzer)

**detrix-output** - Event output routing
- GELF output for Graylog integration
- Future: OpenTelemetry, Prometheus, etc.

### Interface Layer

**detrix-api** - API controllers (thin layer)
- gRPC server (port 50061) - High-performance RPC
- REST server (port 8090) - HTTP/JSON API with 30+ handlers
- WebSocket server (port 8090) - Real-time event streaming
- MCP server (stdio) - LLM integration with 29 tools
- Controllers do DTO mapping ONLY, delegate to services

**detrix-cli** - CLI and composition root
- Command implementations
- Dependency injection and wiring
- Daemon mode management

### Support Crates

**detrix-logging** - Structured logging infrastructure
- GELF-compatible log formatting
- Configurable log levels
- Integration with tracing ecosystem

**detrix-testing** - Test utilities
- Mock implementations of all port traits (organized in `mocks/` module)
  - `mocks/adapters.rs` - MockDapAdapter, StatefulMockDapAdapter, MockDapAdapterFactory
  - `mocks/repositories.rs` - MockMetricRepository, MockEventRepository, MockConnectionRepository, MockDlqRepository, MockSystemEventRepository
  - `mocks/adapters.rs` also contains `MockRemoteAppControl`
- Test fixtures
- E2E testing framework (ApiClient, McpClient, GrpcClient, RestClient)

**detrix-tui** - Terminal UI (experimental)
- Interactive dashboard
- Real-time metric monitoring

## Key Concepts

### Metric
An observation point in your code, defined by location + one or more expressions.

```rust
Metric {
    id: MetricId("user_login"),
    location: Location {
        file_path: "@auth.py",
        line_number: 42
    },
    expressions: vec!["user.id", "user.role"],
    mode: CaptureMode::Stream,
    enabled: true,
    user_id: Some("alice"),       // authenticated owner
    agent_id: Some("uuid-1234"),  // creating agent/session
    ...
}
```

A single metric can capture multiple expressions simultaneously. Each captured event contains an `ExpressionValue` per expression with typed projections (numeric, string, boolean).

**Ownership fields:** `user_id` identifies the authenticated user who created the metric; `agent_id` identifies which agent or session created it. Both are used for multi-tenant access control — see [AUTH.md](AUTH.md).

### Logpoint
A DAP breakpoint with `logMessage` that captures values **without pausing execution**. Detrix converts metrics to logpoints using DAP protocol.

**Note:** Go (Delve) logpoints don't support function calls. When a Go expression contains function calls (like `fmt.Sprintf()`), Detrix automatically uses a breakpoint with "repl" evaluate context instead. This causes a brief pause (~1ms) but enables full expression evaluation. See `docs/ADD_LANGUAGE.md` for details.

### Connection
A DAP adapter connection to a running process. One Detrix daemon can manage multiple connections simultaneously (e.g., Python app + Go service + Rust binary).

### Event
Captured values when a logpoint fires. Contains:
- Metric ID
- One or more `ExpressionValue` entries (one per expression in the metric)
- Timestamp
- Optional: stack trace, memory snapshot

Each `ExpressionValue` includes the expression string, raw JSON value, and an optional typed projection (numeric, string, or boolean).

## Authentication & Authorization

Detrix supports multi-tenant access control. See [AUTH.md](AUTH.md) for full documentation.

**Auth modes:**
- **Auto** — secure by default: auto-generates token saved to `~/detrix/auth-token` (default when no config)
- **Disabled** — no auth, single-user Admin (explicit `mode = "disabled"`)
- **Simple** — per-user static bearer tokens in `detrix.toml`
- **External** — JWT validation via JWKS endpoint (Keycloak, Auth0, etc.)

**MetricScope** (`detrix-application/src/scope.rs`) enforces access control:
- `Admin` — full access to all metrics
- `User(uid)` — read/mutate own metrics
- `Agent { user_id, agent_id }` — read all user's metrics, mutate only own agent's metrics

**Two-tier metric/logpoint model:** Each user owns their own metric at a code location. Detrix merges all enabled metrics into a single shared DAP logpoint with unioned expressions.

## DAP Adapter Pattern

All language adapters follow the same pattern using `BaseAdapter<P>`:

```rust
pub trait OutputParser: Send + Sync + 'static {
    /// Parse DAP output event into MetricEvent
    async fn parse_output(
        body: &OutputEventBody,
        active_metrics: &Arc<RwLock<HashMap<String, Metric>>>,
    ) -> Option<MetricEvent>;

    /// Build logpoint message (DETRICS:name={expr1}\x1F{expr2}\x1F...)
    fn build_logpoint_message(metric: &Metric) -> String;

    /// Whether debugger needs explicit continue after connect
    fn needs_continue_after_connect() -> bool;

    /// Language identifier
    fn language_name() -> &'static str;
}
```

**Benefits:**
- ~80% code reuse across languages
- Only ~100 lines per new language
- Consistent behavior and error handling

## Safety System

Three-layer validation before expressions are evaluated:

1. **Tree-sitter AST Analysis**
   - Uses `tree-sitter-{python,go,rust}` crates for native parsing
   - Location: `detrix-application/src/safety/treesitter/`
   - Key files:
     - `python.rs`, `go.rs`, `rust_lang.rs` - Language parsers
     - `scope.rs` - Scope-aware mutation analysis (local vs global)
     - `node_kinds.rs` - Node kind constants for consistency

2. **Expression Validator** - Function classification enforcement
   - `PythonValidator`, `GoValidator`, `RustValidator`
   - `BaseValidator` trait - Shared classification logic
   - Function classification constants (single source of truth):
     - `{LANG}_PURE_FUNCTIONS` - Side-effect-free functions
     - `{LANG}_IMPURE_FUNCTIONS` - Always-impure (I/O, network, etc.)
     - `{LANG}_ACCEPTABLE_IMPURE` - Logging/printing (treated as pure)
     - `{LANG}_MUTATION_METHODS` - Scope-aware mutations
   - User extension via `user_allowed_functions` / `user_prohibited_functions`
   - Sensitive pattern detection (passwords, tokens, etc.)
   - Location: `detrix-application/src/safety/{python,go,rust}.rs`

3. **LSP Purity Analyzer** (optional) - Deep call hierarchy analysis
   - Analyzes user-defined functions for side effects
   - Caches results for performance
   - Uses language servers (pyright, gopls, rust-analyzer)
   - Location: `detrix-lsp/src/`

## API Protocols

| Protocol | Transport | Port | Purpose | Status |
|----------|-----------|------|---------|--------|
| gRPC | TCP | 50061 | High-performance RPC | ✅ Complete |
| MCP | stdio | - | LLM integration | ✅ Complete |
| REST | HTTP | 8090 | HTTP/JSON API | ✅ Complete |
| WebSocket | WS | 8090 | Real-time event streaming | ✅ Complete |
| DAP | TCP | 4711 | IDE integration (planned) | 📋 Planned |

### MCP Tools (for AI Agents)

**29 tools** for Claude Code and other LLM agents, organized by category:

**Workflow (3):** `observe`, `enable_from_diff`, `add_metric`
**Metrics (5):** `list_metrics`, `get_metric`, `update_metric`, `remove_metric`, `toggle_metric`
**Groups (3):** `list_groups`, `enable_group`, `disable_group`
**Events (3):** `query_metrics`, `query_system_events`, `acknowledge_events`
**Connections (4):** `create_connection`, `list_connections`, `get_connection`, `close_connection`
**Diagnostics (2):** `validate_expression`, `inspect_file`
**Configuration (4):** `get_config`, `update_config`, `validate_config`, `reload_config`
**System (3):** `get_status`, `get_mcp_usage`, `disconnect_all`
**Remote (2):** `wake`, `sleep`

> **`wake` / `sleep`** are the primary cloud debugging tools — the agent wakes the app's debugger, sets metrics, and sleeps it again when done. Zero overhead when not actively debugging.

### REST API Endpoints (Complete)

**Metrics:**
- `GET /api/v1/metrics` - List all metrics
- `POST /api/v1/metrics` - Create metric
- `GET /api/v1/metrics/:id` - Get metric details
- `PUT /api/v1/metrics/:id` - Update metric
- `DELETE /api/v1/metrics/:id` - Delete metric
- `POST /api/v1/metrics/:id/enable` - Enable metric
- `POST /api/v1/metrics/:id/disable` - Disable metric
- `GET /api/v1/metrics/:id/value` - Get latest value
- `GET /api/v1/metrics/:id/history` - Get value history

**Events:**
- `GET /api/v1/events` - Query captured events

**Groups:**
- `GET /api/v1/groups` - List groups
- `GET /api/v1/groups/:name/metrics` - List metrics in group
- `POST /api/v1/groups/:name/enable` - Enable all metrics in group
- `POST /api/v1/groups/:name/disable` - Disable all metrics in group

**Connections:**
- `GET /api/v1/connections` - List connections
- `POST /api/v1/connections` - Create connection
- `GET /api/v1/connections/:id` - Get connection details
- `DELETE /api/v1/connections/:id` - Close connection
- `POST /api/v1/connections/cleanup` - Remove stale connections
- `POST /api/v1/connections/touch` - Refresh last-seen timestamp on connections
- `POST /api/v1/connections/release` - Release reference counts on multiple connections
- `POST /api/v1/connections/:id/attach` - Attach (increment reference count) to a connection
- `POST /api/v1/connections/:id/release` - Release (decrement reference count) from a connection
- `GET /api/v1/connections/:id/references` - List active reference holders for a connection

**Configuration:**
- `GET /api/v1/config` - Get current configuration
- `PUT /api/v1/config` - Update configuration
- `POST /api/v1/config/reload` - Reload from file
- `POST /api/v1/config/validate` - Validate configuration

**Diagnostics:**
- `POST /api/v1/validate_expression` - Validate expression safety
- `POST /api/v1/inspect_file` - Parse source file for functions/variables
- `GET /api/v1/mcp/usage` - Get MCP usage statistics

**System:**
- `GET /health` - Health check
- `GET /metrics` - Prometheus metrics
- `GET /api/v1/status` - Get system status
- `POST /api/v1/disconnect_all` - Disconnect all adapters and flush state

**Remote:**
- `POST /api/v1/wake` - Wake remote app (requires `app_url` in body)
- `POST /api/v1/sleep` - Sleep remote app (requires `app_url` in body)

**MCP Bridge:**
- `POST /mcp` - MCP-over-HTTP bridge (JSON-RPC)
- `POST /mcp/heartbeat` - MCP client heartbeat
- `POST /mcp/disconnect` - MCP client disconnect

**VFS (Cloud):**
- `PUT /api/v1/files` - Provide a source file to the daemon (used by MCP bridge)
- `POST /api/v1/cache/validate` - Validate file cache entries
- `GET /api/v1/cache/:connection_id` - Get cached file hashes for a connection

**Admin (Cloud):**
- `POST /api/v1/admin/disconnect-all` - Force disconnect all connections (bypasses reference counts)
- `POST /api/v1/admin/disable-metrics-by-owner` - Disable all metrics owned by a client identity

### WebSocket Streaming (Complete)

- `GET /ws` - Upgrade to WebSocket for real-time event streaming
- Receives JSON messages with captured events
- Includes introspection data (stack traces, memory snapshots)
- Automatic reconnection support

## Directory Structure

```
detrix/
├── crates/
│   ├── detrix-core/           # Domain entities (pure)
│   ├── detrix-config/         # Configuration types + constants
│   │   └── src/safety/
│   │       ├── python.rs      # Python function constants + config
│   │       ├── go.rs          # Go function constants + config
│   │       └── rust.rs        # Rust function constants + config
│   ├── detrix-ports/          # Port trait definitions (interfaces)
│   │   └── src/
│   │       ├── adapter.rs     # DapAdapter, DapAdapterFactory traits
│   │       ├── repository.rs  # MetricRepository, EventRepository, etc.
│   │       ├── anchor.rs      # AnchorService trait
│   │       ├── cache.rs       # PurityCache trait
│   │       ├── dlq.rs         # DlqRepository trait
│   │       ├── file_watcher.rs# FileWatcher trait
│   │       ├── mcp_usage.rs   # McpUsageRepository trait
│   │       ├── output.rs      # EventOutput trait
│   │       └── purity.rs      # PurityAnalyzer trait
│   ├── detrix-application/    # Business logic + safety + access control
│   │   └── src/
│   │       ├── scope.rs           # MetricScope (multi-tenant access control)
│   │       └── safety/
│   │       ├── mod.rs             # ExpressionValidator trait, ValidatorRegistry
│   │       ├── base_validator.rs  # BaseValidator trait (shared logic)
│   │       ├── validation_result.rs # ValidationResult type
│   │       ├── python.rs          # Python expression validator
│   │       ├── go.rs              # Go expression validator
│   │       ├── rust.rs            # Rust expression validator
│   │       └── treesitter/        # Tree-sitter AST analyzers
│   │           ├── python.rs      # Python parser
│   │           ├── go.rs          # Go parser
│   │           ├── rust_lang.rs   # Rust parser
│   │           ├── scope.rs       # Scope-aware mutation analysis
│   │           ├── cache.rs       # Parser cache
│   │           └── comment_stripper.rs  # Comment removal
│   ├── detrix-storage/        # SQLite implementation
│   ├── detrix-dap/            # DAP adapters
│   ├── detrix-lsp/            # LSP purity analyzer
│   ├── detrix-output/         # Event outputs (GELF)
│   ├── detrix-logging/        # Structured logging
│   ├── detrix-api/            # API controllers (gRPC, REST, WS, MCP)
│   ├── detrix-cli/            # CLI + composition root
│   ├── detrix-testing/        # Test mocks + E2E utilities
│   │   └── src/mocks/
│   │       ├── adapters.rs    # MockDapAdapter, StatefulMockDapAdapter
│   │       └── repositories.rs# MockMetricRepository, MockEventRepository, etc.
│   └── detrix-tui/            # Terminal UI (experimental)
├── clients/                   # Detrix clients
│   ├── python/                # Python client (detrix-py on PyPI)
│   ├── go/                    # Go client (github.com/flashus/detrix/clients/go)
│   └── rust/                  # Rust client (detrix-rs on crates.io)
├── docker/                    # Docker E2E infrastructure
├── fixtures/                  # Example apps for testing
├── examples/
│   └── docker-demo/           # Cloud debugging example (Go service + Detrix daemon)
├── skills/                    # Claude Code skill
└── docs/
    ├── ARCHITECTURE.md        # This file
    ├── AUTH.md                # Authentication & authorization guide
    ├── ADD_LANGUAGE.md        # Guide for adding languages
    ├── INSTALL.md             # Installation guide
    ├── PUBLISHING.md          # Client publishing guide
    └── CONTRIBUTING.md        # Contributing guide
```

## Adding a New Language

See [ADD_LANGUAGE.md](ADD_LANGUAGE.md) for detailed guide. High-level steps:

1. Add `SourceLanguage` variant to `detrix-core/src/entities/language.rs`
2. Add safety config in `detrix-config/src/safety/{lang}.rs` with function constants
3. Implement `OutputParser` trait in `detrix-dap/src/{lang}.rs` (~100 lines)
4. Add to `DapAdapterFactory` and `AdapterLifecycleManager`
5. Implement tree-sitter analyzer in `safety/treesitter/{lang}.rs`
6. Implement `ExpressionValidator` (via `BaseValidator`) in `safety/{lang}.rs`
7. Add LSP purity analyzer in `detrix-lsp/src/{lang}.rs` (optional)
8. Add tests and documentation

## Development Commands

```bash
# Build
cargo build --release

# Run tests
cargo test --all

# Pre-commit checks
cargo fmt --all
cargo clippy --all -- -D warnings
cargo test --all

# Run daemon
./target/release/detrix serve --daemon

# Run MCP for agent
./target/release/detrix mcp

# Connect to debugger
./target/release/detrix connect python --port 5678
```

## Contributing

When contributing to Detrix:

1. **Follow Clean Architecture** - Check dependency rules before importing
2. **Use `From` trait for errors** - Or extension traits for context
3. **Write tests first** - TDD approach preferred
4. **Port traits in detrix-ports** - Never in core or infrastructure
5. **Controllers are thin** - Business logic belongs in services
6. **Run pre-commit checks** - Format, clippy, tests

## Resources

- [Installation Guide](INSTALL.md)
- [Authentication & Authorization](AUTH.md)
- [CLI Reference](CLI.md)
- [Publishing Guide](PUBLISHING.md)
- [Adding Languages](ADD_LANGUAGE.md)
- [GitHub Repository](https://github.com/flashus/detrix)
- [GitHub Issues](https://github.com/flashus/detrix/issues)
- [GitHub Discussions](https://github.com/flashus/detrix/discussions)
