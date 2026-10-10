//! Behaviour tests for `EncryptedSession` over `InMemorySessionService`, inspecting the
//! inner store directly to check what is persisted.
#![cfg(feature = "encrypted-session")]

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, Content, ErrorCategory, Part};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, EncryptedSession, EncryptionKey, Event,
    GetRequest, InMemorySessionService, ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;

const APP: &str = "vault-app";

/// Every secret written by the tests; none may appear in the inner store.
const SECRETS: &[&str] = &[
    "Alice Example",
    "secret-plan",
    "4111-1111",
    "hidden-flag",
    "launch code",
    "secret-agent",
    "prompt-secret",
    "metadata-secret",
    "temp-secret",
    "tool-call-secret",
];

fn key(byte: u8) -> EncryptionKey {
    EncryptionKey::from_bytes(&[byte; 32]).unwrap()
}

fn state(value: Value) -> HashMap<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn identity(user: &str, session: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(user).unwrap(),
        SessionId::try_from(session).unwrap(),
    )
}

fn create_req(user: &str, session: &str, initial: Value) -> CreateRequest {
    CreateRequest {
        app_name: APP.to_string(),
        user_id: user.to_string(),
        session_id: Some(session.to_string()),
        state: state(initial),
    }
}

fn get_req(user: &str, session: &str) -> GetRequest {
    GetRequest {
        app_name: APP.to_string(),
        user_id: user.to_string(),
        session_id: session.to_string(),
        num_recent_events: None,
        after: None,
    }
}

/// An event carrying secrets in every field `EncryptedSession` must protect or drop.
fn secret_event(id: &str) -> Event {
    let mut event = Event::with_id(id, "inv-1");
    event.timestamp = chrono::DateTime::from_timestamp(1_767_225_600, 0).unwrap();
    event.author = "planner".to_string();
    event.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![
            Part::Text { text: "the launch code is 0000".to_string() },
            Part::FunctionCall {
                name: "transfer".to_string(),
                args: json!({"memo": "tool-call-secret"}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            },
        ],
    });
    event.actions.state_delta = state(json!({
        "plan": "secret-plan",
        "user:card": "4111-1111",
        "app:feature": "hidden-flag",
        "temp:scratch": "temp-secret",
    }));
    event.actions.transfer_to_agent = Some("secret-agent".to_string());
    event.long_running_tool_ids = vec!["call-1".to_string()];
    event.llm_request = Some("prompt-secret".to_string());
    event.provider_metadata.insert("trace".to_string(), "metadata-secret".to_string());
    event
}

/// The event `get` returns for `secret_event`: `temp:` keys, `llm_request` and
/// `provider_metadata` are not persisted.
fn persisted(mut event: Event) -> Value {
    event.actions.state_delta.retain(|k, _| !k.starts_with("temp:"));
    event.llm_request = None;
    event.provider_metadata.clear();
    serde_json::to_value(event).unwrap()
}

fn event_values(session: &dyn Session) -> Vec<Value> {
    session.events().all().into_iter().map(|e| serde_json::to_value(e).unwrap()).collect()
}

/// Serializes everything the inner store returns for a session.
async fn raw_dump(inner: &InMemorySessionService, user: &str, session: &str) -> String {
    let raw = inner.get(get_req(user, session)).await.unwrap();
    serde_json::to_string(&json!({
        "state": raw.state().all(),
        "events": event_values(raw.as_ref()),
    }))
    .unwrap()
}

fn assert_no_secrets(dump: &str) {
    for secret in SECRETS {
        assert!(!dump.contains(secret), "plaintext '{secret}' reached the inner store: {dump}");
    }
}

fn assert_code(error: AdkError, category: ErrorCategory, code: &str) {
    assert_eq!((error.category, error.code), (category, code), "{error}");
}

async fn seeded_service(byte: u8) -> EncryptedSession<InMemorySessionService> {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(byte), vec![]);
    service
        .create(create_req(
            "alice",
            "s1",
            json!({"name": "Alice Example", "temp:draft": "temp-secret"}),
        ))
        .await
        .unwrap();
    service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("alice", "s1"),
            event: secret_event("e1"),
        })
        .await
        .unwrap();
    service
}

#[tokio::test]
async fn create_append_get_round_trips_state_and_events() {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(1), vec![]);
    let created = service
        .create(create_req(
            "alice",
            "s1",
            json!({"name": "Alice Example", "temp:draft": "temp-secret"}),
        ))
        .await
        .unwrap();
    assert_eq!(created.state().all(), state(json!({"name": "Alice Example"})));

    let event = secret_event("e1");
    service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("alice", "s1"),
            event: event.clone(),
        })
        .await
        .unwrap();

    let session = service.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(
        session.state().all(),
        state(json!({
            "name": "Alice Example",
            "plan": "secret-plan",
            "user:card": "4111-1111",
            "app:feature": "hidden-flag",
        }))
    );
    assert_eq!(event_values(session.as_ref()), vec![persisted(event)]);
}

