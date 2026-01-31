// Package detrix provides the Detrix Go client for debug-on-demand observability.
//
// This client enables Go applications to be observed by the Detrix daemon
// without any code modifications or performance overhead when inactive.
//
// Basic usage:
//
//	import "github.com/anthropics/detrix-go"
//
//	func main() {
//	    // Initialize client (starts control plane, stays SLEEPING)
//	    err := detrix.Init(detrix.Config{
//	        Name:      "my-service",
//	        DaemonURL: "http://127.0.0.1:8090",
//	    })
//	    if err != nil {
//	        log.Fatal(err)
//	    }
//	    defer detrix.Shutdown()
//
//	    // ... your application code ...
//
//	    // When you need observability:
//	    resp, err := detrix.Wake()
//	    if err != nil {
//	        log.Printf("Wake failed: %v", err)
//	    }
//
//	    // When done:
//	    detrix.Sleep()
//	}
//
// Unlike Python's debugpy, Delve can be fully stopped on Sleep(), providing
// cleaner resource management.
package detrix

import (
	"errors"
	"fmt"
	"log"
	"os"
	"strconv"
	"time"

	"github.com/flashus/detrix/clients/go/internal/auth"
	"github.com/flashus/detrix/clients/go/internal/control"
	"github.com/flashus/detrix/clients/go/internal/daemon"
	"github.com/flashus/detrix/clients/go/internal/delve"
	"github.com/flashus/detrix/clients/go/internal/state"
)

// Config holds client configuration.
type Config struct {
	// Name is the connection name (default: "detrix-client-{pid}")
	Name string

	// ControlHost is the host for the control plane server (default: "127.0.0.1")
	ControlHost string

	// ControlPort is the port for the control plane (0 = auto-assign)
	ControlPort int

	// DebugPort is the port for the debug adapter (0 = auto-assign)
	DebugPort int

	// DaemonURL is the URL of the Detrix daemon (default: "http://127.0.0.1:8090")
	DaemonURL string

	// DelvePath is the path to the dlv binary (default: searches PATH)
	DelvePath string

	// DetrixHome is the path to the Detrix home directory (default: ~/detrix)
	DetrixHome string

	// HealthCheckTimeout is the timeout for daemon health checks (default: 2s)
	HealthCheckTimeout time.Duration

	// RegisterTimeout is the timeout for connection registration (default: 5s)
	RegisterTimeout time.Duration

	// UnregisterTimeout is the timeout for connection unregistration (default: 2s)
	UnregisterTimeout time.Duration

	// DelveStartTimeout is the timeout for Delve to start (default: 10s)
	DelveStartTimeout time.Duration

	// SafeMode enables production-safe mode: only logpoints (non-blocking) are allowed.
	// Disables operations that require breakpoints: function calls, stack traces, memory snapshots.
	// Recommended for production environments where execution pauses are unacceptable.
	SafeMode bool
}

// StatusResponse contains the current client status.
type StatusResponse struct {
	State           string  `json:"state"`
	Name            string  `json:"name"`
	ControlHost     string  `json:"control_host"`
	ControlPort     int     `json:"control_port"`
	DebugPort       int     `json:"debug_port"`
	DebugPortActive bool    `json:"debug_port_active"`
	DaemonURL       string  `json:"daemon_url"`
	ConnectionID    *string `json:"connection_id"`
}

// WakeResponse is the response from a wake operation.
type WakeResponse struct {
	Status       string `json:"status"`
	DebugPort    int    `json:"debug_port"`
	ConnectionID string `json:"connection_id"`
}

// SleepResponse is the response from a sleep operation.
type SleepResponse struct {
	Status string `json:"status"`
}

// Package-level components
var (
	controlServer *control.Server
	daemonClient  *daemon.Client
	delveManager  *delve.Manager
)

// Common errors
var (
	ErrNotInitialized   = errors.New("detrix client not initialized")
	ErrAlreadyInitialized = errors.New("detrix client already initialized")
	ErrWakeInProgress   = errors.New("wake operation already in progress")
)

