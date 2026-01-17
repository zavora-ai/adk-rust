# System Design: Calculator CLI App

## Architecture Overview

The Calculator CLI App is a command-line interface application designed to perform basic arithmetic operations such as addition, subtraction, multiplication, and division. It aims to provide a robust, easy-to-use, and reliable tool for quick computations directly from the terminal. The application will leverage Rust's performance and strong typing, along with the 'clap' crate for efficient command-line argument parsing and user-friendly interaction.

## Component Diagram

```mermaid
graph TD
    A[User Input] --> B{CLI Entry Point (main.rs)};
    B --> C[Argument Parser (cli.rs using clap)];
    C -- Valid Command & Args --> D[Calculator Logic (calculator.rs)];
    C -- Invalid Command/Args / Help / Version --> E[Error Handler (error.rs) / Output Handler];
    D -- Result --> F[Output to Terminal];
    E -- Error Message / Info --> F;
```

## Components

### CLI Entry Point

**Purpose**: The main entry point of the application, responsible for initializing the CLI parser, dispatching commands, and handling top-level errors.

**Interface**:
- main

**Dependencies**: Argument Parser, Calculator Logic, Error Handler

**File**: `src/main.rs`

### Argument Parser

**Purpose**: Handles the parsing of command-line arguments, including subcommands (add, subtract, multiply, divide) and flags (--help, --version), validating input types and argument counts.

**Interface**:
- parse_args
- CliArgs
- Operation

**File**: `src/cli.rs`

### Calculator Logic

**Purpose**: Contains the core business logic for performing arithmetic operations, ensuring correct mathematical calculations and handling specific arithmetic errors like division by zero.

**Interface**:
- add
- subtract
- multiply
- divide

**Dependencies**: Error Handler

**File**: `src/calculator.rs`

### Error Handler

**Purpose**: Defines custom error types for the application, providing clear and user-friendly error messages for various scenarios such as invalid input, insufficient arguments, or division by zero.

**Interface**:
- AppError
- Error
- Display

**File**: `src/error.rs`

## File Structure

```
Calculator CLI App/
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── calculator.rs
│   └── error.rs
└── Cargo.toml
```

## Technology Stack

- **Language**: Rust
- **Testing**: cargo test
- **Build Tool**: Cargo
- **Dependencies**: clap

## Design Decisions

- Use 'clap' crate for CLI argument parsing.: 'clap' is a widely-used, robust, and well-maintained Rust library for command-line argument parsing. It simplifies defining subcommands, flags, and argument validation, ensuring a consistent and user-friendly CLI experience.
- Separate CLI parsing from core arithmetic logic.: Decoupling the argument parsing (src/cli.rs) from the mathematical operations (src/calculator.rs) improves modularity, testability, and maintainability. The core logic can be tested independently of the CLI interface.
- Implement custom error types using 'thiserror' or similar pattern.: Custom error types (src/error.rs) allow for precise error handling and provide clearer, more actionable error messages to the user. This enhances the application's robustness and user experience, directly addressing PRD acceptance criteria for error reporting.
- Use Rust as the primary programming language.: Rust provides strong performance, memory safety, and a robust type system, which are beneficial for creating reliable command-line tools. Its powerful ecosystem, including Cargo and comprehensive testing facilities, supports efficient development.

