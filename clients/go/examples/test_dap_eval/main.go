// Test DAP evaluate with function calls directly against Delve
//
// This test demonstrates:
// - Simple variables work in "watch" context (fast)
// - Function calls FAIL without "call" prefix
// - Function calls WORK with "call" prefix (but BLOCK the process)
//
// Usage:
//   1. Build and run the fixture in one terminal:
//      cd fixtures/go && go build -gcflags="all=-N -l" -o app . && ./app
//
//   2. In another terminal, attach Delve:
//      dlv attach $(pgrep -f "detrix_example_app") --headless --listen=:2345 --api-version=2
//
//   3. Run this test:
//      go run ./examples/test_dap_eval
//
// Expected results:
//   - "watch" context + simple variable: SUCCESS
//   - "repl" context + function call WITHOUT "call": FAIL
//   - "repl" context + function call WITH "call" prefix: SUCCESS (but blocks)
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"os"
	"time"
)

var seq = 0

func main() {
	addr := "127.0.0.1:2345"
	if len(os.Args) > 1 {
		addr = os.Args[1]
	}

	fmt.Printf("Connecting to Delve at %s...\n", addr)

	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	if err != nil {
		fmt.Printf("Failed to connect: %v\n", err)
		os.Exit(1)
	}
	defer func() {
		if err := conn.Close(); err != nil {
			log.Printf("Failed to close connection: %v", err)
		}
	}()

	reader := bufio.NewReader(conn)

	// Initialize
	sendRequest(conn, "initialize", map[string]any{
		"clientID":   "test",
		"adapterID":  "dlv",
		"pathFormat": "path",
	})
	readResponse(reader)

	// Attach (already attached, just configure)
	sendRequest(conn, "attach", map[string]any{
		"mode": "local",
	})
	readResponse(reader)

	// Set a breakpoint first
	fmt.Println("\nSetting breakpoint at line 111...")
	sendRequest(conn, "setBreakpoints", map[string]any{
		"source": map[string]any{
			"path": "/Users/ilyadyachenko/Documents/Yandex.Disk/_src/detrix/detrix-release/fixtures/go/detrix_example_app.go",
		},
		"breakpoints": []map[string]any{
			{"line": 111},
		},
	})
	readResponse(reader)

	// Continue to hit breakpoint
	fmt.Println("\nContinuing to hit breakpoint...")
	sendRequest(conn, "configurationDone", nil)
	readResponse(reader)

	// Wait for stopped event
	fmt.Println("\nWaiting for stopped event...")
	for i := 0; i < 10; i++ {
		resp := readResponse(reader)
		if resp != nil {
			if event, ok := resp["event"].(string); ok && event == "stopped" {
				fmt.Println("Hit breakpoint!")
				break
			}
		}
		time.Sleep(500 * time.Millisecond)
	}

	// Get stack trace to find frame ID
	fmt.Println("\nGetting stack trace...")
	sendRequest(conn, "stackTrace", map[string]any{
		"threadId":   1,
		"startFrame": 0,
		"levels":     1,
	})
	stackResp := readResponse(reader)

	frameId := int64(0)
	if body, ok := stackResp["body"].(map[string]any); ok {
		if frames, ok := body["stackFrames"].([]any); ok && len(frames) > 0 {
			if frame, ok := frames[0].(map[string]any); ok {
				if id, ok := frame["id"].(float64); ok {
					frameId = int64(id)
				}
			}
		}
	}
	fmt.Printf("Frame ID: %d\n", frameId)

	// Test evaluations
	// Key finding: Function calls in Delve DAP require "call " prefix!
	// See: https://github.com/golang/vscode-go/issues/100
	testCases := []struct {
		context string
		expr    string
	}{
		// Simple variables - work in any context
		{"watch", "symbol"},
		{"watch", "quantity"},
		{"watch", "price"},

		// Function calls WITHOUT "call" prefix - expected to FAIL
		{"repl", `fmt.Sprintf("test")`},
		{"repl", `len(symbol)`},

		// Function calls WITH "call" prefix - should WORK (EXPERIMENTAL)
		{"repl", `call fmt.Sprintf("test")`},
		{"repl", `call fmt.Sprintf("%s", symbol)`},
		{"repl", `call len(symbol)`},
	}

	fmt.Println("\n=== Testing evaluate requests ===")
	for _, tc := range testCases {
		fmt.Printf("\nContext: %s, Expression: %s\n", tc.context, tc.expr)
		sendRequest(conn, "evaluate", map[string]any{
			"expression": tc.expr,
			"frameId":    frameId,
			"context":    tc.context,
		})
		resp := readResponse(reader)
		if resp != nil {
			if body, ok := resp["body"].(map[string]any); ok {
				fmt.Printf("  Result: %v\n", body["result"])
			} else if msg, ok := resp["message"].(string); ok {
				fmt.Printf("  Error: %s\n", msg)
			}
			fmt.Printf("  Success: %v\n", resp["success"])
		}
	}

	// Continue execution
	sendRequest(conn, "continue", map[string]any{"threadId": 1})
	readResponse(reader)

	fmt.Println("\nDone!")
}

func sendRequest(conn net.Conn, command string, args map[string]any) {
	seq++
	msg := map[string]any{
		"seq":     seq,
		"type":    "request",
		"command": command,
	}
	if args != nil {
		msg["arguments"] = args
	}

	data, _ := json.Marshal(msg)
	header := fmt.Sprintf("Content-Length: %d\r\n\r\n", len(data))
	if _, err := conn.Write([]byte(header)); err != nil {
		log.Printf("Failed to write header: %v", err)
	}
	if _, err := conn.Write(data); err != nil {
		log.Printf("Failed to write data: %v", err)
	}
}

func readResponse(reader *bufio.Reader) map[string]any {
	// Read Content-Length header
	var contentLength int
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return nil
		}
		if line == "\r\n" {
			break
		}
		if _, err := fmt.Sscanf(line, "Content-Length: %d", &contentLength); err != nil {
			// Not a Content-Length header, continue
			continue
		}
	}

	if contentLength == 0 {
		return nil
	}

	// Read body
	body := make([]byte, contentLength)
	_, err := reader.Read(body)
	if err != nil {
		return nil
	}

	var result map[string]any
	if err := json.Unmarshal(body, &result); err != nil {
		log.Printf("Failed to parse response: %v", err)
		return nil
	}

	return result
}
