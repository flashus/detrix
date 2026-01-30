// Agent simulation test - wake a running Go app and observe it.
//
// This script simulates what an AI agent typically does with Detrix:
//  1. Starts a target application (detrix_example_app)
//  2. Waits for it to initialize
//  3. Wakes the Detrix client in the app via control plane
//  4. Sets metrics on the running process via daemon API
//  5. Receives and displays captured events
//
// Usage:
//
//  1. Make sure the Detrix daemon is running:
//     $ detrix serve --daemon
//
//  2. Build the fixture app (from fixtures/go directory):
//     $ go build -gcflags="all=-N -l" -o detrix_example_app .
//
//  3. Run this test (from clients/go directory):
//     $ go run ./examples/test_wake
//
//     Or with custom daemon port:
//     $ go run ./examples/test_wake --daemon-port 9999
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"syscall"
	"time"
)

var daemonURL = "http://127.0.0.1:8090"

func main() {
	// Parse command line args
	daemonPort := flag.Int("daemon-port", 8090, "Daemon port")
	daemonURLFlag := flag.String("daemon-url", "", "Full daemon URL (overrides --daemon-port)")
	fixtureDir := flag.String("fixture-dir", "", "Path to fixtures/go directory")
	flag.Parse()

	if *daemonURLFlag != "" {
		daemonURL = *daemonURLFlag
	} else {
		daemonURL = fmt.Sprintf("http://127.0.0.1:%d", *daemonPort)
	}

	// Find fixture directory
	fixturePath := *fixtureDir
	if fixturePath == "" {
		// Try to find it relative to current working directory
		// Expected locations:
		// - Running from clients/go: ../../fixtures/go
		// - Running from detrix-release: fixtures/go
		cwd, _ := os.Getwd()
		candidates := []string{
			filepath.Join(cwd, "..", "..", "fixtures", "go"), // from clients/go
			filepath.Join(cwd, "fixtures", "go"),             // from detrix-release
			filepath.Join(cwd, "..", "fixtures", "go"),       // from clients/
		}
		for _, p := range candidates {
			absPath, _ := filepath.Abs(p)
			if _, err := os.Stat(filepath.Join(absPath, "detrix_example_app.go")); err == nil {
				fixturePath = absPath
				break
			}
		}
	}

	if fixturePath == "" {
		fmt.Println("ERROR: Could not find fixtures/go directory")
		fmt.Println("Please specify with --fixture-dir flag")
		os.Exit(1)
	}

	os.Exit(run(fixturePath))
}

func run(fixtureDir string) int {
	fmt.Println(repeatStr("=", 70))
	fmt.Println("Agent Simulation Test - Wake and Observe a Running Go Application")
	fmt.Println(repeatStr("=", 70))
	fmt.Println()
	fmt.Printf("Daemon URL: %s\n", daemonURL)
	fmt.Printf("Fixture dir: %s\n", fixtureDir)
	fmt.Println()

	// Step 1: Check daemon is running
	fmt.Println("1. Checking Detrix daemon...")
	if !checkDaemonHealth() {
		fmt.Println("   ERROR: Detrix daemon is not running!")
		fmt.Println("   Start it with: detrix serve --daemon")
		return 1
	}
	fmt.Println("   Daemon is healthy")
	fmt.Println()

	// Step 2: Start the Go fixture app as subprocess
	fmt.Println("2. Starting Go fixture app...")

	// Build the app first (with debug flags)
	buildCmd := exec.Command("go", "build", "-gcflags=all=-N -l", "-o", "detrix_example_app", ".")
	buildCmd.Dir = fixtureDir
	buildCmd.Stdout = os.Stdout
	buildCmd.Stderr = os.Stderr
	if err := buildCmd.Run(); err != nil {
		fmt.Printf("   ERROR: Failed to build fixture app: %v\n", err)
		return 1
	}
	fmt.Println("   Built fixture app")

	// Start the app with Detrix client enabled
	appPath := filepath.Join(fixtureDir, "detrix_example_app")
	proc := exec.Command(appPath)
	proc.Env = append(os.Environ(),
		"DETRIX_CLIENT_ENABLED=1",
		"DETRIX_DAEMON_URL="+daemonURL,
		"DETRIX_CLIENT_NAME=trade-bot",
	)

	stdout, err := proc.StdoutPipe()
	if err != nil {
		fmt.Printf("   ERROR: Failed to get stdout: %v\n", err)
		return 1
	}

	proc.Stderr = os.Stderr

	if err := proc.Start(); err != nil {
		fmt.Printf("   ERROR: Failed to start app: %v\n", err)
		return 1
	}

	// Ensure cleanup on SIGTERM/SIGINT
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGTERM, syscall.SIGINT)
	go func() {
		<-sigCh
		proc.Process.Signal(syscall.SIGTERM)
	}()

	// Wait for control plane URL in output
	controlURL := ""
	controlPortRe := regexp.MustCompile(`Control plane: (http://[\d.:]+)`)

	scanner := bufio.NewScanner(stdout)
	timeout := time.After(10 * time.Second)

