package main

import (
	"encoding/json"
	"fmt"
	"math/rand"
	"net/http"
	"os"
)

type Transaction struct {
	ID          string  `json:"id"`
	Amount      float64 `json:"amount"`
	Currency    string  `json:"currency"`
	Unit        string  `json:"unit"`
	Description string  `json:"description"`
}

var descriptions = []string{
	"Enterprise license renewal",
	"Cloud infrastructure billing",
	"Professional services",
	"Support tier upgrade",
	"Data transfer fees",
	"API usage charges",
	"Storage expansion",
	"Compute instance hours",
	"SSL certificate renewal",
	"Domain registration",
}

func generateTransactions() []Transaction {
	txns := make([]Transaction, 5)
	for i := range txns {
		id := fmt.Sprintf("TXN-%05d", rand.Intn(99999))
		desc := descriptions[rand.Intn(len(descriptions))]
		baseAmount := 20.0 + rand.Float64()*180.0 // $20-$200

		var amount float64
		var unit string
		if rand.Float64() < 0.3 {
			amount = float64(int(baseAmount*100)) // e.g. 4999
			unit = "cents"
		} else {
			amount = float64(int(baseAmount*100)) / 100.0 // e.g. 49.99
			unit = "dollars"
		}

		txns[i] = Transaction{
			ID:          id,
			Amount:      amount,
			Currency:    "USD",
			Unit:        unit,
			Description: desc,
		}
	}
	return txns
}

func handleTransactions(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	txns := generateTransactions()
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(txns)
}

func handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func main() {
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	http.HandleFunc("/api/transactions", handleTransactions)
	http.HandleFunc("/health", handleHealth)

	fmt.Fprintf(os.Stderr, "pricing-api listening on :%s\n", port)
	if err := http.ListenAndServe(":"+port, nil); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
