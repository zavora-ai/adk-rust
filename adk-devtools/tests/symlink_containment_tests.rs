//! A workspace must contain file tools, including when symlinks are involved.
//!
//! `Workspace::resolve` normalized a path lexically and checked `starts_with(root)`.
//! A symlink sitting lexically under the root satisfies that check while pointing
//! anywhere on the host, and ordinary file I/O follows it. A symlinked parent
//! directory redirected creation and writes the same way. The existing containment
//! test covered `..` traversal only.

#![cfg(unix)]

use adk_core::{ReadonlyContext, Tool, ToolContext};
use adk_devtools::{DevToolset, Workspace};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::symlink;
use std::sync::Arc;

mod common;
use common::TestCtx;

/// Runs the named tool from a full toolset over `workspace`.
async fn run_tool(workspace: &Workspace, name: &str, args: Value) -> adk_core::Result<Value> {
    let toolset = DevToolset::new(workspace.clone());
    let readonly_ctx: Arc<dyn ReadonlyContext> = Arc::new(TestCtx);
    let tools = adk_core::Toolset::tools(&toolset, readonly_ctx).await.unwrap();
    let tool: &Arc<dyn Tool> =
        tools.iter().find(|tool| tool.name() == name).expect("the toolset must expose the tool");
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    tool.execute(ctx, args).await
}

/// A workspace root plus an outside directory, both inside one temp dir.
struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "host secret").unwrap();
    Fixture { _temp: temp, root, outside }
}

#[test]
fn a_symlinked_file_pointing_outside_is_refused() {
    let f = fixture();
    symlink(f.outside.join("secret.txt"), f.root.join("link.txt")).unwrap();

    let workspace = Workspace::new(&f.root);
    let result = workspace.resolve("link.txt");

    assert!(
        result.is_err(),
        "a symlink to a host file was accepted, resolving to {:?}",
        result.ok()
    );
}

#[test]
fn a_symlinked_directory_pointing_outside_is_refused() {
    let f = fixture();
    symlink(&f.outside, f.root.join("escape")).unwrap();

    let workspace = Workspace::new(&f.root);

    assert!(
        workspace.resolve("escape/secret.txt").is_err(),
        "a read through a symlinked directory was accepted"
    );
    // Creation through a symlinked parent must be refused too, even though the
    // final component does not exist yet.
    assert!(
        workspace.resolve("escape/planted.txt").is_err(),
        "a write through a symlinked directory was accepted"
    );
}

#[test]
fn a_nested_symlinked_parent_is_refused() {
    let f = fixture();
    fs::create_dir_all(f.root.join("a/b")).unwrap();
    symlink(&f.outside, f.root.join("a/b/out")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(
        workspace.resolve("a/b/out/secret.txt").is_err(),
        "a symlink deeper in the tree was accepted"
    );
}

#[test]
fn parent_traversal_is_still_refused() {
    // The original containment property must keep holding.
    let f = fixture();
    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("../outside/secret.txt").is_err());
    assert!(workspace.resolve("/etc/passwd").is_err());
}

#[test]
fn ordinary_paths_inside_the_workspace_still_resolve() {
    // Guards against the containment check rejecting legitimate work.
    let f = fixture();
    fs::create_dir_all(f.root.join("src")).unwrap();
    fs::write(f.root.join("src/main.rs"), "fn main() {}").unwrap();

    let workspace = Workspace::new(&f.root);

    let existing = workspace.resolve("src/main.rs").expect("an existing file must resolve");
    assert!(existing.ends_with("src/main.rs"));

    let new_file = workspace.resolve("src/new_module.rs").expect("a new file must resolve");
    assert!(new_file.ends_with("src/new_module.rs"));

    let new_dir = workspace.resolve("docs/guide/index.md").expect("a new nested path must resolve");
    assert!(new_dir.ends_with("docs/guide/index.md"));
}

#[test]
fn a_symlink_that_stays_inside_the_workspace_is_allowed() {
    // Containment is about where a link *points*, not that a link exists.
    // Repositories legitimately contain internal symlinks, and refusing them would
    // break ordinary work without improving containment.
    let f = fixture();
    fs::write(f.root.join("real.txt"), "inside").unwrap();
    symlink(f.root.join("real.txt"), f.root.join("alias.txt")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("alias.txt").is_ok(), "an inside-pointing link must resolve");
    assert!(workspace.resolve("real.txt").is_ok());
}

// ── Dangling symlinks ─────────────────────────────────────────────────
//
// Canonicalizing a dangling symlink fails, and the check stepped past it to the parent
// directory. A write then followed the link and created its target outside the
// workspace, so a cloned repository containing `link -> ~/.zshenv` let `write_file`
// create `~/.zshenv`.

#[test]
fn a_dangling_symlink_pointing_outside_is_refused() {
    let f = fixture();
    symlink(f.outside.join("planted.txt"), f.root.join("link")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("link").is_err(), "a dangling link to a host path was accepted");
}

