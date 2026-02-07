package auth

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestIsLocalhost(t *testing.T) {
	tests := []struct {
		addr     string
		expected bool
	}{
		{"127.0.0.1", true},
		{"127.0.0.1:8080", true},
		{"localhost", true},
		{"localhost:8080", true},
		{"::1", true},
		{"[::1]:8080", true},
		{"192.168.1.1", false},
		{"192.168.1.1:8080", false},
		{"example.com", false},
		{"example.com:443", false},
	}

	for _, tt := range tests {
		t.Run(tt.addr, func(t *testing.T) {
			result := IsLocalhost(tt.addr)
			if result != tt.expected {
				t.Errorf("IsLocalhost(%q) = %v, expected %v", tt.addr, result, tt.expected)
			}
		})
	}
}

func TestIsAuthorizedLocalhost(t *testing.T) {
	req := httptest.NewRequest("GET", "/detrix/status", nil)
	req.RemoteAddr = "127.0.0.1:54321"

	// Localhost should be authorized even without token
	if !IsAuthorized(req, "") {
		t.Error("Expected localhost to be authorized")
	}
	if !IsAuthorized(req, "some-token") {
		t.Error("Expected localhost to be authorized with token")
	}
}

func TestIsAuthorizedRemoteNoToken(t *testing.T) {
	req := httptest.NewRequest("GET", "/detrix/status", nil)
	req.RemoteAddr = "192.168.1.100:54321"

	// Remote without configured token should be denied
	if IsAuthorized(req, "") {
		t.Error("Expected remote without token to be denied")
	}
}

func TestIsAuthorizedRemoteWithValidToken(t *testing.T) {
	req := httptest.NewRequest("GET", "/detrix/status", nil)
	req.RemoteAddr = "192.168.1.100:54321"
	req.Header.Set("Authorization", "Bearer secret-token")

	if !IsAuthorized(req, "secret-token") {
		t.Error("Expected remote with valid token to be authorized")
	}
}

func TestIsAuthorizedRemoteWithInvalidToken(t *testing.T) {
	req := httptest.NewRequest("GET", "/detrix/status", nil)
	req.RemoteAddr = "192.168.1.100:54321"
	req.Header.Set("Authorization", "Bearer wrong-token")

	if IsAuthorized(req, "secret-token") {
		t.Error("Expected remote with invalid token to be denied")
	}
}

func TestIsAuthorizedRemoteMissingBearer(t *testing.T) {
	req := httptest.NewRequest("GET", "/detrix/status", nil)
	req.RemoteAddr = "192.168.1.100:54321"
	req.Header.Set("Authorization", "secret-token") // Missing "Bearer " prefix

	if IsAuthorized(req, "secret-token") {
		t.Error("Expected remote without Bearer prefix to be denied")
	}
}

func TestDiscoverTokenFromEnv(t *testing.T) {
	// Set environment variable
	t.Setenv("DETRIX_TOKEN", "env-token")

	token := DiscoverToken("")
	if token != "env-token" {
		t.Errorf("Expected env-token, got %s", token)
	}
}

func TestDiscoverTokenPriority(t *testing.T) {
	// Environment variable should take priority
	t.Setenv("DETRIX_TOKEN", "env-priority-token")

	token := DiscoverToken("/some/path")
	if token != "env-priority-token" {
		t.Errorf("Expected env-priority-token, got %s", token)
	}
}

func TestIntegrationWithHTTPHandler(t *testing.T) {
	validToken := "test-secret"

	handler := func(w http.ResponseWriter, r *http.Request) {
		if !IsAuthorized(r, validToken) {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		w.WriteHeader(http.StatusOK)
	}

	// Test localhost access (should work without token)
	req := httptest.NewRequest("GET", "/", nil)
	req.RemoteAddr = "127.0.0.1:12345"
	rec := httptest.NewRecorder()
	handler(rec, req)
	if rec.Code != http.StatusOK {
		t.Errorf("Expected 200 for localhost, got %d", rec.Code)
	}

	// Test remote without token (should fail)
	req = httptest.NewRequest("GET", "/", nil)
	req.RemoteAddr = "10.0.0.1:12345"
	rec = httptest.NewRecorder()
	handler(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Errorf("Expected 401 for remote without token, got %d", rec.Code)
	}

	// Test remote with valid token (should work)
	req = httptest.NewRequest("GET", "/", nil)
	req.RemoteAddr = "10.0.0.1:12345"
	req.Header.Set("Authorization", "Bearer "+validToken)
	rec = httptest.NewRecorder()
	handler(rec, req)
	if rec.Code != http.StatusOK {
		t.Errorf("Expected 200 for remote with token, got %d", rec.Code)
	}
}
