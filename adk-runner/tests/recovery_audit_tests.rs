//! Crash recovery leaves an audit record for every call it answers.
//!
//! A process that stops mid-tool leaves its calls unanswered; the recovering runner
//! answers them from the action ledger. Each answer is recorded in the runner's audit
//! log under the configured agent, with the outcome the ledger supports.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, InMemoryAuditRecorder,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, ActionLedger, ActionOutcome, ActionRecord, Agent, AgentId, AgentPrincipal,
    Content, Event, EventStream, InMemoryActionLedger, InvocationContext, Part, ToolEffect,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const AGENT_ID: &str = "spiffe://acme.example/agent/clerk";

/// Never runs: recovery reads and appends to the session without starting the agent.
struct Idle;

#[async_trait]
impl Agent for Idle {
    fn name(&self) -> &str {
        "clerk"
    }

    fn description(&self) -> &str {
        "idle"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        unreachable!("recovery starts no agent")
    }
}

fn call(name: &str, id: &str, args: Value) -> Part {
    Part::FunctionCall {
        name: name.to_string(),
        args,
        id: Some(id.to_string()),
        thought_signature: None,
    }
}

fn recovered(name: &str, id: &str, args: &Value, outcome: AuditRecordOutcome) -> AuditRecordDraft {
    let principal = AgentPrincipal::new(AgentId::new(AGENT_ID).unwrap());
    let acting = ActingContext::new(principal).on_behalf_of(SubjectRef::human("controller"));
    AuditRecordDraft::new(AuditRecordKind::ToolCall, name)
        .actor(AuditActor::new(&acting, "inv-1"))
        .args_digest(adk_core::json_digest(args))
        .outcome(outcome)
        .ext("cause", json!("recovered"))
        .ext("sessionId", json!("s"))
        .ext("functionCallId", json!(id))
}

#[tokio::test]
async fn every_call_answered_on_recovery_is_audited_with_its_ledger_outcome() {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "ap".into(),
            user_id: "controller".into(),
            session_id: Some("s".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    // The stopped process issued a payment that completed and a lookup that never returned.
    let pay_args = json!({ "invoice_id": "INV-1", "amount_usd": 300 });
    let lookup_args = json!({});
    let mut issued = Event::new("inv-1");
    issued.author = "clerk".into();
    issued.llm_response.content = Some(Content {
        role: "model".into(),
        parts: vec![
            call("pay_invoice", "c1", pay_args.clone()),
            call("list_due", "c2", lookup_args.clone()),
        ],
    });
    sessions.append_event("s", issued).await.unwrap();
    let ledger = Arc::new(InMemoryActionLedger::new());
    let key = "ap/controller/s/inv-1/c1";
    ledger
        .begin(&ActionRecord::new(key, "pay_invoice", &pay_args, ToolEffect::NonIdempotent))
        .await
        .unwrap();
    ledger
        .complete(key, ActionOutcome::Succeeded { result_digest: "fnv1a128:01".into() })
        .await
        .unwrap();

    let recorder = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name("ap")
        .agent(Arc::new(Idle))
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .action_ledger(ledger)
        .audit_log(Arc::new(recorder.clone()))
        .agent_principal(AgentPrincipal::new(AgentId::new(AGENT_ID).unwrap()))
        .build()
        .unwrap();

    assert_eq!(runner.recover_unanswered_calls("controller", "s").await.unwrap(), 2);
    assert_eq!(
        recorder.records(),
        vec![
            recovered("pay_invoice", "c1", &pay_args, AuditRecordOutcome::Succeeded),
            recovered("list_due", "c2", &lookup_args, AuditRecordOutcome::OutcomeUnknown),
        ]
    );

    // Answered calls are not recovered, or recorded, again.
    assert_eq!(runner.recover_unanswered_calls("controller", "s").await.unwrap(), 0);
    assert_eq!(recorder.records().len(), 2);
}