#[test]
fn a_dangling_symlinked_directory_is_refused() {
    let f = fixture();
    symlink(f.outside.join("missing-dir"), f.root.join("dir-link")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(
        workspace.resolve("dir-link/planted.txt").is_err(),
        "creation beneath a dangling directory link was accepted"
    );
}

#[test]
fn a_dangling_symlink_is_refused_even_when_it_points_inside() {
    // Where a dangling link lands cannot be verified by canonicalization, so it is
    // refused rather than trusted. Writing to the target path directly still works.
    let f = fixture();
    symlink(f.root.join("not-built-yet.txt"), f.root.join("latest")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("latest").is_err());
    assert!(workspace.resolve("not-built-yet.txt").is_ok());
}

#[test]
fn a_symlink_loop_is_refused() {
    let f = fixture();
    symlink(f.root.join("b"), f.root.join("a")).unwrap();
    symlink(f.root.join("a"), f.root.join("b")).unwrap();

    let workspace = Workspace::new(&f.root);
    assert!(workspace.resolve("a").is_err());
}

#[tokio::test]
async fn write_file_through_a_dangling_symlink_does_not_create_the_target() {
    let f = fixture();
    let target = f.outside.join("planted.txt");
    symlink(&target, f.root.join("link")).unwrap();
    let workspace = Workspace::new(&f.root);

    let result =
        run_tool(&workspace, "write_file", json!({"path": "link", "content": "export EVIL=1"}))
            .await;

    let err = result.expect_err("a write through a dangling link was accepted");
    assert!(err.to_string().contains("escapes the workspace root"), "unexpected error: {err}");
    assert!(!target.exists(), "the write created a file outside the workspace");
}

#[tokio::test]
async fn edit_file_through_a_dangling_symlink_is_refused() {
    let f = fixture();
    let target = f.outside.join("planted.txt");
    symlink(&target, f.root.join("link")).unwrap();
    let workspace = Workspace::new(&f.root);

    let result = run_tool(
        &workspace,
        "edit_file",
        json!({"path": "link", "old_string": "a", "new_string": "b"}),
    )
    .await;

    let err = result.expect_err("an edit through a dangling link was accepted");
    assert!(err.to_string().contains("escapes the workspace root"), "unexpected error: {err}");
    assert!(!target.exists(), "the edit created a file outside the workspace");
}

#[tokio::test]
async fn write_file_through_an_inside_symlink_updates_its_target() {
    // The no-follow open must not break writing through a link that stays inside.
    let f = fixture();
    fs::write(f.root.join("real.txt"), "old").unwrap();
    symlink(f.root.join("real.txt"), f.root.join("alias.txt")).unwrap();
    let workspace = Workspace::new(&f.root);

    run_tool(&workspace, "write_file", json!({"path": "alias.txt", "content": "new"}))
        .await
        .expect("a write through an inside-pointing link must succeed");

    assert_eq!(fs::read_to_string(f.root.join("real.txt")).unwrap(), "new");
    assert!(fs::symlink_metadata(f.root.join("alias.txt")).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn edit_file_still_edits_an_ordinary_file() {
    let f = fixture();
    fs::write(f.root.join("notes.txt"), "hello world").unwrap();
    let workspace = Workspace::new(&f.root);

    run_tool(&workspace, "read_file", json!({"path": "notes.txt"})).await.unwrap();
    run_tool(
        &workspace,
        "edit_file",
        json!({"path": "notes.txt", "old_string": "world", "new_string": "there"}),
    )
    .await
    .expect("an ordinary edit must succeed");

    assert_eq!(fs::read_to_string(f.root.join("notes.txt")).unwrap(), "hello there");
}

// ── glob ──────────────────────────────────────────────────────────────
//
// `glob` joined the pattern onto the root unchecked. The `glob` crate follows `..`
// literally and walks symlinked directories, so both listed paths outside the workspace.

/// The paths a glob call returned.
fn matches(result: &Value) -> Vec<String> {
    result["matches"]
        .as_array()
        .expect("matches must be an array")
        .iter()
        .map(|path| path.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_glob_pattern_with_parent_traversal_is_refused() {
    let f = fixture();
    let workspace = Workspace::new(&f.root);

    for pattern in ["../*", "../../../*", "../outside/*", "src/../../outside/*"] {
        let result = run_tool(&workspace, "glob", json!({"pattern": pattern})).await;
        assert!(result.is_err(), "pattern {pattern} was accepted: {:?}", result.ok());
    }
}

#[tokio::test]
async fn an_absolute_glob_pattern_is_refused() {
    let f = fixture();
    let workspace = Workspace::new(&f.root);
    let pattern = format!("{}/*", f.outside.display());

    let result = run_tool(&workspace, "glob", json!({"pattern": pattern})).await;
    assert!(result.is_err(), "an absolute pattern was accepted: {:?}", result.ok());
}

#[tokio::test]
async fn glob_does_not_list_through_a_symlinked_directory_pointing_outside() {
    let f = fixture();
    fs::create_dir_all(f.root.join("src")).unwrap();
    fs::write(f.root.join("src/main.rs"), "fn main() {}").unwrap();
    symlink(&f.outside, f.root.join("escape")).unwrap();
    let workspace = Workspace::new(&f.root);

    let result = run_tool(&workspace, "glob", json!({"pattern": "**/*"})).await.unwrap();
    let listed = matches(&result);

    assert!(
        listed.iter().all(|path| !path.starts_with("escape")),
        "glob listed through a link to the host: {listed:?}"
    );
    assert!(listed.contains(&"src/main.rs".to_string()), "ordinary files must still be listed");
}

#[tokio::test]
async fn glob_still_lists_through_a_symlink_that_stays_inside() {
    let f = fixture();
    fs::create_dir_all(f.root.join("real")).unwrap();
    fs::write(f.root.join("real/a.txt"), "a").unwrap();
    symlink(f.root.join("real"), f.root.join("alias")).unwrap();
    let workspace = Workspace::new(&f.root);

    let result = run_tool(&workspace, "glob", json!({"pattern": "alias/*.txt"})).await.unwrap();
    assert_eq!(matches(&result), vec!["alias/a.txt".to_string()]);
}

#[tokio::test]
async fn glob_treats_metacharacters_in_the_root_literally() {
    // An unescaped `[` in the root path is read as a character class.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("ws[1]");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), "").unwrap();
    let workspace = Workspace::new(&root);

    let result = run_tool(&workspace, "glob", json!({"pattern": "*.rs"})).await.unwrap();
    assert_eq!(matches(&result), vec!["a.rs".to_string()]);
}
