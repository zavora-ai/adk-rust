# Calculator CLI App

## Project Overview

The Calculator CLI App is a simple command-line interface application designed to perform basic arithmetic operations. It will allow users to quickly perform addition, subtraction, multiplication, and division directly from their terminal. The primary goal is to provide a robust, easy-to-use, and reliable tool for everyday calculations without the need for a graphical user interface. Target users include developers, system administrators, and anyone who prefers command-line tools for quick computations.

## User Stories

### US-001: Perform Addition

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to add two or more numbers, so that I can get their sum.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc add <number1> <number2> ...', THE system SHALL output the sum of the numbers.
- [ ] 2. IF the user inputs non-numeric values for addition, THEN THE system SHALL display an error message: "Invalid input. Please provide numbers only."

### US-002: Perform Subtraction

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to subtract a sequence of numbers from an initial number, so that I can get their difference.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc subtract <number1> <number2> ...', THE system SHALL output the result of subtracting subsequent numbers from the first.
- [ ] 2. IF the user inputs non-numeric values for subtraction, THEN THE system SHALL display an error message: "Invalid input. Please provide numbers only."

### US-003: Perform Multiplication

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to multiply two or more numbers, so that I can get their product.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc multiply <number1> <number2> ...', THE system SHALL output the product of the numbers.
- [ ] 2. IF the user inputs non-numeric values for multiplication, THEN THE system SHALL display an error message: "Invalid input. Please provide numbers only."

### US-004: Perform Division

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to divide one number by another, so that I can get their quotient.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc divide <dividend> <divisor>', THE system SHALL output the quotient.
- [ ] 2. IF the user inputs a divisor of zero, THEN THE system SHALL display an error message: "Error: Cannot divide by zero."
- [ ] 3. IF the user inputs non-numeric values for division, THEN THE system SHALL display an error message: "Invalid input. Please provide numbers only."

### US-005: Display Help Information

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to view a help message, so that I can understand how to use the calculator and its available commands.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc --help' or 'calc -h', THE system SHALL display a comprehensive help message listing all supported operations (add, subtract, multiply, divide) and their correct syntax.

### US-006: Display Version Information

**Priority**: 2
**Status**: pending

**User Story**: As a user, I want to see the application's version, so that I know which version I am currently using.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs 'calc --version' or 'calc -v', THE system SHALL display the current version number of the calculator application.

### US-007: Handle Invalid Command Input

**Priority**: 1
**Status**: pending

**User Story**: As a user, I want to be informed if I enter an unrecognized command, so that I can correct my input or seek help.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs an unknown command (e.g., 'calc unknown 5 3'), THE system SHALL display an error message indicating the command is unrecognized and suggest using the help option: "Unknown command. Use 'calc --help' for usage information."

### US-008: Handle Insufficient Arguments

**Priority**: 2
**Status**: pending

**User Story**: As a user, I want to be informed if I provide too few arguments for an operation, so that I can correct my input.

#### Acceptance Criteria

- [ ] 1. WHEN the user inputs an operation without sufficient numbers (e.g., 'calc add 5'), THE system SHALL display an error message indicating insufficient arguments and suggest using the help option: "Insufficient arguments for command 'add'. Use 'calc --help' for usage."

