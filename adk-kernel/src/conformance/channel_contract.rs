//! The contract every chat channel adapter passes (spec 3.3 §5.3, §7.2, §10 K1, §11 S-I2,
//! S-I5, S-I6, S-I10; feature `channels`).
//!
//! Each case runs the adapter under a [`ChannelGateway`] over a [`ContractHost`], an
//! in-memory kernel double, and checks what the adapter put on the wire and what it made of
//! the person's input:
//!
//! | Case | Contract |
//! |------|----------|
//! | [`an_unpaired_account_gets_a_code_and_starts_nothing`] | An unpaired account's message gets a pairing code in its thread and starts no turn (S-I6) |
//! | [`a_paired_message_starts_a_turn_and_the_reply_returns_to_its_thread`] | After its code is accepted the account's message starts a turn as its person, authenticated, and the reply comes back to the same thread |
//! | [`a_redelivered_message_starts_one_turn`] | The same platform message delivered twice starts one turn (K-I3) |
//! | [`decision_cards_carry_no_arguments`] | A card reaches the person's thread with its agent, summary and risk; no argument value ever leaves the machine (S-I5) |
//! | [`allow_once_decides_once_and_a_second_answer_is_already_decided`] | "Allow once" approves at the card's version; a second press is answered "decided already" and decides nothing (S-I2) |
//! | [`deny_rejects`] | "Deny" rejects |
//! | [`high_risk_offers_no_answer_and_a_forged_one_is_refused`] | A high-risk card offers no button, and an answer forged for it decides nothing (§7.2) |
//! | [`stop_all_stops_and_resume_is_refused`] | "stop all" stops everything; "resume" is refused and everything stays stopped (S-I10) |
//! | [`a_stopped_kernel_starts_no_turn`] | While everything is stopped a message starts nothing and says so |
//! | [`an_unpaired_account_cannot_answer`] | An answer from an unpaired account decides nothing and gets a pairing code |
//! | [`a_restart_rings_what_still_waits`] | After a restart, every decision that still waits is rung again, and only those |
//! | [`quiet_time_holds_cards`] | A card for a decision asked in quiet time arrives when quiet time ends |
//!
//! Generate the tests with [`channel_contract_tests!`](crate::channel_contract_tests) and a
//! [`ChannelAdapterHarness`] that makes a fresh adapter per case.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, GovernanceControl, InMemoryApprovalStore,
    NewRun, RunRecord, RunState, StoreApprovalService, ToolConfirmationRequest,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::{BoxStream, StreamExt};
use serde_json::json;
use tokio::sync::broadcast;

use crate::channels::{
    ChannelAnswer, ChannelBindingStore, ChannelGateway, ChannelHost, ChannelInbound, ChannelSink,
    InMemoryChannel, InMemoryChannelBindingStore, InboundOutcome, accept_pairing,
};
use crate::error::{KernelError, Result};

/// The person every paired account in the suite acts for.
const PERSON: &str = "ana";
/// The approval group decisions route to, as `adk-rust up` routes them.
const GROUP: &str = "operator";

/// A chat adapter as the suite drives it (spec 3.3 §10 K1): its [`ChannelSink`], what it
/// put on the wire, and the [`ChannelInbound`] values it makes of the person's input.
#[async_trait]
pub trait ChannelAdapter: ChannelSink {
    /// Everything the adapter put on the wire to `thread`, in order, as text: what left the
    /// machine.
    async fn wire(&self, thread: &str) -> Vec<String>;

    /// What the adapter makes of `account` typing `text` in `thread`, as a new message.
    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound;

    /// What the adapter makes of `account` pressing `answer` on the last card it sent to
    /// `thread`, or `None` when that card offers no such button.
    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound>;

    /// The pairing code the adapter delivered to `thread`, as the person reads it.
    async fn pairing_code(&self, thread: &str) -> Option<String>;
}

