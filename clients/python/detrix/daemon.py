"""Daemon client for communicating with Detrix daemon.

This module provides an abstraction layer for communicating with the Detrix
daemon. It defines a DaemonClient Protocol for testability and provides
a default HTTP implementation using httpx.

Example:
    # Using module-level convenience functions
    if check_daemon_health("http://127.0.0.1:8090"):
        connection_id = register_connection(
            daemon_url="http://127.0.0.1:8090",
            host="127.0.0.1",
            port=5678,
            connection_id="my-service-123",
            token=None,
        )

    # Using the client class directly (for custom configuration)
    client = HttpDaemonClient("http://127.0.0.1:8090")
    if client.health_check():
        connection_id = client.register("127.0.0.1", 5678, "my-service-123")
"""

import contextlib
from typing import Protocol, runtime_checkable

import httpx

from .errors import DaemonError

# Backward compatibility: DaemonConnectionError is the historical name
# New code should use DaemonError from errors.py
DaemonConnectionError = DaemonError


@runtime_checkable
class DaemonClient(Protocol):
    """Protocol for daemon communication.

    This protocol defines the interface for communicating with the Detrix
    daemon. It enables dependency injection and easy mocking in tests.

    Implementations:
        - HttpDaemonClient: Default HTTP-based implementation using httpx

    Example for testing:
        class MockDaemonClient:
            def health_check(self, timeout: float = 2.0) -> bool:
                return True

            def register(self, host: str, port: int, connection_id: str,
                        token: Optional[str] = None) -> str:
                return connection_id

            def unregister(self, connection_id: str,
                          token: Optional[str] = None) -> None:
                pass

        # Use in tests
        mock_client = MockDaemonClient()
        assert isinstance(mock_client, DaemonClient)
    """

    def health_check(self, timeout: float = 2.0) -> bool:
        """Check if daemon is reachable and healthy.

        Args:
            timeout: Request timeout in seconds

        Returns:
            True if daemon is healthy, False otherwise
        """
        ...

    def register(
        self,
        host: str,
        port: int,
        connection_id: str,
        token: str | None = None,
        timeout: float = 5.0,
    ) -> str:
        """Register a DAP connection with the daemon.

        Args:
            host: Host where debugpy is listening
            port: Port where debugpy is listening
            connection_id: Unique connection identifier
            token: Optional authentication token
            timeout: Request timeout in seconds

        Raises:
            DaemonError: If daemon is not reachable or registration fails

        Returns:
            Connection ID from daemon response
        """
        ...

    def unregister(
        self,
        connection_id: str,
        token: str | None = None,
        timeout: float = 2.0,
    ) -> None:
        """Unregister connection from daemon.

        Args:
            connection_id: Connection ID to unregister
            token: Optional authentication token
            timeout: Request timeout in seconds

        Note:
            Errors are typically ignored (best effort cleanup).
        """
        ...


