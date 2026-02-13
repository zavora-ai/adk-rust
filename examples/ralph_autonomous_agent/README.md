# Ralph Autonomous Agent

This example demonstrates a fully native ADK-Rust autonomous agent that continuously executes development tasks from a Product Requirements Document (PRD).

## Architecture: Loop vs. Worker Pattern

This implementation uses a **Loop-Worker** architectural pattern, which separates orchestration from execution:

1.  **Loop Agent (Orchestrator)**:
    *   Maintains the high-level state of the project.
    *   Reads and updates the PRD.
    *   Decides which task to tackle next based on dependencies and priority.
    *   Delegates specific tasks to Worker Agents.

2.  **Worker Agent (Executor)**:
    *   Receives a scoped task from the Loop Agent.
    *   Executes the task (coding, file manipulation).
    *   Runs verification steps (compile, test, lint).
    *   Reports success or failure back to the Loop Agent.

## State Management

State is managed primarily through the **Product Requirements Document (PRD)**. The agent parses the PRD to understand:
*   Project goals
*   User stories and acceptance criteria
*   Current progress status

This approach ensures the agent remains aligned with the project requirements throughout the autonomous loop.

## Usage

Run the agent:

```bash
cargo run -p ralph-autonomous-agent
```

Configuration is handled via environment variables (see `config.rs` for details).
