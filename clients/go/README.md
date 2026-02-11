# Detrix Go Client

Go client library for Detrix debug-on-demand observability.

## Installation

```bash
go get github.com/flashus/detrix/clients/go
```

**Prerequisites:**
- Go 1.21+
- Delve debugger (`dlv`) installed and in PATH

Install Delve:
```bash
go install github.com/go-delve/delve/cmd/dlv@latest
```

## Quick Start

```go
package main

import (
    "log"
    detrix "github.com/flashus/detrix/clients/go"
)

func main() {
    // Initialize client (starts control plane, stays SLEEPING)
    err := detrix.Init(detrix.Config{
        Name:      "my-service",
        DaemonURL: "http://127.0.0.1:8090",
    })
    if err != nil {
        log.Fatal(err)
    }
    defer detrix.Shutdown()

    // Your application code...
    runApp()
}
```

## Try It

Run the end-to-end example that simulates an AI agent: starts a sample app, wakes it, adds metrics, and captures events.

```bash
# 1. Start the Detrix server
detrix serve --daemon

# 2. Build the Go fixture app (from clients/go/)
cd ../../fixtures/go && go build -gcflags="all=-N -l" -o detrix_example_app . && cd ../../clients/go

# 3. Run the agent simulation
go run ./examples/test_wake --daemon-port 8090
```

Other examples in `examples/`:

| Example | Description | Run |
|---------|-------------|-----|
| `basic_usage` | Init / wake / sleep cycle | `go run ./examples/basic_usage` |
| `trade_bot` | Long-running app with embedded client | `go run ./examples/trade_bot` |
| `test_wake` | Agent simulation (starts app, wakes, observes) | `go run ./examples/test_wake` |

## API

### `Init(cfg Config) error`

Initialize the Detrix client. Starts the control plane HTTP server in SLEEPING state.

```go
detrix.Init(detrix.Config{
    Name:        "my-service",           // Connection name (default: "detrix-client-{pid}")
    DaemonURL:   "http://127.0.0.1:8090", // Daemon URL
    ControlHost: "127.0.0.1",             // Control plane host
    ControlPort: 0,                       // Control plane port (0 = auto)
    DebugPort:   0,                       // Debug adapter port (0 = auto)
})
```

### `Wake() (WakeResponse, error)`

Start the debugger and register with the daemon.

```go
resp, err := detrix.Wake()
if err != nil {
    log.Printf("Wake failed: %v", err)
}
fmt.Printf("Debug port: %d\n", resp.DebugPort)
```

### `Sleep() (SleepResponse, error)`

Stop the debugger and unregister from the daemon.

```go
resp, err := detrix.Sleep()
// Delve process is fully stopped (unlike Python's debugpy)
```

### `Status() StatusResponse`

Get the current client status.

```go
status := detrix.Status()
fmt.Printf("State: %s, Port: %d\n", status.State, status.ControlPort)
```

### `Shutdown() error`

Stop the client and clean up resources.

```go
detrix.Shutdown()
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DETRIX_NAME` | Connection name | `detrix-client-{pid}` |
| `DETRIX_DAEMON_URL` | Daemon URL | `http://127.0.0.1:8090` |
| `DETRIX_CONTROL_HOST` | Control plane host | `127.0.0.1` |
| `DETRIX_CONTROL_PORT` | Control plane port | `0` (auto) |
| `DETRIX_DEBUG_PORT` | Debug adapter port | `0` (auto) |
| `DETRIX_TOKEN` | Auth token for remote access | - |
| `DETRIX_DELVE_PATH` | Path to dlv binary | searches PATH |
| `DETRIX_HOME` | Detrix home directory | `~/detrix` |
| `DETRIX_HEALTH_CHECK_TIMEOUT` | Health check timeout (seconds) | `2.0` |
| `DETRIX_REGISTER_TIMEOUT` | Registration timeout (seconds) | `5.0` |
| `DETRIX_UNREGISTER_TIMEOUT` | Unregistration timeout (seconds) | `2.0` |

## Control Plane API

The client exposes an HTTP control plane (same as Python client):

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/detrix/health` | GET | No | Health check |
| `/detrix/status` | GET | Yes* | Get current status |
| `/detrix/info` | GET | Yes* | Get process info |
| `/detrix/wake` | POST | Yes* | Start debugger |
| `/detrix/sleep` | POST | Yes* | Stop debugger |

*Localhost requests bypass authentication

## Architecture

Unlike Python's debugpy which runs in-process, Go uses Delve as a **separate process**:

```
┌─────────────────────────────────────┐
│  Go Application                     │
│  ┌─────────────────────────────────┐│
│  │ Detrix Client                   ││
│  │ - Control plane HTTP server     ││
│  │ - Delve process manager         ││
│  └─────────────────────────────────┘│
└─────────────────────────────────────┘
        │ spawns & manages
        ↓
