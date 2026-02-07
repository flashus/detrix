# Python Client Architecture

This document describes the architecture of the Detrix Python client.

## Module Structure

```
src/detrix/
├── __init__.py      # Public API: init, status, wake, sleep, shutdown
├── _operations.py   # Core wake/sleep operations (shared by __init__ and control)
├── _state.py        # Global state singleton with thread-safe access
├── auth.py          # Token discovery (env var, file)
├── config.py        # Configuration utilities (ports, names, env vars)
├── control.py       # HTTP control plane server
├── daemon.py        # Daemon client protocol and HTTP implementation
├── debugger.py      # debugpy lifecycle management
├── errors.py        # Error type hierarchy
└── py.typed         # PEP 561 marker for type checking
```

## Module Dependencies

```
┌─────────────────┐
│   __init__.py   │  ← Public API (delegates to _operations)
│   control.py    │  ← HTTP server (delegates to _operations)
├─────────────────┤
│  _operations.py │  ← Core wake/sleep logic (shared)
├─────────────────┤
│   daemon.py     │  ← Daemon communication
│   debugger.py   │  ← debugpy management
├─────────────────┤
│   _state.py     │  ← Global state singleton
│   auth.py       │  ← Token handling
│   config.py     │  ← Configuration
│   errors.py     │  ← Error types
└─────────────────┘
```

**Dependency Rules:**
- `__init__.py` imports from `_operations`, `_state`, `config`, `control`, `daemon`, `debugger`, `errors`
- `control.py` imports from `_operations`, `_state`, `auth`, `debugger`, `errors`
- `_operations.py` imports from `_state`, `auth`, `daemon`, `debugger`, `errors`
- `daemon.py` imports only from `errors`
- `debugger.py` has no local imports (only stdlib + debugpy)
- `_state.py`, `auth.py`, `config.py`, `errors.py` have no local imports

## State Machine

```
┌──────────┐               ┌────────┐    success    ┌───────┐
│ SLEEPING │ ──────────► │ WAKING │ ───────────► │ AWAKE │
└──────────┘    wake()     └────────┘              └───────┘
    ▲                          │                      │
    │                          │ failure              │
    │                          └──────────────────────┤
    │                                                 │
    │                               sleep()           │
    └─────────────────────────────────────────────────┘
```

**Note:** WAKING is a transitional state used internally during `wake()` to allow
`status()` calls while network I/O is in progress. It is not exposed in the public API.

### States

| State    | debugpy Loaded | debugpy Listening | Registered with Daemon |
|----------|----------------|-------------------|------------------------|
| SLEEPING | No             | No*               | No                     |
| WAKING   | Yes            | In progress       | In progress            |
| AWAKE    | Yes            | Yes               | Yes                    |

*Note: After first wake, debugpy port remains open due to debugpy limitation.
The `debug_port_active` field tracks the actual port state.

**WAKING** is a transitional state that exists only during the `wake()` call while
network I/O is in progress. This allows `status()` to return immediately without
blocking on daemon communication.

### Transitions

| From     | To       | Trigger        | Actions                                    |
|----------|----------|----------------|--------------------------------------------|
| SLEEPING | WAKING   | wake()         | Load debugpy, mark transitional            |
| WAKING   | AWAKE    | wake() success | Start listener, register with daemon       |
| WAKING   | SLEEPING | wake() failure | Revert to sleeping state                   |
| AWAKE    | SLEEPING | sleep()        | Unregister, mark state (port stays open)   |

## Thread Safety

### Global State Access

All state access must use the `ClientState.lock` (RLock):

```python
state = get_state()
with state.lock:
    # Read or modify state fields
    state.state = State.AWAKE
    state.connection_id = "..."
```

The lock is reentrant to allow nested acquisitions in the same thread.

### Lock-Free Pattern for Network I/O

The `wake()` and `sleep()` functions use a three-phase lock-free pattern to
prevent `status()` calls from blocking during network I/O:

```python
# Phase 1: Read state (short lock)
with state.lock:
    if state.state == State.AWAKE:
        return already_awake_result
    previous_state = state.state
    state.state = State.WAKING  # Transitional state
    # Capture config for use outside lock
    daemon_url = state.daemon_url
    ...

# Phase 2: Network I/O (no lock held)
# status() can return immediately while this runs
check_daemon_health(daemon_url)
register_connection(...)

# Phase 3: Update state (short lock)
with state.lock:
    state.state = State.AWAKE
    state.connection_id = connection_id
```

### Wake Lock

A separate `wake_lock` (non-reentrant Lock) prevents concurrent `wake()` calls:

```python
if not state.wake_lock.acquire(blocking=False):
    # Another thread is waking - wait for it
    with state.wake_lock:
        pass
    # Check result and return
```

This ensures only one wake operation runs at a time, while allowing `status()`
and other read operations to proceed without blocking.

### Control Plane Thread

The HTTP control plane runs in a daemon thread. Request handlers access the
global state through the lock. The daemon thread flag ensures the thread
terminates when the main process exits.

### Lock Acquisition Order

