# Auth fixture file for Detrix tests
# This file provides sample Python code for authentication/authorization testing
# Important lines are documented for test reference
#
# Line References:
# - Line 129: user = User() - used by tests requiring User

"""
Detrix Authentication Test Fixture Module

This module provides mock user authentication, session management, and
permission checking for testing the Detrix observability platform.

Line 129: user = User() is referenced by integration tests.
"""

from typing import Dict, List, Optional, Set
from dataclasses import dataclass, field
import hashlib
import secrets


# ============================================================================
# User Model
# ============================================================================

class User:
    """Mock user object for authentication testing."""

    def __init__(self, user_id: Optional[int] = None, username: str = ""):
        self.id = user_id
        self.username = username
        self.email: Optional[str] = None
        self.session_id: Optional[str] = None
        self.roles: List[str] = []
        self.permissions: Set[str] = set()
        self.is_active: bool = True

    def has_role(self, role: str) -> bool:
        """Check if user has a specific role."""
        return role in self.roles

    def has_permission(self, permission: str) -> bool:
        """Check if user has a specific permission."""
        return permission in self.permissions

    def add_role(self, role: str) -> None:
        """Add a role to the user."""
        if role not in self.roles:
            self.roles.append(role)

    def __repr__(self) -> str:
        return f"User(id={self.id}, username={self.username!r})"


# ============================================================================
# Session Management
# ============================================================================

@dataclass
class Session:
    """User session data."""
    session_id: str
    user_id: int
    created_at: float
    expires_at: float
    data: Dict[str, str] = field(default_factory=dict)

    def is_expired(self, current_time: float) -> bool:
        """Check if session has expired."""
        return current_time > self.expires_at


class SessionStore:
    """In-memory session storage."""

    def __init__(self):
        self.sessions: Dict[str, Session] = {}

    def create_session(self, user_id: int, ttl_seconds: int = 3600) -> Session:
        """Create a new session for a user."""
        import time
        session_id = secrets.token_urlsafe(32)
        now = time.time()
        session = Session(
            session_id=session_id,
            user_id=user_id,
            created_at=now,
            expires_at=now + ttl_seconds
        )
        self.sessions[session_id] = session
        return session

    def get_session(self, session_id: str) -> Optional[Session]:
        """Retrieve a session by ID."""
        return self.sessions.get(session_id)

    def delete_session(self, session_id: str) -> bool:
        """Delete a session."""
        if session_id in self.sessions:
            del self.sessions[session_id]
            return True
        return False


# ============================================================================
# Authentication Functions
# ============================================================================

def hash_password(password: str, salt: str = "") -> str:
    """Hash a password with optional salt."""
    combined = f"{salt}{password}".encode("utf-8")
    return hashlib.sha256(combined).hexdigest()


def verify_password(password: str, hashed: str, salt: str = "") -> bool:
    """Verify a password against its hash."""
    return hash_password(password, salt) == hashed


def authenticate(username: str, password: str) -> Optional[User]:
    """Authenticate a user by username and password."""
    # Mock authentication - always succeeds for non-empty credentials
    if username and password:
        user = User(user_id=1, username=username)
        user.email = f"{username}@example.com"
        return user
    return None
user = User()  # Line 129 - used by tests


# ============================================================================
# Authorization
# ============================================================================

class PermissionChecker:
    """Check user permissions against required permissions."""

    def __init__(self, required: List[str]):
        self.required = set(required)

    def check(self, user: User) -> bool:
        """Check if user has all required permissions."""
        return self.required.issubset(user.permissions)


def require_roles(*roles: str):
    """Decorator to require specific roles."""
    def decorator(func):
        def wrapper(user: User, *args, **kwargs):
            for role in roles:
                if not user.has_role(role):
                    raise PermissionError(f"Missing role: {role}")
            return func(user, *args, **kwargs)
        return wrapper
    return decorator


@require_roles("admin")
def admin_action(user: User, action: str) -> Dict[str, str]:
    """Perform an admin-only action."""
    return {"action": action, "performed_by": user.username}


# ============================================================================
# Token Management
# ============================================================================

def generate_token(user_id: int) -> str:
    """Generate an authentication token."""
    return f"token_{user_id}_{secrets.token_hex(16)}"


def validate_token(token: str) -> Optional[int]:
    """Validate a token and return user_id if valid."""
    if token.startswith("token_"):
        parts = token.split("_")
        if len(parts) >= 2:
            try:
                return int(parts[1])
            except ValueError:
                pass
    return None

