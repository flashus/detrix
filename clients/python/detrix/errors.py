"""Error types for Detrix Python client.

This module defines a hierarchy of exceptions for the Detrix client:

    DetrixError (base)
    ├── ConfigError - Configuration/initialization errors
    ├── DaemonError - Communication with daemon failed
    ├── DebuggerError - debugpy-related errors
    └── ControlPlaneError - Control plane server errors

All exceptions inherit from DetrixError, allowing:
    try:
        detrix.wake()
    except DetrixError as e:
        # Catch any Detrix-related error
        ...

For backward compatibility, DaemonConnectionError is kept as an alias
for DaemonError.
"""


class DetrixError(Exception):
    """Base exception for all Detrix client errors.

    All other Detrix exceptions inherit from this class, allowing
    callers to catch all Detrix-related errors with a single except clause.
    """

    pass


class ConfigError(DetrixError):
    """Configuration or initialization error.

    Raised when:
    - Invalid configuration values (ports, URLs)
    - Client already initialized
    - Missing required configuration
    """

    pass


class DaemonError(DetrixError):
    """Error communicating with the Detrix daemon.

    Raised when:
    - Daemon is not reachable (network error)
    - Daemon returns an error response
    - Invalid daemon URL
    - Registration/unregistration fails
    """

    pass


# Backward compatibility alias
DaemonConnectionError = DaemonError


class DebuggerError(DetrixError):
    """Error related to the debugpy debugger.

    Raised when:
    - Failed to load debugpy
    - Failed to start debugpy listener
    - debugpy import error
    """

    pass


class ControlPlaneError(DetrixError):
    """Error related to the control plane HTTP server.

    Raised when:
    - Failed to start control server
    - Control server port conflict
    - Control server thread error
    """

    pass
