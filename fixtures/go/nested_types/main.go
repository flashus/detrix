// Complex Types Fixture for eBPF Integration Testing
// Tests nested structs, slices, maps, pointers, interfaces with depth-limited capture
//
// Build: go build -gcflags="all=-N -l" -o detrix_complex_types detrix_complex_types.go
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

package main

import (
	"fmt"
	"math/rand"
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

	rand.Seed(time.Now().UnixNano())

	log("Complex types fixture started")
	log("Testing: nested structs, slices, maps, pointers, interfaces")
	log("")

	iteration := 0

	for running {
		iteration++

		// Create nested order structure
		order := createRandomOrder(iteration)

		// Test pointer capture
		globalOrder = &order

		// OFFSET_ORDER - main capture point (all fields in scope)
		logOrder(order) // OFFSET_ORDER

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

func createRandomOrder(iteration int) Order {
	statuses := []OrderStatus{StatusPending, StatusConfirmed, StatusShipped, StatusDelivered}

	return Order{
		ID:        iteration,
		Product:   createRandomProduct(),
		Trader:    createRandomTrader(),
		Items:     createRandomItems(rand.Intn(5) + 1),
		Tags:      createRandomTags(rand.Intn(3) + 1),
		Total:     rand.Float64()*1000 + 100,
		Timestamp: time.Now(),
		Status:    statuses[rand.Intn(len(statuses))],
	}
}

func createRandomProduct() Product {
	return Product{
		SKU:   fmt.Sprintf("SKU-%d", rand.Intn(10000)),
		Name:  randomProductName(),
		Price: rand.Float64()*500 + 50,
		Category: Category{
			ID:   rand.Intn(100),
			Name: randomCategoryName(),
			Metadata: Metadata{
				CreatedAt: time.Now().AddDate(0, -rand.Intn(12), 0),
				Tags:      []string{"tag1", "tag2", "tag3"},
				Extra:     map[string]string{"source": "api", "version": "1.0"},
			},
		},
	}
}

func createRandomTrader() Trader {
	return Trader{
		ID:   rand.Intn(10000),
		Name: randomTraderName(),
		Address: Address{
			Street:  fmt.Sprintf("%d %s St", rand.Intn(999), randomStreetName()),
			City:    randomCityName(),
			Country: "USA",
			Zip:     fmt.Sprintf("%05d", rand.Intn(99999)),
		},
		Email: fmt.Sprintf("trader%d@example.com", rand.Intn(10000)),
	}
}

func createRandomItems(count int) []OrderItem {
	items := make([]OrderItem, count)
	for i := range items {
		product := createRandomProduct()
		items[i] = OrderItem{
			Product:  product,
			Quantity: rand.Intn(10) + 1,
			Total:    product.Price * float64(rand.Intn(10)+1),
		}
	}
	return items
}

func createRandomTags(count int) map[string]Tag {
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

func logOrder(order Order) {
	fmt.Fprintf(os.Stderr, "Order #%d: %s x $%.2f [%s] items=%d\n",
		order.ID,
		order.Product.Name,
		order.Total,
		order.Status,
		len(order.Items))
}

func randomProductName() string {
	products := []string{"Laptop", "Phone", "Tablet", "Monitor", "Keyboard", "Mouse", "Headphones", "Camera"}
	return products[rand.Intn(len(products))]
}

func randomCategoryName() string {
	categories := []string{"Electronics", "Accessories", "Computing", "Audio", "Photography"}
	return categories[rand.Intn(len(categories))]
}

func randomTraderName() string {
	names := []string{"Alice", "Bob", "Charlie", "Diana", "Eve", "Frank", "Grace", "Henry"}
	return names[rand.Intn(len(names))]
}

func randomStreetName() string {
	streets := []string{"Main", "Oak", "Maple", "Cedar", "Pine", "Elm", "Washington", "Lake"}
	return streets[rand.Intn(len(streets))]
}

func randomCityName() string {
	cities := []string{"New York", "Los Angeles", "Chicago", "Houston", "Phoenix", "Philadelphia", "San Antonio", "San Diego"}
	return cities[rand.Intn(len(cities))]
}

func log(args ...interface{}) {
	fmt.Fprintln(os.Stderr, args...)
}
