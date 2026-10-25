//! Change safety (spec 3.1 §7.26, §9.2 I66, I67): digest-bearing read records, the
//! before-hash, multi-edit `edit_file` and `apply_patch`.

use std::path::Path;
use std::sync::Arc;

use adk_core::{ReadonlyContext, Tool, ToolContext};
use adk_devtools::{DevToolset, InMemoryReadLedger, ReadLedger, ReadScope, Workspace, sha256_hex};
use proptest::prelude::*;
use serde_json::{Value, json};

mod common;
use common::TestCtx;

async fn tools(ws: &Workspace) -> Vec<Arc<dyn Tool>> {
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(TestCtx);
    adk_core::Toolset::tools(&DevToolset::new(ws.clone()), ctx).await.unwrap()
}

async fn call(tools: &[Arc<dyn Tool>], name: &str, args: Value) -> adk_core::Result<Value> {
    let tool = tools.iter().find(|tool| tool.name() == name).expect("tool present");
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    tool.execute(ctx, args).await
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

fn code(result: adk_core::Result<Value>) -> String {
    result.expect_err("the call fails").code.to_string()
}

// ── Read records with content (D7) ──

#[tokio::test]
async fn an_edit_of_a_file_changed_since_it_was_read_is_stale_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    call(&tools, "read_file", json!({ "path": "a.txt" })).await.unwrap();

    // Another member, a formatter or the person changes the file.
    std::fs::write(dir.path().join("a.txt"), "hello there world\n").unwrap();

    let edit = json!({ "path": "a.txt", "old_string": "world", "new_string": "planet" });
    let error = call(&tools, "edit_file", edit.clone()).await.unwrap_err();
    assert_eq!(error.code, "devtools.stale");
    assert!(error.message.contains("Read it again"), "{}", error.message);
    assert_eq!(read(dir.path(), "a.txt"), "hello there world\n");

    call(&tools, "read_file", json!({ "path": "a.txt" })).await.unwrap();
    call(&tools, "edit_file", edit).await.unwrap();
    assert_eq!(read(dir.path(), "a.txt"), "hello there planet\n");
}

#[tokio::test]
async fn the_agents_own_writes_update_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    call(&tools, "write_file", json!({ "path": "a.txt", "content": "one\n" })).await.unwrap();
    call(&tools, "edit_file", json!({ "path": "a.txt", "old_string": "one", "new_string": "two" }))
        .await
        .unwrap();
    call(&tools, "edit_file", json!({ "path": "a.txt", "old_string": "two", "new_string": "3" }))
        .await
        .unwrap();
    assert_eq!(read(dir.path(), "a.txt"), "3\n");
}

#[tokio::test]
async fn read_records_survive_a_new_workspace_over_the_same_ledger() {
    // spec 3.1 §7.33 fix 5: a held edit approved from a new process finds the read.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let ledger: Arc<dyn ReadLedger> = Arc::new(InMemoryReadLedger::new());
    {
        let first = tools(&Workspace::new(dir.path()).with_read_ledger(ledger.clone())).await;
        call(&first, "read_file", json!({ "path": "a.txt" })).await.unwrap();
    }
    let scope = ReadScope::new("test-app", "tester", "session-1", "test-agent");
    assert_eq!(ledger.digest(&scope, "a.txt").await.unwrap(), Some(sha256_hex(b"hello\n")));

    let fresh = tools(&Workspace::new(dir.path())).await;
    let edit = json!({ "path": "a.txt", "old_string": "hello", "new_string": "bye" });
    assert_eq!(code(call(&fresh, "edit_file", edit.clone()).await), "devtools.not_read");
    let second = tools(&Workspace::new(dir.path()).with_read_ledger(ledger)).await;
    call(&second, "edit_file", edit).await.unwrap();
    assert_eq!(read(dir.path(), "a.txt"), "bye\n");
}

// ── The before-hash (expect_sha256) ──

