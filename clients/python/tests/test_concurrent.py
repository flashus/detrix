"""Tests for concurrent operations in the Detrix Python client.

These tests verify that the client handles concurrent access correctly,
including multiple threads calling wake/sleep and concurrent HTTP requests.
"""

import concurrent.futures
import contextlib
import json
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

import detrix
from detrix._state import reset_state


class MockDaemonHandler(BaseHTTPRequestHandler):
    """Mock daemon HTTP handler for concurrent tests."""

    call_count = 0
    lock = threading.Lock()

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
            with MockDaemonHandler.lock:
                MockDaemonHandler.call_count += 1

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
def mock_daemon():
    """Start a mock daemon server."""
    MockDaemonHandler.call_count = 0
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


class TestConcurrentInit:
    """Test concurrent init calls."""

    def test_concurrent_init_only_one_succeeds(self):
        """Two threads call init() via a barrier; exactly one succeeds."""
        barrier = threading.Barrier(2)
        results: list[str] = []
        lock = threading.Lock()

        def try_init(idx: int):
            barrier.wait()
            try:
                detrix.init(name=f"test-{idx}")
                with lock:
                    results.append("ok")
            except detrix.ConfigError:
                with lock:
                    results.append("already_initialized")
            except Exception as e:
                with lock:
                    results.append(f"error: {e}")

        t1 = threading.Thread(target=try_init, args=(1,))
        t2 = threading.Thread(target=try_init, args=(2,))
        t1.start()
        t2.start()
        t1.join()
        t2.join()

        assert sorted(results) == ["already_initialized", "ok"]


class TestConcurrentWakeSleep:
    """Test concurrent wake/sleep operations."""

    def test_concurrent_wake_calls(self, mock_daemon):
        """Test multiple concurrent wake calls - only one should register."""
        detrix.init(name="test", daemon_url=mock_daemon)

        results = []

        def call_wake():
            try:
                result = detrix.wake()
                results.append(result)
            except Exception as e:
                results.append({"error": str(e)})

        # Start multiple threads calling wake
        threads = [threading.Thread(target=call_wake) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All calls should succeed, but only one should actually register
        # (others return "already_awake")
        assert len(results) == 5
        awake_count = sum(1 for r in results if r.get("status") == "awake")
        already_awake_count = sum(
            1 for r in results if r.get("status") == "already_awake"
        )

        assert awake_count == 1
        assert already_awake_count == 4

        # Daemon should only receive one registration
        assert MockDaemonHandler.call_count == 1

    def test_concurrent_sleep_calls(self, mock_daemon):
        """Test multiple concurrent sleep calls."""
        detrix.init(name="test", daemon_url=mock_daemon)
        detrix.wake()

        results = []

        def call_sleep():
            result = detrix.sleep()
            results.append(result)

        # Start multiple threads calling sleep
        threads = [threading.Thread(target=call_sleep) for _ in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All calls should succeed, but only one should actually sleep
        assert len(results) == 5
        sleeping_count = sum(1 for r in results if r.get("status") == "sleeping")
        already_sleeping_count = sum(
            1 for r in results if r.get("status") == "already_sleeping"
        )

        assert sleeping_count == 1
        assert already_sleeping_count == 4

    def test_rapid_wake_sleep_cycles(self, mock_daemon):
        """Test rapid wake/sleep cycles from multiple threads."""
        detrix.init(name="test", daemon_url=mock_daemon)

        errors = []

        def cycle_wake_sleep(iterations):
            for _ in range(iterations):
                try:
                    detrix.wake()
                    time.sleep(0.01)  # Small delay
                    detrix.sleep()
                except Exception as e:
                    errors.append(str(e))

        # Start multiple threads doing wake/sleep cycles
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
            futures = [executor.submit(cycle_wake_sleep, 5) for _ in range(3)]
            concurrent.futures.wait(futures)

        # No errors should occur (state machine should handle concurrent access)
        assert len(errors) == 0

        # Final state should be consistent
        status = detrix.status()
        assert status["state"] in ("sleeping", "awake")


class TestConcurrentControlPlane:
    """Test concurrent HTTP requests to control plane."""

    def test_concurrent_status_requests(self, mock_daemon):
        """Test multiple concurrent status requests."""
        detrix.init(name="test", daemon_url=mock_daemon)
        status = detrix.status()
        port = status["control_port"]

        results = []

        def fetch_status():
            try:
                url = f"http://127.0.0.1:{port}/detrix/status"
                req = urllib.request.Request(url)
                with urllib.request.urlopen(req, timeout=5) as response:
                    data = json.loads(response.read().decode())
                    results.append(data)
            except Exception as e:
                results.append({"error": str(e)})

        # Make many concurrent requests
        threads = [threading.Thread(target=fetch_status) for _ in range(20)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All requests should succeed with consistent data
        assert len(results) == 20
        for r in results:
            assert "error" not in r
            assert r["name"].startswith("test-")

    def test_concurrent_health_checks(self, mock_daemon):
        """Test concurrent health check requests."""
        detrix.init(name="test", daemon_url=mock_daemon)
        status = detrix.status()
        port = status["control_port"]

        results = []

        def fetch_health():
            try:
                url = f"http://127.0.0.1:{port}/detrix/health"
                req = urllib.request.Request(url)
                with urllib.request.urlopen(req, timeout=5) as response:
                    data = json.loads(response.read().decode())
                    results.append(data)
            except Exception as e:
                results.append({"error": str(e)})

        # Make many concurrent requests
        threads = [threading.Thread(target=fetch_health) for _ in range(50)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All requests should succeed
        assert len(results) == 50
        for r in results:
            assert "error" not in r
            assert r["status"] == "ok"
            assert r["service"] == "detrix-client"


class TestStateConsistency:
    """Test state consistency under concurrent access."""

    def test_state_lock_prevents_corruption(self, mock_daemon):
        """Test that state lock prevents data corruption."""
        detrix.init(name="test", daemon_url=mock_daemon)

        iterations = 100
        errors = []

        def toggle_state():
            for _ in range(iterations):
                try:
                    status = detrix.status()
                    # Status should always be a valid state
                    assert status["state"] in ("sleeping", "warm", "awake")
                    # name should always be set
                    assert status["name"].startswith("test-")
                except AssertionError as e:
                    errors.append(str(e))

        # Run status checks while wake/sleep are happening
        def wake_sleep_loop():
            for _ in range(iterations // 2):
                try:
                    detrix.wake()
                    detrix.sleep()
                except Exception:
                    pass  # Ignore errors, we're testing state consistency

        threads = [
            threading.Thread(target=toggle_state),
            threading.Thread(target=toggle_state),
            threading.Thread(target=wake_sleep_loop),
        ]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # No assertion errors should occur
        assert len(errors) == 0, f"State consistency errors: {errors}"
