# Sandboxed Code Execution

The `adk-sandbox` crate provides isolated code execution for ADK agents, with two levels of isolation:

1. **Process isolation** — child processes with environment isolation and timeout enforcement
2. **OS-level sandbox profiles** — kernel-level restrictions on filesystem, network, and process spawning

## Backends

| Backend | Isolation Level | Languages | Feature Flag |
|---------|----------------|-----------|--------------|
| `ProcessBackend` | Environment + timeout | Rust, Python, JS, TS, Command | `process` (default) |
| `ProcessBackend` + sandbox | Kernel-level | Same as above | `process` + `sandbox-native` |
| `WasmBackend` | Full (memory, fs, network) | WASM only | `wasm` |

## OS Sandbox Profiles

OS-level sandbox enforcement restricts child processes at the kernel level. This goes beyond environment isolation — the OS itself blocks unauthorized filesystem access, network connections, and process spawning.

### Platform Support

| Platform | Enforcer | How It Works |
|----------|----------|-------------|
| macOS | Seatbelt (`sandbox-exec`) | Syscall-level rules: "allow default, deny dangerous" |
| Linux | bubblewrap (`bwrap`) | Filesystem namespace isolation (whitelist mounts) |
| Windows | AppContainer | Token-based ACL restrictions |

### Quick Start

```rust
use adk_sandbox::{
    ProcessBackend, ProcessConfig, SandboxBackend,
    SandboxPolicyBuilder, get_enforcer,
};

// 1. Define what the sandboxed process can do
let policy = SandboxPolicyBuilder::new()
    .allow_read("/usr")           // Read system libraries
    .allow_read_write("/tmp/work") // Write to work directory
    .allow_process_spawn()         // Python needs to exec
    // Network is denied by default
    .env("PATH", "/usr/bin:/usr/local/bin")
    .build();

// 2. Get the platform-appropriate enforcer
let enforcer = get_enforcer()?;

// 3. Create a sandboxed backend
let backend = ProcessBackend::with_sandbox(
    ProcessConfig::default(),
    enforcer,
    policy,
);

// 4. Execute code — network is blocked, writes restricted
let result = backend.execute(request).await?;
```

### Feature Flags

```toml
[dependencies]
# Auto-detect platform enforcer
adk-sandbox = { version = "0.7.0", features = ["process", "sandbox-native"] }

# Or pick a specific platform
adk-sandbox = { version = "0.7.0", features = ["process", "sandbox-macos"] }
adk-sandbox = { version = "0.7.0", features = ["process", "sandbox-linux"] }
```

### SandboxPolicy

The policy defines what a sandboxed process is allowed to do:

| Field | Default | Description |
|-------|---------|-------------|
| `allowed_paths` | `[]` (deny all) | Filesystem paths with read-only or read-write access |
| `allow_network` | `false` | Whether network access is permitted |
| `allow_process_spawn` | `false` | Whether child process spawning is permitted |
| `env` | `{}` | Environment variables for the sandboxed process |

### Platform Differences

**macOS (Seatbelt):** Uses "allow default, deny dangerous" — starts with full access, then blocks network, file writes, and process spawning. A pure whitelist approach doesn't work because Python needs dozens of macOS-specific syscall categories at startup.

**Linux (bubblewrap):** Uses namespace-based whitelist — nothing exists by default, you mount only what's needed. Install with `apt install bubblewrap` or `dnf install bubblewrap`.

**Windows (AppContainer):** Uses token-based ACLs — the process runs with a restricted SID that has no access by default, then you grant ACLs on specific paths.

### Example

See [`examples/sandbox_agent/`](https://github.com/zavora-ai/adk-rust/tree/main/examples/sandbox_agent) for a full LLM-agent-driven example that executes Python code in a sandboxed environment with network access blocked by the OS kernel.
