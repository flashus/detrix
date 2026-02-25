package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	detrix "github.com/flashus/detrix/clients/go"
)

type Transaction struct {
	ID          string  `json:"id"`
	Amount      float64 `json:"amount"`
	Currency    string  `json:"currency"`
	Unit        string  `json:"unit"`
	Description string  `json:"description"`
}

var running = true

func log(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
}

func getEnv(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

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
		Name:        getEnv("DETRIX_CLIENT_NAME", "order-service"),
		DaemonURL:   getEnv("DETRIX_DAEMON_URL", "http://127.0.0.1:8090"),
		ControlPort: controlPort,
	})
	if err != nil {
		log("Detrix client init failed: %v", err)
		return
	}

	status := detrix.Status()
	fmt.Printf("Control plane: http://127.0.0.1:%d\n", status.ControlPort)
}

func fetchTransactions(apiURL string) ([]Transaction, error) {
	resp, err := http.Get(apiURL + "/api/transactions")
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var txns []Transaction
	if err := json.NewDecoder(resp.Body).Decode(&txns); err != nil {
		return nil, err
	}
	return txns, nil
}

func calculateRevenue(transactions []Transaction) float64 {
	total := 0.0
	for _, txn := range transactions {
		amount := txn.Amount
		unit := txn.Unit
		_ = unit
		total += amount
	}
	return total
}

func main() {
	signal.Ignore(syscall.SIGPIPE)

	initDetrixClient()
	defer detrix.Shutdown()

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGTERM, syscall.SIGINT)
	go func() {
		<-sigChan
		log("Received shutdown signal, stopping...")
		running = false
	}()

	apiURL := getEnv("PRICING_API_URL", "http://pricing-api:8080")
	interval := 3 * time.Second

	log("order-service started")
	log("Fetching transactions from %s every %v", apiURL, interval)
	log("")

	batch := 0
	dailyRevenue := 0.0

	for running {
		batch++
		transactions, err := fetchTransactions(apiURL)
		if err != nil {
			log("Batch #%d: fetch error: %v", batch, err)
			time.Sleep(interval)
			continue
		}

		batchRevenue := calculateRevenue(transactions)
		dailyRevenue += batchRevenue

		log("Batch #%d: %d transactions, batch revenue: $%.2f, daily total: $%.2f",
			batch, len(transactions), batchRevenue, dailyRevenue)

		time.Sleep(interval)
	}

	log("order-service stopped. Final daily revenue: $%.2f", dailyRevenue)
}