/// Makes a fresh adapter for each case.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::channels::InMemoryChannel;
/// use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl ChannelAdapterHarness for InMemory {
///     async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
///         Arc::new(InMemoryChannel::new("chat"))
///     }
/// }
/// ```
#[async_trait]
pub trait ChannelAdapterHarness: Send + Sync {
    /// A fresh adapter, with nothing sent.
    async fn adapter(&self) -> Arc<dyn ChannelAdapter>;
}

#[async_trait]
impl ChannelAdapter for InMemoryChannel {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.wire(thread).lines().map(str::to_string).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        self.text(account, thread, text)
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let card = self.last_card(thread)?;
        self.press(account, thread, &card, answer)
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        InMemoryChannel::pairing_code(self, thread)
    }
}

/// A kernel double the suite runs the gateway on (spec 3.3 §10 K1): a real approval service
/// and kill switch, turns kept in memory and finished on demand, and quiet time set by the
/// test.
pub struct ContractHost {
    approvals: Arc<dyn ApprovalService>,
    governance: GovernanceControl,
    recorder: InMemoryAuditRecorder,
    turns: Mutex<BTreeMap<String, NewRun>>,
    finished: broadcast::Sender<RunRecord>,
    quiet_until: Mutex<Option<DateTime<Utc>>>,
}

impl std::fmt::Debug for ContractHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContractHost").finish_non_exhaustive()
    }
}

impl Default for ContractHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractHost {
    /// A host with nothing started and nothing stopped.
    pub fn new() -> Self {
        Self {
            approvals: Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))),
            governance: GovernanceControl::new(),
            recorder: InMemoryAuditRecorder::new(),
            turns: Mutex::default(),
            finished: broadcast::channel(64).0,
            quiet_until: Mutex::default(),
        }
    }

    /// The turns started so far: process id and run.
    pub fn turns(&self) -> Vec<(String, NewRun)> {
        let turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        turns.iter().map(|(pid, run)| (pid.clone(), run.clone())).collect()
    }

    /// Finishes turn `pid` with the agent's reply `text`.
    ///
    /// # Panics
    ///
    /// When no turn has that id.
    pub fn finish(&self, pid: &str, text: &str) {
        let run = self.turns.lock().unwrap_or_else(PoisonError::into_inner)[pid].clone();
        let mut record = RunRecord::queued(pid, run, Utc::now());
        record.state = RunState::Completed;
        record.result = Some(json!({ "text": text }));
        let _ = self.finished.send(record);
    }

    /// Holds cards until `until`, or lifts quiet time with `None`.
    pub fn set_quiet_until(&self, until: Option<DateTime<Utc>>) {
        *self.quiet_until.lock().unwrap_or_else(PoisonError::into_inner) = until;
    }

    /// Whether everything is stopped.
    pub fn is_stopped(&self) -> bool {
        self.governance.is_frozen()
    }

    /// The `origin` of every `kernel.channel` record written so far.
    pub fn channel_origins(&self) -> Vec<serde_json::Value> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == crate::audit::CHANNEL)
            .filter_map(|record| record.ext.get("origin").cloned())
            .collect()
    }

    /// The `kernel.channel` records written so far, as their actions.
    pub fn channel_actions(&self) -> Vec<String> {
        self.recorder
            .records()
            .into_iter()
            .filter(|record| record.action == crate::audit::CHANNEL)
            .filter_map(|record| {
                record.ext.get("action").and_then(|v| v.as_str()).map(str::to_string)
            })
            .collect()
    }

    /// Submits a decision for `PERSON` calling `tool` with `args` at `risk`, as a held call
    /// would, and returns it.
    ///
    /// # Panics
    ///
    /// When the approval service refuses it.
    pub async fn ask(
        &self,
        summary: &str,
        args: serde_json::Value,
        risk: ApprovalRisk,
    ) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new("send_email", args);
        let id = format!("decision-{}", uuid::Uuid::new_v4().simple());
        let request = ApprovalRequest::new(
            id,
            ApprovalScope::new("adk-rust", PERSON, "session-1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group(GROUP)]).allow_self_approval(),
        )
        .with_summary(summary)
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "Finance")));
        self.approvals.submit(request).await.expect("the approval service takes the decision")
    }

    /// The decision `id` now.
    ///
    /// # Panics
    ///
    /// When the approval service fails.
    pub async fn decision(&self, id: &str) -> ApprovalRequest {
        self.approvals.get(id).await.expect("read").expect("the decision exists")
    }
}

