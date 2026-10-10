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
