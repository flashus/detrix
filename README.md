<table style="border:none;">
<tr>
<td width="300">
<img alt="Detrix" src="assets/logo.png" width="300">
</td>
<td>

# Detrix

**Observe any line of running code. No changes, no restarts, production-safe.**

- **Zero-code observability** — Add metrics to any line without redeployment
- **Cloud debugging** — Observe code running in Docker containers and remote hosts
- **Built for AI agents** — 29 MCP tools for Claude Code, Cursor, Windsurf
- **Production-safe** — DAP logpoints capture values without pausing. Python, Go, Rust

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE) [![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/) [![Python](https://img.shields.io/badge/python-debugpy-3776AB.svg)](https://github.com/microsoft/debugpy) [![Go](https://img.shields.io/badge/go-delve-00ADD8.svg)](https://github.com/go-delve/delve) [![Rust](https://img.shields.io/badge/rust-lldb--dap-DEA584.svg)](https://lldb.llvm.org/)

</td>
</tr>
</table>

---

## See It in Action

<video src="https://github.com/user-attachments/assets/186b0cd3-86a2-423c-9ba8-642a9e4095f9" controls width="100%"></video>

> **1-minute demo:** An AI agent finds a production bug by observing running code — zero print statements, zero restarts.

```
You:   "My checkout flow is dropping orders. Can you look at what's
        happening around checkout.py line 127?"

Agent:  → connects to your running app
        → adds an observation point at checkout.py:127
        → captures order.total, order.currency, len(order.items)

        I see 47 events in the last minute. 3 have negative totals —
        all with currency="JPY". The discount is calculated on the
        USD-converted amount but applied to the original JPY amount.
        That's the bug.
```

No code was modified. No restarts. The agent observed the running process, found the bug, and explained it.

---

## Quick Start

### 1. Install Detrix

```bash
git clone https://github.com/flashus/detrix.git
cd detrix && cargo build --release
./target/release/detrix init
```

### 2. Start your app with a debugger

```bash
# Python
python -m debugpy --listen 5678 app.py

# Go
dlv debug --headless --listen=127.0.0.1:5678 --api-version=2 main.go

# Rust
lldb-dap --port 5678
```

### 3. Connect your AI agent

Configure Claude Code, Cursor, or Windsurf — see the [setup guide](docs/INSTALL.md).

That's it. The agent handles everything from here: connecting, adding metrics, querying events, and cleaning up when done.

---

## How It Works

Detrix talks to your app's debugger via the **Debug Adapter Protocol (DAP)**. It sets **logpoints** — breakpoints that evaluate an expression and log the result instead of pausing. Your application runs at full speed; Detrix captures the values.

```
  AI Agent                 Detrix Daemon              Your App
  (Claude Code, Cursor,    (local or Docker/cloud)    (Python/Go/Rust, local/cloud)
    Windsurf, local)
      │                         │                          │
      │── "observe line 127" ──▶│                          │
      │                         │── DAP logpoint ─────────▶│
      │                         │◀─ captured values ───────│
      │◀── structured events ───│                          │
      │                         │                          │
      │   App never pauses. No code changes. No restarts.  │
```

The daemon runs locally or alongside your service in Docker — same protocol either way. In cloud mode, source files are fetched automatically via VFS so the agent can find the right lines without the source on your machine. See the [Installation Guide](docs/INSTALL.md#cloud-debugging) for cloud setup.

---

## Detrix Clients

For production and long-running services, embed the Detrix client directly. Zero overhead when idle — the agent wakes the debugger on demand.

```python
import detrix
detrix.init(name="my-app")   # That's it. Agent controls the rest.
```

| Language | Install | Docs |
|----------|---------|------|
| Python | `pip install detrix-py` | [Python Client](clients/python/README.md) |
| Go | `go get github.com/flashus/detrix/clients/go` | [Go Client](clients/go/README.md) |
| Rust | `detrix-rs = "1.1.1"` in Cargo.toml | [Rust Client](clients/rust/README.md) |

Don't want to embed a client? [Start the debugger manually](#2-start-your-app-with-a-debugger) and let the agent attach directly.

See the [Clients Manual](docs/CLIENTS.md) for full documentation.

---

## Features

| | |
|---|---|
| **Zero-downtime instrumentation** | Add metrics without restarting your app |
| **Cloud debugging** | Observe Docker containers and remote hosts — no VPN, no port forwarding |
| **Multi-expression metrics** | Capture multiple values per observation point |
| **Multi-language** | Python (debugpy), Go (delve), Rust (lldb-dap) |
| **Capture modes** | Stream, sample, throttle, first-hit, time-based |
| **Runtime introspection** | Stack traces, memory snapshots, TTL |
| **Safety validation** | Three-layer expression validation prevents unsafe code |
| **29 MCP tools** | Full AI agent integration via Model Context Protocol |
| **4 API protocols** | MCP (stdio), gRPC, REST, WebSocket |

---

## Documentation

| | |
|---|---|
| [Installation Guide](docs/INSTALL.md) | Setup for Claude Code, Cursor, Windsurf |
| [CLI Reference](docs/CLI.md) | Command-line interface |
| [Clients Manual](docs/CLIENTS.md) | Python, Go, Rust client libraries |
| [Architecture](docs/ARCHITECTURE.md) | Clean Architecture with 13 Rust crates |
| [Adding Languages](docs/ADD_LANGUAGE.md) | Extend Detrix to new languages |

---

## Contributing

```bash
cargo fmt --all && cargo clippy --all -- -D warnings && cargo test --all
```

1. Fork the repository
2. Create a feature branch
3. Run the checks above
4. Submit a Pull Request

---

## License

MIT License — see [LICENSE](LICENSE).

**Built with Rust for developers and AI agents.**
