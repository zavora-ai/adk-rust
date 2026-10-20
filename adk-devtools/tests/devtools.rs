//! Integration tests for the developer tools against a temp workspace.

use std::sync::Arc;

use adk_core::{ReadonlyContext, Tool, ToolContext};
use adk_devtools::{DevToolset, Workspace};
use serde_json::{Value, json};

mod common;
use common::TestCtx;

async fn tools(ws: &Workspace) -> Vec<Arc<dyn Tool>> {
    let ts = DevToolset::new(ws.clone());
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(TestCtx);
    adk_core::Toolset::tools(&ts, ctx).await.unwrap()
}

fn find<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools.iter().find(|t| t.name() == name).expect("tool present")
}

async fn run(tool: &Arc<dyn Tool>, args: Value) -> adk_core::Result<Value> {
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    tool.execute(ctx, args).await
}

#[tokio::test]
async fn write_read_edit_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    let tools = tools(&ws).await;

    // write
    let w = run(
        find(&tools, "write_file"),
        json!({"path": "src/main.rs", "content": "fn main() {}\n"}),
    )
    .await
    .unwrap();
    assert_eq!(w["bytes_written"], 13);

    // read (also marks the file as read for editing)
    let r = run(find(&tools, "read_file"), json!({"path": "src/main.rs"})).await.unwrap();
    assert!(r["content"].as_str().unwrap().contains("fn main()"));
    assert_eq!(r["total_lines"], 1);

    // edit
    let e = run(
        find(&tools, "edit_file"),
        json!({"path": "src/main.rs", "old_string": "fn main() {}", "new_string": "fn main() { println!(\"hi\"); }"}),
    )
    .await
    .unwrap();
    assert_eq!(e["replacements"], 1);

    let after = std::fs::read_to_string(dir.path().join("src/main.rs")).unwrap();
    assert!(after.contains("println!"));
}

#[tokio::test]
async fn edit_requires_prior_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
    let ws = Workspace::new(dir.path());
    let tools = tools(&ws).await;

    // editing without reading first must fail
    let err = run(
        find(&tools, "edit_file"),
        json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"}),
    )
    .await;
    assert!(err.is_err(), "edit before read should be rejected");
}

#[tokio::test]
async fn path_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::new(dir.path());
    let tools = tools(&ws).await;
    let err = run(find(&tools, "read_file"), json!({"path": "../../etc/passwd"})).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn glob_and_grep() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
    std::fs::write(dir.path().join("src/util.rs"), "fn gamma() {}\n").unwrap();
    let ws = Workspace::new(dir.path());
    let tools = tools(&ws).await;

    let g = run(find(&tools, "glob"), json!({"pattern": "src/**/*.rs"})).await.unwrap();
    assert_eq!(g["count"], 2);

    let gr = run(find(&tools, "grep"), json!({"pattern": "fn beta"})).await.unwrap();
    assert_eq!(gr["count"], 1);
    assert!(gr["matches"][0].as_str().unwrap().contains("lib.rs:2"));
}

#[tokio::test]
async fn bash_runs_in_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::new(dir.path()).allow_bash(true);
    let tools = tools(&ws).await;

    let b = run(find(&tools, "bash"), json!({"command": "echo hello && exit 3"})).await.unwrap();
    assert!(b["stdout"].as_str().unwrap().contains("hello"));
    assert_eq!(b["exit_code"], 3);
}

#[tokio::test]
async fn read_only_workspace_hides_mutators() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::read_only(dir.path());
    let tools = tools(&ws).await;
    assert!(tools.iter().any(|t| t.name() == "read_file"));
    assert!(!tools.iter().any(|t| t.name() == "write_file"));
    assert!(!tools.iter().any(|t| t.name() == "edit_file"));
    assert!(!tools.iter().any(|t| t.name() == "bash"));
}

// ── Read-before-edit is tracked per session and agent (spec 3.1 §10 D2) ──

/// A tool context for one session and agent.
struct ScopedCtx {
    session: &'static str,
    agent: &'static str,
}

#[async_trait::async_trait]
impl ReadonlyContext for ScopedCtx {
    fn invocation_id(&self) -> &str {
        "inv"
    }
    fn agent_name(&self) -> &str {
        self.agent
    }
    fn user_id(&self) -> &str {
        "tester"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        self.session
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &adk_core::Content {
        static CONTENT: std::sync::OnceLock<adk_core::Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| adk_core::Content::new("user").with_text("hi"))
    }
}

