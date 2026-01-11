# Ralph - Autonomous Agent Loop Example

An autonomous agent loop that runs continuously until all PRD items are complete. No bash scripts needed — everything runs within ADK-Rust.

## Overview

Ralph demonstrates ADK-Rust's native loop capabilities for building autonomous development agents. It uses a PRD-driven approach where the agent reads tasks from a JSON file, executes them using custom tools, and tracks progress until completion.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Ralph                                 │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────┐   │
│  │                   LoopAgent                          │   │
│  │  (Wraps the orchestrator for continuous execution)  │   │
│  └─────────────────────────────────────────────────────┘   │
│                           │                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Loop Agent (Orchestrator)               │   │
│  │  - Checks PRD stats                                  │   │
│  │  - Gets next task                                    │   │
│  │  - Marks tasks complete                              │   │
│  │  - Signals exit when done                            │   │
│  └─────────────────────────────────────────────────────┘   │
│                           │                                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                   Custom Tools                       │   │
│  ├─────────────┬─────────────┬─────────────┬───────────┤   │
│  │  PrdTool    │  GitTool    │  TestTool   │ FileTool  │   │
│  │  - get_next │  - add      │  - check    │ - read    │   │
│  │  - complete │  - commit   │  - test     │ - write   │   │
│  │  - stats    │  - status   │  - clippy   │ - append  │   │
│  │  - learning │  - diff     │  - fmt      │ - list    │   │
│  └─────────────┴─────────────┴─────────────┴───────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## Features

| Feature | Description |
|---------|-------------|
| 🔄 **Autonomous Loop** | Continuous execution using ADK-Rust's native `LoopAgent` |
| 📋 **PRD-Driven** | JSON-based task management with priorities and acceptance criteria |
| 🛠️ **Custom Tools** | Native ADK-Rust tools for Git, files, quality checks, and PRD management |
| ✅ **Quality Gates** | Automated `cargo check`, `test`, `clippy`, and `fmt` verification |
| 💾 **State Persistence** | PRD updates and progress logs for auditability |
| 🤖 **Multi-Agent Ready** | Worker agent builder included for future delegation patterns |

## Quick Start

### Prerequisites

- Rust 1.85+
- Google API key (Gemini)

### Setup

1. Set your API key:
```bash
export GOOGLE_API_KEY=your-api-key-here
```

2. Run Ralph:
```bash
cargo run -p ralph
```

### Using a `.env` File

Create a `.env` file in the `examples/ralph/` directory:
```env
GOOGLE_API_KEY=your-api-key-here
RALPH_PRD_PATH=prd.json
RALPH_MAX_ITERATIONS=100
RALPH_MODEL=gemini-2.5-flash
```

## Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `GOOGLE_API_KEY` | (required) | Gemini API key |
| `RALPH_PRD_PATH` | `prd.json` | Path to the PRD file |
| `RALPH_PROGRESS_PATH` | `progress.txt` | Path to learnings log |
| `RALPH_MAX_ITERATIONS` | `100` | Maximum loop iterations |
| `RALPH_MODEL` | `gemini-2.5-flash` | Model to use |

## Project Structure

```
examples/ralph/
├── Cargo.toml              # Package dependencies
├── prd.json                # Example PRD with user stories
├── progress.txt            # Learnings log (created at runtime)
└── src/
    ├── main.rs             # Entry point
    ├── agents/
    │   ├── mod.rs
    │   ├── loop_agent.rs   # Main orchestrator (LlmAgent)
    │   └── worker_agent.rs # Task executor (for extension)
    ├── tools/
    │   ├── mod.rs
    │   ├── prd_tool.rs     # PRD management
    │   ├── git_tool.rs     # Git operations
    │   ├── test_tool.rs    # Quality checks
    │   └── file_tool.rs    # File operations
    └── models/
        ├── mod.rs
        ├── prd.rs          # PRD data structures
        └── config.rs       # Configuration
```

## PRD Format

The PRD file (`prd.json`) uses the following schema:

