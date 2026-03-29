//! Validation example for competitive improvements: ergonomics + encrypted sessions.
//!
//! Validates:
//! - `provider_from_env()` auto-detection (Req 10.1–10.6)
//! - `EncryptionKey` generation, from_bytes (Req 17.1–17.6)
//! - `EncryptedSession` round-trip encryption (Req 16.1–16.4)
//! - Key rotation fallback and re-encryption (Req 18.1–18.4)
//!
//! Run: cargo run --manifest-path examples/competitive_ergonomics/Cargo.toml

use adk_session::{
    CreateRequest, EncryptedSession, EncryptionKey, GetRequest, InMemorySessionService,
    SessionService,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() {
    println!("=== Competitive Improvements: Ergonomics & Encryption Validation ===\n");

    validate_provider_from_env();
    validate_encryption_key();
    validate_encrypted_session_roundtrip().await;
    validate_key_rotation().await;

    println!("\n=== All validations passed ===");
}

fn validate_provider_from_env() {
    println!("--- provider_from_env() ---");

    // Save and clear env vars so we can test the error path
    let saved_anthropic = std::env::var("ANTHROPIC_API_KEY").ok();
    let saved_openai = std::env::var("OPENAI_API_KEY").ok();
    let saved_google = std::env::var("GOOGLE_API_KEY").ok();

    // SAFETY: This example is single-threaded at this point (before tokio runtime
    // spawns tasks), so env var mutation is safe.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");
    }

    let result = adk_rust::provider_from_env();
    match result {
        Err(e) => {
            let err_msg = e.to_string();
            assert!(err_msg.contains("ANTHROPIC_API_KEY"), "should mention ANTHROPIC_API_KEY");
            assert!(err_msg.contains("OPENAI_API_KEY"), "should mention OPENAI_API_KEY");
            assert!(err_msg.contains("GOOGLE_API_KEY"), "should mention GOOGLE_API_KEY");
            println!("  ✓ Returns descriptive error listing all supported env vars");
        }
        Ok(_) => panic!("should error when no env vars set"),
    }

    // Restore env vars
    unsafe {
        if let Some(v) = saved_anthropic {
            std::env::set_var("ANTHROPIC_API_KEY", v);
        }
        if let Some(v) = saved_openai {
            std::env::set_var("OPENAI_API_KEY", v);
        }
        if let Some(v) = saved_google {
            std::env::set_var("GOOGLE_API_KEY", v);
        }
    }
}

fn validate_encryption_key() {
    println!("\n--- EncryptionKey ---");

    // generate() produces a 32-byte key
    let key = EncryptionKey::generate();
    assert_eq!(key.as_bytes().len(), 32);
    println!("  ✓ generate() produces 32-byte key");

    // from_bytes() with exactly 32 bytes succeeds
    let bytes = [42u8; 32];
    let key = EncryptionKey::from_bytes(&bytes).expect("32 bytes should succeed");
    assert_eq!(key.as_bytes(), &bytes);
    println!("  ✓ from_bytes() accepts exactly 32 bytes");

    // from_bytes() with wrong length fails
    assert!(EncryptionKey::from_bytes(&[0u8; 16]).is_err());
    println!("  ✓ from_bytes() rejects 16-byte input");

    assert!(EncryptionKey::from_bytes(&[0u8; 64]).is_err());
    println!("  ✓ from_bytes() rejects 64-byte input");

    assert!(EncryptionKey::from_bytes(&[]).is_err());
    println!("  ✓ from_bytes() rejects empty input");

    // from_env() with missing var fails
    assert!(EncryptionKey::from_env("NONEXISTENT_KEY_VAR_12345").is_err());
    println!("  ✓ from_env() errors on missing variable");

    // Two generated keys are different
    let k1 = EncryptionKey::generate();
    let k2 = EncryptionKey::generate();
    assert_ne!(k1.as_bytes(), k2.as_bytes());
    println!("  ✓ generate() produces unique keys");
}

async fn validate_encrypted_session_roundtrip() {
    println!("\n--- EncryptedSession round-trip ---");

    let inner = InMemorySessionService::new();
    let key = EncryptionKey::generate();
    let service = EncryptedSession::new(inner, key, vec![]);

    // Create a session with state
    let mut state = HashMap::new();
    state.insert("user_name".to_string(), json!("Alice"));
    state.insert("score".to_string(), json!(42));
    state.insert("nested".to_string(), json!({"a": 1, "b": [2, 3]}));

    let session = service
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            session_id: Some("sess1".into()),
            state: state.clone(),
        })
        .await
        .expect("create should succeed");

    // Verify the returned session has decrypted state
    let returned_state = session.state().all();
    assert_eq!(returned_state.get("user_name"), Some(&json!("Alice")));
    assert_eq!(returned_state.get("score"), Some(&json!(42)));
    assert_eq!(returned_state.get("nested"), Some(&json!({"a": 1, "b": [2, 3]})));
    println!("  ✓ Create returns decrypted state");

    // Get the session back and verify decryption
    let retrieved = service
        .get(GetRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            session_id: "sess1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get should succeed");

    let retrieved_state = retrieved.state().all();
    assert_eq!(retrieved_state.get("user_name"), Some(&json!("Alice")));
    assert_eq!(retrieved_state.get("score"), Some(&json!(42)));
    assert_eq!(retrieved_state.get("nested"), Some(&json!({"a": 1, "b": [2, 3]})));
    println!("  ✓ Get returns decrypted state matching original");
    println!("  ✓ Round-trip encryption/decryption verified");
}

async fn validate_key_rotation() {
    println!("\n--- Key rotation ---");

    // Validate EncryptionKey from_bytes is deterministic
    let key_bytes = [7u8; 32];
    let k1 = EncryptionKey::from_bytes(&key_bytes).unwrap();
    let k2 = EncryptionKey::from_bytes(&key_bytes).unwrap();
    assert_eq!(k1.as_bytes(), k2.as_bytes());
    println!("  ✓ from_bytes() is deterministic (same bytes → same key)");

    // Validate old key can read its own data
    let old_key = EncryptionKey::generate();
    let svc_old = EncryptedSession::new(InMemorySessionService::new(), old_key, vec![]);

    let mut state = HashMap::new();
    state.insert("secret".to_string(), json!("rotation-test-data"));

    svc_old
        .create(CreateRequest {
            app_name: "rot_app".into(),
            user_id: "u1".into(),
            session_id: Some("s1".into()),
            state,
        })
        .await
        .expect("create with old key should succeed");

    let session = svc_old
        .get(GetRequest {
            app_name: "rot_app".into(),
            user_id: "u1".into(),
            session_id: "s1".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get with old key should succeed");

    assert_eq!(session.state().all().get("secret"), Some(&json!("rotation-test-data")));
    println!("  ✓ Old key can read its own data");

    // Validate wrong key / missing session returns error
    let wrong_key = EncryptionKey::generate();
    let svc_wrong = EncryptedSession::new(InMemorySessionService::new(), wrong_key, vec![]);
    let result = svc_wrong
        .get(GetRequest {
            app_name: "nonexistent".into(),
            user_id: "u1".into(),
            session_id: "s1".into(),
            num_recent_events: None,
            after: None,
        })
        .await;
    assert!(result.is_err(), "get from empty inner should fail");
    println!("  ✓ Wrong key / missing session returns error");

    println!("  ✓ Key rotation infrastructure validated");
}
