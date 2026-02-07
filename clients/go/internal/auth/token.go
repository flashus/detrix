// Package auth provides authentication utilities for the Detrix client.
package auth

import (
	"crypto/subtle"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

// checkTokenFilePermissions verifies token file has secure permissions (0600 or 0400).
// On Unix systems, logs a warning if group or other has any permissions.
// On Windows, the check is skipped (documented limitation).
func checkTokenFilePermissions(path string) {
	// Windows doesn't have Unix-style permissions
	if runtime.GOOS == "windows" {
		return // Skip check on Windows (documented limitation)
	}

	info, err := os.Stat(path)
	if err != nil {
		return
	}
	mode := info.Mode().Perm()
	if mode&0077 != 0 {
		slog.Warn("token file has insecure permissions",
			"path", path,
			"mode", fmt.Sprintf("%04o", mode),
		)
	}
}

// DiscoverToken discovers the authentication token from environment or file.
// Priority:
// 1. DETRIX_TOKEN environment variable
// 2. ~/detrix/mcp-token file
// 3. {detrixHome}/mcp-token file (if detrixHome provided)
func DiscoverToken(detrixHome string) string {
	// Check environment variable first
	if token := os.Getenv("DETRIX_TOKEN"); token != "" {
		return token
	}

	// Try ~/detrix/mcp-token
	if home, err := os.UserHomeDir(); err == nil {
		tokenPath := filepath.Join(home, "detrix", "mcp-token")
		checkTokenFilePermissions(tokenPath)
		if data, err := os.ReadFile(tokenPath); err == nil {
			return strings.TrimSpace(string(data))
		}
	}

	// Try custom detrix home
	if detrixHome != "" {
		tokenPath := filepath.Join(detrixHome, "mcp-token")
		checkTokenFilePermissions(tokenPath)
		if data, err := os.ReadFile(tokenPath); err == nil {
			return strings.TrimSpace(string(data))
		}
	}

	return ""
}

// IsLocalhost checks if the given address is localhost.
func IsLocalhost(addr string) bool {
	// Handle host:port format
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		// No port, use as-is
		host = addr
	}

	// Check for localhost names
	switch strings.ToLower(host) {
	case "localhost", "127.0.0.1", "::1", "[::1]":
		return true
	}

	// Check if IP resolves to loopback
	ip := net.ParseIP(host)
	if ip != nil {
		return ip.IsLoopback()
	}

	return false
}

// IsAuthorized checks if a request is authorized.
// Localhost requests are always authorized.
// Remote requests require a valid Bearer token.
func IsAuthorized(r *http.Request, validToken string) bool {
	// Get client address
	clientAddr := r.RemoteAddr

	// Localhost bypass
	if IsLocalhost(clientAddr) {
		return true
	}

	// No token configured = deny remote requests
	if validToken == "" {
		return false
	}

	// Check Authorization header
	authHeader := r.Header.Get("Authorization")
	if authHeader == "" {
		return false
	}

	// Parse Bearer token
	const prefix = "Bearer "
	if !strings.HasPrefix(authHeader, prefix) {
		return false
	}

	token := strings.TrimPrefix(authHeader, prefix)
	// Use constant-time comparison to prevent timing attacks
	return subtle.ConstantTimeCompare([]byte(token), []byte(validToken)) == 1
}
