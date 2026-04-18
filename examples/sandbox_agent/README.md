# Sandbox Agent Example

An LLM-agent-driven example that demonstrates the OS sandbox profiles feature with an actual Gemini-powered agent that executes Python code in a sandboxed environment. Works on macOS, Linux, and Windows.

## What It Demonstrates

- **SandboxPolicy builder** — declarative, platform-agnostic policy defining allowed filesystem paths, network, and process permissions
- **Platform enforcer** — automatic selection of the OS-native sandbox (Seatbelt on macOS, bubblewrap on Linux, AppContainer on Windows)
- **Sandboxed ProcessBackend** — code execution with kernel-level restrictions
- **LLM agent with sandboxed tool** — Gemini generates Python code, executes it through the sandbox
- **Sandbox enforcement** — successful code execution within allowed paths, blocked network access
- **Graceful fallback** — runs unsandboxed with a warning if no enforcer is available

## Platform Support

| Platform | Enforcer | Install |
|----------|----------|---------|
| macOS | Seatbelt (`sandbox-exec`) | Built-in |
| Linux | bubblewrap (`bwrap`) | `apt install bubblewrap` or `dnf install bubblewrap` |
| Windows | AppContainer | Built-in (Windows 8+) |
| Other | None (fallback) | Runs unsandboxed with warning |

## Sandbox Policy

| Permission | Setting |
|---|---|
| Read access | System paths (platform-specific: `/usr`, `/lib`, `C:\Windows`, etc.) |
| Read-write access | Temporary work directory (`$TMPDIR/adk_sandbox_agent_work`) |
| Network access | **Denied** |
| Process spawning | **Allowed** (Python interpreter needs it) |
| Environment | `PATH` set to platform-appropriate interpreter paths |

## Prerequisites

- **GOOGLE_API_KEY** — set in `.env` or as an environment variable
- **Python 3** — must be installed and on PATH
- **Platform tools** — see Platform Support table above

## Run

```bash
# From the workspace root
cargo run --manifest-path examples/sandbox_agent/Cargo.toml

# Or with debug logging
RUST_LOG=debug cargo run --manifest-path examples/sandbox_agent/Cargo.toml
```

## Expected Output

The example runs two prompts through the agent:

### Prompt 1: Fibonacci Table (Success)

The agent writes and executes a Python script that calculates the first 20 Fibonacci numbers and prints them as a formatted table. The sandbox allows this because:
- Python interpreter is in an allowed read path (`/usr/bin`)
- No network access is needed
- Output goes to stdout (captured by the backend)

### Prompt 2: Network Fetch (Blocked)

The agent attempts to fetch data from `https://example.com` using Python's `urllib`. The sandbox blocks this because:
- Network access is denied in the policy
- Seatbelt enforces `(deny default)` with no `(allow network*)` directive
- The Python script fails with a network error, which the agent reports back

## Architecture

```
┌─────────────────────────────────────────────┐
│  Gemini LLM (gemini-3.1-flash-lite-preview)       │
│  "Write Python code to answer questions"    │
└──────────────┬──────────────────────────────┘
               │ generate code
               ▼
┌─────────────────────────────────────────────┐
│  SandboxedCodeTool                          │
│  Accepts: { language: "python", code: "…" } │
│  Returns: { stdout, stderr, exit_code }     │
└──────────────┬──────────────────────────────┘
               │ ExecRequest
               ▼
┌─────────────────────────────────────────────┐
│  ProcessBackend (with sandbox)              │
│  ┌────────────────────────────────────────┐ │
│  │ SandboxEnforcer (Seatbelt on macOS)    │ │
│  │ wrap_command() → sandbox-exec -p …     │ │
│  └────────────────────────────────────────┘ │
│  Spawns: sandbox-exec -p <profile> python3  │
└─────────────────────────────────────────────┘
```

## Files

| File | Description |
|---|---|
| `Cargo.toml` | Standalone crate with adk-sandbox, adk-agent, adk-runner dependencies |
| `src/main.rs` | Full example: policy, enforcer, backend, tool, agent, runner |
| `.env.example` | Template for required environment variables |
| `README.md` | This file |
