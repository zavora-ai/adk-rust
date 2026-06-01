//! Integration tests for Managed Agents webhook signature verification.
//!
//! Webhook endpoint registration is done via the Anthropic Console UI, not the API.
//! These tests verify the signature verification logic and event parsing work
//! correctly with realistic payloads.
//!
//! Run with:
//! ```bash
//! cargo test -p adk-anthropic --features managed-agents --test managed_agents_webhooks_integration
//! ```

#![cfg(feature = "managed-agents")]

use adk_anthropic::managed_agents::{
    WebhookVerifyError, session_events, vault_events, verify_webhook,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ─── Test Helpers ────────────────────────────────────────────────────────────

fn make_secret() -> (String, Vec<u8>) {
    let key = b"integration-test-secret-32bytes!"; // 32 bytes
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    (format!("whsec_{encoded}"), key.to_vec())
}

fn sign(payload: &str, timestamp: u64, key: &[u8]) -> String {
    let signed_content = format!("v1.{timestamp}.{payload}");
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(signed_content.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    format!("v1,{timestamp},{signature}")
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

// ─── Session Webhook Event Tests ─────────────────────────────────────────────

#[test]
fn test_session_status_idled_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_01SessionIdled",
        "created_at": "2026-06-01T03:00:00Z",
        "data": {
            "type": "session.status_idled",
            "id": "sesn_01ABC123",
            "organization_id": "org_01XYZ",
            "workspace_id": "ws_01DEF"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.event_type, "event");
    assert_eq!(event.id, "event_01SessionIdled");
    assert_eq!(event.data.event_type, session_events::STATUS_IDLED);
    assert_eq!(event.data.id, "sesn_01ABC123");
    assert_eq!(event.data.organization_id.as_deref(), Some("org_01XYZ"));
    assert_eq!(event.data.workspace_id.as_deref(), Some("ws_01DEF"));
}

#[test]
fn test_session_status_run_started_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_02RunStarted",
        "created_at": "2026-06-01T03:01:00Z",
        "data": {
            "type": "session.status_run_started",
            "id": "sesn_02DEF456"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, session_events::STATUS_RUN_STARTED);
    assert_eq!(event.data.id, "sesn_02DEF456");
}

#[test]
fn test_session_status_terminated_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_03Terminated",
        "created_at": "2026-06-01T03:02:00Z",
        "data": {
            "type": "session.status_terminated",
            "id": "sesn_03GHI789"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, session_events::STATUS_TERMINATED);
}

#[test]
fn test_session_thread_created_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_04Thread",
        "created_at": "2026-06-01T03:03:00Z",
        "data": {
            "type": "session.thread_created",
            "id": "sesn_04JKL012"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, session_events::THREAD_CREATED);
}

// ─── Vault Webhook Event Tests ───────────────────────────────────────────────

#[test]
fn test_vault_created_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_05VaultCreated",
        "created_at": "2026-06-01T03:04:00Z",
        "data": {
            "type": "vault.created",
            "id": "vlt_01ABC",
            "organization_id": "org_01XYZ",
            "workspace_id": "ws_01DEF"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, vault_events::VAULT_CREATED);
    assert_eq!(event.data.id, "vlt_01ABC");
}

#[test]
fn test_vault_credential_archived_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_06CredArchived",
        "created_at": "2026-06-01T03:05:00Z",
        "data": {
            "type": "vault_credential.archived",
            "id": "vcrd_01ABC"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, vault_events::CREDENTIAL_ARCHIVED);
}

#[test]
fn test_vault_credential_refresh_failed_event() {
    let (secret, key) = make_secret();
    let payload = r#"{
        "type": "event",
        "id": "event_07RefreshFailed",
        "created_at": "2026-06-01T03:06:00Z",
        "data": {
            "type": "vault_credential.refresh_failed",
            "id": "vcrd_02DEF"
        }
    }"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    let event = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event.data.event_type, vault_events::CREDENTIAL_REFRESH_FAILED);
}

