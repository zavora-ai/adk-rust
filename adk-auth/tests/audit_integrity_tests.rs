//! Guarded tools fail closed when their audit sink fails, and the file audit chain
//! survives restarts and detects tampering.
//!
//! `ProtectedTool` and `ScopedTool` discarded the result of `AuditSink::log`, so a tool
//! ran with no record of the decision. `FileAuditSink::with_chaining` restarted its chain
//! on every open, so the first event after a restart linked to nothing.

use adk_auth::{
    AccessControl, AuditEvent, AuditFailureMode, AuditOutcome, AuditSink, AuthError, FileAuditSink,
    Permission, ProtectedTool, Role, ScopeGuard, StaticScopeResolver,
};
use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Tool,
    ToolContext,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts executions; requires the `payments:write` scope.
struct PayTool {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for PayTool {
    fn name(&self) -> &str {
        "pay"
    }
    fn description(&self) -> &str {
        "Pays an invoice"
    }
    fn required_scopes(&self) -> &[&str] {
        &["payments:write"]
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "paid": true }))
    }
}

/// A sink whose storage is down.
struct BrokenSink;

#[async_trait]
impl AuditSink for BrokenSink {
    async fn log(&self, _event: AuditEvent) -> Result<(), AuthError> {
        Err(AuthError::AuditError("disk full".to_string()))
    }
}

struct Alice {
    content: Content,
}

#[async_trait]
impl ReadonlyContext for Alice {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "agent"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for Alice {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for Alice {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
}

fn alice() -> Arc<dyn ToolContext> {
    Arc::new(Alice { content: Content::new("user") })
}

fn access_control() -> Arc<AccessControl> {
    Arc::new(
        AccessControl::builder()
            .role(Role::new("payer").allow(Permission::Tool("pay".into())))
            .assign("alice", "payer")
            .build()
            .unwrap(),
    )
}

#[tokio::test]
async fn a_protected_tool_does_not_run_when_its_audit_sink_fails() {
    let runs = Arc::new(AtomicUsize::new(0));
    let tool = ProtectedTool::with_audit(
        PayTool { runs: runs.clone() },
        access_control(),
        Arc::new(BrokenSink),
    );

    let error = tool.execute(alice(), json!({})).await.unwrap_err();

    assert_eq!(error.code, "auth.audit_failed", "{error}");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_scoped_tool_does_not_run_when_its_audit_sink_fails() {
    let runs = Arc::new(AtomicUsize::new(0));
    let guard =
        ScopeGuard::with_audit(StaticScopeResolver::new(vec!["payments:write"]), BrokenSink);
    let tool = guard.protect(PayTool { runs: runs.clone() });

    let error = tool.execute(alice(), json!({})).await.unwrap_err();

    assert_eq!(error.code, "auth.audit_failed", "{error}");
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn warn_mode_runs_the_tool_without_a_record() {
    let runs = Arc::new(AtomicUsize::new(0));
    let guard =
        ScopeGuard::with_audit(StaticScopeResolver::new(vec!["payments:write"]), BrokenSink)
            .with_audit_failure_mode(AuditFailureMode::Warn);
    guard.protect(PayTool { runs: runs.clone() }).execute(alice(), json!({})).await.unwrap();

    ProtectedTool::with_audit(
        PayTool { runs: runs.clone() },
        access_control(),
        Arc::new(BrokenSink),
    )
    .with_audit_failure_mode(AuditFailureMode::Warn)
    .execute(alice(), json!({}))
    .await
    .unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A temporary audit file removed on drop.
struct AuditFile(PathBuf);

impl AuditFile {
    fn new() -> Self {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        Self(std::env::temp_dir().join(format!("adk-audit-{}-{nanos}.jsonl", std::process::id())))
    }
}

impl Drop for AuditFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn log(sink: &FileAuditSink, user: &str, outcome: AuditOutcome) {
    sink.log(AuditEvent::tool_access(user, "pay", outcome)).await.unwrap();
}

#[tokio::test]
async fn the_chain_resumes_from_the_last_line_after_a_restart() {
    let file = AuditFile::new();
    {
        let sink = FileAuditSink::with_chaining(&file.0).unwrap();
        log(&sink, "alice", AuditOutcome::Allowed).await;
        log(&sink, "bob", AuditOutcome::Denied).await;
    }

    let reopened = FileAuditSink::with_chaining(&file.0).unwrap();
    log(&reopened, "carol", AuditOutcome::Allowed).await;

    assert_eq!(reopened.verify().unwrap(), 3);
}

#[tokio::test]
async fn an_edited_line_fails_verification() {
    let file = AuditFile::new();
    let sink = FileAuditSink::with_chaining(&file.0).unwrap();
    log(&sink, "alice", AuditOutcome::Denied).await;
    log(&sink, "bob", AuditOutcome::Allowed).await;
    log(&sink, "carol", AuditOutcome::Allowed).await;
    assert_eq!(sink.verify().unwrap(), 3);

    let original = std::fs::read_to_string(&file.0).unwrap();
    let tampered = original.replacen("\"outcome\":\"denied\"", "\"outcome\":\"allowed\"", 1);
    assert_ne!(original, tampered);
    std::fs::write(&file.0, tampered).unwrap();

    let error = FileAuditSink::verify_file(&file.0, None).unwrap_err();
    assert!(error.to_string().contains("line 2"), "{error}");
}

#[tokio::test]
async fn a_removed_line_fails_verification() {
    let file = AuditFile::new();
    let sink = FileAuditSink::with_chaining(&file.0).unwrap();
    for user in ["alice", "bob", "carol"] {
        log(&sink, user, AuditOutcome::Allowed).await;
    }
    let lines: Vec<String> =
        std::fs::read_to_string(&file.0).unwrap().lines().map(str::to_string).collect();
    std::fs::write(&file.0, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

    assert!(FileAuditSink::verify_file(&file.0, None).is_err());
}

#[tokio::test]
async fn an_hmac_chain_cannot_be_recomputed_without_the_key() {
    let file = AuditFile::new();
    let sink = FileAuditSink::with_hmac_chaining(&file.0, b"audit-key".to_vec()).unwrap();
    log(&sink, "alice", AuditOutcome::Denied).await;
    log(&sink, "bob", AuditOutcome::Allowed).await;
    assert_eq!(sink.verify().unwrap(), 2);
    assert!(FileAuditSink::verify_file(&file.0, Some(b"wrong-key")).is_err());
    assert!(FileAuditSink::verify_file(&file.0, None).is_err());

    // Rewrite the first line and relink the second with plain SHA-256, as someone who
    // can write the file but lacks the key would.
    let lines: Vec<String> =
        std::fs::read_to_string(&file.0).unwrap().lines().map(str::to_string).collect();
    let forged_first = lines[0].replace("\"outcome\":\"denied\"", "\"outcome\":\"allowed\"");
    let mut second: AuditEvent = serde_json::from_str(&lines[1]).unwrap();
    second.prev_hash = AuditEvent::tool_access("x", "y", AuditOutcome::Allowed)
        .with_prev_hash(&forged_first)
        .prev_hash;
    let forged = format!("{forged_first}\n{}\n", serde_json::to_string(&second).unwrap());
    std::fs::write(&file.0, forged).unwrap();

    assert!(FileAuditSink::verify_file(&file.0, None).is_ok(), "a SHA-256 chain is forgeable");
    assert!(FileAuditSink::verify_file(&file.0, Some(b"audit-key")).is_err());
}
