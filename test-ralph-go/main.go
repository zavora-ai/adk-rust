package main

import (
	"fmt"
	"os"
)

const version = "1.0.0"
const usage = "Usage: hello-world-cli [--version | --help]"

var exit = os.Exit

func main() {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "--version":
			fmt.Println(version)
			exit(0)
		case "--help":
			fmt.Println(usage)
			exit(0)
		default:
			fmt.Fprintln(os.Stderr, "Error: Unknown argument '" + os.Args[1] + "'")
			fmt.Println(usage)
			exit(1)
		}
	}
	fmt.Println("Hello, World!")
}