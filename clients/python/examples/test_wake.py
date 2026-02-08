#!/usr/bin/env python3
"""Agent simulation test - wake a running app and observe it.

This script simulates what an AI agent typically does with Detrix:
1. Starts a target application (trade_bot_detrix.py)
2. Waits for it to initialize
3. Wakes the Detrix client in the app via control plane
4. Sets metrics on the running process via daemon API
5. Receives and displays captured events

Usage:
    1. Make sure the Detrix daemon is running:
       $ detrix serve --daemon

    2. Run this test (from clients/python directory):
       $ uv run python examples/test_wake.py

    Or with custom daemon port:
       $ uv run python examples/test_wake.py --daemon-port 9999

    The script will automatically start trade_bot_detrix.py as a subprocess.
"""
import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from typing import Any

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
TRADE_BOT_PATH = os.path.join(SCRIPT_DIR, "trade_bot_detrix.py")


# Global daemon URL, set from command line
DAEMON_URL = "http://127.0.0.1:8090"


def api_request(
    url: str,
    method: str = "GET",
    data: dict[str, Any] | None = None,
    timeout: float = 10.0,
) -> dict[str, Any] | list[Any] | None:
    """Make an HTTP request and return JSON response."""
    headers = {"Content-Type": "application/json"}
    body = json.dumps(data).encode() if data else None

    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        print(f"  HTTP Error {e.code}: {e.read().decode()}")
        return None
    except urllib.error.URLError as e:
        print(f"  URL Error: {e.reason}")
        return None


def check_daemon_health() -> bool:
    """Check if Detrix daemon is running."""
    result = api_request(f"{DAEMON_URL}/health")
    return result is not None and result.get("service") == "detrix"


def get_connection(connection_prefix: str) -> dict[str, Any] | None:
    """Get connection info from daemon."""
    connections = api_request(f"{DAEMON_URL}/api/v1/connections")
    if connections:
        for conn in connections:
            if conn["connectionId"].startswith(connection_prefix):
                return conn
    return None


def add_metric(
    connection_id: str,
    file_path: str,
    line: int,
    expression: str,
    name: str,
) -> dict[str, Any] | None:
    """Add a metric with a single expression via daemon API."""
    data = {
        "name": name,
        "connectionId": connection_id,
        "location": {
            "file": file_path,
            "line": line,
        },
        "expressions": [expression],
        "language": "python",
        "enabled": True,
    }
    return api_request(f"{DAEMON_URL}/api/v1/metrics", method="POST", data=data)


def add_multi_expr_metric(
    connection_id: str,
    file_path: str,
    line: int,
    expressions: list[str],
    name: str,
) -> dict[str, Any] | None:
    """Add a metric with multiple expressions via daemon API.

    Multi-expression metrics capture several variables simultaneously at the
    same line, producing one event with all values per hit.
    """
    data = {
        "name": name,
        "connectionId": connection_id,
        "location": {
            "file": file_path,
            "line": line,
        },
        "expressions": expressions,
        "language": "python",
        "enabled": True,
    }
    return api_request(f"{DAEMON_URL}/api/v1/metrics", method="POST", data=data)


def get_events(
    metric_id: int, limit: int = 10, since_micros: int | None = None
) -> list[dict[str, Any]]:
    """Get events for a metric."""
    url = f"{DAEMON_URL}/api/v1/events?metricId={metric_id}&limit={limit}"
    if since_micros is not None:
        url += f"&since={since_micros}"
    result = api_request(url)
    return result if isinstance(result, list) else []


