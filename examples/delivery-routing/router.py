"""
Delivery Router

Selects the optimal delivery route for each package based on
weather-driven risk assessment.

Routes (cheapest to most expensive):
  - express:   risk 0.0 - 0.3  (fast, cheap)
  - standard:  risk 0.3 - 0.6  (balanced)
  - cautious:  risk 0.6 - 1.0  (slow, expensive, insured)
"""

from weather_client import fetch_weather
from risk_calculator import calculate_risk

ROUTES = [
    ("express",  0.0, 0.3),
    ("standard", 0.3, 0.6),
    ("cautious", 0.6, 1.0),
]

ROUTE_COSTS = {
    "express":  12.99,
    "standard": 24.99,
    "cautious": 49.99,
}


def route_package(city: str, package_id: str) -> dict:
    """Determine the best route for a package given the destination city."""
    weather = fetch_weather(city)
    risk = calculate_risk(weather)
    route = next(r[0] for r in ROUTES if r[1] <= risk < r[2])
    cost = ROUTE_COSTS[route]

    return {
        "package": package_id,
        "city": city,
        "route": route,
        "risk": risk,
        "cost": cost,
    }
