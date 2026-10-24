//! The channel gateway (spec 3.3 §10 K1): inbound messages become pairing codes, turns,
//! answers and stops; decisions and replies go out to the right thread.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use adk_core::approval::{ApprovalEventKind, ApprovalNotification, ApprovalNotifier};
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalState, ErrorCategory, NewRun,
    OnConflict, RunRecord, RunState,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{
    ChannelAnswer, ChannelBinding, ChannelBindingStore, ChannelHost, ChannelInbound, ChannelNotice,
    ChannelPairingRequest, ChannelSink, DecisionCard, InboundBody, InboundOutcome, StopNotice,
    TurnOrigin, card_summary, chat_may_decide, thread_session,
};
use crate::audit;
use crate::error::{KernelError, Result};
use crate::pairing::secret;
use crate::sched::QueueClass;

/// The target a paired account's turn runs by default: the harness, as `adk-rust up`
/// registers it.
const DEFAULT_TARGET: &str = "adk-rust";
/// How long a chat account's pairing code works by default: an hour, time to reach the
/// machine the kernel runs on.
const DEFAULT_CODE_TTL: Duration = Duration::from_secs(60 * 60);
/// How many accounts of one channel may wait to be paired at once by default.
const DEFAULT_MAX_WAITING: usize = 5;

/// The decision doorbell (spec 3.3 §5.3, §10 K1): an `ApprovalNotifier` registered on
/// `StoreApprovalService::with_notifier` that hands each submitted decision to the
/// [`ChannelGateway`] given it, which sends the card to every paired thread of the
/// decision's person.
///
/// The approval service is built before the kernel and the gateway, so the doorbell holds
/// the decisions it is told about until the gateway starts.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_kernel::channels::ChannelDoorbell;
///
/// let doorbell = Arc::new(ChannelDoorbell::new());
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_notifier(doorbell.clone());
/// # let _ = service;
/// ```
pub struct ChannelDoorbell {
    sender: mpsc::UnboundedSender<ApprovalRequest>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<ApprovalRequest>>>,
}

impl fmt::Debug for ChannelDoorbell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelDoorbell").finish_non_exhaustive()
    }
}

