// Example basic demonstrates the Detrix Go client usage.
package main

import (
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	detrix "github.com/flashus/detrix/clients/go"
)

func main() {
	// Initialize Detrix client
	err := detrix.Init(detrix.Config{
		Name:      "example-app",
		DaemonURL: "http://127.0.0.1:8090",
	})
	if err != nil {
		log.Fatalf("Failed to initialize Detrix: %v", err)
	}
	defer func() {
		if err := detrix.Shutdown(); err != nil {
			log.Printf("Failed to shutdown Detrix: %v", err)
		}
	}()

	// Print control plane URL
	status := detrix.Status()
	fmt.Printf("Control plane: http://127.0.0.1:%d\n", status.ControlPort)
	fmt.Printf("State: %s\n", status.State)

	// Set up signal handling for graceful shutdown
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGTERM, syscall.SIGINT)

	// Run application loop
	ticker := time.NewTicker(1 * time.Second)
	defer ticker.Stop()

	counter := 0
	for {
		select {
		case <-sigCh:
			fmt.Println("\nShutting down...")
			return
		case <-ticker.C:
			counter++
			// Example: some application work
			result := processData(counter)
			fmt.Printf("Processed: %d -> %d\n", counter, result)
		}
	}
}

// processData is an example function that could be observed via Detrix.
func processData(input int) int {
	// Simulate some computation
	result := input * 2
	if input%10 == 0 {
		result += 100
	}
	return result
}
