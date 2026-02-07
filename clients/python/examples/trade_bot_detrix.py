#!/usr/bin/env python3
"""Long-running trading bot with Detrix client - runs until Ctrl+C

This is a typical application that integrates the Detrix Python client.
It initializes the client but stays in SLEEPING state, exposing a control
plane endpoint for external agents to wake it when observability is needed.

Usage:
    1. Run this script:
       $ python trade_bot_detrix.py

    Or with custom daemon URL:
       $ python trade_bot_detrix.py --daemon-url http://127.0.0.1:9999

    2. The bot will print its control plane URL.
       An agent can then POST to /detrix/wake to enable observability.

    3. Press Ctrl+C to stop.
"""
import argparse
import os
import random
import signal
import sys
import time

# Add the detrix package to path for development
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import detrix

running = True


def signal_handler(signum, frame):
    global running
    print("\nReceived shutdown signal, stopping...")
    running = False


signal.signal(signal.SIGTERM, signal_handler)
signal.signal(signal.SIGINT, signal_handler)


def place_order(symbol: str, quantity: int, price: float) -> int:
    """Place a trading order."""
    order_id = random.randint(1000, 9999)
    total = quantity * price
    print(f"  Order #{order_id}: {symbol} x{quantity} @ ${price:.2f} = ${total:.2f}")
    return order_id


def calculate_pnl(entry_price: float, current_price: float, quantity: int) -> float:
    """Calculate profit/loss."""
    pnl = (current_price - entry_price) * quantity
    return pnl


def main():
    global running

    parser = argparse.ArgumentParser(description="Trading bot with Detrix client")
    parser.add_argument(
        "--daemon-url",
        type=str,
        default=os.environ.get("DETRIX_DAEMON_URL", "http://127.0.0.1:8090"),
        help="Daemon URL (default: http://127.0.0.1:8090 or DETRIX_DAEMON_URL env var)",
    )
    args = parser.parse_args()

    # Initialize Detrix client - starts control plane but stays SLEEPING
    # No debugger loaded, no daemon interaction yet - zero overhead
    print("Initializing Detrix client...", flush=True)
    detrix.init(
        name="trade-bot",
        daemon_url=args.daemon_url,
    )

    status = detrix.status()
    control_url = f"http://{status['control_host']}:{status['control_port']}"
    print(f"  State: {status['state']}", flush=True)
    print(f"  Control plane: {control_url}", flush=True)
    print(flush=True)
    print("  To enable observability, an agent can POST to:", flush=True)
    print(f"    curl -X POST {control_url}/detrix/wake", flush=True)
    print(flush=True)

    print("=" * 60, flush=True)
    print("Trading bot started - runs forever until Ctrl+C", flush=True)
    print("=" * 60, flush=True)
    print(flush=True)

    symbols = ["BTCUSD", "ETHUSD", "SOLUSD"]
    iteration = 0

    while running:
        iteration += 1
        symbol = random.choice(symbols)
        quantity = random.randint(1, 50)
        price = random.uniform(100, 1000)

        # Line 83 - place_order call (good place for a metric)
        order_id = place_order(symbol, quantity, price)

        # Lines 86-88 - calculate pnl (good place for metrics)
        entry_price = price
        current_price = price * random.uniform(0.95, 1.05)
        pnl = calculate_pnl(entry_price, current_price, quantity)

        print(f"    -> P&L: ${pnl:.2f} (iteration {iteration})")
        print()

        time.sleep(2)

    print("Shutting down Detrix client...")
    detrix.shutdown()
    print("Trading bot stopped!")


if __name__ == "__main__":
    main()
