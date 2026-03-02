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

> **1-minute demo:** An AI agent finds a production bug by observing running code — zero print statements, zero restarts.

Here's what a real debugging session looks like:

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

No code was modified. No restarts. What used to mean edit → rebuild → redeploy → reproduce is now a single conversation — typically 2–3× faster, often more.

You don't need to know the exact line number either — describe the behavior and the agent finds where to look.

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
| Rust | `detrix-rs = "1.1.1"` in Cargo.toml | [Rust Client](clients/rust/README.md) |

> **Production pattern:** Build one service instance with debug symbols and a Detrix client. Route suspect traffic to it via Kafka, a sidecar, or your load balancer. The rest of your fleet runs unaffected — full-speed, no instrumentation overhead. You get deep observability on one instance without touching production.

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
| **Auth** | Bearer token auth (static or JWT/JWKS) — designed to run behind your reverse proxy |
| **Event streaming** | Forward captured events to Graylog |
| **4 API protocols** | MCP (stdio), gRPC, REST, WebSocket |

---

## Documentation

| | |
|---|---|
| [Installation Guide](docs/INSTALL.md) | Install, language setup, agent config, cloud debugging |
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
