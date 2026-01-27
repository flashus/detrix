// Test fixture: Impure function call chain (3 levels deep)
//
// Call chain: ProcessOrder -> SaveToDatabase -> WriteLog
// All functions have side effects (file I/O, fmt.Print, global state).
package main

import (
	"fmt"
	"os"
	"os/exec"
)

// Global state - side effect
var orderCount = 0
var orderHistory []map[string]interface{}

// WriteLog is Level 3: Impure function - writes to stdout (side effect).
func WriteLog(message string) {
	fmt.Printf("[LOG] %s\n", message)
}

// SaveToDatabase is Level 2: Impure function - calls WriteLog (has print side effect).
func SaveToDatabase(orderID string, data map[string]interface{}) bool {
	orderHistory = append(orderHistory, map[string]interface{}{
		"id":   orderID,
		"data": data,
	})
	WriteLog(fmt.Sprintf("Saved order %s", orderID))
	return true
}

// ProcessOrder is Level 1: Impure function - modifies global state and calls impure functions.
func ProcessOrder(orderID string, items []string, customer string) map[string]interface{} {
	orderCount++

	orderData := map[string]interface{}{
		"order_id": orderID,
		"items":    items,
		"customer": customer,
		"sequence": orderCount,
	}

	SaveToDatabase(orderID, orderData)
	return orderData
}

// ReadConfig is an impure function - reads from file system.
func ReadConfig(path string) (map[string]string, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return map[string]string{"content": string(content)}, nil
}

// ExecuteCommand is an impure function - executes system command.
func ExecuteCommand(cmd string) error {
	return exec.Command("sh", "-c", cmd).Run()
}

// SendNotification is an impure function - uses fmt.Print (I/O side effect).
func SendNotification(message string) {
	fmt.Printf("NOTIFICATION: %s\n", message)
}

// HttpFetch is an impure function - makes network request.
func HttpFetch(url string) (string, error) {
	// Note: Would use http.Get in real code, but we just mark it
	// This is for testing the impure function detection
	return "", fmt.Errorf("not implemented")
}
