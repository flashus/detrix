"""
Test fixture: Pure function call chain (3 levels deep)

Call chain: calculate_total -> compute_subtotal -> compute_item_price
All functions are pure (no side effects).
"""
import math


def compute_item_price(quantity: int, unit_price: float) -> float:
    """Level 3: Pure function - computes item price using math operations."""
    return round(quantity * unit_price, 2)


def compute_subtotal(items: list[tuple[int, float]]) -> float:
    """Level 2: Pure function - calls compute_item_price for each item."""
    total = 0.0
    for quantity, price in items:
        total += compute_item_price(quantity, price)
    return total


def calculate_total(items: list[tuple[int, float]], tax_rate: float) -> float:
    """Level 1: Pure function - calls compute_subtotal and applies tax."""
    subtotal = compute_subtotal(items)
    tax = math.floor(subtotal * tax_rate * 100) / 100
    return subtotal + tax


# Additional pure helper functions for testing
def format_currency(amount: float) -> str:
    """Pure function - formats amount as currency string."""
    return f"${amount:.2f}"


def validate_items(items: list) -> bool:
    """Pure function - validates items list."""
    return len(items) > 0 and all(
        isinstance(item, tuple) and len(item) == 2 for item in items
    )
