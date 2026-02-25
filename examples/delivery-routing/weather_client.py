"""
Weather Client

Fetches weather data from the external API and normalizes it into a
standard format used by the rest of the application.

This module was written when the API returned:
    {"temperature": 72, "conditions": "clear", "wind_speed": 5}

The API recently migrated to v2 and changed the response format.
The code still runs without errors, but...
"""

import re
import urllib.request
import json

API_BASE = "http://127.0.0.1:9777/v2/weather"


def fetch_weather(city: str) -> dict:
    """Fetch current weather for a city and return normalized data."""
    url = f"{API_BASE}/{city}"
    with urllib.request.urlopen(url) as resp:
        raw = json.loads(resp.read().decode())
    return normalize(raw)


def normalize(raw: dict) -> dict:
    """Normalize raw API response into our internal weather format."""
    return {
        "temperature": float(raw.get("temperature", 0)),
        "conditions": str(raw.get("conditions", "unknown")),
        "wind_speed": _parse_wind(raw.get("wind_speed", 0)),
    }


def _parse_wind(val) -> float:
    """Parse wind speed from various formats (int, float, or string with units)."""
    if isinstance(val, (int, float)):
        return float(val)
    match = re.search(r"[\d.]+", str(val))
    return float(match.group()) if match else 0.0
