// Test fixture: Transitive impure function call chain
//
// Call chain: EntryPoint -> MiddleLayer -> ReadFile
// The impure os.ReadFile call is 2 levels deep.
package main

import (
	"os"
	"strings"
)

// ReadFile is an impure function - reads from file system.
func ReadFile(path string) (string, error) {
	content, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	return string(content), nil
}

// MiddleLayer is a middle function - calls impure ReadFile.
func MiddleLayer(path string) (string, error) {
	content, err := ReadFile(path)
	if err != nil {
		return "", err
	}
	return strings.ToUpper(content), nil
}

// EntryPoint is the entry point - transitively calls impure ReadFile.
func EntryPoint(path string) (string, error) {
	return MiddleLayer(path)
}