// Init initializes the Detrix client.
//
// This starts the control plane HTTP server but does NOT contact the daemon.
// The client starts in SLEEPING state.
//
// Configuration can also be provided via environment variables:
//   - DETRIX_CLIENT_NAME
//   - DETRIX_DAEMON_URL
//   - DETRIX_CONTROL_HOST
//   - DETRIX_CONTROL_PORT
//   - DETRIX_DEBUG_PORT
//   - DETRIX_DELVE_PATH
//
// Function parameters take precedence over environment variables.
func Init(cfg Config) error {
	if state.IsInitialized() {
		return ErrAlreadyInitialized
	}

	// Apply defaults and environment overrides
	cfg = resolveConfig(cfg)

	// Validate Delve is available
	delvePath, err := delve.FindDelve(cfg.DelvePath)
	if err != nil {
		return fmt.Errorf("delve not found: %w", err)
	}

	// Initialize state
	s := state.Get()
	s.Lock()
	s.Name = state.GenerateConnectionName(cfg.Name)
	s.ControlHost = cfg.ControlHost
	s.ControlPort = cfg.ControlPort
	s.DebugPort = cfg.DebugPort
	s.DaemonURL = cfg.DaemonURL
	s.DelvePath = delvePath
	s.DetrixHome = cfg.DetrixHome
	s.SafeMode = cfg.SafeMode
	s.HealthCheckTimeoutMs = int(cfg.HealthCheckTimeout.Milliseconds())
	s.RegisterTimeoutMs = int(cfg.RegisterTimeout.Milliseconds())
	s.UnregisterTimeoutMs = int(cfg.UnregisterTimeout.Milliseconds())
	s.DelveStartTimeoutMs = int(cfg.DelveStartTimeout.Milliseconds())
	s.State = state.StateSleeping
	s.Unlock()

	// Initialize components
	daemonClient = daemon.NewClient()
	delveManager = delve.NewManager(delvePath, cfg.DelveStartTimeout)

	// Discover auth token
	authToken := auth.DiscoverToken(cfg.DetrixHome)

	// Create and start control server
	controlServer = control.NewServer(
		cfg.ControlHost,
		cfg.ControlPort,
		authToken,
		statusProvider,
		wakeHandler,
		sleepHandler,
	)

	actualPort, err := controlServer.Start(cfg.ControlHost, cfg.ControlPort)
	if err != nil {
		return fmt.Errorf("failed to start control plane: %w", err)
	}

	s.Lock()
	s.ActualControlPort = actualPort
	s.Unlock()

	state.SetInitialized(true)
	return nil
}

// Status returns the current client status.
func Status() StatusResponse {
	s := state.Get()
	snap := s.Snapshot()

	return StatusResponse{
		State:           string(snap.State),
		Name:            snap.Name,
		ControlHost:     snap.ControlHost,
		ControlPort:     snap.ControlPort,
		DebugPort:       snap.DebugPort,
		DebugPortActive: snap.DebugPortActive,
		DaemonURL:       snap.DaemonURL,
		ConnectionID:    snap.ConnectionID,
	}
}

// Wake starts the debugger and registers with the daemon.
//
// This spawns a Delve process to attach to the current process,
// then registers the connection with the Detrix daemon.
func Wake() (WakeResponse, error) {
	return WakeWithURL("")
}