scanLoop:
	for {
		select {
		case <-timeout:
			fmt.Println("   ERROR: Timeout waiting for control plane URL")
			proc.Process.Kill()
			return 1
		default:
			if !scanner.Scan() {
				break scanLoop
			}
			line := scanner.Text()
			fmt.Printf("   [app] %s\n", line)

			if matches := controlPortRe.FindStringSubmatch(line); len(matches) > 1 {
				controlURL = matches[1]
				break scanLoop
			}
		}
	}

	if controlURL == "" {
		fmt.Println("   ERROR: Could not find control plane URL in output")
		proc.Process.Kill()
		return 1
	}

	fmt.Printf("   Found control plane: %s\n", controlURL)
	fmt.Println()

	// Continue reading app output in background
	go func() {
		io.Copy(io.Discard, stdout)
	}()

	// Step 3: Wait 3 seconds (simulating agent discovering the app)
	fmt.Println("3. Waiting 3 seconds (simulating agent discovery time)...")
	for i := 3; i > 0; i-- {
		fmt.Printf("   %d... ", i)
		time.Sleep(1 * time.Second)
	}
	fmt.Println()
	fmt.Println()

	// Step 4: Wake the client via control plane
	fmt.Println("4. Waking Detrix client...")
	wakeResp, err := apiRequest(controlURL+"/detrix/wake", "POST", nil)
	if err != nil {
		fmt.Printf("   ERROR: Failed to wake client: %v\n", err)
		proc.Process.Kill()
		return 1
	}

	wakeResult, ok := wakeResp.(map[string]interface{})
	if !ok {
		fmt.Println("   ERROR: Invalid wake response")
		proc.Process.Kill()
		return 1
	}

	fmt.Printf("   Status: %v\n", wakeResult["status"])
	fmt.Printf("   Debug port: %v\n", wakeResult["debug_port"])
	fmt.Printf("   Connection ID: %v\n", wakeResult["connection_id"])
	connectionID, _ := wakeResult["connection_id"].(string)
	fmt.Println()

	// Step 5: Verify connection on daemon
	fmt.Println("5. Verifying connection registered with daemon...")
	time.Sleep(1 * time.Second) // Give daemon time to establish DAP connection

	// Connection ID is based on host:port, not name (e.g., "127_0_0_1_61100")
	// Search by the connection ID returned from wake
	conn := getConnectionByID(connectionID)
	if conn != nil {
		statusMap := map[float64]string{1: "disconnected", 2: "connecting", 3: "connected"}
		status, _ := conn["status"].(float64)
		fmt.Printf("   Connection: %v\n", conn["connectionId"])
		fmt.Printf("   Status: %s\n", statusMap[status])
		fmt.Printf("   Port: %v\n", conn["port"])
	} else {
		fmt.Println("   WARNING: Connection not found on daemon")
	}
	fmt.Println()

	// Step 6: Add metrics
	fmt.Println("6. Adding metrics to observe the running application...")

	fixtureAppPath := filepath.Join(fixtureDir, "detrix_example_app.go")

	// Metric 1: SIMPLE VARIABLE (line 111) - symbol
	// Tests logpoint-based variable capture (non-blocking, RECOMMENDED)
	metric1 := addMetric(connectionID, fixtureAppPath, 111,
		`symbol`,
		"order_symbol", "go")

	var metric1ID float64
	if metric1 != nil {
		metric1ID, _ = metric1["metricId"].(float64)
		fmt.Printf("   Metric 1 (order_symbol @ line 111): ID=%.0f [SIMPLE VARIABLE - logpoint mode]\n", metric1ID)
	} else {
		fmt.Println("   WARNING: Failed to add metric 1")
	}

	// Metric 2: SIMPLE VARIABLE (line 122) - pnl
	// Tests logpoint-based variable capture (non-blocking, RECOMMENDED)
	metric2 := addMetric(connectionID, fixtureAppPath, 122,
		`pnl`,
		"pnl_value", "go")

	var metric2ID float64
	if metric2 != nil {
		metric2ID, _ = metric2["metricId"].(float64)
		fmt.Printf("   Metric 2 (pnl_value @ line 122): ID=%.0f [SIMPLE VARIABLE - logpoint mode]\n", metric2ID)
	} else {
		fmt.Println("   WARNING: Failed to add metric 2")
	}

	// Metric 3: FUNCTION CALL (line 116) - len(symbol)
	// Tests breakpoint-based function call (BLOCKING)
	// Note: Variadic functions (fmt.Sprintf, fmt.Println) are NOT supported by Delve
	// Only non-variadic functions work: len(), cap(), user methods with fixed args
	metric3 := addMetric(connectionID, fixtureAppPath, 116,
		`len(symbol)`,
		"symbol_length", "go")

	var metric3ID float64
	if metric3 != nil {
		metric3ID, _ = metric3["metricId"].(float64)
		fmt.Printf("   Metric 3 (symbol_length @ line 116): ID=%.0f [FUNCTION CALL - breakpoint mode]\n", metric3ID)
	} else {
		fmt.Println("   WARNING: Failed to add metric 3 (function call)")
	}

	// Metric 4: STACK TRACE CAPTURE (line 115) - entryPrice
	// Tests introspection with stack trace (BLOCKING)
	// Uses a simple variable but with captureStackTrace enabled
	metric4 := addMetricWithIntrospection(connectionID, fixtureAppPath, 115,
		`entryPrice`,
		"entry_price_with_stack", "go")

	var metric4ID float64
	if metric4 != nil {
		metric4ID, _ = metric4["metricId"].(float64)
		fmt.Printf("   Metric 4 (entry_price_with_stack @ line 115): ID=%.0f [WITH STACK TRACE]\n", metric4ID)
	} else {
		fmt.Println("   WARNING: Failed to add metric 4 (stack trace)")
	}
	fmt.Println()

	// Step 7: Wait and collect events
	fmt.Println("7. Waiting for events (15 seconds)...")
	fmt.Println()

	eventsReceived := 0
	seenEvents := make(map[string]bool)

	// Helper to check and print events for a metric
	checkEvents := func(metricID float64, metricName string) {
		if metricID <= 0 {
			return
		}
		events := getEvents(int(metricID), 10)
		for _, event := range events {
			eventID := fmt.Sprintf("%.0f-%v", metricID, event["timestamp"])
			if seenEvents[eventID] {
				continue
			}
			seenEvents[eventID] = true
			eventsReceived++

			result, _ := event["result"].(map[string]interface{})
			value := ""
			if result != nil {
				value, _ = result["valueJson"].(string)
			}
			ts := formatTimestamp(event["timestamp"])
			fmt.Printf("   [EVENT] %s: %s (%s)\n", metricName, value, ts)

			// Check for stack trace
			if stackTrace, ok := event["stackTrace"].(map[string]interface{}); ok {
				if frames, ok := stackTrace["frames"].([]interface{}); ok && len(frames) > 0 {
					fmt.Printf("           Stack trace (%d frames):\n", len(frames))
					for i, f := range frames {
						if i >= 5 {
							fmt.Printf("             ... and %d more frames\n", len(frames)-5)
							break
						}
						frame, _ := f.(map[string]interface{})
						name, _ := frame["name"].(string)
						file, _ := frame["file"].(string)
						line, _ := frame["line"].(float64)
						fmt.Printf("             [%d] %s (%s:%d)\n", i, name, filepath.Base(file), int(line))
					}
				}
			}
		}
	}

	for i := 0; i < 15; i++ {
		time.Sleep(1 * time.Second)
		checkEvents(metric1ID, "order_symbol")
		checkEvents(metric2ID, "pnl_value")
		checkEvents(metric3ID, "symbol_length")
		checkEvents(metric4ID, "entry_price_with_stack")
	}

	fmt.Println()
	fmt.Printf("   Total events received: %d\n", eventsReceived)
	fmt.Println()

	// Step 8: Cleanup
	fmt.Println("8. Cleaning up...")

	// Stop the app
	proc.Process.Signal(syscall.SIGTERM)
	done := make(chan error)
	go func() {
		done <- proc.Wait()
	}()

	select {
	case <-time.After(5 * time.Second):
		proc.Process.Kill()
		fmt.Println("   App killed (timeout)")
	case <-done:
		fmt.Println("   App stopped")
	}

	fmt.Println()
	fmt.Println(repeatStr("=", 70))
	if eventsReceived > 0 {
		fmt.Println("TEST PASSED - Successfully observed running Go application!")
	} else {
		fmt.Println("TEST COMPLETED - No events captured (check metric expressions)")
	}
	fmt.Println(repeatStr("=", 70))

	return 0
}