#[async_trait]
impl ChannelHost for ContractHost {
    fn app_name(&self) -> &str {
        "adk-rust"
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn quiet_until(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let until = *self.quiet_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.filter(|until| *until > at)
    }

    fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
        Some(self.approvals.clone())
    }

    fn audit_trail(&self) -> Option<AuditTrail> {
        Some(AuditTrail::new(Arc::new(self.recorder.clone())))
    }

    async fn start_turn(&self, run: NewRun) -> Result<String> {
        if self.governance.is_frozen() {
            return Err(KernelError::Frozen {
                reason: self.governance.reason().unwrap_or_default(),
            });
        }
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((pid, _)) =
            turns.iter().find(|(_, started)| started.idempotency_key == run.idempotency_key)
        {
            return Ok(pid.clone());
        }
        let pid = format!("turn-{}", turns.len() + 1);
        turns.insert(pid.clone(), run);
        Ok(pid)
    }

    async fn finished_turns(&self) -> Result<BoxStream<'static, RunRecord>> {
        let receiver = self.finished.subscribe();
        Ok(futures::stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(record) => return Some((record, receiver)),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed())
    }

    async fn stop_all(&self, reason: &str, by: AuditActor) -> Result<()> {
        self.governance.freeze_recorded(reason, by).await.map_err(KernelError::from)
    }
}

/// One case's world: the adapter, the host, the binding store and the gateway over them.
struct World {
    adapter: Arc<dyn ChannelAdapter>,
    host: Arc<ContractHost>,
    bindings: Arc<dyn ChannelBindingStore>,
    gateway: ChannelGateway,
}

impl World {
    async fn new(harness: &dyn ChannelAdapterHarness) -> Self {
        let adapter = harness.adapter().await;
        let host = Arc::new(ContractHost::new());
        let bindings: Arc<dyn ChannelBindingStore> = Arc::new(InMemoryChannelBindingStore::new());
        let sink: Arc<dyn ChannelSink> = adapter.clone();
        let gateway = ChannelGateway::builder(host.clone(), bindings.clone())
            .sink(sink)
            .home_url("https://home.example")
            .build();
        gateway.start().await.expect("the gateway starts");
        Self { adapter, host, bindings, gateway }
    }

    async fn receive(&self, inbound: ChannelInbound) -> InboundOutcome {
        self.gateway.receive(inbound).await.expect("the gateway takes the message")
    }

    /// Pairs `account` in `thread` to the person, as the person accepting its code does.
    async fn pair(&self, account: &str, thread: &str) {
        let hello = self.adapter.typed(account, thread, "hello").await;
        assert_eq!(self.receive(hello).await, InboundOutcome::PairingRequested);
        let code = self.adapter.pairing_code(thread).await.expect("the adapter delivered a code");
        accept_pairing(self.bindings.as_ref(), &code, PERSON, &[GROUP.to_string()], Utc::now())
            .await
            .expect("the code pairs the account");
    }

    async fn wire(&self, thread: &str) -> String {
        self.adapter.wire(thread).await.join("\n")
    }
}

