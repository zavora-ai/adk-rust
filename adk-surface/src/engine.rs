//! The engine (spec 3.3 §6): the feed, the projection and the commands every surface shares.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::{ApprovalDecision, ApprovalRequest, ApprovalState, Clock, SystemClock};
use chrono::{DateTime, FixedOffset, Local, Utc};
use futures::stream::{self, BoxStream, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::answers::{AnswerLog, AnswerRecord, InMemoryAnswerLog, SURFACE_AUDIT_DECIDE};
use crate::error::{Result, SurfaceError};
use crate::model::{
    AgentRow, Briefing, Decided, DecisionCard, FeedEntry, Mode, ModeView, Reversibility, StatusLine,
};
use crate::origin::{Caller, Origin};
use crate::project::{self, Place, Projector};
use crate::rules::{self, Answer, Row};
use crate::source::Source;
use crate::words;

/// The audit action of "Send" and "Queue" (spec 3.3 §11).
pub const SURFACE_AUDIT_SEND: &str = "surface.send";
/// The audit action of "Stop all" (spec 3.3 §11).
pub const SURFACE_AUDIT_STOP_ALL: &str = "surface.stop_all";
/// The audit action of "Resume all" (spec 3.3 §11).
pub const SURFACE_AUDIT_RESUME_ALL: &str = "surface.resume_all";
/// The audit action of a mode change (spec 3.3 §11).
pub const SURFACE_AUDIT_SET_MODE: &str = "surface.set_mode";

/// How often the feed reads the session store when the source has nothing faster.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How long queued input waits for the session to become free before it is dropped from
/// the delivery loop (it stays queued for the next decision to pick up).
const DRAIN_PATIENCE: Duration = Duration::from_secs(600);

/// A command from a surface (spec 3.3 §6). Every command runs as an authenticated
/// [`Caller`], whose subject, surface and [`Origin`] reach its audit record.
///
/// # Example
///
/// ```rust
/// use adk_surface::{Answer, Command};
///
/// let command: Command = serde_json::from_value(serde_json::json!({
///     "type": "decide",
///     "decisionId": "apr_1",
///     "version": 1,
///     "answer": "allowOnce",
/// }))
/// .unwrap();
/// assert!(matches!(command, Command::Decide { answer: Answer::AllowOnce, .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Command {
    /// Answers a decision at the version the surface saw (S-I2).
    #[serde(rename_all = "camelCase")]
    Decide {
        /// The card's `id`.
        decision_id: String,
        /// The card's `version`.
        version: u64,
        /// The answer.
        answer: Answer,
        /// The changed arguments of an `edit`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edited_args: Option<Value>,
        /// Why, for a `deny`; reported to the agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The surface showed the call in full (§7.1).
        #[serde(default)]
        expanded: bool,
    },
    /// Cancels a spoken "allow once" inside its cancel window (§7.2).
    #[serde(rename_all = "camelCase")]
    CancelAnswer {
        /// The decision.
        decision_id: String,
    },
    /// Sends a message; queued instead while a decision of the session is open (S-I4).
    #[serde(rename_all = "camelCase")]
    Send {
        /// The session.
        session_id: String,
        /// The message.
        text: String,
    },
    /// Queues a message as the session's next, after its open decisions (S-I4).
    #[serde(rename_all = "camelCase")]
    Queue {
        /// The session.
        session_id: String,
        /// The message.
        text: String,
    },
    /// Stops every agent (S-I10).
    #[serde(rename_all = "camelCase")]
    StopAll {
        /// Why.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Resumes after a stop: the web home or a paired local terminal only (S-I10).
    ResumeAll,
    /// Changes how much a session's agent acts on its own.
    #[serde(rename_all = "camelCase")]
    SetMode {
        /// The session.
        session_id: String,
        /// The mode.
        mode: Mode,
    },
}

/// What a command did (spec 3.3 §6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CommandOutcome {
    /// This answer was recorded; `card.decided` says how it ended when it did.
    #[serde(rename_all = "camelCase")]
    Answered {
        /// The decision after the answer.
        card: DecisionCard,
    },
    /// Someone answered first: shown, never retried (S-I2).
    #[serde(rename_all = "camelCase")]
    AlreadyDecided {
        /// Who answered, where, how and when.
        decided: Decided,
        /// The decision.
        card: DecisionCard,
    },
    /// The decision changed since the version the surface saw and is still open: show it
    /// again (S-I2).
    #[serde(rename_all = "camelCase")]
    Changed {
        /// The decision now.
        card: DecisionCard,
    },
    /// A spoken "allow once" takes effect at `cancel_until` unless cancelled (§7.2).
    #[serde(rename_all = "camelCase")]
    Pending {
        /// The decision.
        decision_id: String,
        /// When it takes effect.
        cancel_until: DateTime<Utc>,
        /// "Allowing once in 5 seconds".
        text: String,
    },
    /// A spoken answer was cancelled in its window.
    #[serde(rename_all = "camelCase")]
    AnswerCancelled {
        /// The decision.
        decision_id: String,
    },
    /// The message started a turn.
    #[serde(rename_all = "camelCase")]
    Sent {
        /// The session.
        session_id: String,
    },
    /// The message waits until the session's open decisions are answered (S-I4).
    #[serde(rename_all = "camelCase")]
    Queued {
        /// The session.
        session_id: String,
        /// Its place in the queue, from 1.
        position: u32,
        /// "Sent after this card is answered".
        text: String,
    },
    /// Every agent stopped.
    #[serde(rename_all = "camelCase")]
    Stopped {
        /// "All agents stopped".
        text: String,
    },
    /// Agents run again.
    Resumed,
    /// The mode changed.
    #[serde(rename_all = "camelCase")]
    ModeSet {
        /// The mode now.
        mode: ModeView,
    },
}

