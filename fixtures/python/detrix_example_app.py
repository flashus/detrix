#!/usr/bin/env python3
"""
Example Trading Bot for Detrix MVP Demo
This script demonstrates dynamic metrics capture via debugpy

Usage:
  python -m debugpy --listen 5678 --wait-for-client detrix_example_app.py
  
Then run Detrix:
  detrix serve --port 5678 --config detrix.toml
"""

import time
import random


def place_order(symbol, quantity, price):
    """Place a trading order"""
    print(f"Placing order: {symbol} x{quantity} @ ${price}")

    # METRIC: order_placed (line 20)
    order_id = random.randint(1000, 9999)

    return order_id


def update_position(symbol, position_size, unrealized_pnl):
    """Update position information"""
    print(f"Position update: {symbol} size={position_size} P&L=${unrealized_pnl:.2f}")

    # METRIC: position_updated (line 30)
    last_update = time.time()

    return last_update


def execute_trade(order_id, filled_quantity, average_price):
    """Execute a trade"""
    print(f"Trade executed: Order#{order_id} filled {filled_quantity} @ ${average_price:.2f}")

    # METRIC: trade_executed (line 40)
    execution_time = time.time()

    return execution_time


def main():
    """Main trading loop"""
    print("✅ Detrix connected!")
    print()

    print("=" * 60)
    print("Detrix MVP Demo - Trading Bot")
    print("=" * 60)
    print()
    print("This bot will:")
    print("  1. Place orders")
    print("  2. Update positions")
    print("  3. Execute trades")
    print()
    print("Metrics will be captured automatically via debugpy logpoints!")
    print("=" * 60)
    print()

    symbols = ["BTCUSD", "ETHUSD", "SOLUSD"]

    for i in range(10):
        symbol = random.choice(symbols)

        # Place an order
        quantity = random.randint(1, 100)
        price = random.uniform(20000, 70000) if symbol == "BTCUSD" else random.uniform(1000, 5000)
        order_id = place_order(symbol, quantity, price)

        time.sleep(0.5)

        # Update position
        position_size = random.randint(-100, 100)
        unrealized_pnl = random.uniform(-1000, 1000)
        update_position(symbol, position_size, unrealized_pnl)

        time.sleep(0.5)

        # Execute trade
        filled_qty = quantity
        avg_price = price * random.uniform(0.99, 1.01)
        execute_trade(order_id, filled_qty, avg_price)

        time.sleep(1)
        print()

    print("=" * 60)
    print("Trading bot finished!")
    print("Check Detrix for captured metrics")
    print("=" * 60)


if __name__ == "__main__":
    main()