class HttpDaemonClient:
    """HTTP-based implementation of DaemonClient using httpx.

    This is the default implementation that communicates with the Detrix
    daemon over HTTP/HTTPS. Uses httpx for:
    - Connection pooling (automatic keep-alive)
    - Better timeout handling
    - HTTP/2 support (when server supports it)

    Args:
        base_url: Base URL of the daemon (e.g., "http://127.0.0.1:8090")
        connect_timeout: Timeout for establishing connections (default: 2.0s)
        default_timeout: Default timeout for requests (default: 5.0s)

    Example:
        client = HttpDaemonClient("http://127.0.0.1:8090")
        if client.health_check():
            conn_id = client.register("127.0.0.1", 5678, "my-service")
        client.close()  # Close connection pool when done

        # Or use as context manager:
        with HttpDaemonClient("http://127.0.0.1:8090") as client:
            if client.health_check():
                conn_id = client.register("127.0.0.1", 5678, "my-service")
    """

    def __init__(
        self,
        base_url: str,
        connect_timeout: float = 2.0,
        default_timeout: float = 5.0,
    ):
        """Initialize the HTTP daemon client.

        Args:
            base_url: Base URL of the daemon (e.g., "http://127.0.0.1:8090")
            connect_timeout: Timeout for establishing connections
            default_timeout: Default timeout for requests
        """
        self.base_url = base_url.rstrip("/")
        self._connect_timeout = connect_timeout
        self._default_timeout = default_timeout
        # Create a client with connection pooling
        self._client = httpx.Client(
            base_url=self.base_url,
            timeout=httpx.Timeout(default_timeout, connect=connect_timeout),
        )

    def __enter__(self) -> "HttpDaemonClient":
        """Context manager entry."""
        return self

    def __exit__(self, *args: object) -> None:
        """Context manager exit - close the client."""
        self.close()

    def close(self) -> None:
        """Close the HTTP client and release connections."""
        self._client.close()

    def health_check(self, timeout: float = 2.0) -> bool:
        """Check if daemon is reachable and healthy.

        Args:
            timeout: Request timeout in seconds

        Returns:
            True if daemon is healthy, False otherwise
        """
        try:
            response = self._client.get("/health", timeout=timeout)
            if response.status_code == 200:
                data = response.json()
                return bool(data.get("service") == "detrix")
        except (httpx.HTTPError, ValueError):
            # ValueError from json parsing, HTTPError from network issues
            pass
        return False

    def register(
        self,
        host: str,
        port: int,
        connection_id: str,
        token: str | None = None,
        timeout: float = 5.0,
    ) -> str:
        """Register a DAP connection with the daemon.

        Args:
            host: Host where debugpy is listening
            port: Port where debugpy is listening
            connection_id: Unique connection identifier
            token: Optional authentication token
            timeout: Request timeout in seconds

        Raises:
            DaemonError: If daemon is not reachable or registration fails

        Returns:
            Connection ID from daemon response
        """
        payload = {
            "host": host,
            "port": port,
            "language": "python",
            "connectionId": connection_id,
        }

        headers = {}
        if token:
            headers["Authorization"] = f"Bearer {token}"

        try:
            response = self._client.post(
                "/api/v1/connections",
                json=payload,
                headers=headers,
                timeout=timeout,
            )
            response.raise_for_status()
            data = response.json()
            result: str = data.get("connectionId", connection_id)
            return result
        except httpx.HTTPStatusError as e:
            raise DaemonError(
                f"Failed to register connection: HTTP {e.response.status_code} - "
                f"{e.response.reason_phrase}"
            ) from e
        except httpx.ConnectError as e:
            raise DaemonError(
                f"Cannot connect to daemon at {self.base_url}: {e}"
            ) from e
        except httpx.HTTPError as e:
            raise DaemonError(
                f"Cannot connect to daemon at {self.base_url}: {e}"
            ) from e

    def unregister(
        self,
        connection_id: str,
        token: str | None = None,
        timeout: float = 2.0,
    ) -> None:
        """Unregister connection from daemon.

        Args:
            connection_id: Connection ID to unregister
            token: Optional authentication token
            timeout: Request timeout in seconds

        Note:
            Errors are ignored (best effort cleanup).
        """
        headers = {}
        if token:
            headers["Authorization"] = f"Bearer {token}"

        # Best effort - ignore errors during cleanup
        with contextlib.suppress(httpx.HTTPError):
            self._client.delete(
                f"/api/v1/connections/{connection_id}",
                headers=headers,
                timeout=timeout,
            )


# Module-level convenience functions that use HttpDaemonClient
# These maintain backward compatibility with existing code
# Note: These create a new client for each call. For repeated calls,
# prefer using HttpDaemonClient directly for connection reuse.


def check_daemon_health(daemon_url: str, timeout: float = 2.0) -> bool:
    """Check if daemon is reachable and healthy.

    This is a convenience wrapper around HttpDaemonClient.health_check().

    Args:
        daemon_url: Base URL of the daemon (e.g., http://127.0.0.1:8090)
        timeout: Request timeout in seconds

    Returns:
        True if daemon is healthy, False otherwise
    """
    with HttpDaemonClient(daemon_url) as client:
        return client.health_check(timeout=timeout)


def register_connection(
    daemon_url: str,
    host: str,
    port: int,
    connection_id: str,
    token: str | None,
    timeout: float = 5.0,
) -> str:
    """Register a DAP connection with the daemon.

    This is a convenience wrapper around HttpDaemonClient.register().

    Args:
        daemon_url: Base URL of the daemon
        host: Host where debugpy is listening
        port: Port where debugpy is listening
        connection_id: Unique connection identifier
        token: Optional authentication token
        timeout: Request timeout in seconds

    Raises:
        DaemonError: If daemon is not reachable or registration fails

    Returns:
        Connection ID from daemon response
    """
    with HttpDaemonClient(daemon_url) as client:
        return client.register(host, port, connection_id, token, timeout)


def unregister_connection(
    daemon_url: str,
    connection_id: str,
    token: str | None,
    timeout: float = 2.0,
) -> None:
    """Unregister connection from daemon.

    This is a convenience wrapper around HttpDaemonClient.unregister().

    Args:
        daemon_url: Base URL of the daemon
        connection_id: Connection ID to unregister
        token: Optional authentication token
        timeout: Request timeout in seconds

    Note:
        Errors are ignored (best effort cleanup).
    """
    with HttpDaemonClient(daemon_url) as client:
        client.unregister(connection_id, token, timeout)
