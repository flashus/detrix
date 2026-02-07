// Example basic_usage demonstrates the Detrix Go client usage.
//
// Prerequisites:
//
//	1. Start the Detrix daemon first:
//	   $ detrix serve --daemon
//
//	2. Run this example (from clients/go directory):
//	   $ go run ./examples/basic_usage
//
// The client will:
//
//	1. Start a control plane server for remote management
//	2. Stay in SLEEPING state (zero overhead)
//	3. On wake(): start Delve and register with daemon
//	4. Allow the daemon to set metrics/observation points
//	5. On sleep(): stop debugger and unregister
package main

import (
	"fmt"
	"time"

	detrix "github.com/flashus/detrix/clients/go"
)

func main() {
	// Initialize client - starts control plane but stays SLEEPING
	// No debugger loaded, no daemon interaction yet
	fmt.Println("Initializing Detrix client...")
	err := detrix.Init(detrix.Config{
		Name:      "example-service",
		DaemonURL: "http://127.0.0.1:8090", // Default daemon URL
	})
	if err != nil {
		fmt.Printf("Failed to initialize: %v\n", err)
		return
	}
	defer func() {
		if err := detrix.Shutdown(); err != nil {
			fmt.Printf("Failed to shutdown Detrix: %v\n", err)
		}
	}()

	status := detrix.Status()
	fmt.Printf("Status after init: %s\n", status.State)
	fmt.Printf("Control plane at: 127.0.0.1:%d\n", status.ControlPort)

	// Wake up - start debugger and register with daemon
	fmt.Println("\nWaking up (starting debugger, registering with daemon)...")
	result, err := detrix.Wake()
	if err != nil {
		fmt.Printf("Could not connect to daemon: %v\n", err)
		fmt.Println("Make sure the daemon is running: detrix serve --daemon")
	} else {
		fmt.Printf("Wake result: %+v\n", result)
		fmt.Printf("Debug port: %d\n", result.DebugPort)
		fmt.Printf("Connection ID: %s\n", result.ConnectionId)

		// Now the daemon can set metrics on this process
		// Simulate some work
		fmt.Println("\nRunning application logic...")
		for i := 0; i < 5; i++ {
			processRequest(i)
			time.Sleep(1 * time.Second)
		}
	}

	// Sleep - stop debugger and unregister
	fmt.Println("\nGoing to sleep...")
	if _, err := detrix.Sleep(); err != nil {
		fmt.Printf("Failed to sleep: %v\n", err)
	}
	fmt.Printf("Status after sleep: %s\n", detrix.Status().State)

	// Cleanup
	fmt.Println("\nShutting down...")
	fmt.Println("Done!")
}

// processRequest simulates processing a request.
//
// The Detrix daemon can set observation points on any line in this function
// to capture variable values without modifying the code.
func processRequest(requestID int) {
	data := map[string]interface{}{
		"id":        requestID,
		"processed": true,
	}
	result := transformData(data)
	fmt.Printf("  Processed request %d: %v\n", requestID, result)
}

// transformData transforms data.
//
// Example metric: Add observation at line 82 to capture 'data' value.
func transformData(data map[string]interface{}) map[string]interface{} {
	data["transformed"] = true
	data["timestamp"] = time.Now().Unix()
	return data
}
