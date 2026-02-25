package detrix

import (
	"os"
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

func TestDetectBuildInfo(t *testing.T) {
	// Save original env vars to restore after test
	origEnv := make(map[string]string)
	envVars := []string{
		"DETRIX_BUILD_COMMIT", "DETRIX_BUILD_TAG",
		"GIT_COMMIT", "CI_COMMIT_SHA", "GITHUB_SHA",
		"GIT_TAG", "CI_COMMIT_TAG", "GITHUB_REF_NAME", "GITHUB_REF_TYPE",
	}
	for _, key := range envVars {
		origEnv[key] = os.Getenv(key)
	}
	defer func() {
		// Restore original env
		for key, val := range origEnv {
			if val == "" {
				_ = os.Unsetenv(key)
			} else {
				_ = os.Setenv(key, val)
			}
		}
	}()

	tests := []struct {
		name         string
		envVars      map[string]string
		config       *Config
		expectCommit string
		expectTag    string
	}{
		{
			name: "explicit config override",
			config: &Config{
				BuildCommit: "explicit-commit",
				BuildTag:    "explicit-tag",
			},
			expectCommit: "explicit-commit",
			expectTag:    "explicit-tag",
		},
		{
			name: "DETRIX env vars",
			envVars: map[string]string{
				"DETRIX_BUILD_COMMIT": "detrix-commit",
				"DETRIX_BUILD_TAG":    "detrix-tag",
			},
			expectCommit: "detrix-commit",
			expectTag:    "detrix-tag",
		},
		{
			name: "CI env vars - GitHub",
			envVars: map[string]string{
				"GITHUB_SHA":      "github-commit",
				"GITHUB_REF_TYPE": "tag",
				"GITHUB_REF_NAME": "v1.0.0",
			},
			expectCommit: "github-commit",
			expectTag:    "v1.0.0",
		},
		{
			name: "CI env vars - GitLab",
			envVars: map[string]string{
				"CI_COMMIT_SHA": "gitlab-commit",
				"CI_COMMIT_TAG": "v2.0.0",
			},
			expectCommit: "gitlab-commit",
			expectTag:    "v2.0.0",
		},
		{
			name: "priority: DETRIX over CI",
			envVars: map[string]string{
				"DETRIX_BUILD_COMMIT": "detrix-commit",
				"GITHUB_SHA":          "github-commit",
			},
			expectCommit: "detrix-commit",
		},
		{
			name:         "no env vars - may detect from git or ldflags",
			expectCommit: "", // empty or git-detected
			expectTag:    "", // empty or Version
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Clear all env vars
			for _, key := range envVars {
				_ = os.Unsetenv(key)
			}

			// Set test env vars
			for k, v := range tt.envVars {
				_ = os.Setenv(k, v)
			}

			commit, tag := detectBuildInfo(tt.config)

			// For the "no env vars" case, we allow git detection or ldflags values
			if tt.name == "no env vars - may detect from git or ldflags" {
				// Just verify function doesn't panic
				return
			}

			if commit != tt.expectCommit {
				t.Errorf("commit = %q, want %q", commit, tt.expectCommit)
			}
			if tt.expectTag != "" && tag != tt.expectTag {
				t.Errorf("tag = %q, want %q", tag, tt.expectTag)
			}
		})
	}
}

func TestTryGitRevParse(t *testing.T) {
	// Just verify it doesn't panic
	result := tryGitRevParse()
	// In a git repo, should return a commit SHA or empty string
	if result != "" && len(result) != 40 {
		t.Logf("Git rev-parse returned unexpected length: %d (value: %s)", len(result), result)
	}
}
