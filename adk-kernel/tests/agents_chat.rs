//! A suggested agent in a chat thread (spec 3.3 §5.7, §7.2, S-I11): its card offers only
//! "Don't add", at medium and high risk alike, and the gateway refuses an approval forged
//! from it, so no chat message adds an agent.

#![cfg(all(feature = "channels", feature = "conformance"))]

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    AgentId, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, InMemoryApprovalStore, NewRun, RunRecord,
    StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::agents::AGENT_CREATE_TOOL_NAME;
use adk_kernel::channels::{
    ChannelAnswer, ChannelBindingStore, ChannelGateway, ChannelHost, ChannelInbound,
    InMemoryChannel, InMemoryChannelBindingStore, InboundOutcome, accept_pairing,
};
use adk_kernel::conformance::ContractHost;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde_json::json;

const CHANNEL: &str = "chat";
const PERSON: &str = "ana";
const GROUP: &str = "operator";

struct Host {
    contract: ContractHost,
    approvals: Arc<dyn ApprovalService>,
}

#[async_trait]
impl ChannelHost for Host {
    fn app_name(&self) -> &str {
        self.contract.app_name()
    }

    fn now(&self) -> DateTime<Utc> {
        self.contract.now()
    }

    async fn turn(&self, _pid: &str) -> adk_kernel::Result<Option<RunRecord>> {
        Ok(None)
    }

    fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
        Some(self.approvals.clone())
    }

    fn audit_trail(&self) -> Option<AuditTrail> {
        self.contract.audit_trail()
    }

    async fn start_turn(&self, run: NewRun) -> adk_kernel::Result<String> {
        self.contract.start_turn(run).await
    }

    async fn finished_turns(&self) -> adk_kernel::Result<BoxStream<'static, RunRecord>> {
        self.contract.finished_turns().await
    }

    async fn stop_all(&self, reason: &str, by: AuditActor) -> adk_kernel::Result<()> {
        self.contract.stop_all(reason, by).await
    }
}

/// A suggestion of ADK-Rust's to add "Finance", at `risk`, as the agent table submits one.
async fn suggest(approvals: &dyn ApprovalService, risk: ApprovalRisk) -> ApprovalRequest {
    let definition = json!({ "displayName": "Finance", "area": "default", "mode": "ask" });
    let call = ToolConfirmationRequest::new(AGENT_CREATE_TOOL_NAME, definition.clone());
    let proposer = AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap();
    let request = ApprovalRequest::new(
        format!("agent-finance-{}", uuid::Uuid::new_v4().simple()),
        ApprovalScope::new("adk-rust", PERSON, "session-1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::human(PERSON), ApproverSelector::group(GROUP)]),
    )
    .with_redacted_args(definition)
    .with_requester(SubjectRef::agent(&proposer))
    .with_summary("Add an agent called Finance")
    .with_risk(risk)
    .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject]);
    approvals.submit(request).await.expect("submitted")
}

async fn paired() -> (ChannelGateway, Arc<InMemoryChannel>, Arc<dyn ApprovalService>) {
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let host = Arc::new(Host { contract: ContractHost::new(), approvals: approvals.clone() });
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let chat = Arc::new(InMemoryChannel::new(CHANNEL));
    let gateway = ChannelGateway::builder(host, bindings.clone())
        .sink(chat.clone())
        .home_url("http://localhost:8080/ui")
        .build();
    let hello = chat.text("ana-chat", "t-ana", "hello");
    assert_eq!(gateway.receive(hello).await.unwrap(), InboundOutcome::PairingRequested);
    let code = chat.pairing_code("t-ana").expect("a code");
    accept_pairing(bindings.as_ref(), &code, PERSON, &[GROUP.to_string()], Utc::now())
        .await
        .expect("paired");
    chat.clear("t-ana");
    (gateway, chat, approvals)
}

/// The card of a medium-risk suggestion offers "Don't add" only and links to the home; an
/// approval forged from it is refused and the suggestion still waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_medium_risk_suggestion_offers_dont_add_only() {
    let (gateway, chat, approvals) = paired().await;
    let suggestion = suggest(approvals.as_ref(), ApprovalRisk::Medium).await;
    gateway.ring(&suggestion).await.unwrap();
    let card = chat.last_card("t-ana").expect("the suggestion's card");
    assert_eq!(card.decision, suggestion.id);
    assert!(card.adds_agent);
    assert_eq!(card.answers, [ChannelAnswer::Deny]);
    assert_eq!(card.label(ChannelAnswer::Deny), "Don't add");
    assert!(card.text().contains("Open in home: http://localhost:8080/ui"), "{}", card.text());
    assert!(!card.summary.contains("Finance"), "chat carries no argument (S-I5): {}", card.summary);

    let forged = ChannelInbound::answer(
        CHANNEL,
        "ana-chat",
        "t-ana",
        "m-forged",
        &card,
        ChannelAnswer::AllowOnce,
    );
    assert!(matches!(
        gateway.receive(forged).await.unwrap(),
        InboundOutcome::DecisionRefused { .. }
    ));
    let still = approvals.get(&suggestion.id).await.unwrap().unwrap();
    assert_eq!(still.state, ApprovalState::Pending, "no chat message adds an agent");
}

/// A high-risk suggestion can still be refused from chat: "Don't add" is the Deny column.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_high_risk_suggestion_can_be_refused_from_chat() {
    let (gateway, chat, approvals) = paired().await;
    let suggestion = suggest(approvals.as_ref(), ApprovalRisk::High).await;
    gateway.ring(&suggestion).await.unwrap();
    let card = chat.last_card("t-ana").expect("the suggestion's card");
    assert_eq!(card.answers, [ChannelAnswer::Deny]);
    let deny =
        ChannelInbound::answer(CHANNEL, "ana-chat", "t-ana", "m-deny", &card, ChannelAnswer::Deny);
    assert!(matches!(gateway.receive(deny).await.unwrap(), InboundOutcome::Decided { .. }));
    let refused = approvals.get(&suggestion.id).await.unwrap().unwrap();
    assert_eq!(refused.state, ApprovalState::Rejected);
}

/// Every other card keeps its answers: an everyday medium-risk call still offers both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_cards_keep_their_answers() {
    let (gateway, chat, approvals) = paired().await;
    let call = ToolConfirmationRequest::new("send_email", json!({ "to": "client@example.com" }));
    let request = ApprovalRequest::new(
        "decision-email",
        ApprovalScope::new("adk-rust", PERSON, "session-1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group(GROUP)]).allow_self_approval(),
    )
    .with_summary("Send the invoice")
    .with_risk(ApprovalRisk::Medium);
    let request = approvals.submit(request).await.unwrap();
    gateway.ring(&request).await.unwrap();
    let card = chat.last_card("t-ana").expect("the card");
    assert!(!card.adds_agent);
    assert_eq!(card.answers, [ChannelAnswer::AllowOnce, ChannelAnswer::Deny]);
    assert_eq!(card.label(ChannelAnswer::Deny), "Deny");
}