/// Polls `probe` every 10 ms for at most five seconds.
async fn eventually<F, Fut>(what: &str, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if probe().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// An unpaired account's message gets a pairing code in its thread and starts no turn.
pub async fn an_unpaired_account_gets_a_code_and_starts_nothing(
    harness: &dyn ChannelAdapterHarness,
) {
    let world = World::new(harness).await;
    let inbound = world.adapter.typed("stranger", "t-stranger", "run rm -rf /").await;
    assert_eq!(world.receive(inbound).await, InboundOutcome::PairingRequested);
    assert!(
        world.adapter.pairing_code("t-stranger").await.is_some(),
        "the adapter showed the code"
    );
    assert!(world.host.turns().is_empty(), "nothing started");
    let again = world.adapter.typed("stranger", "t-stranger", "hello?").await;
    assert_eq!(world.receive(again).await, InboundOutcome::PairingWaiting);
    assert!(world.host.turns().is_empty(), "still nothing started");
    assert!(world.host.channel_actions().contains(&"pairing.requested".to_string()));
}

/// After its code is accepted the account's message starts a turn as its person and the
/// reply comes back to the same thread.
pub async fn a_paired_message_starts_a_turn_and_the_reply_returns_to_its_thread(
    harness: &dyn ChannelAdapterHarness,
) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let message = world.adapter.typed("ana-chat", "t-ana", "what's on today?").await;
    let InboundOutcome::TurnStarted { pid } = world.receive(message).await else {
        panic!("a paired message starts a turn");
    };
    let (_, run) = world.host.turns().into_iter().find(|(id, _)| *id == pid).expect("the turn");
    assert_eq!(run.user_id, PERSON);
    assert!(run.authenticated, "the pairing authenticates the person");
    assert_eq!(run.principal, None, "a person's turn, not a service's");
    assert_eq!(run.input["text"], "what's on today?");
    world.host.finish(&pid, "Two meetings and the invoice.");
    let adapter = world.adapter.clone();
    eventually("the reply in the thread", || {
        let adapter = adapter.clone();
        async move { adapter.wire("t-ana").await.join("\n").contains("Two meetings and the invoice.") }
    })
    .await;
    assert!(world.adapter.wire("t-other").await.is_empty(), "no other thread hears it");
}

/// The same platform message delivered twice starts one turn.
pub async fn a_redelivered_message_starts_one_turn(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let message = world.adapter.typed("ana-chat", "t-ana", "draft the reply").await;
    let first = world.receive(message.clone()).await;
    let second = world.receive(message).await;
    assert_eq!(first, second);
    assert_eq!(world.host.turns().len(), 1);
}

/// A card reaches the person's thread with its agent, summary and risk; no argument value
/// leaves the machine.
pub async fn decision_cards_carry_no_arguments(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let canary = "canary-7f3a@client.example";
    let request = world
        .host
        .ask(
            &format!("Send the October invoice to {canary}"),
            json!({"to": canary, "body": "secret-body-91c2", "amount": 4200}),
            ApprovalRisk::Medium,
        )
        .await;
    assert_eq!(world.gateway.ring(&request).await.expect("ring"), 1);
    let wire = world.wire("t-ana").await;
    assert!(wire.contains("Finance"), "the agent is named: {wire}");
    assert!(wire.contains("Send the October invoice to"), "the summary is shown: {wire}");
    for argument in [canary, "secret-body-91c2"] {
        assert!(!wire.contains(argument), "an argument left the machine: {argument} in {wire}");
    }
    assert!(world.adapter.pressed("ana-chat", "t-ana", ChannelAnswer::AllowOnce).await.is_some());
    assert!(world.adapter.pressed("ana-chat", "t-ana", ChannelAnswer::Deny).await.is_some());
    assert!(world.host.channel_actions().contains(&"notify".to_string()));
}

/// "Allow once" approves at the card's version; a second press is answered "decided
/// already" and decides nothing.
pub async fn allow_once_decides_once_and_a_second_answer_is_already_decided(
    harness: &dyn ChannelAdapterHarness,
) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let request = world.host.ask("Send the invoice", json!({}), ApprovalRisk::Low).await;
    world.gateway.ring(&request).await.expect("ring");
    let press = world
        .adapter
        .pressed("ana-chat", "t-ana", ChannelAnswer::AllowOnce)
        .await
        .expect("a button");
    assert!(matches!(world.receive(press.clone()).await, InboundOutcome::Decided { .. }));
    let decided = world.host.decision(&request.id).await;
    assert_eq!(decided.state, ApprovalState::Approved);
    assert_eq!(decided.decisions.len(), 1);
    assert_eq!(decided.decisions[0].approver.id, PERSON);
    let mut again = press;
    again.message_id.push_str("-again");
    assert!(matches!(world.receive(again).await, InboundOutcome::AlreadyDecided { .. }));
    assert_eq!(world.host.decision(&request.id).await.decisions.len(), 1, "decided once");
    assert!(world.wire("t-ana").await.to_lowercase().contains("already"));
}

