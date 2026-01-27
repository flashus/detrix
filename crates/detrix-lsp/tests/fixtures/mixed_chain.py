"""
Test fixture: Mixed purity function call chain

Call chain: analyze_data -> process_items -> log_result
Starts pure but ends with impure function call.
"""
import json


def log_result(result: dict) -> None:
    """Impure function - prints to stdout."""
    print(f"Result: {json.dumps(result)}")


def process_items(items: list[int]) -> dict:
    """Mixed function - does pure computation but calls impure log_result."""
    total = sum(items)
    average = total / len(items) if items else 0
    result = {"total": total, "average": average, "count": len(items)}
    log_result(result)  # Impure call!
    return result


def analyze_data(data: list[int]) -> dict:
    """Entry point - appears pure but transitively calls impure function."""
    filtered = [x for x in data if x > 0]
    sorted_data = sorted(filtered)
    return process_items(sorted_data)


# Pure function in the same file for comparison
def compute_stats(numbers: list[int]) -> dict:
    """Pure function - only uses pure operations."""
    if not numbers:
        return {"min": 0, "max": 0, "sum": 0}
    return {
        "min": min(numbers),
        "max": max(numbers),
        "sum": sum(numbers),
    }
