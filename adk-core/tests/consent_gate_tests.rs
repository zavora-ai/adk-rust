//! The tool path refuses a tool the consent gate names until the subject consents.

use std::sync::Arc;

use adk_core::audit_log::{AuditRecordKind, AuditTrail, InMemoryAuditRecorder};
use adk_core::subject::{
    ConsentGate, ConsentPurpose, ConsentRecord, ConsentStore, InMemoryConsentStore,
    InMemorySubjectRegistry, SubjectRegistry, SubjectState, VisitorId,
};
use adk_core::{
    ApprovalScope, GovernedCall, RunConfig, SubjectKind, ToolAuthorization, ToolGate,
    authorize_tool_call,
};
use serde_json::json;

async fn authorize(config: &RunConfig, user: &str, tool: &str) -> ToolAuthorization {
    let gate = ToolGate::new(config, "concierge", "inv-1", ApprovalScope::new("shop", user, "s1"));
    authorize_tool_call(&gate, GovernedCall::new(tool, "call-1", json!({ "list": "weekly" })))
        .await
        .expect("authorization decides")
}

#[tokio::test]
async fn a_gated_tool_runs_only_with_consent_for_its_purpose() {
    let consent = Arc::new(InMemoryConsentStore::new());
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let gate = ConsentGate::new(consent.clone())
        .with_registry(registry.clone())
        .with_tool("subscribe", ConsentPurpose::Newsletter);
    let mut config =
        RunConfig::builder().subject_kind(SubjectKind::Visitor).consent_gate(gate).build();
    config.audit = Some(AuditTrail::new(recorder.clone()));
    let visitor = VisitorId::generate();

    match authorize(&config, visitor.as_str(), "subscribe").await {
        ToolAuthorization::Refuse { reason, .. } => {
            assert!(reason.contains("newsletter"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    let denied = recorder.records();
    assert_eq!(
        denied.last().map(|r| (r.kind.clone(), r.policy_ref.clone())),
        Some((AuditRecordKind::ToolCall, Some("consent".to_string())))
    );
    assert!(
        matches!(
            authorize(&config, visitor.as_str(), "search_catalog").await,
            ToolAuthorization::Execute { .. }
        ),
        "a tool the gate does not name runs"
    );

    consent
        .grant(ConsentRecord::new(visitor.subject(), ConsentPurpose::Newsletter, "2026-10"))
        .await
        .unwrap();
    assert!(matches!(
        authorize(&config, visitor.as_str(), "subscribe").await,
        ToolAuthorization::Execute { .. }
    ));

    registry.set_state(&visitor.subject(), SubjectState::Erasing).await.unwrap();
    assert!(
        matches!(
            authorize(&config, visitor.as_str(), "subscribe").await,
            ToolAuthorization::Refuse { .. }
        ),
        "nothing runs for a subject being erased"
    );
}

#[tokio::test]
async fn an_ungated_subject_is_not_asked_for_consent() {
    let gate = ConsentGate::new(Arc::new(InMemoryConsentStore::new()))
        .with_tool("subscribe", ConsentPurpose::Newsletter);
    let config = RunConfig::builder().consent_gate(gate).build();
    assert!(matches!(
        authorize(&config, "alice", "subscribe").await,
        ToolAuthorization::Execute { .. }
    ));
}
