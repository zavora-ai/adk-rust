//! The channel gateway's decision rules and lifecycle (spec 3.3 §7.2, §7.4, §10 K1, §11
//! S-I2), on the contract suite's host with the in-memory chat:
//!
//! - chat never decides money: a money call's card carries no answers and a forged answer
//!   is refused, for the tools the gateway names and those a mounted driver declares
//!   `financial`;
//! - a stopped gateway started again still rings the doorbell's decisions, and a second
//!   start sends each reply once;
//! - a decision is carded once, whether the doorbell or `ring_waiting` rings it first;
//! - an answer that counts toward a quorum waits for the others, and never re-sends cards;
//! - only a person the policy asks gets a card, and an answer from anyone else is told so;
//! - concurrent messages from unpaired accounts keep the waiting limit and one code per
//!   account.

#![cfg(all(feature = "channels", feature = "conformance"))]

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, InMemoryApprovalStore, NewRun, RunRecord,
    StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::channels::{
    ChannelAnswer, ChannelBinding, ChannelBindingStore, ChannelDoorbell, ChannelGateway,
    ChannelHost, ChannelInbound, ChannelNotice, ChannelPairingRequest, Delivered, InMemoryChannel,
    InMemoryChannelBindingStore, InboundOutcome, accept_pairing,
};
use adk_kernel::conformance::ContractHost;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde_json::json;

const CHANNEL: &str = "chat";
const PERSON: &str = "ana";
const GROUP: &str = "operator";

/// The contract host with an approval service of the test's own, so a test chooses its
/// notifier and its policies.
struct Host {
    contract: ContractHost,
    approvals: Arc<dyn ApprovalService>,
}

impl Host {
    fn new(doorbell: Option<Arc<ChannelDoorbell>>) -> Arc<Self> {
        let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()));
        let service = match doorbell {
            Some(doorbell) => service.with_notifier(doorbell),
            None => service,
        };
        Arc::new(Self { contract: ContractHost::new(), approvals: Arc::new(service) })
    }

    /// Submits a decision of the person calling `tool` at `risk` under `policy`.
    async fn ask(&self, tool: &str, risk: ApprovalRisk, policy: ApprovalPolicy) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new(tool, json!({ "to": "client@example.com" }));
        let request = ApprovalRequest::new(
            format!("decision-{}", uuid::Uuid::new_v4().simple()),
            ApprovalScope::new("adk-rust", PERSON, "session-1"),
            &call,
            policy,
        )
        .with_summary(format!("Call {tool}"))
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "Finance")));
        self.approvals.submit(request).await.expect("submitted")
    }
}

#[async_trait]
impl ChannelHost for Host {
    fn app_name(&self) -> &str {
        self.contract.app_name()
    }