// Helper functions

func repeatStr(s string, n int) string {
	result := ""
	for i := 0; i < n; i++ {
		result += s
	}
	return result
}

func apiRequest(url string, method string, data interface{}) (interface{}, error) {
	var body io.Reader
	if data != nil {
		jsonData, err := json.Marshal(data)
		if err != nil {
			return nil, err
		}
		body = bytes.NewReader(jsonData)
	}

	req, err := http.NewRequest(method, url, body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")

	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("HTTP %d: %s", resp.StatusCode, string(respBody))
	}

	var result interface{}
	if err := json.Unmarshal(respBody, &result); err != nil {
		return nil, err
	}
	return result, nil
}

func checkDaemonHealth() bool {
	resp, err := apiRequest(daemonURL+"/health", "GET", nil)
	if err != nil {
		return false
	}
	result, ok := resp.(map[string]interface{})
	if !ok {
		return false
	}
	return result["service"] == "detrix"
}

func getConnectionByID(connectionID string) map[string]interface{} {
	resp, err := apiRequest(daemonURL+"/api/v1/connections", "GET", nil)
	if err != nil {
		return nil
	}
	connections, ok := resp.([]interface{})
	if !ok {
		return nil
	}
	for _, c := range connections {
		conn, ok := c.(map[string]interface{})
		if !ok {
			continue
		}
		connID, _ := conn["connectionId"].(string)
		if connID == connectionID {
			return conn
		}
	}
	return nil
}

