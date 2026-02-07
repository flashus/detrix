"""Detrix Python client - Debug-on-demand observability with zero overhead.

This client enables Python applications to be observed by the Detrix daemon
without any code modifications or performance overhead when inactive.

Basic usage:

    import detrix

    # Initialize client (starts control plane, stays SLEEPING)
    detrix.init(name="my-service", daemon_url="http://127.0.0.1:8090")

    # ... your application code ...

    # When you need observability:
    detrix.wake()  # Starts debugpy + registers with daemon

    # When done:
    detrix.sleep()  # Stops debugger + unregisters
    detrix.shutdown()  # Cleanup

Known Limitations:
    - debugpy cannot stop its listener once started. Calling sleep() marks the
      client state as sleeping but the debug port remains open. This is a
      limitation of debugpy, not Detrix.
"""

import contextlib
import threading
from typing import Any

from ._generated import StatusResponse
from ._operations import do_sleep, do_wake
from ._state import State, get_state, reset_state
from .config import generate_connection_name, get_env_config
from .control import start_control_server, stop_control_server
from .daemon import DaemonClient, HttpDaemonClient
from .errors import (
    ConfigError,
    ControlPlaneError,
    DaemonConnectionError,  # backward compat alias
    DaemonError,
    DebuggerError,
    DetrixError,
)

__version__ = "0.1.0"
__all__ = [
    "init",
    "status",
    "wake",
    "sleep",
    "shutdown",
    # Errors
    "DetrixError",
    "ConfigError",
    "DaemonError",
    "DaemonConnectionError",  # backward compat alias
    "DebuggerError",
    "ControlPlaneError",
    # Daemon client abstraction
    "DaemonClient",
    "HttpDaemonClient",
]


_initialized = False
_init_lock = threading.Lock()


def init(
    *,
    name: str = "",
    control_host: str = "127.0.0.1",
    control_port: int = 0,
    debug_port: int = 0,
    daemon_url: str = "http://127.0.0.1:8090",
    detrix_home: str | None = None,
    health_check_timeout: float = 2.0,
    register_timeout: float = 5.0,
    unregister_timeout: float = 2.0,
) -> None:
    """Initialize the Detrix client.

    This starts the control plane HTTP server but does NOT contact the daemon.
    The client starts in SLEEPING state.

    Args:
        name: Connection name (default: "detrix-client-{pid}")
        control_host: Host for control plane server (default: 127.0.0.1)
        control_port: Port for control plane (0 = auto-assign)
        debug_port: Port for debugpy (0 = auto-assign when waking)
        daemon_url: URL of the Detrix daemon (default: http://127.0.0.1:8090)
        detrix_home: Path to Detrix home directory (default: ~/detrix)
        health_check_timeout: Timeout for daemon health checks in seconds (default: 2.0)
        register_timeout: Timeout for connection registration in seconds (default: 5.0)
        unregister_timeout: Timeout for connection unregistration in seconds (default: 2.0)

    Environment Variables:
        Timeouts can also be configured via environment variables:
        - DETRIX_HEALTH_CHECK_TIMEOUT
        - DETRIX_REGISTER_TIMEOUT
        - DETRIX_UNREGISTER_TIMEOUT

        Function arguments take precedence over environment variables.

    Raises:
        ConfigError: If client is already initialized or configuration is invalid
    """
    global _initialized

    with _init_lock:
        if _initialized:
            raise ConfigError("Detrix client is already initialized. Call shutdown() first.")

        _init_inner(
            name=name,
            control_host=control_host,
            control_port=control_port,
            debug_port=debug_port,
            daemon_url=daemon_url,
            detrix_home=detrix_home,
            health_check_timeout=health_check_timeout,
            register_timeout=register_timeout,
            unregister_timeout=unregister_timeout,
        )
        _initialized = True