    fn now(&self) -> DateTime<Utc> {
        self.contract.now()
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

fn operators() -> ApprovalPolicy {
    ApprovalPolicy::new([ApproverSelector::group(GROUP)]).allow_self_approval()
}

/// A gateway over `host` and the in-memory chat, the person's account paired in `t-ana`.
async fn paired(
    host: Arc<Host>,
    bindings: Arc<dyn ChannelBindingStore>,
    build: impl FnOnce(
        adk_kernel::channels::ChannelGatewayBuilder,
    ) -> adk_kernel::channels::ChannelGatewayBuilder,
) -> (ChannelGateway, Arc<InMemoryChannel>) {
    let chat = Arc::new(InMemoryChannel::new(CHANNEL));
    let gateway = build(ChannelGateway::builder(host, bindings.clone()).sink(chat.clone())).build();
    let hello = chat.text("ana-chat", "t-ana", "hello");
    assert_eq!(gateway.receive(hello).await.unwrap(), InboundOutcome::PairingRequested);
    let code = chat.pairing_code("t-ana").expect("a code");
    accept_pairing(bindings.as_ref(), &code, PERSON, &[GROUP.to_string()], Utc::now())
        .await
        .expect("paired");
    chat.clear("t-ana");
    (gateway, chat)
}

/// The cards delivered to `thread` for decision `id`.
fn cards_for(chat: &InMemoryChannel, thread: &str, id: &str) -> usize {
    chat.delivered(thread)
        .iter()
        .filter(
            |delivered| matches!(delivered, Delivered::Decision { card } if card.decision == id),
        )
        .count()
}

/// Polls `probe` every 10 ms for at most three seconds.
async fn eventually(what: &str, mut probe: impl FnMut() -> bool) {
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A call that moves money is carded with no answers, and an answer forged
/// from an everyday card is refused; the decision still waits for the home.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_never_decides_money() {
    let host = Host::new(None);
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let (gateway, chat) =
        paired(host.clone(), bindings, |builder| builder.money_tools(["transfer"])).await;

    let everyday = host.ask("send_email", ApprovalRisk::Medium, operators()).await;
    gateway.ring(&everyday).await.unwrap();
    let card = chat.last_card("t-ana").expect("the everyday card");
    assert_eq!(card.answers, [ChannelAnswer::AllowOnce, ChannelAnswer::Deny]);

    for tool in ["transfer", "payments_checkout_complete"] {
        let money = host.ask(tool, ApprovalRisk::Medium, operators()).await;
        gateway.ring(&money).await.unwrap();
        let card = chat.last_card("t-ana").expect("the money card");
        assert_eq!(card.decision, money.id);
        assert!(card.answers.is_empty(), "{tool}: chat offers no answer to money");
        assert!(card.text().contains("Decide in the home"), "{}", card.text());
        // An adapter that forges an answer from the money card is refused.
        let forged = ChannelInbound::answer(
            CHANNEL,
            "ana-chat",
            "t-ana",
            format!("m-{tool}"),
            &card,
            ChannelAnswer::AllowOnce,
        );
        assert!(matches!(
            gateway.receive(forged).await.unwrap(),
            InboundOutcome::DecisionRefused { .. }
        ));
        let now = host.approvals.get(&money.id).await.unwrap().unwrap();
        assert_eq!(now.state, ApprovalState::Pending, "{tool} still waits for the home");
    }
}

/// On the kernel, a tool a mounted driver declares `financial` moves money.
#[cfg(feature = "drivers")]
#[tokio::test]
async fn a_financial_driver_tool_moves_money_on_the_kernel() {
    use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
    use adk_kernel::stores::InMemoryDriverStore;

    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
    let manifest = DriverManifest::from_toml(
        r#"
        id        = "bank"
        command   = "mcp-bank"
        lifecycle = "lazy"

        [tools]
        balance  = "read"
        bank_pay = "financial"
        "#,
    )
    .unwrap();
    drivers.mount(manifest, PERSON).await.unwrap();
    let kernel = adk_kernel::Kernel::builder()
        .config(adk_kernel::KernelConfig::default().with_app_name("adk-rust"))
        .run_store(Arc::new(adk_core::InMemoryRunStore::new()))
        .drivers(drivers)
        .build()
        .unwrap();
    let request = |tool: &str| {
        ApprovalRequest::new(
            "d1",
            ApprovalScope::new("adk-rust", PERSON, "s1"),
            &ToolConfirmationRequest::new(tool, json!({})),
            operators(),
        )
    };
    assert!(ChannelHost::moves_money(&kernel, &request("bank_pay")));
    assert!(!ChannelHost::moves_money(&kernel, &request("balance")));
    assert!(!ChannelHost::moves_money(&kernel, &request("send_email")));
}

/// A gateway stopped and started again still rings the doorbell, and a
/// second start sends each reply once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_gateway_rings_and_a_second_start_replies_once() {
    let doorbell = Arc::new(ChannelDoorbell::new());
    let host = Host::new(Some(doorbell.clone()));
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let (gateway, chat) =
        paired(host.clone(), bindings, |builder| builder.doorbell(doorbell.clone())).await;
    gateway.start().await.unwrap();
    gateway.stop();
    gateway.start().await.unwrap();
    gateway.start().await.unwrap();

    let request = host.ask("send_email", ApprovalRisk::Low, operators()).await;
    eventually("the card after a restart", || cards_for(&chat, "t-ana", &request.id) == 1).await;

    let message = chat.text("ana-chat", "t-ana", "what is on today?");
    let InboundOutcome::TurnStarted { pid } = gateway.receive(message).await.unwrap() else {
        panic!("the message starts a turn");
    };
    host.contract.finish(&pid, "Two meetings.");
    eventually("the reply", || chat.messages("t-ana").contains(&"Two meetings.".to_string())).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let replies =
        chat.messages("t-ana").iter().filter(|text| text.as_str() == "Two meetings.").count();
    assert_eq!(replies, 1, "a second start sends no second reply");
}

/// A decision the doorbell queued before the gateway started, and
/// `ring_waiting` found after, is carded once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decision_queued_before_start_is_carded_once() {
    let doorbell = Arc::new(ChannelDoorbell::new());
    let host = Host::new(Some(doorbell.clone()));
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let (gateway, chat) =
        paired(host.clone(), bindings, |builder| builder.doorbell(doorbell.clone())).await;
    let request = host.ask("send_email", ApprovalRisk::Low, operators()).await;
    gateway.start().await.unwrap();
    gateway.ring_waiting().await.unwrap();
    eventually("the card", || cards_for(&chat, "t-ana", &request.id) >= 1).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(cards_for(&chat, "t-ana", &request.id), 1, "{}", chat.wire("t-ana"));
}

