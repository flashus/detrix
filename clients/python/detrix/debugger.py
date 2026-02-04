"""debugpy lifecycle management for Detrix Python client.

This module manages the debugpy debug adapter for Python. It handles loading
the debugpy module lazily and starting/stopping the debug listener.

IMPORTANT LIMITATION:
    debugpy does not support stopping its listener once started. The
    listen() function can only be called once per process. This is a
    limitation of debugpy itself, not Detrix.

    Implications:
    - sleep_debugger() only marks the client state as sleeping; the actual
      debug port remains open and accepting connections
    - Calling wake_debugger() after sleep_debugger() will return the same
      port that was originally opened
    - The debug port is effectively allocated for the lifetime of the process

    This limitation exists because debugpy's underlying sockets and threads
    cannot be cleanly shut down. See:
    https://github.com/microsoft/debugpy/issues/895
"""

import logging
import sys
import threading
from typing import Any

_logger = logging.getLogger("detrix.debugger")

# Lazy import to avoid loading debugpy until needed
_debugger_lock = threading.Lock()
_debugpy_loaded = False
_debugpy_listening = False
_debugpy_port: int | None = None


def _get_debugpy() -> Any:
    """Lazy import of debugpy."""
    import debugpy
    return debugpy


def wake_debugger(host: str, port: int) -> tuple[bool, int]:
    """Start debugpy listening on specified host/port.

    This transitions to AWAKE state.

    Note: debugpy can only listen once per process. If it's already listening,
    this function returns the existing port.

    Args:
        host: Host to listen on
        port: Port to listen on (0 for auto-assign)

    Returns:
        Tuple of (success, actual_port)
    """
    global _debugpy_loaded, _debugpy_listening, _debugpy_port

    with _debugger_lock:
        try:
            debugpy = _get_debugpy()
            _debugpy_loaded = True

            # Check if already listening (debugpy can only listen once per process)
            if _debugpy_port is not None:
                # Already started in this process, just update state
                _debugpy_listening = True
                return True, _debugpy_port

            # Start listening
            _, actual_port = debugpy.listen((host, port))
            _debugpy_listening = True
            _debugpy_port = actual_port
            return True, actual_port

        except Exception:
            _logger.warning("Failed to start debugpy", exc_info=True)
            return False, 0


def sleep_debugger() -> bool:
    """Mark debugger as not listening (state only, port remains open).

    IMPORTANT: This function does NOT actually stop debugpy's listener.
    debugpy does not support stopping its listener once started. This is
    a limitation of debugpy itself.

    What this function does:
    - Updates the internal state tracking to mark the debugger as "sleeping"
    - The actual debug port (_debugpy_port) REMAINS OPEN and accepting
      connections until the process exits

    What this function does NOT do:
    - Close the debug port
    - Stop the debugpy listener thread
    - Free up the port for other processes

    This behavior is intentional and matches the design of the Detrix client
    state machine. When wake_debugger() is called again, it will return the
    same port that was originally opened.

    Returns:
        True (always succeeds since this only changes internal state)
    """
    global _debugpy_listening
    with _debugger_lock:
        _debugpy_listening = False
    return True


def reset_debugger_state() -> None:
    """Reset debugger state (for testing only).

    Note: This does NOT stop the actual debugpy listener, just resets tracking.
    """
    global _debugpy_loaded, _debugpy_listening, _debugpy_port
    with _debugger_lock:
        _debugpy_loaded = False
        _debugpy_listening = False
        # Don't reset _debugpy_port since debugpy is still actually listening


def is_debugger_loaded() -> bool:
    """Check if debugpy has been loaded.

    Returns:
        True if debugpy is loaded in memory
    """
    with _debugger_lock:
        return _debugpy_loaded


def is_debugger_listening() -> bool:
    """Check if debugpy is currently marked as listening.

    Returns:
        True if debugpy is marked as listening
    """
    with _debugger_lock:
        return _debugpy_listening


def get_python_info() -> dict[str, str]:
    """Get Python interpreter information.

    Returns:
        Dictionary with Python version and executable path
    """
    return {
        "python_version": sys.version,
        "python_executable": sys.executable,
    }
