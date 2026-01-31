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
//
// NOTE: Regular output goes to stderr to avoid SIGPIPE when the test harness
// closes stdout after reading the control plane URL.

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

// log writes to stderr to avoid SIGPIPE when stdout is closed
func log(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
}

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
	log("\nReceived shutdown signal, stopping...")
	running = false
}

func placeOrder(symbol string, quantity int, price float64) int {
	orderID := rand.Intn(9000) + 1000 // 1000-9999
	total := float64(quantity) * price
	log("Order #%d: %s x%d @ $%.2f = $%.2f", orderID, symbol, quantity, price, total)
	return orderID
}

func calculatePnl(entryPrice, currentPrice float64, quantity int) float64 {
	pnl := (currentPrice - entryPrice) * float64(quantity)
	return pnl
}

func main() {
	// Ignore SIGPIPE to prevent exit when stdout is closed by test harness
	signal.Ignore(syscall.SIGPIPE)

	// Initialize Detrix client if enabled
	initDetrixClient()
	defer detrix.Shutdown()

	rand.Seed(time.Now().UnixNano())

	// Setup signal handler for graceful shutdown
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGTERM, syscall.SIGINT)
	go signalHandler(sigChan)

	log("Trading bot started - runs forever until Ctrl+C")
	log("Add metrics with Detrix to observe values!")
	log("")

	symbols := []string{"BTCUSD", "ETHUSD", "SOLUSD"}
	iteration := 0

	for running {
		iteration++
		// LINE NUMBERS BELOW ARE CRITICAL FOR E2E TESTS
		// Do not modify without updating dap_scenarios.rs
		symbol := symbols[rand.Intn(len(symbols))] // Line 117: symbol
		quantity := rand.Intn(50) + 1              // Line 118: quantity
		price := rand.Float64()*900 + 100          // Line 119: price

		// Line 122 - place_order call (symbol, quantity, price in scope)
		orderID := placeOrder(symbol, quantity, price) // Line 122: orderID

		// Calculate pnl
		entryPrice := price                                     // Line 125: entryPrice
		currentPrice := price * (0.95 + rand.Float64()*0.1)     // Line 126: currentPrice
		pnl := calculatePnl(entryPrice, currentPrice, quantity) // Line 127: pnl (all vars in scope)

		// Suppress unused variable warnings
		_ = orderID // Line 130
		_ = pnl     // Line 131

		log("  -> P&L: $%.2f (iteration %d)", pnl, iteration) // Line 133: print (all vars in scope)

		time.Sleep(3 * time.Second) // Same as Python - 3 seconds
	}

	log("Trading bot stopped!")
}
