//! # Blender Studio — ADK-Rust + DeepSeek Flash + two MCP servers
//!
//! An agent that builds a 3D scene in a live Blender session, driving two MCP
//! servers at once through one ADK toolset manager:
//!
//! - **`blender`** — the official Blender Lab MCP server (26 tools). Runs Python in
//!   the live session, reads the scene, searches the bundled API docs, renders, and
//!   reports its own window layout as JSON.
//! - **`desktop`** — `computer-use-mcp`. Does what Blender's Python cannot: launch
//!   the app, capture the whole window, clear a modal dialog that is blocking the
//!   socket, and make gestural strokes.
//!
//! The routing policy between them is not hard-coded here. It is loaded verbatim
//! from `skills/blender-agent/SKILL.md` in the computer-use-mcp checkout, so the
//! policy is versioned with the tools it describes rather than buried in a prompt
//! literal.
//!
//! ## Why both servers
//!
//! Blender exposes almost nothing to the OS accessibility layer — its UI is drawn
//! in OpenGL, so `get_ui_tree` returns a handful of nodes and no controls. The
//! structure comes from Blender itself via `get_screenshot_of_window_as_json`. The
//! skill file encodes that, along with the setup gotchas that otherwise cost an
//! afternoon (Blender's "Allow Online Access" gate, the shared `blender-mcp`
//! package name, the window-screenshot size ceiling, and sandboxed render paths).
//!
//! ## Setup
//!
//! ```bash
//! export DEEPSEEK_API_KEY=...
//! export BLENDER_MCP_BIN=/path/to/official/blender-mcp     # from projects.blender.org
//! export COMPUTER_USE_SKILLS=/path/to/computer-use-mcp     # for the skill file
//! cargo run --manifest-path examples/blender_studio/Cargo.toml
//! ```
//!
//! Pass a task to override the default:
//!
//! ```bash
//! cargo run --manifest-path examples/blender_studio/Cargo.toml -- \
//!   "Build a low-poly desk lamp and render a thumbnail"
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Part};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort, ThinkingMode};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::manager::McpServerManager;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Desktop tools this workflow may reach. Everything else in `computer-use-mcp`
/// stays unreachable: no `run_script`, no `filesystem`, no `process_kill`.
///
/// Blender's own MCP server already executes arbitrary Python, so widening the
/// desktop surface on top of that buys nothing and costs a great deal.
/// Expose only an allowed subset of another toolset's tools.
///
/// `autoApprove` in the server config does **not** do this. It is parsed into
/// `McpServerConfig::auto_approve` and never consulted when tools are listed, so an
/// agent configured with it still receives the server's whole surface — for
/// computer-use that is around seventy tools including `run_script`, which executes
/// arbitrary AppleScript or PowerShell, and `filesystem`, which reads and writes
/// anywhere the process can reach. Approval configuration is not access control, and
/// an example people copy should not imply that it is.
struct Allowed {
    inner: Arc<dyn adk_core::Toolset>,
    allow: &'static [&'static str],
    /// Tools belonging to servers this filter does not govern, passed through by
    /// prefix. Blender's own surface is all legitimately needed here.
    passthrough_prefix: &'static str,
}

#[async_trait::async_trait]
impl adk_core::Toolset for Allowed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn adk_core::ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
        Ok(self
            .inner
            .tools(ctx)
            .await?
            .into_iter()
            .filter(|tool| {
                let name = tool.name();
                // Names are prefixed `server__tool` only when they collide, so match
                // the trailing segment.
                let leaf = name.rsplit("__").next().unwrap_or(name);
                leaf.starts_with(self.passthrough_prefix) || self.allow.contains(&leaf)
            })
            .collect())
    }
}

/// The desktop tools this workflow needs. Everything else is refused.
///
/// Named for what it now does — an allowlist — rather than for the approval config it
/// used to be handed to.
const DESKTOP_ALLOWED: &[&str] = &[
    "open_application",
    "activate_app",
    "list_windows",
    "get_window",
    "get_display_size",
    "screenshot",
    "zoom",
    "left_click",
    "key",
    "mouse_drag",
    "wait",
];

fn preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// Load the routing policy from the computer-use-mcp checkout.
///
/// Falls back to a short inline summary so the example still runs when the
/// checkout is not present, but the file is the source of truth.
fn load_skill() -> String {
    let root = std::env::var("COMPUTER_USE_SKILLS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../computer-use-mcp"));
    let path = root.join("skills/blender-agent/SKILL.md");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            println!("  ✓ skill loaded from {}", path.display());
            // Strip YAML front matter; the body is the policy.
            text.split_once("---\n")
                .and_then(|(_, rest)| rest.split_once("---\n"))
                .map(|(_, body)| body.trim().to_string())
                .unwrap_or(text)
        }
        Err(error) => {
            println!("  ! skill not found at {} ({error}); using inline summary", path.display());
            "Route work by capability. Use blender tools for anything inside Blender: \
             execute_blender_code to build, get_objects_summary to read state, \
             search_api_docs before writing unfamiliar bpy, render_thumbnail_to_path to \
             verify. Never call get_ui_tree or find_element on Blender: its UI is drawn in \
             OpenGL and exposes no accessible controls; use \
             get_screenshot_of_window_as_json instead. Use desktop tools only for what has \
             no API: launching Blender, whole-window screenshots, clearing modal dialogs, \
             and gestural strokes."
                .to_string()
        }
    }
}