/// One frame of a session's feed (spec 3.3 §6): the snapshot, then the live tail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FeedFrame {
    /// One event of the session store; resume after its `seq` (`Last-Event-ID`).
    #[serde(rename_all = "camelCase")]
    Entry {
        /// The entry.
        entry: FeedEntry,
    },
    /// The session's open decisions changed.
    #[serde(rename_all = "camelCase")]
    Decisions {
        /// The open decisions of the session.
        cards: Vec<DecisionCard>,
    },
    /// The store's history of the session got shorter (it was rewound): drop what was shown;
    /// the entries follow again from the start.
    #[serde(rename_all = "camelCase")]
    Reset {
        /// How many events the store holds now.
        len: u64,
    },
}

type Classify<T> = Arc<dyn Fn(&ApprovalRequest) -> T + Send + Sync>;

/// Builds a [`SurfaceEngine`].
pub struct SurfaceEngineBuilder {
    source: Arc<dyn Source>,
    audit: Option<AuditTrail>,
    answers: Option<Arc<dyn AnswerLog>>,
    projector: Projector,
    quiet_set: bool,
    poll: Duration,
    clock: Arc<dyn Clock>,
    voice_window: Duration,
}

impl fmt::Debug for SurfaceEngineBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfaceEngineBuilder")
            .field("projector", &self.projector)
            .finish_non_exhaustive()
    }
}

impl SurfaceEngineBuilder {
    /// Writes a record of every command to `trail` (spec 3.3 §11). Without one, commands are
    /// not audited.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Keeps how each answer was given in `log` instead of in memory.
    #[must_use]
    pub fn answers(mut self, log: Arc<dyn AnswerLog>) -> Self {
        self.answers = Some(log);
        self
    }

    /// Where decisions are also shown, for "Also on your phone and in Telegram".
    #[must_use]
    pub fn places(mut self, places: impl IntoIterator<Item = Place>) -> Self {
        self.projector.places = places.into_iter().collect();
        self
    }

    /// Shows times at `offset` east of UTC instead of the host's time zone.
    #[must_use]
    pub fn utc_offset(mut self, offset: FixedOffset) -> Self {
        self.projector.offset = offset;
        self
    }

    /// Quiet time, overriding the source's.
    #[must_use]
    pub fn quiet_hours(mut self, quiet: adk_kernel::QuietHours) -> Self {
        self.projector.quiet = Some(quiet);
        self.quiet_set = true;
        self
    }

    /// Shows the agent named `name` as `display_name`.
    #[must_use]
    pub fn agent_name(mut self, name: impl Into<String>, display_name: impl Into<String>) -> Self {
        self.projector.names.insert(name.into(), display_name.into());
        self
    }

    /// Writes a decision's heading — what will happen, in plain words — with `heading`
    /// instead of from the tool and its main argument.
    #[must_use]
    pub fn heading(
        mut self,
        heading: impl Fn(&ApprovalRequest) -> String + Send + Sync + 'static,
    ) -> Self {
        self.projector.heading = Arc::new(heading) as Classify<String>;
        self
    }

    /// Tells calls that spend money with `money` instead of by the `payments_*` tool names.
    #[must_use]
    pub fn money(
        mut self,
        money: impl Fn(&ApprovalRequest) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.projector.money = Arc::new(money) as Classify<bool>;
        self
    }

    /// Tells whether a call can be taken back with `reversibility`.
    #[must_use]
    pub fn reversibility(
        mut self,
        reversibility: impl Fn(&ApprovalRequest) -> Reversibility + Send + Sync + 'static,
    ) -> Self {
        self.projector.reversibility = Arc::new(reversibility) as Classify<Reversibility>;
        self
    }

