package main

import (
	"io"
	"os"
	"testing"
)

func TestMainOutput(t *testing.T) {
	oldArgs := os.Args
	oldStdout := os.Stdout

	os.Args = []string{"cmd"} // Simulate no arguments
	r, w, _ := os.Pipe()
	os.Stdout = w

	main()

	w.Close()
	out, _ := io.ReadAll(r)
	os.Stdout = oldStdout
	os.Args = oldArgs

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

func TestInvalidArguments(t *testing.T) {
	oldArgs := os.Args
	oldStdout := os.Stdout
	oldStderr := os.Stderr
	oldExit := exit

	expectedStderr := "Error: Unknown argument '--unknown-arg'\n"
	expectedStdout := "Usage: hello-world-cli [--version | --help]\n"
	exitCode := 0

	os.Args = []string{"cmd", "--unknown-arg"}

	rout, wout, _ := os.Pipe()
	os.Stdout = wout
	err, werr, _ := os.Pipe()
	os.Stderr = werr

	exited := false
	exit = func(code int) {
		exited = true
		exitCode = code
		panic("exit")
	}

	defer func() {
		os.Stdout = oldStdout
		os.Stderr = oldStderr
		os.Args = oldArgs
		exit = oldExit
		if r := recover(); r != nil && r != "exit" {
			t.Fatalf("Unexpected panic: %v", r)
		}
	}()

	main()

	wout.Close()
	werr.Close()
	out, _ := io.ReadAll(rout)
	errOut, _ := io.ReadAll(err)

	if !exited {
		t.Error("Expected to exit, but did not")
	}

	if exitCode == 0 {
		t.Errorf("Expected non-zero exit code, got %d", exitCode)
	}

	if string(errOut) != expectedStderr {
		t.Errorf("Expected stderr '%s', got '%s'", expectedStderr, string(errOut))
	}

	if string(out) != expectedStdout {
		t.Errorf("Expected stdout '%s', got '%s'", expectedStdout, string(out))
	}
}