/// "Deny" rejects.
pub async fn deny_rejects(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let request = world.host.ask("Post the update", json!({}), ApprovalRisk::Medium).await;
    world.gateway.ring(&request).await.expect("ring");
    let press =
        world.adapter.pressed("ana-chat", "t-ana", ChannelAnswer::Deny).await.expect("a button");
    assert!(matches!(world.receive(press).await, InboundOutcome::Decided { .. }));
    assert_eq!(world.host.decision(&request.id).await.state, ApprovalState::Rejected);
}

/// A high-risk card offers no button, and an answer forged for it decides nothing.
pub async fn high_risk_offers_no_answer_and_a_forged_one_is_refused(
    harness: &dyn ChannelAdapterHarness,
) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    for risk in [ApprovalRisk::High, ApprovalRisk::Critical] {
        let request = world.host.ask("Pay the supplier", json!({}), risk).await;
        world.gateway.ring(&request).await.expect("ring");
        assert!(
            world.adapter.pressed("ana-chat", "t-ana", ChannelAnswer::AllowOnce).await.is_none()
        );
        assert!(world.wire("t-ana").await.contains("https://home.example"), "it links to the home");
        let forged = ChannelInbound::answer(
            world.adapter.channel(),
            "ana-chat",
            "t-ana",
            format!("forged-{risk:?}"),
            &crate::channels::DecisionCard::from_request(&request, None),
            ChannelAnswer::AllowOnce,
        );
        assert!(matches!(world.receive(forged).await, InboundOutcome::DecisionRefused { .. }));
        assert_eq!(world.host.decision(&request.id).await.state, ApprovalState::Pending);
    }
}

/// "stop all" stops everything; "resume" is refused and everything stays stopped.
pub async fn stop_all_stops_and_resume_is_refused(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let stop = world.adapter.typed("ana-chat", "t-ana", "stop all").await;
    assert_eq!(world.receive(stop).await, InboundOutcome::Stopped);
    assert!(world.host.is_stopped());
    let resume = world.adapter.typed("ana-chat", "t-ana", "resume").await;
    assert_eq!(world.receive(resume).await, InboundOutcome::ResumeRefused);
    assert!(world.host.is_stopped(), "a chat thread cannot resume");
    let actions = world.host.channel_actions();
    assert!(
        actions.contains(&"stop".to_string()) && actions.contains(&"resume.refused".to_string())
    );
    let origin = json!({ "kind": "chat", "channel": world.adapter.channel() });
    let origins = world.host.channel_origins();
    assert!(
        !origins.is_empty() && origins.iter().all(|recorded| *recorded == origin),
        "{origins:?}"
    );
}

/// While everything is stopped a message starts nothing and says so.
pub async fn a_stopped_kernel_starts_no_turn(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    world.host.stop_all("test", AuditActor::default()).await.expect("stop");
    let message = world.adapter.typed("ana-chat", "t-ana", "are you there?").await;
    assert_eq!(world.receive(message).await, InboundOutcome::TurnRefused);
    assert!(world.host.turns().is_empty());
    assert!(world.wire("t-ana").await.contains("stopped"));
}

/// An answer from an unpaired account decides nothing and gets a pairing code.
pub async fn an_unpaired_account_cannot_answer(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let request = world.host.ask("Send the invoice", json!({}), ApprovalRisk::Low).await;
    world.gateway.ring(&request).await.expect("ring");
    let card = crate::channels::DecisionCard::from_request(&request, None);
    let forged = ChannelInbound::answer(
        world.adapter.channel(),
        "mallory",
        "t-mallory",
        "forged-1",
        &card,
        ChannelAnswer::AllowOnce,
    );
    assert_eq!(world.receive(forged).await, InboundOutcome::PairingRequested);
    assert_eq!(world.host.decision(&request.id).await.state, ApprovalState::Pending);
}

