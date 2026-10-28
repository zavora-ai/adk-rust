//! Chat on a served site (spec 3.5 §10 X12, §7.19, §7.8.2 "Entry points", §5 S18, A-I24).
//!
//! A served site's chat has two kinds of sender, and the kernel's channel gateway runs with a
//! [`SiteChat`] route that tells them apart:
//!
//! | Sender | What happens |
//! |---|---|
//! | An account paired to the owner or staff | The gateway's turn on the Steward, as that person, its changes entering through the paired device; decision cards ring the account. Photos the message carried join the draft the turn made, or the item it scheduled, through `channel::OwnerChannel`, which asks the person to confirm their alt text in the thread |
//! | Anyone else | The Inbox reads the message as a visitor's — or, without an Inbox, the site files an `inbound_message` signal — and nothing else: no pairing code, no turn, no media kept (A-I24) |
//! | Anyone else typing `/pair` | The gateway's pairing code, which the owner accepts with `adk-rust pair accept` |
//!
//! The adapters live in `adk-surface`, which depends on this crate, so a host builds them
//! and hands them over as [`ChatAdapters`] in [`SiteOptions::chat`](super::SiteOptions::chat);
//! the `adk-rust website` command builds them from the project's `channels.json`
//! ([`super::config::chat`]). WhatsApp's webhook is mounted on the site's public edge at
//! [`WHATSAPP_PATH`].

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{ApprovalRequest, ApprovalService, NewRun, Result, RunRecord};
use adk_harness::website::{AuthoringRouter, ENTRY_STATE_KEY, SiteRuntime};
use adk_kernel::channels::{
    ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelHost, ChannelInbound, ChannelSink,
    InboundBody, InboundRoute,
};
use adk_site::pipeline::EntryPoint;
use async_trait::async_trait;
use awp_types::MessageCategory;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::Mutex;

use crate::channel::{ChannelPhoto, OwnerChannel};
use crate::inbox::SiteInbox;

/// Where a served site's edge serves WhatsApp's webhook (spec 3.5 §10 X12).
pub const WHATSAPP_PATH: &str = "/channels/whatsapp";

/// The command a stranger types to be sent a pairing code.
pub const PAIR_COMMAND: &str = "/pair";

/// The chat platforms a served site talks on, built by a host that has their adapters (spec
/// 3.5 §10 X12): the `adk-rust website` command builds them from `channels.json`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelGateway, ChannelSink, InMemoryChannel};
/// use adk_website::deploy::chat::ChatAdapters;
///
/// /// One in-memory chat, with nothing to serve on the edge.
/// struct InMemory(Arc<InMemoryChannel>);
///
/// #[async_trait::async_trait]
/// impl ChatAdapters for InMemory {
///     fn sinks(&self) -> Vec<Arc<dyn ChannelSink>> {
///         vec![self.0.clone()]
///     }
///
///     async fn start(&self, _gateway: ChannelGateway) -> adk_core::Result<axum::Router> {
///         Ok(axum::Router::new())
///     }
/// }
/// ```
#[async_trait]
pub trait ChatAdapters: Send + Sync {
    /// Each adapter's sink, one per channel.
    fn sinks(&self) -> Vec<Arc<dyn ChannelSink>>;

    /// Starts the adapters on `gateway`, the served site's, which has every sink; returns the
    /// routes the site's public edge serves, such as WhatsApp's webhook at [`WHATSAPP_PATH`].
    ///
    /// # Errors
    ///
    /// An adapter that does not start.
    async fn start(&self, gateway: ChannelGateway) -> Result<axum::Router>;
}

/// Whether `inbound` is a stranger's explicit request for a pairing code: `/pair`, or
/// `/pair@bot`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::ChannelInbound;
/// use adk_website::deploy::chat::asks_to_pair;
///
/// assert!(asks_to_pair(&ChannelInbound::text("telegram", "9", "9", "m", " /pair ")));
/// assert!(!asks_to_pair(&ChannelInbound::text("telegram", "9", "9", "m", "pair me with a loaf")));
/// ```
pub fn asks_to_pair(inbound: &ChannelInbound) -> bool {
    let InboundBody::Text { text } = &inbound.body else { return false };
    let text = text.trim().to_ascii_lowercase();
    text == PAIR_COMMAND || text.strip_prefix("/pair@").is_some_and(|bot| !bot.contains(' '))
}

/// Photos a paired person sent, waiting for the draft their turn makes.
struct Staged {
    inbound: ChannelInbound,
    device: String,
    photos: Vec<ChannelPhoto>,
}

