// Complex Types Fixture for eBPF Integration Testing
// Tests nested structs, slices, maps, pointers, interfaces with depth-limited capture
//
// Build: go build -gcflags="all=-N -l" -o nested_types .
//
// Type hierarchy depth:
//   Order (depth 0)
//   ├─ Product (depth 1)
//   │  └─ Category (depth 2)
//   │     └─ Metadata (depth 3)
//   ├─ Trader (depth 1)
//   │  └─ Address (depth 2)
//   ├─ []OrderItem (depth 1) - slice
//   │  └─ OrderItem (depth 2)
//   │     └─ Product (depth 3) - circular reference
//   └─ map[string]Tag (depth 1) - map
//
// Test capture points (use find_logpoint() in tests):
//   - OFFSET_ORDER: Capture full Order struct
//   - OFFSET_HISTORY: Capture struct with fixed-size array
//   - OFFSET_POINTER: Capture pointer to struct
//   - OFFSET_ITEMS: Capture slice items
//   - OFFSET_MAP: Capture map values
//   - OFFSET_STATUS: Capture enum-like type
//   - OFFSET_DEEP: Capture deeply nested field
//   - OFFSET_TIME: Capture time.Time struct
//
// DETERMINISTIC VALUES (for testing):
//   - Iteration 1: ID=1, Product.Name="Laptop", Trader.Name="Alice", Status="PENDING"
//   - Iteration 2: ID=2, Product.Name="Phone", Trader.Name="Bob", Status="CONFIRMED"
//   - Iteration 3: ID=3, Product.Name="Tablet", Trader.Name="Charlie", Status="SHIPPED"
//   - Fixed array: Prices=[100.5, 101.2, 99.8, 102.1, 100.0], Avg=100.72

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"time"
)

// Metadata - depth 3 (leaf)
type Metadata struct {
	CreatedAt time.Time
	Tags      []string
	Extra     map[string]string
}

// Category - depth 2
type Category struct {
	ID       int
	Name     string
	Metadata Metadata
}

// Product - depth 1
type Product struct {
	SKU      string
	Name     string
	Price    float64
	Category Category
}

// Address - depth 2
type Address struct {
	Street  string
	City    string
	Country string
	Zip     string
}

// Trader - depth 1
type Trader struct {
	ID      int
	Name    string
	Address Address
	Email   string
}

// OrderItem - depth 2 (contains reference back to Product)
type OrderItem struct {
	Product   Product
	Quantity  int
	Total     float64
}

// Tag - simple type for map testing
type Tag struct {
	Key   string
	Value string
}

// Order - root struct (depth 0)
type Order struct {
	ID        int
	Product   Product      // depth 1
	Trader    Trader       // depth 1
	Items     []OrderItem  // depth 1 (slice)
	Tags      map[string]Tag // depth 1 (map)
	Total     float64
	Timestamp time.Time
	Status    OrderStatus
}

// PriceHistory - fixed-size array test
type PriceHistory struct {
	Prices [5]float64  // Fixed-size array
	Avg    float64
}

// OrderPtr - pointer test wrapper
type OrderPtr struct {
	Order *Order
	Count int
}

// OrderStatus - enum-like type
type OrderStatus string

const (
	StatusPending   OrderStatus = "PENDING"
	StatusConfirmed OrderStatus = "CONFIRMED"
	StatusShipped   OrderStatus = "SHIPPED"
	StatusDelivered OrderStatus = "DELIVERED"
)

// Global for testing pointer capture
var globalOrder *Order

func main() {
	// Ignore SIGPIPE
	signal.Ignore(syscall.SIGPIPE)

	log("Complex types fixture started")
	log("Testing: nested structs, slices, maps, pointers, interfaces")
	log("")

	iteration := 0

	for running {
		iteration++

		// Create deterministic nested order structure
		order := createOrder(iteration)

		// Test pointer capture
		globalOrder = &order

		// Create array test data (always the same values)
		history := PriceHistory{
			Prices: [5]float64{100.5, 101.2, 99.8, 102.1, 100.0},
			Avg:    100.72,
		}

		// Create pointer wrapper test data
		ptrWrapper := OrderPtr{
			Order: &order,
			Count: iteration,
		}

		// OFFSET_ORDER - main capture point (all fields in scope)
		logOrder(order) // OFFSET_ORDER

		// OFFSET_HISTORY - capture struct with fixed-size array
		_ = history // OFFSET_HISTORY

		// OFFSET_POINTER - capture pointer to struct
		_ = ptrWrapper // OFFSET_POINTER

		// OFFSET_ITEMS - capture slice items
		for i, item := range order.Items {
			_ = i
			_ = item // OFFSET_ITEMS
		}

		// OFFSET_MAP - capture map values
		for key, tag := range order.Tags {
			_ = key
			_ = tag // OFFSET_MAP
		}

		// OFFSET_STATUS - capture enum-like type
		status := order.Status // OFFSET_STATUS

		// OFFSET_DEEP - capture deeply nested field
		categoryName := order.Product.Category.Name // OFFSET_DEEP

		// OFFSET_TIME - capture time.Time struct
		timestamp := order.Timestamp // OFFSET_TIME

		_ = status
		_ = categoryName
		_ = timestamp

		time.Sleep(3 * time.Second)
	}

	log("Complex types fixture stopped! Total iterations:", iteration)
}

