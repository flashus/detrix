// Package state provides thread-safe state management for the Detrix client.
package state

import (
	"fmt"
	"os"
	"os/exec"
	"sync"
)

// State represents the client state machine state.
type State string

const (
	// StateSleeping is the initial/inactive state.
	// Control plane running, debugger not started, not registered with daemon.
	StateSleeping State = "sleeping"

	// StateWaking is the transitional state during wake operation.
	// Debugger starting, daemon registration in progress.
	StateWaking State = "waking"

	// StateAwake is the active state.
	// Debugger listening, registered with daemon.
	StateAwake State = "awake"
)

// DelveProcess holds information about a running Delve process.
type DelveProcess struct {
	Cmd  *exec.Cmd
	Host string
	Port int
}

// ClientState holds all client state.
// Access must be synchronized using the embedded RWMutex.
type ClientState struct {
	sync.RWMutex

	// Configuration (set once in Init)
	Name        string
	ControlHost string
	ControlPort int
	DebugPort   int
	DaemonURL   string
	DelvePath   string
	DetrixHome  string
	SafeMode    bool // SafeMode: only logpoints allowed, no breakpoint operations

	// Timeouts (in milliseconds for simplicity)
	HealthCheckTimeoutMs int
	RegisterTimeoutMs    int
	UnregisterTimeoutMs  int
	DelveStartTimeoutMs  int

	// Runtime state
	State             State
	ActualControlPort int
	ActualDebugPort   int
	DebugPortActive   bool
	ConnectionID      *string

	// Process management
	DelveProcess *DelveProcess
}

// Global client state instance
var (
	globalState       *ClientState
	globalStateMu     sync.Mutex
	globalWakeLock    sync.Mutex
	globalInitialized bool
)

// Get returns the global client state, initializing if needed.
func Get() *ClientState {
	globalStateMu.Lock()
	defer globalStateMu.Unlock()

	if globalState == nil {
		globalState = &ClientState{
			State:                StateSleeping,
			HealthCheckTimeoutMs: 2000,
			RegisterTimeoutMs:    5000,
			UnregisterTimeoutMs:  2000,
			DelveStartTimeoutMs:  10000,
		}
	}
	return globalState
}

// Reset resets the global state to initial values.
func Reset() {
	globalStateMu.Lock()
	defer globalStateMu.Unlock()

	globalState = nil
	globalInitialized = false
}

// IsInitialized returns whether the client has been initialized.
func IsInitialized() bool {
	globalStateMu.Lock()
	defer globalStateMu.Unlock()
	return globalInitialized
}

// SetInitialized marks the client as initialized.
func SetInitialized(v bool) {
	globalStateMu.Lock()
	defer globalStateMu.Unlock()
	globalInitialized = v
}

// AcquireWakeLock acquires the wake lock to prevent concurrent wake operations.
func AcquireWakeLock() {
	globalWakeLock.Lock()
}

// ReleaseWakeLock releases the wake lock.
func ReleaseWakeLock() {
	globalWakeLock.Unlock()
}

// TryAcquireWakeLock tries to acquire the wake lock without blocking.
// Returns true if acquired, false if already held.
func TryAcquireWakeLock() bool {
	return globalWakeLock.TryLock()
}

// StatusSnapshot returns a snapshot of the current status.
// This is safe to call from any goroutine.
type StatusSnapshot struct {
	State           State
	Name            string
	ControlHost     string
	ControlPort     int
	DebugPort       int
	DebugPortActive bool
	DaemonURL       string
	ConnectionID    *string
}

// Snapshot returns a thread-safe snapshot of the current state.
func (s *ClientState) Snapshot() StatusSnapshot {
	s.RLock()
	defer s.RUnlock()

	var connID *string
	if s.ConnectionID != nil {
		v := *s.ConnectionID
		connID = &v
	}

	return StatusSnapshot{
		State:           s.State,
		Name:            s.Name,
		ControlHost:     s.ControlHost,
		ControlPort:     s.ActualControlPort,
		DebugPort:       s.ActualDebugPort,
		DebugPortActive: s.DebugPortActive,
		DaemonURL:       s.DaemonURL,
		ConnectionID:    connID,
	}
}

// GenerateConnectionName generates a connection name.
// If name is empty, generates "detrix-client-{pid}".
func GenerateConnectionName(name string) string {
	if name != "" {
		return name
	}
	return fmt.Sprintf("detrix-client-%d", os.Getpid())
}