// ─── Signature Verification Edge Cases ───────────────────────────────────────

#[test]
fn test_tampered_payload_fails_verification() {
    let (secret, key) = make_secret();
    let original_payload = r#"{"type":"event","id":"event_01","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01"}}"#;
    let ts = current_timestamp();
    let sig = sign(original_payload, ts, &key);

    // Tamper with the payload
    let tampered = r#"{"type":"event","id":"event_01","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_HACKED"}}"#;

    let result = verify_webhook(tampered, &sig, &secret);
    assert!(matches!(result, Err(WebhookVerifyError::SignatureMismatch)));
}

#[test]
fn test_replay_attack_fails() {
    let (secret, key) = make_secret();
    let payload = r#"{"type":"event","id":"event_01","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01"}}"#;
    let old_ts = current_timestamp() - 600; // 10 minutes ago
    let sig = sign(payload, old_ts, &key);

    let result = verify_webhook(payload, &sig, &secret);
    assert!(matches!(result, Err(WebhookVerifyError::TimestampExpired { .. })));
}

#[test]
fn test_wrong_secret_fails() {
    let (_secret, key) = make_secret();
    let payload = r#"{"type":"event","id":"event_01","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01"}}"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    // Use a different secret for verification
    let wrong_key = b"wrong-secret-that-is-32-bytes!!";
    let wrong_encoded = base64::engine::general_purpose::STANDARD.encode(wrong_key);
    let wrong_secret = format!("whsec_{wrong_encoded}");

    let result = verify_webhook(payload, &sig, &wrong_secret);
    assert!(matches!(result, Err(WebhookVerifyError::SignatureMismatch)));
}

#[test]
fn test_idempotent_event_id() {
    let (secret, key) = make_secret();
    let payload = r#"{"type":"event","id":"event_SAME_ID","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01"}}"#;
    let ts = current_timestamp();
    let sig = sign(payload, ts, &key);

    // Verify twice (simulating a retry with same event ID)
    let event1 = verify_webhook(payload, &sig, &secret).unwrap();
    let event2 = verify_webhook(payload, &sig, &secret).unwrap();
    assert_eq!(event1.id, event2.id);
    assert_eq!(event1.id, "event_SAME_ID");
}

// ─── Event Type Constants ────────────────────────────────────────────────────

#[test]
fn test_all_session_event_type_constants() {
    assert_eq!(session_events::STATUS_RUN_STARTED, "session.status_run_started");
    assert_eq!(session_events::STATUS_IDLED, "session.status_idled");
    assert_eq!(session_events::STATUS_RESCHEDULED, "session.status_rescheduled");
    assert_eq!(session_events::STATUS_TERMINATED, "session.status_terminated");
    assert_eq!(session_events::THREAD_CREATED, "session.thread_created");
    assert_eq!(session_events::THREAD_IDLED, "session.thread_idled");
    assert_eq!(session_events::THREAD_TERMINATED, "session.thread_terminated");
    assert_eq!(session_events::OUTCOME_EVALUATION_ENDED, "session.outcome_evaluation_ended");
}

#[test]
fn test_all_vault_event_type_constants() {
    assert_eq!(vault_events::VAULT_CREATED, "vault.created");
    assert_eq!(vault_events::VAULT_ARCHIVED, "vault.archived");
    assert_eq!(vault_events::VAULT_DELETED, "vault.deleted");
    assert_eq!(vault_events::CREDENTIAL_CREATED, "vault_credential.created");
    assert_eq!(vault_events::CREDENTIAL_ARCHIVED, "vault_credential.archived");
    assert_eq!(vault_events::CREDENTIAL_DELETED, "vault_credential.deleted");
    assert_eq!(vault_events::CREDENTIAL_REFRESH_FAILED, "vault_credential.refresh_failed");
}