/// The served site's route through the kernel's channel gateway (spec 3.5 §10 X12, §7.19,
/// A-I24): see the [module documentation](self).
pub struct SiteChat {
    runtime: Arc<SiteRuntime>,
    inbox: Option<Arc<SiteInbox>>,
    signals: Arc<dyn adk_server::awp::SignalEmitter>,
    principal: String,
    owner: Option<OwnerChannel>,
    sinks: BTreeMap<String, Arc<dyn ChannelSink>>,
    staged: Mutex<HashMap<String, Staged>>,
    /// Threads whose paired person's turn is running.
    busy: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl std::fmt::Debug for SiteChat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteChat").field("channels", &self.sinks.keys()).finish_non_exhaustive()
    }
}

impl SiteChat {
    /// The route of the site whose back office acts on `runtime`: strangers go to `inbox`,
    /// else a signal on `signals` from `principal`; a paired person's photos go through the
    /// back office's media intake, as `bindings` pair them; replies leave through `sinks`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_website::deploy::chat::SiteChat;
    ///
    /// # fn example(site: &adk_website::deploy::RunningSite, signals: Arc<dyn adk_server::awp::SignalEmitter>, bindings: Arc<dyn adk_kernel::channels::ChannelBindingStore>, chat: Arc<adk_kernel::channels::InMemoryChannel>) {
    /// let route = SiteChat::new(site.runtime.clone(), site.inbox.clone(), signals, "urn:adk:agent:juniper/edge", bindings, vec![chat]);
    /// # let _ = route;
    /// # }
    /// ```
    pub fn new(
        runtime: Arc<SiteRuntime>,
        inbox: Option<Arc<SiteInbox>>,
        signals: Arc<dyn adk_server::awp::SignalEmitter>,
        principal: impl Into<String>,
        bindings: Arc<dyn ChannelBindingStore>,
        sinks: Vec<Arc<dyn ChannelSink>>,
    ) -> Self {
        let owner = match OwnerChannel::new(runtime.clone(), AuthoringRouter::new(bindings)) {
            Ok(owner) => Some(owner),
            Err(error) => {
                tracing::warn!(error = %error, "the owner's photos cannot reach drafts");
                None
            }
        };
        Self {
            runtime,
            inbox,
            signals,
            principal: principal.into(),
            owner,
            sinks: sinks.into_iter().map(|sink| (sink.channel().to_string(), sink)).collect(),
            staged: Mutex::new(HashMap::new()),
            busy: std::sync::Mutex::default(),
        }
    }

    /// A paired person's turn started in `thread`: photos that arrive meanwhile wait for the
    /// draft it makes. The host calls it as the turn starts.
    pub fn turn_started(&self, thread: &str) {
        self.busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread.to_string());
    }

    /// The owner's chat channel, for its alt-text confirmations.
    pub fn owner_channel(&self) -> Option<&OwnerChannel> {
        self.owner.as_ref()
    }

    async fn say(&self, inbound: &ChannelInbound, text: &str) {
        let Some(sink) = self.sinks.get(&inbound.channel) else { return };
        if let Err(error) = sink.send_message(&inbound.thread, text).await {
            tracing::warn!(channel.name = %inbound.channel, error = %error, "a site chat reply could not be sent");
        }
    }

    /// A stranger's message: the Inbox's, or a signal (A-I24).
    async fn stranger(&self, inbound: &ChannelInbound) -> Result<()> {
        if !matches!(inbound.body, InboundBody::Text { .. }) {
            return Ok(());
        }
        if let Some(inbox) = &self.inbox {
            inbox.receive_inbound(inbound).await?;
            return Ok(());
        }
        let thread = format!("{}:{}", inbound.channel, inbound.thread);
        let signal = adk_harness::website::inbound_signal(
            &inbound.channel,
            &thread,
            MessageCategory::Other,
            &self.principal,
            Utc::now(),
        );
        self.signals.emit(signal).await
    }

    /// Keeps the photos of the paired person's `inbound`, from `device`, for the draft their
    /// turn makes (spec 3.5 §5 S18). Only images are kept; they reach the site only through
    /// the media pipeline.
    pub async fn stage(&self, inbound: &ChannelInbound, device: &str) -> usize {
        let caption = match &inbound.body {
            InboundBody::Text { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        };
        let photos: Vec<ChannelPhoto> = inbound
            .attachments
            .iter()
            .filter(|file| file.media_type.starts_with("image/"))
            .map(|file| ChannelPhoto {
                name: file.name.clone(),
                bytes: file.bytes.clone().into(),
                caption: file.caption.clone().or_else(|| caption.clone()),
            })
            .collect();
        let count = photos.len();
        if count > 0 {
            let mut staged = self.staged.lock().await;
            let entry = staged.entry(inbound.thread.clone()).or_insert_with(|| Staged {
                inbound: inbound.clone(),
                device: device.to_string(),
                photos: Vec::new(),
            });
            entry.photos.extend(photos);
        }
        count
    }

    /// A paired person's turn in `thread` finished: the photos their message carried join
    /// the newest draft that entered through their device, and the person is asked to confirm
    /// their alt text (spec 3.5 §5 S18). Returns how many photos were attached; photos wait
    /// while the person's device has no draft.
    pub async fn turn_finished(&self, thread: &str) -> usize {
        self.busy.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(thread);
        let Some(owner) = &self.owner else { return 0 };
        let Some(staged) = self.staged.lock().await.remove(thread) else { return 0 };
        let entry = EntryPoint::PairedDevice { device: staged.device.clone() };
        let Some((draft, items)) = self.runtime.newest_draft_from(&entry).await else {
            self.staged.lock().await.insert(thread.to_string(), staged);
            return 0;
        };
        let Some(item) = items.first() else { return 0 };
        let title = item
            .fields
            .get("title")
            .and_then(|title| title.as_str())
            .map_or_else(|| item.slug.clone(), str::to_string);
        let count = staged.photos.len();
        match owner.attach(&staged.inbound, &draft, &item.id, &title, staged.photos).await {
            Ok(attached) => {
                self.say(&staged.inbound, &attached.reply).await;
                attached.photos.len()
            }
            Err(error) => {
                tracing::warn!(error = %error, photos = count, "a paired person's photos could not join the draft");
                0
            }
        }
    }
}

