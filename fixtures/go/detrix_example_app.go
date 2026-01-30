// Trading Bot Example for Go (Delve) Integration Testing
// This file matches the Python trade_bot_forever.py algorithm exactly
//
// To run with Delve DAP:
//   dlv debug --listen=:13640 --headless --api-version=2 --accept-multiclient ./detrix_example_app.go
//
// Or build and run separately:
//   go build -gcflags="all=-N -l" -o detrix_example_app detrix_example_app.go
//   dlv exec --listen=:13640 --headless --api-version=2 --accept-multiclient ./detrix_example_app
//
// With Detrix client enabled (for client tests):
//   DETRIX_CLIENT_ENABLED=1 DETRIX_DAEMON_URL=http://127.0.0.1:8090 go run .

package main

import (
	"fmt"
	"math/rand"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	detrix "github.com/flashus/detrix/clients/go"
)

var running = true

// Optional Detrix client initialization for client tests
// Set DETRIX_CLIENT_ENABLED=1 to enable, provide DETRIX_DAEMON_URL and DETRIX_CONTROL_PORT
func initDetrixClient() {
	if os.Getenv("DETRIX_CLIENT_ENABLED") != "1" {
		return
	}

	controlPort := 0
	if v := os.Getenv("DETRIX_CONTROL_PORT"); v != "" {
		if p, err := strconv.Atoi(v); err == nil {
			controlPort = p
		}
	}

	err := detrix.Init(detrix.Config{
		Name:        getEnvOrDefault("DETRIX_CLIENT_NAME", "trade-bot"),
		DaemonURL:   getEnvOrDefault("DETRIX_DAEMON_URL", "http://127.0.0.1:8090"),
		ControlPort: controlPort,
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "Failed to initialize Detrix client: %v\n", err)
		return
	}

	status := detrix.Status()
	fmt.Printf("Control plane: http://127.0.0.1:%d\n", status.ControlPort)
}

func getEnvOrDefault(key, defaultValue string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return defaultValue
}

func signalHandler(sigChan chan os.Signal) {
	<-sigChan
	fmt.Println("\nReceived shutdown signal, stopping...")
	running = false
}

func placeOrder(symbol string, quantity int, price float64) int {
	orderID := rand.Intn(9000) + 1000 // 1000-9999
	total := float64(quantity) * price
	fmt.Printf("Order #%d: %s x%d @ $%.2f = $%.2f\n", orderID, symbol, quantity, price, total)
	return orderID
}

func calculatePnl(entryPrice, currentPrice float64, quantity int) float64 {
	pnl := (currentPrice - entryPrice) * float64(quantity)
	return pnl
}

func main() {
	// Initialize Detrix client if enabled (13 lines added, matches Python offset)
	initDetrixClient()
	defer detrix.Shutdown()

	rand.Seed(time.Now().UnixNano())

	// Setup signal handler for graceful shutdown
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGTERM, syscall.SIGINT)
	go signalHandler(sigChan)

	fmt.Println("Trading bot started - runs forever until Ctrl+C")
	fmt.Println("Add metrics with Detrix to observe values!")
	fmt.Println()

	symbols := []string{"BTCUSD", "ETHUSD", "SOLUSD"}
	iteration := 0

	for running {
		iteration++
		// LINE NUMBERS BELOW ARE CRITICAL FOR E2E TESTS
		// Do not modify without updating dap_scenarios.rs
		symbol := symbols[rand.Intn(len(symbols))] // Line 106: symbol
		quantity := rand.Intn(50) + 1              // Line 107: quantity
		price := rand.Float64()*900 + 100          // Line 108: price

		// Line 111 - place_order call (symbol, quantity, price in scope)
		orderID := placeOrder(symbol, quantity, price) // Line 111: orderID

		// Calculate pnl
		entryPrice := price                                     // Line 114: entryPrice
		currentPrice := price * (0.95 + rand.Float64()*0.1)     // Line 115: currentPrice
		pnl := calculatePnl(entryPrice, currentPrice, quantity) // Line 116: pnl (all vars in scope)

		// Suppress unused variable warnings
		_ = orderID
		_ = pnl

		fmt.Printf("  -> P&L: $%.2f (iteration %d)\n", pnl, iteration) // Line 122: print (all vars in scope)

		time.Sleep(3 * time.Second) // Same as Python - 3 seconds
	}

	fmt.Println("Trading bot stopped!")
}
