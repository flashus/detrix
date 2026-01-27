# Detrix

> **LLM-First Dynamic Observability Platform**
> Add metrics to any line of code without redeployment. Built for AI agents and developers.

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## What is Detrix?

Detrix is agentic debugger that lets you **dynamically add metrics to any line of code** in your running application—no redeployment, no code changes, no restarts.

**How it works:** Detrix uses **debugger protocols** (DAP) to set **non-breaking logpoints** that capture values without modifying source code or pausing execution.
After installation, you can start your app with debugger and use Detrix to add metrics dynamically. Ask your agent to debug code using detrix.

```bash
# Add a metric dynamically (requires a connection first)
detrix metric add order_total --location "@checkout.py#127" --expression "order.total" --connection python

# Query metric events
detrix event query --metric order_total
```

---

## Installation

```bash
# Clone and build
git clone https://github.com/flashus/detrix.git
cd detrix
cargo build --release

# Verify
./target/release/detrix --version

# Initialize configuration (creates ~/detrix/detrix.toml)
./target/release/detrix init
```

Then configure your AI client. See [INSTALL.md](docs/INSTALL.md) for detailed setup instructions for Claude Code, Cursor, and Windsurf.

---

## Basic Usage

### 1. Start Your App with Debugger

```bash
# Python
python -m debugpy --listen 127.0.0.1:5678 --wait-for-client app.py

# Go
dlv debug --headless --listen=:5678 --api-version=2 main.go

# Rust (option 1: lldb-dap with TCP support)
lldb-dap --port 5678

# Rust (option 2: detrix wrapper for lldb-dap(osx/linux)/CodeLLDB(windows))
detrix lldb-serve ./target/debug/my_app --listen 127.0.0.1:5678
```

### 2. Use with AI Assistant

```
"Add a metric to track user login attempts at auth.py line 42"
"Show me the last 100 events for checkout_total"
"Add a metric with stack trace, auto-disable after 30 minutes"
```

### 3. Or Use CLI

```bash
# Create connection first
detrix connection create --port 5678 --language python --id myconn

# Add metric
detrix metric add user_login --location "@auth.py#42" --expression "user.id" --connection myconn

# Query events
detrix event query --metric user_login
```

---

## Features

| Feature | Description |
|---------|-------------|
| **Zero-downtime instrumentation** | Add metrics without restarting |
| **Multi-language support** | Python, Go, Rust |
| **Multiple capture modes** | Stream, sample, throttle, first-hit, time-based |
| **Runtime introspection** | Stack traces, memory snapshots, TTL |
| **Safety validation** | Expression validation prevents unsafe code |
| **LLM-native** | 28 MCP tools for AI agent integration |

### API Protocols

| Protocol | Port | Purpose |
|----------|------|---------|
| MCP | stdio | LLM integration |
| gRPC | 50061 | High-performance RPC |
| REST | 8090 | HTTP/JSON API |
| WebSocket | 8090 | Real-time event streaming |

---

## Configuration

Initialize default configuration:

```bash
# Create config at default location (~/detrix/detrix.toml)
detrix init

# Or specify custom location
detrix init --path /custom/path/detrix.toml

# Overwrite existing config
detrix init --force
```

**Configuration discovery priority:**
1. `--config <path>` CLI argument
2. `DETRIX_CONFIG` environment variable
3. `~/detrix/detrix.toml` (default)

Example configuration (see [detrix.toml](detrix.toml) for all options):

```toml
[storage]
path = "./detrix.db"
pool_size = 5

[api.rest]
enabled = true
host = "127.0.0.1"
port = 8090

[api.grpc]
enabled = true
port = 50061

[safety]
enable_ast_analysis = true
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     Detrix Daemon                       │
│                    (Rust + SQLite)                      │
└─────────────────────────────────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
   ┌────▼────┐       ┌────▼────┐       ┌────▼────┐
   │   MCP    │       │  REST    │       │   DAP    │
   │  Server  │       │  gRPC    │       │ Adapters │
   └────┬─────┘       └────┬─────┘       └────┬─────┘
        │                  │                  │
   ┌────▼────┐       ┌────▼────┐       ┌────▼────┐
   │  Claude  │       │   CLI    │       │ debugpy  │
   │  Cursor  │       │   Web    │       │  delve   │
   │Windsurf  │       │   Apps   │       │ lldb-dap │
   └──────────┘       └──────────┘       └──────────┘
```

Built with Clean Architecture (DDD, SOLID). See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

---

## Development

```bash
# Build
cargo build --release

# Run tests
cargo test --all

# Pre-commit checks
# Run all quality checks
task pre-commit

# Or manually:
cargo fmt --all
cargo clippy --all -- -D warnings
cargo test --all
```



---

## Roadmap

### v1.0 (Current)
- ✅ Python support via debugpy
- ✅ Go support via delve
- ✅ Rust support via lldb-dap
- ✅ MCP server for LLM integration
- ✅ Stack trace capture
- ✅ Memory snapshots
- ✅ Time-based sampling
- ✅ SQLite storage
- ✅ Dead-letter queue
- ✅ gRPC API
- ✅ REST API
- ✅ WebSocket streaming
- ✅ Prometheus metrics export
- ✅ System control (wake/sleep/status)
- ✅ Config hot reload

### Planned
- 🔲 Node.js/TypeScript support
- 🔲 Metric templates
- 🔲 Web dashboard


---

## FAQ

**Q: Does this work in production?**
A: Yes. Detrix uses standard DAP logpoints with minimal overhead. Memory snapshots and stack traces add more overhead.

**Q: What languages are supported?**
A: Python, Go, and Rust (v1.0). Node.js is planned.

**Q: Is it safe to evaluate expressions?**
A: Detrix includes three-layer safety validation: tree-sitter AST analysis, function classification (pure/impure/mutation), and optional LSP purity analysis. Prevents dangerous operations like `eval()`, file I/O, network calls, etc.

---

## Contributing

1. Fork the repository
2. Create a feature branch
3. Run `cargo fmt && cargo clippy --all -- -D warnings && cargo test --all`
4. Submit a Pull Request

See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for development guidelines.

---

## License

MIT License - see [LICENSE](LICENSE) file.

---

## Links

- [Installation Guide](docs/INSTALL.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Adding Language Support](docs/ADD_LANGUAGE.md)
- [GitHub Issues](https://github.com/flashus/detrix/issues)

**Built with Rust for developers and AI agents**