func addMetric(connectionID, filePath string, line int, expression, name, language string) map[string]interface{} {
	data := map[string]interface{}{
		"name":         name,
		"connectionId": connectionID,
		"location": map[string]interface{}{
			"file": filePath,
			"line": line,
		},
		"expression": expression,
		"language":   language,
		"enabled":    true,
	}

	resp, err := apiRequest(daemonURL+"/api/v1/metrics", "POST", data)
	if err != nil {
		fmt.Printf("   Error adding metric: %v\n", err)
		return nil
	}

	result, ok := resp.(map[string]interface{})
	if !ok {
		return nil
	}
	return result
}

func addMetricWithIntrospection(connectionID, filePath string, line int, expression, name, language string) map[string]interface{} {
	data := map[string]interface{}{
		"name":         name,
		"connectionId": connectionID,
		"location": map[string]interface{}{
			"file": filePath,
			"line": line,
		},
		"expression":        expression,
		"language":          language,
		"enabled":           true,
		"captureStackTrace": true, // Forces breakpoint mode for function calls
	}

	resp, err := apiRequest(daemonURL+"/api/v1/metrics", "POST", data)
	if err != nil {
		fmt.Printf("   Error adding metric with introspection: %v\n", err)
		return nil
	}

	result, ok := resp.(map[string]interface{})
	if !ok {
		return nil
	}
	return result
}

func getEvents(metricID int, limit int) []map[string]interface{} {
	url := fmt.Sprintf("%s/api/v1/events?metricId=%d&limit=%d", daemonURL, metricID, limit)
	resp, err := apiRequest(url, "GET", nil)
	if err != nil {
		return nil
	}

	events, ok := resp.([]interface{})
	if !ok {
		return nil
	}

	result := make([]map[string]interface{}, 0, len(events))
	for _, e := range events {
		if event, ok := e.(map[string]interface{}); ok {
			result = append(result, event)
		}
	}
	return result
}

func formatTimestamp(ts interface{}) string {
	switch v := ts.(type) {
	case float64:
		if v > 0 {
			t := time.UnixMicro(int64(v))
			return t.Format("15:04:05")
		}
	case int64:
		if v > 0 {
			t := time.UnixMicro(v)
			return t.Format("15:04:05")
		}
	}
	return fmt.Sprintf("%v", ts)
}
