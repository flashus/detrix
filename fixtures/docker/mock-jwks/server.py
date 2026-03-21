#!/usr/bin/env python3
"""Minimal JWKS HTTP server for Docker E2E testing.

Serves the pre-generated JWKS JSON at:
  /jwks
  /.well-known/jwks.json
"""

import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

JWKS_PATH = os.path.join(os.path.dirname(__file__), "jwks.json")

with open(JWKS_PATH) as f:
    JWKS_CONTENT = f.read().encode()


class JwksHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass  # suppress request logs

    def do_GET(self):
        if self.path in ("/jwks", "/.well-known/jwks.json"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(JWKS_CONTENT)))
            self.end_headers()
            self.wfile.write(JWKS_CONTENT)
        else:
            self.send_response(404)
            self.end_headers()


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "8080"))
    server = HTTPServer(("0.0.0.0", port), JwksHandler)
    print(f"Mock JWKS server listening on :{port}", flush=True)
    server.serve_forever()
