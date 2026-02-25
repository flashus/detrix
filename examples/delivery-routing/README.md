# Delivery Routing Demo

A delivery service routes packages based on weather-driven risk scores.
The external weather API silently changed its response format. The app
keeps running with zero errors — but every package takes the slow,
expensive route, costing the business money while appearing to work perfectly.

## The Scenario

A logistics company uses weather data to pick delivery routes:

| Route    | Risk Range | Cost    | When Used                 |
|----------|------------|---------|---------------------------|
| express  | 0.0 – 0.3 | $12.99  | Clear skies, low wind     |
| standard | 0.3 – 0.6 | $24.99  | Moderate conditions       |
| cautious | 0.6 – 1.0 | $49.99  | Bad weather, high risk    |

After a "routine" API upgrade, **express routes vanish**. Even on a
sunny, calm day in Miami — every package goes standard or cautious.
No errors in logs, no crashes, no alerts. Just higher costs.

## Quick Start

**Terminal 1** — Start the weather API:

```bash
cd examples/mock-services
python weather_api.py
```

**Terminal 2** — Start the delivery app with debugpy:

```bash
cd examples/delivery-routing
python -m debugpy --listen 5678 app.py
```

Watch the output — every package gets `standard` route, no `express` ever.

## Debugging with Detrix

This is the intended workflow. The app is running, something is wrong
with routing costs, and you need to figure out why — without stopping
the app or adding print statements.

### Step 1: Ask the agent

```
Our delivery app never picks express routes even for sunny cities.
Risk scores cluster around 0.3-0.5 for all cities. The app runs at
examples/delivery-routing/app.py and debugpy is on port 5678.
Use Detrix to find what's wrong.
```

### Step 2: Agent works

The agent will:

1. Connect to debugpy at 5678
2. Observe raw API response at `weather_client.py` — sees `['clear']` (a list, not string)
3. Observe `cond_risk` at `risk_calculator.py` — sees `0.5` (default) for every request
4. Explain the bug: `str(["clear"])` produces `"['clear']"` which never matches the lookup table
5. Suggest the fix

### Why This Bug Is Hard Without Detrix

- **No errors** — `str()` on a list is valid Python
- **Code reads correctly** — `str(raw.get("conditions"))` looks reasonable
- **Reasonable defaults mask the issue** — 0.5 for unknown conditions seems safe
- **Wind speed is a red herring** — it also changed format but `_parse_wind()` handles it correctly
- **Type looks correct** — the result IS a string; it's just the wrong string

## File Overview

| File                | Purpose                              |
|---------------------|--------------------------------------|
| `app.py`            | Entry point, batch processing loop   |
| `weather_client.py` | Fetches + normalizes weather from API|
| `risk_calculator.py`| Scores risk from weather data        |
| `router.py`         | Picks route based on risk score      |
