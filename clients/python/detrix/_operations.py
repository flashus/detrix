"""Core wake/sleep operations for Detrix Python client.

This module contains the shared implementation of wake/sleep logic used by
both the public API (__init__.py) and the control plane (control.py).
"""

import logging
from pathlib import Path
from urllib.parse import urlparse

from ._generated import SleepResponse, SleepStatus, WakeResponse, WakeStatus
from ._state import State, get_state
from .auth import discover_auth_token
from .daemon import check_daemon_health, register_connection, unregister_connection
from .debugger import sleep_debugger, wake_debugger
from .errors import DaemonError, DebuggerError

_logger = logging.getLogger("detrix.operations")


def do_wake(daemon_url: str | None = None, validate_url: bool = True) -> WakeResponse:
    """Execute the wake operation.

    This is the core wake logic shared by both the public API and control plane.

    Args:
        daemon_url: Override daemon URL (optional)
        validate_url: Whether to validate the daemon URL format (default: True).
                     Set to False when called from control plane where URL
                     validation may have different requirements.

    Returns:
        WakeResponse with status, debug_port, and connection_id

    Raises:
        DaemonError: If daemon is not reachable or URL is invalid
        DebuggerError: If debugpy fails to start
    """
    state = get_state()

    # Acquire wake lock (blocks until available, like Go/Rust clients)
    with state.wake_lock:
        # Phase 1: Read state and validate (short lock)
        with state.lock:
            if state.state == State.AWAKE:
                return WakeResponse(
                    status=WakeStatus.already_awake,
                    debug_port=state.actual_debug_port,
                    connection_id=state.connection_id or "",
                )
            if state.state == State.WAKING:
                # Shouldn't happen since we hold wake_lock, but be safe
                raise DebuggerError("Wake already in progress")

            # Determine daemon URL
            effective_daemon_url = daemon_url or state.daemon_url
            if not effective_daemon_url:
                raise DaemonError("No daemon_url configured")

            # Validate daemon URL format
            if validate_url:
                parsed = urlparse(effective_daemon_url)
                if not parsed.scheme or not parsed.netloc:
                    raise DaemonError(
                        f"Invalid daemon_url format: {effective_daemon_url!r}. "
                        "Expected format: http://host:port"
                    )
                if parsed.scheme not in ("http", "https"):
                    raise DaemonError(
                        f"Invalid daemon_url scheme: {parsed.scheme!r}. "
                        "Must be 'http' or 'https'"
                    )

            # Mark as WAKING so status() shows transitional state
            previous_state = state.state
            state.state = State.WAKING

            # Capture config for use outside lock
            control_host = state.control_host
            debug_port = state.debug_port
            connection_name = state.name
            detrix_home_path = state.detrix_home
            health_timeout = state.health_check_timeout
            reg_timeout = state.register_timeout

        # Phase 2: Network I/O (no lock held - status() won't block)
        try:
            # Check daemon health
            if not check_daemon_health(effective_daemon_url, timeout=health_timeout):
                raise DaemonError(
                    f"Cannot connect to daemon at {effective_daemon_url}"
                )

            # Start debugpy (debugpy handles port=0 atomically)
            success, actual_port = wake_debugger(control_host, debug_port)
            if not success:
                raise DebuggerError("Failed to start debugpy")

            # Get auth token
            detrix_home = Path(detrix_home_path) if detrix_home_path else None
            token = discover_auth_token(detrix_home)

            # Register connection with daemon
            try:
                connection_id = register_connection(
                    daemon_url=effective_daemon_url,
                    host=control_host,
                    port=actual_port,
                    connection_id=connection_name,
                    token=token,
                    timeout=reg_timeout,
                )
            except DaemonError:
                # debugpy started but registration failed
                # Note: sleep_debugger() resets state but cannot stop debugpy server.
                # This is a known debugpy limitation - the server can only be stopped
                # on process exit. The port will remain bound until the process restarts.
                _logger.warning(
                    "Wake failed: debugpy started on port %d but registration failed. "
                    "The debugpy port remains open until process restart (debugpy limitation). "
                    "State has been reset to allow retry.",
                    actual_port,
                )
                sleep_debugger()
                raise

        except Exception:
            # Revert state on any failure
            with state.lock:
                state.state = previous_state
            raise

        # Phase 3: Update state (short lock)
        with state.lock:
            state.state = State.AWAKE
            state.actual_debug_port = actual_port
            state.debug_port_active = True
            state.connection_id = connection_id
            state.daemon_url = effective_daemon_url

        return WakeResponse(
            status=WakeStatus.awake,
            debug_port=actual_port,
            connection_id=connection_id,
        )


def do_sleep() -> SleepResponse:
    """Execute the sleep operation.

    This is the core sleep logic shared by both the public API and control plane.

    Returns:
        SleepResponse with status
    """
    state = get_state()

    # Read state and get info for unregistration (short lock)
    with state.lock:
        if state.state == State.SLEEPING:
            return SleepResponse(status=SleepStatus.already_sleeping)

        is_waking = state.state == State.WAKING

        # Capture info for unregistration
        connection_id = state.connection_id
        daemon_url_for_unreg = state.daemon_url
        detrix_home_str = state.detrix_home
        unreg_timeout = state.unregister_timeout

    # If waking, wait for wake to complete first
    if is_waking:
        with state.wake_lock:
            pass  # Wait for wake to complete

    # Unregister connection (best effort, outside lock)
    if connection_id and daemon_url_for_unreg:
        detrix_home = Path(detrix_home_str) if detrix_home_str else None
        token = discover_auth_token(detrix_home)
        unregister_connection(
            daemon_url=daemon_url_for_unreg,
            connection_id=connection_id,
            token=token,
            timeout=unreg_timeout,
        )

    # Mark debugger as stopped (note: port remains open due to debugpy limitation)
    sleep_debugger()

    # Update state (short lock)
    # Re-read state.state since it may have changed while we were doing I/O
    with state.lock:
        # Another thread may have called sleep() while we were unregistering
        # mypy doesn't understand that state.state can change between lock acquisitions
        current = state.state  # Fresh read
        if current == State.SLEEPING:  # type: ignore[comparison-overlap]
            return SleepResponse(status=SleepStatus.already_sleeping)

        # Note: debug_port_active remains True because debugpy can't close its port
        # Don't reset actual_debug_port - it's still valid if we wake again
        state.state = State.SLEEPING
        state.connection_id = None

    return SleepResponse(status=SleepStatus.sleeping)
