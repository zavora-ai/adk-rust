//! `bash` through a launcher that starts its process itself (spec 3.2 §10 W5).
//!
//! The launcher runs each script with `cmd.exe /d /c` under the Windows restricted-token
//! enforcer's `spawn_contained`, with the workspace granted read-write. `BashTool` keeps its
//! semantics around the contained process: exit code, streaming, the output cap, the timeout,
//! and the Job Object's kill when the shell exits and when the call is dropped (W1). The
//! scripts need nothing from Git for Windows. They run on the PR tier's `windows` job.

#![cfg(windows)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool, ToolContext,
};
use adk_devtools::{BashTool, CommandLauncher, DevToolError, Workspace};
use adk_sandbox::process_tree::ContainedChild;
use adk_sandbox::sandbox::SandboxPolicyBuilder;
use adk_sandbox::sandbox::windows::WindowsEnforcer;
use async_trait::async_trait;
use serde_json::{Value, json};

/// Runs each script with `cmd.exe /d /c` under the restricted token, in the workspace root.
struct Confined;

impl CommandLauncher for Confined {
    fn command(
        &self,
        _ws: &Workspace,
        _script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        Err(DevToolError::Other("this launcher starts its processes itself".into()))
    }

    fn isolation(&self) -> &str {
        "restricted-token"
    }

    fn spawn_contained(
        &self,
        ws: &Workspace,
        script: &str,
        env: &[(String, String)],
        _ctx: &dyn ToolContext,
    ) -> Option<Result<ContainedChild, DevToolError>> {
        let mut builder = SandboxPolicyBuilder::new().allow_read_write(ws.root());
        for (key, value) in env {
            builder = builder.env(key.clone(), value.clone());
        }
        let policy = builder.build();
        let system = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
        Some(
            WindowsEnforcer::new()
                .acknowledge_open_network()
                .spawn_contained(
                    &policy,
                    system.join("System32").join("cmd.exe").as_os_str(),
                    &[OsString::from("/d"), OsString::from("/c"), OsString::from(script)],
                    ws.root(),
                )
                .map_err(|error| DevToolError::Other(error.to_string())),
        )
    }
}

/// A tool context that records the output streamed as progress.
#[derive(Default)]
struct Streamed {
    chunks: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl ReadonlyContext for Streamed {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "tester"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("hi"))
    }
}

