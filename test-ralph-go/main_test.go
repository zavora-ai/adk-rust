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

func TestVersionFlag(t *testing.T) {
	oldArgs := os.Args
	oldStdout := os.Stdout
	oldExit := exit

	os.Args = []string{"cmd", "--version"}
	r, w, _ := os.Pipe()
	os.Stdout = w

	exited := false
	exit = func(code int) {
		exited = true
		panic("exit") // Use panic to stop execution within the test
	}

	defer func() {
		os.Stdout = oldStdout
		os.Args = oldArgs
		exit = oldExit // Restore the original exit function
		if r := recover(); r != nil && r != "exit" {
			t.Fatalf("Unexpected panic: %v", r)
		}
	}()

	main()

	w.Close()
	out, _ := io.ReadAll(r)

	if !exited {
		t.Error("Expected to exit, but did not")
	}
	if string(out) != version+"\n" {
		t.Errorf("Expected '%s\n', got '%s'", version, string(out))
	}
}

func TestHelpFlag(t *testing.T) {
	oldArgs := os.Args
	oldStdout := os.Stdout
	oldExit := exit

	os.Args = []string{"cmd", "--help"}
	r, w, _ := os.Pipe()
	os.Stdout = w

	exited := false
	exit = func(code int) {
		exited = true
		panic("exit")
	}

	defer func() {
		os.Stdout = oldStdout
		os.Args = oldArgs
		exit = oldExit
		if r := recover(); r != nil && r != "exit" {
			t.Fatalf("Unexpected panic: %v", r)
		}
	}()

	main()

	w.Close()
	out, _ := io.ReadAll(r)

	if !exited {
		t.Error("Expected to exit, but did not")
	}
	if string(out) != "Usage: hello-world-cli [--version | --help]\n" {
		t.Errorf("Expected 'Usage: hello-world-cli [--version | --help]\n', got '%s'", string(out))
	}
}