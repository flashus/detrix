// Trading Bot Example for Go (Delve) Integration Testing
// This file matches the Python trade_bot_forever.py algorithm exactly
//
// To run with Delve DAP:
//   dlv debug --listen=:13640 --headless --api-version=2 --accept-multiclient .
//
// Or build and run separately:
//   go build -gcflags="all=-N -l" -o detrix_example_app .
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

// Order represents a trading order (unexported fields for testing).
// NOTE: Fields like symbol, price, quantity exist to test scope-aware variable finding.
// When searching for "symbol", the struct field definition should be deprioritized
// vs the function body usage where the variable is actually in scope.
type Order struct {
	symbol   string
	quantity int
	price    float64
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

// tradeTick executes one trading iteration. Called by multiple goroutines
// so the e2e test can verify distinct goid capture across goroutines.
func tradeTick(workerID int) {
	symbols := []string{"BTCUSD", "ETHUSD", "SOLUSD"}
	// LINE NUMBERS: single source of truth is dap_scenarios.rs::go_lines
	// If you add/remove lines before tradeTick(), update go_lines::MAIN_LINE only.
	symbol := symbols[rand.Intn(len(symbols))]          // OFFSET_SYMBOL
	quantity := rand.Intn(50) + 1                       // OFFSET_QUANTITY
	price := rand.Float64()*900 + 100                   // OFFSET_PRICE
	direction := [2]string{"BUY", "SELL"}[rand.Intn(2)] // OFFSET_DIRECTION

	// Two different dynamic string creation methods for comprehensive testing:
	// 1. String concatenation with + operator (uses runtime.concatstrings)
	labelConcat := symbol + "_" + direction + "_" + fmt.Sprintf("%.0f", price) // OFFSET_LABEL_CONCAT
	// 2. Pure fmt.Sprintf (uses different runtime path)
	labelSprintf := fmt.Sprintf("%s_%s_%.0f", symbol, direction, price) // OFFSET_LABEL_SPRINTF

	// OFFSET_ORDER_ID - place_order call (symbol, quantity, price, label in scope)
	orderID := placeOrder(symbol, quantity, price) // OFFSET_ORDER_ID

	// Calculate pnl
	entryPrice := price                                     // OFFSET_ENTRY_PRICE
	currentPrice := price * (0.95 + rand.Float64()*0.1)     // OFFSET_CURRENT_PRICE
	pnl := calculatePnl(entryPrice, currentPrice, quantity) // OFFSET_PNL (all vars in scope)

	// Introspection breakpoint targets (must be real statements, not `_ = x`)
	_ = orderID                                                    // suppress unused
	_ = entryPrice                                                 // suppress unused
	_ = currentPrice                                               // suppress unused
	_ = labelSprintf                                               // suppress unused (captured by eBPF)
	log("  -> [%s] P&L: $%.2f (worker %d)", labelConcat, pnl, workerID) // OFFSET_LOG (all vars in scope)
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

	// Main goroutine runs trading loop alongside workers.
	for running {
		tradeTick(0)
		time.Sleep(3 * time.Second) // Same as Python - 3 seconds
	}

	log("Trading bot stopped!")
}

// ── Background goroutines for goid capture testing ───────────────────────────
// Spawn worker goroutines that call tradeTick() with different IDs.
// All workers hit the same PC offsets as main(), producing events with
// distinct goids that the e2e test verifies.
// Started via init() so they don't shift any lines in tradeTick().

func init() {
	// 3 worker goroutines — each gets a unique ID so they produce
	// different log output and distinct goids in captured events.
	for w := 1; w <= 3; w++ {
		go func(id int) {
			for running {
				tradeTick(id)
				time.Sleep(3 * time.Second)
			}
		}(w)
	}
}
