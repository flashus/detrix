"""Tests for control plane HTTP server."""

import json
import time
import urllib.error
import urllib.request

import pytest

from detrix._state import get_state, reset_state
from detrix.control import start_control_server, stop_control_server


@pytest.fixture
def control_server():
    """Start control server for tests."""
    reset_state()
    state = get_state()
    state.name = "test-service-123"
    state.daemon_url = "http://127.0.0.1:8090"

    port = start_control_server("127.0.0.1", 0)
    state.actual_control_port = port
    time.sleep(0.1)  # Give server time to start
    yield f"http://127.0.0.1:{port}"
    stop_control_server()
    reset_state()


def fetch_json(url: str, method: str = "GET", data: dict = None) -> dict:
    """Helper to fetch JSON from URL."""
    body = None
    headers = {}
    if data:
        body = json.dumps(data).encode("utf-8")
        headers["Content-Type"] = "application/json"

    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=5) as response:
        return json.loads(response.read().decode("utf-8"))


class TestHealthEndpoint:
    """Test /detrix/health endpoint."""

    def test_returns_ok(self, control_server):
        """Test health endpoint returns ok."""
        result = fetch_json(f"{control_server}/detrix/health")
        assert result["status"] == "ok"
        assert result["service"] == "detrix-client"


class TestStatusEndpoint:
    """Test /detrix/status endpoint."""

    def test_returns_state(self, control_server):
        """Test status endpoint returns state."""
        result = fetch_json(f"{control_server}/detrix/status")
        assert result["state"] == "sleeping"
        assert result["name"] == "test-service-123"
        assert result["daemon_url"] == "http://127.0.0.1:8090"


class TestInfoEndpoint:
    """Test /detrix/info endpoint."""

    def test_returns_info(self, control_server):
        """Test info endpoint returns process info."""
        import os

        result = fetch_json(f"{control_server}/detrix/info")
        assert result["name"] == "test-service-123"
        assert result["pid"] == os.getpid()
        assert "python_version" in result


class TestSleepEndpoint:
    """Test /detrix/sleep endpoint."""

    def test_sleep_from_sleeping(self, control_server):
        """Test sleep when already sleeping."""
        result = fetch_json(f"{control_server}/detrix/sleep", method="POST")
        assert result["status"] == "already_sleeping"


class TestWakeEndpoint:
    """Test /detrix/wake endpoint."""

    def test_wake_without_daemon(self, control_server):
        """Test wake fails without daemon."""
        # Daemon is not running, should fail
        try:
            fetch_json(f"{control_server}/detrix/wake", method="POST")
            pytest.fail("Expected error")
        except urllib.error.HTTPError as e:
            assert e.code == 503  # Service unavailable
            data = json.loads(e.read().decode("utf-8"))
            assert "Cannot connect to daemon" in data["error"]

    def test_wake_with_daemon_url_override(self, control_server):
        """Test wake with daemon URL in request body."""
        # Use port 1 which is reserved and nothing should be listening on
        # Should fail since no daemon running
        try:
            fetch_json(
                f"{control_server}/detrix/wake",
                method="POST",
                data={"daemon_url": "http://127.0.0.1:1"}
            )
            pytest.fail("Expected error")
        except urllib.error.HTTPError as e:
            assert e.code == 503
            data = json.loads(e.read().decode("utf-8"))
            assert "http://127.0.0.1:1" in data["error"]
