package main

import (
	"fmt"
	"time"
)

func main() {
	fmt.Println("agent-counter: starting")

	for {
		n := time.Now().UnixNano() % 1000
		fmt.Printf("agent-counter: n=%d\n", n)
		time.Sleep(500 * time.Millisecond)
	}
}