def main() -> int:
    global DAEMON_URL

    parser = argparse.ArgumentParser(description="Agent simulation test")
    parser.add_argument(
        "--daemon-port",
        type=int,
        default=int(os.environ.get("DETRIX_DAEMON_PORT", "8090")),
        help="Daemon port (default: 8090 or DETRIX_DAEMON_PORT env var)",
    )
    parser.add_argument(
        "--daemon-url",
        type=str,
        default=os.environ.get("DETRIX_DAEMON_URL", ""),
        help="Full daemon URL (overrides --daemon-port)",
    )
    args = parser.parse_args()

    if args.daemon_url:
        DAEMON_URL = args.daemon_url
    else:
        DAEMON_URL = f"http://127.0.0.1:{args.daemon_port}"

    print("=" * 70)
    print("Agent Simulation Test - Wake and Observe a Running Application")
    print("=" * 70)
    print()
    print(f"Daemon URL: {DAEMON_URL}")
    print()

    # Step 1: Check daemon is running
    print("1. Checking Detrix daemon...")
    if not check_daemon_health():
        print("   ERROR: Detrix daemon is not running!")
        print("   Start it with: detrix serve --daemon")
        return 1
    print("   Daemon is healthy")
    print()

    # Step 2: Start trade_bot as subprocess
    # Use uv run to ensure we're using the project's virtual environment
    print("2. Starting trade_bot_detrix.py...")
    proc = subprocess.Popen(
        ["uv", "run", "python", "-u", TRADE_BOT_PATH, "--daemon-url", DAEMON_URL],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        cwd=os.path.dirname(SCRIPT_DIR),  # Run from clients/python
        env={**os.environ, "PYTHONUNBUFFERED": "1"},
    )

    # Wait for control plane URL in output
    control_url = None
    start_time = time.time()
    timeout_secs = 10.0

    assert proc.stdout is not None  # For type checker
    while time.time() - start_time < timeout_secs:
        line = proc.stdout.readline()
        if not line:
            break
        print(f"   [bot] {line.rstrip()}")

        # Look for control plane URL
        match = re.search(r"Control plane: (http://[\d.:]+)", line)
        if match:
            control_url = match.group(1)
            break

    if not control_url:
        print("   ERROR: Could not find control plane URL in output")
        proc.terminate()
        return 1

    print(f"   Found control plane: {control_url}")
    print()

    # Step 3: Wait 3 seconds (simulating agent discovering the app)
    print("3. Waiting 3 seconds (simulating agent discovery time)...")
    for i in range(3, 0, -1):
        print(f"   {i}...", end=" ", flush=True)
        time.sleep(1)
    print()
    print()

    # Step 4: Wake the client via control plane
    print("4. Waking Detrix client in trade_bot...")
    wake_result = api_request(f"{control_url}/detrix/wake", method="POST")
    if not wake_result:
        print("   ERROR: Failed to wake client")
        proc.terminate()
        return 1

    print(f"   Status: {wake_result.get('status')}")
    print(f"   Debug port: {wake_result.get('debug_port')}")
    print(f"   Connection ID: {wake_result.get('connection_id')}")
    connection_id = wake_result.get("connection_id")
    print()

    # Step 5: Verify connection on daemon
    print("5. Verifying connection registered with daemon...")
    time.sleep(1)  # Give daemon time to establish DAP connection
    conn = get_connection("trade-bot")
    if conn:
        status_map = {1: "disconnected", 2: "connecting", 3: "connected"}
        print(f"   Connection: {conn['connectionId']}")
        print(f"   Status: {status_map.get(conn['status'], conn['status'])}")
        print(f"   Port: {conn['port']}")
    else:
        print("   WARNING: Connection not found on daemon")
    print()

    # Step 6: Add metrics
    print("6. Adding metrics to observe the running application...")

    # Record session start so we only query events from this run
    import time as _time

    session_start_micros = int(_time.time() * 1_000_000)

    # Metric 1: Observe order placement (line 103 in trade_bot - place_order call)
    # Available vars at line 103: symbol, quantity, price, iteration
    metric1 = add_metric(
        connection_id=connection_id,
        file_path=TRADE_BOT_PATH,
        line=103,  # place_order call
        expression="f'Order: {symbol} x{quantity} @ ${price:.2f}'",
        name="order_info",
    )
    if metric1:
        metric1_id = metric1.get("metricId")
        print(f"   Metric 1 (order_info): ID={metric1_id}")
    else:
        print("   WARNING: Failed to add metric 1")
        metric1_id = None

    # Metric 2: Observe P&L result (line 110 in trade_bot - after pnl calculation)
    # Available vars at line 110: pnl, iteration, entry_price, current_price
    metric2 = add_metric(
        connection_id=connection_id,
        file_path=TRADE_BOT_PATH,
        line=110,  # print statement with pnl
        expression="f'P&L: ${pnl:.2f} (iter {iteration})'",
        name="pnl_data",
    )
    if metric2:
        metric2_id = metric2.get("metricId")
        print(f"   Metric 2 (pnl_data): ID={metric2_id}")
    else:
        print("   WARNING: Failed to add metric 2")
        metric2_id = None

    # Metric 3: Multi-expression - capture symbol, quantity, price in one metric
    # Available vars at line 106: symbol, quantity, price, order_id
    metric3 = add_multi_expr_metric(
        connection_id=connection_id,
        file_path=TRADE_BOT_PATH,
        line=106,  # entry_price = price (symbol, quantity, price in scope)
        expressions=["symbol", "quantity", "price"],
        name="order_details",
    )
    if metric3:
        metric3_id = metric3.get("metricId")
        print(f"   Metric 3 (order_details): ID={metric3_id} [MULTI-EXPRESSION: symbol, quantity, price]")
    else:
        print("   WARNING: Failed to add metric 3 (multi-expression)")
        metric3_id = None

    print()

    # Step 7: Wait and collect events
    print("7. Waiting for events (15 seconds)...")
    print()

    events_received = 0
    seen_events: set[int] = set()

    for i in range(15):
        time.sleep(1)

        # Check for events on metric 1
        if metric1_id:
            events = get_events(metric1_id, limit=10, since_micros=session_start_micros)
            for event in events:
                event_id = event.get("metricId", 0) * 1000000 + event.get("timestamp", 0)
                if event_id in seen_events:
                    continue
                seen_events.add(event_id)
                events_received += 1
                # Value is in values[0].valueJson
                values = event.get("values", [])
                value = values[0].get("valueJson", "") if values else ""
                ts = event.get("timestamp", 0)
                # Convert microseconds to readable time if it's an int
                if isinstance(ts, int) and ts > 0:
                    from datetime import datetime
                    ts_str = datetime.fromtimestamp(ts / 1_000_000).strftime("%H:%M:%S")
                else:
                    ts_str = str(ts)[:19]
                print(f"   [EVENT] order_info: {value} ({ts_str})")

        # Check for events on metric 2
        if metric2_id:
            events = get_events(metric2_id, limit=10, since_micros=session_start_micros)
            for event in events:
                event_id = event.get("metricId", 0) * 1000000 + event.get("timestamp", 0)
                if event_id in seen_events:
                    continue
                seen_events.add(event_id)
                events_received += 1
                # Value is in values[0].valueJson
                values = event.get("values", [])
                value = values[0].get("valueJson", "") if values else ""
                ts = event.get("timestamp", 0)
                # Convert microseconds to readable time if it's an int
                if isinstance(ts, int) and ts > 0:
                    from datetime import datetime
                    ts_str = datetime.fromtimestamp(ts / 1_000_000).strftime("%H:%M:%S")
                else:
                    ts_str = str(ts)[:19]
                print(f"   [EVENT] pnl_data: {value} ({ts_str})")

        # Check for events on metric 3 (multi-expression)
        if metric3_id:
            events = get_events(metric3_id, limit=10, since_micros=session_start_micros)
            for event in events:
                event_id = event.get("metricId", 0) * 1000000 + event.get("timestamp", 0)
                if event_id in seen_events:
                    continue
                seen_events.add(event_id)
                events_received += 1
                # Multi-expression: values is an array of {expression, valueJson, ...}
                values = event.get("values", [])
                parts = [f"{v.get('expression')}={v.get('valueJson', '?')}" for v in values]
                ts = event.get("timestamp", 0)
                if isinstance(ts, int) and ts > 0:
                    from datetime import datetime
                    ts_str = datetime.fromtimestamp(ts / 1_000_000).strftime("%H:%M:%S")
                else:
                    ts_str = str(ts)[:19]
                print(f"   [EVENT] order_details: {', '.join(parts)} ({ts_str})")

    print()
    print(f"   Total events received: {events_received}")
    print()

    # Step 8: Cleanup
    print("8. Cleaning up...")

    # Sleep the client first to unregister from daemon and clean up metrics/events
    try:
        api_request(f"{control_url}/detrix/sleep", method="POST")
    except Exception:
        pass

    # Stop the trade bot
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
        print("   Trade bot stopped")
    except subprocess.TimeoutExpired:
        proc.kill()
        print("   Trade bot killed (timeout)")

    print()
    print("=" * 70)
    if events_received > 0:
        print("TEST PASSED - Successfully observed running application!")
    else:
        print("TEST COMPLETED - No events captured (check metric expressions)")
    print("=" * 70)

    return 0


if __name__ == "__main__":
    sys.exit(main())
