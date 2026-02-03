"""Control plane HTTP server for Detrix Python client."""

import hmac
import json
import logging
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from ._generated import (
    ErrorResponse,
    HealthResponse,
    InfoResponse,
    Service,
    Status,
    StatusResponse,
)
from ._operations import do_sleep, do_wake
from ._state import get_state
from .auth import discover_auth_token, is_localhost
from .debugger import get_python_info
from .errors import DaemonError, DebuggerError

_logger = logging.getLogger("detrix.control")

# Maximum request body size (1MB) to prevent DoS via large payloads
MAX_BODY_SIZE = 1_048_576


class ControlHandler(BaseHTTPRequestHandler):
    """HTTP request handler for client control plane."""

    def log_message(self, format: str, *args: Any) -> None:
        """Suppress default logging."""
        pass

    def _send_json_response(self, data: dict[str, Any], status: int = 200) -> None:
        """Send a JSON response."""
        body = json.dumps(data).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_error_response(self, message: str, status: int = 400) -> None:
        """Send an error response."""
        self._send_json_response(ErrorResponse(error=message).model_dump(), status)

    def _check_auth(self) -> bool:
        """Check authorization for requests to the control plane.

        Security Model:
            - Localhost requests (127.0.0.1, ::1, localhost) are ALWAYS allowed
              without authentication. This is safe because only local processes
              can access these addresses.
            - Remote requests require a valid Bearer token that matches the
              token configured in DETRIX_TOKEN or ~/detrix/mcp-token.
            - If no token is configured AND the request is from a non-localhost
              address, access is DENIED. This is a change from the previous
              behavior which allowed all access when no token was set.

        Design Rationale:
            The control plane exposes sensitive operations (wake, sleep) that
            can enable debugging of the process. While localhost access is
            safe (only the local machine), remote access should require
            explicit authentication to prevent unauthorized debugging.

        Returns:
            True if authorized, False otherwise
        """
        # Get client address
        client_host = self.client_address[0] if self.client_address else "unknown"

        # Localhost requests are ALWAYS allowed - this is the primary use case
        # for the control plane and is safe since only local processes can connect
        if is_localhost(client_host):
            return True

        # Non-localhost requests require a valid token
        state = get_state()
        detrix_home = Path(state.detrix_home) if state.detrix_home else None
        token = discover_auth_token(detrix_home)

        # If no token is configured for remote access, deny by default
        # This is a security-conscious default: remote debugging requires explicit setup
        if not token:
            return False

        auth_header = self.headers.get("Authorization", "")
        if auth_header.startswith("Bearer "):
            provided_token = auth_header[7:]
            # Use constant-time comparison to prevent timing attacks
            return hmac.compare_digest(provided_token.encode(), token.encode())

        return False

    def _read_json_body(self) -> dict[str, Any] | None:
        """Read and parse JSON body from request.

        Returns None for:
        - Missing/invalid Content-Length header
        - Negative Content-Length (malformed request)
        - Content-Length exceeding MAX_BODY_SIZE (DoS protection)
        - JSON decode errors
        """
        try:
            content_length = int(self.headers.get("Content-Length", 0))
        except (ValueError, TypeError):
            return None

        if content_length <= 0:
            return None
        if content_length > MAX_BODY_SIZE:
            return None

        try:
            body = self.rfile.read(content_length)
            result: dict[str, Any] = json.loads(body.decode("utf-8"))
            return result
        except (json.JSONDecodeError, UnicodeDecodeError):
            return None

    def do_GET(self) -> None:
        """Handle GET requests."""
        path = urlparse(self.path).path

        if path == "/detrix/health":
            self._handle_health()
        elif path == "/detrix/status":
            if not self._check_auth():
                self._send_error_response("Unauthorized", 401)
                return
            self._handle_status()
        elif path == "/detrix/info":
            if not self._check_auth():
                self._send_error_response("Unauthorized", 401)
                return
            self._handle_info()
        else:
            self._send_error_response("Not found", 404)

    def do_POST(self) -> None:
        """Handle POST requests."""
        path = urlparse(self.path).path

        if not self._check_auth():
            self._send_error_response("Unauthorized", 401)
            return

        if path == "/detrix/wake":
            self._handle_wake()
        elif path == "/detrix/sleep":
            self._handle_sleep()
        else:
            self._send_error_response("Not found", 404)

    def _handle_health(self) -> None:
        """Handle /detrix/health endpoint."""
        response = HealthResponse(status=Status.ok, service=Service.detrix_client)
        self._send_json_response(response.model_dump())

    def _handle_status(self) -> None:
        """Handle /detrix/status endpoint."""
        state = get_state()
        with state.lock:
            response = StatusResponse(
                state=state.state.value,  # type: ignore[arg-type]
                name=state.name,
                control_host=state.control_host,
                control_port=state.actual_control_port,
                debug_port=state.actual_debug_port,
                debug_port_active=state.debug_port_active,
                daemon_url=state.daemon_url,  # type: ignore[arg-type]
                connection_id=state.connection_id,
            )
            self._send_json_response(response.model_dump(mode="json"))

    def _handle_info(self) -> None:
        """Handle /detrix/info endpoint."""
        state = get_state()
        python_info = get_python_info()
        with state.lock:
            response = InfoResponse(
                name=state.name,
                pid=os.getpid(),
                python_version=python_info["python_version"],
                python_executable=python_info["python_executable"],
            )
            self._send_json_response(response.model_dump())

    def _handle_wake(self) -> None:
        """Handle /detrix/wake endpoint.

        Delegates to shared do_wake() operation and converts result to HTTP response.
        """
        # Parse optional body for daemon_url override
        body = self._read_json_body()
        daemon_url = None
        if body and isinstance(body, dict):
            daemon_url = body.get("daemon_url")

        try:
            response = do_wake(daemon_url)
            self._send_json_response(response.model_dump())
        except DaemonError as e:
            self._send_error_response(str(e), 503)
        except DebuggerError as e:
            self._send_error_response(str(e), 500)
        except Exception as e:
            self._send_error_response(f"Wake failed: {e}", 500)

    def _handle_sleep(self) -> None:
        """Handle /detrix/sleep endpoint.

        Delegates to shared do_sleep() operation and converts result to HTTP response.
        """
        response = do_sleep()
        self._send_json_response(response.model_dump())


