# Sandbox Workspace Agent Example

Demonstrates the full sandbox-agent-harness lifecycle — Manifest definition, workspace provisioning, session management, tool binding, LLM agent execution, and optional snapshot/resume. The agent creates a Rust hello-world project, compiles it, and runs it inside the sandbox workspace.

## What It Demonstrates

- **Manifest definition** — declarative workspace structure with directory entries
- **SandboxConfig construction** — capabilities, timeouts, and snapshot settings
- **LocalUnixClient** — default backend using local filesystem and child processes
- **DockerClient** — optional Docker-based isolation via `--docker` flag
- **Tool binding** — `SandboxRunner` injects `exec_command`, `write_file`, and `list_dir` only
  while the sandbox session is live
- **LLM agent loop** — Gemini-powered agent that creates, compiles, and runs a Rust project
- **Snapshot/resume** — optional workspace persistence and verification via `--snapshot` flag
- **Lifecycle observability** — structured output with section banners and phase tracking

## Prerequisites

| Requirement | Purpose | Required? |
|-------------|---------|-----------|
| **Rust toolchain** (`cargo`, `rustc`) | Must be available in the sandbox environment for the agent task to compile and run the hello-world project | Yes |
| **GOOGLE_API_KEY** | Gemini LLM API access | Yes |
| **Docker** | Container-based sandbox isolation | Only with `--docker` flag |

### Rust Toolchain

The agent task creates a Rust project inside the sandbox workspace and runs `cargo build`. When using `LocalUnixClient` (the default), the host system's Rust toolchain is used directly. Ensure `cargo` and `rustc` are installed and on your PATH:

```bash
rustc --version
cargo --version
```