#[async_trait::async_trait]
impl adk_core::CallbackContext for ScopedCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl ToolContext for ScopedCtx {
    fn function_call_id(&self) -> &str {
        "call"
    }
    fn actions(&self) -> adk_core::EventActions {
        adk_core::EventActions::default()
    }
    fn set_actions(&self, _actions: adk_core::EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

async fn run_as(
    tool: &Arc<dyn Tool>,
    session: &'static str,
    agent: &'static str,
    args: Value,
) -> adk_core::Result<Value> {
    tool.execute(Arc::new(ScopedCtx { session, agent }), args).await
}

#[tokio::test]
async fn a_read_in_one_session_does_not_permit_an_edit_in_another() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
    // One workspace, cloned into each toolset, as a server sharing it across sessions does.
    let ws = Workspace::new(dir.path());
    let first = tools(&ws.clone()).await;
    let second = tools(&ws.clone()).await;
    let edit = json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"});

    run_as(find(&first, "read_file"), "session-a", "coder", json!({"path": "a.txt"}))
        .await
        .unwrap();

    let other_session =
        run_as(find(&second, "edit_file"), "session-b", "coder", edit.clone()).await;
    assert!(
        other_session.unwrap_err().to_string().contains("must be read with read_file"),
        "session-b edited a file only session-a had read"
    );
    let other_agent =
        run_as(find(&first, "edit_file"), "session-a", "reviewer", edit.clone()).await;
    assert!(other_agent.is_err(), "another agent in the session edited a file it never read");

    run_as(find(&second, "edit_file"), "session-a", "coder", edit).await.unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "bye world");
}

// ── Secret files are excluded from search and reads (spec 3.1 §10 D5) ──

fn secret_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("certs")).unwrap();
    std::fs::write(dir.path().join(".env"), "API_KEY=canary-secret\n").unwrap();
    std::fs::write(dir.path().join(".env.production"), "API_KEY=canary-prod\n").unwrap();
    std::fs::write(dir.path().join("certs/server.pem"), "API_KEY in a pem\n").unwrap();
    std::fs::write(dir.path().join("src/config.rs"), "let key = env(\"API_KEY\");\n").unwrap();
    dir
}

#[tokio::test]
async fn grep_and_glob_skip_secret_files() {
    let dir = secret_workspace();
    let tools = tools(&Workspace::new(dir.path())).await;

    let found = run(find(&tools, "grep"), json!({"pattern": "API_KEY"})).await.unwrap();
    // `Workspace::display` renders the platform separator, so Windows prints `src\config.rs`.
    let config = std::path::Path::new("src").join("config.rs").display().to_string();
    assert_eq!(found["matches"], json!([format!("{config}:1: let key = env(\"API_KEY\");")]));

    let direct = run(find(&tools, "grep"), json!({"pattern": "canary", "path": ".env"})).await;
    assert_eq!(direct.unwrap()["count"], 0, "naming the secret file as the search path read it");

    let listed = run(find(&tools, "glob"), json!({"pattern": "**/*"})).await.unwrap();
    let listed: Vec<&str> =
        listed["matches"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(listed, ["certs", "src", "src/config.rs"]);
}

#[tokio::test]
async fn read_file_refuses_a_secret_file() {
    let dir = secret_workspace();
    #[cfg(unix)]
    std::os::unix::fs::symlink(".env", dir.path().join("settings.txt")).unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;

    for path in [".env", ".env.production", "certs/server.pem", "./.ENV"] {
        let error = run(find(&tools, "read_file"), json!({ "path": path })).await.unwrap_err();
        assert_eq!(
            (error.category, error.code),
            (adk_core::ErrorCategory::Forbidden, "devtools.path_excluded"),
            "{path}: {error}"
        );
        assert!(!error.to_string().contains("canary"));
    }
    #[cfg(unix)]
    {
        let through_link =
            run(find(&tools, "read_file"), json!({"path": "settings.txt"})).await.unwrap_err();
        assert_eq!(through_link.code, "devtools.path_excluded", "a symlink reached .env");
    }
}

#[tokio::test]
async fn an_empty_exclusion_list_reads_and_searches_everything() {
    let dir = secret_workspace();
    let tools = tools(&Workspace::new(dir.path()).exclude_globs([])).await;

    let read = run(find(&tools, "read_file"), json!({"path": ".env"})).await.unwrap();
    assert!(read["content"].as_str().unwrap().contains("canary-secret"));
    let found = run(find(&tools, "grep"), json!({"pattern": "canary"})).await.unwrap();
    assert_eq!(found["count"], 2);
}