// WakeWithURL starts the debugger with a daemon URL override.
func WakeWithURL(daemonURL string) (WakeResponse, error) {
	if !state.IsInitialized() {
		return WakeResponse{}, ErrNotInitialized
	}

	// Acquire wake lock
	state.AcquireWakeLock()
	defer state.ReleaseWakeLock()

	s := state.Get()

	// Phase 1: Check current state (short lock)
	s.Lock()
	if s.State == state.StateAwake {
		resp := WakeResponse{
			Status:       "already_awake",
			DebugPort:    s.ActualDebugPort,
			ConnectionID: "",
		}
		if s.ConnectionID != nil {
			resp.ConnectionID = *s.ConnectionID
		}
		s.Unlock()
		return resp, nil
	}
	if s.State == state.StateWaking {
		s.Unlock()
		return WakeResponse{}, ErrWakeInProgress
	}

	// Transition to WAKING
	s.State = state.StateWaking
	targetDaemonURL := daemonURL
	if targetDaemonURL == "" {
		targetDaemonURL = s.DaemonURL
	}
	debugHost := s.ControlHost
	debugPort := s.DebugPort
	name := s.Name
	detrixHome := s.DetrixHome
	safeMode := s.SafeMode
	healthTimeout := time.Duration(s.HealthCheckTimeoutMs) * time.Millisecond
	registerTimeout := time.Duration(s.RegisterTimeoutMs) * time.Millisecond
	s.Unlock()

	// Phase 2: Network operations (no lock held)

	// 2a. Check daemon health
	if err := daemonClient.HealthCheck(targetDaemonURL, healthTimeout); err != nil {
		s.Lock()
		s.State = state.StateSleeping
		s.Unlock()
		return WakeResponse{}, fmt.Errorf("daemon not reachable: %w", err)
	}

	// 2b. Spawn Delve and attach to self
	delveProc, err := delveManager.SpawnAndAttach(debugHost, debugPort)
	if err != nil {
		s.Lock()
		s.State = state.StateSleeping
		s.Unlock()
		return WakeResponse{}, fmt.Errorf("failed to start delve: %w", err)
	}

	// 2c. Discover auth token
	token := auth.DiscoverToken(detrixHome)

	// 2d. Register with daemon
	connID, err := daemonClient.Register(targetDaemonURL, daemon.RegisterRequest{
		Host:     debugHost,
		Port:     delveProc.Port,
		Language: "go",
		Name:     name,
		Token:    token,
		SafeMode: safeMode,
	}, registerTimeout)
	if err != nil {
		if killErr := delveManager.Kill(delveProc); killErr != nil {
			log.Printf("detrix: failed to kill Delve process during cleanup: %v", killErr)
		}
		s.Lock()
		s.State = state.StateSleeping
		s.Unlock()
		return WakeResponse{}, fmt.Errorf("failed to register with daemon: %w", err)
	}

	// Phase 3: Update state (short lock)
	s.Lock()
	s.State = state.StateAwake
	s.ActualDebugPort = delveProc.Port
	s.DebugPortActive = true
	s.ConnectionID = &connID
	s.DelveProcess = &state.DelveProcess{
		Cmd:  delveProc.Cmd,
		Host: delveProc.Host,
		Port: delveProc.Port,
	}
	s.Unlock()

	return WakeResponse{
		Status:       "awake",
		DebugPort:    delveProc.Port,
		ConnectionID: connID,
	}, nil
}

// Sleep stops the debugger and unregisters from the daemon.
//
// Unlike Python's debugpy, Delve can be fully stopped, providing
// cleaner resource management.
func Sleep() (SleepResponse, error) {
	if !state.IsInitialized() {
		return SleepResponse{}, ErrNotInitialized
	}

	s := state.Get()

	s.Lock()
	if s.State == state.StateSleeping {
		s.Unlock()
		return SleepResponse{Status: "already_sleeping"}, nil
	}

	// If waking, wait for wake to complete first.
	// Use a loop to re-check state after re-acquiring the lock,
	// since another wake operation could have started in the meantime.
	for s.State == state.StateWaking {
		s.Unlock()
		state.AcquireWakeLock()
		state.ReleaseWakeLock()
		s.Lock()
		// Loop re-checks state in case another wake started
	}

	// Re-check for sleeping state after waiting for wake
	if s.State == state.StateSleeping {
		s.Unlock()
		return SleepResponse{Status: "already_sleeping"}, nil
	}

	connID := s.ConnectionID
	daemonURL := s.DaemonURL
	delveProc := s.DelveProcess
	unregisterTimeout := time.Duration(s.UnregisterTimeoutMs) * time.Millisecond
	s.Unlock()

	// Unregister from daemon (best effort)
	if connID != nil {
		_ = daemonClient.Unregister(daemonURL, *connID, unregisterTimeout)
	}

	// Kill Delve process (Go advantage: we CAN stop the debugger!)
	if delveProc != nil {
		if err := delveManager.Kill(&delve.Process{
			Cmd:  delveProc.Cmd,
			Host: delveProc.Host,
			Port: delveProc.Port,
		}); err != nil {
			log.Printf("detrix: failed to kill Delve process: %v", err)
		}
	}

	// Update state
	s.Lock()
	s.State = state.StateSleeping
	s.ConnectionID = nil
	s.DelveProcess = nil
	s.DebugPortActive = false
	s.Unlock()

	return SleepResponse{Status: "sleeping"}, nil
}

