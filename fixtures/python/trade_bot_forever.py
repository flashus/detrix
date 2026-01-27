#!/usr/bin/env python3
"""Long-running trading bot for Detrix testing - runs until Ctrl+C"""
import time
import random
import signal
import sys

running = True

def signal_handler(signum, frame):
    global running
    print("\nReceived shutdown signal, stopping...")
    running = False

signal.signal(signal.SIGTERM, signal_handler)
signal.signal(signal.SIGINT, signal_handler)

def place_order(symbol, quantity, price):
    """Place a trading order"""
    order_id = random.randint(1000, 9999)
    total = quantity * price
    print(f"Order #{order_id}: {symbol} x{quantity} @ ${price:.2f} = ${total:.2f}")
    return order_id

def calculate_pnl(entry_price, current_price, quantity):
    """Calculate profit/loss"""
    pnl = (current_price - entry_price) * quantity
    return pnl

def main():
    global running
    print("Trading bot started - runs forever until Ctrl+C")
    print("Add metrics with Detrix to observe values!")
    print()
    
    symbols = ["BTCUSD", "ETHUSD", "SOLUSD"]
    iteration = 0
    
    while running:
        iteration += 1
        symbol = random.choice(symbols)
        quantity = random.randint(1, 50)
        price = random.uniform(100, 1000)
        
        # Line 42 - place_order call
        order_id = place_order(symbol, quantity, price)
        
        # Line 45 - calculate pnl
        entry_price = price
        current_price = price * random.uniform(0.95, 1.05)
        pnl = calculate_pnl(entry_price, current_price, quantity)
        
        print(f"  -> P&L: ${pnl:.2f} (iteration {iteration})")
        print()
        
        time.sleep(3)  # Slower so we can see events
    
    print("Trading bot stopped!")

if __name__ == "__main__":
    main()