impl ChannelDoorbell {
    /// A doorbell with no gateway yet.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver: Mutex::new(Some(receiver)) }
    }

    fn take_receiver(&self) -> Option<mpsc::UnboundedReceiver<ApprovalRequest>> {
        self.receiver.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

impl Default for ChannelDoorbell {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalNotifier for ChannelDoorbell {
    /// Passes each submitted decision on; every other change rings nothing.
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        if notification.kind == ApprovalEventKind::Submitted {
            // A gateway that is gone hears nothing; the decision waits in the inbox.
            let _ = self.sender.send(notification.request.clone());
        }
        Ok(())
    }
}

/// Builds a [`ChannelGateway`].
pub struct ChannelGatewayBuilder {
    host: Arc<dyn ChannelHost>,
    bindings: Arc<dyn ChannelBindingStore>,
    sinks: BTreeMap<String, Arc<dyn ChannelSink>>,
    target: String,
    home: Option<String>,
    code_ttl: Duration,
    max_waiting: usize,
    doorbell: Option<Arc<ChannelDoorbell>>,
}

impl fmt::Debug for ChannelGatewayBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelGatewayBuilder")
            .field("sinks", &self.sinks.keys().collect::<Vec<_>>())
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl ChannelGatewayBuilder {
    /// Adds a channel's adapter; a second adapter of the same channel replaces the first.
    #[must_use]
    pub fn sink(mut self, sink: Arc<dyn ChannelSink>) -> Self {
        self.sinks.insert(sink.channel().to_string(), sink);
        self
    }

    /// The target paired accounts' turns run; defaults to `adk-rust`, the harness.
    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    /// The web home's address, which cards and notices link to for what chat cannot do.
    #[must_use]
    pub fn home_url(mut self, url: impl Into<String>) -> Self {
        self.home = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// How long a pairing code sent to an unpaired account works; defaults to an hour.
    #[must_use]
    pub fn code_ttl(mut self, ttl: Duration) -> Self {
        self.code_ttl = ttl;
        self
    }

    /// How many accounts of one channel may wait to be paired at once; defaults to five.
    #[must_use]
    pub fn max_waiting(mut self, max: usize) -> Self {
        self.max_waiting = max;
        self
    }

    /// Rings `doorbell`'s decisions once [`ChannelGateway::start`] runs.
    #[must_use]
    pub fn doorbell(mut self, doorbell: Arc<ChannelDoorbell>) -> Self {
        self.doorbell = Some(doorbell);
        self
    }

    /// The gateway.
    pub fn build(self) -> ChannelGateway {
        ChannelGateway {
            inner: Arc::new(Inner {
                host: self.host,
                bindings: self.bindings,
                sinks: self.sinks,
                target: self.target,
                home: self.home,
                code_ttl: self.code_ttl,
                max_waiting: self.max_waiting,
                doorbell: self.doorbell,
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }
}

/// The kernel's side of every chat channel (spec 3.3 §5.3, §10 K1, S-I5, S-I6, S-I10): see
/// the [module documentation](super) for the rules it applies.
///
/// An adapter hands each inbound message to [`receive`](Self::receive);
/// [`start`](Self::start) follows the host's finished turns, so each reply goes back to the
/// thread its turn came from, and rings the doorbell's decisions.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelGateway, ChannelInbound, InMemoryChannel, InMemoryChannelBindingStore};
///
/// # async fn example(kernel: adk_kernel::Kernel) -> adk_kernel::Result<()> {
/// let chat = Arc::new(InMemoryChannel::new("chat"));
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(chat.clone())
///     .build();
/// gateway.start().await?;
/// gateway.receive(ChannelInbound::text("chat", "ana", "t1", "m1", "hello")).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ChannelGateway {
    inner: Arc<Inner>,
}

struct Inner {
    host: Arc<dyn ChannelHost>,
    bindings: Arc<dyn ChannelBindingStore>,
    sinks: BTreeMap<String, Arc<dyn ChannelSink>>,
    target: String,
    home: Option<String>,
    code_ttl: Duration,
    max_waiting: usize,
    doorbell: Option<Arc<ChannelDoorbell>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in self.tasks.get_mut().unwrap_or_else(PoisonError::into_inner).drain(..) {
            task.abort();
        }
    }
}

impl fmt::Debug for ChannelGateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelGateway")
            .field("sinks", &self.inner.sinks.keys().collect::<Vec<_>>())
            .field("target", &self.inner.target)
            .field("home", &self.inner.home)
            .finish_non_exhaustive()
    }
}

/// A command a typed message gives.
enum Control {
    Stop,
    Resume,
}

/// The command `text` gives, when it is one: "stop all" or `/stop`, "resume" or `/resume`.
fn control_word(text: &str) -> Option<Control> {
    let text = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    // A platform command may name the bot: `/stop@agent_bot`.
    let text = match text.strip_prefix('/') {
        Some(command) => format!("/{}", command.split('@').next().unwrap_or_default()),
        None => text,
    };
    match text.as_str() {
        "stop all" | "stop everything" | "/stop" | "/stopall" | "/stop_all" => Some(Control::Stop),
        "resume" | "resume all" | "unfreeze" | "/resume" | "/resumeall" | "/resume_all"
        | "/unfreeze" => Some(Control::Resume),
        _ => None,
    }
}

impl ChannelGateway {
    /// A gateway over `host` keeping pairings in `bindings`, with no channels yet.
    pub fn builder(
        host: Arc<dyn ChannelHost>,
        bindings: Arc<dyn ChannelBindingStore>,
    ) -> ChannelGatewayBuilder {
        ChannelGatewayBuilder {
            host,
            bindings,
            sinks: BTreeMap::new(),
            target: DEFAULT_TARGET.to_string(),
            home: None,
            code_ttl: DEFAULT_CODE_TTL,
            max_waiting: DEFAULT_MAX_WAITING,
            doorbell: None,
        }
    }

    /// The binding store.
    pub fn bindings(&self) -> &Arc<dyn ChannelBindingStore> {
        &self.inner.bindings
    }

    /// Follows the host's finished turns, sending each reply to the thread its turn came
    /// from, and rings the doorbell's decisions. Stopped by [`stop`](Self::stop) or when the
    /// last handle to the gateway is dropped.
    ///
    /// # Errors
    ///
    /// Returns the host's error when it cannot follow its turns.
    pub async fn start(&self) -> Result<()> {
        let mut turns = self.inner.host.finished_turns().await?;
        let weak = Arc::downgrade(&self.inner);
        let replies = tokio::spawn(async move {
            while let Some(run) = turns.next().await {
                let Some(gateway) = upgrade(&weak) else { break };
                if let Err(error) = gateway.reply(&run).await {
                    tracing::warn!(run.id = %run.run_id, error = %error, "a chat reply could not be sent");
                }
            }
        });
        self.track(replies);
        if let Some(mut receiver) =
            self.inner.doorbell.as_ref().and_then(|bell| bell.take_receiver())
        {
            let weak = Arc::downgrade(&self.inner);
            let bell = tokio::spawn(async move {
                while let Some(request) = receiver.recv().await {
                    let Some(gateway) = upgrade(&weak) else { break };
                    if let Err(error) = gateway.ring(&request).await {
                        tracing::warn!(approval.id = %request.id, error = %error, "a decision card could not be sent");
                    }
                }
            });
            self.track(bell);
        }
        Ok(())
    }

    /// Stops following turns and ringing decisions.
    pub fn stop(&self) {
        let tasks =
            std::mem::take(&mut *self.inner.tasks.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks {
            task.abort();
        }
    }

    fn track(&self, task: JoinHandle<()>) {
        let mut tasks = self.inner.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    /// Handles one inbound message (spec 3.3 §10 K1): an unpaired account gets a pairing code
    /// and starts nothing; a paired account's message starts a turn as its person, "stop
    /// all" stops everything, "resume" is refused, and an answer decides at low and medium
    /// risk.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for a channel with no sink, an audit failure under
    /// `AuditFailureMode::Block` for an action that must be recorded first, or a store's,
    /// the host's or the approval service's error.
    pub async fn receive(&self, inbound: ChannelInbound) -> Result<InboundOutcome> {
        let sink = self.sink(&inbound.channel)?;
        let id = ChannelBinding::id_for(&inbound.channel, &inbound.account);
        let binding = self.inner.bindings.get_binding(&id).await?.filter(ChannelBinding::is_active);
        let Some(binding) = binding else {
            return self.unpaired(sink.as_ref(), &inbound).await;
        };
        match &inbound.body {
            InboundBody::Text { text } => match control_word(text) {
                Some(Control::Stop) => self.stop_all(sink.as_ref(), &binding, &inbound).await,
                Some(Control::Resume) => {
                    self.refuse_resume(sink.as_ref(), &binding, &inbound).await
                }
                None => self.turn(sink.as_ref(), &binding, &inbound, text).await,
            },
            InboundBody::StopAll => self.stop_all(sink.as_ref(), &binding, &inbound).await,
            InboundBody::ResumeAll => self.refuse_resume(sink.as_ref(), &binding, &inbound).await,
            InboundBody::Answer { decision, version, answer } => {
                self.answer(sink.as_ref(), &binding, &inbound, decision, *version, *answer).await
            }
        }
    }

    /// Sends the card of `request` to every paired thread of its person, or, during quiet
    /// time, when quiet time ends if the decision still waits (spec 3.3 §5.3, §7.3). Returns
    /// how many threads it reached now.
    ///
    /// # Errors
    ///
    /// Returns the binding store's error.
    pub async fn ring(&self, request: &ApprovalRequest) -> Result<usize> {
        if request.state != ApprovalState::Pending {
            return Ok(0);
        }
        let bindings = self.bindings_of(&request.scope.user_id).await?;
        if bindings.is_empty() {
            return Ok(0);
        }
        let now = self.inner.host.now();
        if let Some(until) = self.inner.host.quiet_until(now) {
            let weak = Arc::downgrade(&self.inner);
            let id = request.id.clone();
            tracing::debug!(approval.id = %id, quiet.until = %until, "decision card held for quiet time");
            let held = tokio::spawn(async move {
                if let Err(error) = ring_after_quiet(weak, &id).await {
                    tracing::warn!(approval.id = %id, error = %error, "a held decision card could not be sent");
                }
            });
            self.track(held);
            return Ok(0);
        }
        Ok(self.deliver_card(request, &bindings).await)
    }

    /// Rings every decision still waiting for a paired person, as [`ring`](Self::ring) does
    /// one (spec 3.3 §7.3): call it after a restart, so a decision whose card a restart held
    /// through quiet time, or never sent, still reaches the thread. A card sent before the
    /// restart is sent again. Returns how many threads it reached now.
    ///
    /// # Errors
    ///
    /// Returns the binding store's or the approval service's error.
    pub async fn ring_waiting(&self) -> Result<usize> {
        let Some(approvals) = self.inner.host.approvals() else { return Ok(0) };
        let mut people: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for binding in self.inner.bindings.list_bindings().await? {
            if binding.is_active() && self.inner.sinks.contains_key(&binding.channel) {
                people.entry(binding.user_id.clone()).or_default().extend(binding.groups);
            }
        }
        let mut reached = 0;
        for (user, groups) in people {
            let mut cursor = None;
            loop {
                let query = adk_core::InboxQuery::for_approver(
                    SubjectRef::human(user.clone()),
                    groups.clone(),
                )
                .with_cursor(cursor)
                .with_limit(200);
                let page = approvals.inbox(query).await?;
                for request in page.items.iter().filter(|request| request.scope.user_id == user) {
                    reached += self.ring(request).await?;
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
        Ok(reached)
    }

    /// The active bindings of `user_id` whose channel has a sink.
    async fn bindings_of(&self, user_id: &str) -> Result<Vec<ChannelBinding>> {
        Ok(self
            .inner
            .bindings
            .list_bindings()
            .await?
            .into_iter()
            .filter(|binding| {
                binding.is_active()
                    && binding.user_id == user_id
                    && self.inner.sinks.contains_key(&binding.channel)
            })
            .collect())
    }

    async fn deliver_card(&self, request: &ApprovalRequest, bindings: &[ChannelBinding]) -> usize {
        let card = DecisionCard::from_request(request, self.inner.home.as_deref());
        let mut reached = 0;
        for binding in bindings {
            let Some(sink) = self.inner.sinks.get(&binding.channel) else { continue };
            let extra = [("risk", json!(request.risk)), ("version", json!(request.version))];
            if let Err(error) = self
                .record(binding, Some(&binding.user_id), "notify", Some(&request.id), &extra)
                .await
            {
                tracing::warn!(error = %error, "a decision card was not sent: its record failed");
                continue;
            }
            match sink.send_decision(&binding.thread, &card).await {
                Ok(()) => reached += 1,
                Err(error) => {
                    tracing::warn!(channel = %binding.channel, error = %error, "a decision card could not be sent")
                }
            }
        }
        reached
    }

    /// Sends a finished turn's reply to its thread: the agent's answer when it completed, a
    /// notice when it failed. Returns whether it sent anything.
    async fn reply(&self, run: &RunRecord) -> Result<bool> {
        let Some(origin) = TurnOrigin::of(run) else { return Ok(false) };
        let Some(sink) = self.inner.sinks.get(&origin.channel) else { return Ok(false) };
        // An account removed while its turn ran hears nothing more.
        let id = ChannelBinding::id_for(&origin.channel, &origin.account);
        if !self.inner.bindings.get_binding(&id).await?.is_some_and(|binding| binding.is_active()) {
            return Ok(false);
        }
        match run.state {
            RunState::Completed => {
                let text = run
                    .result
                    .as_ref()
                    .and_then(|result| result.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    return Ok(false);
                }
                sink.send_message(&origin.thread, text).await?;
                Ok(true)
            }
            RunState::Failed => {
                sink.send_notice(&origin.thread, &ChannelNotice::TurnFailed).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn sink(&self, channel: &str) -> Result<Arc<dyn ChannelSink>> {
        self.inner.sinks.get(channel).cloned().ok_or_else(|| KernelError::Config {
            reason: format!(
                "no adapter for channel `{channel}` on this gateway: add it with \
                 ChannelGatewayBuilder::sink"
            ),
        })
    }

    async fn unpaired(
        &self,
        sink: &dyn ChannelSink,
        inbound: &ChannelInbound,
    ) -> Result<InboundOutcome> {
        let store = &self.inner.bindings;
        let now = self.inner.host.now();
        store.prune_requests(now).await?;
        if let Some(waiting) = store.get_request(&inbound.channel, &inbound.account).await?
            && waiting.expires_at > now
        {
            let notice = ChannelNotice::PairingWaiting { expires_at: waiting.expires_at };
            self.notify(sink, &inbound.thread, &notice).await;
            return Ok(InboundOutcome::PairingWaiting);
        }
        let waiting = store
            .list_requests()
            .await?
            .iter()
            .filter(|request| request.channel == inbound.channel && request.expires_at > now)
            .count();
        let account = Account::of(inbound);
        if waiting >= self.inner.max_waiting {
            self.record(&account, None, "pairing.busy", None, &[]).await?;
            self.notify(sink, &inbound.thread, &ChannelNotice::PairingBusy).await;
            return Ok(InboundOutcome::PairingBusy);
        }
        let code = secret::new_code()?;
        let expires_at = now + chrono::Duration::from_std(self.inner.code_ttl).unwrap_or_default();
        let mut request = ChannelPairingRequest::new(
            secret::code_hash(&code),
            &inbound.channel,
            &inbound.account,
            &inbound.thread,
            now,
            expires_at,
        );
        request.display_name = inbound
            .display_name
            .as_deref()
            .map(|name| secret::clean_name(name, super::MAX_NAME_CHARS))
            .filter(|name| !name.is_empty());
        self.record(
            &account,
            None,
            "pairing.requested",
            Some(&ChannelBinding::id_for(&inbound.channel, &inbound.account)),
            &[],
        )
        .await?;
        store.put_request(request.clone()).await?;
        let notice = ChannelNotice::PairingCode { code, expires_at };
        if let Err(error) = sink.send_notice(&inbound.thread, &notice).await {
            // A code the account never received would block it until it expires.
            store.take_request(&request.code_hash).await?;
            return Err(error);
        }
        Ok(InboundOutcome::PairingRequested)
    }

    async fn turn(
        &self,
        sink: &dyn ChannelSink,
        binding: &ChannelBinding,
        inbound: &ChannelInbound,
        text: &str,
    ) -> Result<InboundOutcome> {
        let session = thread_session(&inbound.channel, &inbound.thread);
        let origin = TurnOrigin {
            channel: inbound.channel.clone(),
            account: inbound.account.clone(),
            thread: inbound.thread.clone(),
        };
        // The pairing is the authentication: the turn is the paired person's own, a `Human`
        // subject, keyed by the platform's message id so a redelivery starts nothing (K-I3).
        let run = NewRun::new(
            QueueClass::INTERACTIVE,
            self.inner.host.app_name(),
            binding.user_id.clone(),
            self.inner.target.clone(),
            json!({ "text": text, "channel": origin }),
        )
        .with_session_id(session.clone())
        .with_idempotency_key(format!(
            "channel:{}:{}:{}",
            inbound.channel, inbound.thread, inbound.message_id
        ))
        .with_concurrency_key(session.clone(), OnConflict::Enqueue)
        .with_authenticated_user();
        self.record(binding, Some(&binding.user_id), "message", Some(&session), &[]).await?;
        match self.inner.host.start_turn(run).await {
            Ok(pid) => Ok(InboundOutcome::TurnStarted { pid }),
            Err(KernelError::Frozen { .. }) => {
                let notice = ChannelNotice::Stopped { home: self.inner.home.clone() };
                self.notify(sink, &inbound.thread, &notice).await;
                Ok(InboundOutcome::TurnRefused)
            }
            Err(error) => Err(error),
        }
    }

    async fn stop_all(
        &self,
        sink: &dyn ChannelSink,
        binding: &ChannelBinding,
        inbound: &ChannelInbound,
    ) -> Result<InboundOutcome> {
        // Stopping takes effect even when its record cannot be written.
        if let Err(error) = self.record(binding, Some(&binding.user_id), "stop", None, &[]).await {
            tracing::warn!(error = %error, "the chat stop's record failed; stopping regardless");
        }
        let reason = format!("stopped from {} by {}", inbound.channel, binding.user_id);
        let by = AuditActor::subject(SubjectRef::human(binding.user_id.clone()));
        self.inner.host.stop_all(&reason, by).await?;
        let notice = StopNotice { at: self.inner.host.now(), home: self.inner.home.clone() };
        if let Err(error) = sink.send_stopped(&inbound.thread, &notice).await {
            tracing::warn!(error = %error, "the stop notice could not be sent");
        }
        Ok(InboundOutcome::Stopped)
    }

    async fn refuse_resume(
        &self,
        sink: &dyn ChannelSink,
        binding: &ChannelBinding,
        inbound: &ChannelInbound,
    ) -> Result<InboundOutcome> {
        if let Err(error) =
            self.record(binding, Some(&binding.user_id), "resume.refused", None, &[]).await
        {
            tracing::warn!(error = %error, "the refused resume's record failed");
        }
        let notice = ChannelNotice::ResumeRefused { home: self.inner.home.clone() };
        self.notify(sink, &inbound.thread, &notice).await;
        Ok(InboundOutcome::ResumeRefused)
    }

    async fn answer(
        &self,
        sink: &dyn ChannelSink,
        binding: &ChannelBinding,
        inbound: &ChannelInbound,
        decision: &str,
        version: u64,
        answer: ChannelAnswer,
    ) -> Result<InboundOutcome> {
        let thread = &inbound.thread;
        let refused =
            |decision: &str| InboundOutcome::DecisionRefused { decision: decision.to_string() };
        let gone = ChannelNotice::DecisionGone;
        let Some(approvals) = self.inner.host.approvals() else {
            self.notify(sink, thread, &gone).await;
            return Ok(refused(decision));
        };
        // Another person's decision reads as no decision at all (S-I8).
        let request = approvals
            .get(decision)
            .await?
            .filter(|request| request.scope.user_id == binding.user_id);
        let Some(request) = request else {
            self.record(
                binding,
                Some(&binding.user_id),
                "decide.refused",
                Some(decision),
                &[("reason", json!("gone"))],
            )
            .await?;
            self.notify(sink, thread, &gone).await;
            return Ok(refused(decision));
        };
        let action = match answer {
            ChannelAnswer::AllowOnce => ApprovalAction::Approve,
            ChannelAnswer::Deny => ApprovalAction::Reject,
        };
        if !chat_may_decide(request.risk) || !request.allowed_actions.contains(&action) {
            // §7.2: chat says yes once or no, at low and medium risk only.
            let extra = [("reason", json!("not_in_chat")), ("risk", json!(request.risk))];
            self.record(binding, Some(&binding.user_id), "decide.refused", Some(decision), &extra)
                .await?;
            self.notify(
                sink,
                thread,
                &ChannelNotice::DecideInHome { home: self.inner.home.clone() },
            )
            .await;
            return Ok(refused(decision));
        }
        if request.state.is_terminal() {
            self.notify(sink, thread, &already_decided(&request, &binding.user_id)).await;
            return Ok(InboundOutcome::AlreadyDecided { decision: decision.to_string() });
        }
        let extra =
            [("answer", json!(answer)), ("version", json!(version)), ("risk", json!(request.risk))];
        self.record(binding, Some(&binding.user_id), "decide", Some(decision), &extra).await?;
        let subject = SubjectRef::human(binding.user_id.clone());
        let mut choice = match answer {
            ChannelAnswer::AllowOnce => ApprovalDecision::approve(subject),
            ChannelAnswer::Deny => ApprovalDecision::reject(subject, String::new()),
        }
        .with_groups(binding.groups.iter().cloned());
        if answer == ChannelAnswer::Deny {
            choice.reason = None;
        }
        match approvals.decide(decision, version, choice).await {
            Ok(decided) => {
                let summary = card_summary(&decided.summary, &decided.args_redacted);
                self.notify(sink, thread, &ChannelNotice::Decided { answer, summary }).await;
                Ok(InboundOutcome::Decided { decision: decision.to_string() })
            }
            // S-I2: a stale answer is shown, never retried.
            Err(error) if error.category == ErrorCategory::Conflict => {
                match approvals.get(decision).await? {
                    Some(current) if current.state == ApprovalState::Pending => {
                        self.notify(sink, thread, &ChannelNotice::DecisionChanged).await;
                        let card = DecisionCard::from_request(&current, self.inner.home.as_deref());
                        if let Err(error) = sink.send_decision(thread, &card).await {
                            tracing::warn!(error = %error, "a fresh decision card could not be sent");
                        }
                        Ok(InboundOutcome::DecisionChanged { decision: decision.to_string() })
                    }
                    Some(current) => {
                        self.notify(sink, thread, &already_decided(&current, &binding.user_id))
                            .await;
                        Ok(InboundOutcome::AlreadyDecided { decision: decision.to_string() })
                    }
                    None => {
                        self.notify(sink, thread, &gone).await;
                        Ok(refused(decision))
                    }
                }
            }
            Err(error) if error.category == ErrorCategory::Forbidden => {
                self.notify(sink, thread, &gone).await;
                Ok(refused(decision))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Sends `notice`, logging a failure: a notice is a courtesy, not the action.
    async fn notify(&self, sink: &dyn ChannelSink, thread: &str, notice: &ChannelNotice) {
        if let Err(error) = sink.send_notice(thread, notice).await {
            tracing::warn!(channel = sink.channel(), error = %error, "a chat notice could not be sent");
        }
    }

    /// Writes a `kernel.channel` record of `action` by the account (spec 3.3 §11).
    ///
    /// # Errors
    ///
    /// The trail's error under `AuditFailureMode::Block`.
    async fn record(
        &self,
        account: &impl AccountRef,
        user: Option<&str>,
        action: &str,
        target: Option<&str>,
        extra: &[(&str, Value)],
    ) -> Result<()> {
        let (channel, account) = (account.channel(), account.account());
        tracing::info!(
            channel.name = channel,
            channel.action = action,
            channel.target = target.unwrap_or_default(),
            "kernel channel action"
        );
        let Some(trail) = self.inner.host.audit_trail() else { return Ok(()) };
        let actor = match user {
            Some(user) => AuditActor::subject(SubjectRef::human(user)),
            None => AuditActor::default(),
        };
        let draft = AuditRecordDraft::new(
            AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.into()),
            audit::CHANNEL,
        )
        .actor(actor)
        .ext("surface", json!("chat"))
        .ext("channel", json!(channel))
        .ext("account", json!(account))
        .ext("action", json!(action))
        .ext("target", json!(target));
        let draft = extra.iter().fold(draft, |draft, (key, value)| draft.ext(*key, value.clone()));
        trail.record(draft).await.map_err(KernelError::from)
    }
}

fn upgrade(weak: &Weak<Inner>) -> Option<ChannelGateway> {
    weak.upgrade().map(|inner| ChannelGateway { inner })
}

/// Waits for quiet time to end, then sends decision `id`'s card if it still waits.
async fn ring_after_quiet(weak: Weak<Inner>, id: &str) -> Result<usize> {
    loop {
        let Some(gateway) = upgrade(&weak) else { return Ok(0) };
        let now = gateway.inner.host.now();
        let Some(until) = gateway.inner.host.quiet_until(now) else { break };
        drop(gateway);
        tokio::time::sleep((until - now).to_std().unwrap_or_default()).await;
    }
    let Some(gateway) = upgrade(&weak) else { return Ok(0) };
    let Some(approvals) = gateway.inner.host.approvals() else { return Ok(0) };
    let Some(request) = approvals.get(id).await? else { return Ok(0) };
    if request.state != ApprovalState::Pending {
        return Ok(0);
    }
    let bindings = gateway.bindings_of(&request.scope.user_id).await?;
    Ok(gateway.deliver_card(&request, &bindings).await)
}

/// The notice of a decision that was answered before.
fn already_decided(request: &ApprovalRequest, user_id: &str) -> ChannelNotice {
    let Some(last) = request.decisions.last() else {
        return ChannelNotice::DecisionGone;
    };
    let said = match last.kind {
        ApprovalAction::Approve | ApprovalAction::Edit => "yes",
        ApprovalAction::Reject => "no",
        ApprovalAction::Respond => "something else",
    };
    let by = (last.approver.id != user_id).then(|| last.approver.id.clone());
    ChannelNotice::AlreadyDecided { by, said: said.to_string(), at: Some(last.decided_at) }
}

/// Something naming a channel and an account, for the records.
trait AccountRef {
    fn channel(&self) -> &str;
    fn account(&self) -> &str;
}

impl AccountRef for ChannelBinding {
    fn channel(&self) -> &str {
        &self.channel
    }

    fn account(&self) -> &str {
        &self.account
    }
}

/// An unpaired account.
struct Account<'a> {
    channel: &'a str,
    account: &'a str,
}

impl<'a> Account<'a> {
    fn of(inbound: &'a ChannelInbound) -> Self {
        Self { channel: &inbound.channel, account: &inbound.account }
    }
}

impl AccountRef for Account<'_> {
    fn channel(&self) -> &str {
        self.channel
    }

    fn account(&self) -> &str {
        self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_words() {
        for text in ["stop all", "Stop all.", "/stop", "/stop@agent_bot", " STOP ALL! "] {
            assert!(matches!(control_word(text), Some(Control::Stop)), "{text}");
        }
        for text in ["resume", "Resume all", "/resume@agent_bot", "/unfreeze"] {
            assert!(matches!(control_word(text), Some(Control::Resume)), "{text}");
        }
        for text in ["stop", "please stop all the things", "stop allowing it", "resume the draft"] {
            assert!(control_word(text).is_none(), "{text}");
        }
    }
}
