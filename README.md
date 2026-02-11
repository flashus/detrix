# Detrix

> **LLM-First Dynamic Observability Platform**
> Add metrics to any line of code without redeployment. Built for AI agents and developers.

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## What is Detrix?

Detrix gives AI agents the ability to **see inside running code**. An agent can place observation points on any line, capture values, and query results -- all through natural conversation, with zero code changes or restarts.

Under the hood, Detrix uses **debugger protocols** (DAP) to set **non-breaking logpoints** that capture values without modifying source code or pausing execution.

Here's what working with Detrix looks like:

```
You:   "My checkout flow is dropping orders. Can you look at what's happening
        around checkout.py line 127?"

Agent: I'll connect to your running app and add an observation point.
       → detrix_connection_create(port=5678, language="python")
       → detrix_metric_add("order_total", location="@checkout.py#127",
           expressions=["order.total", "order.currency", "len(order.items)"])
       Metric is live. I'm capturing values now.

You:   "What are we seeing?"

Agent: → detrix_event_query(metric="order_total", limit=50)
       I see 47 events in the last minute. Most orders look normal, but there
       are 3 where order.total is negative -- that's likely the bug. They all
       have currency="JPY". Want me to add a stack trace to see the call path
       for those?

You:   "Yes, and also watch the discount calculation"

Agent: → detrix_metric_update("order_total", stack_trace=true)
       → detrix_metric_add("discount_calc", location="@pricing.py#89",
           expressions=["discount.percentage", "discount.source"])
       Done. Both metrics are capturing. I'll check back in a moment...

       → detrix_event_query(metric="discount_calc")
       Found it. The discount for JPY orders is being calculated as a
       percentage of the USD-converted amount, but applied to the original
       JPY amount. The negative totals happen when discount > converted total.
```

No print statements were added. No code was modified. No restarts. The agent observed the running process, found the bug, and explained it.

---

## How It Works

```
┌─────────────────────────────────────────────────────────┐
│                     Detrix Server                       │
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

Detrix talks to your app's debugger (debugpy, delve, lldb-dap) via the **Debug Adapter Protocol**. It sets **logpoints** -- breakpoints that evaluate an expression and log the result instead of pausing. Your application runs at full speed; Detrix captures the values.

---

## Getting Started

### Install

```bash
git clone https://github.com/flashus/detrix.git
cd detrix
cargo build --release

# Initialize configuration (creates ~/detrix/detrix.toml)
./target/release/detrix init
```

Then configure your AI client. See [INSTALL.md](docs/INSTALL.md) for setup instructions for Claude Code, Cursor, and Windsurf.

### Start your app with a debugger

```bash
# Python
python -m debugpy --listen 127.0.0.1:5678 --wait-for-client app.py

# Go
dlv debug --headless --listen=:5678 --api-version=2 main.go

# Rust
lldb-dap --port 5678
# or: detrix lldb-serve ./target/debug/my_app --listen 127.0.0.1:5678
```

That's the only manual step. From here, the agent handles everything: connecting, adding metrics, querying events, and cleaning up when done.

---

## Detrix Clients

For production and long-running services, embed the Detrix client directly. It adds a lightweight control plane that lets an agent wake the debugger on demand -- zero overhead when not in use, no manual debugger startup required.

```python
import detrix

