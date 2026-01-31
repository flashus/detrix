"""Integration tests for Detrix Python client."""

import contextlib
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

import detrix
from detrix._state import reset_state
from detrix.errors import ConfigError, DaemonError


class MockDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon HTTP handler for integration tests."""

    registered_connections = {}

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

            conn_id = data.get("connectionId", "test-conn")
            MockDaemonHandler.registered_connections[conn_id] = data

            self.send_response(201)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({
                "connectionId": conn_id,
                "host": data.get("host"),
                "port": data.get("port"),
            }).encode())
        else:
            self.send_response(404)
            self.end_headers()

    def do_DELETE(self):
        """Handle DELETE requests."""
        if self.path.startswith("/api/v1/connections/"):
            conn_id = self.path.split("/")[-1]
            MockDaemonHandler.registered_connections.pop(conn_id, None)
            self.send_response(200)
            self.end_headers()
        else:
            self.send_response(404)
            self.end_headers()


@pytest.fixture
def mock_daemon():
    """Start a mock daemon server."""
    MockDaemonHandler.registered_connections = {}
    server = HTTPServer(("127.0.0.1", 0), MockDaemonHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{port}"
    server.shutdown()


@pytest.fixture(autouse=True)
def cleanup():
    """Clean up client state before and after each test."""
    with contextlib.suppress(Exception):
        detrix.shutdown()
    reset_state()
    yield
    with contextlib.suppress(Exception):
        detrix.shutdown()
    reset_state()


class TestClientLifecycle:
    """Test client lifecycle."""

    def test_init_creates_control_plane(self):
        """Test init starts control plane server."""
        detrix.init(name="test-service")
        status = detrix.status()

        assert status["state"] == "sleeping"
        assert status["name"].startswith("test-service-")
        assert status["control_port"] > 0

    def test_init_twice_raises(self):
        """Test init twice raises error."""
        detrix.init(name="test-1")
        with pytest.raises(ConfigError, match="already initialized"):
            detrix.init(name="test-2")

    def test_init_negative_timeout_raises(self):
        """Test init with negative timeout raises error."""
        with pytest.raises(ConfigError, match="Timeout must be positive"):
            detrix.init(name="test", health_check_timeout=-1.0)

    def test_init_zero_timeout_raises(self):
        """Test init with zero timeout raises error."""
        with pytest.raises(ConfigError, match="Timeout must be positive"):
            detrix.init(name="test", register_timeout=0.0)

    def test_shutdown_allows_reinit(self):
        """Test shutdown allows reinit."""
        detrix.init(name="test-1")
        detrix.shutdown()
        detrix.init(name="test-2")
        status = detrix.status()
        assert status["name"].startswith("test-2-")


class TestAwakeState:
    """Test awake state transitions."""

    def test_wake_without_daemon_fails(self):
        """Test wake without daemon raises error."""
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")

        with pytest.raises(DaemonError, match="Cannot connect to daemon"):
            detrix.wake()

    def test_wake_with_daemon(self, mock_daemon):
        """Test wake with running daemon."""
        detrix.init(name="test", daemon_url=mock_daemon)
        result = detrix.wake()

        assert result["status"] == "awake"
        assert result["debug_port"] > 0
        assert result["connection_id"].startswith("test-")

        # Check daemon received registration
        assert len(MockDaemonHandler.registered_connections) == 1

    def test_wake_idempotent(self, mock_daemon):
        """Test wake is idempotent."""
        detrix.init(name="test", daemon_url=mock_daemon)
        detrix.wake()
        result = detrix.wake()

        assert result["status"] == "already_awake"

    def test_wake_with_daemon_url_override(self, mock_daemon):
        """Test wake with daemon URL override."""
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")  # Wrong URL

        # Override with correct URL
        result = detrix.wake(daemon_url=mock_daemon)
        assert result["status"] == "awake"


class TestSleepState:
    """Test sleep state transitions."""

    def test_sleep_from_sleeping(self):
        """Test sleep when already sleeping."""
        detrix.init(name="test")
        result = detrix.sleep()

        assert result["status"] == "already_sleeping"

    def test_sleep_from_awake(self, mock_daemon):
        """Test sleep from awake state."""
        detrix.init(name="test", daemon_url=mock_daemon)
        detrix.wake()

        # Verify connection registered
        assert len(MockDaemonHandler.registered_connections) == 1

        result = detrix.sleep()
        assert result["status"] == "sleeping"

        # Verify connection unregistered
        assert len(MockDaemonHandler.registered_connections) == 0

        status = detrix.status()
        assert status["state"] == "sleeping"
        assert status["connection_id"] is None


