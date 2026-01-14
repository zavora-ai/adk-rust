package main

import (
	"io"
	"os"
	"testing"
)

func TestMainOutput(t *testing.T) {
	oldStdout := os.Stdout
	r, w, _ := os.Pipe()
	os.Stdout = w

	main()

	w.Close()
	out, _ := io.ReadAll(r)
	os.Stdout = oldStdout

	if string(out) != "Hello, World!\n" {
		t.Errorf("Expected 'Hello, World!\n', got '%s'", string(out))
	}
}