#[async_trait]
impl CallbackContext for Streamed {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for Streamed {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
    async fn emit_progress(&self, stream: &str, text: &str) {
        self.chunks.lock().unwrap().push((stream.to_string(), text.to_string()));
    }
}

/// A workspace under its own temporary directory, which also holds `outside`, a directory
/// only the current user can open.
struct Layout {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
}

fn layout() -> Layout {
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let workspace = base.join("ws");
    let outside = base.join("outside");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let user = std::env::var("USERNAME").expect("USERNAME");
    let status = std::process::Command::new("icacls")
        .arg(&outside)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    Layout { _root: root, workspace, outside }
}

fn workspace(dir: &Path) -> Workspace {
    Workspace::new(dir).allow_bash(true).bash_timeout(Duration::from_secs(60))
}

async fn run(
    workspace: Workspace,
    ctx: Arc<dyn ToolContext>,
    script: &str,
    timeout_secs: Option<u64>,
) -> adk_core::Result<Value> {
    let bash = BashTool::new(workspace).with_launcher(Arc::new(Confined));
    let mut args = json!({ "command": script });
    if let Some(secs) = timeout_secs {
        args["timeout_secs"] = json!(secs);
    }
    bash.execute(ctx, args).await
}

/// The size of `tick`, once it exists, after the background loop that appends to it has
/// had `settle` to run on; `None` when it was never written.
fn ticks_after(tick: &Path, settle: Duration) -> Option<(u64, u64)> {
    let size = std::fs::metadata(tick).ok()?.len();
    std::thread::sleep(settle);
    Some((size, std::fs::metadata(tick).ok()?.len()))
}

/// A background `cmd` that appends to `tick.txt` for as long as it runs.
const BACKGROUND_TICKS: &str =
    "start /b cmd /d /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt";

#[tokio::test]
async fn output_and_exit_code_reach_the_result_and_stream_as_progress() {
    let layout = layout();
    let ctx = Arc::new(Streamed::default());

    let result = run(
        workspace(&layout.workspace),
        ctx.clone(),
        "echo hello& echo oops 1>&2& exit /b 3",
        None,
    )
    .await
    .expect("the contained command runs");

    assert_eq!(result["exit_code"], json!(3), "{result}");
    assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello", "{result}");
    assert_eq!(result["stderr"].as_str().unwrap().trim(), "oops", "{result}");
    assert_eq!(result["truncated"], json!(false));
    let chunks = ctx.chunks.lock().unwrap().clone();
    assert!(
        chunks.iter().any(|(stream, text)| stream == "stdout" && text.contains("hello")),
        "{chunks:?}"
    );
}

#[tokio::test]
async fn long_output_keeps_its_head_and_tail_inline() {
    let layout = layout();
    let result = run(
        workspace(&layout.workspace),
        Arc::new(Streamed::default()),
        "for /l %i in (1,1,6000) do @echo line %i",
        None,
    )
    .await
    .expect("the contained command runs");

    assert_eq!(result["truncated"], json!(true), "{result}");
    let stdout = result["stdout"].as_str().unwrap();
    assert!(stdout.chars().count() <= 30_000 + 64, "{} characters kept", stdout.chars().count());
    assert!(stdout.starts_with("line 1"), "{stdout}");
    assert!(stdout.contains(" bytes left out]"), "{stdout}");
    assert!(stdout.trim_end().ends_with("line 6000"), "{stdout}");
}

#[tokio::test]
async fn the_command_is_confined_to_the_workspace() {
    let layout = layout();
    let script = format!(
        "echo inside> inside.txt& echo outside> \"{}\"",
        layout.outside.join("escaped.txt").display()
    );

    run(workspace(&layout.workspace), Arc::new(Streamed::default()), &script, None)
        .await
        .expect("the contained command runs");

    assert_eq!(
        std::fs::read_to_string(layout.workspace.join("inside.txt")).unwrap().trim(),
        "inside"
    );
    assert!(!layout.outside.join("escaped.txt").exists(), "a write landed outside the workspace");
}

#[tokio::test]
async fn a_background_process_does_not_outlive_the_call() {
    let layout = layout();
    // The shell spins long enough for the background loop to start, then exits.
    let script = format!("{BACKGROUND_TICKS}& for /l %j in (1,1,1000000) do @rem");

    let started = Instant::now();
    let result = run(workspace(&layout.workspace), Arc::new(Streamed::default()), &script, None)
        .await
        .expect("the call returns when the shell exits");

    assert!(started.elapsed() < Duration::from_secs(45), "took {:?}", started.elapsed());
    assert_eq!(result["exit_code"], json!(0), "{result}");
    let (before, after) = ticks_after(&layout.workspace.join("tick.txt"), Duration::from_secs(2))
        .expect("the background loop ran");
    assert_eq!(before, after, "a background process kept running after the call");
}

#[tokio::test]
async fn a_timeout_kills_every_process_the_command_started() {
    let layout = layout();
    let script = format!("{BACKGROUND_TICKS}& ping -n 300 127.0.0.1 >nul");

    let started = Instant::now();
    let result =
        run(workspace(&layout.workspace), Arc::new(Streamed::default()), &script, Some(5)).await;

    assert!(result.is_err(), "the call must time out: {result:?}");
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    let (before, after) = ticks_after(&layout.workspace.join("tick.txt"), Duration::from_secs(2))
        .expect("the background loop ran");
    assert_eq!(before, after, "a background process outlived the timeout");
}

#[tokio::test]
async fn dropping_the_call_kills_every_process_the_command_started() {
    let layout = layout();
    let script = format!("{BACKGROUND_TICKS}& ping -n 300 127.0.0.1 >nul");

    let call = run(workspace(&layout.workspace), Arc::new(Streamed::default()), &script, None);
    // The call cannot finish within the window, so the outer timeout drops it mid-flight, as
    // a freeze or a cancelled run does.
    let dropped = tokio::time::timeout(Duration::from_secs(5), call).await;

    assert!(dropped.is_err(), "the call finished: {dropped:?}");
    let (before, after) = ticks_after(&layout.workspace.join("tick.txt"), Duration::from_secs(2))
        .expect("the background loop ran");
    assert_eq!(before, after, "a background process outlived the dropped call");
}