If not installed, see [rustup.rs](https://rustup.rs/).

### Docker (optional)

Docker is required only when using the `--docker` flag. The `DockerClient` provisions the workspace inside a Docker container using the `rust:latest` base image (which includes the Rust toolchain).

```bash
# Verify Docker is available
docker info
```

If Docker is not installed or the daemon is not running, the example will print an actionable error and exit. Install Docker from [docs.docker.com/get-docker](https://docs.docker.com/get-docker/).

## Setup

```bash
# Copy the environment template
cp examples/sandbox_workspace_agent/.env.example examples/sandbox_workspace_agent/.env

# Edit .env and set your API key
# GOOGLE_API_KEY=your-key-here
```

Or export directly:

```bash
export GOOGLE_API_KEY=your-key-here
```

## Usage

### Default (LocalUnixClient)

Provisions the workspace as a local temporary directory and executes commands via child processes:

```bash
GOOGLE_API_KEY=... cargo run --manifest-path examples/sandbox_workspace_agent/Cargo.toml
```

### Docker backend

Provisions the workspace inside a Docker container with the `rust:latest` image:

```bash
GOOGLE_API_KEY=... cargo run --manifest-path examples/sandbox_workspace_agent/Cargo.toml -- --docker
```

### With snapshot/resume

Enables workspace snapshotting after the agent loop completes, then resumes from the snapshot to verify contents are preserved:

```bash
GOOGLE_API_KEY=... cargo run --manifest-path examples/sandbox_workspace_agent/Cargo.toml -- --snapshot
```

### Both flags

```bash
GOOGLE_API_KEY=... cargo run --manifest-path examples/sandbox_workspace_agent/Cargo.toml -- --docker --snapshot
```

### Debug logging

Control log verbosity with `RUST_LOG`:

```bash
RUST_LOG=debug GOOGLE_API_KEY=... cargo run --manifest-path examples/sandbox_workspace_agent/Cargo.toml
```

## CLI Flags

| Flag | Description | Default |
|------|-------------|---------|
| `--docker` | Use `DockerClient` instead of `LocalUnixClient` for sandbox isolation. Requires Docker to be installed and running. | Off (uses `LocalUnixClient`) |
| `--snapshot` | Enable snapshot/resume demonstration after the agent loop completes. Snapshots the workspace, resumes from it, and verifies contents are preserved. | Off |

## Expected Output

The example prints structured output with section banners for each lifecycle phase:

```
════════════════════════════════════════════════════════════
  Sandbox Workspace Agent Example
════════════════════════════════════════════════════════════
  Backend: LocalUnixClient
  Snapshot: disabled

════════════════════════════════════════════════════════════
  Phase 1: Manifest Definition
════════════════════════════════════════════════════════════
  Manifest entries:
    📁 hello-world/
    📁 hello-world/src/

════════════════════════════════════════════════════════════
  Phase 2: SandboxConfig Construction
════════════════════════════════════════════════════════════
  Capabilities: Shell, Filesystem
  Session timeout: 120s
  Command timeout: 60s
  Snapshot on stop: false

════════════════════════════════════════════════════════════
  Phase 3: Agent and Runner Construction
════════════════════════════════════════════════════════════
  Building LlmAgent with Gemini model...
  ✅ Agent built: sandbox-workspace-agent
  ✅ Runner and SandboxRunner constructed

════════════════════════════════════════════════════════════
  Phase 4: Managed Sandbox Execution
════════════════════════════════════════════════════════════
  Provisioning, running, snapshotting, and cleaning up...

  🔧 Tool call: list_dir
  🔧 Tool call: write_file
  🔧 Tool call: write_file
  🔧 Tool call: exec_command
     Args: { "command": "cargo build", "cwd": "hello-world" }
  ✅ Exit code: 0
  🔧 Tool call: exec_command
     Args: { "command": "./target/debug/hello-world", "cwd": "hello-world" }
  ✅ Exit code: 0
  ┌─── stdout ─────────────────────────────────
  │ Hello, world!
  └─────────────────────────────────────────────

════════════════════════════════════════════════════════════
  Phase 5: Results
════════════════════════════════════════════════════════════
  ✅ Agent execution completed successfully
  ✅ Sandbox session stopped
  Snapshot disabled

════════════════════════════════════════════════════════════
  Summary
════════════════════════════════════════════════════════════
  ✓ Manifest definition
  ✓ SandboxConfig construction
  ✓ Runner construction
  ✓ Managed sandbox execution
  ✓ Stop/cleanup
  ✓ Snapshot
```

When `--snapshot` is enabled, an additional phase appears:

```
════════════════════════════════════════════════════════════
  Phase 6: Snapshot/Resume Verification
════════════════════════════════════════════════════════════
  📸 Snapshot ID: snap_abc123
  Resuming from snapshot: snap_abc123
  ✅ Resumed session: session_xyz
  Workspace contents after resume:
    Dir hello-world
    ...
```

## Configuration

| Environment Variable | Description | Required |
|---------------------|-------------|----------|
| `GOOGLE_API_KEY` | Google AI API key for Gemini model access | Yes |
| `RUST_LOG` | Log level filter (e.g., `info`, `debug`, `warn`) | No (default: `info`) |

## Architecture

```
┌─────────────────────────────────────────────┐
│  Gemini LLM (gemini-2.5-flash)              │
│  "Create a Rust hello-world project"        │
└──────────────┬──────────────────────────────┘
               │ tool calls
               ▼
┌─────────────────────────────────────────────┐
│  SandboxRunner                              │
│  Lifecycle: provision → start → bind →      │
│             agent loop → snapshot → stop    │
└──────────────┬──────────────────────────────┘
               │
       ┌───────┴───────┐
       ▼               ▼
┌─────────────┐  ┌─────────────┐
│ LocalUnix   │  │ Docker      │
│ Client      │  │ Client      │
│ (default)   │  │ (--docker)  │
└─────────────┘  └─────────────┘
```

## Files

| File | Description |
|------|-------------|
| `Cargo.toml` | Standalone crate with sandbox, runner, agent, and model dependencies |
| `src/main.rs` | Entry point, CLI parsing, lifecycle orchestration |
| `src/config.rs` | Manifest and SandboxConfig construction |
| `src/display.rs` | Event formatting, banners, and summary output |
| `.env.example` | Template for required environment variables |
| `README.md` | This file |