class ControlServer:
    """Control plane HTTP server wrapper."""

    def __init__(self, host: str, port: int):
        """Initialize the control server.

        Args:
            host: Host to bind to
            port: Port to bind to (0 for auto-assign)
        """
        self.host = host
        self.port = port
        self.server: HTTPServer | None = None
        self.thread: threading.Thread | None = None
        self.actual_port = 0

    def start(self) -> int:
        """Start the control server in a background thread.

        Returns:
            Actual port the server is listening on
        """
        self.server = HTTPServer((self.host, self.port), ControlHandler)
        self.actual_port = self.server.server_address[1]

        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

        return self.actual_port

    def _serve(self) -> None:
        """Serve requests until shutdown."""
        if self.server:
            self.server.serve_forever()

    def stop(self, timeout: float = 2.0) -> None:
        """Stop the control server.

        Args:
            timeout: Maximum time to wait for the server thread to terminate.
                    If the thread doesn't terminate within this time, a warning
                    is logged but the function returns anyway.
        """
        if self.server:
            self.server.shutdown()
            self.server.server_close()
            self.server = None

        if self.thread:
            self.thread.join(timeout=timeout)
            if self.thread.is_alive():
                _logger.warning(
                    "Control server thread did not terminate within %.1f seconds. "
                    "The thread may still be running in the background.",
                    timeout,
                )
            self.thread = None


# Global server instance
_control_server: ControlServer | None = None


def start_control_server(host: str, port: int) -> int:
    """Start the global control server.

    Args:
        host: Host to bind to
        port: Port to bind to (0 for auto-assign)

    Returns:
        Actual port the server is listening on
    """
    global _control_server
    if _control_server is not None:
        return _control_server.actual_port

    _control_server = ControlServer(host, port)
    return _control_server.start()


def stop_control_server() -> None:
    """Stop the global control server."""
    global _control_server
    if _control_server is not None:
        _control_server.stop()
        _control_server = None
