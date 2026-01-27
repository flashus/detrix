// Test fixture: Mixed purity function call chain
//
// Call chain: AnalyzeData -> ProcessItems -> LogResult
// Starts pure but ends with impure function call.
package main

import (
	"encoding/json"
	"fmt"
	"sort"
)

// Result represents the analysis result
type Result struct {
	Total   int     `json:"total"`
	Average float64 `json:"average"`
	Count   int     `json:"count"`
}

// LogResult is an impure function - prints to stdout.
func LogResult(result Result) {
	jsonBytes, _ := json.Marshal(result)
	fmt.Printf("Result: %s\n", string(jsonBytes))
}

// ProcessItems is a mixed function - does pure computation but calls impure LogResult.
func ProcessItems(items []int) Result {
	total := 0
	for _, item := range items {
		total += item
	}

	var average float64
	if len(items) > 0 {
		average = float64(total) / float64(len(items))
	}

	result := Result{
		Total:   total,
		Average: average,
		Count:   len(items),
	}

	LogResult(result) // Impure call!
	return result
}

// AnalyzeData is the entry point - appears pure but transitively calls impure function.
func AnalyzeData(data []int) Result {
	// Filter positive numbers
	filtered := make([]int, 0)
	for _, x := range data {
		if x > 0 {
			filtered = append(filtered, x)
		}
	}

	// Sort the data
	sort.Ints(filtered)

	return ProcessItems(filtered)
}

// ComputeStats is a pure function - only uses pure operations.
func ComputeStats(numbers []int) map[string]int {
	if len(numbers) == 0 {
		return map[string]int{"min": 0, "max": 0, "sum": 0}
	}

	minVal := numbers[0]
	maxVal := numbers[0]
	sum := 0

	for _, n := range numbers {
		if n < minVal {
			minVal = n
		}
		if n > maxVal {
			maxVal = n
		}
		sum += n
	}

	return map[string]int{
		"min": minVal,
		"max": maxVal,
		"sum": sum,
	}
}