/// An answer that counts toward a quorum says the decision waits for
/// someone else; a stale card gets one fresh card, and pressing it again sends no other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quorum_answer_waits_for_the_others() {
    let host = Host::new(None);
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let (gateway, chat) = paired(host.clone(), bindings, |builder| builder).await;
    let request = host.ask("send_email", ApprovalRisk::Low, operators().with_quorum(2)).await;
    gateway.ring(&request).await.unwrap();
    let card = chat.last_card("t-ana").unwrap();

    let press = chat.press("ana-chat", "t-ana", &card, ChannelAnswer::AllowOnce).unwrap();
    gateway.receive(press).await.unwrap();
    assert_eq!(
        chat.notices("t-ana").last(),
        Some(&ChannelNotice::WaitingForOthers { answered: true }),
        "the answer counted, nothing was allowed yet"
    );
    assert_eq!(
        host.approvals.get(&request.id).await.unwrap().unwrap().state,
        ApprovalState::Pending
    );

    // The old card's version is stale: one fresh card.
    let stale = chat.press("ana-chat", "t-ana", &card, ChannelAnswer::AllowOnce).unwrap();
    assert!(matches!(
        gateway.receive(stale).await.unwrap(),
        InboundOutcome::DecisionChanged { .. }
    ));
    let fresh = chat.last_card("t-ana").unwrap();
    assert_ne!(fresh.version, card.version);
    // Pressing the fresh card again counts nothing new and sends no other card.
    let again = chat.press("ana-chat", "t-ana", &fresh, ChannelAnswer::AllowOnce).unwrap();
    gateway.receive(again).await.unwrap();
    assert_eq!(
        chat.notices("t-ana").last(),
        Some(&ChannelNotice::WaitingForOthers { answered: true })
    );
    assert_eq!(cards_for(&chat, "t-ana", &request.id), 2, "{}", chat.wire("t-ana"));
}

/// A decision the policy does not let the person answer is carded on
/// neither path, and an answer to it is told it waits for someone else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_who_may_answer_gets_a_card() {
    let host = Host::new(None);
    let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
    let (gateway, chat) = paired(host.clone(), bindings, |builder| builder).await;
    // The person's own call, routed to the operators, who may not approve their own.
    let call = ToolConfirmationRequest::new("send_email", json!({}));
    let mut request = ApprovalRequest::new(
        "own-call",
        ApprovalScope::new("adk-rust", PERSON, "session-1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group(GROUP)]),
    )
    .with_summary("Send the invoice")
    .with_risk(ApprovalRisk::Low);
    request.requester = Some(SubjectRef::human(PERSON));
    let request = host.approvals.submit(request).await.unwrap();

    assert_eq!(gateway.ring(&request).await.unwrap(), 0, "the doorbell sends no card");
    assert_eq!(gateway.ring_waiting().await.unwrap(), 0, "nor does a restart");
    assert_eq!(cards_for(&chat, "t-ana", &request.id), 0);

    let card = adk_kernel::channels::DecisionCard::from_request(&request, None);
    let forged = ChannelInbound::answer(
        CHANNEL,
        "ana-chat",
        "t-ana",
        "m-own",
        &card,
        ChannelAnswer::AllowOnce,
    );
    gateway.receive(forged).await.unwrap();
    assert_eq!(
        chat.notices("t-ana").last(),
        Some(&ChannelNotice::WaitingForOthers { answered: false }),
        "not \"no longer waiting\": it still waits"
    );
}