#[async_trait]
impl InboundRoute for SiteChat {
    /// A stranger's message goes to the Inbox, except `/pair`; a paired person's alt-text
    /// answer is the owner channel's. A paired person's photos wait for the draft of the turn
    /// running in their thread; with no turn running they join their newest draft at once, or
    /// — when they have none and the message has words, such as "add this loaf" — the message
    /// is a turn whose draft they join.
    async fn take(
        &self,
        inbound: &ChannelInbound,
        binding: Option<&ChannelBinding>,
    ) -> adk_kernel::Result<bool> {
        let Some(binding) = binding else {
            if asks_to_pair(inbound) {
                return Ok(false);
            }
            self.stranger(inbound).await.map_err(adk_kernel::KernelError::Core)?;
            return Ok(true);
        };
        if let Some(owner) = &self.owner
            && let Some(answer) =
                owner.reply(inbound).await.map_err(adk_kernel::KernelError::Core)?
        {
            self.say(inbound, &answer).await;
            return Ok(true);
        }
        let busy = self
            .busy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&inbound.thread);
        if self.stage(inbound, &binding.id).await == 0 {
            return Ok(false);
        }
        if busy {
            return Ok(true);
        }
        let words = matches!(&inbound.body, InboundBody::Text { text } if !text.trim().is_empty());
        let entry = EntryPoint::PairedDevice { device: binding.id.clone() };
        if words && self.runtime.newest_draft_from(&entry).await.is_none() {
            return Ok(false);
        }
        if self.turn_finished(&inbound.thread).await == 0 {
            self.say(
                inbound,
                "Got the photos. Tell me what they are for, and they will join that draft.",
            )
            .await;
        }
        Ok(true)
    }
}

/// The served kernel as the gateway's host (spec 3.5 §7.8.2 "Entry points"): a paired
/// person's turn starts in a session that records their device as its entry point, and a
/// finished turn lets [`SiteChat`] attach the photos its message carried.
pub(crate) struct SiteChatHost {
    pub(crate) kernel: Arc<dyn ChannelHost>,
    pub(crate) sessions: Option<Arc<dyn adk_session::SessionService>>,
    pub(crate) bindings: Arc<dyn ChannelBindingStore>,
    pub(crate) chat: Arc<SiteChat>,
}

/// The thread a chat turn came from, from its run's input.
fn thread_of(run: &RunRecord) -> Option<String> {
    run.input.get("channel")?.get("thread")?.as_str().map(str::to_string)
}

