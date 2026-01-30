// Package delve provides Delve process lifecycle management.
package delve

import (
	"bytes"
	"fmt"
	"net"
	"os"
	"os/exec"
	"strconv"
	"syscall"
	"time"
)

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
func (m *Manager) SpawnAndAttach(host string, port int) (*Process, error) {
	// Find delve binary
	delvePath, err := FindDelve(m.DelvePath)
	if err != nil {
		return nil, err
	}

	pid := os.Getpid()

	// Resolve port 0 to an available port
	actualPort := port
	if actualPort == 0 {
		listener, err := net.Listen("tcp", fmt.Sprintf("%s:0", host))
		if err != nil {
			return nil, fmt.Errorf("failed to find available port: %w", err)
		}
		actualPort = listener.Addr().(*net.TCPAddr).Port
		listener.Close()
	}

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
		fmt.Sprintf("--listen=%s:%d", host, actualPort),
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
		Port:   actualPort,
		stdout: &stdout,
		stderr: &stderr,
	}

	// Wait for DAP port to accept connections
	if err := m.waitForReady(host, actualPort, cmd.Process); err != nil {
		// Kill process if startup failed
		m.Kill(proc)
		return nil, fmt.Errorf("delve failed to start: %w\nLogs:\n%s", err, proc.Logs())
	}

	return proc, nil
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
			conn.Close()
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
		proc.Cmd.Process.Kill()
		// Wait for it to actually die
		<-done
		return nil
	}
}

// IsReady checks if Delve's DAP port is accepting connections.
func IsReady(host string, port int) bool {
	conn, err := net.DialTimeout("tcp", net.JoinHostPort(host, strconv.Itoa(port)), 100*time.Millisecond)
	if err != nil {
		return false
	}
	conn.Close()
	return true
}
