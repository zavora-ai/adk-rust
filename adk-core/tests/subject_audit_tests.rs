//! Audit records name subjects by pseudonym, and the audit log erases a subject by
//! redacting the bodies that name it, keeping every hash.

use std::sync::Arc;

use adk_core::audit_log::{
    AuditActor, AuditAppend, AuditHash, AuditLogEntry, AuditLogStore, AuditRecordDraft,
    AuditRecordKind, AuditRecorder, InMemoryAuditLogStore, InMemoryAuditRecorder,
};
use adk_core::principal::{AgentId, SubjectRef};
use adk_core::subject::{
    AuditLogSubjectData, InMemorySubjectRegistry, PseudonymousRecorder, SubjectDataStore,
    SubjectRegistry, SubjectScope, VisitorId,
};
use chrono::{TimeZone, Utc};

fn draft(on_behalf_of: SubjectRef, chain: Vec<SubjectRef>) -> AuditRecordDraft {
    let mut actor = AuditActor::subject(on_behalf_of);
    actor.delegation_chain = chain;
    AuditRecordDraft::new(AuditRecordKind::ToolCall, "search").actor(actor)
}

#[tokio::test]
async fn records_name_covered_subjects_by_pseudonym() {
    let inner = Arc::new(InMemoryAuditRecorder::new());
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let visitor = VisitorId::generate().subject();
    let alice = SubjectRef::human("alice");
    let planner = SubjectRef::agent(&AgentId::new("urn:adk:agent:shop/planner").unwrap());

    let visitors_only = PseudonymousRecorder::new(inner.clone(), registry.clone());
    visitors_only
        .record(draft(visitor.clone(), vec![alice.clone(), planner.clone()]))
        .await
        .unwrap();
    let pseudonym = registry.get(&visitor).await.unwrap().unwrap().pseudonym;
    let recorded = inner.records().pop().unwrap().actor;
    assert_eq!(recorded.on_behalf_of, Some(SubjectRef::visitor(pseudonym.clone())));
    assert_eq!(
        recorded.delegation_chain,
        vec![alice.clone(), planner.clone()],
        "humans and agents keep their ids by default"
    );

    let everyone = PseudonymousRecorder::new(inner.clone(), registry.clone()).for_every_subject();
    everyone.record(draft(alice.clone(), vec![alice.clone(), planner.clone()])).await.unwrap();
    let alice_pseudonym = registry.get(&alice).await.unwrap().unwrap().pseudonym;
    let recorded = inner.records().pop().unwrap().actor;
    assert_eq!(recorded.on_behalf_of, Some(SubjectRef::human(alice_pseudonym.clone())));
    assert_eq!(recorded.delegation_chain, vec![SubjectRef::human(alice_pseudonym), planner]);

    visitors_only.record(draft(visitor.clone(), vec![])).await.unwrap();
    assert_eq!(
        inner.records().pop().unwrap().actor.on_behalf_of,
        Some(SubjectRef::visitor(pseudonym.clone())),
        "stable"
    );
    registry.remove(&visitor).await.unwrap();
    visitors_only.record(draft(visitor, vec![])).await.unwrap();
    let after = inner.records().pop().unwrap().actor.on_behalf_of.unwrap();
    assert_ne!(after.id, pseudonym, "an erased subject's old pseudonym is never reused");
}

/// Appends records naming `subjects`, with leaf hashes derived from their positions.
async fn seed(store: &InMemoryAuditLogStore, log: &str, subjects: &[SubjectRef]) {
    let entries = subjects
        .iter()
        .enumerate()
        .map(|(seq, subject)| {
            let record = draft(subject.clone(), vec![]).into_record(
                log,
                seq as u64,
                Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
                None,
            );
            AuditLogEntry {
                seq: seq as u64,
                leaf_hash: AuditHash([seq as u8; 32]),
                record: Some(serde_json::to_string(&record).unwrap()),
            }
        })
        .collect();
    store.append(AuditAppend { log_id: log.into(), entries, nodes: vec![] }).await.unwrap();
}

#[tokio::test]
async fn erasure_redacts_the_subjects_records_and_keeps_every_hash() {
    let store = InMemoryAuditLogStore::new();
    let alice = SubjectRef::human("alice");
    let bob = SubjectRef::human("bob");
    let visitor = VisitorId::generate().subject();
    seed(
        &store,
        "replica-1",
        &[alice.clone(), bob.clone(), SubjectRef::service("alice"), visitor.clone()],
    )
    .await;
    seed(&store, "replica-2", std::slice::from_ref(&bob)).await;

    let audit = AuditLogSubjectData::new(Arc::new(store.clone()));
    let scope = SubjectScope::new(["shop"], alice).with_linked(vec![visitor]);
    let exported = audit.export(&scope).await.unwrap();
    assert_eq!(exported["logs"][0]["logId"], "replica-1");
    assert_eq!(
        exported["logs"][0]["records"].as_array().unwrap().len(),
        3,
        "alice as any kind, and her visitor id"
    );

    assert_eq!(audit.erase(&scope).await.unwrap().deleted, 3);
    let entries = store.read("replica-1", 0..4).await.unwrap();
    assert_eq!(
        entries.iter().map(|e| e.record.is_some()).collect::<Vec<_>>(),
        [false, true, false, false]
    );
    assert_eq!(
        entries.iter().map(|e| e.leaf_hash).collect::<Vec<_>>(),
        (0..4u8).map(|n| AuditHash([n; 32])).collect::<Vec<_>>()
    );
    assert!(
        store.read("replica-2", 0..1).await.unwrap()[0].record.is_some(),
        "bob's other log is untouched"
    );
    assert_eq!(audit.erase(&scope).await.unwrap().deleted, 0);
}