#[tokio::test]
async fn expect_sha256_binds_edits_and_writes_to_the_reviewed_content() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    let v1 = sha256_hex(b"v1\n");

    // A decision's hash stands in for the read record.
    call(
        &tools,
        "edit_file",
        json!({ "path": "a.txt", "old_string": "v1", "new_string": "v2", "expect_sha256": v1 }),
    )
    .await
    .unwrap();
    assert_eq!(read(dir.path(), "a.txt"), "v2\n");
    let stale = call(
        &tools,
        "edit_file",
        json!({ "path": "a.txt", "old_string": "v2", "new_string": "v3", "expect_sha256": v1 }),
    )
    .await;
    assert_eq!(code(stale), "devtools.stale");

    let write =
        |expect: Value| json!({ "path": "a.txt", "content": "w\n", "expect_sha256": expect });
    assert_eq!(code(call(&tools, "write_file", write(json!(v1))).await), "devtools.stale");
    assert_eq!(code(call(&tools, "write_file", write(json!("absent"))).await), "devtools.stale");
    call(&tools, "write_file", write(json!(sha256_hex(b"v2\n")))).await.unwrap();
    assert_eq!(read(dir.path(), "a.txt"), "w\n");

    call(
        &tools,
        "write_file",
        json!({ "path": "new.txt", "content": "n", "expect_sha256": "absent" }),
    )
    .await
    .unwrap();
    assert_eq!(
        code(call(&tools, "write_file", write(json!("not-a-digest"))).await),
        "devtools.invalid_argument"
    );
}

// ── edit_file { edits: [...] } ──

#[tokio::test]
async fn several_edits_apply_together_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let a = 1;\nlet b = 2;\nlet c = 3;\n").unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    call(&tools, "read_file", json!({ "path": "a.rs" })).await.unwrap();

    let failing = json!({ "path": "a.rs", "edits": [
        { "old_string": "a = 1", "new_string": "a = 10" },
        { "old_string": "missing", "new_string": "x" }
    ] });
    let error = call(&tools, "edit_file", failing).await.unwrap_err();
    assert_eq!(error.code, "devtools.no_match");
    assert!(error.message.contains("edits[1]"), "{}", error.message);
    assert_eq!(read(dir.path(), "a.rs"), "let a = 1;\nlet b = 2;\nlet c = 3;\n");

    let result = call(
        &tools,
        "edit_file",
        json!({ "path": "a.rs", "edits": [
            { "old_string": "a = 1", "new_string": "a = 10" },
            { "old_string": "c = 3", "new_string": "c = 30" }
        ] }),
    )
    .await
    .unwrap();
    assert_eq!(result["replacements"], 2);
    assert_eq!(read(dir.path(), "a.rs"), "let a = 10;\nlet b = 2;\nlet c = 30;\n");
}

// ── apply_patch (D8) ──

const PATCH: &str = "*** Begin Patch
*** Add File: docs/new.md
+# New
*** Update File: src/lib.rs
@@ fn two()
-    2
+    22
*** Delete File: old.txt
*** Update File: src/name.rs
*** Move to: src/renamed.rs
@@
-old
+new
*** End Patch";

fn patch_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "fn one() {\n    2\n}\nfn two() {\n    2\n}\n")
        .unwrap();
    std::fs::write(dir.path().join("old.txt"), "gone\n").unwrap();
    std::fs::write(dir.path().join("src/name.rs"), "old\r\nkeep\r\n").unwrap();
    dir
}

