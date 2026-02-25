"""
Weather API Server

Serves weather data for cities on port 9777.

Usage:
    python weather_api.py          # default
    python weather_api.py --v1     # legacy format
"""

import json
import random
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler

V1_CITY_WEATHER = {
    "miami":    {"temperature": 88, "conditions": "clear",  "wind_speed": 8},
    "seattle":  {"temperature": 52, "conditions": "rain",   "wind_speed": 12},
    "denver":   {"temperature": 35, "conditions": "snow",   "wind_speed": 22},
    "chicago":  {"temperature": 45, "conditions": "cloudy", "wind_speed": 18},
    "phoenix":  {"temperature": 105, "conditions": "clear", "wind_speed": 3},
    "boston":    {"temperature": 40, "conditions": "storm",  "wind_speed": 35},
    "austin":   {"temperature": 78, "conditions": "clear",  "wind_speed": 6},
    "portland": {"temperature": 48, "conditions": "rain",   "wind_speed": 15},
}

V1_DEFAULT = {"temperature": 65, "conditions": "cloudy", "wind_speed": 10}

V2_CITY_WEATHER = {
    "miami":    {"temperature": 88, "conditions": ["clear"],           "wind_speed": "8 mph"},
    "seattle":  {"temperature": 52, "conditions": ["rain"],            "wind_speed": "12 mph"},
    "denver":   {"temperature": 35, "conditions": ["snow"],            "wind_speed": "22 mph"},
    "chicago":  {"temperature": 45, "conditions": ["cloudy"],          "wind_speed": "18 mph"},
    "phoenix":  {"temperature": 105, "conditions": ["clear"],          "wind_speed": "3 mph"},
    "boston":    {"temperature": 40, "conditions": ["storm"],           "wind_speed": "35 mph"},
    "austin":   {"temperature": 78, "conditions": ["clear"],           "wind_speed": "6 mph"},
    "portland": {"temperature": 48, "conditions": ["rain", "cloudy"],  "wind_speed": "15 mph"},
}

V2_DEFAULT = {"temperature": 65, "conditions": ["cloudy"], "wind_speed": "10 mph"}


def _make_handler(city_data: dict, default: dict):
    class WeatherHandler(BaseHTTPRequestHandler):
        def do_GET(self):
            parts = self.path.strip("/").split("/")
            if len(parts) == 3 and parts[0] == "v2" and parts[1] == "weather":
                city = parts[2].lower()
                weather = dict(city_data.get(city, default))
                weather["temperature"] += random.randint(-3, 3)

                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps(weather).encode())
            else:
                self.send_response(404)
                self.end_headers()

        def log_message(self, *_args):
            pass

    return WeatherHandler


if __name__ == "__main__":
    version = 2
    if "--v1" in sys.argv:
        version = 1

    if version == 1:
        handler = _make_handler(V1_CITY_WEATHER, V1_DEFAULT)
    else:
        handler = _make_handler(V2_CITY_WEATHER, V2_DEFAULT)

    label = "v1" if version == 1 else "v2"
    print(f"Weather API ({label}) running on http://127.0.0.1:9777")
    print("Try: curl http://127.0.0.1:9777/v2/weather/miami")

    server = HTTPServer(("127.0.0.1", 9777), handler)
    server.serve_forever()
