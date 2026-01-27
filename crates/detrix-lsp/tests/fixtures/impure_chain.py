"""
Test fixture: Impure function call chain (3 levels deep)

Call chain: process_order -> save_to_database -> write_log
All functions have side effects (file I/O, print, global state).
"""

# Global state - side effect
_order_count = 0
_order_history = []


def write_log(message: str) -> None:
    """Level 3: Impure function - writes to stdout (side effect)."""
    print(f"[LOG] {message}")


def save_to_database(order_id: str, data: dict) -> bool:
    """Level 2: Impure function - calls write_log (has print side effect)."""
    global _order_history
    _order_history.append({"id": order_id, "data": data})
    write_log(f"Saved order {order_id}")
    return True


def process_order(order_id: str, items: list, customer: str) -> dict:
    """Level 1: Impure function - modifies global state and calls impure functions."""
    global _order_count
    _order_count += 1

    order_data = {
        "order_id": order_id,
        "items": items,
        "customer": customer,
        "sequence": _order_count,
    }

    save_to_database(order_id, order_data)

    return order_data


# Additional impure functions for testing
def read_config(path: str) -> dict:
    """Impure function - reads from file system."""
    with open(path, "r") as f:
        return {"content": f.read()}


def execute_command(cmd: str) -> None:
    """Impure function - executes system command."""
    import os
    os.system(cmd)


def send_notification(message: str) -> None:
    """Impure function - uses print (I/O side effect)."""
    print(f"NOTIFICATION: {message}")
