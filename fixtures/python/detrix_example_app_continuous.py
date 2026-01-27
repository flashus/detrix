#!/usr/bin/env python3
"""
Continuous Trading Bot for Manual TUI Testing

This script runs indefinitely until Ctrl+C, generating trading events
that can be observed via Detrix metrics.

Usage:
  python -m debugpy --listen 5678 fixtures/detrix_example_app_continuous.py

Then connect Detrix and add metrics to observe values.
"""

import time
import random
import signal
import sys

# Global flag for graceful shutdown
running = True


def signal_handler(sig, frame):
    global running
    print("\n\nShutting down trading bot...")
    running = False


signal.signal(signal.SIGINT, signal_handler)
signal.signal(signal.SIGTERM, signal_handler)


class Portfolio:
    """Track portfolio state"""
    def __init__(self):
        self.cash = 100000.0
        self.positions = {}
        self.total_trades = 0
        self.total_pnl = 0.0


def place_order(portfolio, symbol, quantity, price, side):
    """Place a trading order - line 40 is good for metrics"""
    order_id = random.randint(10000, 99999)
    order_value = quantity * price

    # METRIC POINT: Capture order details
    order_info = {
        "order_id": order_id,
        "symbol": symbol,
        "quantity": quantity,
        "price": price,
        "side": side,
        "value": order_value
    }

    return order_id, order_value


def execute_trade(portfolio, symbol, quantity, price, side):
    """Execute a trade and update portfolio - line 58 is good for metrics"""
    cost = quantity * price

    if side == "BUY":
        portfolio.cash -= cost
        portfolio.positions[symbol] = portfolio.positions.get(symbol, 0) + quantity
    else:
        portfolio.cash += cost
        portfolio.positions[symbol] = portfolio.positions.get(symbol, 0) - quantity

    portfolio.total_trades += 1

    # METRIC POINT: Capture trade execution
    trade_result = {
        "symbol": symbol,
        "side": side,
        "quantity": quantity,
        "price": price,
        "new_position": portfolio.positions[symbol],
        "remaining_cash": portfolio.cash
    }

    return trade_result


def calculate_pnl(portfolio, current_prices):
    """Calculate portfolio P&L - line 82 is good for metrics"""
    portfolio_value = portfolio.cash

    for symbol, qty in portfolio.positions.items():
        if symbol in current_prices:
            portfolio_value += qty * current_prices[symbol]

    # METRIC POINT: Capture P&L calculation
    pnl_data = {
        "cash": portfolio.cash,
        "positions": dict(portfolio.positions),
        "portfolio_value": portfolio_value,
        "total_trades": portfolio.total_trades
    }

    return portfolio_value


def get_market_price(symbol, base_prices):
    """Simulate market price with random fluctuation - line 100"""
    base = base_prices.get(symbol, 100.0)
    fluctuation = random.uniform(-0.02, 0.02)  # +/- 2%
    current_price = base * (1 + fluctuation)
    return round(current_price, 2)


def main():
    """Main trading loop - runs continuously"""
    print("=" * 60)
    print("Detrix TUI Test - Continuous Trading Bot")
    print("=" * 60)
    print()
    print("This bot will run continuously until Ctrl+C")
    print("Connect Detrix and add metrics to observe values:")
    print()
    print("  Suggested metrics:")
    print("    - order_info at line 48 (order details)")
    print("    - trade_result at line 74 (trade execution)")
    print("    - pnl_data at line 95 (portfolio P&L)")
    print("    - current_price at line 109 (market prices)")
    print()
    print("=" * 60)
    print()

    # Initialize
    portfolio = Portfolio()
    symbols = ["BTCUSD", "ETHUSD", "SOLUSD", "AVAXUSD", "DOTUSD"]
    base_prices = {
        "BTCUSD": 45000.0,
        "ETHUSD": 2500.0,
        "SOLUSD": 100.0,
        "AVAXUSD": 35.0,
        "DOTUSD": 7.0
    }

    iteration = 0

    while running:
        iteration += 1
        symbol = random.choice(symbols)
        current_price = get_market_price(symbol, base_prices)
        side = random.choice(["BUY", "SELL"])
        quantity = random.randint(1, 10)

        # Place and execute order
        order_id, order_value = place_order(portfolio, symbol, quantity, current_price, side)
        trade_result = execute_trade(portfolio, symbol, quantity, current_price, side)

        # Calculate current P&L
        current_prices = {s: get_market_price(s, base_prices) for s in symbols}
        portfolio_value = calculate_pnl(portfolio, current_prices)

        # Print status
        print(f"[{iteration:5d}] {side:4s} {quantity:2d}x {symbol:8s} @ ${current_price:>10.2f} | "
              f"Cash: ${portfolio.cash:>12.2f} | Portfolio: ${portfolio_value:>12.2f}")

        # Vary the sleep time for realistic trading patterns
        sleep_time = random.uniform(0.5, 2.0)

        # Check running flag during sleep (allows faster shutdown)
        sleep_end = time.time() + sleep_time
        while running and time.time() < sleep_end:
            time.sleep(0.1)

    print()
    print("=" * 60)
    print(f"Final Results:")
    print(f"  Total Trades: {portfolio.total_trades}")
    print(f"  Final Cash: ${portfolio.cash:.2f}")
    print(f"  Positions: {portfolio.positions}")
    print("=" * 60)


if __name__ == "__main__":
    main()