```json
{
  "project": "Project Name",
  "branchName": "feature/branch-name",
  "description": "Project description",
  "userStories": [
    {
      "id": "US-001",
      "title": "Task title",
      "description": "What the task accomplishes",
      "acceptanceCriteria": [
        "Criterion 1",
        "Criterion 2"
      ],
      "priority": 1,
      "passes": false,
      "notes": ""
    }
  ]
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `project` | string | Project name |
| `branchName` | string | Git branch for this work |
| `description` | string | Overall description |
| `userStories` | array | List of tasks to complete |
| `userStories[].id` | string | Unique task identifier |
| `userStories[].priority` | number | Lower = higher priority |
| `userStories[].passes` | boolean | Whether task is complete |

## Custom Tools

### PrdTool

Manages PRD tasks:

| Action | Parameters | Description |
|--------|------------|-------------|
| `get_stats` | - | Returns completion stats |
| `get_next_task` | - | Returns highest priority incomplete task |
| `mark_complete` | `task_id` | Marks a task as done |
| `add_learning` | `learning` | Appends to progress log |

### GitTool

Git operations:

| Command | Parameters | Description |
|---------|------------|-------------|
| `add` | `files` (optional) | Stage files |
| `commit` | `message` | Create commit |
| `status` | - | Get repo status |
| `diff` | - | Get staged diff |
| `checkout_branch` | `branch` | Switch/create branch |

### TestTool

Quality checks:

| Check Type | Description |
|------------|-------------|
| `check` | Run `cargo check` |
| `test` | Run `cargo test` |
| `clippy` | Run `cargo clippy` |
| `fmt` | Check formatting |
| `all` | Run check, test, and clippy |

### FileTool

File operations:

| Operation | Parameters | Description |
|-----------|------------|-------------|
| `read` | `path` | Read file contents |
| `write` | `path`, `content` | Write to file |
| `append` | `path`, `content` | Append to file |
| `list` | `path` | List directory contents |

## How It Works

1. **Startup**: Ralph loads the PRD and initializes tools
2. **Loop Iteration**: 
   - Orchestrator checks `prd_manager.get_stats`
   - If all complete, calls `exit_loop`
   - Otherwise, gets next task and processes it
3. **Task Processing**: Mark complete and continue
4. **Completion**: Loop exits when all tasks pass

## Example Session

```
🤖 Ralph Starting...
Project: My Rust Project
Description: Add user management functionality

⚙️ Max iterations: 100
📋 Tasks: 0/3 complete

ADK Console Mode
Agent: ralph
Type your message and press Enter. Ctrl+C to exit.

> Start implementing the PRD tasks
[tool-call] prd_manager {"action":"get_stats"}
[tool-response] {"complete":0,"total":3,"is_complete":false}
[tool-call] prd_manager {"action":"get_next_task"}
[tool-response] {"task":{"id":"US-001","title":"Create User struct",...}}
...
```

## Extending Ralph

### Adding New Tools

1. Create a new tool in `src/tools/`:
```rust
use adk_core::{AdkError, Result, Tool, ToolContext};

pub struct MyTool { /* ... */ }

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Description" }
    
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, params: Value) -> Result<Value> {
        // Implementation
    }
}
```

2. Add to `src/tools/mod.rs`
3. Add to the tools vector in `main.rs`

### Using Worker Agents

The `WorkerAgentBuilder` is provided for multi-agent patterns:

```rust
use crate::agents::WorkerAgentBuilder;

let worker = WorkerAgentBuilder::new(&api_key, &model_name)
    .with_tools(tools)
    .build(&task_context)?;
```

## Native vs Bash

| Aspect | Bash | Native (Ralph) |
|--------|------|----------------|
| Implementation | Shell script + ADK | Pure Rust |
| Type Safety | Runtime | Compile-time |
| Error Handling | Exit codes | `Result<T, E>` |
| Concurrency | Sequential | Async/await |
| Tool Integration | Shell commands | Native ADK tools |
| Debugging | Log files | Tracing + structured logging |

## License

Apache-2.0 - See the main [LICENSE](../../LICENSE) file.