#[tokio::test]
async fn nothing_plaintext_reaches_the_inner_store() {
    let service = seeded_service(1).await;

    assert_no_secrets(&raw_dump(service.inner(), "alice", "s1").await);

    let listed = service
        .inner()
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();
    for session in &listed {
        assert_no_secrets(&serde_json::to_string(&session.state().all()).unwrap());
        assert_no_secrets(&serde_json::to_string(&event_values(session.as_ref())).unwrap());
    }
}

#[tokio::test]
async fn empty_initial_state_still_encrypts_later_writes() {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(1), vec![]);
    service.create(create_req("alice", "s1", json!({}))).await.unwrap();

    let mut event = Event::with_id("e1", "inv-1");
    event.actions.state_delta = state(json!({"plan": "secret-plan"}));
    service
        .append_event_for_identity(AppendEventRequest { identity: identity("alice", "s1"), event })
        .await
        .unwrap();

    assert_no_secrets(&raw_dump(service.inner(), "alice", "s1").await);
    let session = service.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(session.state().all(), state(json!({"plan": "secret-plan"})));
}

#[tokio::test]
async fn shared_tiers_stay_shared_across_sessions() {
    let service = seeded_service(1).await;

    let second = service.create(create_req("alice", "s2", json!({}))).await.unwrap();
    assert_eq!(
        second.state().all(),
        state(json!({"user:card": "4111-1111", "app:feature": "hidden-flag"}))
    );

    let other_user = service.create(create_req("bob", "s3", json!({}))).await.unwrap();
    assert_eq!(other_user.state().all(), state(json!({"app:feature": "hidden-flag"})));
}

#[tokio::test]
async fn list_returns_decrypted_state() {
    let service = seeded_service(1).await;

    let listed = service
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].state().all(),
        state(json!({
            "name": "Alice Example",
            "plan": "secret-plan",
            "user:card": "4111-1111",
            "app:feature": "hidden-flag",
        }))
    );
    assert_eq!(event_values(listed[0].as_ref()), vec![persisted(secret_event("e1"))]);
}

#[tokio::test]
async fn a_different_key_cannot_read_the_session() {
    let inner = seeded_service(1).await.into_inner();
    let wrong = EncryptedSession::new(inner, key(2), vec![]);

    let error = wrong.get(get_req("alice", "s1")).await.err().unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.decrypt_failed");

    let error = wrong
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.decrypt_failed");
}

#[tokio::test]
async fn ciphertext_moved_to_another_session_or_key_fails_authentication() {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(1), vec![]);
    service.create(create_req("alice", "a1", json!({"balance": 100}))).await.unwrap();
    let mut event = Event::with_id("e1", "inv-1");
    event.actions.state_delta = state(json!({"note": "secret-plan"}));
    service
        .append_event_for_identity(AppendEventRequest { identity: identity("alice", "a1"), event })
        .await
        .unwrap();

    let raw_alice = service.inner().get(get_req("alice", "a1")).await.unwrap();
    let alice_balance = raw_alice.state().get("balance").unwrap();
    let alice_event = raw_alice.events().at(0).unwrap().clone();

    // State value copied into another user's session.
    service
        .inner()
        .create(create_req("bob", "b1", json!({"balance": alice_balance.clone()})))
        .await
        .unwrap();
    let error = service.get(get_req("bob", "b1")).await.err().unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.decrypt_failed");

    // State value copied to another key of the same session.
    service
        .inner()
        .create(create_req("alice", "a2", json!({"limit": alice_balance})))
        .await
        .unwrap();
    let error = service.get(get_req("alice", "a2")).await.err().unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.decrypt_failed");

    // Whole stored event replayed into another session.
    service.create(create_req("bob", "b2", json!({}))).await.unwrap();
    service
        .inner()
        .append_event_for_identity(AppendEventRequest {
            identity: identity("bob", "b2"),
            event: alice_event,
        })
        .await
        .unwrap();
    let error = service.get(get_req("bob", "b2")).await.err().unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.decrypt_failed");
}

