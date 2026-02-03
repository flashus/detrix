"""Token handling for Detrix Python client."""

import os
import stat
import sys
import warnings
from pathlib import Path

from .config import get_detrix_home, get_token_file_path


def _check_token_file_permissions(path: Path) -> bool:
    """Verify token file has secure permissions (0600 or 0400).

    On Unix systems, checks if group or other has any permissions and issues
    a warning if so. Token files should only be readable by the owner.

    Args:
        path: Path to the token file

    Returns:
        True if permissions check passed or skipped, False on error
    """
    # Windows doesn't have Unix-style permissions
    if sys.platform == "win32":
        return True  # Skip check on Windows (documented limitation)

    try:
        mode = path.stat().st_mode
        # Check if group or other has any permissions
        if mode & (stat.S_IRWXG | stat.S_IRWXO):
            warnings.warn(
                f"Token file {path} has insecure permissions "
                f"(mode={oct(mode & 0o777)}). Should be 0600 or 0400.",
                UserWarning,
                stacklevel=3,
            )
        return True
    except OSError:
        return False


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
            _check_token_file_permissions(token_file)
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