#[tokio::test]
async fn a_patch_adds_updates_deletes_and_moves_and_reports_each_file() {
    let dir = patch_workspace();
    let tools = tools(&Workspace::new(dir.path())).await;
    let lib_before = sha256_hex(read(dir.path(), "src/lib.rs").as_bytes());

    let result = call(&tools, "apply_patch", json!({ "patch": PATCH })).await.unwrap();

    assert_eq!(read(dir.path(), "docs/new.md"), "# New\n");
    assert_eq!(read(dir.path(), "src/lib.rs"), "fn one() {\n    2\n}\nfn two() {\n    22\n}\n");
    assert!(!dir.path().join("old.txt").exists());
    assert!(!dir.path().join("src/name.rs").exists());
    assert_eq!(read(dir.path(), "src/renamed.rs"), "new\r\nkeep\r\n", "CRLF kept");
    let files = result["files"].as_array().unwrap();
    let ops: Vec<(&str, &str)> =
        files.iter().map(|f| (f["op"].as_str().unwrap(), f["path"].as_str().unwrap())).collect();
    assert_eq!(
        ops,
        vec![
            ("add", "docs/new.md"),
            ("update", "src/lib.rs"),
            ("delete", "old.txt"),
            ("move", "src/renamed.rs")
        ]
    );
    assert_eq!(files[0]["beforeSha256"], Value::Null);
    assert_eq!(files[1]["beforeSha256"], json!(lib_before));
    assert_eq!(
        files[1]["afterSha256"],
        json!(sha256_hex(read(dir.path(), "src/lib.rs").as_bytes()))
    );
    assert_eq!(files[2]["afterSha256"], Value::Null);
    assert_eq!(files[3]["from"], json!("src/name.rs"));

    // The patch's writes are the agent's own: an edit needs no read.
    call(
        &tools,
        "edit_file",
        json!({ "path": "src/lib.rs", "old_string": "22", "new_string": "3" }),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_hunk_that_does_not_apply_fails_the_whole_patch_before_any_write() {
    let dir = patch_workspace();
    let tools = tools(&Workspace::new(dir.path())).await;
    let broken = PATCH.replace("-old\n+new", "-old\n-not in the file\n+new");

    let error = call(&tools, "apply_patch", json!({ "patch": broken })).await.unwrap_err();

    assert_eq!(error.code, "devtools.invalid_argument");
    assert!(error.message.contains("hunk 1 of 1 for src/name.rs"), "{}", error.message);
    assert!(error.message.contains("nearest candidate"), "{}", error.message);
    assert!(!dir.path().join("docs/new.md").exists(), "an earlier file was written");
    assert!(dir.path().join("old.txt").exists());
    assert_eq!(read(dir.path(), "src/lib.rs"), "fn one() {\n    2\n}\nfn two() {\n    2\n}\n");
}

#[cfg(unix)]
#[tokio::test]
async fn a_write_that_fails_midway_restores_the_files_already_written() {
    let dir = patch_workspace();
    // The last file's directory cannot be created: `blocker` is a file.
    std::fs::write(dir.path().join("blocker"), "").unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    let patch = PATCH.replace("*** Move to: src/renamed.rs", "*** Move to: blocker/renamed.rs");

    let error = call(&tools, "apply_patch", json!({ "patch": patch })).await.unwrap_err();

    assert!(!error.code.is_empty());
    assert!(!dir.path().join("docs/new.md").exists(), "the added file was taken back");
    assert_eq!(read(dir.path(), "old.txt"), "gone\n", "the deleted file was restored");
    assert_eq!(read(dir.path(), "src/lib.rs"), "fn one() {\n    2\n}\nfn two() {\n    2\n}\n");
    assert_eq!(read(dir.path(), "src/name.rs"), "old\r\nkeep\r\n");
    let leftovers: Vec<_> =
        walk(dir.path()).into_iter().filter(|name| name.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

#[tokio::test]
async fn a_patch_is_stale_when_a_file_changed_since_it_was_read_or_reviewed() {
    let dir = patch_workspace();
    let tools = tools(&Workspace::new(dir.path())).await;
    call(&tools, "read_file", json!({ "path": "src/lib.rs" })).await.unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "fn one() {\n    2\n}\nfn two() {\n    2\n}\n// x\n",
    )
    .unwrap();

    assert_eq!(
        code(call(&tools, "apply_patch", json!({ "patch": PATCH })).await),
        "devtools.stale"
    );
    assert!(!dir.path().join("docs/new.md").exists());

    let expect = json!({ "src/lib.rs": sha256_hex(b"something else") });
    let reviewed = json!({ "patch": PATCH, "expect_sha256": expect });
    assert_eq!(code(call(&tools, "apply_patch", reviewed).await), "devtools.stale");
    let expect = json!({ "docs/new.md": sha256_hex(b"x") });
    let reviewed = json!({ "patch": PATCH, "expect_sha256": expect });
    assert_eq!(code(call(&tools, "apply_patch", reviewed).await), "devtools.stale");
    assert!(dir.path().join("old.txt").exists());
}

#[tokio::test]
async fn a_patch_refuses_escapes_duplicates_existing_targets_and_secret_files() {
    let dir = patch_workspace();
    std::fs::write(dir.path().join(".env"), "KEY=secret\n").unwrap();
    let tools = tools(&Workspace::new(dir.path())).await;
    let one = |body: &str| json!({ "patch": format!("*** Begin Patch\n{body}\n*** End Patch") });

    assert_eq!(
        code(call(&tools, "apply_patch", one("*** Add File: ../outside.txt\n+x")).await),
        "devtools.path_escape"
    );
    assert_eq!(
        code(call(&tools, "apply_patch", one("*** Add File: old.txt\n+x")).await),
        "devtools.invalid_argument"
    );
    assert_eq!(
        code(
            call(
                &tools,
                "apply_patch",
                one("*** Delete File: old.txt\n*** Delete File: ./old.txt")
            )
            .await
        ),
        "devtools.invalid_argument"
    );
    let secret =
        call(&tools, "apply_patch", one("*** Update File: .env\n@@\n-KEY=x\n+KEY=y")).await;
    let secret = secret.unwrap_err();
    assert_eq!(secret.code, "devtools.path_excluded");
    assert!(!secret.message.contains("KEY="), "the file's lines leak: {}", secret.message);
    assert_eq!(
        code(call(&tools, "apply_patch", json!({ "patch": "not a patch" })).await),
        "devtools.invalid_argument"
    );
    assert!(dir.path().join("old.txt").exists());
}

#[tokio::test]
async fn a_read_only_workspace_offers_no_apply_patch() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&Workspace::read_only(dir.path())).await;
    assert!(!tools.iter().any(|tool| tool.name() == "apply_patch"));
}

// ── Properties (spec 3.1 §11) ──

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// A stale file is never written: whatever changes between the read and the edit,
    /// the edit writes only when the content is what was read.
    #[test]
    fn a_stale_file_is_never_written(original in "[a-c]{1,8}", changed in "[a-c]{1,8}") {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("f"), format!("<{original}>")).unwrap();
            let tools = tools(&Workspace::new(dir.path())).await;
            call(&tools, "read_file", json!({ "path": "f" })).await.unwrap();
            std::fs::write(dir.path().join("f"), format!("<{changed}>")).unwrap();
            let result = call(
                &tools,
                "edit_file",
                json!({ "path": "f", "old_string": "<", "new_string": "[" }),
            )
            .await;
            let now = read(dir.path(), "f");
            if original == changed {
                prop_assert!(result.is_ok());
                prop_assert_eq!(now, format!("[{changed}>"));
            } else {
                prop_assert_eq!(result.unwrap_err().code, "devtools.stale");
                prop_assert_eq!(now, format!("<{changed}>"));
            }
            Ok(())
        })?;
    }

    /// `apply_patch` leaves every file at its before or after digest: all of them after a
    /// success, all of them before a failure.
    #[test]
    fn apply_patch_leaves_every_file_before_or_after(
        lines in proptest::collection::vec("[a-d]{1,3}", 1..6),
        break_last in any::<bool>(),
    ) {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let mut patch = String::from("*** Begin Patch\n");
            let mut before = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                let name = format!("f{index}.txt");
                let content = format!("{index}:{line}\n");
                std::fs::write(dir.path().join(&name), &content).unwrap();
                before.push((name.clone(), content));
                let old = if break_last && index + 1 == lines.len() { "missing" } else { line };
                patch.push_str(&format!("*** Update File: {name}\n@@\n-{index}:{old}\n+{index}:{line}!\n"));
            }
            patch.push_str("*** End Patch");
            let tools = tools(&Workspace::new(dir.path())).await;
            let result = call(&tools, "apply_patch", json!({ "patch": patch })).await;
            prop_assert_eq!(result.is_ok(), !break_last);
            for (name, content) in before {
                let now = read(dir.path(), &name);
                if break_last {
                    prop_assert_eq!(now, content);
                } else {
                    prop_assert_eq!(now, content.replace('\n', "!\n"));
                }
            }
            Ok(())
        })?;
    }
}
