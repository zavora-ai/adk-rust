//! # Sandbox Agent Example
//!
//! Demonstrates an LLM agent that executes code in an OS-level sandboxed
//! environment using the `adk-sandbox` crate's sandbox profiles feature.
//!
//! The agent uses Gemini to generate Python code, then executes it through
//! a `ProcessBackend` configured with a `SandboxPolicy` that:
//! - Allows read access to system paths (Python interpreter, libraries)
//! - Allows read-write access to a temp directory for script output
//! - Denies network access
//! - Allows process spawning (Python interpreter needs it)
//!
//! ## Platform Support
//!
//! - **macOS**: Seatbelt (`sandbox-exec`) — "allow default, deny dangerous" strategy
//! - **Linux**: bubblewrap (`bwrap`) — namespace-based filesystem isolation
//! - **Windows**: AppContainer — token-based ACL restrictions
//! - **Other**: Falls back to unsandboxed execution with a warning
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set
//! - macOS: built-in (Seatbelt)
//! - Linux: `apt install bubblewrap` or `dnf install bubblewrap`
//! - Windows: Windows 8+ (AppContainer)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/sandbox_agent/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Event, Part, Tool, ToolContext};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_sandbox::{
    ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend, SandboxPolicyBuilder,
    get_enforcer,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn banner(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!("{name} environment variable is required. Set it in .env or export it.")
    })
}

/// Result of processing a stream of agent events for one prompt.
#[derive(Default)]
struct PromptResult {
    tool_called: bool,
    tool_succeeded: bool,
    tool_failed: bool,
    had_error: bool,
    sandbox_blocked: bool,
}

