"""Tests for configuration management."""

import os
from pathlib import Path
from unittest import mock

from detrix.config import (
    generate_connection_name,
    get_detrix_home,
    get_env_config,
    get_free_port,
    get_token_file_path,
)


class TestGetDetrixHome:
    """Test get_detrix_home function."""

    def test_override_takes_precedence(self):
        """Test that override parameter takes precedence."""
        result = get_detrix_home("/custom/path")
        assert result == Path("/custom/path")

    def test_env_var_used_when_set(self):
        """Test that DETRIX_HOME env var is used."""
        with mock.patch.dict(os.environ, {"DETRIX_HOME": "/env/path"}):
            result = get_detrix_home()
            assert result == Path("/env/path")

    def test_default_is_home_detrix(self):
        """Test default is ~/detrix."""
        with mock.patch.dict(os.environ, {}, clear=True):
            # Remove DETRIX_HOME if present
            os.environ.pop("DETRIX_HOME", None)
            result = get_detrix_home()
            assert result == Path.home() / "detrix"


class TestGetTokenFilePath:
    """Test get_token_file_path function."""

    def test_returns_mcp_token_in_home(self):
        """Test token file path is in detrix home."""
        home = Path("/custom/home")
        result = get_token_file_path(home)
        assert result == Path("/custom/home/mcp-token")

    def test_uses_default_home(self):
        """Test uses default detrix home."""
        with mock.patch.dict(os.environ, {}, clear=True):
            os.environ.pop("DETRIX_HOME", None)
            result = get_token_file_path()
            assert result == Path.home() / "detrix" / "mcp-token"


class TestGetFreePort:
    """Test get_free_port function."""

    def test_returns_valid_port(self):
        """Test returns a valid port number."""
        port = get_free_port()
        assert isinstance(port, int)
        assert 1024 <= port <= 65535

    def test_returns_different_ports(self):
        """Test returns different ports on subsequent calls."""
        ports = {get_free_port() for _ in range(5)}
        # Should get mostly unique ports (might have some collisions)
        assert len(ports) >= 3


class TestGenerateConnectionName:
    """Test generate_connection_name function."""

    def test_with_name(self):
        """Test with custom name."""
        result = generate_connection_name("my-service")
        assert result.startswith("my-service-")
        assert str(os.getpid()) in result

    def test_without_name(self):
        """Test with empty name."""
        result = generate_connection_name("")
        assert result.startswith("detrix-client-")
        assert str(os.getpid()) in result


class TestGetEnvConfig:
    """Test get_env_config function."""

    def test_returns_env_values(self):
        """Test returns environment variable values."""
        with mock.patch.dict(os.environ, {
            "DETRIX_NAME": "test-name",
            "DETRIX_CONTROL_HOST": "192.168.1.1",
            "DETRIX_CONTROL_PORT": "9000",
            "DETRIX_DEBUG_PORT": "5679",
            "DETRIX_DAEMON_URL": "http://example.com:8090",
            "DETRIX_TOKEN": "secret-token",
        }):
            config = get_env_config()
            assert config["name"] == "test-name"
            assert config["control_host"] == "192.168.1.1"
            assert config["control_port"] == "9000"
            assert config["debug_port"] == "5679"
            assert config["daemon_url"] == "http://example.com:8090"
            assert config["token"] == "secret-token"

    def test_returns_none_for_unset_vars(self):
        """Test returns None for unset variables."""
        with mock.patch.dict(os.environ, {}, clear=True):
            # Remove any existing detrix env vars
            for key in list(os.environ.keys()):
                if key.startswith("DETRIX"):
                    del os.environ[key]
            config = get_env_config()
            assert config["name"] is None
            assert config["control_host"] is None

    def test_returns_timeout_env_values(self):
        """Test returns timeout environment variable values."""
        with mock.patch.dict(os.environ, {
            "DETRIX_HEALTH_CHECK_TIMEOUT": "3.5",
            "DETRIX_REGISTER_TIMEOUT": "10.0",
            "DETRIX_UNREGISTER_TIMEOUT": "1.5",
        }):
            config = get_env_config()
            assert config["health_check_timeout"] == "3.5"
            assert config["register_timeout"] == "10.0"
            assert config["unregister_timeout"] == "1.5"

    def test_timeout_env_vars_are_none_when_unset(self):
        """Test timeout env vars are None when not set."""
        with mock.patch.dict(os.environ, {}, clear=True):
            for key in list(os.environ.keys()):
                if key.startswith("DETRIX"):
                    del os.environ[key]
            config = get_env_config()
            assert config["health_check_timeout"] is None
            assert config["register_timeout"] is None
            assert config["unregister_timeout"] is None

    def test_verify_ssl_default_true(self):
        """Test DETRIX_VERIFY_SSL defaults to True."""
        with mock.patch.dict(os.environ, {}, clear=True):
            for key in list(os.environ.keys()):
                if key.startswith("DETRIX"):
                    del os.environ[key]
            config = get_env_config()
            assert config["verify_ssl"] is True

    def test_verify_ssl_false_values(self):
        """Test DETRIX_VERIFY_SSL recognizes false-like values."""
        for value in ("false", "0", "no", "off", "False", "FALSE", "NO"):
            with mock.patch.dict(os.environ, {"DETRIX_VERIFY_SSL": value}):
                config = get_env_config()
                assert config["verify_ssl"] is False, f"Expected False for {value!r}"

    def test_verify_ssl_true_values(self):
        """Test DETRIX_VERIFY_SSL treats other values as True."""
        for value in ("true", "1", "yes", "True", "anything"):
            with mock.patch.dict(os.environ, {"DETRIX_VERIFY_SSL": value}):
                config = get_env_config()
                assert config["verify_ssl"] is True, f"Expected True for {value!r}"

    def test_ca_bundle_env_var(self):
        """Test DETRIX_CA_BUNDLE is returned from env."""
        with mock.patch.dict(os.environ, {"DETRIX_CA_BUNDLE": "/path/to/ca.pem"}):
            config = get_env_config()
            assert config["ca_bundle"] == "/path/to/ca.pem"

    def test_ca_bundle_none_when_unset(self):
        """Test DETRIX_CA_BUNDLE is None when not set."""
        with mock.patch.dict(os.environ, {}, clear=True):
            for key in list(os.environ.keys()):
                if key.startswith("DETRIX"):
                    del os.environ[key]
            config = get_env_config()
            assert config["ca_bundle"] is None
