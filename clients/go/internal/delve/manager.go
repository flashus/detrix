// Package delve provides Delve process lifecycle management.
package delve

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// maxPortRetries is the number of times to retry port allocation
// when the ephemeral port is taken between allocation and Delve startup
const maxPortRetries = 3

// Process represents a running Delve process.
type Process struct {
	Cmd    *exec.Cmd
	Host   string
	Port   int
	stdout *bytes.Buffer
	stderr *bytes.Buffer
}

// Logs returns the combined stdout and stderr output from Delve.
func (p *Process) Logs() string {
	if p == nil {
		return ""
	}
	var result string
	if p.stdout != nil {
		result += "=== STDOUT ===\n" + p.stdout.String()
	}
	if p.stderr != nil {
		result += "\n=== STDERR ===\n" + p.stderr.String()
	}
	return result
}

// Manager handles Delve process lifecycle.
type Manager struct {
	DelvePath string
	Timeout   time.Duration
}

// NewManager creates a new Delve manager.
func NewManager(delvePath string, timeout time.Duration) *Manager {
	if delvePath == "" {
		delvePath = "dlv" // Use PATH lookup
	}
	if timeout == 0 {
		timeout = 10 * time.Second
	}
	return &Manager{
		DelvePath: delvePath,
		Timeout:   timeout,
	}
}

// FindDelve locates the Delve binary.
// Returns the path if found, or an error if not found.
func FindDelve(delvePath string) (string, error) {
	if delvePath != "" {
		// Check if explicit path exists
		if _, err := os.Stat(delvePath); err == nil {
			return delvePath, nil
		}
		return "", fmt.Errorf("delve not found at %s", delvePath)
	}

	// Search in PATH
	path, err := exec.LookPath("dlv")
	if err != nil {
		return "", fmt.Errorf("delve (dlv) not found in PATH: %w", err)
	}
	return path, nil
}

// SpawnAndAttach spawns Delve to attach to the current process.
// It waits for the DAP server to become ready before returning.
//
// When port is 0, an ephemeral port is allocated. Due to a TOCTOU race
// (the port may be taken between allocation and Delve startup), this
// operation is retried up to maxPortRetries times.
func (m *Manager) SpawnAndAttach(host string, port int) (*Process, error) {
	// Find delve binary
	delvePath, err := FindDelve(m.DelvePath)
	if err != nil {
		return nil, err
	}

	// If port is specified (non-zero), no retries needed
	if port != 0 {
		return m.spawnDelve(delvePath, host, port)
	}

	// Port 0: ephemeral port allocation with retry logic
	// This handles the TOCTOU race where the port may be taken between
	// net.Listen close and Delve startup
	var lastErr error
	for attempt := 0; attempt < maxPortRetries; attempt++ {
		// Allocate an ephemeral port
		listener, err := net.Listen("tcp", fmt.Sprintf("%s:0", host))
		if err != nil {
			return nil, fmt.Errorf("failed to find available port: %w", err)
		}
		actualPort := listener.Addr().(*net.TCPAddr).Port
		if err := listener.Close(); err != nil {
			slog.Debug("failed to close port listener", "error", err)
		} // Release port for Delve to bind

		proc, err := m.spawnDelve(delvePath, host, actualPort)
		if err == nil {
			return proc, nil
		}

		// Check if it's a port bind error (TOCTOU race)
		if isPortBindError(err) {
			lastErr = err
			continue // Retry with a new port
		}

		// Non-port error, fail immediately
		return nil, err
	}

	return nil, fmt.Errorf("failed after %d port allocation retries: %w", maxPortRetries, lastErr)
}

// spawnDelve spawns a Delve process on the specified port.
func (m *Manager) spawnDelve(delvePath string, host string, port int) (*Process, error) {
	pid := os.Getpid()

	// Build command:
	// dlv attach <pid> --headless --accept-multiclient --api-version=2 --continue --listen=<host>:<port>
	// The --continue flag is critical: without it, Delve pauses the process on attach,
	// which would hang the wake request (the process can't respond while paused).
	args := []string{
		"attach",
		strconv.Itoa(pid),
		"--headless",
		"--accept-multiclient",
		"--api-version=2",
		"--continue",
		fmt.Sprintf("--listen=%s:%d", host, port),
	}

	cmd := exec.Command(delvePath, args...)

	// Capture output for debugging
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	// Set process group so we can kill the whole group later
	cmd.SysProcAttr = &syscall.SysProcAttr{
		Setpgid: true,
	}

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("failed to start delve: %w", err)
	}

	proc := &Process{
		Cmd:    cmd,
		Host:   host,
		Port:   port,
		stdout: &stdout,
		stderr: &stderr,
	}

	// Wait for DAP port to accept connections
	if err := m.waitForReady(host, port, cmd.Process); err != nil {
		// Kill process if startup failed
		if killErr := m.Kill(proc); killErr != nil {
			slog.Warn("failed to kill delve process during cleanup", "error", killErr)
		}
		return nil, fmt.Errorf("delve failed to start: %w\nLogs:\n%s", err, proc.Logs())
	}

	return proc, nil
}

// isPortBindError checks if the error indicates a port bind failure
// (address already in use), which suggests a TOCTOU race condition.
func isPortBindError(err error) bool {
	if err == nil {
		return false
	}
	errStr := err.Error()
	// Common indicators of port bind failure
	return strings.Contains(errStr, "address already in use") ||
		strings.Contains(errStr, "bind: address already in use") ||
		errors.Is(err, syscall.EADDRINUSE)
}

// waitForReady polls the DAP port until it accepts connections.
func (m *Manager) waitForReady(host string, port int, proc *os.Process) error {
	deadline := time.Now().Add(m.Timeout)
	addr := net.JoinHostPort(host, strconv.Itoa(port))
	checkInterval := 100 * time.Millisecond

	for time.Now().Before(deadline) {
		// Check if process died
		if !processAlive(proc) {
			return fmt.Errorf("delve process terminated unexpectedly")
		}

		// Try to connect
		conn, err := net.DialTimeout("tcp", addr, checkInterval)
		if err == nil {
			if err := conn.Close(); err != nil {
				slog.Debug("failed to close probe connection", "error", err)
			}
			return nil
		}

		time.Sleep(checkInterval)
	}

	return fmt.Errorf("delve did not become ready within %v", m.Timeout)
}

// processAlive checks if a process is still running.
func processAlive(proc *os.Process) bool {
	if proc == nil {
		return false
	}
	// Signal 0 checks if process exists without sending a signal
	err := proc.Signal(syscall.Signal(0))
	return err == nil
}

// Kill terminates the Delve process gracefully.
func (m *Manager) Kill(proc *Process) error {
	if proc == nil || proc.Cmd == nil || proc.Cmd.Process == nil {
		return nil
	}

	// Try graceful shutdown first (SIGTERM)
	if err := proc.Cmd.Process.Signal(syscall.SIGTERM); err != nil {
		// Process might already be dead
		return nil
	}

	// Wait with timeout
	done := make(chan error, 1)
	go func() {
		_, err := proc.Cmd.Process.Wait()
		done <- err
	}()

	select {
	case <-done:
		return nil
	case <-time.After(2 * time.Second):
		// Force kill
		if err := proc.Cmd.Process.Kill(); err != nil {
			slog.Warn("delve force kill failed", "error", err)
		}
		// Wait for it to actually die
		<-done
		return nil
	}
}

