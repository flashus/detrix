package control

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestUpdateToken(t *testing.T) {
	s := NewServer("127.0.0.1", 0, "initial-token", nil, nil, nil)

	// Verify initial token
	if got := s.getToken(); got != "initial-token" {
		t.Fatalf("expected initial-token, got %s", got)
	}

	// Update token
	s.UpdateToken("new-token")

	if got := s.getToken(); got != "new-token" {
		t.Fatalf("expected new-token, got %s", got)
	}

	// Update to empty (effectively disabling auth for remote)
	s.UpdateToken("")

	if got := s.getToken(); got != "" {
		t.Fatalf("expected empty string, got %s", got)
	}
}

func TestWithAuthUsesCurrentToken(t *testing.T) {
	s := NewServer("127.0.0.1", 0, "old-token", nil, nil, nil)

	handler := s.withAuth(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})

	// Request with old token should succeed (localhost bypass)
	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	req.RemoteAddr = "127.0.0.1:12345"
	w := httptest.NewRecorder()
	handler(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 from localhost, got %d", w.Code)
	}

	// Remote request with old token
	req = httptest.NewRequest(http.MethodGet, "/test", nil)
	req.RemoteAddr = "192.168.1.1:12345"
	req.Header.Set("Authorization", "Bearer old-token")
	w = httptest.NewRecorder()
	handler(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 with correct token, got %d", w.Code)
	}

	// Update token
	s.UpdateToken("new-token")

	// Remote request with old token should now fail
	req = httptest.NewRequest(http.MethodGet, "/test", nil)
	req.RemoteAddr = "192.168.1.1:12345"
	req.Header.Set("Authorization", "Bearer old-token")
	w = httptest.NewRecorder()
	handler(w, req)

	if w.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 with old token after update, got %d", w.Code)
	}

	// Remote request with new token should succeed
	req = httptest.NewRequest(http.MethodGet, "/test", nil)
	req.RemoteAddr = "192.168.1.1:12345"
	req.Header.Set("Authorization", "Bearer new-token")
	w = httptest.NewRecorder()
	handler(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected 200 with new token, got %d", w.Code)
	}
}

func TestWithAuthRejectsUnauthorized(t *testing.T) {
	s := NewServer("127.0.0.1", 0, "secret", nil, nil, nil)

	handler := s.withAuth(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})

	// Remote request without token
	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	req.RemoteAddr = "192.168.1.1:12345"
	w := httptest.NewRecorder()
	handler(w, req)

	if w.Code != http.StatusUnauthorized {
		t.Fatalf("expected 401 without token, got %d", w.Code)
	}

	var body map[string]string
	if err := json.NewDecoder(w.Body).Decode(&body); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}
	if body["error"] != "unauthorized" {
		t.Fatalf("expected 'unauthorized' error, got %q", body["error"])
	}
}