#[tokio::test]
async fn rotation_reencrypts_state_and_preserves_events() {
    let old = seeded_service(1).await;
    old.create(create_req("alice", "s2", json!({"pin": "4111-1111"}))).await.unwrap();

    let rotated = EncryptedSession::new(old.into_inner(), key(2), vec![key(1)]);
    let session = rotated.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(
        session.state().all(),
        state(json!({
            "name": "Alice Example",
            "plan": "secret-plan",
            "user:card": "4111-1111",
            "app:feature": "hidden-flag",
        }))
    );
    assert_eq!(event_values(session.as_ref()), vec![persisted(secret_event("e1"))]);

    // The rotation event is stored but hidden; a second read finds nothing left to rotate.
    let raw_event_count =
        || async { rotated.inner().get(get_req("alice", "s1")).await.unwrap().events().len() };
    assert_eq!(raw_event_count().await, 2);
    let again = rotated.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(event_values(again.as_ref()), vec![persisted(secret_event("e1"))]);
    assert_eq!(raw_event_count().await, 2);

    // State of a session without events is readable with the new key alone once rotated.
    rotated.get(get_req("alice", "s2")).await.unwrap();
    let new_only = EncryptedSession::new(rotated.into_inner(), key(2), vec![]);
    let s2 = new_only.get(get_req("alice", "s2")).await.unwrap();
    assert_eq!(
        s2.state().all(),
        state(json!({"pin": "4111-1111", "user:card": "4111-1111", "app:feature": "hidden-flag"}))
    );
}

/// Inner service whose event appends always fail.
struct RejectingAppends(InMemorySessionService);

#[async_trait]
impl SessionService for RejectingAppends {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        self.0.create(req).await
    }

    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        self.0.get(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        self.0.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.0.delete(req).await
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> adk_core::Result<()> {
        Err(AdkError::session("append rejected"))
    }

    async fn append_event_for_identity(&self, _req: AppendEventRequest) -> adk_core::Result<()> {
        Err(AdkError::session("append rejected"))
    }
}

#[tokio::test]
async fn rotation_failure_is_returned() {
    let old =
        EncryptedSession::new(RejectingAppends(InMemorySessionService::new()), key(1), vec![]);
    old.create(create_req("alice", "s1", json!({"name": "Alice Example"}))).await.unwrap();

    let rotated = EncryptedSession::new(old.into_inner(), key(2), vec![key(1)]);
    let error = rotated.get(get_req("alice", "s1")).await.err().unwrap();
    assert_eq!(error.message, "append rejected");
}

#[tokio::test]
async fn append_by_bare_session_id_is_rejected() {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(1), vec![]);
    service.create(create_req("alice", "s1", json!({}))).await.unwrap();

    let error = service.append_event("s1", secret_event("e1")).await.err().unwrap();
    assert_code(error, ErrorCategory::Unsupported, "session.encryption.identity_required");
    assert_eq!(service.inner().get(get_req("alice", "s1")).await.unwrap().events().len(), 0);
}

#[tokio::test]
async fn reserved_state_key_is_rejected() {
    let service = EncryptedSession::new(InMemorySessionService::new(), key(1), vec![]);
    let error = service
        .create(create_req("alice", "s1", json!({"__encrypted_state": "x"})))
        .await
        .err()
        .unwrap();
    assert_code(error, ErrorCategory::InvalidInput, "session.encryption.reserved_key");
}

/// Encrypts `state` the way releases before the envelope format did: one blob, no AAD.
fn legacy_blob(key_bytes: [u8; 32], state: &Value) -> String {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;

    let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();
    let nonce = [7u8; 12];
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), serde_json::to_vec(state).unwrap().as_slice())
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode([nonce.as_slice(), &ciphertext].concat())
}

#[tokio::test]
async fn legacy_data_is_rejected_by_default_and_migrated_on_opt_in() {
    let inner = InMemorySessionService::new();
    inner
        .create(create_req(
            "alice",
            "s1",
            json!({"__encrypted_state": legacy_blob([1; 32], &json!({"name": "Alice Example"}))}),
        ))
        .await
        .unwrap();
    let mut legacy_event = Event::with_id("e1", "inv-1");
    legacy_event.actions.state_delta = state(json!({"plan": "secret-plan"}));
    inner
        .append_event_for_identity(AppendEventRequest {
            identity: identity("alice", "s1"),
            event: legacy_event.clone(),
        })
        .await
        .unwrap();

    let strict = EncryptedSession::new(inner, key(1), vec![]);
    let error = strict.get(get_req("alice", "s1")).await.err().unwrap();
    assert_code(error, ErrorCategory::Internal, "session.encryption.unencrypted_data");

    let migrating =
        EncryptedSession::new(strict.into_inner(), key(1), vec![]).with_legacy_migration(true);
    let session = migrating.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(
        session.state().all(),
        state(json!({"name": "Alice Example", "plan": "secret-plan"}))
    );
    assert_eq!(event_values(session.as_ref()), vec![serde_json::to_value(legacy_event).unwrap()]);

    let raw = migrating.inner().get(get_req("alice", "s1")).await.unwrap();
    let raw_state = raw.state().all();
    assert_eq!(raw_state.get("__encrypted_state"), Some(&Value::Null));
    assert_no_secrets(&serde_json::to_string(&raw_state).unwrap());

    let again = migrating.get(get_req("alice", "s1")).await.unwrap();
    assert_eq!(again.state().all(), session.state().all());
}