/// Per-server tool-call counts and wall time, so the run reports where it spent
/// its effort rather than only what it concluded.
#[derive(Default)]
struct Metrics {
    calls: BTreeMap<String, usize>,
    /// Streamed text arrives as many small deltas; count turns, not deltas.
    turns: usize,
    started: Option<Instant>,
}

impl Metrics {
    /// The MCP manager exposes tool names unprefixed, so a tool is attributed to
    /// the desktop server when it is one we allow-listed and to Blender otherwise.
    fn record(&mut self, tool: &str) {
        let server = if DESKTOP_ALLOWED.contains(&tool) { "desktop" } else { "blender" };
        *self.calls.entry(server.to_string()).or_default() += 1;
        *self.calls.entry(format!("  {tool}")).or_default() += 1;
    }

    fn report(&self) {
        let elapsed = self.started.map(|start| start.elapsed()).unwrap_or_default();
        println!("\n── run summary ─────────────────────────────────────");
        println!("wall time      {:.1}s", elapsed.as_secs_f64());
        println!("model turns    {}", self.turns);
        for (key, count) in &self.calls {
            if !key.starts_with("  ") {
                println!("{key:<14} {count} tool call(s)");
            }
        }
        println!("\nby tool:");
        for (key, count) in &self.calls {
            if let Some(tool) = key.strip_prefix("  ") {
                println!("  {tool:<44} {count}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,adk=info".into()),
        )
        .init();

    println!("=== Blender Studio (ADK-Rust + DeepSeek Flash + 2 MCP servers) ===\n");

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("Set DEEPSEEK_API_KEY");
    let blender_bin = std::env::var("BLENDER_MCP_BIN").expect(
        "Set BLENDER_MCP_BIN to the official blender-mcp binary. Note the official \
         server and the community one share the name `blender-mcp`: `uvx blender-mcp` \
         installs the community package, so the official server must come from \
         projects.blender.org and be invoked by path.",
    );

    // ── 1. Both MCP servers, under one manager ───────────────────────
    let config = format!(
        r#"{{
            "mcpServers": {{
                "blender": {{
                    "command": {blender_bin:?},
                    "args": ["--transport", "stdio"]
                }},
                "desktop": {{
                    "command": "npx",
                    "args": ["--yes", "--prefer-offline", "@zavora-ai/computer-use-mcp"]
                }}
            }}
        }}"#
    );

    let manager = McpServerManager::from_json(&config)?
        .with_health_check_interval(Duration::from_secs(60))
        .with_grace_period(Duration::from_secs(3));

    println!("Starting MCP servers…");
    for (name, result) in &manager.start_all().await {
        match result {
            Ok(()) => println!("  ✓ {name}"),
            Err(error) => return Err(format!("MCP server '{name}' failed to start: {error}").into()),
        }
    }

    // ── 2. DeepSeek Flash, thinking left uncapped ────────────────────
    //
    // Reasoning is billed against max_tokens, and how much a dense screenshot
    // needs is not predictable: one Blender window took 6,959 reasoning tokens
    // before the first word of the answer. Capping it truncates the answer and
    // still bills in full, so no max_tokens is set here.
    let model = DeepSeekClient::new(
        DeepSeekConfig::new(&api_key, adk_model::catalog::DEEPSEEK_DEFAULT)
            .with_thinking_mode(ThinkingMode::Enabled)
            .with_reasoning_effort(ReasoningEffort::Max),
    )?;
    println!("  ✓ model {} (thinking enabled, effort max)", adk_model::catalog::DEEPSEEK_DEFAULT);

    // ── 3. Agent, with the skill as its instruction ──────────────────
    let manager = Arc::new(manager);
    let agent = LlmAgentBuilder::new("blender-studio")
        .description("Builds 3D scenes in a live Blender session via the official Blender MCP, using computer-use only for what has no API")
        .model(Arc::new(model))
        .instruction(format!(
            "You build 3D scenes in a live Blender session.\n\n\
             Tools are namespaced by the server that owns them: `blender__*` acts \
             inside Blender, `desktop__*` acts on the operating system. Read the \
             policy below and follow it — it was written from measurements of this \
             exact setup, and the failure modes it lists are real.\n\n\
             Verify before you claim success: query the scene or render a thumbnail. \
             Never report a result you have not checked.\n\n\
             ── routing policy ──\n{}",
            load_skill()
        ))
        // The filter is what actually restricts the surface. Blender's own tools pass
        // through by prefix because all of them are needed here; the desktop server's
        // are narrowed to the list above.
        .toolset(Arc::new(Allowed {
            inner: Arc::clone(&manager) as Arc<dyn adk_core::Toolset>,
            allow: DESKTOP_ALLOWED,
            passthrough_prefix: "blender",
        }) as Arc<dyn adk_core::Toolset>)
        .build()?;

