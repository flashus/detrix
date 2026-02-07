# Detrix Rust Client

Debug-on-demand observability library for Rust applications. Enables AI-powered debugging of running Rust processes without code modifications or restarts.

## Features

- **Zero overhead when sleeping**: No debugger loaded until explicitly woken
- **No code changes required**: Add metrics to any line of code at runtime
- **Production-safe**: Non-breaking observation points (logpoints) that don't pause execution
- **Clean lifecycle**: Unlike Python's debugpy, LLDB can be fully stopped on sleep

## Quick Start

```rust
use detrix::{self, Config};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize client (starts control plane, stays SLEEPING)
    detrix::init(Config {
        name: Some("my-service".to_string()),
        ..Config::default()
    })?;

    // Your application code runs normally with zero overhead...

    // When debugging is needed (could be months later):
    // POST http://127.0.0.1:{control_port}/detrix/wake
    // → Client spawns lldb-dap and attaches to self
    // → Registers connection with Detrix daemon
    // → AI can now add metrics and observe values

    // Cleanup on shutdown
    detrix::shutdown()?;
    Ok(())
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Rust Application Process                                           │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ Application Code                                                 ││
│  │   use detrix;                                                   ││
│  │   detrix::init(Config::default())?;                             ││
│  └─────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ Control Plane HTTP Server (background thread)                   ││
│  │   - Exposes /detrix/* endpoints                                 ││
│  │   - Manages lldb-dap process lifecycle                          ││
│  └─────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ LLDB Manager                                                     ││
│  │   - Spawns: lldb-dap --connection listen://host:port            ││
│  │   - Sends DAP attach request with PID                           ││
│  │   - Monitors process health                                      ││
│  └─────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
        │
        │ ptrace attach (OS-level)
        ↓
┌─────────────────────────────────────────────────────────────────────┐
│  lldb-dap Process (SEPARATE process)                                │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │ DAP Server                                                       ││
│  │   - Listens on debug_port                                       ││
│  │   - Accepts connections from Detrix daemon                      ││
│  │   - Sets logpoints for metrics                                  ││
│  └─────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
```

## API

### Initialization

```rust
use detrix::{self, Config};
use std::time::Duration;

// With defaults
detrix::init(Config::default())?;

// With custom configuration
detrix::init(Config {
    name: Some("my-service".to_string()),
    control_host: "127.0.0.1".to_string(),
    control_port: 0,  // 0 = auto-assign
    debug_port: 0,    // 0 = auto-assign
    daemon_url: "http://127.0.0.1:8090".to_string(),
    lldb_dap_path: None,  // searches PATH
    detrix_home: None,
    safe_mode: false,
    health_check_timeout: Duration::from_secs(2),
    register_timeout: Duration::from_secs(5),
    unregister_timeout: Duration::from_secs(2),
    lldb_start_timeout: Duration::from_secs(10),
})?;
```

### Status

```rust
let status = detrix::status();
println!("State: {}", status.state);
println!("Control port: {}", status.control_port);
if let Some(ref conn_id) = status.connection_id {
    println!("Connection ID: {}", conn_id);
}
```

### Wake/Sleep (programmatic)

```rust
// Wake: start debugger and register with daemon
let response = detrix::wake()?;
println!("Debug port: {}", response.debug_port);

// Or wake with daemon URL override
let response = detrix::wake_with_url("http://192.168.1.100:8090")?;

// Sleep: stop debugger and unregister
let response = detrix::sleep()?;
```

### Shutdown

```rust
// Cleanup on application exit
detrix::shutdown()?;
```

## Control Plane Endpoints

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/detrix/health` | GET | No | Health check (always 200 OK) |
| `/detrix/status` | GET | Yes* | Current state, ports, connection info |
| `/detrix/info` | GET | Yes* | App metadata (name, PID, Rust version) |
| `/detrix/wake` | POST | Yes* | Start debugger, register with daemon |
| `/detrix/sleep` | POST | Yes* | Stop debugger, unregister |

*Localhost bypass: 127.0.0.1 and ::1 always authorized

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DETRIX_CLIENT_NAME` | Connection name | `detrix-client-{pid}` |
| `DETRIX_DAEMON_URL` | Daemon URL | `http://127.0.0.1:8090` |
| `DETRIX_CONTROL_HOST` | Control plane bind host | `127.0.0.1` |
| `DETRIX_CONTROL_PORT` | Control plane port | `0` (auto) |
| `DETRIX_DEBUG_PORT` | Debug adapter port | `0` (auto) |
| `DETRIX_TOKEN` | Auth token for remote access | - |
| `DETRIX_LLDB_DAP_PATH` | Path to lldb-dap binary | searches PATH |

## Requirements

### lldb-dap

The client requires `lldb-dap` (formerly `lldb-vscode`) to be installed:

**macOS:**
```bash
# Via Homebrew LLVM
brew install llvm
# Binary at: /opt/homebrew/opt/llvm/bin/lldb-dap

# Or via Xcode Command Line Tools
xcode-select --install
# Binary at: xcrun -f lldb-dap
```

**Linux:**
```bash
# Ubuntu/Debian
sudo apt install lldb

# Binary at: /usr/bin/lldb-dap
```

### Debug Symbols

For useful debugging, compile with debug symbols:

```toml
# Cargo.toml
[profile.release]
debug = true
```

## Platform Notes

### macOS
- May need to grant "Developer Tools" access in System Preferences
- lldb-dap paths: `/opt/homebrew/opt/llvm/bin/lldb-dap` or via `xcrun -f lldb-dap`

### Linux
- ptrace_scope=1 works (lldb-dap spawned as child process)
- Check: `cat /proc/sys/kernel/yama/ptrace_scope`

## License

MIT