detrix.init(name="my-app")   # Starts control plane HTTP server. Zero overhead.
# App runs normally. When the agent needs to observe:
#   POST /detrix/wake  → starts debugpy, registers with Detrix server
#   Now the agent can add metrics, capture values, inspect state
#   POST /detrix/sleep → stops debugger, back to zero overhead
```

The agent controls the full lifecycle: wake when it needs to investigate, observe, then sleep when done.

Alternatively, you can [start the debugger manually](#getting-started) and let the agent attach directly -- useful for quick debugging sessions.

| | Client Embed | Attach to debugger |
|--|-----------|----------------|
| **Setup** | `detrix.init()` in your app | Start debugger manually |
| **Debugger lifecycle** | Agent manages (wake/sleep) | You manage |
| **Overhead when idle** | Zero -- no debugger loaded | Debugger always running |
| **Best for** | Production, long-running services | Quick debugging sessions |

| Language | Install | Docs |
|----------|---------|------|
| Python | `pip install detrix-py` | [Python Client](clients/python/README.md) |
| Go | `go get github.com/flashus/detrix/clients/go` | [Go Client](clients/go/README.md) |
| Rust | `detrix-rs = "1.0.0"` in Cargo.toml | [Rust Client](clients/rust/README.md) |

See the [Clients Manual](docs/CLIENTS.md) for full documentation, configuration, and publishing details.

---

## Features

| Feature | Description |
|---------|-------------|
| **Zero-downtime instrumentation** | Add metrics without restarting |
| **Multi-expression metrics** | Capture multiple values per observation point |
| **Multi-language support** | Python, Go, Rust |
| **Multiple capture modes** | Stream, sample, throttle, first-hit, time-based |
| **Runtime introspection** | Stack traces, memory snapshots, TTL |
| **Safety validation** | Three-layer expression validation prevents unsafe code |
| **LLM-native** | 28 MCP tools for AI agent integration |
| **Detrix clients** | Python, Go, Rust libraries for programmatic integration |

### API Protocols

| Protocol | Port | Purpose |
|----------|------|---------|
| MCP | stdio | LLM integration |
| gRPC | 50061 | High-performance RPC |
| REST | 8090 | HTTP/JSON API |
| WebSocket | 8090 | Real-time event streaming |

---

## CLI

For power users and scripting, Detrix has a full CLI. See [CLI Reference](docs/CLI.md).

---

## Configuration

Config file: `~/detrix/detrix.toml` (discovery: `--config <path>` > `DETRIX_CONFIG` env > default)

See [detrix.toml](detrix.toml) for all options. The daemon auto-spawns when using MCP; manage it manually with `detrix daemon start|stop|status|restart`. Logs in `~/detrix/log/`.

---

## Architecture

Clean Architecture (DDD, SOLID) with 13 Rust crates. See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

---

## Development

```bash
cargo build --release
cargo test --all

# Pre-commit
cargo fmt --all && cargo clippy --all -- -D warnings && cargo test --all
# or: task pre-commit
```

---

## Roadmap

**v1.0** -- Multi-language DAP support, MCP/gRPC/REST/WebSocket APIs, safety validation, introspection. See [CHANGELOG.md](CHANGELOG.md) for full details.

**v1.1 (Current)** -- Multi-expression metrics, `ExpressionValue` typed projections, detrix clients (Python, Go, Rust).

**Planned** -- Node.js/TypeScript support, web dashboard.

---

## FAQ

**Does this work in production?**
Yes. DAP logpoints have minimal overhead. Advanced features like function calls, memory dumps, and call stacks require setting actual breakpoints and may impact performance -- use those in development environments.

**Is it safe to evaluate expressions?**
Three-layer validation: tree-sitter AST analysis, function classification (pure/impure/mutation), and optional LSP purity analysis. Blocks `eval()`, file I/O, network calls, etc.

---

## Contributing

1. Fork the repository
2. Create a feature branch
3. Run `cargo fmt && cargo clippy --all -- -D warnings && cargo test --all`
4. Submit a Pull Request

See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for guidelines.

---

## License

MIT License - see [LICENSE](LICENSE) file.

---

- [Installation Guide](docs/INSTALL.md) | [CLI Reference](docs/CLI.md) | [Clients Manual](docs/CLIENTS.md) | [Architecture](docs/ARCHITECTURE.md) | [Adding Language Support](docs/ADD_LANGUAGE.md) | [GitHub Issues](https://github.com/flashus/detrix/issues)

**Built with Rust for developers and AI agents.**
