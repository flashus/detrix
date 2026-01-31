// Package control provides the HTTP control plane server for the Detrix client.
package control

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"runtime"
	"sync"
	"time"

	"github.com/flashus/detrix/clients/go/internal/auth"
)

// StatusProvider is a function that returns the current status.
type StatusProvider func() map[string]any

// WakeHandler is a function that handles wake requests.
type WakeHandler func(daemonURL string) (map[string]any, error)

// SleepHandler is a function that handles sleep requests.
type SleepHandler func() (map[string]any, error)

// Server is the HTTP control plane server.
type Server struct {
	httpServer     *http.Server
	listener       net.Listener
	actualPort     int
	statusProvider StatusProvider
	wakeHandler    WakeHandler
	sleepHandler   SleepHandler
	authToken      string
	mu             sync.Mutex
	running        bool
}

// NewServer creates a new control plane server.
func NewServer(
	host string,
	port int,
	authToken string,
	statusProvider StatusProvider,
	wakeHandler WakeHandler,
	sleepHandler SleepHandler,
) *Server {
	return &Server{
		statusProvider: statusProvider,
		wakeHandler:    wakeHandler,
		sleepHandler:   sleepHandler,
		authToken:      authToken,
	}
}

// Start starts the HTTP server.
// Returns the actual port the server is listening on.
func (s *Server) Start(host string, port int) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.running {
		return s.actualPort, nil
	}

	// Create listener first to get actual port
	addr := fmt.Sprintf("%s:%d", host, port)
	listener, err := net.Listen("tcp", addr)
	if err != nil {
		return 0, fmt.Errorf("failed to listen on %s: %w", addr, err)
	}

	s.listener = listener
	s.actualPort = listener.Addr().(*net.TCPAddr).Port

	// Create router (Go 1.21 compatible - no method patterns)
	mux := http.NewServeMux()

	// Health endpoint (no auth required)
	mux.HandleFunc("/detrix/health", s.handleHealth)

	// Authenticated endpoints
	mux.HandleFunc("/detrix/status", s.withAuth(s.handleStatus))
	mux.HandleFunc("/detrix/info", s.withAuth(s.handleInfo))
	mux.HandleFunc("/detrix/wake", s.withAuth(s.handleWake))
	mux.HandleFunc("/detrix/sleep", s.withAuth(s.handleSleep))

	s.httpServer = &http.Server{
		Handler: mux,
	}

	// Start server in background
	go func() {
		if err := s.httpServer.Serve(listener); err != nil && err != http.ErrServerClosed {
			// Log error but don't crash
			fmt.Fprintf(os.Stderr, "Control plane server error: %v\n", err)
		}
	}()

	s.running = true
	return s.actualPort, nil
}

// Stop stops the HTTP server gracefully.
func (s *Server) Stop() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if !s.running || s.httpServer == nil {
		return nil
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	err := s.httpServer.Shutdown(ctx)
	s.running = false
	return err
}

// ActualPort returns the actual port the server is listening on.
func (s *Server) ActualPort() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.actualPort
}

// withAuth wraps a handler with authentication.
func (s *Server) withAuth(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !auth.IsAuthorized(r, s.authToken) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusUnauthorized)
			if err := json.NewEncoder(w).Encode(map[string]string{"error": "unauthorized"}); err != nil {
				fmt.Fprintf(os.Stderr, "control: failed to write auth error response: %v\n", err)
			}
			return
		}
		next(w, r)
	}
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(map[string]string{
		"status":  "ok",
		"service": "detrix-client",
	}); err != nil {
		fmt.Fprintf(os.Stderr, "control: failed to write health response: %v\n", err)
	}
}

func (s *Server) handleStatus(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	if s.statusProvider == nil {
		w.WriteHeader(http.StatusInternalServerError)
		if err := json.NewEncoder(w).Encode(map[string]string{"error": "status provider not configured"}); err != nil {
			fmt.Fprintf(os.Stderr, "control: failed to write status error response: %v\n", err)
		}
		return
	}
	if err := json.NewEncoder(w).Encode(s.statusProvider()); err != nil {
		fmt.Fprintf(os.Stderr, "control: failed to write status response: %v\n", err)
	}
}

func (s *Server) handleInfo(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")

	status := s.statusProvider()
	name, _ := status["name"].(string)

	if err := json.NewEncoder(w).Encode(map[string]any{
		"name":       name,
		"pid":        os.Getpid(),
		"go_version": runtime.Version(),
	}); err != nil {
		fmt.Fprintf(os.Stderr, "control: failed to write info response: %v\n", err)
	}
}

func (s *Server) handleWake(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")

	if s.wakeHandler == nil {
		w.WriteHeader(http.StatusInternalServerError)
		if err := json.NewEncoder(w).Encode(map[string]string{"error": "wake handler not configured"}); err != nil {
			fmt.Fprintf(os.Stderr, "control: failed to write wake error response: %v\n", err)
		}
		return
	}

	// Parse optional daemon_url from request body
	var req struct {
		DaemonURL string `json:"daemon_url,omitempty"`
	}
	if r.Body != nil {
		// Ignore decode errors - daemon_url is optional
		_ = json.NewDecoder(r.Body).Decode(&req)
	}

	result, err := s.wakeHandler(req.DaemonURL)
	if err != nil {
		// Determine appropriate status code
		statusCode := http.StatusServiceUnavailable
		w.WriteHeader(statusCode)
		if encErr := json.NewEncoder(w).Encode(map[string]string{"error": err.Error()}); encErr != nil {
			fmt.Fprintf(os.Stderr, "control: failed to write wake error response: %v\n", encErr)
		}
		return
	}

	if err := json.NewEncoder(w).Encode(result); err != nil {
		fmt.Fprintf(os.Stderr, "control: failed to write wake response: %v\n", err)
	}
}

func (s *Server) handleSleep(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "application/json")

	if s.sleepHandler == nil {
		w.WriteHeader(http.StatusInternalServerError)
		if err := json.NewEncoder(w).Encode(map[string]string{"error": "sleep handler not configured"}); err != nil {
			fmt.Fprintf(os.Stderr, "control: failed to write sleep error response: %v\n", err)
		}
		return
	}

	result, err := s.sleepHandler()
	if err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		if encErr := json.NewEncoder(w).Encode(map[string]string{"error": err.Error()}); encErr != nil {
			fmt.Fprintf(os.Stderr, "control: failed to write sleep error response: %v\n", encErr)
		}
		return
	}

	if err := json.NewEncoder(w).Encode(result); err != nil {
		fmt.Fprintf(os.Stderr, "control: failed to write sleep response: %v\n", err)
	}
}
