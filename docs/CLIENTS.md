# Detrix Client SDKs

Unified manual for the Detrix client libraries (Python, Go, Rust).

## Overview

The Detrix clients embed a lightweight **control plane** inside your application. An AI agent (or operator) can wake the debugger on demand, observe values through Detrix, then put it back to sleep -- all without restarting the app or touching its source code.

**When to use the Client vs attach to debugger:**

| | Client Embed | Attach to debugger |
|--|-----------|----------------|
| **Setup** | `detrix.init()` in your app | Start debugger manually |
| **Debugger lifecycle** | Agent manages (wake/sleep) | You manage |
| **Overhead when idle** | Zero -- no debugger loaded | Debugger always running |
| **Best for** | Production, long-running services | Quick debugging sessions |

Embed the client for production and long-running services. Attach to a debugger directly for quick debugging sessions.

## How It Works

```
1. App starts with client → control plane HTTP server is running, debugger is NOT loaded
2. Agent calls POST /detrix/wake → client starts the language debugger, registers with Detrix server
3. Agent adds metrics via Detrix → server sets logpoints through the debugger
4. Agent queries events → captured values flow back through Detrix
5. Agent calls POST /detrix/sleep → client stops the debugger, back to zero overhead
```

```
┌─────────────────────────────────────────────────────┐
│  Your Application                                   │
│  ┌───────────────────────────────────────────────┐  │
│  │ Detrix Client (control plane HTTP server)     │  │
│  │   /detrix/wake  → starts debugger             │  │
│  │   /detrix/sleep → stops debugger              │  │
│  └───────────────────────────────────────────────┘  │
│                    │ spawns                         │
│                    ▼                               │
│  ┌───────────────────────────────────────────────┐  │
│  │ Debugger (debugpy / delve / lldb-dap)         │  │
│  │   DAP server ← Detrix server connects here    │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
         ▲                              │
         │ wake/sleep                    │ DAP protocol
         │                              ▼
┌─────────────┐              ┌─────────────────────┐
│  AI Agent   │◄──────────►│  Detrix Server      │
│  (MCP/REST) │              │  (metrics, events)  │
└─────────────┘              └─────────────────────┘
```

## Installation

### Python

```bash
# Using uv (recommended)
uv add detrix-py

# Using pip
pip install detrix-py
```

**Requires:** Python 3.10+. debugpy is installed automatically as a dependency.

### Go

```bash
go get github.com/flashus/detrix/clients/go
```

**Requires:** Go 1.21+, Delve debugger in PATH:
```bash
go install github.com/go-delve/delve/cmd/dlv@latest
```

### Rust

Add to your `Cargo.toml`:
```toml
[dependencies]
detrix-rs = "1.0.0"
```