impl SiteChatHost {
    /// Starts `run`'s session with the paired device as its entry point, when it is new.
    async fn entered(&self, run: &NewRun) -> Result<()> {
        let (Some(sessions), Some(session)) = (&self.sessions, run.session_id.as_deref()) else {
            return Ok(());
        };
        let origin = run.input.get("channel");
        let field = |name: &str| origin.and_then(|o| o.get(name)).and_then(|v| v.as_str());
        let (Some(channel), Some(account)) = (field("channel"), field("account")) else {
            return Ok(());
        };
        let id = ChannelBinding::id_for(channel, account);
        let Some(binding) = self
            .bindings
            .get_binding(&id)
            .await
            .map_err(|e| adk_core::AdkError::config(e.to_string()))?
            .filter(ChannelBinding::is_active)
        else {
            return Ok(());
        };
        let found = sessions
            .get(adk_session::GetRequest {
                app_name: run.app_name.clone(),
                user_id: run.user_id.clone(),
                session_id: session.to_string(),
                num_recent_events: Some(0),
                after: None,
            })
            .await;
        if found.is_ok() {
            return Ok(());
        }
        let entry = EntryPoint::PairedDevice { device: binding.id };
        let state = HashMap::from([(
            ENTRY_STATE_KEY.to_string(),
            serde_json::to_value(entry).unwrap_or_default(),
        )]);
        sessions
            .create(adk_session::CreateRequest {
                app_name: run.app_name.clone(),
                user_id: run.user_id.clone(),
                session_id: Some(session.to_string()),
                state,
            })
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ChannelHost for SiteChatHost {
    fn app_name(&self) -> &str {
        self.kernel.app_name()
    }

    fn now(&self) -> DateTime<Utc> {
        self.kernel.now()
    }

    fn quiet_until(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.kernel.quiet_until(at)
    }

    fn approvals(&self) -> Option<Arc<dyn ApprovalService>> {
        self.kernel.approvals()
    }

    async fn quiet_ends(
        &self,
        request: &ApprovalRequest,
        at: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        self.kernel.quiet_ends(request, at).await
    }

    fn rates_high(&self, request: &ApprovalRequest) -> bool {
        self.kernel.rates_high(request)
    }

    fn moves_money(&self, request: &ApprovalRequest) -> bool {
        self.kernel.moves_money(request)
    }

    fn audit_trail(&self) -> Option<AuditTrail> {
        self.kernel.audit_trail()
    }

    async fn start_turn(&self, run: NewRun) -> adk_kernel::Result<String> {
        if let Err(error) = self.entered(&run).await {
            tracing::warn!(error = %error, "a paired person's session could not record its entry point");
        }
        if let Some(thread) =
            run.input.get("channel").and_then(|o| o.get("thread")).and_then(|t| t.as_str())
        {
            self.chat.turn_started(thread);
        }
        self.kernel.start_turn(run).await
    }

    async fn finished_turns(&self) -> adk_kernel::Result<BoxStream<'static, RunRecord>> {
        let chat = self.chat.clone();
        let turns = self.kernel.finished_turns().await?;
        Ok(turns
            .then(move |run| {
                let chat = chat.clone();
                async move {
                    if let Some(thread) = thread_of(&run) {
                        chat.turn_finished(&thread).await;
                    }
                    run
                }
            })
            .boxed())
    }

    async fn turn(&self, pid: &str) -> adk_kernel::Result<Option<RunRecord>> {
        let run = self.kernel.turn(pid).await?;
        if let Some(run) = run.as_ref().filter(|run| run.state.is_terminal())
            && let Some(thread) = thread_of(run)
        {
            self.chat.turn_finished(&thread).await;
        }
        Ok(run)
    }

    async fn stop_all(&self, reason: &str, by: AuditActor) -> adk_kernel::Result<()> {
        self.kernel.stop_all(reason, by).await
    }
}

/// Starts the served site's chat on `kernel` (spec 3.5 §10 X12): one gateway over every
/// adapter's sink, routed by [`SiteChat`], ringing the doorbell the approval service rings,
/// its turns the Steward's; then the adapters. Returns the gateway and the routes the edge
/// serves.
pub(crate) async fn start(
    adapters: &Arc<dyn ChatAdapters>,
    kernel: &adk_kernel::Kernel,
    site: &super::RunningSite,
    bindings: Arc<dyn ChannelBindingStore>,
    doorbell: Option<Arc<adk_kernel::channels::ChannelDoorbell>>,
    sessions: Option<Arc<dyn adk_session::SessionService>>,
) -> Result<(ChannelGateway, axum::Router)> {
    use crate::team::AgentRole;
    let sinks = adapters.sinks();
    let chat = Arc::new(SiteChat::new(
        site.runtime.clone(),
        site.inbox.clone(),
        site.signals(),
        AgentRole::Inbox.principal(&site.site),
        bindings.clone(),
        sinks.clone(),
    ));
    let host = Arc::new(SiteChatHost {
        kernel: Arc::new(kernel.clone()),
        sessions,
        bindings: bindings.clone(),
        chat: chat.clone(),
    });
    let mut builder =
        ChannelGateway::builder(host, bindings).target(AgentRole::Steward.as_str()).route(chat);
    for sink in sinks {
        builder = builder.sink(sink);
    }
    if let Some(doorbell) = doorbell {
        builder = builder.doorbell(doorbell);
    }
    let gateway = builder.build();
    gateway.start().await.map_err(|e| adk_core::AdkError::config(e.to_string()))?;
    let routes = adapters.start(gateway.clone()).await?;
    let ring = gateway.clone();
    tokio::spawn(async move {
        // Decisions that waited through a restart reach the paired accounts again.
        if let Err(error) = ring.ring_waiting().await {
            tracing::warn!(error = %error, "the waiting decisions could not be sent to the site's chat");
        }
    });
    Ok((gateway, routes))
}
