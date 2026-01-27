"""Configuration management for Detrix Python client."""

import os
import socket
from pathlib import Path


def get_detrix_home(override: str | None = None) -> Path:
    """Get the Detrix home directory.

    Args:
        override: Optional path override

    Returns:
        Path to Detrix home directory (default: ~/detrix)
    """
    if override:
        return Path(override)

    env_home = os.environ.get("DETRIX_HOME")
    if env_home:
        return Path(env_home)

    return Path.home() / "detrix"


def get_token_file_path(detrix_home: Path | None = None) -> Path:
    """Get the path to the MCP token file.

    Args:
        detrix_home: Optional Detrix home directory

    Returns:
        Path to mcp-token file
    """
    home = detrix_home or get_detrix_home()
    return home / "mcp-token"


def get_free_port() -> int:
    """Get a free port assigned by the OS.

    Returns:
        Available port number
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        s.listen(1)
        port: int = s.getsockname()[1]
    return port


def generate_connection_name(name: str) -> str:
    """Generate a connection name with PID suffix.

    Args:
        name: Base name for the connection

    Returns:
        Connection name in format "{name}-{pid}"
    """
    pid = os.getpid()
    base_name = name or "detrix-client"
    return f"{base_name}-{pid}"


def get_env_config() -> dict[str, str | None]:
    """Get configuration from environment variables.

    Supported environment variables:
        DETRIX_NAME: Connection name
        DETRIX_CONTROL_HOST: Host for control plane server
        DETRIX_CONTROL_PORT: Port for control plane server
        DETRIX_DEBUG_PORT: Port for debugpy
        DETRIX_DAEMON_URL: URL of the Detrix daemon
        DETRIX_TOKEN: Authentication token
        DETRIX_HEALTH_CHECK_TIMEOUT: Timeout for daemon health checks (seconds)
        DETRIX_REGISTER_TIMEOUT: Timeout for connection registration (seconds)
        DETRIX_UNREGISTER_TIMEOUT: Timeout for connection unregistration (seconds)

    Returns:
        Dictionary with configuration values from environment
    """
    return {
        "name": os.environ.get("DETRIX_NAME"),
        "control_host": os.environ.get("DETRIX_CONTROL_HOST"),
        "control_port": os.environ.get("DETRIX_CONTROL_PORT"),
        "debug_port": os.environ.get("DETRIX_DEBUG_PORT"),
        "daemon_url": os.environ.get("DETRIX_DAEMON_URL"),
        "token": os.environ.get("DETRIX_TOKEN"),
        "health_check_timeout": os.environ.get("DETRIX_HEALTH_CHECK_TIMEOUT"),
        "register_timeout": os.environ.get("DETRIX_REGISTER_TIMEOUT"),
        "unregister_timeout": os.environ.get("DETRIX_UNREGISTER_TIMEOUT"),
    }