┌─────────────────────────────────────┐
│  Delve Process                      │
│  - Attaches via ptrace              │
│  - DAP server for Detrix daemon     │
└─────────────────────────────────────┘
```

**Advantages over Python:**
- Can fully stop debugger on `Sleep()` (Python's debugpy cannot stop its listener)
- Clean resource management
- Fresh debugger state on each `Wake()`

## Expression Evaluation

### Simple Variables (Recommended)

Go metrics use **logpoints** for fast, non-blocking evaluation:

```go
// Simple variables - RECOMMENDED for production observability
addMetric(connID, file, 100, "symbol", "order_symbol", "go")
addMetric(connID, file, 101, "quantity", "order_qty", "go")
addMetric(connID, file, 102, "user.Name", "username", "go")
addMetric(connID, file, 103, "order.Price", "price", "go")
```

**Characteristics:**
- Non-blocking (no pause in execution)
- Minimal overhead
- Production-safe

### Function Calls (Supported but Blocking)

Detrix supports Go function calls via Delve's `call` command, but with important caveats:

```go
// Function calls - WORK but BLOCK the process
addMetric(connID, file, 111, `len(symbol)`, "symbol_len", "go")
addMetric(connID, file, 112, `user.GetBalance()`, "balance", "go")
addMetric(connID, file, 113, `calculatePnl(entry, current, qty)`, "pnl", "go")
```

**How it works:**
1. Detrix auto-detects function calls in expressions
2. Uses **breakpoint mode** (pauses execution)
3. Adds `call` prefix automatically for Delve DAP
4. Resumes execution after evaluation

**Warnings:**
- **BLOCKS the target process** while function executes
- Delve marks this as "highly EXPERIMENTAL"
- May cause issues with concurrent code
- Better for debugging than production observability

**Critical limitation - Variadic functions NOT supported:**
```go
// ❌ DOES NOT WORK - variadic functions fail with error like:
// "Unable to evaluate expression: can not convert value of type string to []interface {}"
addMetric(connID, file, 111, `fmt.Sprintf("%s x%d", symbol, qty)`, ...)
addMetric(connID, file, 112, `fmt.Println(symbol)`, ...)  // variadic

// ✅ WORKS - non-variadic functions
addMetric(connID, file, 111, `len(symbol)`, ...)           // builtin
addMetric(connID, file, 112, `user.GetBalance()`, ...)     // method
addMetric(connID, file, 113, `math.Sqrt(value)`, ...)      // fixed arity
```

Delve's `call` command cannot handle variadic functions (`...` parameters).
The error is returned in the event value for easy debugging.
See [Delve Issue #2261](https://github.com/go-delve/delve/issues/2261).

**When to use function calls:**
- Calling getters/methods that do simple calculations
- Built-in functions like `len()`, `cap()`
- User-defined functions with fixed parameter counts
- One-off debugging sessions

**Preferred alternative:** Capture variables separately and format in your agent:
```go
// Non-blocking approach - RECOMMENDED
addMetric(connID, file, 111, "symbol", "order_symbol", "go")
addMetric(connID, file, 112, "quantity", "order_qty", "go")
// Format in your analysis tool instead
```

### Mixing Metric Modes

Detrix supports mixing simple variables (logpoint mode) and function calls (breakpoint mode)
in the same file, **as long as they're on different lines**:

```go
// Line 111: Simple variable - uses logpoint mode (non-blocking)
addMetric(connID, file, 111, "symbol", "order_symbol", "go")

// Line 116: Function call - uses breakpoint mode (blocking)
addMetric(connID, file, 116, "len(symbol)", "symbol_len", "go")

// Both work together! Each is processed independently.
```

**How it works:**
1. When a metric is added, Detrix decides mode based on the expression:
   - Simple variable → logpoint (with `logMessage`)
   - Function call OR introspection → breakpoint (no `logMessage`)
2. ALL breakpoints for the file are sent in one `setBreakpoints` request
3. Logpoints capture via DAP `output` events (non-blocking)
4. Breakpoints capture via DAP `stopped` events (pauses briefly)

**Important constraints:**
- ⚠️ Only ONE metric per line (DAP limitation)
- Metrics on the same line: last one wins
- Function calls should be on different lines from simple variables

### Introspection (Breakpoint Mode)

For stack traces and memory snapshots, Detrix uses breakpoint mode:

```go
// With introspection - pauses briefly for data capture
addMetric(connID, file, 120, "pnl", "pnl_value", "go")
// + set captureStackTrace: true or captureMemorySnapshot: true
```

### Technical Reference

- [VSCode Go Issue #100](https://github.com/golang/vscode-go/issues/100) - Function call implementation
- [Delve DAP docs](https://go.googlesource.com/vscode-go/+/HEAD/docs/dlv-dap.md) - Debug console `call` syntax
- [Delve Issue #2261](https://github.com/go-delve/delve/issues/2261) - Variadic function limitations

## Platform Notes

### Linux

May require ptrace permissions:
```bash
# Check current setting
cat /proc/sys/kernel/yama/ptrace_scope

# If value is 1 or higher, Delve spawned by the client can still attach
# because it's a child process. No changes needed.
```

### macOS

May need to grant Developer Tools access in System Preferences → Security & Privacy.

## Testing

```bash
cd clients/go
go test ./...
```

## License

MIT
