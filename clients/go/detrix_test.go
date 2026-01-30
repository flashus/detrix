package detrix

import (
	"testing"
	"time"

	"github.com/flashus/detrix/clients/go/internal/state"
)

func TestResolveConfig(t *testing.T) {
	// Test default values
	cfg := resolveConfig(Config{})

	if cfg.ControlHost != "127.0.0.1" {
		t.Errorf("Expected ControlHost 127.0.0.1, got %s", cfg.ControlHost)
	}
	if cfg.DaemonURL != "http://127.0.0.1:8090" {
		t.Errorf("Expected DaemonURL http://127.0.0.1:8090, got %s", cfg.DaemonURL)
	}
	if cfg.HealthCheckTimeout != 2*time.Second {
		t.Errorf("Expected HealthCheckTimeout 2s, got %v", cfg.HealthCheckTimeout)
	}
	if cfg.RegisterTimeout != 5*time.Second {
		t.Errorf("Expected RegisterTimeout 5s, got %v", cfg.RegisterTimeout)
	}
	if cfg.DelveStartTimeout != 10*time.Second {
		t.Errorf("Expected DelveStartTimeout 10s, got %v", cfg.DelveStartTimeout)
	}
}

func TestResolveConfigPreservesExplicit(t *testing.T) {
	cfg := resolveConfig(Config{
		ControlHost:        "192.168.1.1",
		DaemonURL:          "http://custom:9000",
		HealthCheckTimeout: 5 * time.Second,
	})

	if cfg.ControlHost != "192.168.1.1" {
		t.Errorf("Expected ControlHost 192.168.1.1, got %s", cfg.ControlHost)
	}
	if cfg.DaemonURL != "http://custom:9000" {
		t.Errorf("Expected DaemonURL http://custom:9000, got %s", cfg.DaemonURL)
	}
	if cfg.HealthCheckTimeout != 5*time.Second {
		t.Errorf("Expected HealthCheckTimeout 5s, got %v", cfg.HealthCheckTimeout)
	}
}

func TestGenerateConnectionName(t *testing.T) {
	// Test with explicit name
	name := state.GenerateConnectionName("my-service")
	if name != "my-service" {
		t.Errorf("Expected my-service, got %s", name)
	}

	// Test with empty name (should generate default)
	name = state.GenerateConnectionName("")
	if name == "" {
		t.Error("Expected generated name, got empty")
	}
	// Should contain "detrix-client-"
	if len(name) < 15 {
		t.Errorf("Generated name too short: %s", name)
	}
}

func TestStatusBeforeInit(t *testing.T) {
	// Reset any previous state
	state.Reset()

	// Status should work even before Init
	status := Status()
	if status.State != "sleeping" {
		t.Errorf("Expected sleeping state, got %s", status.State)
	}
}

func TestWakeBeforeInit(t *testing.T) {
	// Reset any previous state
	state.Reset()

	_, err := Wake()
	if err != ErrNotInitialized {
		t.Errorf("Expected ErrNotInitialized, got %v", err)
	}
}

func TestSleepBeforeInit(t *testing.T) {
	// Reset any previous state
	state.Reset()

	_, err := Sleep()
	if err != ErrNotInitialized {
		t.Errorf("Expected ErrNotInitialized, got %v", err)
	}
}

func TestShutdownBeforeInit(t *testing.T) {
	// Reset any previous state
	state.Reset()

	// Shutdown before init should be safe (no-op)
	err := Shutdown()
	if err != nil {
		t.Errorf("Expected nil error, got %v", err)
	}
}