    /// How often the feed reads the store when the source signals nothing sooner.
    #[must_use]
    pub fn poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// The clock "waiting since" and quiet time are read against.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// How long a spoken "allow once" can be cancelled (default
    /// [`VOICE_CANCEL_WINDOW_SECS`](crate::rules::VOICE_CANCEL_WINDOW_SECS)).
    #[must_use]
    pub fn voice_cancel_window(mut self, window: Duration) -> Self {
        self.voice_window = window;
        self
    }

    /// The engine.
    pub fn build(mut self) -> SurfaceEngine {
        if !self.quiet_set {
            self.projector.quiet = self.source.quiet_hours();
        }
        SurfaceEngine {
            inner: Arc::new(Inner {
                source: self.source,
                projector: self.projector,
                audit: self.audit,
                answers: self.answers.unwrap_or_else(|| Arc::new(InMemoryAnswerLog::new())),
                poll: self.poll,
                clock: self.clock,
                voice_window: self.voice_window,
                queues: Mutex::new(HashMap::new()),
                draining: Mutex::new(HashSet::new()),
                spoken: Mutex::new(HashMap::new()),
            }),
        }
    }
}

/// The engine every surface runs on (spec 3.3 §6): it reads a [`Source`], projects what it
/// reads into cards, thread items, agent rows, the status line and the briefing, and carries
/// out commands under the rules of §7.2 and §7.4.
///
/// Surfaces hold no state a reconnect cannot rebuild: everything here is read from the
/// source's stores, except input queued behind an open decision, which waits in this
/// process.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};
///
/// # async fn example(sessions: Arc<dyn adk_session::SessionService>, approvals: Arc<dyn adk_core::ApprovalService>) -> adk_surface::Result<()> {
/// let source = LocalSource::builder("adk-rust").sessions(sessions).approvals(approvals).build()?;
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// let me = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
/// for card in engine.decisions(&me).await? {
///     println!("{} · {}", card.heading, card.status_text);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct SurfaceEngine {
    inner: Arc<Inner>,
}

struct Inner {
    source: Arc<dyn Source>,
    projector: Projector,
    audit: Option<AuditTrail>,
    answers: Arc<dyn AnswerLog>,
    poll: Duration,
    clock: Arc<dyn Clock>,
    voice_window: Duration,
    /// Input queued behind an open decision, per (user, session), oldest first.
    queues: Mutex<HashMap<(String, String), VecDeque<String>>>,
    /// Sessions with a delivery loop running.
    draining: Mutex<HashSet<(String, String)>>,
    /// Spoken "allow once" answers inside their cancel window, by decision.
    spoken: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl fmt::Debug for SurfaceEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SurfaceEngine")
            .field("app_name", &self.inner.source.app_name())
            .field("projector", &self.inner.projector)
            .finish_non_exhaustive()
    }
}

impl SurfaceEngine {
    /// A builder over `source`.
    pub fn builder(source: Arc<dyn Source>) -> SurfaceEngineBuilder {
        let offset = *Local::now().offset();
        SurfaceEngineBuilder {
            source,
            audit: None,
            answers: None,
            projector: Projector::new(offset),
            quiet_set: false,
            poll: DEFAULT_POLL_INTERVAL,
            clock: Arc::new(SystemClock),
            voice_window: Duration::from_secs(u64::from(rules::VOICE_CANCEL_WINDOW_SECS)),
        }
    }

