"""
Risk Calculator

Calculates a delivery risk score (0.0 to 1.0) based on weather conditions.
Used by the router to select appropriate delivery routes.

Risk factors:
  - Temperature extremes (30% weight)
  - Weather conditions (50% weight) - most important factor
  - Wind speed (20% weight)
"""

CONDITION_RISKS = {
    "clear": 0.1,
    "cloudy": 0.2,
    "rain": 0.6,
    "snow": 0.8,
    "storm": 0.95,
}


def calculate_risk(weather: dict) -> float:
    """Calculate overall delivery risk score from weather data."""
    temp_risk = _temperature_risk(weather["temperature"])
    cond_risk = CONDITION_RISKS.get(weather["conditions"], 0.5)
    wind_risk = min(weather["wind_speed"] / 50.0, 1.0)

    return round(0.3 * temp_risk + 0.5 * cond_risk + 0.2 * wind_risk, 3)


def _temperature_risk(temp: float) -> float:
    """Score temperature risk. Extremes are risky for deliveries."""
    if temp < 20:
        return 0.7
    if temp > 100:
        return 0.7
    return 0.1
