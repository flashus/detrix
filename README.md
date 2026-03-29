<table style="border:none;">
<tr>
<td width="300">
<img alt="Detrix" src="assets/logo.png" width="300">
</td>
<td>

# Detrix

**Give your AI agent eyes inside any running program.**

- **Watch any variable at any line** — agent sets an observation point in seconds, zero code changes during debugging
- **Local or cloud** — same workflow for Docker containers and remote hosts
- **Python, Go, Rust** — observation points capture values without pausing, without restarting
- **Built for agents** — observe, query, manage observations via natural language; Claude Code, Cursor, Windsurf

</td>
</tr>
</table>

[![Tests](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/flashus/eb80c012d4f6458bb24fb705bdf5ab57/raw/detrix-tests.json)](https://github.com/flashus/detrix/actions/workflows/ci.yml)
[![CI](https://github.com/flashus/detrix/actions/workflows/ci.yml/badge.svg)](https://github.com/flashus/detrix/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/badge/ghcr.io-flashus%2Fdetrix-blue?logo=docker)](https://ghcr.io/flashus/detrix)
[![crates.io](https://img.shields.io/crates/v/detrix-rs.svg)](https://crates.io/crates/detrix-rs)
[![PyPI](https://img.shields.io/pypi/v/detrix-py.svg)](https://pypi.org/project/detrix-py/)
[![Go](https://pkg.go.dev/badge/github.com/flashus/detrix/clients/go.svg)](https://pkg.go.dev/github.com/flashus/detrix/clients/go)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## See It in Action
<video src="https://github.com/user-attachments/assets/2f6cc317-e09b-48ae-a098-d553d59a26e4" controls width="100%"></video>

> **1-minute demo:** A Go service in Docker starts reporting wrong numbers. Nothing in the client code changed. Claude watches it live and finds the cause.

Here's what the session looks like:

```
You:    "The order-service in Docker is reporting wildly inflated revenue.
         Daily total is in the thousands when transactions should be $20-200."

Agent:  → reads the source, spots dead unit-conversion code
        → wakes the service via Detrix
        → observes txn.Amount, txn.Unit, total
          (no line number — find_variable locates it in the running container)

        "Found it. The pricing API changed what it's returning.

        txn.Amount = 8957, txn.Unit = "cents" → should be $89.57, summed as $8957

        In calculateRevenue, the unit field is fetched and discarded:

        unit := txn.Unit
        _ = unit          // ← conversion was intended, never implemented
        total += amount   // ← adds cents as if they were dollars

        This didn't matter before. It matters now."

        → fixed the client code
        → reported the API contract change upstream
```

No code was modified to instrument. No restarts. The old workflow — add a log line, rebuild, redeploy, wait for the bug to reproduce — replaced by watching it live.

You don't need to know the line number either — describe the behavior and the agent finds where to look.

---

## Why Detrix?

You hit a bug. The old workflow: add a `print`, restart, reproduce, remove the print, repeat. If it's in production, redeploy. If it's in a Docker container, get into the container. If it's intermittent, wait.

With Detrix, you just ask the agent. It finds the right line, plants an observation point, and tells you what it sees — live, nothing restarting.

That bug that cost you hours last week — redeploy after redeploy, still can't reproduce — your agent can investigate it in minutes, while your app keeps running.

| | `print()` / `logging` | Detrix |
|---|---|---|
| **Iteration speed** | Hours (edit → rebuild → deploy) | Minutes |
| **Add new observation** | Edit code → restart | Ask the agent — no code, no restart¹ |
| **Production-safe** | Output pollution, perf risk | Non-breaking observation points |
| **Events** | Ephemeral stream | Stored, queryable by metric and time |
| **Capture control** | Every hit, no filtering | Throttle, sample, first-hit, interval |
| **Cleanup** | Manual (easy to forget, ships to prod) | One command — or automatic expiry |
| **Sensitive data** | Secrets can leak via log output | Sensitive-named vars blocked by default; configurable blacklist + whitelist in `detrix.toml` |

> ¹ Embed `detrix.init()` once for zero restarts forever. Or restart once to attach the debugger (`--debugpy`, `dlv`, `lldb-dap`) — from that point on, the agent adds and removes observations without any further restarts.

---

## Quick Start

*Try it in 2 minutes. Your agent handles everything after step 3.*

### 1. Install Detrix

**macOS** (Homebrew):
```bash
brew install flashus/tap/detrix
```

**macOS / Linux** (shell script):
```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/flashus/detrix/releases/latest/download/detrix-installer.sh | sh
```

**Windows** (PowerShell):
```powershell
irm https://github.com/flashus/detrix/releases/latest/download/detrix-installer.ps1 | iex
```

**Docker** (linux/amd64, linux/arm64):
```bash
docker pull ghcr.io/flashus/detrix:latest
```

**Build from source**:
```bash
cargo install --git https://github.com/flashus/detrix detrix
```

Then initialise (creates config and sets up local storage):
```bash
detrix init
```

### 2. Add to your app

One line — the debugger sleeps until your agent needs it, zero overhead when idle:

```python
import detrix
detrix.init(name="my-app")
```

Go and Rust work the same way — see [App Integration](#app-integration).

### 3. Connect your agent

**Claude Code:**
```bash
claude mcp add --scope user detrix -- detrix mcp
```

**Cursor / Windsurf** — add to `.mcp.json` in your project root:
```json
{
  "mcpServers": {
    "detrix": {
      "command": "detrix",
      "args": ["mcp"]
    }
  }
}
```

For cloud setup and other editors, see the [setup guide](docs/INSTALL.md).

That's it. Ask your agent to observe any line in your running app — no restarts, nothing ships to prod.

---

**Alternative: connect without embedding**

Don't want to add a dependency? Start your app directly under a debugger instead:

```bash
# Python
python -m debugpy --listen 127.0.0.1:5678 app.py

# Go
dlv debug --headless --listen=127.0.0.1:5678 --api-version=2 main.go

# Rust
lldb-dap --port 5678
```

Listens on `127.0.0.1` — local only. See the [language setup guide](docs/INSTALL.md#2-start-your-app-with-the-debugger) for remote and Docker.

---

## How It Works

Detrix is a daemon that runs locally or in the cloud and connects your AI agent to any running process via 29 MCP tools. Under the hood, it talks to your app's debugger via the **Debug Adapter Protocol (DAP)**. It sets **logpoints** — breakpoints that evaluate an expression and log the result instead of pausing. Your application runs at full speed; Detrix captures the values.

```
  AI Agent                 Detrix Daemon              Debugger (DAP)         Your App
  (Claude Code, Cursor,    (local or Docker/cloud)    debugpy / dlv /        (Python/Go/Rust,
    Windsurf, local)                                  lldb-dap               local/cloud)
      │                         │                          │                      │
      │── "observe line 127" ──▶│                          │                      │
      │                         │── set logpoint ─────────▶│                      │
      │                         │                          │── captures value ───▶│
      │                         │◀────────────── captured values ─────────────────│
      │◀── structured events ───│                          │                      │
      │                         │                          │                      │
      │         App never pauses. No code changes. No restarts.                   │
```

The daemon runs locally or alongside your service in Docker — same protocol either way. In cloud mode, source files are fetched automatically so the agent can find the right lines without them on your machine. See the [Installation Guide](docs/INSTALL.md#cloud-debugging) for cloud setup.

---

## App Integration

```python
import detrix
detrix.init(name="my-app")   # That's it. Agent controls the rest.
```

| Language | Install | Docs |
|----------|---------|------|
| Python | `pip install detrix-py` | [Python Client](clients/python/README.md) |
| Go | `go get github.com/flashus/detrix/clients/go` | [Go Client](clients/go/README.md) |
| Rust | `detrix-rs = "1.2.0"` in Cargo.toml | [Rust Client](clients/rust/README.md) |

> **Production pattern:** Build one service instance with debug symbols and a Detrix client. Route suspect traffic to it via Kafka, a sidecar, or your load balancer. The rest of your fleet runs unaffected — full-speed, no instrumentation overhead. You get deep observability on one instance without touching production.

See the [Clients Manual](docs/CLIENTS.md) for full documentation.

---

## Build Configuration for Observability

Detrix uses debugger logpoints to observe variables. For expressions to evaluate successfully, the binary needs **debug symbols** (DWARF info). The good news: you don't need a debug build. Each language has a production-friendly strategy that preserves most variable visibility with minimal performance impact.

### Go

Go's default `go build` already produces an **optimized binary with DWARF debug symbols**. Delve attaches to it and evaluates logpoint expressions on most variables — especially heap-allocated struct fields. No special flags needed.

```bash
# This is already debuggable. Nothing to change.
go build -o myservice ./cmd/myservice
```

If the optimizer hides a specific local variable (`<optimized out>`), use **per-package debug flags** to disable optimizations only where you need visibility:

```bash
# Disable optimizations in the payments package only.
# net/http, encoding/json, and all other packages stay fully optimized.
go build -gcflags="myservice/internal/payments=-N -l" -o myservice ./cmd/myservice
```

| Build | Performance | Variable visibility |
|---|---|---|
| `go build` (default) | Full speed | High — struct fields, most locals |
| `go build -gcflags="pkg=-N -l"` | One package slower | ~100% in that package |
| `go build -gcflags="all=-N -l"` | All packages slower | 100% everywhere (dev only) |

**Stripping symbols** (`-ldflags="-s -w"`) removes DWARF info and makes debugging impossible. If you strip for production, keep one instance with symbols for Detrix.

### Rust

Rust debug builds (`cargo build`) are 10-100x slower than release — unusable in production. Instead, use a **custom Cargo profile** that compiles your dependencies at full optimization (O3) and your code at a lower level (O1) that preserves most variable visibility:

```toml
# Cargo.toml — add a "detrix" profile for observable production builds

[profile.detrix]
inherits = "release"
opt-level = 1              # Your code: light optimization, most variables visible
debug = 2                  # Full DWARF debug info
codegen-units = 16         # Default parallelism

[profile.detrix.package."*"]
opt-level = 3              # All dependencies: full optimization
debug = false              # No debug info needed for deps (smaller binary)
```

```bash
cargo build --profile detrix
```

| Profile | Your code | Dependencies | Performance vs release | Variable visibility |
|---|---|---|---|---|
| `release` | O3, no debug | O3, no debug | Baseline | Cannot observe |
| `release` + `debug = 2` | O3, DWARF | O3, no debug | ~0-2% overhead | Low — many locals optimized out |
| **`detrix` (recommended)** | **O1, DWARF** | **O3, no debug** | **~5-15% overhead** | **High — most variables visible** |
| `dev` (debug) | O0, DWARF | O0, DWARF | 10-100x slower | 100% (dev only) |

**Generics caveat:** Generic code from dependencies (serde, tokio, axum) is monomorphized in your crate and compiles at **your** opt-level (O1), not the dependency's (O3). For most services this is negligible since I/O dominates, but serialization-heavy hot paths may see a larger impact.

For functions you want to keep fully visible regardless of optimization:
```rust
#[inline(never)]  // Prevents inlining — always visible as a separate stack frame
fn process_order(order: &Order) -> Result<ProcessedOrder> {
    // Detrix can always set logpoints here
}
```

### Python

No special build configuration needed. CPython is always debuggable — debugpy attaches to the running interpreter as-is.

```python
import detrix
detrix.init(name="my-app")  # Works on any Python 3.10+ installation
```

---

See the [Clients Manual](docs/CLIENTS.md) for full documentation.

---

## Features

**No code changes.** The agent instruments your running code via observation points — nothing gets committed, nothing ships to prod.

**No pausing.** Observation points evaluate expressions at full execution speed, with no breakpoint-style halting. For high-frequency code paths, use sample or throttle modes to control event volume.

**No forgotten cleanup.** Metrics expire automatically via TTL, or remove everything with one command.

| | |
|---|---|
| **Agent tools** | 29 MCP tools — observe any line, query events, enable/disable observation groups, and clean up; no line number needed |
| **Zero-downtime instrumentation** | Add metrics without restarting your app |
| **Multi-variable capture** | Capture multiple variables per observation point |
| **Capture modes** | Stream, sample, throttle, first-hit, periodic sampling (every N sec) |
| **Runtime introspection** | Stack traces, memory snapshots, variable inspection, expression evaluation |
| **Multi-language** | Python (debugpy), Go (delve), Rust (lldb-dap) |
| **Cloud debugging** | Observe Docker containers and remote hosts — no VPN, no port forwarding |
| **Durable storage** | Events stored in SQLite on the daemon host. Run Detrix on a remote server, connect your agent in the morning and ask what happened overnight. Daemon auto-reconnects to the debug adapter if it restarts. |
| **Extensible** | New frontends via open API; new language support by implementing a language adapter — [Adding Languages](docs/ADD_LANGUAGE.md) |
| **Safety validation** | Sensitive variable names (`password`, `api_key`, `token`, `secret`, `private_key`, etc.) blocked before capture. Configurable blacklist + whitelist for variable names and functions in `detrix.toml`. Enable **safe mode** per connection to allow only variable watching — no expression execution, no stack traces, no memory snapshots. Blocked operations return a clear named error so the agent can explain the constraint. |
| **Auth** | Multi-tenant access control: per-user static tokens or JWT/JWKS, role-based authorization (Admin/User), per-agent metric isolation — [Auth Guide](docs/AUTH.md) |
| **Event streaming** | Forward captured events to Graylog |
| **4 API protocols** | MCP (stdio), gRPC, REST, WebSocket |

---

## Documentation

| | |
|---|---|
| [Installation Guide](docs/INSTALL.md) | Install, language setup, agent config, cloud debugging |
| [Authentication](docs/AUTH.md) | Auth modes, per-user tokens, JWT/JWKS, access control |
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

Found a bug? [Open an issue](https://github.com/flashus/detrix/issues).
Found in minutes what took you days? [Tell us in Discussions](https://github.com/flashus/detrix/discussions).
