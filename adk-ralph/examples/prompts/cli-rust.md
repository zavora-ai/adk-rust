# CLI Task Manager - Rust

Create a CLI task manager in Rust called 'taskr'.

## Purpose

A command-line tool for managing personal tasks with local storage, supporting priorities, due dates, and filtering.

## Features

- **Task Management**
  - Add tasks with title, description, priority (high/medium/low), and optional due date
  - List tasks with filtering by status (pending/completed) and priority
  - Mark tasks as complete
  - Delete tasks by ID
  - Edit existing tasks

- **Organization**
  - Tag tasks with custom labels
  - Group tasks by project
  - Sort by priority, due date, or creation date

- **User Experience**
  - Colored terminal output for priorities and status
  - Interactive mode for bulk operations
  - Export tasks to JSON or CSV

## Technical Requirements

- Use `clap` for argument parsing with derive macros
- Use `serde` for JSON serialization
- Use `colored` for terminal colors
- Store tasks in `~/.taskr/tasks.json`
- Support XDG base directory specification

## Testing

- Unit tests for task operations
- Integration tests for CLI commands
- Property tests for serialization round-trips
