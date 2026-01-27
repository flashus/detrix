// Trading Bot Example for Go (Delve) Integration Testing
// This file matches the Python trade_bot_forever.py algorithm exactly
//
// To run with Delve DAP:
//   dlv debug --listen=:13640 --headless --api-version=2 --accept-multiclient ./detrix_example_app.go
//
// Or build and run separately:
//   go build -gcflags="all=-N -l" -o detrix_example_app detrix_example_app.go
//   dlv exec --listen=:13640 --headless --api-version=2 --accept-multiclient ./detrix_example_app

package main

import (
	"fmt"
	"math/rand"
	"os"
	"os/signal"
	"syscall"
	"time"
)

var running = true

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
		symbol := symbols[rand.Intn(len(symbols))]
		quantity := rand.Intn(50) + 1                       // 1-50
		price := rand.Float64()*900 + 100                   // 100-1000

		// Line 62 - place_order call (matches Python line 46)
		orderID := placeOrder(symbol, quantity, price)

		// Calculate pnl (matches Python line 51)
		entryPrice := price
		currentPrice := price * (0.95 + rand.Float64()*0.1) // 0.95-1.05
		pnl := calculatePnl(entryPrice, currentPrice, quantity)

		// Suppress unused variable warnings
		_ = orderID
		_ = pnl

		fmt.Printf("  -> P&L: $%.2f (iteration %d)\n", pnl, iteration)
		fmt.Println()

		time.Sleep(3 * time.Second) // Same as Python - 3 seconds
	}

	fmt.Println("Trading bot stopped!")
}