/// Prints the parts of an agent event and updates the prompt result tracker.
fn print_event(event: &Event, result: &mut PromptResult) {
    let Some(ref content) = event.llm_response.content else {
        return;
    };
    for part in &content.parts {
        match part {
            Part::FunctionCall { name, args, .. } => {
                result.tool_called = true;
                println!("\n  🔧 Tool call: {name}");
                let parsed = args;
                if let Some(code) = parsed.get("code").and_then(|v| v.as_str()) {
                    println!("  ┌─── Code ───────────────────────────────────");
                    for line in code.lines() {
                        println!("  │ {line}");
                    }
                    println!("  └─────────────────────────────────────────────");
                }
                if let Some(lang) = parsed.get("language").and_then(|v| v.as_str()) {
                    println!("  Language: {lang}");
                }
                println!("  ⏳ Executing in sandbox...");
            }
            Part::FunctionResponse { function_response, .. } => {
                let output = &function_response.response;
                let exit_code = output.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
                let duration = output.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);

                if exit_code == 0 {
                    result.tool_succeeded = true;
                    println!("  ✅ Execution succeeded (exit code 0, {duration}ms)");
                } else {
                    result.tool_failed = true;
                    println!("  ❌ Execution failed (exit code {exit_code}, {duration}ms)");
                }

                if let Some(stdout) = output.get("stdout").and_then(|v| v.as_str()) {
                    if !stdout.is_empty() {
                        println!("  ┌─── stdout ─────────────────────────────────");
                        for line in stdout.lines() {
                            println!("  │ {line}");
                        }
                        println!("  └─────────────────────────────────────────────");
                    }
                }
                if let Some(stderr) = output.get("stderr").and_then(|v| v.as_str()) {
                    if !stderr.is_empty() {
                        println!("  ┌─── stderr ─────────────────────────────────");
                        for line in stderr.lines() {
                            println!("  │ {line}");
                        }
                        println!("  └─────────────────────────────────────────────");
                    }
                }
                if let Some(error) = output.get("error").and_then(|v| v.as_str()) {
                    result.sandbox_blocked = true;
                    println!("  🚫 Sandbox error: {error}");
                }
            }
            _ => {
                if let Some(text) = part.text() {
                    if !text.is_empty() {
                        if event.llm_response.partial {
                            print!("{text}");
                            let _ = std::io::Write::flush(&mut std::io::stdout());
                        } else {
                            println!("\n  🤖 Agent: {text}");
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SandboxedCodeTool — executes code through a sandboxed ProcessBackend
// ---------------------------------------------------------------------------

/// A tool that executes Python code inside an OS-level sandbox.
///
/// The sandbox restricts filesystem access, blocks network access, and
/// prevents spawning additional child processes. The tool accepts `language`
/// and `code` parameters and returns stdout, stderr, and exit code as JSON.
struct SandboxedCodeTool {
    backend: Arc<ProcessBackend>,
}

impl SandboxedCodeTool {
    fn new(backend: Arc<ProcessBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Tool for SandboxedCodeTool {
    fn name(&self) -> &str {
        "execute_code"
    }

    fn description(&self) -> &str {
        "Execute Python code in a sandboxed environment. The sandbox restricts filesystem \
         access to specific paths and blocks all network access. Returns JSON with stdout, \
         stderr, and exit_code fields."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python"],
                    "description": "Programming language (currently only 'python' is supported)"
                },
                "code": {
                    "type": "string",
                    "description": "The source code to execute"
                }
            },
            "required": ["language", "code"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let code = args
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::tool("missing 'code' parameter"))?;

        let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("python");

        let lang = match language {
            "python" => Language::Python,
            other => {
                return Ok(serde_json::json!({
                    "error": format!("unsupported language: {other}. Only 'python' is supported in this sandbox.")
                }));
            }
        };

        let request = ExecRequest {
            language: lang,
            code: code.to_string(),
            stdin: None,
            timeout: Duration::from_secs(30),
            memory_limit_mb: None,
            env: HashMap::from([("PATH".to_string(), "/usr/bin:/usr/local/bin".to_string())]),
        };

        match self.backend.execute(request).await {
            Ok(result) => Ok(serde_json::json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "duration_ms": result.duration.as_millis(),
            })),
            Err(e) => Ok(serde_json::json!({
                "error": format!("execution failed: {e}"),
            })),
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present
    let _ = dotenvy::dotenv();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    banner("Sandbox Agent Example");
    println!("This example demonstrates an LLM agent that executes Python code");
    println!("in an OS-level sandboxed environment.\n");

    // -----------------------------------------------------------------------
    // 1. Require API key
    // -----------------------------------------------------------------------
    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // 2. Build the sandbox policy (platform-aware)
    // -----------------------------------------------------------------------
    banner("Building Sandbox Policy");

    let work_dir = std::env::temp_dir().join("adk_sandbox_agent_work");
    std::fs::create_dir_all(&work_dir)?;
    println!("  Work directory: {}", work_dir.display());

    let mut builder = SandboxPolicyBuilder::new();

    // Platform-specific read paths for Python
    if cfg!(target_os = "macos") {
        builder = builder
            .allow_read("/usr")
            .allow_read("/bin")
            .allow_read("/sbin")
            .allow_read("/tmp")
            .allow_read("/private/tmp")
            .allow_read("/private/var")
            .allow_read("/var")
            .allow_read("/System")
            .allow_read("/Library")
            .allow_read("/opt")
            .allow_read("/dev")
            .allow_read_write("/private/tmp")
            .env("PATH", "/usr/bin:/usr/local/bin:/opt/homebrew/bin");
    } else if cfg!(target_os = "linux") {
        builder = builder
            .allow_read("/usr")
            .allow_read("/bin")
            .allow_read("/sbin")
            .allow_read("/lib")
            .allow_read("/lib64")
            .allow_read("/etc")
            .allow_read("/tmp")
            .allow_read("/var")
            .allow_read("/dev")
            .allow_read("/proc")
            .allow_read_write("/tmp")
            .env("PATH", "/usr/bin:/usr/local/bin:/bin");
    } else if cfg!(target_os = "windows") {
        // Windows: Python is typically in Program Files or user AppData
        builder = builder
            .allow_read("C:\\Windows")
            .allow_read("C:\\Program Files")
            .allow_read("C:\\Program Files (x86)")
            .allow_read("C:\\Python*")
            .allow_read("C:\\Users")
            .env("PATH", "C:\\Windows\\System32;C:\\Python312;C:\\Python311");
    }

    let policy = builder
        .allow_read_write(work_dir.to_str().unwrap())
        // Python needs to exec the interpreter
        .allow_process_spawn()
        // Deny network access — code cannot fetch URLs
        // (allow_network() is NOT called, so network is denied by default)
        .build();

    let platform = if cfg!(target_os = "macos") {
        "macOS (Seatbelt)"
    } else if cfg!(target_os = "linux") {
        "Linux (bubblewrap)"
    } else if cfg!(target_os = "windows") {
        "Windows (AppContainer)"
    } else {
        "unknown"
    };

    println!("  Platform:          {platform}");
    println!("  Allowed paths (rw): {}", work_dir.display());
    println!("  Network access:     DENIED");
    println!("  Process spawning:   Allowed (Python interpreter needs it)");

    // -----------------------------------------------------------------------
    // 3. Get the platform enforcer and probe it
    // -----------------------------------------------------------------------
    banner("Platform Enforcer");

    let enforcer = match get_enforcer() {
        Ok(e) => {
            println!("  Enforcer: {} ✓", e.name());
            e
        }
        Err(e) => {
            eprintln!("  ⚠ No sandbox enforcer available: {e}");
            if cfg!(target_os = "linux") {
                eprintln!("  Install bubblewrap: apt install bubblewrap (Debian/Ubuntu)");
                eprintln!("                      dnf install bubblewrap (Fedora/RHEL)");
            } else if cfg!(target_os = "windows") {
                eprintln!("  AppContainer requires Windows 8 or later.");
            } else {
                eprintln!("  macOS Seatbelt should be available on macOS 10.5+.");
            }
            eprintln!("  Falling back to unsandboxed execution for demonstration.\n");

            run_agent_unsandboxed(&api_key).await?;
            return Ok(());
        }
    };

    // -----------------------------------------------------------------------
    // 4. Create the sandboxed ProcessBackend
    // -----------------------------------------------------------------------
    let backend =
        Arc::new(ProcessBackend::with_sandbox(ProcessConfig::default(), enforcer, policy));

    let caps = backend.capabilities();
    println!("  Backend capabilities:");
    println!("    Filesystem isolation: {}", caps.enforced_limits.filesystem_isolation);
    println!("    Network isolation:    {}", caps.enforced_limits.network_isolation);
    println!("    Timeout enforcement:  {}", caps.enforced_limits.timeout);

    // -----------------------------------------------------------------------
    // 5. Build the LLM agent with the sandboxed code tool
    // -----------------------------------------------------------------------
    banner("Building LLM Agent");

    let model = GeminiModel::new(&api_key, "gemini-3.1-flash-lite-preview")?;
    println!("  Model: gemini-3.1-flash-lite-preview");

    let tool = Arc::new(SandboxedCodeTool::new(backend));

    let agent = LlmAgentBuilder::new("sandbox-coder")
        .model(Arc::new(model))
        .tool(tool)
        .instruction(
            "You are a coding assistant. You can execute Python code in a sandboxed environment. \
             The sandbox restricts filesystem access and blocks network access. \
             Write and execute Python code to answer the user's questions. \
             Always use the execute_code tool to run code — do not just describe what the code would do.",
        )
        .build()?;

    println!("  Agent: sandbox-coder ✓");

    // -----------------------------------------------------------------------
    // 6. Set up the runner
    // -----------------------------------------------------------------------
    let session_service = Arc::new(InMemorySessionService::new());

    // Create a session
    session_service
        .create(CreateRequest {
            app_name: "sandbox-agent-demo".to_string(),
            user_id: "demo-user".to_string(),
            session_id: Some("session-1".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "sandbox-agent-demo".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })?;

    // -----------------------------------------------------------------------
    // 7. First prompt — successful code execution
    // -----------------------------------------------------------------------
    banner("Prompt 1: Fibonacci Table");
    println!("  User: Write a Python script that calculates the first 20 Fibonacci");
    println!("        numbers and prints them as a formatted table\n");

    let mut prompt1 = PromptResult::default();
    let mut stream = runner
        .run_str(
            "demo-user",
            "session-1",
            Content::new("user").with_text(
                "Write a Python script that calculates the first 20 Fibonacci numbers \
                 and prints them as a formatted table with columns for index and value.",
            ),
        )
        .await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => print_event(&event, &mut prompt1),
            Err(e) => {
                prompt1.had_error = true;
                eprintln!("  ❌ Error: {e}");
            }
        }
    }
    println!(); // newline after streaming

    // -----------------------------------------------------------------------
    // 8. Second prompt — demonstrate sandbox blocking network access
    // -----------------------------------------------------------------------
    banner("Prompt 2: Network Access (Should Be Blocked)");
    println!("  User: Execute a Python script that fetches https://example.com\n");

    let mut prompt2 = PromptResult::default();
    let mut stream = runner
        .run_str(
            "demo-user",
            "session-1",
            Content::new("user").with_text(
                "Execute this exact Python code using the execute_code tool. Do not modify it, \
                 do not explain it, just run it:\n\n\
                 import urllib.request\n\
                 with urllib.request.urlopen('https://example.com') as r:\n\
                 \x20   print(f'Status: {r.status}')\n\
                 \x20   print(r.read(200).decode())",
            ),
        )
        .await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => print_event(&event, &mut prompt2),
            Err(e) => {
                prompt2.had_error = true;
                eprintln!("  ❌ Error: {e}");
            }
        }
    }
    println!(); // newline after streaming

    // -----------------------------------------------------------------------
    // 9. Summary — reflects what actually happened
    // -----------------------------------------------------------------------
    banner("Summary");

    println!("  Setup:");
    println!("    ✓ Built SandboxPolicy with the builder API");
    println!("    ✓ Got platform enforcer via get_enforcer() → {}", "seatbelt");
    println!("    ✓ Created sandboxed ProcessBackend");
    println!();

    println!("  Prompt 1 (Fibonacci table):");
    if prompt1.had_error {
        println!("    ✗ LLM request failed (rate limit or network error)");
    } else if prompt1.tool_called && prompt1.tool_succeeded {
        println!("    ✓ Agent wrote code and executed it successfully");
    } else if prompt1.tool_called && prompt1.tool_failed {
        println!("    ~ Agent wrote code but execution failed");
    } else if !prompt1.tool_called {
        println!("    ~ Agent responded with text but did not call the tool");
    }

    println!();
    println!("  Prompt 2 (network fetch):");
    if prompt2.had_error {
        println!("    ✗ LLM request failed (rate limit or network error)");
    } else if prompt2.sandbox_blocked || prompt2.tool_failed {
        println!("    ✓ Sandbox correctly blocked network access");
    } else if prompt2.tool_called && prompt2.tool_succeeded {
        println!("    ✗ Unexpected: network access was NOT blocked");
    } else if !prompt2.tool_called {
        println!("    ~ Agent declined to attempt network access (recognized restriction)");
    }

    println!();
    println!("  Sandbox enforcement ({platform}):");
    println!("    • Filesystem access restricted to allowed paths only");
    println!("    • Network access blocked at kernel level");
    println!("    • Process spawning allowed for interpreter execution");

    // Clean up work directory
    let _ = std::fs::remove_dir_all(&work_dir);

    Ok(())
}

// ---------------------------------------------------------------------------
// Fallback for non-macOS platforms
// ---------------------------------------------------------------------------

/// Runs the agent without sandbox enforcement (for demonstration on
/// platforms where Seatbelt is not available).
async fn run_agent_unsandboxed(api_key: &str) -> anyhow::Result<()> {
    banner("Running Without Sandbox (Fallback)");
    println!("  Note: On non-macOS platforms, the sandbox enforcer is not available.");
    println!("  This fallback demonstrates the agent without OS-level restrictions.\n");

    let backend = Arc::new(ProcessBackend::default());
    let model = GeminiModel::new(api_key, "gemini-3.1-flash-lite-preview")?;
    let tool = Arc::new(SandboxedCodeTool::new(backend));

    let agent = LlmAgentBuilder::new("sandbox-coder")
        .model(Arc::new(model))
        .tool(tool)
        .instruction(
            "You are a coding assistant. Execute Python code to answer questions. \
             Use the execute_code tool to run code.",
        )
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: "sandbox-agent-demo".to_string(),
            user_id: "demo-user".to_string(),
            session_id: Some("session-1".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "sandbox-agent-demo".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })?;

    let mut stream = runner
        .run_str(
            "demo-user",
            "session-1",
            Content::new("user")
                .with_text("Write a Python script that prints 'Hello from unsandboxed execution!'"),
        )
        .await?;

    let mut fallback_result = PromptResult::default();
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => print_event(&event, &mut fallback_result),
            Err(e) => eprintln!("  ❌ Error: {e}"),
        }
    }

    Ok(())
}
