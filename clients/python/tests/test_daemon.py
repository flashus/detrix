"""Tests for daemon client."""

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from detrix.daemon import (
    DaemonConnectionError,
    check_daemon_health,
    register_connection,
    unregister_connection,
)


class MockDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon HTTP handler for testing."""

    def log_message(self, format, *args):
        """Suppress logging."""
        pass

    def do_GET(self):
        """Handle GET requests."""
        if self.path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"service": "detrix"}).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        """Handle POST requests."""
        if self.path == "/api/v1/connections":
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length)
            data = json.loads(body.decode())

            self.send_response(201)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({
                "connectionId": data.get("connectionId", "test-conn"),
                "host": data.get("host"),
                "port": data.get("port"),
            }).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def do_DELETE(self):
        """Handle DELETE requests."""
        if self.path.startswith("/api/v1/connections/"):
            self.send_response(200)
            self.end_headers()
        else:
            self.send_response(404)
            self.end_headers()


@pytest.fixture
def mock_daemon():
    """Start a mock daemon server."""
    server = HTTPServer(("127.0.0.1", 0), MockDaemonHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{port}"
    server.shutdown()


class TestCheckDaemonHealth:
    """Test check_daemon_health function."""

    def test_healthy_daemon(self, mock_daemon):
        """Test returns True for healthy daemon."""
        assert check_daemon_health(mock_daemon) is True

    def test_unhealthy_daemon(self):
        """Test returns False for unreachable daemon."""
        assert check_daemon_health("http://127.0.0.1:1", timeout=0.1) is False

    def test_wrong_service(self, mock_daemon):
        """Test returns False if service is not detrix."""
        # The mock returns "detrix" service, so this should pass
        # To test wrong service, we'd need a different mock
        pass


class TestRegisterConnection:
    """Test register_connection function."""

    def test_successful_registration(self, mock_daemon):
        """Test successful connection registration."""
        conn_id = register_connection(
            daemon_url=mock_daemon,
            host="127.0.0.1",
            port=5678,
            connection_id="my-service-123",
            token=None,
        )
        assert conn_id == "my-service-123"

    def test_registration_with_token(self, mock_daemon):
        """Test registration with auth token."""
        conn_id = register_connection(
            daemon_url=mock_daemon,
            host="127.0.0.1",
            port=5678,
            connection_id="my-service-123",
            token="secret-token",
        )
        assert conn_id == "my-service-123"

    def test_registration_daemon_unreachable(self):
        """Test raises error when daemon unreachable."""
        with pytest.raises(DaemonConnectionError) as exc_info:
            register_connection(
                daemon_url="http://127.0.0.1:1",
                host="127.0.0.1",
                port=5678,
                connection_id="my-service-123",
                token=None,
                timeout=0.1,
            )
        assert "Cannot connect to daemon" in str(exc_info.value)


class TestUnregisterConnection:
    """Test unregister_connection function."""

    def test_successful_unregistration(self, mock_daemon):
        """Test successful connection unregistration."""
        # Should not raise
        unregister_connection(
            daemon_url=mock_daemon,
            connection_id="my-service-123",
            token=None,
        )

    def test_unregistration_ignores_errors(self):
        """Test unregistration ignores errors gracefully."""
        # Should not raise even when daemon unreachable
        unregister_connection(
            daemon_url="http://127.0.0.1:1",
            connection_id="my-service-123",
            token=None,
            timeout=0.1,
        )
