"""Tests for error recovery in the Detrix Python client.

These tests verify that the client handles errors gracefully and can
recover from various failure scenarios.
"""

import contextlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

import detrix
from detrix._state import reset_state
from detrix.errors import ConfigError, DaemonError


class SlowDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon that responds slowly."""

    delay = 5.0  # Delay in seconds (longer than default timeout)

    def log_message(self, format, *args):
        """Suppress logging."""
        pass

    def do_GET(self):
        """Handle GET requests with delay."""
        time.sleep(self.delay)
        if self.path == "/health":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"service": "detrix"}).encode())
        else:
            self.send_response(404)
            self.end_headers()


class ErrorDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon that returns errors."""

    error_code = 500
    error_message = "Internal Server Error"

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
        """Handle POST requests with error."""
        if self.path == "/api/v1/connections":
            self.send_response(ErrorDaemonHandler.error_code)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps({"error": ErrorDaemonHandler.error_message}).encode()
            )
        else:
            self.send_response(404)
            self.end_headers()


class WorkingDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon that works normally."""

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
            self.wfile.write(
                json.dumps({"connectionId": data.get("name", data.get("connectionId", "mock-conn"))}).encode()
            )
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
def working_daemon():
    """Start a working mock daemon server."""
    server = HTTPServer(("127.0.0.1", 0), WorkingDaemonHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{port}"
    server.shutdown()


@pytest.fixture
def error_daemon():
    """Start an error-returning mock daemon server."""
    server = HTTPServer(("127.0.0.1", 0), ErrorDaemonHandler)
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


class TestDaemonConnectionErrors:
    """Test handling of daemon connection errors."""

    def test_wake_after_failed_wake(self, working_daemon):
        """Test wake succeeds after a previous failed wake attempt."""
        # First try to wake with bad daemon URL
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")

        with pytest.raises(DaemonError, match="Cannot connect to daemon"):
            detrix.wake()

        # State should still be sleeping after failed wake
        status = detrix.status()
        assert status["state"] == "sleeping"

        # Now wake with correct daemon URL
        result = detrix.wake(daemon_url=working_daemon)
        assert result["status"] == "awake"

    def test_wake_after_registration_failure(self, error_daemon):
        """Test wake after daemon registration fails."""
        ErrorDaemonHandler.error_code = 500
        ErrorDaemonHandler.error_message = "Registration failed"

        detrix.init(name="test", daemon_url=error_daemon)

        # First wake fails during registration
        # Error can be HTTP status error or connection error depending on timing
        with pytest.raises(DaemonError, match=r"(Failed to register|Cannot connect|HTTP 500)"):
            detrix.wake()

        # State should be sleeping (even though debugpy port may be open)
        status = detrix.status()
        assert status["state"] == "sleeping"

    def test_sleep_handles_daemon_unavailable(self, working_daemon):
        """Test sleep handles daemon being unavailable during unregister."""
        detrix.init(name="test", daemon_url=working_daemon)
        detrix.wake()

        # Simulate daemon going away by changing URL to bad one
        # (unregister is best-effort, should not raise)
        from detrix._state import get_state

        state = get_state()
        with state.lock:
            state.daemon_url = "http://127.0.0.1:1"

        # Sleep should still work (unregister is best effort)
        result = detrix.sleep()
        assert result["status"] == "sleeping"


class TestConfigurationErrors:
    """Test handling of configuration errors."""

    def test_invalid_control_port_from_env(self, monkeypatch):
        """Test handling of invalid control port from environment."""
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "invalid")

        with pytest.raises(ConfigError, match="Invalid DETRIX_CONTROL_PORT"):
            detrix.init(name="test")

    def test_invalid_debug_port_from_env(self, monkeypatch):
        """Test handling of invalid debug port from environment."""
        monkeypatch.setenv("DETRIX_DEBUG_PORT", "not-a-port")

        with pytest.raises(ConfigError, match="Invalid DETRIX_DEBUG_PORT"):
            detrix.init(name="test")

    def test_port_out_of_range(self, monkeypatch):
        """Test handling of port number out of range."""
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "70000")

        with pytest.raises(ConfigError, match="Invalid DETRIX_CONTROL_PORT"):
            detrix.init(name="test")

    def test_reinit_after_config_error(self, monkeypatch):
        """Test can initialize after a config error."""
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "invalid")

        with pytest.raises(ConfigError):
            detrix.init(name="test")

        # Clear the env var
        monkeypatch.delenv("DETRIX_CONTROL_PORT")

        # Should be able to init now
        detrix.init(name="test")
        status = detrix.status()
        assert status["state"] == "sleeping"


class TestURLValidation:
    """Test URL validation error handling."""

    def test_invalid_daemon_url_format(self):
        """Test wake fails with invalid daemon URL format."""
        detrix.init(name="test", daemon_url="not-a-valid-url")

        with pytest.raises(DaemonError, match="Invalid daemon_url format"):
            detrix.wake()

    def test_invalid_daemon_url_scheme(self):
        """Test wake fails with invalid URL scheme."""
        detrix.init(name="test", daemon_url="ftp://127.0.0.1:8090")

        with pytest.raises(DaemonError, match="Invalid daemon_url scheme"):
            detrix.wake()

    def test_empty_daemon_url(self):
        """Test wake fails with empty daemon URL."""
        detrix.init(name="test")

        # Override the daemon_url to empty
        from detrix._state import get_state

        state = get_state()
        with state.lock:
            state.daemon_url = ""

        with pytest.raises(DaemonError, match="No daemon_url configured"):
            detrix.wake()


class TestDaemonResponseErrors:
    """Test handling of various daemon response errors."""

    def test_daemon_returns_400(self, error_daemon):
        """Test handling of 400 Bad Request from daemon."""
        ErrorDaemonHandler.error_code = 400
        ErrorDaemonHandler.error_message = "Bad Request"

        detrix.init(name="test", daemon_url=error_daemon)

        # Error can be HTTP status error or connection error depending on timing
        with pytest.raises(DaemonError, match=r"(HTTP 400|Cannot connect|Failed to register)"):
            detrix.wake()

    def test_daemon_returns_401(self, error_daemon):
        """Test handling of 401 Unauthorized from daemon."""
        ErrorDaemonHandler.error_code = 401
        ErrorDaemonHandler.error_message = "Unauthorized"

        detrix.init(name="test", daemon_url=error_daemon)

        # Error can be HTTP status error or connection error depending on timing
        with pytest.raises(DaemonError, match=r"(HTTP 401|Cannot connect|Failed to register)"):
            detrix.wake()

    def test_daemon_returns_503(self, error_daemon):
        """Test handling of 503 Service Unavailable from daemon."""
        ErrorDaemonHandler.error_code = 503
        ErrorDaemonHandler.error_message = "Service Unavailable"

        detrix.init(name="test", daemon_url=error_daemon)

        # Error can be HTTP status error or connection error depending on timing
        with pytest.raises(DaemonError, match=r"(HTTP 503|Cannot connect|Failed to register)"):
            detrix.wake()


class TestRecoveryAfterErrors:
    """Test recovery scenarios after errors."""

    def test_reinit_after_error(self):
        """Test reinitialization after an error."""
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")

        # Try to wake, it fails
        with pytest.raises(DaemonError):
            detrix.wake()

        # Shutdown and reinit
        detrix.shutdown()
        detrix.init(name="test-2")

        status = detrix.status()
        assert status["name"].startswith("test-2-")

    def test_multiple_failed_wakes(self):
        """Test multiple failed wake attempts don't corrupt state."""
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")

        for _ in range(5):
            with pytest.raises(DaemonError):
                detrix.wake()

            # State should always be sleeping
            status = detrix.status()
            assert status["state"] == "sleeping"

    def test_successful_wake_after_multiple_failures(self, working_daemon):
        """Test successful wake after multiple failures."""
        detrix.init(name="test", daemon_url="http://127.0.0.1:1")

        # Multiple failed attempts
        for _ in range(3):
            with pytest.raises(DaemonError):
                detrix.wake()

        # Finally succeed
        result = detrix.wake(daemon_url=working_daemon)
        assert result["status"] == "awake"


class TestExceptionLogging:
    """Test that broad exception handlers emit log messages."""

    def test_daemon_health_check_logs_on_failure(self, caplog):
        """Health check logs debug message on failure."""
        import logging
        from detrix.daemon import check_daemon_health

        with caplog.at_level(logging.DEBUG, logger="detrix.daemon"):
            result = check_daemon_health("http://127.0.0.1:1", timeout=0.1)

        assert result is False
        assert any("Health check failed" in r.message for r in caplog.records)

    def test_debugger_wake_logs_on_failure(self, caplog):
        """wake_debugger logs warning on failure."""
        import logging
        from unittest.mock import patch

        from detrix.debugger import reset_debugger_state, wake_debugger

        reset_debugger_state()

        with (
            caplog.at_level(logging.WARNING, logger="detrix.debugger"),
            patch("detrix.debugger._get_debugpy", side_effect=RuntimeError("test")),
        ):
            success, port = wake_debugger("127.0.0.1", 0)

        assert success is False
        assert any("Failed to start debugpy" in r.message for r in caplog.records)
