//! Key rotation over SQLite (spec 3.1 §7.12, §9.2 I57): `rewrap_sqlite` re-seals every
//! session, event, app and user value and every held call, decision and request a previous
//! key sealed; the retired key then opens nothing; a database written under a key refuses
//! to open without it; and no payload reaches the file in plaintext.
#![cfg(all(feature = "encrypted-session", feature = "sqlite"))]

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalStore, ApproverSelector, Content,
    ToolApproval, ToolConfirmationRequest,
};
use adk_session::{
    AppendEventRequest, CreateRequest, EncryptedApprovalStore, EncryptedSession, EncryptionKey,
    Event, GetRequest, SessionService, SqliteApprovalStore, SqliteSessionService,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

const APP: &str = "rewrap-app";
const SECRETS: &[&str] = &[
    "secret-plan",
    "Alice Example",
    "hidden-flag",
    "launch code",
    "secret-step",
    "rm -rf build-secret",
    "approver-note",
    "edited-secret",
];

fn key(byte: u8) -> EncryptionKey {
    EncryptionKey::from_bytes(&[byte; 32]).unwrap()
}

fn identity() -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from("alice").unwrap(),
        SessionId::try_from("s1").unwrap(),
    )
}

fn get() -> GetRequest {
    GetRequest {
        app_name: APP.into(),
        user_id: "alice".into(),
        session_id: "s1".into(),
        num_recent_events: None,
        after: None,
    }
}

/// The database file and its write-ahead log, after the pool closed.
fn file_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap();
    let wal = path.with_extension("db-wal");
    if let Ok(more) = std::fs::read(&wal) {
        bytes.extend(more);
    }
    bytes
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

async fn open(dir: &Path) -> (sqlx::SqlitePool, std::path::PathBuf) {
    let path = dir.join("harness.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let service = SqliteSessionService::new(&url).await.unwrap();
    service.migrate().await.unwrap();
    (service.pool().clone(), path)
}

#[tokio::test]
async fn sessions_are_resealed_under_the_new_key_and_the_old_key_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path) = open(dir.path()).await;
    let under_old =
        EncryptedSession::new(SqliteSessionService::from_pool(pool.clone()), key(1), vec![]);
    let mut state: HashMap<String, Value> = HashMap::new();
    state.insert("plan".into(), json!("secret-plan"));
    state.insert("user:name".into(), json!("Alice Example"));
    state.insert("app:flag".into(), json!("hidden-flag"));
    under_old
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: Some("s1".into()),
            state,
        })
        .await
        .unwrap();
    let mut event = Event::new("inv-1");
    event.author = "model".into();
    event.llm_response.content = Some(Content::new("model").with_text("the launch code is 42"));
    event.actions.state_delta.insert("step".into(), json!("secret-step"));
    under_old
        .append_event_for_identity(AppendEventRequest { identity: identity(), event })
        .await
        .unwrap();

    let rotated =
        EncryptedSession::new(SqliteSessionService::from_pool(pool.clone()), key(2), vec![key(1)]);
    let rows = rotated.rewrap_sqlite().await.unwrap();
    assert_eq!(rows, 4, "the session, its event, the app tier and the user tier");
    assert_eq!(rotated.rewrap_sqlite().await.unwrap(), 0, "nothing is left under the old key");

    let only_new =
        EncryptedSession::new(SqliteSessionService::from_pool(pool.clone()), key(2), vec![]);
    let session = only_new.get(get()).await.unwrap();
    assert_eq!(session.state().get("plan"), Some(json!("secret-plan")));
    assert_eq!(session.state().get("step"), Some(json!("secret-step")));
    assert_eq!(session.state().get("user:name"), Some(json!("Alice Example")));
    assert_eq!(session.state().get("app:flag"), Some(json!("hidden-flag")));
    let events = session.events().all();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].llm_response.content.as_ref().unwrap().parts[0]
            .text()
            .unwrap()
            .contains("launch code")
    );

    let only_old =
        EncryptedSession::new(SqliteSessionService::from_pool(pool.clone()), key(1), vec![]);
    let error = only_old.get(get()).await.err().expect("the old key alone opens nothing");
    assert_eq!(error.code, "session.encryption.decrypt_failed");

    // A wrapper with no key at all is the plain store: it sees envelopes, never text.
    pool.close().await;
    let bytes = file_bytes(&path);
    for secret in SECRETS.iter().take(5) {
        assert!(!contains(&bytes, secret), "{secret} reached the file in plaintext");
    }
    assert!(contains(&bytes, "adk-enc:v1:"), "the file holds envelopes");
}

#[tokio::test]
async fn approvals_are_resealed_under_the_new_key_and_never_reach_the_file_in_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, path) = open(dir.path()).await;
    let inner = SqliteApprovalStore::from_pool(pool.clone());
    inner.migrate().await.unwrap();
    let scope = ApprovalScope::new(APP, "alice", "s1");
    let held = ToolConfirmationRequest::new("bash", json!({ "command": "rm -rf build-secret" }))
        .with_function_call_id("call-1");
    let decided =
        ToolConfirmationRequest::new("write_file", json!({ "path": "a", "content": "x" }));
    let under_old = EncryptedApprovalStore::new(inner, key(1), vec![]);
    under_old.record_pending(&scope, &held).await.unwrap();
    under_old
        .decide(
            &scope,
            &decided.fingerprint(),
            ToolApproval {
                edited_args: Some(json!({ "path": "a", "content": "edited-secret" })),
                message: Some("approver-note".into()),
                ..ToolApproval::approve()
            },
        )
        .await
        .unwrap();
    let request = ApprovalRequest::new(
        "req-1",
        scope.clone(),
        &held,
        ApprovalPolicy::new(Vec::<ApproverSelector>::new()),
    );
    under_old.create_request(&request).await.unwrap();

    let rotated = EncryptedApprovalStore::new(
        SqliteApprovalStore::from_pool(pool.clone()),
        key(2),
        vec![key(1)],
    );
    assert_eq!(
        rotated.rewrap_sqlite().await.unwrap(),
        3,
        "the held call, the decision, the request"
    );
    assert_eq!(rotated.rewrap_sqlite().await.unwrap(), 0);

    let only_new =
        EncryptedApprovalStore::new(SqliteApprovalStore::from_pool(pool.clone()), key(2), vec![]);
    let pending = only_new.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request, held);
    assert_eq!(pending[0].fingerprint, held.fingerprint());
    let approval = only_new.take_approval(&scope, &decided.fingerprint()).await.unwrap().unwrap();
    assert_eq!(approval.edited_args, Some(json!({ "path": "a", "content": "edited-secret" })));
    assert_eq!(approval.message.as_deref(), Some("approver-note"));
    let stored = only_new.get_request("req-1").await.unwrap().unwrap();
    assert_eq!(stored.args_redacted, held.args);
    assert_eq!(stored.summary, request.summary);

    let only_old =
        EncryptedApprovalStore::new(SqliteApprovalStore::from_pool(pool.clone()), key(1), vec![]);
    assert_eq!(
        only_old.pending(&scope).await.unwrap_err().code,
        "session.encryption.decrypt_failed"
    );
    assert_eq!(
        only_old.get_request("req-1").await.unwrap_err().code,
        "session.encryption.decrypt_failed"
    );

    pool.close().await;
    let bytes = file_bytes(&path);
    for secret in &SECRETS[5..] {
        assert!(!contains(&bytes, secret), "{secret} reached the file in plaintext");
    }
}
