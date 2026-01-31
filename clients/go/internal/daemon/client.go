// Package daemon provides HTTP client for communicating with the Detrix daemon.
package daemon

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"time"
)

// Client is an HTTP client for the Detrix daemon.
type Client struct {
	httpClient *http.Client
}

// NewClient creates a new daemon client.
func NewClient() *Client {
	return &Client{
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// HealthCheck checks if the daemon is reachable.
func (c *Client) HealthCheck(daemonURL string, timeout time.Duration) error {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, "GET", daemonURL+"/health", nil)
	if err != nil {
		return fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("daemon not reachable at %s: %w", daemonURL, err)
	}
	defer func() {
		if err := resp.Body.Close(); err != nil {
			log.Printf("daemon: failed to close response body: %v", err)
		}
	}()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("daemon health check failed: status %d", resp.StatusCode)
	}

	return nil
}

// RegisterRequest is the request body for connection registration.
type RegisterRequest struct {
	Host     string `json:"host"`
	Port     int    `json:"port"`
	Language string `json:"language"`
	Name     string `json:"name"`
	Token    string `json:"token,omitempty"`
	SafeMode bool   `json:"safe_mode,omitempty"`
}

// RegisterResponse is the response from connection registration.
type RegisterResponse struct {
	ConnectionID string `json:"connectionId"`
	Status       string `json:"status"`
}

// Register registers a connection with the daemon.
func (c *Client) Register(daemonURL string, req RegisterRequest, timeout time.Duration) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	body, err := json.Marshal(req)
	if err != nil {
		return "", fmt.Errorf("failed to marshal request: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, "POST", daemonURL+"/api/v1/connections", bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return "", fmt.Errorf("failed to register with daemon: %w", err)
	}
	defer func() {
		if err := resp.Body.Close(); err != nil {
			log.Printf("daemon: failed to close response body: %v", err)
		}
	}()

	respBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return "", fmt.Errorf("registration failed: status %d, body: %s", resp.StatusCode, string(respBody))
	}

	var regResp RegisterResponse
	if err := json.Unmarshal(respBody, &regResp); err != nil {
		return "", fmt.Errorf("failed to parse response: %w", err)
	}

	return regResp.ConnectionID, nil
}

// Unregister unregisters a connection from the daemon.
// This is best-effort and errors are logged but not returned.
func (c *Client) Unregister(daemonURL string, connectionID string, timeout time.Duration) error {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, "DELETE", daemonURL+"/api/v1/connections/"+connectionID, nil)
	if err != nil {
		return fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("failed to unregister: %w", err)
	}
	defer func() {
		if err := resp.Body.Close(); err != nil {
			log.Printf("daemon: failed to close response body: %v", err)
		}
	}()

	// Accept 200, 204, or 404 (already deleted)
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNoContent && resp.StatusCode != http.StatusNotFound {
		return fmt.Errorf("unregister failed: status %d", resp.StatusCode)
	}

	return nil
}
