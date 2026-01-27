"""Global state and state machine for Detrix Python client."""

import threading
from dataclasses import dataclass, field
from enum import Enum


class State(Enum):
    """Client state machine states.

    State transitions:
        SLEEPING -> WAKING -> AWAKE (via wake())
        AWAKE -> SLEEPING (via sleep())

    The WAKING state is a transitional state that indicates wake() is in progress.
    This allows other threads to check status() without blocking on network I/O.
    """

    SLEEPING = "sleeping"
    WAKING = "waking"  # Transitional state during wake()
    AWAKE = "awake"


@dataclass
class ClientState:
    """Global client state container.

    Attributes:
        state: Current client state (SLEEPING, WAKING, AWAKE)
        name: Connection name for this client
        control_host: Host for the control plane HTTP server
        control_port: Requested control port (0 = auto-assign)
        debug_port: Requested debug port (0 = auto-assign on wake)
        daemon_url: URL of the Detrix daemon
        connection_id: Connection ID from daemon registration (None if not registered)
        actual_control_port: Actual port the control server is listening on
        actual_debug_port: Actual port debugpy is listening on (may remain set after sleep
                          due to debugpy limitation - it cannot stop its listener)
        debug_port_active: True if debugpy port is actually open and accepting connections.
                          This differs from 'state' because debugpy cannot close its port
                          once opened. This field tracks the actual port state.
        detrix_home: Path to Detrix home directory
        health_check_timeout: Timeout for daemon health checks (seconds)
        register_timeout: Timeout for connection registration (seconds)
        unregister_timeout: Timeout for connection unregistration (seconds)
        lock: Thread lock for state access (short-duration only)
        wake_lock: Separate lock for wake operation to prevent concurrent wake attempts

    Thread Safety:
        The `lock` is held only briefly for state reads/writes. Network I/O is performed
        outside the lock to prevent blocking status() calls. The `wake_lock` ensures only
        one thread performs the wake operation at a time.
    """

    state: State = State.SLEEPING
    name: str = ""
    control_host: str = "127.0.0.1"
    control_port: int = 0
    debug_port: int = 0
    daemon_url: str = "http://127.0.0.1:8090"
    connection_id: str | None = None
    actual_control_port: int = 0
    actual_debug_port: int = 0
    debug_port_active: bool = False
    detrix_home: str | None = None
    # Timeout configuration (seconds)
    health_check_timeout: float = 2.0
    register_timeout: float = 5.0
    unregister_timeout: float = 2.0
    lock: threading.RLock = field(default_factory=threading.RLock)
    wake_lock: threading.Lock = field(default_factory=threading.Lock)


# Global singleton state
_client_state: ClientState | None = None


def get_state() -> ClientState:
    """Get the global client state, creating if needed."""
    global _client_state
    if _client_state is None:
        _client_state = ClientState()
    return _client_state


def reset_state() -> None:
    """Reset global state (for testing)."""
    global _client_state
    _client_state = None