/// A binding store whose request listing is slow, so concurrent unpaired messages overlap.
struct SlowRequests(InMemoryChannelBindingStore);

#[async_trait]
impl ChannelBindingStore for SlowRequests {
    async fn put_binding(&self, binding: ChannelBinding) -> adk_kernel::Result<()> {
        self.0.put_binding(binding).await
    }
    async fn get_binding(&self, id: &str) -> adk_kernel::Result<Option<ChannelBinding>> {
        self.0.get_binding(id).await
    }
    async fn list_bindings(&self) -> adk_kernel::Result<Vec<ChannelBinding>> {
        self.0.list_bindings().await
    }
    async fn put_request(&self, request: ChannelPairingRequest) -> adk_kernel::Result<()> {
        self.0.put_request(request).await
    }
    async fn get_request(
        &self,
        channel: &str,
        account: &str,
    ) -> adk_kernel::Result<Option<ChannelPairingRequest>> {
        self.0.get_request(channel, account).await
    }
    async fn take_request(
        &self,
        code_hash: &str,
    ) -> adk_kernel::Result<Option<ChannelPairingRequest>> {
        self.0.take_request(code_hash).await
    }
    async fn list_requests(&self) -> adk_kernel::Result<Vec<ChannelPairingRequest>> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.0.list_requests().await
    }
    async fn prune_requests(&self, before: DateTime<Utc>) -> adk_kernel::Result<usize> {
        self.0.prune_requests(before).await
    }
}

/// Unpaired accounts writing at once keep the waiting limit, and one
/// account writing twice at once gets one code.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_unpaired_messages_keep_the_limits() {
    let host = Host::new(None);
    let bindings: Arc<dyn ChannelBindingStore> =
        Arc::new(SlowRequests(InMemoryChannelBindingStore::new()));
    let chat = Arc::new(InMemoryChannel::new(CHANNEL));
    let gateway =
        ChannelGateway::builder(host, bindings.clone()).sink(chat.clone()).max_waiting(2).build();
    let mut writes = Vec::new();
    for index in 0..8 {
        let (gateway, chat) = (gateway.clone(), chat.clone());
        writes.push(tokio::spawn(async move {
            let hello = chat.text(&format!("stranger-{index}"), &format!("t-{index}"), "hi");
            gateway.receive(hello).await.unwrap()
        }));
    }
    for write in writes {
        write.await.unwrap();
    }
    assert_eq!(bindings.list_requests().await.unwrap().len(), 2, "at most two wait");

    // A message sent twice at once, as a redelivered webhook is.
    let host = Host::new(None);
    let bindings: Arc<dyn ChannelBindingStore> =
        Arc::new(SlowRequests(InMemoryChannelBindingStore::new()));
    let chat = Arc::new(InMemoryChannel::new(CHANNEL));
    let gateway = ChannelGateway::builder(host, bindings.clone()).sink(chat.clone()).build();
    let mut twice = Vec::new();
    for _ in 0..2 {
        let (gateway, chat) = (gateway.clone(), chat.clone());
        twice.push(tokio::spawn(async move {
            gateway.receive(chat.text("stranger", "t-s", "hello?")).await.unwrap()
        }));
    }
    let mut outcomes = Vec::new();
    for write in twice {
        outcomes.push(write.await.unwrap());
    }
    assert!(outcomes.contains(&InboundOutcome::PairingRequested), "{outcomes:?}");
    assert!(outcomes.contains(&InboundOutcome::PairingWaiting), "{outcomes:?}");
    let codes = chat
        .notices("t-s")
        .iter()
        .filter(|notice| matches!(notice, ChannelNotice::PairingCode { .. }))
        .count();
    assert_eq!(codes, 1, "one code for the account");
}
