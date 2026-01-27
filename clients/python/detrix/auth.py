"""Token handling for Detrix Python client."""

import os
from pathlib import Path

from .config import get_detrix_home, get_token_file_path


def discover_auth_token(detrix_home: Path | None = None) -> str | None:
    """Discover authentication token from environment or file.

    Priority:
    1. DETRIX_TOKEN environment variable
    2. ~/detrix/mcp-token file

    Args:
        detrix_home: Optional Detrix home directory override

    Returns:
        Authentication token or None if not found
    """
    # Check environment variable first
    env_token = os.environ.get("DETRIX_TOKEN")
    if env_token:
        return env_token.strip()

    # Try reading from token file
    home = detrix_home or get_detrix_home()
    token_file = get_token_file_path(home)

    try:
        if token_file.exists():
            return token_file.read_text().strip()
    except (OSError, PermissionError):
        pass

    return None


def get_auth_headers(token: str | None) -> dict[str, str]:
    """Get authorization headers for HTTP requests.

    Args:
        token: Authentication token

    Returns:
        Headers dictionary with Authorization header if token provided
    """
    if token:
        return {"Authorization": f"Bearer {token}"}
    return {}


def is_localhost(host: str) -> bool:
    """Check if host is localhost.

    Args:
        host: Host string to check

    Returns:
        True if host is localhost
    """
    # Note: 0.0.0.0 is intentionally excluded - it's a bind address, not a client address.
    # A client connecting "from" 0.0.0.0 would indicate a spoofed or malformed request.
    localhost_names = {"localhost", "127.0.0.1", "::1"}
    return host.lower() in localhost_names