var running = true

// createOrder creates a deterministic Order based on iteration number
func createOrder(iteration int) Order {
	// Deterministic status selection based on iteration
	statuses := []OrderStatus{StatusPending, StatusConfirmed, StatusShipped, StatusDelivered}
	statusIndex := (iteration - 1) % len(statuses)

	// Fixed timestamp for deterministic testing
	// Using a verifiable date: 2026-04-02 19:20:00 UTC + iteration offset
	// Unix timestamp: 1775102400 (2026-04-02 19:20:00 UTC)
	timestamp := time.Unix(1775102400+int64(iteration*1000), 0)

	return Order{
		ID:        iteration,
		Product:   createProduct(iteration),
		Trader:    createTrader(iteration),
		Items:     createItems(iteration),
		Tags:      createTags(iteration),
		Total:     float64(iteration) * 100.5,
		Timestamp: timestamp,
		Status:    statuses[statusIndex],
	}
}

// createProduct creates a deterministic Product
func createProduct(iteration int) Product {
	productNames := []string{"Laptop", "Phone", "Tablet", "Monitor", "Keyboard", "Mouse", "Headphones", "Camera"}
	productIndex := (iteration - 1) % len(productNames)

	categoryNames := []string{"Electronics", "Accessories", "Computing", "Audio", "Photography"}
	categoryIndex := (iteration - 1) % len(categoryNames)

	// Fixed CreatedAt timestamp for deterministic testing
	createdAt := time.Unix(int64(iteration*1000000), 0)

	return Product{
		SKU:   fmt.Sprintf("SKU-%04d", iteration*1111%10000),
		Name:  productNames[productIndex],
		Price: float64(iteration) * 50.25,
		Category: Category{
			ID:   iteration * 10 % 100,
			Name: categoryNames[categoryIndex],
			Metadata: Metadata{
				CreatedAt: createdAt,
				Tags:      []string{"tag1", "tag2", "tag3"},
				Extra:     map[string]string{"source": "api", "version": "1.0"},
			},
		},
	}
}

// createTrader creates a deterministic Trader
func createTrader(iteration int) Trader {
	traderNames := []string{"Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace", "Henry"}
	traderIndex := (iteration - 1) % len(traderNames)

	streetNames := []string{"Main", "Oak", "Maple", "Cedar", "Pine", "Elm", "Washington", "Lake"}
	streetIndex := (iteration - 1) % len(streetNames)

	cityNames := []string{"New York", "Los Angeles", "Chicago", "Houston", "Phoenix", "Philadelphia", "San Antonio", "San Diego"}
	cityIndex := (iteration - 1) % len(cityNames)

	return Trader{
		ID:   iteration * 100 % 10000,
		Name: traderNames[traderIndex],
		Address: Address{
			Street:  fmt.Sprintf("%d %s St", (iteration*111)%999, streetNames[streetIndex]),
			City:    cityNames[cityIndex],
			Country: "USA",
			Zip:     fmt.Sprintf("%05d", (iteration*1234)%99999),
		},
		Email: fmt.Sprintf("trader%d@example.com", iteration),
	}
}

// createItems creates deterministic OrderItems
func createItems(iteration int) []OrderItem {
	// Always create 2 items for deterministic testing
	count := 2
	items := make([]OrderItem, count)
	for i := 0; i < count; i++ {
		product := createProduct(iteration*10 + i + 1)
		items[i] = OrderItem{
			Product:  product,
			Quantity: (i + 1) * 2,
			Total:    product.Price * float64((i+1)*2),
		}
	}
	return items
}

// createTags creates deterministic Tags map
func createTags(iteration int) map[string]Tag {
	// Always create 2 tags for deterministic testing
	count := 2
	tags := make(map[string]Tag, count)
	for i := 0; i < count; i++ {
		key := fmt.Sprintf("tag%d", i)
		tags[key] = Tag{
			Key:   key,
			Value: fmt.Sprintf("value%d", i),
		}
	}
	return tags
}

// logOrder prints order as pretty-printed JSON for debugging
func logOrder(order Order) {
	// Print human-readable summary
	fmt.Fprintf(os.Stderr, "Order #%d: %s x $%.2f [%s] items=%d\n",
		order.ID,
		order.Product.Name,
		order.Total,
		order.Status,
		len(order.Items))

	// Print pretty-printed JSON for detailed debugging
	if jsonData, err := json.MarshalIndent(order, "  ", "  "); err == nil {
		fmt.Fprintf(os.Stderr, "  JSON: %s\n", string(jsonData))
	}
}

func log(args ...interface{}) {
	fmt.Fprintln(os.Stderr, args...)
}
