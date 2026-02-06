"""Tests for authentication handling."""

import os
from unittest import mock

from detrix.auth import discover_auth_token, get_auth_headers, is_localhost


class TestDiscoverAuthToken:
    """Test discover_auth_token function."""

    def test_env_var_takes_precedence(self, tmp_path):
        """Test that DETRIX_TOKEN env var takes precedence."""
        # Create a token file with different content
        token_file = tmp_path / "mcp-token"
        token_file.write_text("file-token")

        with mock.patch.dict(os.environ, {"DETRIX_TOKEN": "env-token"}):
            result = discover_auth_token(tmp_path)
            assert result == "env-token"

    def test_reads_from_file(self, tmp_path):
        """Test reads token from file when env not set."""
        token_file = tmp_path / "mcp-token"
        token_file.write_text("file-token\n")
        token_file.chmod(0o600)

        with mock.patch.dict(os.environ, {}, clear=True):
            os.environ.pop("DETRIX_TOKEN", None)
            result = discover_auth_token(tmp_path)
            assert result == "file-token"

    def test_returns_none_when_not_found(self, tmp_path):
        """Test returns None when token not found."""
        with mock.patch.dict(os.environ, {}, clear=True):
            os.environ.pop("DETRIX_TOKEN", None)
            result = discover_auth_token(tmp_path)
            assert result is None

    def test_strips_whitespace(self):
        """Test strips whitespace from token."""
        with mock.patch.dict(os.environ, {"DETRIX_TOKEN": "  token-with-spaces  \n"}):
            result = discover_auth_token()
            assert result == "token-with-spaces"


class TestGetAuthHeaders:
    """Test get_auth_headers function."""

    def test_with_token(self):
        """Test returns authorization header with token."""
        headers = get_auth_headers("my-token")
        assert headers == {"Authorization": "Bearer my-token"}

    def test_without_token(self):
        """Test returns empty dict without token."""
        headers = get_auth_headers(None)
        assert headers == {}

    def test_with_empty_token(self):
        """Test returns empty dict with empty token."""
        headers = get_auth_headers("")
        assert headers == {}


class TestIsLocalhost:
    """Test is_localhost function."""

    def test_localhost_string(self):
        """Test 'localhost' is recognized."""
        assert is_localhost("localhost") is True
        assert is_localhost("LOCALHOST") is True

    def test_ipv4_loopback(self):
        """Test IPv4 loopback is recognized."""
        assert is_localhost("127.0.0.1") is True

    def test_ipv6_loopback(self):
        """Test IPv6 loopback is recognized."""
        assert is_localhost("::1") is True

    def test_any_address(self):
        """Test 0.0.0.0 is NOT recognized as localhost (security fix).

        0.0.0.0 is a bind address, not a valid client source address.
        Treating it as localhost could allow auth bypass.
        """
        assert is_localhost("0.0.0.0") is False

    def test_non_localhost(self):
        """Test non-localhost addresses."""
        assert is_localhost("192.168.1.1") is False
        assert is_localhost("example.com") is False
        assert is_localhost("10.0.0.1") is False
