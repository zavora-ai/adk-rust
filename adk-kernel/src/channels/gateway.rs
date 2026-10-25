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
    ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalRisk, ApprovalState, ErrorCategory,
    NewRun, OnConflict, RunRecord, RunState,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{
    ChannelAnswer, ChannelBinding, ChannelBindingStore, ChannelHost, ChannelInbound, ChannelNotice,
    ChannelPairingRequest, ChannelSink, DecisionCard, InboundBody, InboundOutcome, StopNotice,
    TAKE_OVER_TOOL, TurnOrigin, card_summary, chat_may_decide, thread_session,
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
/// The approval service's code for an answer at a version the decision has left.
const VERSION_CONFLICT_CODE: &str = "approval.version_conflict";
/// The approval service's code for an approver whose answer already counts toward a quorum.
const ALREADY_DECIDED_CODE: &str = "approval.already_decided";
/// How often the gateway rings the decisions still waiting by default: a card that could not
/// be sent, or a decision held on another replica, reaches the thread within a minute.
const DEFAULT_RESWEEP: Duration = Duration::from_secs(60);
/// How often the gateway asks after the turns it started that have not finished.
const TURN_POLL: Duration = Duration::from_secs(2);
/// The longest one wait for the end of quiet time sleeps before it reads the clock again:
/// a timer does not run while the machine sleeps, the wall clock does.
const CLOCK_CHECK: Duration = Duration::from_secs(60);

/// The decision doorbell (spec 3.3 §5.3, §10 K1): an `ApprovalNotifier` registered on
/// `StoreApprovalService::with_notifier` that hands each submitted decision to the
/// [`ChannelGateway`] given it, which sends the card to every paired thread of the
/// decision's person.
///
/// The approval service is built before the kernel and the gateway, so the doorbell holds
/// the decisions it is told about until the gateway starts, and again while a stopped
/// gateway is started anew.
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
    /// Held by the running gateway's bell task; a stopped task lets the next one take it.
    receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ApprovalRequest>>>,
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
        Self { sender, receiver: Arc::new(tokio::sync::Mutex::new(receiver)) }
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
            // The doorbell holds its receiver, so a send fails only while it is dropped.
            if self.sender.send(notification.request.clone()).is_err() {
                tracing::warn!(approval.id = %notification.request.id, "the decision doorbell is gone; the decision waits in the inbox");
            }
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
    money_tools: Vec<String>,
    resweep: Duration,
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

    /// How often a started gateway rings the decisions still waiting, as
    /// [`ChannelGateway::ring_waiting`] does; defaults to a minute.
    #[must_use]
    pub fn resweep(mut self, every: Duration) -> Self {
        self.resweep = every;
        self
    }

    /// Adds tools whose calls move money, which chat never decides (spec 3.3 §7.4 rule 4):
    /// their cards link to the home and carry no answers. A name ending in `*` names every
    /// tool it starts. The list starts with `payments_*`, the tools `adk-payments`
    /// publishes, beside the tools the host's drivers declare `financial`
    /// ([`ChannelHost::moves_money`]).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn example(builder: adk_kernel::channels::ChannelGatewayBuilder) {
    /// let builder = builder.money_tools(["transfer", "bank_*"]);
    /// # let _ = builder;
    /// # }
    /// ```
    #[must_use]
    pub fn money_tools(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.money_tools.extend(names.into_iter().map(Into::into));
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
                money_tools: self.money_tools,
                resweep: self.resweep,
                lifecycle: Mutex::new(Lifecycle::default()),
                held: Mutex::new(Vec::new()),
                carded: Mutex::new(Carded::default()),
                started: Mutex::new(std::collections::BTreeSet::new()),
                replied: Mutex::new(Carded::default()),
                unpaired: tokio::sync::Mutex::new(()),
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
    money_tools: Vec<String>,
    resweep: Duration,
    /// The tasks [`ChannelGateway::start`] runs, and whether it runs.
    lifecycle: Mutex<Lifecycle>,
    /// The cards held through quiet time.
    held: Mutex<Vec<JoinHandle<()>>>,
    /// The decisions carded, or held to be carded, at their version.
    carded: Mutex<Carded>,
    /// The turns this gateway started that have not finished.
    started: Mutex<std::collections::BTreeSet<String>>,
    /// The turns replied to, so a turn finished here and seen in the run store is answered
    /// once.
    replied: Mutex<Carded>,
    /// Taken while an unpaired account is given a code, so the waiting limit and one code
    /// per account hold for messages that arrive together.
    unpaired: tokio::sync::Mutex<()>,
}

/// Whether the gateway runs, and its tasks.
#[derive(Default)]
struct Lifecycle {
    running: bool,
    /// Counts starts and stops, so a start that a stop overtook spawns nothing.
    generation: u64,
    tasks: Vec<JoinHandle<()>>,
}

/// The decisions the gateway carded at a version, newest last, at most [`Carded::LIMIT`].
#[derive(Default)]
struct Carded {
    seen: std::collections::HashSet<(String, u64)>,
    order: std::collections::VecDeque<(String, u64)>,
}

impl Carded {
    const LIMIT: usize = 4096;

    /// Notes decision `id` at `version`; `false` when it was noted before.
    fn note(&mut self, id: &str, version: u64) -> bool {
        let key = (id.to_string(), version);
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > Self::LIMIT
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        true
    }

    /// Forgets decision `id` at `version`, so it can be carded again.
    fn forget(&mut self, id: &str, version: u64) {
        let key = (id.to_string(), version);
        if self.seen.remove(&key) {
            self.order.retain(|noted| *noted != key);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let lifecycle = self.lifecycle.get_mut().unwrap_or_else(PoisonError::into_inner);
        let held = self.held.get_mut().unwrap_or_else(PoisonError::into_inner);
        for task in lifecycle.tasks.drain(..).chain(held.drain(..)) {
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
            money_tools: vec!["payments_*".to_string()],
            resweep: DEFAULT_RESWEEP,
        }
    }

    /// The binding store.
    pub fn bindings(&self) -> &Arc<dyn ChannelBindingStore> {
        &self.inner.bindings
    }

    /// Follows the host's finished turns, sending each reply to the thread its turn came
    /// from, and rings the doorbell's decisions. Stopped by [`stop`](Self::stop) or when the
    /// last handle to the gateway is dropped; starting a running gateway changes nothing,
    /// and a stopped one starts again with the decisions the doorbell held meanwhile.
    ///
    /// # Errors
    ///
    /// Returns the host's error when it cannot follow its turns.
    pub async fn start(&self) -> Result<()> {
        let generation = {
            let mut lifecycle = self.lifecycle();
            if lifecycle.running {
                return Ok(());
            }
            lifecycle.running = true;
            lifecycle.generation += 1;
            lifecycle.generation
        };
        let mut turns = match self.inner.host.finished_turns().await {
            Ok(turns) => turns,
            Err(error) => {
                let mut lifecycle = self.lifecycle();
                if lifecycle.generation == generation {
                    lifecycle.running = false;
                }
                return Err(error);
            }
        };
        let mut lifecycle = self.lifecycle();
        if !lifecycle.running || lifecycle.generation != generation {
            // A stop came first.
            return Ok(());
        }
        let weak = Arc::downgrade(&self.inner);
        lifecycle.tasks.push(tokio::spawn(async move {
            while let Some(run) = turns.next().await {
                let Some(gateway) = upgrade(&weak) else { break };
                gateway.finished(&run).await;
            }
        }));
        // A turn another replica ran finishes on that replica's bus: ask after it.
        let weak = Arc::downgrade(&self.inner);
        lifecycle.tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(TURN_POLL).await;
                let Some(gateway) = upgrade(&weak) else { break };
                gateway.follow_started().await;
            }
        }));
        // A card that could not be sent, or a decision another replica holds, is rung again.
        let weak = Arc::downgrade(&self.inner);
        let every = self.inner.resweep;
        lifecycle.tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(gateway) = upgrade(&weak) else { break };
                if let Err(error) = gateway.ring_waiting().await {
                    tracing::warn!(error = %error, "the waiting decisions could not be rung");
                }
            }
        }));
        if let Some(doorbell) = &self.inner.doorbell {
            let receiver = doorbell.receiver.clone();
            let weak = Arc::downgrade(&self.inner);
            lifecycle.tasks.push(tokio::spawn(async move {
                // The previous bell task, once aborted, lets go of the receiver.
                let mut receiver = receiver.lock().await;
                while let Some(request) = receiver.recv().await {
                    let Some(gateway) = upgrade(&weak) else { break };
                    if let Err(error) = gateway.ring(&request).await {
                        tracing::warn!(approval.id = %request.id, error = %error, "a decision card could not be sent");
                    }
                }
            }));
        }
        Ok(())
    }

    /// Stops following turns and ringing decisions, and drops the cards held through quiet
    /// time; [`ring_waiting`](Self::ring_waiting) after the next start sends them.
    pub fn stop(&self) {
        let tasks = {
            let mut lifecycle = self.lifecycle();
            lifecycle.running = false;
            lifecycle.generation += 1;
            std::mem::take(&mut lifecycle.tasks)
        };
        let held =
            std::mem::take(&mut *self.inner.held.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks.into_iter().chain(held) {
            task.abort();
        }
        // Held cards were dropped: they may be carded again.
        *self.inner.carded.lock().unwrap_or_else(PoisonError::into_inner) = Carded::default();
    }

    fn lifecycle(&self) -> std::sync::MutexGuard<'_, Lifecycle> {
        self.inner.lifecycle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn hold(&self, task: JoinHandle<()>) {
        let mut held = self.inner.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain(|task| !task.is_finished());
        held.push(task);
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

    /// Sends the card of `request` to every paired thread of its person, when the policy
    /// lets that person answer it, or, during quiet time, when quiet time ends if the
    /// decision still waits (spec 3.3 §5.3, §7.3). The card carries answers only when chat
    /// may decide it: low or medium risk, and no money (§7.2, §7.4). A decision is carded
    /// once at each version, whether the doorbell or [`ring_waiting`](Self::ring_waiting)
    /// rings it first. Returns how many threads it reached now.
    ///
    /// # Errors
    ///
    /// Returns the binding store's error.
    pub async fn ring(&self, request: &ApprovalRequest) -> Result<usize> {
        if request.state != ApprovalState::Pending {
            return Ok(0);
        }
        let bindings = self.answering_bindings(request).await?;
        if bindings.is_empty() {
            return Ok(0);
        }
        if !self.carded().note(&request.id, request.version) {
            return Ok(0);
        }
        let now = self.inner.host.now();
        if let Some(until) = self.inner.host.quiet_ends(request, now).await {
            let weak = Arc::downgrade(&self.inner);
            let held = request.clone();
            tracing::debug!(approval.id = %request.id, quiet.until = %until, "decision card held for quiet time");
            let held = tokio::spawn(async move {
                let id = held.id.clone();
                if let Err(error) = ring_after_quiet(weak, held).await {
                    tracing::warn!(approval.id = %id, error = %error, "a held decision card could not be sent");
                }
            });
            self.hold(held);
            return Ok(0);
        }
        let reached = self.deliver_card(request, &bindings).await;
        if reached == 0 {
            self.carded().forget(&request.id, request.version);
        }
        Ok(reached)
    }

    fn carded(&self) -> std::sync::MutexGuard<'_, Carded> {
        self.inner.carded.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replies to the finished turn `run` once, however it was seen to finish.
    async fn finished(&self, run: &RunRecord) {
        self.inner.started.lock().unwrap_or_else(PoisonError::into_inner).remove(&run.run_id);
        let first =
            self.inner.replied.lock().unwrap_or_else(PoisonError::into_inner).note(&run.run_id, 0);
        if first && let Err(error) = self.reply(run).await {
            tracing::warn!(run.id = %run.run_id, error = %error, "a chat reply could not be sent");
        }
    }

    /// Replies to the turns this gateway started that finished, wherever they ran.
    async fn follow_started(&self) {
        let started: Vec<String> = self
            .inner
            .started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        for pid in started {
            match self.inner.host.turn(&pid).await {
                Ok(Some(run)) if run.state.is_terminal() => self.finished(&run).await,
                Ok(Some(_)) => {}
                // The host follows its turns only on its bus, or the run is gone.
                Ok(None) => {
                    self.inner.started.lock().unwrap_or_else(PoisonError::into_inner).remove(&pid);
                }
                Err(error) => {
                    tracing::debug!(process.pid = %pid, error = %error, "a chat turn could not be read");
                }
            }
        }
    }

    /// The active bindings of `request`'s person whose channel has a sink, when the policy
    /// lets that person answer it, as the inbox lists it for them.
    async fn answering_bindings(&self, request: &ApprovalRequest) -> Result<Vec<ChannelBinding>> {
        let mut bindings = self.bindings_of(&request.scope.user_id).await?;
        bindings.retain(|binding| may_answer(request, binding));
        Ok(bindings)
    }

    /// Whether chat may answer `request` at all: low or medium risk, and no money (spec 3.3
    /// §7.2, §7.4 rule 4).
    fn chat_may_answer(&self, request: &ApprovalRequest) -> bool {
        chat_may_decide(self.risk_of(request)) && !self.moves_money(request)
    }

    /// The risk of `request`: as the request rates it, or high when the host rates it so.
    fn risk_of(&self, request: &ApprovalRequest) -> ApprovalRisk {
        if self.inner.host.rates_high(request) {
            request.risk.max(ApprovalRisk::High)
        } else {
            request.risk
        }
    }

    /// Whether `request`'s call moves money: the host says so, or the tool is one the
    /// gateway was told moves money.
    fn moves_money(&self, request: &ApprovalRequest) -> bool {
        self.inner.host.moves_money(request)
            || self.inner.money_tools.iter().any(|name| match name.strip_suffix('*') {
                Some(prefix) => request.tool_name.starts_with(prefix),
                None => request.tool_name == *name,
            })
    }

    /// The card of `request`, with its answers only when chat may answer it.
    fn card(&self, request: &ApprovalRequest) -> DecisionCard {
        let mut card = DecisionCard::from_request(request, self.inner.home.as_deref());
        card.risk = self.risk_of(request);
        // A suggested agent keeps "Don't add" at any risk (spec 3.3 §7.2).
        if !self.chat_may_answer(request) && !card.adds_agent {
            card.answers.clear();
        }
        card
    }

    /// Rings every decision still waiting for a paired person, as [`ring`](Self::ring) does
    /// one (spec 3.3 §7.3): call it after a restart, so a decision whose card a restart held
    /// through quiet time, or never sent, still reaches the thread. A card this gateway sent
    /// since it started is not sent again, while one sent before a process restart, or
    /// before [`stop`](Self::stop), is. Returns how many threads it reached now.
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
        let card = self.card(request);
        let mut reached = 0;
        for binding in bindings {
            let Some(sink) = self.inner.sinks.get(&binding.channel) else { continue };
            let extra =
                [("risk", json!(self.risk_of(request))), ("version", json!(request.version))];
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
        // One at a time: the waiting count and the account's earlier code are read and the
        // new code stored with no other unpaired message in between.
        let _serial = self.inner.unpaired.lock().await;
        let store = &self.inner.bindings;
        let now = self.inner.host.now();
        store.prune_requests(now).await?;
        if let Some(waiting) = store.get_request(&inbound.channel, &inbound.account).await?
            && waiting.expires_at > now
        {
            // A dismissed account hears nothing until its dismissal ends.
            if waiting.dismissed {
                tracing::debug!(channel.name = %inbound.channel, "a dismissed chat account wrote");
                return Ok(InboundOutcome::PairingDismissed);
            }
            let notice = ChannelNotice::PairingWaiting { expires_at: waiting.expires_at };
            self.notify(sink, &inbound.thread, &notice).await;
            return Ok(InboundOutcome::PairingWaiting);
        }
        let waiting = store
            .list_requests()
            .await?
            .iter()
            .filter(|request| {
                request.channel == inbound.channel && !request.dismissed && request.expires_at > now
            })
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
            Ok(pid) => {
                self.inner
                    .started
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(pid.clone());
                Ok(InboundOutcome::TurnStarted { pid })
            }
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
        let take_over = request.tool_name == TAKE_OVER_TOOL && answer == ChannelAnswer::AllowOnce;
        // Spec 3.3 §7.2, S-I11: chat refuses a suggested agent at any risk, never adds one.
        let adds_agent = request.tool_name == crate::agents::AGENT_CREATE_TOOL_NAME;
        let not_here = if adds_agent {
            answer != ChannelAnswer::Deny
        } else {
            !self.chat_may_answer(&request) || take_over
        };
        if not_here || !request.allowed_actions.contains(&action) {
            // §7.2, §7.4 rule 4: chat says yes once or no, at low and medium risk only, and
            // never to money; a take-over's secret is typed in the home (§8.2).
            let reason = match (adds_agent, self.moves_money(&request), take_over) {
                (true, _, _) => "adds_agent",
                (false, true, _) => "money",
                (false, false, true) => "take_over",
                (false, false, false) => "not_in_chat",
            };
            let extra = [("reason", json!(reason)), ("risk", json!(self.risk_of(&request)))];
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
        let extra = [
            ("answer", json!(answer)),
            ("version", json!(version)),
            ("risk", json!(self.risk_of(&request))),
        ];
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
            // A quorum's first answer counts, and the decision still waits.
            Ok(decided) if decided.state == ApprovalState::Pending => {
                let notice = ChannelNotice::WaitingForOthers { answered: true };
                self.notify(sink, thread, &notice).await;
                Ok(InboundOutcome::Decided { decision: decision.to_string() })
            }
            Ok(decided) => {
                let summary = card_summary(&decided.summary, &decided.args_redacted);
                self.notify(sink, thread, &ChannelNotice::Decided { answer, summary }).await;
                Ok(InboundOutcome::Decided { decision: decision.to_string() })
            }
            // S-I2: a stale answer is shown, never retried; only a changed decision that
            // still waits gets a fresh card.
            Err(error) if error.code == VERSION_CONFLICT_CODE => {
                match approvals.get(decision).await? {
                    Some(current) if current.state == ApprovalState::Pending => {
                        self.notify(sink, thread, &ChannelNotice::DecisionChanged).await;
                        let card = self.card(&current);
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
            // This person's answer already counts toward the quorum.
            Err(error) if error.code == ALREADY_DECIDED_CODE => {
                let notice = ChannelNotice::WaitingForOthers { answered: true };
                self.notify(sink, thread, &notice).await;
                Ok(InboundOutcome::AlreadyDecided { decision: decision.to_string() })
            }
            // Resolved or expired meanwhile.
            Err(error) if error.category == ErrorCategory::Conflict => {
                match approvals.get(decision).await? {
                    Some(current) if current.state.is_terminal() => {
                        self.notify(sink, thread, &already_decided(&current, &binding.user_id))
                            .await;
                        Ok(InboundOutcome::AlreadyDecided { decision: decision.to_string() })
                    }
                    _ => {
                        self.notify(sink, thread, &gone).await;
                        Ok(refused(decision))
                    }
                }
            }
            // The policy does not ask this person, or forbids them answering their own call.
            Err(error) if error.category == ErrorCategory::Forbidden => {
                let notice = ChannelNotice::WaitingForOthers { answered: false };
                self.notify(sink, thread, &notice).await;
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
        // The origin as `adk-surface` records an answer's: how it was given (spec 3.3 §11).
        .ext("origin", json!({ "kind": "chat", "channel": channel }))
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

/// Waits for quiet time to end, then sends `held`'s card if the decision still waits; the
/// card was noted at `held`'s version. The wait reads the wall clock at least every
/// [`CLOCK_CHECK`], so a machine that slept through the night sends the card when quiet
/// time ends, not hours later.
async fn ring_after_quiet(weak: Weak<Inner>, held: ApprovalRequest) -> Result<usize> {
    loop {
        let Some(gateway) = upgrade(&weak) else { return Ok(0) };
        let now = gateway.inner.host.now();
        let Some(until) = gateway.inner.host.quiet_ends(&held, now).await else { break };
        drop(gateway);
        let wait = (until - now).to_std().unwrap_or_default().min(CLOCK_CHECK);
        tokio::time::sleep(wait).await;
    }
    let Some(gateway) = upgrade(&weak) else { return Ok(0) };
    let Some(approvals) = gateway.inner.host.approvals() else { return Ok(0) };
    let Some(request) = approvals.get(&held.id).await? else { return Ok(0) };
    if request.state != ApprovalState::Pending {
        return Ok(0);
    }
    let bindings = gateway.answering_bindings(&request).await?;
    let reached = gateway.deliver_card(&request, &bindings).await;
    if reached == 0 {
        // The next sweep rings it again.
        gateway.carded().forget(&held.id, held.version);
    }
    Ok(reached)
}

/// Whether the person `binding` acts for may answer `request`: an approver the policy
/// names, and not its requester when the policy forbids self-approval.
fn may_answer(request: &ApprovalRequest, binding: &ChannelBinding) -> bool {
    let person = SubjectRef::human(binding.user_id.clone());
    request.is_eligible(&person, &binding.groups)
        && !(request.policy.forbid_self_approval && request.is_self(&person))
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