**Requires:** `lldb-dap` installed (macOS/Linux). On Windows, Detrix uses CodeLLDB which is downloaded automatically. See [Rust Client README](../clients/rust/README.md#requirements) for install instructions per platform.

**Important:** Compile with debug symbols for useful observation:
```toml
[profile.release]
debug = true
```

## Quick Start

All three clients follow the same pattern: **init** (start control plane) -> **wake** (start debugger) -> observe -> **sleep** (stop debugger).

### Python

```python
import detrix

detrix.init(name="my-service", daemon_url="http://127.0.0.1:8090")

# Your application code runs normally...
# Agent wakes when needed:
detrix.wake()
# ... agent observes via Detrix ...
detrix.sleep()
```

### Go

```go
package main

import (
    "log"
    detrix "github.com/flashus/detrix/clients/go"
)

func main() {
    err := detrix.Init(detrix.Config{
        Name:      "my-service",
        DaemonURL: "http://127.0.0.1:8090",
    })
    if err != nil {
        log.Fatal(err)
    }
    defer detrix.Shutdown()

    // Your application code...
}
```

### Rust

```rust
use detrix_rs::{self as detrix, Config};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    detrix::init(Config {
        name: Some("my-service".to_string()),
        ..Config::default()
    })?;

    // Your application code...
    // Cleanup happens automatically when the process exits.
    // Call detrix::shutdown()? explicitly if you need clean
    // unregistration from the Detrix server before exit.
    Ok(())
}
```

**Note on shutdown:** All three clients provide an explicit `shutdown()` function for clean unregistration from the Detrix server. If the process just exits without calling it, the server will detect the stale connection via health checks. In Go, use `defer detrix.Shutdown()`. In Python, call `detrix.shutdown()` at exit or rely on process termination.

## Configuration

All clients accept the same set of environment variables:

**`DETRIX_NAME`** -- Connection name used to identify this application in the Detrix server. If not set, defaults to `detrix-client-{pid}` where `{pid}` is the process ID. Use a descriptive name like `"order-service"` or `"payment-worker"` so the agent can tell connections apart.

**`DETRIX_DAEMON_URL`** -- URL of the Detrix server (default: `http://127.0.0.1:8090`). The client calls this URL to register/unregister connections during wake/sleep. Must include the scheme (`http://` or `https://`).

**`DETRIX_CONTROL_HOST`** -- Host the control plane HTTP server binds to (default: `127.0.0.1`). Set to `0.0.0.0` to accept remote connections (requires `DETRIX_TOKEN` for security).

**`DETRIX_CONTROL_PORT`** -- Port for the control plane HTTP server (default: `0` = OS auto-assigns a free port). The agent discovers the port from the Detrix server after the client registers.

**`DETRIX_DEBUG_PORT`** -- Port the debugger listens on (default: `0` = OS auto-assigns). This is the port the Detrix server connects to via DAP to set logpoints. Use `0` unless you have a specific reason to fix the port.

**`DETRIX_TOKEN`** -- Bearer token for authenticating remote requests to the control plane. Localhost requests (127.0.0.1, ::1) always bypass authentication. If not set, the client reads `~/detrix/mcp-token` file as fallback. If neither is configured, remote requests are denied.

**`DETRIX_HOME`** -- Path to the Detrix home directory (default: `~/detrix`). Used to locate the `mcp-token` file and other configuration.

**`DETRIX_HEALTH_CHECK_TIMEOUT`** -- Timeout in seconds for the health check call to the Detrix server during wake (default: `2.0`). See [Timeout Workflow](#timeout-workflow) below.

**`DETRIX_REGISTER_TIMEOUT`** -- Timeout in seconds for registering the connection with the Detrix server during wake (default: `5.0`). Longer than the health check because the server may need to initialize the DAP adapter.

**`DETRIX_UNREGISTER_TIMEOUT`** -- Timeout in seconds for unregistering the connection during sleep (default: `2.0`). Best-effort: if it fails, sleep continues and the server detects the stale connection via health checks.

**Language-specific:**

**`DETRIX_DELVE_PATH`** (Go) -- Path to the `dlv` binary. If not set, searches PATH. Install with `go install github.com/go-delve/delve/cmd/dlv@latest`.

**`DETRIX_LLDB_DAP_PATH`** (Rust) -- Path to the `lldb-dap` binary. If not set, searches PATH. On Windows, CodeLLDB is downloaded automatically.

**`DETRIX_VERIFY_SSL`** (Python) -- Whether to verify TLS certificates when communicating with the Detrix server (default: `true`). Set to `false` for self-signed certificates in development.

**`DETRIX_CA_BUNDLE`** (Python) -- Path to a CA bundle file for TLS verification. Use when the Detrix server uses a certificate signed by a private CA.

### Timeout Workflow

The three timeout env vars control specific steps in the wake/sleep lifecycle:

**`wake()` workflow:**

```
1. HEALTH CHECK                          ⏱ DETRIX_HEALTH_CHECK_TIMEOUT (2s)
   Client → GET {daemon_url}/health
   Verifies the Detrix server is reachable before starting a debugger.
   If it times out → DaemonError, wake aborts.
                    │
                    ▼
2. START DEBUGGER                        (no timeout env var)
   Python: debugpy.listen(host, port)
   Go:     spawn dlv attach --pid
   Rust:   spawn lldb-dap --connection
                    │
                    ▼
3. REGISTER CONNECTION                   ⏱ DETRIX_REGISTER_TIMEOUT (5s)
   Client → POST {daemon_url}/api/v1/connections
   Registers the debugger with the Detrix server so it can set logpoints.
   Longer than health check because the server may need to initialize
   the DAP adapter. If it times out → DaemonError, debugger is stopped.
                    │
                    ▼
   State = AWAKE. Agent can now add metrics.
```

**`sleep()` workflow:**

```
1. UNREGISTER CONNECTION                 ⏱ DETRIX_UNREGISTER_TIMEOUT (2s)
   Client → DELETE {daemon_url}/api/v1/connections/{id}
   Best-effort: if it fails, sleep continues anyway.
   The server will detect the stale connection via health checks.
                    │
                    ▼
2. STOP DEBUGGER                         (no timeout env var)
   Go/Rust: kill debugger process.
   Python:  cannot stop debugpy (known limitation).
                    │
                    ▼
   State = SLEEPING. Zero overhead.
```

## State Machine

All clients implement the same two-state lifecycle (plus a transient WAKING state during wake):

```
┌──────────┐         wake()          ┌───────┐
│ SLEEPING │ ──────────────────────► │ AWAKE │
└──────────┘                         └───────┘
    ▲                                    │
    │              sleep()               │
    └────────────────────────────────────┘
```

- **SLEEPING** -- No debugger loaded. Zero CPU/memory overhead. Control plane HTTP server is running.
- **AWAKE** -- Debugger listening on a port, connection registered with Detrix server. Agent can add metrics and capture events.

## Control Plane API

All clients expose the same HTTP endpoints on the control plane:

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/detrix/health` | GET | No | Health check (always 200) |
| `/detrix/status` | GET | Yes* | Current state, ports, connection info |
| `/detrix/info` | GET | Yes* | Process metadata (name, PID, language, version) |
| `/detrix/wake` | POST | Yes* | Start debugger, register with server |
| `/detrix/sleep` | POST | Yes* | Stop debugger, unregister from server |

\* Localhost requests (127.0.0.1, ::1) bypass authentication.

## Security

### Authentication Model

- **Localhost requests** are always allowed without authentication (the common case).
- **Remote requests** require a `Bearer` token in the `Authorization` header.

### Configuring a Token

Set `DETRIX_TOKEN` environment variable, **or** place the token in `~/detrix/mcp-token`:

```bash
echo "my-secret-token" > ~/detrix/mcp-token
chmod 600 ~/detrix/mcp-token
```

If no token is configured, remote requests are denied by default.

### Remote Exposure

The control plane is designed for **localhost access only**. If you need remote access, use a reverse proxy (nginx, HAProxy) with TLS, rate limiting, and IP allowlisting. See the [Python Client README](../clients/python/README.md#remote-exposure-guidelines) for a full nginx example.

## Language-Specific Notes

### Python (debugpy)

- debugpy runs **in-process** -- no separate debugger process.
- **Known limitation:** After `sleep()`, the debug port remains open until the process exits. This is a [debugpy limitation](https://github.com/microsoft/debugpy/issues/895). Subsequent `wake()` calls reuse the same port.

### Go (Delve)

- Delve runs as a **separate process** that attaches via ptrace.
- Can **fully stop** the debugger on `sleep()` (unlike Python).
- **Expression evaluation:** Simple variables use non-blocking logpoints (recommended). Function calls are supported but **block the target process** -- see [Go Client README](../clients/go/README.md#expression-evaluation) for details.
- **Variadic functions** (e.g., `fmt.Sprintf`) are not supported in Delve's `call` command.
- Linux: may require ptrace permissions (child processes work by default with `ptrace_scope=1`).
- macOS: may need Developer Tools access.

### Rust (lldb-dap / CodeLLDB)

- On macOS/Linux: uses lldb-dap as a **separate process** that attaches via ptrace.
- On Windows: uses CodeLLDB (downloaded automatically).
- Can **fully stop** the debugger on `sleep()`.
- **Debug symbols required:** Compile with `[profile.release] debug = true` for useful observation.
- Install lldb-dap: `brew install llvm` (macOS) or `sudo apt install lldb` (Linux).
- macOS: may need Developer Tools access.

## Further Reading

- [Python Client README](../clients/python/README.md) -- full API reference, error handling, security details
- [Go Client README](../clients/go/README.md) -- full API reference, expression evaluation modes, platform notes
- [Rust Client README](../clients/rust/README.md) -- full API reference, lldb-dap setup, platform notes
- [Publishing Guide](PUBLISHING.md) -- how to publish to PyPI, pkg.go.dev, crates.io (maintainers)
- [Architecture](ARCHITECTURE.md) -- Detrix server architecture