When multiple locks are needed:
1. Try `wake_lock` first (non-blocking)
2. Then acquire `ClientState.lock` briefly for state reads/writes
3. Never hold any lock during network I/O

## Error Hierarchy

```
DetrixError (base)
├── ConfigError        # Configuration/initialization errors
├── DaemonError        # Communication with daemon failed
├── DebuggerError      # debugpy-related errors
└── ControlPlaneError  # Control plane server errors
```

`DaemonConnectionError` is kept as an alias for `DaemonError` for backward
compatibility.

## Control Plane HTTP API

| Endpoint        | Method | Auth Required | Description                |
|-----------------|--------|---------------|----------------------------|
| /detrix/health  | GET    | No            | Health check               |
| /detrix/status  | GET    | Remote only   | Get current status         |
| /detrix/info    | GET    | Remote only   | Get process info           |
| /detrix/wake    | POST   | Remote only   | Start debugger + register  |
| /detrix/sleep   | POST   | Remote only   | Unregister connection      |

### Authentication

- Localhost requests (127.0.0.1, ::1, localhost, 0.0.0.0) are always allowed
- Remote requests require Bearer token matching DETRIX_TOKEN or ~/detrix/mcp-token
- If no token is configured, remote requests are DENIED (security-first default)

## Known Limitations

### 1. debugpy Cannot Stop Listener

**Symptom:** After `sleep()`, the debug port remains open.

**Cause:** debugpy does not support stopping its listener once started. The
`listen()` function can only be called once per process.

**Impact:**
- `debug_port_active` remains True after sleep
- Same port is reused on subsequent wake calls
- Port is freed only when process exits

**Reference:** https://github.com/microsoft/debugpy/issues/895

### 2. Port Allocation Race Condition (TOCTOU) - FIXED

**Previous Issue:** `get_free_port()` found a free port, closed the socket, then
passed the port to debugpy. Another process could grab the port in between.

**Fix:** We now pass `debug_port=0` directly to `debugpy.listen()`, which handles
port allocation atomically. The port is assigned by the OS at bind time, with no
window for another process to claim it.

**Note:** `get_free_port()` still exists in `config.py` for other use cases,
but is no longer used in the wake path.

### 3. Control Server Thread Termination

**Symptom:** Control server thread may not terminate immediately on shutdown.

**Cause:** HTTP server shutdown is cooperative. If a request is in progress,
the thread waits for it to complete.

**Impact:** Warning logged if thread doesn't terminate within timeout (default 2s).
Thread is marked as daemon, so it won't prevent process exit.

## Configuration Priority

Configuration values are resolved in this order (first wins):
1. Explicit parameters to `init()`
2. Environment variables (DETRIX_*)
3. Default values

| Parameter             | Env Variable                  | Default                 |
|-----------------------|-------------------------------|-------------------------|
| name                  | DETRIX_NAME                   | "detrix-client-{pid}"   |
| control_host          | DETRIX_CONTROL_HOST           | "127.0.0.1"             |
| control_port          | DETRIX_CONTROL_PORT           | 0 (auto-assign)         |
| debug_port            | DETRIX_DEBUG_PORT             | 0 (auto-assign)         |
| daemon_url            | DETRIX_DAEMON_URL             | "http://127.0.0.1:8090" |
| token                 | DETRIX_TOKEN                  | (from file)             |
| detrix_home           | DETRIX_HOME                   | ~/detrix                |
| health_check_timeout  | DETRIX_HEALTH_CHECK_TIMEOUT   | 2.0 seconds             |
| register_timeout      | DETRIX_REGISTER_TIMEOUT       | 5.0 seconds             |
| unregister_timeout    | DETRIX_UNREGISTER_TIMEOUT     | 2.0 seconds             |

## DaemonClient Protocol

The `DaemonClient` protocol enables dependency injection and testing:

```python
class DaemonClient(Protocol):
    def health_check(self, timeout: float = 2.0) -> bool: ...
    def register(self, host: str, port: int, connection_id: str,
                 token: Optional[str] = None, timeout: float = 5.0) -> str: ...
    def unregister(self, connection_id: str,
                   token: Optional[str] = None, timeout: float = 2.0) -> None: ...
```

`HttpDaemonClient` is the default implementation using httpx with connection pooling.

## Go/Rust Implementation Notes

When implementing clients in Go or Rust:

1. **Singleton Pattern:** Use package-level state with mutex protection
   - Go: `sync.RWMutex` protecting a struct
   - Rust: `OnceCell<RwLock<ClientState>>`

2. **Error Handling:** Implement equivalent error types with the same hierarchy

3. **HTTP Client:** Use native HTTP clients (net/http, reqwest) implementing
   the same endpoints and error handling

4. **Debug Adapter:** Use language-appropriate DAP libraries (delve for Go,
   lldb-dap/CodeLLDB for Rust)

5. **Thread Safety:** Use native synchronization primitives matching the Python
   RLock semantics (reentrant mutex)

6. **Known Limitations:** Document the same limitations - debug adapter port
   persistence is a common issue across DAP implementations
