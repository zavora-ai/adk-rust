//! `ConsentGatedMemoryService` remembers a visitor only with consent for a persistent
//! profile, and erasure still reaches what it stored.

use std::sync::Arc;

use adk_core::Content;
use adk_core::subject::{
    ConsentGate, ConsentPurpose, ConsentRecord, ConsentStore, InMemoryConsentStore,
    InMemorySubjectRegistry, SUBJECT_ERASING_CODE, SubjectRegistry, SubjectState, VisitorId,
};
use adk_memory::{
    ConsentGatedMemoryService, InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest,
};

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: chrono::Utc::now(),
    }
}

fn search(user: &str) -> SearchRequest {
    SearchRequest {
        query: "peanuts".into(),
        user_id: user.into(),
        app_name: "shop".into(),
        limit: None,
        min_score: None,
        project_id: None,
    }
}

#[tokio::test]
async fn memory_needs_consent_for_a_persistent_profile() {
    let inner = Arc::new(InMemoryMemoryService::new());
    let consent = Arc::new(InMemoryConsentStore::new());
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let memory = ConsentGatedMemoryService::new(
        inner.clone(),
        ConsentGate::new(consent.clone()).with_registry(registry.clone()),
    );
    let visitor = VisitorId::generate();
    let user = visitor.as_str();

    memory.add_entry("shop", user, entry("Allergic to peanuts.")).await.unwrap();
    memory.add_session("shop", user, "s1", vec![entry("peanuts again")]).await.unwrap();
    assert!(inner.search(search(user)).await.unwrap().memories.is_empty(), "nothing stored");

    consent
        .grant(ConsentRecord::new(visitor.subject(), ConsentPurpose::PersistentProfile, "2026-10"))
        .await
        .unwrap();
    memory.add_entry("shop", user, entry("Allergic to peanuts.")).await.unwrap();
    assert_eq!(memory.search(search(user)).await.unwrap().memories.len(), 1);

    consent.revoke_purpose(&visitor.subject(), None, chrono::Utc::now()).await.unwrap();
    assert!(memory.search(search(user)).await.unwrap().memories.is_empty(), "revoked: no recall");
    assert_eq!(
        inner.search(search(user)).await.unwrap().memories.len(),
        1,
        "revoking erases nothing"
    );

    registry.set_state(&visitor.subject(), SubjectState::Erasing).await.unwrap();
    let refused = memory.add_entry("shop", user, entry("more")).await.unwrap_err();
    assert_eq!(refused.code, SUBJECT_ERASING_CODE);
    memory.delete_user("shop", user).await.unwrap();
    assert!(inner.search(search(user)).await.unwrap().memories.is_empty(), "erasure reaches it");

    memory.add_entry("shop", "alice", entry("peanuts for alice")).await.unwrap();
    assert_eq!(
        memory.search(search("alice")).await.unwrap().memories.len(),
        1,
        "ungated users pass"
    );
}
