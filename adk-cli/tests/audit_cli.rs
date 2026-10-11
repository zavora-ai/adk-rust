//! `adk-rust audit` verifies exports, rejects tampered ones, and generates keys.

#![cfg(feature = "audit")]

use std::process::Command;
use std::sync::Arc;

use adk_auth::signed_audit::{AuditExport, AuditLog, AuditSigner, Ed25519Signer};
use adk_core::audit_log::{
    AuditLogStore, AuditRecordDraft, AuditRecordKind, InMemoryAuditLogStore,
};

fn adk_rust(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_adk-rust")).args(args).output().expect("binary runs")
}

async fn write_log(store: Arc<dyn AuditLogStore>, signer: Arc<Ed25519Signer>) {
    let log =
        AuditLog::builder(store, "replica-1", signer).checkpoint_every(3).open().await.unwrap();
    for n in 0..7 {
        log.append(AuditRecordDraft::new(AuditRecordKind::ToolCall, format!("tool-{n}")))
            .await
            .unwrap();
    }
    log.checkpoint().await.unwrap();
}

#[tokio::test]
async fn verify_accepts_a_valid_export_and_rejects_a_tampered_one() {
    let dir = tempfile::tempdir().unwrap();
    let signer = Arc::new(Ed25519Signer::generate("audit.example.com").unwrap());
    let store = Arc::new(InMemoryAuditLogStore::new());
    write_log(store.clone(), signer.clone()).await;
    let export = AuditExport::from_store(store.as_ref(), "replica-1").await.unwrap();
    let good = dir.path().join("good.jsonl");
    std::fs::write(&good, export.to_jsonl()).unwrap();
    let vkey = signer.verifier_key();

    let output = adk_rust(&["audit", "verify", good.to_str().unwrap(), "--key", &vkey]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("VALID"), "{stdout}");

    let bad = dir.path().join("bad.jsonl");
    std::fs::write(&bad, export.to_jsonl().replacen("tool-4", "tool-X", 1)).unwrap();
    let output = adk_rust(&["audit", "verify", bad.to_str().unwrap(), "--key", &vkey, "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!report["failures"].as_array().unwrap().is_empty());
}

#[test]
fn keygen_writes_a_usable_key_pair() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = dir.path().join("audit");
    let prefix = prefix.to_str().unwrap();
    let output = adk_rust(&["audit", "keygen", "audit.example.com", "--out", prefix]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let key = std::fs::read_to_string(dir.path().join("audit.key")).unwrap();
    let vkey = std::fs::read_to_string(dir.path().join("audit.vkey")).unwrap();
    let signer = Ed25519Signer::from_note_key(key.trim()).unwrap();
    assert_eq!(signer.verifier_key(), vkey.trim());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("audit.key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let again = adk_rust(&["audit", "keygen", "audit.example.com", "--out", prefix]);
    assert!(!again.status.success());
    assert_eq!(std::fs::read_to_string(dir.path().join("audit.key")).unwrap(), key);
}

#[cfg(feature = "audit-sql")]
#[tokio::test]
async fn export_reads_a_sqlite_store() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("audit.db").display());
    let store = adk_session::SqliteAuditLogStore::new(&url).await.unwrap();
    store.migrate().await.unwrap();
    let signer = Arc::new(Ed25519Signer::generate("audit.example.com").unwrap());
    write_log(Arc::new(store), signer.clone()).await;

    let out = dir.path().join("export.jsonl");
    let out = out.to_str().unwrap();
    let output = adk_rust(&[
        "audit",
        "export",
        "--database-url",
        &url,
        "--log-id",
        "replica-1",
        "--out",
        out,
    ]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let output = adk_rust(&["audit", "verify", out, "--key", &signer.verifier_key()]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));

    let missing = adk_rust(&["audit", "export", "--database-url", &url, "--log-id", "nope"]);
    assert!(!missing.status.success());
}