    // ── 4. Runner ────────────────────────────────────────────────────
    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(adk_session::CreateRequest {
            app_name: "blender-studio".to_string(),
            user_id: "artist".to_string(),
            session_id: Some("studio-1".to_string()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("blender-studio")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()?;

    // ── 5. Task ──────────────────────────────────────────────────────
    let task = std::env::args().nth(1).unwrap_or_else(|| {
        "Confirm Blender is running and the blender server is connected, then clear the \
         default scene and build a small low-poly desk scene: a desk surface, a lamp, and \
         a mug, all resting on or above z=0 and not intersecting each other. Give every \
         object a descriptive name and a distinct material colour. Point the camera at the \
         scene from an isometric angle, render a thumbnail, and report the object names \
         with their world positions and the thumbnail path."
            .to_string()
    });

    println!("\nTask: {}\n{}", preview(&task, 160), "─".repeat(60));

    let mut metrics = Metrics { started: Some(Instant::now()), ..Default::default() };
    let mut buffered = String::new();
    // Flush the accumulated text of a turn when a tool call or the stream ends.
    macro_rules! flush {
        () => {
            if !buffered.trim().is_empty() {
                metrics.turns += 1;
                println!("\n{}", buffered.trim());
                buffered.clear();
            }
        };
    }
    let mut stream = runner
        .run_str(
            "artist",
            "studio-1",
            Content { role: "user".to_string(), parts: vec![Part::Text { text: task }] },
        )
        .await?;

    while let Some(event) = stream.next().await {
        match event {
            Ok(event) => {
                let Some(content) = &event.llm_response.content else { continue };
                for part in &content.parts {
                    match part {
                        Part::Text { text } => buffered.push_str(text),
                        Part::FunctionCall { name, args, .. } => {
                            flush!();
                            metrics.record(name);
                            let args = serde_json::to_string(args).unwrap_or_default();
                            println!("→ {name} {}", preview(&args, 110));
                        }
                        Part::FunctionResponse { function_response, .. } => {
                            let body =
                                serde_json::to_string(&function_response.response).unwrap_or_default();
                            println!("← {} {}", function_response.name, preview(&body, 110));
                        }
                        _ => {}
                    }
                }
            }
            Err(error) => {
                eprintln!("stream error: {error}");
                break;
            }
        }
    }

    flush!();
    println!("{}", "─".repeat(60));
    metrics.report();
    manager.shutdown().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finding this replaces: `autoApprove` in the server config is parsed into
    /// `McpServerConfig::auto_approve` and never consulted when tools are listed, so
    /// the agent received computer-use's whole surface. Approval configuration is not
    /// access control.
    #[test]
    fn the_dangerous_desktop_tools_are_not_reachable() {
        for tool in [
            "run_script",      // arbitrary AppleScript or PowerShell
            "filesystem",      // reads and writes anywhere the process can reach
            "process_kill",
            "registry",
            "write_clipboard", // could carry anything into another application
            "set_value",
        ] {
            assert!(
                !DESKTOP_ALLOWED.contains(&tool),
                "{tool} must not be in the desktop allowlist for a modelling workflow"
            );
        }
    }

    /// And the ones it genuinely needs are present, or the workflow cannot run.
    #[test]
    fn the_tools_this_workflow_needs_are_allowed() {
        for tool in ["screenshot", "zoom", "mouse_drag", "left_click", "key", "list_windows"] {
            assert!(DESKTOP_ALLOWED.contains(&tool), "{tool} is needed to drive Blender by pixel");
        }
    }

    /// Blender's own surface passes by prefix, because all of it is needed here. This
    /// pins that the passthrough is a deliberate prefix and not an empty string, which
    /// would let everything through.
    #[test]
    fn the_passthrough_prefix_is_not_a_wildcard() {
        let filter = Allowed {
            inner: Arc::new(Empty) as Arc<dyn adk_core::Toolset>,
            allow: DESKTOP_ALLOWED,
            passthrough_prefix: "blender",
        };
        assert!(!filter.passthrough_prefix.is_empty(), "an empty prefix allows every tool");
        assert!("blender_execute_code".starts_with(filter.passthrough_prefix));
        assert!(!"run_script".starts_with(filter.passthrough_prefix));
    }

    struct Empty;

    #[async_trait::async_trait]
    impl adk_core::Toolset for Empty {
        fn name(&self) -> &str {
            "empty"
        }
        async fn tools(&self, _ctx: Arc<dyn adk_core::ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
            Ok(Vec::new())
        }
    }
}