// Shutdown stops the client and cleans up resources.
func Shutdown() error {
	if !state.IsInitialized() {
		return nil
	}

	// Sleep first to unregister and stop Delve
	_, _ = Sleep()

	// Stop control server
	if controlServer != nil {
		if err := controlServer.Stop(); err != nil {
			log.Printf("detrix: failed to stop control server: %v", err)
		}
	}

	// Reset state
	state.Reset()

	return nil
}

// resolveConfig applies defaults and environment variable overrides.
func resolveConfig(cfg Config) Config {
	// Defaults
	if cfg.ControlHost == "" {
		cfg.ControlHost = getEnvOrDefault("DETRIX_CONTROL_HOST", "127.0.0.1")
	}
	if cfg.DaemonURL == "" {
		cfg.DaemonURL = getEnvOrDefault("DETRIX_DAEMON_URL", "http://127.0.0.1:8090")
	}
	if cfg.Name == "" {
		cfg.Name = os.Getenv("DETRIX_CLIENT_NAME")
	}
	if cfg.DelvePath == "" {
		cfg.DelvePath = os.Getenv("DETRIX_DELVE_PATH")
	}

	// Port overrides
	if cfg.ControlPort == 0 {
		if v := os.Getenv("DETRIX_CONTROL_PORT"); v != "" {
			if port, err := strconv.Atoi(v); err == nil {
				cfg.ControlPort = port
			}
		}
	}
	if cfg.DebugPort == 0 {
		if v := os.Getenv("DETRIX_DEBUG_PORT"); v != "" {
			if port, err := strconv.Atoi(v); err == nil {
				cfg.DebugPort = port
			}
		}
	}

	// Timeout defaults
	if cfg.HealthCheckTimeout == 0 {
		cfg.HealthCheckTimeout = 2 * time.Second
	}
	if cfg.RegisterTimeout == 0 {
		cfg.RegisterTimeout = 5 * time.Second
	}
	if cfg.UnregisterTimeout == 0 {
		cfg.UnregisterTimeout = 2 * time.Second
	}
	if cfg.DelveStartTimeout == 0 {
		cfg.DelveStartTimeout = 10 * time.Second
	}

	return cfg
}

func getEnvOrDefault(key, defaultValue string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return defaultValue
}

// Handlers for control plane

func statusProvider() map[string]any {
	status := Status()
	return map[string]any{
		"state":             status.State,
		"name":              status.Name,
		"control_host":      status.ControlHost,
		"control_port":      status.ControlPort,
		"debug_port":        status.DebugPort,
		"debug_port_active": status.DebugPortActive,
		"daemon_url":        status.DaemonURL,
		"connection_id":     status.ConnectionID,
	}
}

func wakeHandler(daemonURL string) (map[string]any, error) {
	resp, err := WakeWithURL(daemonURL)
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"status":        resp.Status,
		"debug_port":    resp.DebugPort,
		"connection_id": resp.ConnectionID,
	}, nil
}

func sleepHandler() (map[string]any, error) {
	resp, err := Sleep()
	if err != nil {
		return nil, err
	}
	return map[string]any{
		"status": resp.Status,
	}, nil
}
