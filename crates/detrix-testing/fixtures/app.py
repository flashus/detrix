# Application fixture file for Detrix tests
# This file provides sample Python code for HTTP request/response testing
# Important lines are documented for test reference
#
# Line References:
# - Line 101: request = Request() - used by tests requiring Request

"""
Detrix Application Test Fixture Module

This module provides mock HTTP request/response handling, middleware patterns,
and API routing concepts for testing the Detrix observability platform.

Line 101: request = Request() is referenced by integration tests.
"""

from typing import Dict, List, Optional, Any, Callable
from dataclasses import dataclass, field


# ============================================================================
# HTTP Request/Response Models
# ============================================================================

class Request:
    """Mock HTTP request object for testing."""

    def __init__(self, method: str = "GET", path: str = "/"):
        self.method = method
        self.path = path
        self.headers: Dict[str, str] = {}
        self.query_params: Dict[str, str] = {}
        self.body: Optional[bytes] = None
        self.user_id: Optional[int] = None
        self.session_id: Optional[str] = None
        self.client_ip: str = "127.0.0.1"

    def get_header(self, name: str, default: str = "") -> str:
        """Get a header value by name."""
        return self.headers.get(name.lower(), default)

    def set_header(self, name: str, value: str) -> None:
        """Set a header value."""
        self.headers[name.lower()] = value

    def is_authenticated(self) -> bool:
        """Check if request has valid authentication."""
        return self.user_id is not None


class Response:
    """Mock HTTP response object for testing."""

    def __init__(self, status_code: int = 200):
        self.status_code = status_code
        self.headers: Dict[str, str] = {}
        self.body: Optional[bytes] = None
        self.content_type: str = "application/json"

    def set_json(self, data: Any) -> None:
        """Set JSON response body."""
        import json
        self.body = json.dumps(data).encode("utf-8")
        self.content_type = "application/json"


# ============================================================================
# Middleware and Routing
# ============================================================================

class Middleware:
    """Base middleware class."""

    def process_request(self, request: Request) -> Optional[Response]:
        """Process incoming request. Return Response to short-circuit."""
        return None

    def process_response(self, request: Request, response: Response) -> Response:
        """Process outgoing response."""
        return response


class AuthMiddleware(Middleware):
    """Authentication middleware."""

    def __init__(self, required_paths: List[str] = None):
        self.required_paths = required_paths or ["/api/"]

    def process_request(self, request: Request) -> Optional[Response]:
        """Check authentication for protected paths."""
        for path in self.required_paths:
            if request.path.startswith(path) and not request.is_authenticated():
                return Response(status_code=401)
        return None


# Additional spacing to ensure line 101 alignment



request = Request()  # Line 101 - used by tests


class Router:
    """Simple request router."""

    def __init__(self):
        self.routes: Dict[str, Dict[str, Callable]] = {}

    def add_route(self, method: str, path: str, handler: Callable) -> None:
        """Register a route handler."""
        if path not in self.routes:
            self.routes[path] = {}
        self.routes[path][method.upper()] = handler

    def get(self, path: str) -> Callable:
        """Decorator for GET routes."""
        def decorator(handler: Callable) -> Callable:
            self.add_route("GET", path, handler)
            return handler
        return decorator

    def post(self, path: str) -> Callable:
        """Decorator for POST routes."""
        def decorator(handler: Callable) -> Callable:
            self.add_route("POST", path, handler)
            return handler
        return decorator


# ============================================================================
# Application Context
# ============================================================================

@dataclass
class AppContext:
    """Application context for request handling."""
    request: Request
    config: Dict[str, Any] = field(default_factory=dict)
    user: Optional[Any] = None
    start_time: float = 0.0


def create_context(req: Request) -> AppContext:
    """Create an application context from a request."""
    import time
    return AppContext(request=req, start_time=time.time())


def handle_request(ctx: AppContext) -> Response:
    """Main request handler."""
    response = Response(200)
    response.set_json({"status": "ok", "path": ctx.request.path})
    return response