def _init_inner(
    *,
    name: str,
    control_host: str,
    control_port: int,
    debug_port: int,
    daemon_url: str,
    detrix_home: str | None,
    health_check_timeout: float,
    register_timeout: float,
    unregister_timeout: float,
) -> None:
    """Internal init implementation (called under _init_lock)."""
    # Get environment config as fallbacks
    env_config = get_env_config()

    # Resolve configuration with environment fallbacks
    resolved_name = name or env_config.get("name") or ""
    resolved_control_host = control_host or env_config.get("control_host") or "127.0.0.1"
    resolved_daemon_url = daemon_url or env_config.get("daemon_url") or "http://127.0.0.1:8090"

    def resolve_port(param_value: int, env_key: str, env_name: str) -> int:
        """Resolve port value from parameter or environment variable.

        Port 0 is valid and means "auto-assign an available port".
        When port is 0, the system will allocate an ephemeral port
        when the debugger or control server starts.

        Args:
            param_value: Explicit port value (0 for auto-assign, >0 for specific port)
            env_key: Key in env_config dict to check for environment override
            env_name: Environment variable name for error messages

        Returns:
            Resolved port (0-65535), where 0 means auto-assign

        Raises:
            ConfigError: If environment variable contains an invalid port value
        """
        if param_value:
            return param_value
        env_val = env_config.get(env_key)
        if env_val:
            try:
                port_val = int(env_val)
                if not (0 <= port_val <= 65535):
                    raise ValueError(f"Port must be 0-65535, got {port_val}")
                return port_val
            except ValueError as e:
                raise ConfigError(f"Invalid {env_name}: {env_val!r}") from e
        return 0

    resolved_control_port = resolve_port(
        control_port, "control_port", "DETRIX_CONTROL_PORT"
    )
    resolved_debug_port = resolve_port(
        debug_port, "debug_port", "DETRIX_DEBUG_PORT"
    )

    # Resolve timeout configuration
    def resolve_timeout(
        param_value: float, env_key: str, env_name: str, default: float
    ) -> float:
        """Resolve timeout value from parameter or environment variable.

        Args:
            param_value: Explicit timeout value in seconds (must be positive)
            env_key: Key in env_config dict to check for environment override
            env_name: Environment variable name for error messages
            default: Default timeout value if not specified

        Returns:
            Resolved timeout in seconds (positive float)

        Raises:
            ConfigError: If timeout value is not positive
        """
        # If parameter differs from default, use it (explicit override)
        if param_value != default:
            if param_value <= 0:
                raise ConfigError(f"Timeout must be positive, got {param_value}")
            return param_value
        # Otherwise check environment
        env_val = env_config.get(env_key)
        if env_val:
            try:
                timeout_val = float(env_val)
                if timeout_val <= 0:
                    raise ValueError(f"Timeout must be positive, got {timeout_val}")
                return timeout_val
            except ValueError as e:
                raise ConfigError(f"Invalid {env_name}: {env_val!r}") from e
        return default

    resolved_health_timeout = resolve_timeout(
        health_check_timeout, "health_check_timeout", "DETRIX_HEALTH_CHECK_TIMEOUT", 2.0
    )
    resolved_register_timeout = resolve_timeout(
        register_timeout, "register_timeout", "DETRIX_REGISTER_TIMEOUT", 5.0
    )
    resolved_unregister_timeout = resolve_timeout(
        unregister_timeout, "unregister_timeout", "DETRIX_UNREGISTER_TIMEOUT", 2.0
    )

    # Generate connection name
    connection_name = generate_connection_name(resolved_name)

    # Update global state
    state = get_state()
    with state.lock:
        state.name = connection_name
        state.control_host = resolved_control_host
        state.control_port = resolved_control_port
        state.debug_port = resolved_debug_port
        state.daemon_url = resolved_daemon_url
        state.detrix_home = detrix_home
        state.health_check_timeout = resolved_health_timeout
        state.register_timeout = resolved_register_timeout
        state.unregister_timeout = resolved_unregister_timeout
        state.verify_ssl = env_config.get("verify_ssl", True)  # type: ignore[assignment]
        state.ca_bundle = env_config.get("ca_bundle")  # type: ignore[assignment]
        state.state = State.SLEEPING

    # Start control server
    actual_port = start_control_server(resolved_control_host, resolved_control_port)
    with state.lock:
        state.actual_control_port = actual_port


def status() -> dict[str, Any]:
    """Get the current client status.

    Returns:
        Dictionary with state, ports, connection_id, daemon_url, debug_port_active
    """
    state = get_state()
    with state.lock:
        response = StatusResponse(
            state=state.state.value,  # type: ignore[arg-type]
            name=state.name,
            control_host=state.control_host,
            control_port=state.actual_control_port,
            debug_port=state.actual_debug_port,
            debug_port_active=state.debug_port_active,
            daemon_url=state.daemon_url,
            connection_id=state.connection_id,
        )
        return response.model_dump(mode="json")


def wake(daemon_url: str | None = None) -> dict[str, Any]:
    """Start the debugger and register with the daemon.

    This starts debugpy listening for connections and registers
    the connection with the Detrix daemon.

    Thread Safety:
        Uses a lock-free pattern with WAKING transitional state. The main lock
        is held only briefly for state reads/writes. Network I/O is performed
        outside the lock so status() calls don't block. A separate wake_lock
        prevents concurrent wake attempts.

    Args:
        daemon_url: Override daemon URL (optional)

    Raises:
        DaemonError: If daemon is not reachable or URL is invalid
        DebuggerError: If debugpy fails to start

    Returns:
        Dictionary with status, debug_port, connection_id
    """
    return do_wake(daemon_url).model_dump()


def sleep() -> dict[str, Any]:
    """Stop the debugger and unregister from the daemon.

    This transitions to SLEEPING state. The connection is
    unregistered from the daemon (best effort).

    Note: Due to debugpy limitations, the debug port remains open even after
    sleep(). The debug_port_active field in state continues to be True because
    debugpy cannot stop its listener. See debugger.py for details.

    Returns:
        Dictionary with status
    """
    return do_sleep().model_dump()


def shutdown() -> None:
    """Shutdown the client and cleanup resources.

    This stops the control server and resets all state.
    Call init() again to reinitialize.
    """
    global _initialized

    with _init_lock:
        # Sleep first to unregister
        with contextlib.suppress(Exception):
            sleep()

        # Stop control server
        stop_control_server()

        # Reset state
        reset_state()

        _initialized = False
