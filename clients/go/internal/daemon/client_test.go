package daemon

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestHealthCheck_Success(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/health" {
			t.Errorf("expected /health, got %s", r.URL.Path)
		}
		if r.Method != http.MethodGet {
			t.Errorf("expected GET, got %s", r.Method)
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	if err := client.HealthCheck(server.URL, 5*time.Second); err != nil {
		t.Errorf("expected health check to succeed, got error: %v", err)
	}
}

func TestHealthCheck_Failure(t *testing.T) {
	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	err = client.HealthCheck("http://127.0.0.1:1", 1*time.Second)
	if err == nil {
		t.Error("expected health check to fail for unreachable server")
	}
}

func TestHealthCheck_NonOKStatus(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	err = client.HealthCheck(server.URL, 5*time.Second)
	if err == nil {
		t.Error("expected health check to fail for 503 status")
	}
}

func TestRegister_Success(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/api/v1/connections" {
			t.Errorf("expected /api/v1/connections, got %s", r.URL.Path)
		}
		if r.Method != http.MethodPost {
			t.Errorf("expected POST, got %s", r.Method)
		}
		if r.Header.Get("Content-Type") != "application/json" {
			t.Errorf("expected application/json content type, got %s", r.Header.Get("Content-Type"))
		}

		var req RegisterRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			t.Fatalf("failed to decode request: %v", err)
		}

		if req.Host != "127.0.0.1" || req.Port != 5678 || req.Language != "go" {
			t.Errorf("unexpected request: %+v", req)
		}

		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		resp := RegisterResponse{
			ConnectionID: "test-conn-id",
			Status:       "connected",
		}
		if err := json.NewEncoder(w).Encode(resp); err != nil {
			t.Fatalf("failed to encode response: %v", err)
		}
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	connID, err := client.Register(server.URL, RegisterRequest{
		Host:     "127.0.0.1",
		Port:     5678,
		Language: "go",
		Name:     "test-client",
	}, 5*time.Second)
	if err != nil {
		t.Fatalf("Register failed: %v", err)
	}
	if connID != "test-conn-id" {
		t.Errorf("expected test-conn-id, got %s", connID)
	}
}

func TestRegister_ServerError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte("internal error"))
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	_, err = client.Register(server.URL, RegisterRequest{
		Host:     "127.0.0.1",
		Port:     5678,
		Language: "go",
		Name:     "test-client",
	}, 5*time.Second)
	if err == nil {
		t.Error("expected Register to fail for 500 status")
	}
}

func TestUnregister_Success(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodDelete {
			t.Errorf("expected DELETE, got %s", r.Method)
		}
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	err = client.Unregister(server.URL, "test-conn-id", 5*time.Second)
	if err != nil {
		t.Errorf("Unregister failed: %v", err)
	}
}

func TestUnregister_NotFound(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNotFound)
	}))
	defer server.Close()

	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}

	// 404 should be accepted (already deleted)
	err = client.Unregister(server.URL, "nonexistent", 5*time.Second)
	if err != nil {
		t.Errorf("Unregister should accept 404, got error: %v", err)
	}
}

func TestNewClient_DefaultOptions(t *testing.T) {
	client, err := NewClient(nil)
	if err != nil {
		t.Fatalf("NewClient with nil opts failed: %v", err)
	}
	if client == nil {
		t.Error("expected non-nil client")
	}
}

func TestNewClient_InvalidCABundle(t *testing.T) {
	opts := &ClientOptions{
		VerifyTLS: true,
		CABundle:  "/nonexistent/ca-bundle.pem",
	}
	_, err := NewClient(opts)
	if err == nil {
		t.Error("expected error for invalid CA bundle path")
	}
}

func TestNewClient_CustomTimeout(t *testing.T) {
	opts := &ClientOptions{
		VerifyTLS: true,
		Timeout:   10 * time.Second,
	}
	client, err := NewClient(opts)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}
	if client == nil {
		t.Error("expected non-nil client")
	}
}

func TestRegisterRequestIdentityFields(t *testing.T) {
	req := RegisterRequest{
		Host:          "127.0.0.1",
		Port:          5678,
		Language:      "go",
		Name:          "test-client",
		WorkspaceRoot: "/workspace",
		Hostname:      "test-host",
	}

	data, err := json.Marshal(req)
	if err != nil {
		t.Fatalf("failed to marshal RegisterRequest: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("failed to unmarshal JSON: %v", err)
	}

	// Must use "name" field (UUID identity system)
	if _, ok := raw["name"]; !ok {
		t.Errorf("JSON must contain \"name\" key, got: %s", string(data))
	}
	if raw["name"] != "test-client" {
		t.Errorf("expected name=test-client, got %v", raw["name"])
	}

	// Must include identity fields for UUID generation
	if _, ok := raw["workspaceRoot"]; !ok {
		t.Errorf("JSON must contain \"workspaceRoot\" key, got: %s", string(data))
	}
	if _, ok := raw["hostname"]; !ok {
		t.Errorf("JSON must contain \"hostname\" key, got: %s", string(data))
	}
}

func TestNewClient_InsecureSkipVerify(t *testing.T) {
	opts := &ClientOptions{
		VerifyTLS: false,
	}
	client, err := NewClient(opts)
	if err != nil {
		t.Fatalf("NewClient failed: %v", err)
	}
	if client == nil {
		t.Error("expected non-nil client")
	}
}
