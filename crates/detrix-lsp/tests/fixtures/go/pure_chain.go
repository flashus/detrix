// Test fixture: Pure function call chain (3 levels deep)
//
// Call chain: CalculateTotal -> ComputeSubtotal -> ComputeItemPrice
// All functions are pure (no side effects).
package main

import (
	"fmt"
	"math"
)

// Item represents a single item with quantity and price
type Item struct {
	Quantity  int
	UnitPrice float64
}

// ComputeItemPrice is Level 3: Pure function - computes item price using math operations.
func ComputeItemPrice(quantity int, unitPrice float64) float64 {
	return math.Round(float64(quantity)*unitPrice*100) / 100
}

// ComputeSubtotal is Level 2: Pure function - calls ComputeItemPrice for each item.
func ComputeSubtotal(items []Item) float64 {
	total := 0.0
	for _, item := range items {
		total += ComputeItemPrice(item.Quantity, item.UnitPrice)
	}
	return total
}

// CalculateTotal is Level 1: Pure function - calls ComputeSubtotal and applies tax.
func CalculateTotal(items []Item, taxRate float64) float64 {
	subtotal := ComputeSubtotal(items)
	tax := math.Floor(subtotal*taxRate*100) / 100
	return subtotal + tax
}

// FormatCurrency is a pure function - formats amount as currency string.
func FormatCurrency(amount float64) string {
	return fmt.Sprintf("$%.2f", amount)
}

// ValidateItems is a pure function - validates items slice.
func ValidateItems(items []Item) bool {
	if len(items) == 0 {
		return false
	}
	for _, item := range items {
		if item.Quantity < 0 || item.UnitPrice < 0 {
			return false
		}
	}
	return true
}