    /// The source.
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.inner.source
    }

    fn now(&self) -> DateTime<Utc> {
        self.inner.clock.now()
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// The decisions waiting for `caller`, oldest first, drawn from their inbox entries
    /// (spec 3.3 §7.1, S-I1).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn decisions(&self, caller: &Caller) -> Result<Vec<DecisionCard>> {
        self.open_decisions(caller, None).await
    }

    /// The decisions of one session waiting for `caller`, oldest first (spec 3.3 §10 C1).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn session_decisions(
        &self,
        caller: &Caller,
        session: &str,
    ) -> Result<Vec<DecisionCard>> {
        self.open_decisions(caller, Some(session)).await
    }

    async fn open_decisions(
        &self,
        caller: &Caller,
        session: Option<&str>,
    ) -> Result<Vec<DecisionCard>> {
        let mut requests: Vec<ApprovalRequest> = self
            .inner
            .source
            .requests(caller, session, true)
            .await?
            .into_iter()
            .filter(|request| {
                owns(caller, request)
                    && request.state == ApprovalState::Pending
                    && session.is_none_or(|session| request.scope.session_id == session)
            })
            .collect();
        requests.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let mut cards = Vec::with_capacity(requests.len());
        for request in &requests {
            cards.push(self.card(caller, request).await?);
        }
        Ok(cards)
    }

    /// One decision as `caller` sees it: waiting, or decided with who, where and when.
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` when no decision of the caller has `id` (S-I8), or the
    /// source's error.
    pub async fn decision(&self, caller: &Caller, id: &str) -> Result<DecisionCard> {
        let request = self.owned_request(caller, id).await?;
        self.card(caller, &request).await
    }

    async fn owned_request(&self, caller: &Caller, id: &str) -> Result<ApprovalRequest> {
        self.inner
            .source
            .request(id)
            .await?
            .filter(|request| owns(caller, request))
            .ok_or_else(|| SurfaceError::not_found(format!("decision '{id}'")))
    }

    async fn card(&self, caller: &Caller, request: &ApprovalRequest) -> Result<DecisionCard> {
        let always = match request.state {
            ApprovalState::Pending => {
                self.inner.source.always_allow_offer(request).await.map(|offer| offer.label)
            }
            _ => None,
        };
        let answer = match request.state {
            ApprovalState::Pending => None,
            _ => self.inner.answers.find(&request.id).await?,
        };
        Ok(self.inner.projector.card(request, caller, always, answer.as_ref(), self.now()))
    }

    /// The session's thread: every event of its store, numbered by position (spec 3.3 §6).
    ///
    /// # Errors
    ///
    /// Returns `surface.not_found` when the caller has no such session (S-I8), or the
    /// source's error.
    pub async fn thread(&self, caller: &Caller, session: &str) -> Result<Vec<FeedEntry>> {
        let events = self.transcript(caller, session).await?;
        let links = self.links(caller, session, &events).await?;
        Ok(events
            .iter()
            .enumerate()
            .map(|(index, event)| self.inner.projector.entry(index as u64 + 1, event, &links))
            .collect())
    }

    async fn transcript(&self, caller: &Caller, session: &str) -> Result<Vec<adk_core::Event>> {
        self.inner
            .source
            .transcript(caller.user_id(), session)
            .await?
            .ok_or_else(|| SurfaceError::not_found(format!("session '{session}'")))
    }

    /// The inbox entry of each held call in the session's `events`, by `seq`
    /// ([`project::links`]).
    async fn links(
        &self,
        caller: &Caller,
        session: &str,
        events: &[adk_core::Event],
    ) -> Result<HashMap<u64, String>> {
        let requests: Vec<ApprovalRequest> = self
            .inner
            .source
            .requests(caller, Some(session), false)
            .await?
            .into_iter()
            .filter(|request| owns(caller, request) && request.scope.session_id == session)
            .collect();
        Ok(project::links(events, &requests))
    }

    /// The session's feed (spec 3.3 §6): every event after `after` (none: from the start) as
    /// an [`Entry`](FeedFrame::Entry) numbered by its position in the session store, the
    /// session's open decisions, then the live tail. Resuming with the last `seq` received
    /// never skips or repeats an event.
    ///
    /// The stream ends with an error when the session cannot be read.
    pub fn feed(
        &self,
        caller: Caller,
        session: String,
        after: Option<u64>,
    ) -> BoxStream<'static, Result<FeedFrame>> {
        struct State {
            engine: SurfaceEngine,
            caller: Caller,
            session: String,
            sent: u64,
            cards: Option<Vec<(String, u64)>>,
            buffer: VecDeque<FeedFrame>,
            first: bool,
            done: bool,
        }
        let state = State {
            engine: self.clone(),
            caller,
            session,
            sent: after.unwrap_or(0),
            cards: None,
            buffer: VecDeque::new(),
            first: true,
            done: false,
        };
        stream::unfold(state, |mut state| async move {
            loop {
                if let Some(frame) = state.buffer.pop_front() {
                    return Some((Ok(frame), state));
                }
                if state.done {
                    return None;
                }
                if !state.first {
                    state.engine.inner.source.changed(state.engine.inner.poll).await;
                }
                state.first = false;
                match state
                    .engine
                    .refresh(&state.caller, &state.session, state.sent, &state.cards)
                    .await
                {
                    Ok((frames, sent, cards)) => {
                        state.sent = sent;
                        state.cards = Some(cards);
                        state.buffer.extend(frames);
                    }
                    Err(error) => {
                        state.done = true;
                        return Some((Err(error), state));
                    }
                }
            }
        })
        .boxed()
    }

    /// The frames since `sent`, and the new cursor and open-decision signature.
    async fn refresh(
        &self,
        caller: &Caller,
        session: &str,
        mut sent: u64,
        cards: &Option<Vec<(String, u64)>>,
    ) -> Result<(Vec<FeedFrame>, u64, Vec<(String, u64)>)> {
        let events = self.transcript(caller, session).await?;
        let len = events.len() as u64;
        let mut frames = Vec::new();
        if sent > len {
            frames.push(FeedFrame::Reset { len });
            sent = 0;
        }
        if sent < len {
            let links = self.links(caller, session, &events).await?;
            for (index, event) in
                events.iter().enumerate().skip(usize::try_from(sent).unwrap_or(usize::MAX))
            {
                let entry = self.inner.projector.entry(index as u64 + 1, event, &links);
                frames.push(FeedFrame::Entry { entry });
            }
            sent = len;
        }
        let open = self.session_decisions(caller, session).await?;
        let signature: Vec<(String, u64)> =
            open.iter().map(|card| (card.id.clone(), card.version)).collect();
        if cards.as_ref() != Some(&signature) {
            frames.push(FeedFrame::Decisions { cards: open });
        }
        Ok((frames, sent, signature))
    }

    /// The caller's agents (spec 3.3 §8.1).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn agents(&self, caller: &Caller) -> Result<Vec<AgentRow>> {
        Ok(self
            .inner
            .source
            .agents(caller.user_id())
            .await?
            .iter()
            .map(|state| self.inner.projector.agent_row(state))
            .collect())
    }

    /// The status line (spec 3.3 §5.1): the mode word of `session` (or the default), spend
    /// against the daily limit, quiet time, and the count of decisions.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn status(&self, caller: &Caller, session: Option<&str>) -> Result<StatusLine> {
        let source = &self.inner.source;
        let decisions = self.decisions(caller).await?.len();
        let stopped = source.stopped().await?;
        Ok(StatusLine {
            mode: source.mode(caller.user_id(), session).await?.map(project::mode_view),
            spend: source.spend(caller.user_id()).await?.map(project::spend_view),
            quiet: self.inner.projector.quiet_view(self.now()),
            decisions: u32::try_from(decisions).unwrap_or(u32::MAX),
            stopped,
            stopped_text: stopped.then(|| words::ALL_AGENTS_STOPPED.to_string()),
        })
    }

    /// "While you were away" since `since` (spec 3.3 §5.1, §8.1): spend, waiting decisions,
    /// and one line per event on the `/proc` ledger — the events the kernel's
    /// `GET /kernel/away` lists (§10 K4) — each agent by the name the roster gives it.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn briefing(&self, caller: &Caller, since: DateTime<Utc>) -> Result<Briefing> {
        let source = &self.inner.source;
        let lines = source
            .ledger(since)
            .await?
            .iter()
            .filter_map(|entry| self.inner.projector.briefing_line(entry))
            .collect();
        let waiting = self.decisions(caller).await?.len();
        Ok(Briefing {
            title: words::WHILE_YOU_WERE_AWAY.to_string(),
            since,
            since_text: words::briefing_since(&self.inner.projector.hh_mm(since)),
            spend: source.spend(caller.user_id()).await?.map(project::spend_view),
            waiting: u32::try_from(waiting).unwrap_or(u32::MAX),
            lines,
        })
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    /// Carries out `command` as `caller` (spec 3.3 §6, §7.2, §7.4, §11).
    ///
    /// # Errors
    ///
    /// | Code | When |
    /// |---|---|
    /// | `surface.not_found` | No decision or session of the caller has that id (S-I8) |
    /// | `surface.answer_not_allowed` | The surface may not give this answer (§7.2) |
    /// | `surface.confirm_presence` | A high-risk approval without confirming it is you |
    /// | `surface.call_not_expanded` | A truncated call approved before it was expanded |
    /// | `surface.not_here` | An agent answering, resuming outside the home, loosening the mode from chat or voice |
    /// | `surface.unsupported` | The source cannot do it |
    /// | the source's code | The source failed |
    pub async fn execute(&self, caller: &Caller, command: Command) -> Result<CommandOutcome> {
        match command {
            Command::Decide { decision_id, version, answer, edited_args, reason, expanded } => {
                self.decide(caller, &decision_id, version, answer, edited_args, reason, expanded)
                    .await
            }
            Command::CancelAnswer { decision_id } => self.cancel_answer(caller, &decision_id).await,
            Command::Send { session_id, text } => self.send(caller, session_id, text, false).await,
            Command::Queue { session_id, text } => self.send(caller, session_id, text, true).await,
            Command::StopAll { reason } => self.stop_all(caller, reason).await,
            Command::ResumeAll => self.resume_all(caller).await,
            Command::SetMode { session_id, mode } => self.set_mode(caller, &session_id, mode).await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn decide(
        &self,
        caller: &Caller,
        id: &str,
        version: u64,
        answer: Answer,
        edited_args: Option<Value>,
        reason: Option<String>,
        expanded: bool,
    ) -> Result<CommandOutcome> {
        let request = self.owned_request(caller, id).await?;
        if let Origin::Agent { agent } = &caller.origin {
            let error = SurfaceError::NotHere(format!(
                "an agent ({agent}) never answers a decision; it waits for a person (spec 3.3 §7.4)"
            ));
            self.audit_decide(caller, &request, answer, "refused", Some(&error.code())).await?;
            return Err(error);
        }
        if request.state != ApprovalState::Pending {
            return self.already_decided(caller, &request, answer).await;
        }
        let always = self.inner.source.always_allow_offer(&request).await.map(|offer| offer.label);
        let facts = self.inner.projector.facts(&request, always);
        if let Err(violation) = rules::check(caller, &facts, answer, expanded) {
            let error = SurfaceError::Rule(violation);
            self.audit_decide(caller, &request, answer, "refused", Some(&error.code())).await?;
            return Err(error);
        }
        if answer == Answer::Edit && edited_args.is_none() {
            return Err(SurfaceError::Invalid("an edit needs the changed arguments".into()));
        }
        if rules::row(caller) == Row::Voice && answer == Answer::AllowOnce {
            return self.speak_later(caller, request, version).await;
        }
        self.commit(caller, &request, version, answer, edited_args, reason).await
    }

    /// Records the answer: `ApprovalService::decide` at `version`, never retried (S-I2).
    async fn commit(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
        version: u64,
        answer: Answer,
        edited_args: Option<Value>,
        reason: Option<String>,
    ) -> Result<CommandOutcome> {
        let rule = match answer {
            Answer::AlwaysAllow => {
                Some(self.inner.source.prepare_always_allow(caller, request).await?)
            }
            _ => None,
        };
        let subject = caller.subject.clone();
        let mut decision = match answer {
            Answer::AllowOnce | Answer::AlwaysAllow => ApprovalDecision::approve(subject),
            Answer::Edit => ApprovalDecision::edit(subject, edited_args.unwrap_or(Value::Null)),
            Answer::Deny => ApprovalDecision::reject(subject, reason.clone().unwrap_or_default()),
        }
        .with_groups(caller.groups.clone());
        if answer == Answer::Deny && reason.as_deref().is_none_or(str::is_empty) {
            decision.reason = None;
        }
        match self.inner.source.decide(&request.id, version, decision).await {
            Ok(updated) => {
                if let Some(rule) = rule {
                    rule.commit().await?;
                }
                let record = AnswerRecord {
                    approval_id: updated.id.clone(),
                    by: caller.subject.clone(),
                    via: caller.via(),
                    answer,
                    at: self.now(),
                };
                if updated.state != ApprovalState::Pending {
                    self.inner.answers.record(&record).await?;
                }
                self.audit_decide(caller, &updated, answer, "decided", None).await?;
                if updated.state != ApprovalState::Pending {
                    if let Err(error) = self.inner.source.resolved_by(caller, &updated).await {
                        tracing::warn!(error = %error, decision.id = %updated.id, "the decided call could not be resumed");
                    }
                    self.spawn_drain(caller.clone(), updated.scope.session_id.clone());
                }
                let card = self.card(caller, &updated).await?;
                Ok(CommandOutcome::Answered { card })
            }
            Err(SurfaceError::Source(error))
                if matches!(
                    error.code,
                    "approval.version_conflict" | "approval.already_resolved" | "approval.expired"
                ) || (error.category == adk_core::ErrorCategory::Conflict
                    && error.code != "approval.already_decided") =>
            {
                let current = match self.owned_request(caller, &request.id).await {
                    Ok(current) => current,
                    // The source lists open requests only: answered, but not saying how.
                    Err(SurfaceError::NotFound { .. }) => {
                        return self.decided_elsewhere(caller, request, answer).await;
                    }
                    Err(error) => return Err(error),
                };
                if current.state == ApprovalState::Pending {
                    self.audit_decide(caller, &current, answer, "changed", Some(error.code))
                        .await?;
                    let card = self.card(caller, &current).await?;
                    return Ok(CommandOutcome::Changed { card });
                }
                self.already_decided(caller, &current, answer).await
            }
            Err(error) => Err(error),
        }
    }

    async fn already_decided(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
        answer: Answer,
    ) -> Result<CommandOutcome> {
        self.audit_decide(caller, request, answer, "already_decided", None).await?;
        let card = self.card(caller, request).await?;
        let decided = card.decided.clone().ok_or_else(|| {
            SurfaceError::Invalid(format!("decision '{}' is not decided", request.id))
        })?;
        Ok(CommandOutcome::AlreadyDecided { decided, card })
    }

    /// A decision the source no longer lists after a conflict: decided, details unknown.
    async fn decided_elsewhere(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
        answer: Answer,
    ) -> Result<CommandOutcome> {
        self.audit_decide(caller, request, answer, "already_decided", None).await?;
        let mut card = self.inner.projector.card(request, caller, None, None, self.now());
        let at = self.now();
        let decided = Decided {
            by: String::new(),
            by_you: false,
            via: None,
            at,
            at_text: self.inner.projector.hh_mm(at),
            answer: crate::model::DecidedAnswer::Unknown,
            text: words::DECIDED.to_string(),
        };
        card.status = crate::model::CardStatus::Decided;
        card.status_text = words::DECIDED.to_string();
        card.allowed.options.clear();
        card.allowed.open_in_home = false;
        card.decided = Some(decided.clone());
        Ok(CommandOutcome::AlreadyDecided { decided, card })
    }

    /// A spoken "allow once" waits out its cancel window, then commits (spec 3.3 §5.6, §7.2).
    async fn speak_later(
        &self,
        caller: &Caller,
        request: ApprovalRequest,
        version: u64,
    ) -> Result<CommandOutcome> {
        let window = self.inner.voice_window;
        let cancel_until = self.now()
            + chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(5));
        let engine = self.clone();
        let speaker = caller.clone();
        let id = request.id.clone();
        let task_id = id.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(window).await;
            engine.inner.spoken.lock().unwrap_or_else(|e| e.into_inner()).remove(&task_id);
            if let Err(error) =
                engine.commit(&speaker, &request, version, Answer::AllowOnce, None, None).await
            {
                tracing::warn!(error = %error, decision.id = %task_id, "a spoken answer could not be recorded");
            }
        });
        if let Some(previous) =
            self.inner.spoken.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), handle)
        {
            previous.abort();
        }
        Ok(CommandOutcome::Pending {
            decision_id: id,
            cancel_until,
            text: words::allowing_in(
                u64::try_from(window.as_millis().div_ceil(1000)).unwrap_or(u64::MAX),
            ),
        })
    }

    async fn cancel_answer(&self, caller: &Caller, id: &str) -> Result<CommandOutcome> {
        let request = self.owned_request(caller, id).await?;
        let handle = self.inner.spoken.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        match handle {
            Some(handle) => {
                handle.abort();
                self.audit_decide(caller, &request, Answer::AllowOnce, "cancelled", None).await?;
                Ok(CommandOutcome::AnswerCancelled { decision_id: id.to_string() })
            }
            None => Err(SurfaceError::not_found(format!("a spoken answer to '{id}' to cancel"))),
        }
    }

    async fn send(
        &self,
        caller: &Caller,
        session: String,
        text: String,
        queue: bool,
    ) -> Result<CommandOutcome> {
        if text.trim().is_empty() {
            return Err(SurfaceError::Invalid("the message is empty".into()));
        }
        let open = !self.session_decisions(caller, &session).await?.is_empty();
        // A running turn takes the message next, as an open decision does (S-I4).
        let busy = !open && self.inner.source.busy(caller.user_id(), &session).await?;
        if open || busy || queue {
            let position = {
                let mut queues = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                let entry =
                    queues.entry((caller.user_id().to_string(), session.clone())).or_default();
                entry.push_back(text);
                u32::try_from(entry.len()).unwrap_or(u32::MAX)
            };
            self.audit_command(
                caller,
                SURFACE_AUDIT_SEND,
                json!({ "sessionId": session, "queued": true }),
            )
            .await?;
            if !open {
                self.spawn_drain(caller.clone(), session.clone());
            }
            return Ok(CommandOutcome::Queued {
                session_id: session,
                position,
                text: words::SENT_AFTER_THIS_CARD_IS_ANSWERED.to_string(),
            });
        }
        self.audit_command(
            caller,
            SURFACE_AUDIT_SEND,
            json!({ "sessionId": session, "queued": false }),
        )
        .await?;
        self.inner.source.start_turn(caller, &session, &text).await?;
        Ok(CommandOutcome::Sent { session_id: session })
    }

    /// Delivers the session's queued input, one turn at a time, once it has no open decision
    /// and no running turn (S-I4).
    fn spawn_drain(&self, caller: Caller, session: String) {
        let key = (caller.user_id().to_string(), session.clone());
        {
            let queues = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
            if queues.get(&key).is_none_or(VecDeque::is_empty) {
                return;
            }
        }
        if !self.inner.draining.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            loop {
                if started.elapsed() > DRAIN_PATIENCE {
                    break;
                }
                let open = match engine.session_decisions(&caller, &session).await {
                    Ok(cards) => !cards.is_empty(),
                    Err(_) => true,
                };
                if open {
                    break;
                }
                if engine.inner.source.busy(caller.user_id(), &session).await.unwrap_or(true) {
                    tokio::time::sleep(engine.inner.poll).await;
                    continue;
                }
                let next = engine
                    .inner
                    .queues
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&key)
                    .and_then(VecDeque::pop_front);
                let Some(text) = next else { break };
                if let Err(error) = engine.inner.source.start_turn(&caller, &session, &text).await {
                    tracing::warn!(error = %error, session.id = %session, "queued input could not be sent");
                    break;
                }
                // Let the turn begin before looking again.
                tokio::time::sleep(engine.inner.poll).await;
            }
            engine.inner.draining.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        });
    }

    /// The input queued for the session, oldest first.
    pub fn queued(&self, caller: &Caller, session: &str) -> Vec<String> {
        self.inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(caller.user_id().to_string(), session.to_string()))
            .map(|queue| queue.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Sends the session's queued input once its decisions are answered, after an answer
    /// recorded outside [`execute`](Self::execute), such as a take over (spec 3.3 §7.4 rule 2).
    pub fn deliver_queued(&self, caller: &Caller, session: &str) {
        self.spawn_drain(caller.clone(), session.to_string());
    }

    async fn stop_all(&self, caller: &Caller, reason: Option<String>) -> Result<CommandOutcome> {
        let reason = reason.unwrap_or_else(|| words::STOP_ALL.to_string());
        // Stopping never waits on the audit log (spec 3.2 K-I8): record after, warn on failure.
        self.inner.source.stop_all(caller, &reason).await?;
        if let Err(error) =
            self.audit_command(caller, SURFACE_AUDIT_STOP_ALL, json!({ "reason": reason })).await
        {
            tracing::warn!(error = %error, "stop all took effect; its audit record failed");
        }
        Ok(CommandOutcome::Stopped { text: words::ALL_AGENTS_STOPPED.to_string() })
    }

    async fn resume_all(&self, caller: &Caller) -> Result<CommandOutcome> {
        if !rules::may_resume(caller) {
            return Err(SurfaceError::NotHere(
                "Resume all happens only in the web home or on a paired local terminal \
                 (spec 3.3 S-I10)"
                    .into(),
            ));
        }
        self.audit_command(caller, SURFACE_AUDIT_RESUME_ALL, json!({})).await?;
        self.inner.source.resume_all(caller).await?;
        Ok(CommandOutcome::Resumed)
    }

    async fn set_mode(&self, caller: &Caller, session: &str, mode: Mode) -> Result<CommandOutcome> {
        let target = project::harness_mode(mode);
        let current = self.inner.source.mode(caller.user_id(), Some(session)).await?;
        let loosens = current.is_none_or(|current| current.is_stricter_than(target));
        if matches!(caller.origin, Origin::Agent { .. })
            || (loosens && !rules::may_loosen_mode(caller))
        {
            return Err(SurfaceError::NotHere(
                "letting agents do more on their own is a standing permission: change it in \
                 the home app, on your phone or in the terminal"
                    .into(),
            ));
        }
        self.transcript(caller, session).await?;
        self.audit_command(
            caller,
            SURFACE_AUDIT_SET_MODE,
            json!({ "sessionId": session, "mode": target.as_str() }),
        )
        .await?;
        self.inner.source.set_mode(caller, session, target).await?;
        Ok(CommandOutcome::ModeSet { mode: project::mode_view(target) })
    }

    // -----------------------------------------------------------------------
    // Audit (spec 3.3 §11)
    // -----------------------------------------------------------------------

    fn draft(caller: &Caller, kind: AuditRecordKind, action: &str) -> AuditRecordDraft {
        AuditRecordDraft::new(kind, action)
            .actor(AuditActor::subject(caller.subject.clone()))
            .ext("surface", serde_json::to_value(caller.surface).unwrap_or(Value::Null))
            .ext("origin", serde_json::to_value(&caller.origin).unwrap_or(Value::Null))
    }

    async fn audit_decide(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
        answer: Answer,
        outcome: &str,
        code: Option<&str>,
    ) -> Result<()> {
        let Some(trail) = &self.inner.audit else {
            return Ok(());
        };
        let decision = match (outcome, answer) {
            ("decided", Answer::Deny) => Some(AuditDecision::Rejected),
            ("decided", _) => Some(AuditDecision::Approved),
            _ => None,
        };
        let mut draft = Self::draft(caller, AuditRecordKind::Approval, SURFACE_AUDIT_DECIDE)
            .ext("approvalId", json!(request.id))
            .ext("sessionId", json!(request.scope.session_id))
            .ext("tool", json!(request.tool_name))
            .ext("answer", serde_json::to_value(answer).unwrap_or(Value::Null))
            .ext("outcome", json!(outcome));
        if let Some(decision) = decision {
            draft = draft.decision(decision);
        }
        if let Some(code) = code {
            draft = draft.ext("code", json!(code));
        }
        trail.record(draft).await.map_err(SurfaceError::Source)
    }

    async fn audit_command(&self, caller: &Caller, action: &str, fields: Value) -> Result<()> {
        let Some(trail) = &self.inner.audit else {
            return Ok(());
        };
        let kind = match action {
            SURFACE_AUDIT_STOP_ALL | SURFACE_AUDIT_RESUME_ALL => AuditRecordKind::Governance,
            SURFACE_AUDIT_SET_MODE => AuditRecordKind::Configuration,
            _ => AuditRecordKind::Other("surface".to_string()),
        };
        let mut draft = Self::draft(caller, kind, action);
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        trail.record(draft).await.map_err(SurfaceError::Source)
    }
}

/// The session-owner check every read and command makes (spec 3.3 §11 S-I8).
fn owns(caller: &Caller, request: &ApprovalRequest) -> bool {
    request.scope.user_id == caller.user_id()
}
