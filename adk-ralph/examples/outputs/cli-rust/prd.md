# Product Requirements Document

## Project Overview

**Project Name**: taskr
**Language**: Rust
**Type**: CLI Application

Taskr is a command-line task manager that helps users organize personal tasks with support for priorities, due dates, tags, and projects. Tasks are stored locally in JSON format following XDG base directory conventions.

## Glossary

- **Task**: A unit of work with title, description, priority, status, and optional metadata
- **Priority**: Task importance level (high, medium, low)
- **Status**: Task state (pending, completed)
- **Tag**: Custom label for categorizing tasks
- **Project**: Grouping mechanism for related tasks
- **Storage**: Local JSON file at `~/.taskr/tasks.json` or XDG-compliant location

## User Stories

### US-001: Add Tasks

**Priority**: 1
**Status**: pending

**Description**: As a user, I want to add new tasks with details, so that I can track work I need to complete.

**Acceptance Criteria**:
1. WHEN a user runs `taskr add "Task title"`, THE system SHALL create a new task with pending status
2. WHEN a user provides `--description`, THE system SHALL store the description with the task
3. WHEN a user provides `--priority high|medium|low`, THE system SHALL set the task priority
4. WHEN a user provides `--due YYYY-MM-DD`, THE system SHALL set the due date
5. WHEN a user provides `--tags tag1,tag2`, THE system SHALL associate tags with the task
6. WHEN a user provides `--project name`, THE system SHALL associate the task with a project
7. THE system SHALL generate a unique ID for each task
8. THE system SHALL display the created task with its ID

### US-002: List Tasks

**Priority**: 1
**Status**: pending

**Description**: As a user, I want to list my tasks with filtering options, so that I can see relevant tasks.

**Acceptance Criteria**:
1. WHEN a user runs `taskr list`, THE system SHALL display all pending tasks
2. WHEN a user provides `--all`, THE system SHALL display both pending and completed tasks
3. WHEN a user provides `--status pending|completed`, THE system SHALL filter by status
4. WHEN a user provides `--priority high|medium|low`, THE system SHALL filter by priority
5. WHEN a user provides `--project name`, THE system SHALL filter by project
6. WHEN a user provides `--tag name`, THE system SHALL filter by tag
7. WHEN a user provides `--sort priority|due|created`, THE system SHALL sort accordingly
8. THE system SHALL display tasks with colored output based on priority

### US-003: Complete Tasks

**Priority**: 1
**Status**: pending

**Description**: As a user, I want to mark tasks as complete, so that I can track my progress.

**Acceptance Criteria**:
1. WHEN a user runs `taskr complete <id>`, THE system SHALL mark the task as completed
2. WHEN the task ID does not exist, THE system SHALL display an error message
3. WHEN the task is already completed, THE system SHALL inform the user
4. THE system SHALL record the completion timestamp

### US-004: Delete Tasks

**Priority**: 2
**Status**: pending

**Description**: As a user, I want to delete tasks, so that I can remove items I no longer need.

**Acceptance Criteria**:
1. WHEN a user runs `taskr delete <id>`, THE system SHALL remove the task
2. WHEN the task ID does not exist, THE system SHALL display an error message
3. WHEN a user provides `--force`, THE system SHALL skip confirmation
4. WITHOUT `--force`, THE system SHALL prompt for confirmation

### US-005: Edit Tasks

**Priority**: 2
**Status**: pending

**Description**: As a user, I want to edit existing tasks, so that I can update details as needed.

**Acceptance Criteria**:
1. WHEN a user runs `taskr edit <id>`, THE system SHALL allow updating task fields
2. WHEN a user provides `--title "new title"`, THE system SHALL update the title
3. WHEN a user provides `--description "new desc"`, THE system SHALL update the description
4. WHEN a user provides `--priority new_priority`, THE system SHALL update the priority
5. WHEN a user provides `--due new_date`, THE system SHALL update the due date
6. WHEN the task ID does not exist, THE system SHALL display an error message

### US-006: Export Tasks

**Priority**: 3
**Status**: pending

**Description**: As a user, I want to export my tasks, so that I can use them in other tools.

**Acceptance Criteria**:
1. WHEN a user runs `taskr export --format json`, THE system SHALL output tasks as JSON
2. WHEN a user runs `taskr export --format csv`, THE system SHALL output tasks as CSV
3. WHEN a user provides `--output file.json`, THE system SHALL write to the specified file
4. WITHOUT `--output`, THE system SHALL write to stdout

### US-007: Data Persistence

**Priority**: 1
**Status**: pending

**Description**: As a user, I want my tasks to persist between sessions, so that I don't lose my data.

**Acceptance Criteria**:
1. THE system SHALL store tasks in `$XDG_DATA_HOME/taskr/tasks.json` if XDG is set
2. THE system SHALL fall back to `~/.taskr/tasks.json` if XDG is not set
3. THE system SHALL create the directory if it does not exist
4. THE system SHALL handle concurrent access safely
5. THE system SHALL create backups before destructive operations
