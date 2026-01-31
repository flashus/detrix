// Long-running trading bot with Detrix client - runs until Ctrl+C
//
// This is a typical application that integrates the Detrix Go client.
// It initializes the client but stays in SLEEPING state, exposing a control
// plane endpoint for external agents to wake it when observability is needed.
//
// Usage:
//
//	1. Run this bot:
//	   $ go run ./examples/trade_bot
//
//	Or with custom daemon URL:
//	   $ go run ./examples/trade_bot --daemon-url http://127.0.0.1:9999
//
//	2. The bot will print its control plane URL.
//	   An agent can then POST to /detrix/wake to enable observability.
//
//	3. Press Ctrl+C to stop.
package main

import (
	"flag"
	"fmt"
	"math/rand"
	"os"
	"os/signal"
	"syscall"
	"time"

	detrix "github.com/flashus/detrix/clients/go"
)

var running = true

func main() {
	daemonURL := flag.String("daemon-url", os.Getenv("DETRIX_DAEMON_URL"), "Daemon URL")
	flag.Parse()

	if *daemonURL == "" {
		*daemonURL = "http://127.0.0.1:8090"
	}

	// Set up signal handling
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGTERM, syscall.SIGINT)
	go func() {
		<-sigCh
		fmt.Println("\nReceived shutdown signal, stopping...")
		running = false
	}()

	// Initialize Detrix client - starts control plane but stays SLEEPING
	// No debugger loaded, no daemon interaction yet - zero overhead
	fmt.Println("Initializing Detrix client...")
	err := detrix.Init(detrix.Config{
		Name:      "trade-bot",
		DaemonURL: *daemonURL,
	})
	if err != nil {
		fmt.Printf("Failed to initialize Detrix: %v\n", err)
		return
	}
	defer func() {
		if err := detrix.Shutdown(); err != nil {
			fmt.Printf("Failed to shutdown Detrix: %v\n", err)
		}
	}()

	status := detrix.Status()
	controlURL := fmt.Sprintf("http://127.0.0.1:%d", status.ControlPort)
	fmt.Printf("  State: %s\n", status.State)
	fmt.Printf("  Control plane: %s\n", controlURL)
	fmt.Println()
	fmt.Println("  To enable observability, an agent can POST to:")
	fmt.Printf("    curl -X POST %s/detrix/wake\n", controlURL)
	fmt.Println()

	fmt.Println("============================================================")
	fmt.Println("Trading bot started - runs forever until Ctrl+C")
	fmt.Println("============================================================")
	fmt.Println()

	// Note: rand.Seed is deprecated in Go 1.20+ - the global rand is auto-seeded
	symbols := []string{"BTCUSD", "ETHUSD", "SOLUSD"}
	iteration := 0

	for running {
		iteration++
		symbol := symbols[rand.Intn(len(symbols))]
		quantity := rand.Intn(50) + 1
		price := rand.Float64()*900 + 100

		// Line ~83 - placeOrder call (good place for a metric)
		orderID := placeOrder(symbol, quantity, price)

		// Lines ~86-88 - calculate pnl (good place for metrics)
		entryPrice := price
		currentPrice := price * (0.95 + rand.Float64()*0.1)
		pnl := calculatePnl(entryPrice, currentPrice, quantity)

		// Suppress unused variable warning
		_ = orderID

		fmt.Printf("    -> P&L: $%.2f (iteration %d)\n", pnl, iteration)
		fmt.Println()

		time.Sleep(2 * time.Second)
	}

	fmt.Println("Shutting down Detrix client...")
	fmt.Println("Trading bot stopped!")
}

func placeOrder(symbol string, quantity int, price float64) int {
	orderID := rand.Intn(9000) + 1000 // 1000-9999
	total := float64(quantity) * price
	fmt.Printf("  Order #%d: %s x%d @ $%.2f = $%.2f\n", orderID, symbol, quantity, price, total)
	return orderID
}

func calculatePnl(entryPrice, currentPrice float64, quantity int) float64 {
	pnl := (currentPrice - entryPrice) * float64(quantity)
	return pnl
}
