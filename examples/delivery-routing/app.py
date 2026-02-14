"""
Delivery Routing Service

Main application that continuously processes package deliveries.
Fetches weather data for destination cities and routes packages
through the optimal delivery channel.

Requires a weather API running at http://127.0.0.1:9777
"""

import sys
import time
import random
from router import route_package

CITIES = ["miami", "seattle", "denver", "chicago", "phoenix", "boston", "austin", "portland"]

TOTAL_COST = 0.0
TOTAL_PACKAGES = 0


def generate_package_id() -> str:
    return f"PKG-{random.randint(10000, 99999)}"


def process_batch():
    """Process a batch of 3-5 packages."""
    global TOTAL_COST, TOTAL_PACKAGES

    batch_size = random.randint(3, 5)
    batch_results = []

    for _ in range(batch_size):
        city = random.choice(CITIES)
        pkg_id = generate_package_id()
        result = route_package(city, pkg_id)
        batch_results.append(result)

        TOTAL_PACKAGES += 1
        TOTAL_COST += result["cost"]

    return batch_results


def print_batch_summary(results: list):
    """Print a summary of the processed batch."""
    print(f"\n{'=' * 65}")
    print(f"  Batch processed: {len(results)} packages")
    print(f"  {'Package':<14} {'City':<12} {'Route':<12} {'Risk':<8} {'Cost':>8}")
    print(f"  {'-' * 58}")

    for r in results:
        print(f"  {r['package']:<14} {r['city']:<12} {r['route']:<12} {r['risk']:<8} ${r['cost']:>7.2f}")

    batch_cost = sum(r["cost"] for r in results)
    routes_used = set(r["route"] for r in results)
    print(f"  {'-' * 58}")
    print(f"  Batch cost: ${batch_cost:.2f}  |  Routes used: {', '.join(sorted(routes_used))}")
    print(f"  Running total: {TOTAL_PACKAGES} packages, ${TOTAL_COST:.2f}")
    print(f"{'=' * 65}")


def main():
    print("Delivery Routing Service")
    print("Processing deliveries (Ctrl+C to stop)...\n")

    try:
        while True:
            results = process_batch()
            print_batch_summary(results)
            time.sleep(3)
    except KeyboardInterrupt:
        print(f"\n\nShutting down.")
        print(f"Final stats: {TOTAL_PACKAGES} packages routed, total cost ${TOTAL_COST:.2f}")
        avg = TOTAL_COST / max(TOTAL_PACKAGES, 1)
        print(f"Average cost per package: ${avg:.2f}")
        sys.exit(0)


if __name__ == "__main__":
    main()
