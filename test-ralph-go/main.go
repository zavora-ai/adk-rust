package main

import (
	"fmt"
	"os"
)

const version = "1.0.0"

var exit = os.Exit

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version":
			fmt.Println(version)
			exit(0)
		case "--help":
			fmt.Println("Usage: hello-world-cli [--version | --help]")
			exit(0)
		}
	}
	fmt.Println("Hello, World!")
}