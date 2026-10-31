//! File memory through a real harness run (spec 3.1 §7.15, §9.2 I33): the model writes
//! memory only through `memory_write`/`memory_delete`, every write needs a one-time
//! approval in Ask and Auto and is denied in ReadOnly and Plan, the memory lives outside
//! the workspace, and its index reaches the next run's instruction.

#![cfg(all(feature = "runner", feature = "memory"))]

use crate::common;

use adk_harness::{FileMemory, HarnessMode, MemoryConfig, MemoryLayout, MemoryLimits};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, response_of};
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn write_call() -> ScriptedResponse {
    call(
        "memory_write",
        json!({ "path": "build.md", "summary": "how to build", "content": "Run `cargo build`." }),
    )
}

fn config(base: &std::path::Path) -> MemoryConfig {
    MemoryConfig {
        layout: MemoryLayout::PerUser { base: base.to_path_buf() },
        ..MemoryConfig::default()
    }
}

fn memory_of(base: &std::path::Path, workspace: &std::path::Path, user: &str) -> FileMemory {
    FileMemory::for_user(&config(base), common::APP, user, workspace).unwrap()
}

#[tokio::test]
async fn a_memory_write_needs_a_one_time_approval_and_is_audited_before_it_happens() {
    let base = tempfile::tempdir().unwrap();
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let memory_config = config(base.path());
    let handle = prompter.clone();
    let f = fixture(vec![write_call(), done(), done()], move |b| {
        b.memory(memory_config).prompter(handle)
    })
    .await;
    let events = f.send("remember how to build").await;
    assert_eq!(outcome(&response_of(&events, "memory_write")), Outcome::Executed);
    assert_eq!(prompter.count(), 1, "the write was approved once at a prompt");

    let memory = memory_of(base.path(), f.dir.path(), "alice");
    assert_eq!(memory.read("build.md").await.unwrap(), "Run `cargo build`.");
    assert!(!memory.root().starts_with(f.dir.path()), "memory is outside the workspace");
    assert!(!f.path(".adk").exists(), "nothing was written into the workspace");

    let written = f.records("harness.memory.written");
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].ext["logicalPath"], "build.md");
    assert!(!written[0].ext.contains_key("beforeSha256"), "a new topic has no before digest");
    let after = std::fs::read(memory.root().join("build.md")).unwrap();
    assert_eq!(written[0].ext["afterSha256"], json!(adk_harness_digest(&after)));
    assert!(written[0].ext["functionCallId"].is_string());

    // The next run's instruction carries the index, last, inside its delimited block.
    f.send("what do you remember?").await;
    let system = common::last_system_text(&f.model);
    let block = system.rfind("<memory file=\"MEMORY.md\">").expect("memory block");
    assert!(system[block..].contains("- [build.md](build.md) — how to build"), "{system}");
    assert!(system.trim_end().ends_with("</memory>"), "the index is injected last");

    // Bob has his own, empty memory.
    let bob = memory_of(base.path(), f.dir.path(), "bob");
    assert!(bob.topics().await.unwrap().is_empty());
}

#[tokio::test]
async fn auto_mode_still_asks_for_every_memory_write_and_offers_no_rule() {
    let base = tempfile::tempdir().unwrap();
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    prompter.deny();
    let memory_config = config(base.path());
    let handle = prompter.clone();
    let f = fixture(vec![write_call(), done(), write_call(), done()], move |b| {
        b.memory(memory_config).mode(HarnessMode::Auto).prompter(handle)
    })
    .await;
    let events = f.send("remember").await;
    assert_eq!(outcome(&response_of(&events, "memory_write")), Outcome::Executed);
    assert_eq!(prompter.count(), 1);
    let events = f.send("remember again").await;
    assert_eq!(prompter.count(), 2, "the second write asks again");
    assert_eq!(outcome(&response_of(&events, "memory_write")), Outcome::Denied);
    let prompts = prompter.prompts.lock().unwrap();
    assert!(prompts.iter().all(|prompt| prompt.suggestions.is_empty()), "no rule is ever offered");
    assert!(f.records("harness.memory.written").len() == 1);
}

#[tokio::test]
async fn read_only_and_plan_deny_memory_writes_without_asking() {
    for mode in [HarnessMode::ReadOnly, HarnessMode::Plan] {
        let base = tempfile::tempdir().unwrap();
        let prompter = common::ScriptedPrompter::new();
        let memory_config = config(base.path());
        let handle = prompter.clone();
        let f = fixture(
            vec![write_call(), call("memory_delete", json!({ "path": "build.md" })), done()],
            move |b| b.memory(memory_config).mode(mode).prompter(handle),
        )
        .await;
        let events = f.send("remember").await;
        assert_eq!(outcome(&response_of(&events, "memory_write")), Outcome::Denied, "{mode}");
        assert_eq!(outcome(&response_of(&events, "memory_delete")), Outcome::Denied, "{mode}");
        assert!(memory_of(base.path(), f.dir.path(), "alice").topics().await.unwrap().is_empty());
        assert!(f.records("harness.memory.written").is_empty());
        assert_eq!(prompter.count(), 0, "{mode} denies without asking");
    }
}

#[tokio::test]
async fn memory_read_is_a_read_in_every_mode() {
    let base = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    memory_of(base.path(), workspace.path(), "alice")
        .write("ci.md", "CI runs nextest.", "continuous integration")
        .await
        .unwrap();
    let memory_config = config(base.path());
    let f = common::fixture_in(
        workspace,
        vec![
            call("memory_read", json!({})),
            call("memory_read", json!({ "path": "ci.md" })),
            done(),
        ],
        move |b| b.memory(memory_config).mode(HarnessMode::ReadOnly),
    )
    .await;
    let events = f.send("what do you know?").await;
    let responses = common::responses(&events);
    assert!(responses[0].1["index"].as_str().unwrap().contains("ci.md"), "{responses:?}");
    assert_eq!(responses[1].1["content"], "CI runs nextest.");
}

#[test]
fn memory_is_never_inside_the_workspace_by_default() {
    let workspace = tempfile::tempdir().unwrap();
    let root = MemoryConfig::default().root_for(common::APP, "alice", workspace.path());
    assert!(!root.starts_with(workspace.path()), "{}", root.display());
    assert_eq!(MemoryConfig::default().limits, MemoryLimits::default());
}

fn adk_harness_digest(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}