/// After a restart, a decision that still waits is rung again on the person's thread.
pub async fn a_restart_rings_what_still_waits(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let waiting = world.host.ask("Send the invoice", json!({}), ApprovalRisk::Low).await;
    let decided = world.host.ask("Post the update", json!({}), ApprovalRisk::Low).await;
    world.gateway.ring(&decided).await.expect("ring");
    let press =
        world.adapter.pressed("ana-chat", "t-ana", ChannelAnswer::Deny).await.expect("a button");
    assert!(matches!(world.receive(press).await, InboundOutcome::Decided { .. }));
    assert_eq!(
        world.gateway.ring_waiting().await.expect("ring waiting"),
        1,
        "only what still waits"
    );
    let card = crate::channels::DecisionCard::from_request(&waiting, None);
    let wire = world.wire("t-ana").await;
    assert!(wire.contains(&card.decision), "the waiting decision was rung: {wire}");
}

/// A card for a decision asked in quiet time arrives when quiet time ends.
pub async fn quiet_time_holds_cards(harness: &dyn ChannelAdapterHarness) {
    let world = World::new(harness).await;
    world.pair("ana-chat", "t-ana").await;
    let before = world.adapter.wire("t-ana").await.len();
    world.host.set_quiet_until(Some(Utc::now() + chrono::Duration::milliseconds(300)));
    let request = world.host.ask("Send the invoice", json!({}), ApprovalRisk::Low).await;
    assert_eq!(world.gateway.ring(&request).await.expect("ring"), 0, "held during quiet time");
    assert_eq!(world.adapter.wire("t-ana").await.len(), before, "nothing sent during quiet time");
    let adapter = world.adapter.clone();
    eventually("the held card", || {
        let adapter = adapter.clone();
        async move { adapter.pressed("ana-chat", "t-ana", ChannelAnswer::AllowOnce).await.is_some() }
    })
    .await;
}

/// Generates one test per [`channel_contract`](self) case for a `ChannelAdapterHarness` (spec 3.3 §10 K1; feature `channels`).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[cfg(feature = "channels")]
#[macro_export]
macro_rules! channel_contract_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_an_unpaired_account_gets_a_code_and_starts_nothing() {
            $crate::conformance::channel_contract::an_unpaired_account_gets_a_code_and_starts_nothing(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_a_paired_message_starts_a_turn_and_the_reply_returns_to_its_thread() {
            $crate::conformance::channel_contract::a_paired_message_starts_a_turn_and_the_reply_returns_to_its_thread(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_a_redelivered_message_starts_one_turn() {
            $crate::conformance::channel_contract::a_redelivered_message_starts_one_turn(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_decision_cards_carry_no_arguments() {
            $crate::conformance::channel_contract::decision_cards_carry_no_arguments(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_allow_once_decides_once_and_a_second_answer_is_already_decided() {
            $crate::conformance::channel_contract::allow_once_decides_once_and_a_second_answer_is_already_decided(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_deny_rejects() {
            $crate::conformance::channel_contract::deny_rejects(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_high_risk_offers_no_answer_and_a_forged_one_is_refused() {
            $crate::conformance::channel_contract::high_risk_offers_no_answer_and_a_forged_one_is_refused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_stop_all_stops_and_resume_is_refused() {
            $crate::conformance::channel_contract::stop_all_stops_and_resume_is_refused(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_a_stopped_kernel_starts_no_turn() {
            $crate::conformance::channel_contract::a_stopped_kernel_starts_no_turn(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_an_unpaired_account_cannot_answer() {
            $crate::conformance::channel_contract::an_unpaired_account_cannot_answer(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_a_restart_rings_what_still_waits() {
            $crate::conformance::channel_contract::a_restart_rings_what_still_waits(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_contract_quiet_time_holds_cards() {
            $crate::conformance::channel_contract::quiet_time_holds_cards(&$harness).await;
        }
    };
}
