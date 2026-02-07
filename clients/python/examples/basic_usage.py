#!/usr/bin/env python3
"""Basic usage example for Detrix Python client.

Prerequisites:
    1. Start the Detrix daemon first:
       $ detrix serve --daemon

    2. Run this example (from clients/python directory):
       $ uv run python examples/basic_usage.py

The client will:
    1. Start a control plane server for remote management
    2. Stay in SLEEPING state (zero overhead)
    3. On wake(): start debugpy and register with daemon
    4. Allow the daemon to set metrics/observation points
    5. On sleep(): stop debugger and unregister
"""

import time
import detrix


def main():
    # Initialize client - starts control plane but stays SLEEPING
    # No debugger loaded, no daemon interaction yet
    print("Initializing Detrix client...")
    detrix.init(
        name="example-service",
        daemon_url="http://127.0.0.1:8090",  # Default daemon URL
    )

    status = detrix.status()
    print(f"Status after init: {status['state']}")
    print(f"Control plane at: {status['control_host']}:{status['control_port']}")

    # Wake up - start debugger and register with daemon
    print("\nWaking up (starting debugger, registering with daemon)...")
    try:
        result = detrix.wake()
        print(f"Wake result: {result}")
        print(f"Debug port: {result['debug_port']}")
        print(f"Connection ID: {result['connection_id']}")

        # Now the daemon can set metrics on this process
        # Simulate some work
        print("\nRunning application logic...")
        for i in range(5):
            process_request(i)
            time.sleep(1)

    except detrix.DaemonError as e:
        print(f"Could not connect to daemon: {e}")
        print("Make sure the daemon is running: detrix serve --daemon")

    # Sleep - stop debugger and unregister
    print("\nGoing to sleep...")
    detrix.sleep()
    print(f"Status after sleep: {detrix.status()['state']}")

    # Cleanup
    print("\nShutting down...")
    detrix.shutdown()
    print("Done!")


def process_request(request_id: int):
    """Simulate processing a request.

    The Detrix daemon can set observation points on any line in this function
    to capture variable values without modifying the code.
    """
    data = {"id": request_id, "processed": True}
    result = transform_data(data)
    print(f"  Processed request {request_id}: {result}")


def transform_data(data: dict) -> dict:
    """Transform data.

    Example metric: Add observation at line 82 to capture 'data' value.
    """
    data["transformed"] = True
    data["timestamp"] = time.time()
    return data


if __name__ == "__main__":
    main()
