# Detrix Python Client

Debug-on-demand observability for Python applications with zero overhead when inactive.

## Overview

The Detrix Python client enables your application to be observed by the [Detrix](https://github.com/flashus/detrix) daemon without:

- **Code modifications** - No print statements or logging changes needed
- **Redeployment** - Add metrics to running processes
- **Performance overhead** - Zero cost when not actively observing

## Installation

```bash
# Using uv (recommended)
uv add detrix-py

# Using pip
pip install detrix-py
```

## Quick Start

### 1. Start the Detrix daemon

```bash
detrix serve --daemon
```

### 2. Initialize the client in your application

```python
import detrix

# Initialize client - starts control plane, stays SLEEPING (zero overhead)
detrix.init(
    name="my-service",
    daemon_url="http://127.0.0.1:8090",
)

# Your application code runs normally...
def process_request(request):
    data = transform(request)
    return data
```

### 3. Enable observability when needed

```python
# Wake up - starts debugger, registers with daemon
detrix.wake()

# Now the daemon can set observation points on any line
# No code changes needed!

# When done observing
detrix.sleep()
```

## Try It

Run the end-to-end example that simulates an AI agent: starts a sample app, wakes it, adds metrics, and captures events.

```bash
# 1. Start the Detrix server
detrix serve --daemon

# 2. Run the agent simulation (from clients/python/)
uv run python examples/test_wake.py --daemon-port 8090
```

Other examples in `examples/`:

| Example | Description | Run |
|---------|-------------|-----|
| `basic_usage.py` | Init / wake / sleep cycle | `uv run python examples/basic_usage.py` |
| `trade_bot_detrix.py` | Long-running app with embedded client | `uv run python examples/trade_bot_detrix.py` |
| `test_wake.py` | Agent simulation (starts app, wakes, observes) | `uv run python examples/test_wake.py` |

## API Reference

### `detrix.init(**kwargs)`

Initialize the client. Starts the control plane HTTP server.

**Parameters:**
- `name` (str): Connection name (default: `"detrix-client-{pid}"`)
- `control_host` (str): Control plane host (default: `"127.0.0.1"`)
- `control_port` (int): Control plane port (default: `0` = auto-assign)
- `debug_port` (int): debugpy port (default: `0` = auto-assign on wake)
- `daemon_url` (str): Detrix daemon URL (default: `"http://127.0.0.1:8090"`)
- `start_state` (str): Initial state `"sleeping"` or `"warm"` (default: `"sleeping"`)
- `detrix_home` (str): Path to Detrix home directory (default: `~/detrix`)
- `health_check_timeout` (float): Timeout for daemon health checks in seconds (default: `2.0`)
- `register_timeout` (float): Timeout for connection registration in seconds (default: `5.0`)
- `unregister_timeout` (float): Timeout for connection unregistration in seconds (default: `2.0`)

### `detrix.status() -> dict`

Get current client status.

**Returns:**
```python
{
    "state": "sleeping" | "warm" | "awake",
    "name": "my-service-12345",
    "control_host": "127.0.0.1",
    "control_port": 9000,
    "debug_port": 5678,  # 0 if never awake
    "debug_port_active": True,  # True if debug port is actually open
    "daemon_url": "http://127.0.0.1:8090",
    "connection_id": "my-service-12345",  # None if not awake
}
```

### `detrix.wake(daemon_url: str = None) -> dict`

Start debugger and register with daemon.

**Parameters:**
- `daemon_url` (str): Override daemon URL (optional)

**Raises:**
- `DaemonError`: If daemon is not reachable or URL is invalid

**Returns:**
```python
{"status": "awake", "debug_port": 5678, "connection_id": "my-service-12345"}
```

### `detrix.sleep() -> dict`

Stop debugger and unregister from daemon.

**Returns:**
```python
{"status": "sleeping"}
```

### `detrix.shutdown()`

Stop control server and cleanup. Call `init()` again to reinitialize.

## State Machine

```
┌──────────┐         wake()          ┌───────┐
│ SLEEPING │ ──────────────────────►│ AWAKE │
└──────────┘                         └───────┘
    ▲                                   │
    │              sleep()               │
    └────────────────────────────────────┘
```

- **SLEEPING**: No debugger loaded, zero overhead
- **AWAKE**: debugpy listening, registered with daemon

## Control Plane HTTP Endpoints

The client exposes a local HTTP server for management:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/detrix/health`  | GET | Health check |
| `/detrix/status`  | GET | Get current status |
| `/detrix/info`    | GET | Get process info |
| `/detrix/wake`    | POST | Start debugger + register |
| `/detrix/sleep`   | POST | Stop debugger + unregister |

## Environment Variables

| Variable                        | Description                              |
|---------------------------------|------------------------------------------|
| `DETRIX_NAME`                   | Default connection name                  |
| `DETRIX_CONTROL_HOST`           | Control plane host                       |
| `DETRIX_CONTROL_PORT`           | Control plane port                       |
| `DETRIX_DEBUG_PORT`             | debugpy port                             |
| `DETRIX_DAEMON_URL`             | Daemon URL                               |
| `DETRIX_TOKEN`                  | Authentication token                     |
| `DETRIX_HOME`                   | Detrix home directory                    |
| `DETRIX_HEALTH_CHECK_TIMEOUT`   | Timeout for daemon health checks (secs)  |
| `DETRIX_REGISTER_TIMEOUT`       | Timeout for connection registration (secs)|
| `DETRIX_UNREGISTER_TIMEOUT`     | Timeout for connection unregistration (secs)|

## Security

### Authentication

The control plane HTTP server has a security-first authentication model:

- **Localhost requests** (127.0.0.1, ::1, localhost) are always allowed without authentication
- **Remote requests** require a valid Bearer token

To enable remote access:
1. Set `DETRIX_TOKEN` environment variable, or
2. Create `~/detrix/mcp-token` file with the token

If no token is configured, remote requests are denied by default.

### Remote Exposure Guidelines

The control plane is designed for **localhost access only** by default. If you need to expose it remotely:

1. **Always configure authentication** - Set `DETRIX_TOKEN` or create `~/detrix/mcp-token`
2. **Use a reverse proxy** - Place nginx, HAProxy, or similar in front for:
   - TLS termination (HTTPS)
   - Rate limiting
   - Access logging
   - IP allowlisting
3. **Restrict network access** - Use firewall rules to limit which hosts can connect
4. **Protect the token file** - Ensure `~/detrix/mcp-token` has restrictive permissions (`chmod 600`)

Example nginx configuration:
```nginx
location /detrix/ {
    # Rate limiting
    limit_req zone=detrix burst=10 nodelay;

    # Proxy to control plane
    proxy_pass http://127.0.0.1:9000;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
}
```

**Note:** The control plane exposes debugging capabilities. Unauthorized access could allow an attacker to inspect process state. Always treat it as a sensitive endpoint.

## Error Handling

The client provides a hierarchy of exception types:

```python
from detrix import DetrixError, ConfigError, DaemonError, DebuggerError

try:
    detrix.wake()
except DaemonError as e:
    print(f"Cannot reach daemon: {e}")
except DebuggerError as e:
    print(f"Debugger error: {e}")
except DetrixError as e:
    print(f"General error: {e}")
```

- `DetrixError`: Base class for all Detrix errors
- `ConfigError`: Configuration/initialization errors
- `DaemonError`: Communication with daemon failed
- `DebuggerError`: debugpy-related errors
- `ControlPlaneError`: Control plane server errors

Note: `DaemonConnectionError` is kept as an alias for `DaemonError` for backward compatibility.

## Known Limitations

### 1. debugpy Port Remains Open After Sleep

**Issue:** After calling `sleep()`, the debug port remains open until the process exits.

**Cause:** This is a limitation of debugpy itself - it does not support stopping its listener once started. See [debugpy#895](https://github.com/microsoft/debugpy/issues/895).

**Impact:**
- The `debug_port_active` field in status remains `True` after sleep
- Calling `wake()` after `sleep()` reuses the same port
- The port is only freed when the process terminates

**Workaround:** This is expected behavior. If you need to fully release the port, you must restart the process.

### 2. Port Allocation Race Condition (Fixed)

**Previous issue:** Another process could claim a port between detection and binding.

**Resolution:** The client now passes `port=0` directly to `debugpy.listen()`, which
handles port allocation atomically at bind time. There is no window for another
process to claim the port.

**Note:** If you explicitly specify a port via `debug_port` parameter or
`DETRIX_DEBUG_PORT`, the standard TOCTOU limitation applies (another process
could theoretically bind to that port first). Use `port=0` for guaranteed
atomic allocation.

## Architecture

For detailed architecture documentation, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Development

```bash
# Install dev dependencies
uv sync --all-extras

# Run tests
uv run pytest

# Type check
uv run mypy src/detrix

# Lint
uv run ruff check src/detrix
```

## License

MIT
