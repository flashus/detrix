// Classic Map Fixture for eBPF Integration Testing
// Tests map capture with Go < 1.24 classic hash map implementation.
//
// Build with Go 1.23 or earlier:
//   go build -gcflags="all=-N -l" -o classic_map .
//
// Test capture points:
//   - OFFSET_ORDER:       Order struct with Tags map field
//   - OFFSET_NIL_MAP:     Nil map pointer
//
// DETERMINISTIC VALUES:
//   order.Tags:  {"env": "test", "version": "1.0"}

package main

import (
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"
)

// Order wraps a map field for testing (same pattern as nested_types fixture)
type Order struct {
	ID   int
	Tags map[string]string
}

func main() {
	signal.Ignore(syscall.SIGPIPE)

	fmt.Fprintln(os.Stderr, "Classic map fixture started")
	fmt.Fprintln(os.Stderr, "Testing: struct with map field, nil map")
	fmt.Fprintln(os.Stderr, "")

	iteration := 0
	for running {
		iteration++

		// Create order with map field (classic map, Go 1.23)
		order := Order{
			ID: iteration,
			Tags: map[string]string{
				"env":     "test",
				"version": "1.0",
			},
		}

		// Nil map for testing
		var nilMap map[string]int

		// OFFSET_ORDER - capture struct with map field (non-variadic call)
		logOrder(order) // OFFSET_ORDER

		// OFFSET_NIL_MAP - capture nil map
		logNilMap(nilMap) // OFFSET_NIL_MAP

		time.Sleep(3 * time.Second)
	}

	fmt.Fprintln(os.Stderr, "Classic map fixture stopped! Total iterations:", iteration)
}

var running = true

// Non-variadic function — forces compiler to keep order on stack with DWARF info
func logOrder(o Order) {
	fmt.Fprintln(os.Stderr, "order:", o.ID, o.Tags)
}

// Non-variadic function — forces compiler to keep nilMap on stack with DWARF info
func logNilMap(m map[string]int) {
	fmt.Fprintln(os.Stderr, "nilMap:", m)
}
