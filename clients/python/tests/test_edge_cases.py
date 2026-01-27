"""Tests for edge cases in the Detrix Python client.

These tests verify that the client handles unusual inputs, boundary
conditions, and edge cases correctly.
"""

import contextlib
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

import detrix
from detrix._state import reset_state
from detrix.config import generate_connection_name


class MockDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon HTTP handler."""

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
                json.dumps({"connectionId": data.get("connectionId")}).encode()
            )
        else:
            self.send_response(404)
            self.end_headers()

    def do_DELETE(self):
        """Handle DELETE requests."""
        self.send_response(200)
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


class TestConnectionNames:
    """Test edge cases for connection names."""

    def test_empty_name(self):
        """Test empty name uses default."""
        detrix.init(name="")
        status = detrix.status()
        # Should use default "detrix-client-{pid}"
        assert status["name"].startswith("detrix-client-")
        assert str(os.getpid()) in status["name"]

    def test_long_name(self, mock_daemon):
        """Test very long connection name."""
        long_name = "a" * 1000
        detrix.init(name=long_name, daemon_url=mock_daemon)

        # Should accept long names
        status = detrix.status()
        assert status["name"].startswith(long_name)

        # Should be able to wake
        result = detrix.wake()
        assert result["status"] == "awake"

    def test_unicode_name(self, mock_daemon):
        """Test unicode characters in connection name."""
        unicode_name = "test-服务-🔍"
        detrix.init(name=unicode_name, daemon_url=mock_daemon)

        status = detrix.status()
        assert unicode_name in status["name"]

        # Should be able to wake
        result = detrix.wake()
        assert result["status"] == "awake"

    def test_special_characters_in_name(self, mock_daemon):
        """Test special characters in connection name."""
        special_name = "test-service_v1.2.3+build.123"
        detrix.init(name=special_name, daemon_url=mock_daemon)

        status = detrix.status()
        assert special_name in status["name"]

    def test_whitespace_in_name(self, mock_daemon):
        """Test whitespace in connection name."""
        # Whitespace should be preserved
        whitespace_name = "test service"
        detrix.init(name=whitespace_name, daemon_url=mock_daemon)

        status = detrix.status()
        assert whitespace_name in status["name"]

    def test_connection_name_generation(self):
        """Test connection name generation function."""
        # Empty name should use default
        result = generate_connection_name("")
        assert result.startswith("detrix-client-")
        assert str(os.getpid()) in result

        # Provided name should be used with PID
        result = generate_connection_name("my-service")
        assert result.startswith("my-service-")
        assert str(os.getpid()) in result


class TestPortNumbers:
    """Test edge cases for port numbers."""

    def test_port_zero_auto_assigns(self):
        """Test port 0 auto-assigns an available port."""
        detrix.init(name="test", control_port=0)
        status = detrix.status()

        # Should have an actual port assigned
        assert status["control_port"] > 0
        assert status["control_port"] < 65536

    def test_explicit_port(self):
        """Test explicit port number."""
        # Find a free port first
        import socket

        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("", 0))
            port = s.getsockname()[1]

        detrix.init(name="test", control_port=port)
        status = detrix.status()

        assert status["control_port"] == port

    def test_port_boundary_values(self, monkeypatch):
        """Test boundary values for port numbers."""
        # Port 0 is valid (auto-assign)
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "0")
        detrix.init(name="test")
        status = detrix.status()
        assert status["control_port"] > 0
        detrix.shutdown()
        reset_state()

        # Port 65535 is valid
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "65535")
        # This might fail if port is in use, so just check parsing
        # We'll catch the actual bind error differently
        try:
            detrix.init(name="test")
            detrix.shutdown()
        except OSError:
            pass  # Port might be in use
        reset_state()

        # Port 65536 is invalid
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "65536")
        from detrix.errors import ConfigError
        with pytest.raises(ConfigError):
            detrix.init(name="test")


class TestDaemonURLs:
    """Test edge cases for daemon URLs."""

    def test_url_with_trailing_slash(self, mock_daemon):
        """Test daemon URL with trailing slash."""
        url_with_slash = mock_daemon + "/"
        detrix.init(name="test", daemon_url=url_with_slash)

        result = detrix.wake()
        assert result["status"] == "awake"

    def test_url_with_multiple_trailing_slashes(self, mock_daemon):
        """Test daemon URL with multiple trailing slashes."""
        url_with_slashes = mock_daemon + "///"
        detrix.init(name="test", daemon_url=url_with_slashes)

        result = detrix.wake()
        assert result["status"] == "awake"

    def test_https_url(self):
        """Test HTTPS daemon URL is accepted (validation only)."""
        detrix.init(name="test", daemon_url="https://127.0.0.1:8090")

        # URL validation should pass, but connection will fail
        # because no HTTPS daemon is running
        from detrix.errors import DaemonError

        with pytest.raises(DaemonError, match="Cannot connect"):
            detrix.wake()

    def test_url_with_path(self):
        """Test daemon URL with path component."""
        # URL with path is valid
        detrix.init(name="test", daemon_url="http://127.0.0.1:8090/api")

        # Should fail to connect (no daemon), but URL is valid
        from detrix.errors import DaemonError

        with pytest.raises(DaemonError):
            detrix.wake()


class TestEnvironmentVariables:
    """Test edge cases for environment variable handling."""

    def test_empty_env_vars(self, monkeypatch, mock_daemon):
        """Test empty environment variables are ignored."""
        monkeypatch.setenv("DETRIX_NAME", "")
        monkeypatch.setenv("DETRIX_CONTROL_PORT", "")
        monkeypatch.setenv("DETRIX_DEBUG_PORT", "")

        detrix.init(daemon_url=mock_daemon)
        status = detrix.status()

        # Empty values should be treated as not set
        assert status["name"].startswith("detrix-client-")

    def test_whitespace_env_vars(self, monkeypatch, mock_daemon):
        """Test whitespace-only environment variables."""
        monkeypatch.setenv("DETRIX_NAME", "   ")

        detrix.init(daemon_url=mock_daemon)
        status = detrix.status()

        # Whitespace is preserved as-is
        assert "   " in status["name"]

    def test_env_var_overrides(self, monkeypatch, mock_daemon):
        """Test explicit params override env vars."""
        monkeypatch.setenv("DETRIX_NAME", "env-name")

        detrix.init(name="explicit-name", daemon_url=mock_daemon)
        status = detrix.status()

        # Explicit name should win
        assert status["name"].startswith("explicit-name-")


class TestStateTransitions:
    """Test edge cases in state transitions."""

    def test_shutdown_from_awake(self, mock_daemon):
        """Test shutdown from awake state."""
        detrix.init(name="test", daemon_url=mock_daemon)
        detrix.wake()

        # Shutdown should clean up properly
        detrix.shutdown()

        # Should be able to reinit
        detrix.init(name="test-2", daemon_url=mock_daemon)
        status = detrix.status()
        assert status["name"].startswith("test-2-")

    def test_multiple_shutdowns(self):
        """Test multiple shutdown calls are safe."""
        detrix.init(name="test")
        detrix.shutdown()
        detrix.shutdown()  # Should not raise
        detrix.shutdown()  # Should not raise

    def test_operations_without_init(self):
        """Test operations on uninitialized client."""
        # status() should work (returns default state)
        status = detrix.status()
        assert status["state"] == "sleeping"

        # sleep() should work
        result = detrix.sleep()
        # Already sleeping or just went to sleep
        assert "sleeping" in result["status"]


class TestTimeoutConfiguration:
    """Test edge cases for timeout configuration."""

    def test_invalid_timeout_env_var_non_numeric(self, monkeypatch):
        """Test non-numeric timeout env var raises ConfigError."""
        from detrix.errors import ConfigError

        monkeypatch.setenv("DETRIX_HEALTH_CHECK_TIMEOUT", "not-a-number")

        with pytest.raises(ConfigError, match="Invalid DETRIX_HEALTH_CHECK_TIMEOUT"):
            detrix.init(name="test")

    def test_invalid_timeout_env_var_negative(self, monkeypatch):
        """Test negative timeout env var raises ConfigError."""
        from detrix.errors import ConfigError

        monkeypatch.setenv("DETRIX_REGISTER_TIMEOUT", "-5.0")

        with pytest.raises(ConfigError, match="Invalid DETRIX_REGISTER_TIMEOUT"):
            detrix.init(name="test")

    def test_invalid_timeout_env_var_zero(self, monkeypatch):
        """Test zero timeout env var raises ConfigError."""
        from detrix.errors import ConfigError

        monkeypatch.setenv("DETRIX_UNREGISTER_TIMEOUT", "0")

        with pytest.raises(ConfigError, match="Invalid DETRIX_UNREGISTER_TIMEOUT"):
            detrix.init(name="test")

    def test_timeout_param_overrides_env_var(self, monkeypatch, mock_daemon):
        """Test explicit timeout param overrides env var."""
        # Set a different value in env
        monkeypatch.setenv("DETRIX_HEALTH_CHECK_TIMEOUT", "10.0")

        # Explicit param should override
        detrix.init(
            name="test",
            daemon_url=mock_daemon,
            health_check_timeout=1.0,
        )

        status = detrix.status()
        # Client initialized successfully with overridden timeout
        assert status["state"] == "sleeping"

    def test_timeout_env_vars_used_when_params_default(self, monkeypatch, mock_daemon):
        """Test timeout env vars are used when params are default."""
        monkeypatch.setenv("DETRIX_HEALTH_CHECK_TIMEOUT", "3.5")
        monkeypatch.setenv("DETRIX_REGISTER_TIMEOUT", "10.0")
        monkeypatch.setenv("DETRIX_UNREGISTER_TIMEOUT", "1.5")

        detrix.init(name="test", daemon_url=mock_daemon)

        # Should initialize successfully with env var timeouts
        status = detrix.status()
        assert status["state"] == "sleeping"

    def test_timeout_float_precision(self, monkeypatch, mock_daemon):
        """Test timeout values with high precision."""
        monkeypatch.setenv("DETRIX_HEALTH_CHECK_TIMEOUT", "1.234567890")

        detrix.init(name="test", daemon_url=mock_daemon)
        status = detrix.status()
        assert status["state"] == "sleeping"


class TestDebugPortBehavior:
    """Test edge cases related to debug port behavior."""

    def test_debug_port_active_tracking(self, mock_daemon):
        """Test debug_port_active field tracks actual port state."""
        detrix.init(name="test", daemon_url=mock_daemon)

        # Initially not active
        status = detrix.status()
        assert status["debug_port_active"] is False

        # After wake, should be active
        detrix.wake()
        status = detrix.status()
        assert status["debug_port_active"] is True

        # After sleep, port is still active (debugpy limitation)
        detrix.sleep()
        status = detrix.status()
        # Note: debug_port_active stays True because debugpy can't close its port
        assert status["debug_port_active"] is True

    def test_debug_port_reused_after_sleep(self, mock_daemon):
        """Test same debug port is reused after sleep/wake cycle."""
        detrix.init(name="test", daemon_url=mock_daemon)

        # First wake
        result1 = detrix.wake()
        port1 = result1["debug_port"]

        detrix.sleep()

        # Second wake should return same port
        result2 = detrix.wake()
        port2 = result2["debug_port"]

        assert port1 == port2
