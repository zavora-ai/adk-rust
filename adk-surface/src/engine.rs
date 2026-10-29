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
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::agents::{AgentDetail, AgentLife, GrantedUse};
use crate::answers::{AnswerLog, AnswerRecord, InMemoryAnswerLog, SURFACE_AUDIT_DECIDE};
use crate::error::{Result, SurfaceError};
use crate::hints::{CardHint, CardHints};
use crate::model::{
    AgentRef, AgentRow, Briefing, Decided, DecisionCard, FeedEntry, Mode, ModeView, Reversibility,
    StatusLine,
};
use crate::origin::{Caller, Origin, SurfaceKind};
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
/// The audit action of a drawn form's Send (spec 3.3 §7.4 rule 5, §11).
pub const SURFACE_AUDIT_DRAWN_SUBMITTED: &str = "surface.drawn.submitted";
/// The audit action of an answer as it is given, written before it takes effect; its
/// `surface.decide` record says how it ended (spec 3.3 §11).
pub const SURFACE_AUDIT_ANSWER: &str = "surface.answer";

/// How often the feed reads the session store when the source has nothing faster.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The longest the delivery loop of queued input waits before it looks at the session again,
/// when nothing wakes it sooner. It waits as long as the session has an open decision or a
/// running turn, however long that is (spec 3.3 §7.3, S-I4).
const DRAIN_BACKOFF_MAX: Duration = Duration::from_secs(5);

tokio::task_local! {
    /// The transcript digest of the spoken answer being recorded: every `surface.decide`
    /// record written while it is in scope carries it, and none carries the words
    /// (spec 3.3 §5.6).
    static SPOKEN_DIGEST: String;
}

/// Runs `future` with the transcript digest `digest` in scope, when there is one.
async fn with_digest<F: std::future::Future>(digest: Option<String>, future: F) -> F::Output {
    match digest {
        Some(digest) => SPOKEN_DIGEST.scope(digest, future).await,
        None => future.await,
    }
}

/// A command from a surface (spec 3.3 §6). Every command runs as an authenticated
/// [`Caller`], whose subject, surface and [`Origin`] reach its audit record.
///
/// §6 also names "steer", a message that reaches a turn while it runs. No runtime under the
/// engine takes input in the middle of a turn, so there is no steer command: a message sent
/// during a turn is the session's next message ([`Send`](Self::Send)), and
/// [`StopAll`](Self::StopAll) stops a turn that has gone wrong.
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
    /// Starts a thread with `agent`: the session's first message goes to it, and its
    /// harness stamps the session as its own (spec 3.3 §5.7 "Threads").
    #[serde(rename_all = "camelCase")]
    NewThread {
        /// The agent: a roster row's name.
        agent: String,
        /// The new session.
        session_id: String,
        /// The first message.
        text: String,
    },
    /// Adds an agent by hand, previewed with `digest` (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    AddAgent {
        /// The definition.
        draft: Box<crate::agents::AgentDraft>,
        /// The digest of the preview the person confirmed.
        digest: String,
        /// The suggestion **Change it first** replaces, by the agent id it would get.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<String>,
    },
    /// Changes an agent at the version seen, previewed with `digest` (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    ChangeAgent {
        /// The agent.
        agent: String,
        /// The version seen.
        version: u64,
        /// The definition after the change.
        draft: Box<crate::agents::AgentDraft>,
        /// The digest of the preview the person confirmed.
        digest: String,
    },
    /// Pauses an agent (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    PauseAgent {
        /// The agent.
        agent: String,
    },
    /// Resumes an agent paused by hand: the web home or a paired local terminal (S-I10).
    #[serde(rename_all = "camelCase")]
    ResumeAgent {
        /// The agent.
        agent: String,
    },
    /// Removes an agent at the version seen, previewed with `digest` (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    RemoveAgent {
        /// The agent.
        agent: String,
        /// The version seen.
        version: u64,
        /// The digest of the preview that named what ends.
        digest: String,
    },
    /// Copies one of the person's standing rules to an agent, pinned to it (spec 3.3 §5.7
    /// "Copy one of your rules", S-I19).
    #[serde(rename_all = "camelCase")]
    CopyRuleToAgent {
        /// The agent.
        agent: String,
        /// The rule.
        rule_id: String,
    },
    /// Sends a form an agent drew, from the version the person saw (spec 3.3 §7.4 rule 5):
    /// never runs its tool, but asks for the call, which the drawing agent holds for the
    /// person's decision. Only a person at a screen sends one (`keyboard`, `pointer`,
    /// `touch`). Feature `genui`.
    #[serde(rename_all = "camelCase")]
    SubmitDrawn {
        /// The session the form is in.
        session_id: String,
        /// The drawn surface.
        surface_id: String,
        /// The version the person saw.
        version: u64,
        /// The declared action the person used.
        action_id: String,
        /// What the person typed or picked, by the field's data path (`/to`).
        #[serde(default)]
        values: serde_json::Map<String, Value>,
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
    /// The agent was added (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    AgentAdded {
        /// The agent now.
        agent: crate::agents::AgentDetail,
    },
    /// The agent was changed.
    #[serde(rename_all = "camelCase")]
    AgentChanged {
        /// The agent now.
        agent: crate::agents::AgentDetail,
    },
    /// The agent was paused.
    #[serde(rename_all = "camelCase")]
    AgentPaused {
        /// The agent now.
        agent: crate::agents::AgentDetail,
    },
    /// The agent runs again.
    #[serde(rename_all = "camelCase")]
    AgentResumed {
        /// The agent now.
        agent: crate::agents::AgentDetail,
    },
    /// The agent was removed; its threads stay.
    #[serde(rename_all = "camelCase")]
    AgentRemoved {
        /// What ended.
        ended: crate::agents::EndedWork,
    },
    /// The agent changed since the version the surface showed: show it again (spec 3.3 §5.7).
    #[serde(rename_all = "camelCase")]
    AgentOutOfDate {
        /// The agent now.
        agent: crate::agents::AgentDetail,
        /// "Finance was changed in the terminal at 10:01".
        text: String,
    },
    /// A rule was copied to the agent (spec 3.3 S-I19).
    #[serde(rename_all = "camelCase")]
    RuleCopied {
        /// The agent.
        agent: String,
        /// The rule's words.
        label: String,
    },
    /// A drawn form was sent: its call is on its way to the drawing agent, which holds it for
    /// the person's decision (spec 3.3 §7.4 rule 5). The form's state follows from that
    /// decision, never from this answer.
    #[serde(rename_all = "camelCase")]
    FormSent {
        /// The session.
        session_id: String,
        /// The drawn surface.
        surface_id: String,
        /// The version sent.
        version: u64,
        /// The call asked for.
        call_id: String,
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
    /// The store's history of the session was replaced (rewound, or deleted and started
    /// again): drop what was shown; the entries follow again from the start.
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
    stops_need: Option<String>,
    card_hints: Vec<Arc<dyn CardHints>>,
    granted: Vec<GrantedUse>,
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

    /// Adds `hints`, a feature's [`CardHints`], after those added before (spec 3.3 §7.1): for
    /// each field of a card the first hint that sets it wins, and [`heading`](Self::heading),
    /// [`money`](Self::money) and [`reversibility`](Self::reversibility) stay the fallback. A
    /// hint may only make a card stricter or add words; it never adds an answer or changes the
    /// risk (§7.2).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_core::ApprovalRequest;
    /// use adk_surface::{CardHint, CardHints, LocalSource, SurfaceEngine};
    ///
    /// struct Payments;
    ///
    /// #[async_trait::async_trait]
    /// impl CardHints for Payments {
    ///     async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
    ///         let mine = request.origin.as_ref()?.source == "mcp:payments";
    ///         mine.then(|| CardHint::new().spends_money())
    ///     }
    /// }
    ///
    /// # fn example(source: LocalSource) {
    /// let engine = SurfaceEngine::builder(Arc::new(source)).card_hints(Arc::new(Payments)).build();
    /// # let _ = engine;
    /// # }
    /// ```
    #[must_use]
    pub fn card_hints(mut self, hints: Arc<dyn CardHints>) -> Self {
        self.card_hints.push(hints);
        self
    }

    /// Grants every agent the home runs `granted`, shown on its detail under "On this home it
    /// can also:" (spec 3.3 §5.7, S-I12, S-I13). A host grants a use when it gives each agent
    /// it runs the tools for it; a second grant with the same id replaces the first.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::{GrantedUse, LocalSource, SurfaceEngine};
    ///
    /// # fn example(source: LocalSource) {
    /// let engine = SurfaceEngine::builder(Arc::new(source))
    ///     .grant(GrantedUse::new(
    ///         "genui",
    ///         "Show forms and tables in your threads — sending asks you first",
    ///     ))
    ///     .build();
    /// assert_eq!(engine.granted().len(), 1);
    /// # }
    /// ```
    #[must_use]
    pub fn grant(mut self, granted: GrantedUse) -> Self {
        match self.granted.iter_mut().find(|known| known.id == granted.id) {
            Some(known) => *known = granted,
            None => self.granted.push(granted),
        }
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

    /// Lets only callers in `group` give **Stop all** and **Resume all** (spec 3.3 §11
    /// S-I10). Both act on every run the source makes, whoever started it, so a server that
    /// admits more than one person keeps them for its operator; any other caller gets
    /// `surface.operator_only`. Without it, every caller may.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::{LocalSource, SurfaceEngine};
    ///
    /// # fn example(source: LocalSource) {
    /// let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
    /// # let _ = engine;
    /// # }
    /// ```
    #[must_use]
    pub fn stops_need(mut self, group: impl Into<String>) -> Self {
        self.stops_need = Some(group.into());
        self
    }

    /// The engine.
    pub fn build(mut self) -> SurfaceEngine {
        if !self.quiet_set {
            self.projector.quiet = self.source.quiet_hours();
        }
        // The source's rating counts beside the request's (spec 3.3 §7.2, spec 3.2 K-I9), so a
        // call a kernel rates high needs presence or a paired local terminal on every surface.
        let source = Arc::clone(&self.source);
        let money = Arc::clone(&self.projector.money);
        self.projector.money =
            Arc::new(move |request| money(request) || source.moves_money(request));
        let source = Arc::clone(&self.source);
        self.projector.rates_high = Arc::new(move |request| source.rates_high(request));
        // A drawn form's call never offers Always allow, whatever hints the host adds
        // (spec 3.3 §7.4 rule 5, wave 4 decision 12).
        #[cfg(feature = "genui")]
        let drawn = Arc::new(crate::genui::DrawnHints::new(
            Arc::clone(&self.source),
            self.projector.clone(),
        ));
        #[cfg(feature = "genui")]
        self.card_hints.insert(0, Arc::clone(&drawn) as Arc<dyn CardHints>);
        self.projector.voice_window_secs =
            u32::try_from(self.voice_window.as_millis().div_ceil(1000)).unwrap_or(u32::MAX);
        SurfaceEngine {
            inner: Arc::new(Inner {
                source: self.source,
                projector: self.projector,
                audit: self.audit,
                answers: self.answers.unwrap_or_else(|| Arc::new(InMemoryAnswerLog::new())),
                poll: self.poll,
                clock: self.clock,
                voice_window: self.voice_window,
                stops_need: self.stops_need,
                card_hints: self.card_hints,
                granted: self.granted,
                #[cfg(feature = "genui")]
                drawn,
                queues: Mutex::new(HashMap::new()),
                draining: Mutex::new(HashSet::new()),
                wake: Notify::new(),
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
/// source's stores, except input queued behind an open decision or a running turn, which
/// waits in this process and is sent, oldest first, once the session is free — whichever
/// surface answered the decision.
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
    /// The group Stop all and Resume all need, when only some callers may give them.
    stops_need: Option<String>,
    /// Features' card hints, in the order they were added (spec 3.3 §7.1).
    card_hints: Vec<Arc<dyn CardHints>>,
    /// What every agent the home runs may also do (spec 3.3 §5.7).
    granted: Vec<GrantedUse>,
    /// The cards of drawn forms this engine sent (spec 3.3 §7.4 rule 5).
    #[cfg(feature = "genui")]
    drawn: Arc<crate::genui::DrawnHints>,
    /// Input queued behind an open decision, per (user, session), oldest first.
    queues: Mutex<HashMap<(String, String), VecDeque<String>>>,
    /// Sessions with a delivery loop running. Changed only under the `queues` lock, so a
    /// message queued while a loop ends always finds a loop.
    draining: Mutex<HashSet<(String, String)>>,
    /// Wakes the delivery loops to look again: an answer or a message through this engine.
    wake: Notify,
    /// Spoken "allow once" answers inside their cancel window, by decision.
    spoken: Mutex<HashMap<String, Spoken>>,
}

/// A spoken "allow once" waiting out its cancel window.
struct Spoken {
    /// The task that commits it.
    handle: JoinHandle<()>,
    /// Which answer this is: the task commits only while its own entry is here.
    token: Arc<()>,
    /// Who said it: only they cancel it.
    user: String,
    /// The decision.
    request: ApprovalRequest,
    /// The digest of what was said, for the record of its cancel (spec 3.3 §5.6).
    digest: Option<String>,
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
            stops_need: None,
            card_hints: Vec::new(),
            granted: Vec::new(),
        }
    }

    /// The source.
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.inner.source
    }

    fn now(&self) -> DateTime<Utc> {
        self.inner.clock.now()
    }

    /// What every agent the home runs may also do, as the host granted it (spec 3.3 §5.7).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::{LocalSource, SurfaceEngine};
    ///
    /// # fn example(source: LocalSource) {
    /// let engine = SurfaceEngine::builder(Arc::new(source)).build();
    /// assert!(engine.granted().is_empty());
    /// # }
    /// ```
    pub fn granted(&self) -> &[GrantedUse] {
        &self.inner.granted
    }

    /// How long a spoken answer, or a spoken screen action, can be cancelled (spec 3.3 §5.6).
    #[cfg(feature = "verb-ui")]
    pub(crate) fn voice_cancel_window(&self) -> Duration {
        self.inner.voice_window
    }

    /// `detail` with what the home grants every agent it runs: only an agent that runs, added
    /// or paused, is granted anything (spec 3.3 §5.7).
    fn with_granted(&self, mut detail: AgentDetail) -> AgentDetail {
        detail.granted = match detail.state {
            AgentLife::Active | AgentLife::Paused => self.inner.granted.clone(),
            AgentLife::Suggested
            | AgentLife::Removing
            | AgentLife::Removed
            | AgentLife::Refused => Vec::new(),
        };
        detail
    }

    /// What the features say about `request`'s card, merged in the order they were added
    /// (spec 3.3 §7.1).
    async fn hint(&self, request: &ApprovalRequest) -> CardHint {
        let mut merged = CardHint::new();
        for hints in &self.inner.card_hints {
            if let Some(hint) = hints.hint(request).await {
                merged = merged.then(hint);
            }
        }
        merged
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
            ApprovalState::Pending => self.always_label(request).await,
            _ => None,
        };
        let answer = match request.state {
            ApprovalState::Pending => None,
            // Where an answer was given only adds to the card; the card is shown without it.
            _ => self.inner.answers.find_for(request).await.unwrap_or_else(|error| {
                tracing::warn!(error = %error, decision.id = %request.id, "where a decision was answered could not be read");
                None
            }),
        };
        let hint = self.hint(request).await;
        let mut card =
            self.inner.projector.card(request, caller, always, answer.as_ref(), self.now(), &hint);
        // How far a surface's answer reaches: once, or by the standing rule it made (§5.4).
        if let (Some(decided), Some(record)) = (card.decided.as_mut(), answer.as_ref())
            && decided.via.is_some()
        {
            decided.scope_text = match record.answer {
                Answer::AllowOnce => Some(words::DONE_ONCE.to_string()),
                Answer::AlwaysAllow => self
                    .inner
                    .source
                    .always_allow_offer(request)
                    .await
                    .map(|offer| lower_first(&offer.label)),
                Answer::Edit | Answer::Deny => None,
            };
        }
        // An added agent is drawn with its own name and colour (spec 3.3 §5.7).
        if let Some(detail) = self.inner.source.agent(caller.user_id(), &card.agent.name).await? {
            card.agent = detail.agent;
        }
        // "How it got here" for a suggested agent: the suggesting turn, naming what it read.
        if card.adds_agent.is_some()
            && let Ok(Some(events)) =
                self.inner.source.transcript(caller.user_id(), &request.scope.session_id).await
        {
            let projector = &self.inner.projector;
            let steps = crate::agents::suggestion_steps(
                &events,
                &card.agent.display_name,
                request.created_at,
                |at| projector.hh_mm(at),
            );
            let mut timeline: Vec<crate::model::TimelineStep> = steps
                .into_iter()
                .map(|text| crate::model::TimelineStep { text, current: false })
                .collect();
            // The suggesting turn's steps take the place of a hint's first step and "Asked at".
            let asked = usize::from(hint.first_step.is_some()) + 1;
            timeline.extend(card.timeline.into_iter().skip(asked));
            card.timeline = timeline;
        }
        Ok(card)
    }

    /// The caller's decisions answered or ended since `since`, newest first, for a surface
    /// that opens again after a while, such as the phone (spec 3.3 §5.4).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn decided_since(
        &self,
        caller: &Caller,
        since: DateTime<Utc>,
    ) -> Result<Vec<DecisionCard>> {
        let decided_at = |request: &ApprovalRequest| {
            request
                .decisions
                .last()
                .map(|decision| decision.decided_at)
                .or(request.resolved_at)
                .unwrap_or(request.created_at)
        };
        let mut requests: Vec<ApprovalRequest> = self
            .inner
            .source
            .requests(caller, None, false)
            .await?
            .into_iter()
            .filter(|request| {
                owns(caller, request)
                    && request.state != ApprovalState::Pending
                    && decided_at(request) >= since
            })
            .collect();
        requests.sort_by(|a, b| decided_at(b).cmp(&decided_at(a)).then_with(|| a.id.cmp(&b.id)));
        let mut cards = Vec::with_capacity(requests.len());
        for request in &requests {
            cards.push(self.card(caller, request).await?);
        }
        Ok(cards)
    }

    /// The label of "Always allow" for `request`, when the source can create its rule. A
    /// request that needs more than one approver never offers it: the rule would let every
    /// later call skip the other approvers (spec 3.3 §7.2, §8.2).
    async fn always_label(&self, request: &ApprovalRequest) -> Option<String> {
        if request.policy.quorum > 1 {
            return None;
        }
        self.inner.source.always_allow_offer(request).await.map(|offer| offer.label)
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
        let answered = project::answered_calls(&events);
        Ok(events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                self.inner.projector.entry(index as u64 + 1, event, &links, &answered)
            })
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
    ) -> Result<HashMap<u64, project::Link>> {
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
    /// While the stream runs, a history the store replaced — rewound, or deleted and started
    /// again — sends [`Reset`](FeedFrame::Reset) and the entries again from the start, whether
    /// it got shorter or grew back past what was sent. A client that resumes after `after`
    /// is trusted to hold the store's events up to it.
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
            /// The id of the event at `sent`, once this stream has read it.
            last: Option<String>,
            cards: Option<Vec<(String, u64)>>,
            buffer: VecDeque<FeedFrame>,
            /// The wait for the next change, begun before the last read so nothing that
            /// changes during a read is missed; `None` when a change came during the read.
            next: Option<BoxFuture<'static, ()>>,
            first: bool,
            done: bool,
        }
        let state = State {
            engine: self.clone(),
            caller,
            session,
            sent: after.unwrap_or(0),
            last: None,
            cards: None,
            buffer: VecDeque::new(),
            next: None,
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
                if !state.first
                    && let Some(next) = state.next.take()
                {
                    next.await;
                }
                state.first = false;
                let source = Arc::clone(&state.engine.inner.source);
                let poll = state.engine.inner.poll;
                let mut next: BoxFuture<'static, ()> =
                    Box::pin(async move { source.changed(poll).await });
                state.next = match futures::poll!(next.as_mut()) {
                    std::task::Poll::Ready(()) => None,
                    std::task::Poll::Pending => Some(next),
                };
                match state
                    .engine
                    .refresh(&state.caller, &state.session, state.sent, &state.last, &state.cards)
                    .await
                {
                    Ok((frames, sent, last, cards)) => {
                        state.sent = sent;
                        state.last = last;
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

    /// The frames since `sent`, and the new cursor, the id of the event at it, and the
    /// open-decision signature.
    async fn refresh(
        &self,
        caller: &Caller,
        session: &str,
        mut sent: u64,
        last: &Option<String>,
        cards: &Option<Vec<(String, u64)>>,
    ) -> Result<(Vec<FeedFrame>, u64, Option<String>, Vec<(String, u64)>)> {
        let events = self.transcript(caller, session).await?;
        let len = events.len() as u64;
        let mut frames = Vec::new();
        let at_cursor = usize::try_from(sent)
            .ok()
            .and_then(|sent| sent.checked_sub(1))
            .and_then(|index| events.get(index))
            .map(|event| &event.id);
        // Shorter than what was sent, or another event where the last one sent was: the
        // store's history was replaced.
        let replaced = last.as_ref().is_some_and(|last| at_cursor != Some(last));
        if sent > len || replaced {
            frames.push(FeedFrame::Reset { len });
            sent = 0;
        }
        if sent < len {
            let links = self.links(caller, session, &events).await?;
            let answered = project::answered_calls(&events);
            for (index, event) in
                events.iter().enumerate().skip(usize::try_from(sent).unwrap_or(usize::MAX))
            {
                let entry = self.inner.projector.entry(index as u64 + 1, event, &links, &answered);
                frames.push(FeedFrame::Entry { entry });
            }
            sent = len;
        }
        let last = usize::try_from(sent)
            .ok()
            .and_then(|sent| sent.checked_sub(1))
            .and_then(|index| events.get(index))
            .map(|event| event.id.clone());
        let open = self.session_decisions(caller, session).await?;
        let signature: Vec<(String, u64)> =
            open.iter().map(|card| (card.id.clone(), card.version)).collect();
        if cards.as_ref() != Some(&signature) {
            frames.push(FeedFrame::Decisions { cards: open });
        }
        Ok((frames, sent, last, signature))
    }

    /// The caller's agents (spec 3.3 §8.1).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn agents(&self, caller: &Caller) -> Result<Vec<AgentRow>> {
        let rows: Vec<AgentRow> = self
            .inner
            .source
            .agents(caller.user_id())
            .await?
            .iter()
            .map(|state| self.inner.projector.agent_row(state))
            .collect();
        // The person's own agents join the roster from their definitions, so one that has
        // never run is listed as Ready (spec 3.3 §5.7 "The roster").
        let definitions = self.inner.source.agent_definitions(caller.user_id()).await?;
        let can_change =
            !matches!(caller.origin, Origin::Agent { .. }) && rules::may_change_agents(caller);
        Ok(crate::agents::roster(rows, &definitions, can_change))
    }

    /// The agents `caller` removed, each with its threads, which stay readable (spec 3.3
    /// §5.7, §13 decision 18): what "Removed agents" at the roster's foot lists.
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn removed_agents(
        &self,
        caller: &Caller,
    ) -> Result<Vec<crate::agents::RemovedAgent>> {
        let source = &self.inner.source;
        let mut removed = Vec::new();
        for detail in source.agent_definitions(caller.user_id()).await? {
            if detail.state != crate::agents::AgentLife::Removed {
                continue;
            }
            let threads = source.agent_threads(caller.user_id(), &detail.id).await?;
            removed.push(crate::agents::RemovedAgent::of(
                &detail,
                threads,
                self.inner.projector.offset,
            ));
        }
        removed.sort_by(|a, b| {
            a.agent.display_name.to_lowercase().cmp(&b.agent.display_name.to_lowercase())
        });
        Ok(removed)
    }

    /// Every agent `caller` added, removed ones included, with its version: what the
    /// terminal's `--output json` compares, so a change to any field is one line (spec 3.3
    /// §5.7).
    ///
    /// # Errors
    ///
    /// Returns the source's error.
    pub async fn agent_details(&self, caller: &Caller) -> Result<Vec<crate::agents::AgentDetail>> {
        let details = self.inner.source.agent_definitions(caller.user_id()).await?;
        Ok(details.into_iter().map(|detail| self.with_granted(detail)).collect())
    }

    /// The agent a message goes to when the person names none (spec 3.3 §5.5): the tray's
    /// "Ask Finance". `None` when the source has no single primary agent
    /// ([`Source::primary_agent`]).
    pub fn primary_agent(&self) -> Option<AgentRef> {
        self.inner.source.primary_agent().map(|name| self.inner.projector.agent(&name))
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
            host_utc_offset_minutes: Some(
                self.now().with_timezone(&Local).offset().local_minus_utc() / 60,
            ),
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
            recorded: source.keeps_ledger(),
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
    /// | `surface.operator_only` | Stop all or Resume all from a caller outside the group [`stops_need`](SurfaceEngineBuilder::stops_need) names |
    /// | `surface.invalid` | A spoken answer: it goes through `decide_spoken` with its transcript (feature `voice`) |
    /// | `surface.unsupported` | The source cannot do it |
    /// | the source's code | The source failed |
    pub async fn execute(&self, caller: &Caller, command: Command) -> Result<CommandOutcome> {
        match command {
            // A spoken answer is recorded with its transcript's digest (spec 3.3 §5.6).
            Command::Decide { .. } if rules::row(caller) == Row::Voice => {
                Err(SurfaceError::Invalid(
                    "a spoken answer goes through decide_spoken with its transcript".into(),
                ))
            }
            Command::Decide { decision_id, version, answer, edited_args, reason, expanded } => {
                self.decide(
                    caller,
                    &decision_id,
                    version,
                    answer,
                    edited_args,
                    reason,
                    expanded,
                    None,
                )
                .await
            }
            Command::CancelAnswer { decision_id } => self.cancel_answer(caller, &decision_id).await,
            Command::Send { session_id, text } => self.send(caller, session_id, text, false).await,
            Command::Queue { session_id, text } => self.send(caller, session_id, text, true).await,
            Command::StopAll { reason } => {
                self.may_stop(caller)?;
                self.stop_all(caller, reason).await
            }
            Command::ResumeAll => {
                self.may_stop(caller)?;
                self.resume_all(caller).await
            }
            Command::SetMode { session_id, mode } => self.set_mode(caller, &session_id, mode).await,
            Command::NewThread { agent, session_id, text } => {
                self.new_thread(caller, &agent, session_id, text).await
            }
            command @ (Command::AddAgent { .. }
            | Command::ChangeAgent { .. }
            | Command::PauseAgent { .. }
            | Command::ResumeAgent { .. }
            | Command::RemoveAgent { .. }
            | Command::CopyRuleToAgent { .. }) => self.agent_command(caller, command).await,
            Command::SubmitDrawn { session_id, surface_id, version, action_id, values } => {
                self.submit_drawn(caller, session_id, surface_id, version, &action_id, &values)
                    .await
            }
        }
    }

    /// Sends a drawn form (spec 3.3 §7.4 rule 5), checked in order: the session is the
    /// caller's (S-I8), the caller is a person at a screen, the surface is at `version`, the
    /// action is declared, and the values fit their fields. Then it is recorded and the call
    /// is asked for; nothing runs until the person decides.
    #[cfg(feature = "genui")]
    async fn submit_drawn(
        &self,
        caller: &Caller,
        session: String,
        surface: String,
        version: u64,
        action: &str,
        values: &serde_json::Map<String, Value>,
    ) -> Result<CommandOutcome> {
        let events = self.transcript(caller, &session).await?;
        if !matches!(caller.origin, Origin::Keyboard | Origin::Pointer | Origin::Touch) {
            return Err(SurfaceError::NotHere(words::SEND_FORMS_FROM_A_SCREEN.to_string()));
        }
        let sent = crate::genui::submission(&events, &surface, version, action, values)?;
        // A running turn writes the session meanwhile, so the call waits until it ends, as a
        // message does: a second run would interleave its events with the turn's (S-I4).
        let key = (caller.user_id().to_string(), session.clone());
        let queued = self
            .inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .is_some_and(|queued| !queued.is_empty())
            || self.inner.draining.lock().unwrap_or_else(|e| e.into_inner()).contains(&key);
        if queued || self.inner.source.busy(caller.user_id(), &session).await? {
            return Err(SurfaceError::Invalid(words::SEND_FORM_WHEN_FREE.to_string()));
        }
        let call = adk_core::RequestedCall::new(
            sent.tool.clone(),
            sent.args.clone(),
            caller.subject.clone(),
            adk_core::RequestedVia::form(surface.clone(), version, action),
        );
        let call_id = call.function_call_id.clone();
        if !self.inner.drawn.begin_send(
            caller.user_id(),
            &session,
            (&surface, version),
            &call,
            &events,
        ) {
            return Err(SurfaceError::Invalid(words::FORM_BEING_SENT.to_string()));
        }
        // Recorded before it is asked for: a send whose record failed is never asked for. The
        // record carries digests, never the values: the arguments' digest is the one the
        // call's own approval records carry.
        let args_digest = self.inner.audit.as_ref().map(|trail| trail.digest(&sent.args));
        let asked = async {
            self.record(
                caller,
                SURFACE_AUDIT_DRAWN_SUBMITTED,
                json!({
                    "sessionId": session,
                    "surfaceId": surface,
                    "version": version,
                    "actionId": action,
                    "tool": sent.tool,
                    "argsDigest": args_digest,
                    "valuesDigest": sent.values_digest,
                    "callId": call_id,
                    "drawnBy": sent.drawn_by,
                }),
            )
            .await?;
            self.inner.source.request_call(caller, &session, "", call).await
        };
        if let Err(error) = asked.await {
            self.inner.drawn.withdraw(&call_id);
            return Err(error);
        }
        Ok(CommandOutcome::FormSent { session_id: session, surface_id: surface, version, call_id })
    }

    /// The session's events, when it is the caller's own (S-I8): what an app in it showed.
    #[cfg(feature = "genui-apps")]
    pub(crate) async fn app_transcript(
        &self,
        caller: &Caller,
        session: &str,
    ) -> Result<Vec<adk_core::Event>> {
        self.transcript(caller, session).await
    }

    /// Asks for `call`, a tool call the app from `app` asked for in `session` (spec 3.3 §6.1
    /// `genui-apps`): held as the agent whose call showed the app, for the person's decision,
    /// whose card says "The app from Weather asked" and never offers **Always allow**.
    #[cfg(feature = "genui-apps")]
    pub(crate) async fn ask_for_app_call(
        &self,
        caller: &Caller,
        session: &str,
        call: adk_core::RequestedCall,
        app: &str,
    ) -> Result<()> {
        self.inner.drawn.app_asked(caller.user_id(), session, &call, app);
        self.inner.source.request_call(caller, session, "", call).await
    }

    #[cfg(not(feature = "genui"))]
    async fn submit_drawn(
        &self,
        caller: &Caller,
        session: String,
        _surface: String,
        _version: u64,
        _action: &str,
        _values: &serde_json::Map<String, Value>,
    ) -> Result<CommandOutcome> {
        self.transcript(caller, &session).await?;
        Err(SurfaceError::unsupported("sending a form an agent drew"))
    }

    /// Answers a decision by voice (spec 3.3 §5.6, §7.2): the one way a spoken answer is
    /// recorded. `transcript` is what the person said; every `surface.decide` record of the
    /// answer carries its SHA-256 as `transcriptDigest`, and none carries the words.
    ///
    /// The voice row of §7.2 applies: **Allow once** for an everyday call, which waits out
    /// the cancel window ([`CommandOutcome::Pending`]; its end arrives on
    /// [`SpokenAnswer::settled`](crate::voice::SpokenAnswer::settled)), and **Deny** for any
    /// call. The call counts as not expanded, so a call shown truncated is decided on screen.
    ///
    /// # Errors
    ///
    /// `surface.invalid` when `caller` is not on the voice surface with origin voice; the
    /// codes of [`execute`](Self::execute) otherwise.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_surface::{Answer, Caller, CommandOutcome, Origin, SurfaceEngine, SurfaceKind};
    ///
    /// # async fn example(engine: SurfaceEngine, id: &str, version: u64) -> adk_surface::Result<()> {
    /// let ana = Caller::human("ana", SurfaceKind::Voice, Origin::Voice);
    /// let spoken = engine.decide_spoken(&ana, id, version, Answer::AllowOnce, "allow once").await?;
    /// if let CommandOutcome::Pending { text, .. } = &spoken.outcome {
    ///     println!("{text}");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "voice")]
    pub async fn decide_spoken(
        &self,
        caller: &Caller,
        decision_id: &str,
        version: u64,
        answer: Answer,
        transcript: &str,
    ) -> Result<crate::voice::SpokenAnswer> {
        use sha2::Digest as _;
        if caller.origin != Origin::Voice || caller.surface != crate::origin::SurfaceKind::Voice {
            return Err(SurfaceError::Invalid(
                "decide_spoken records a spoken answer: the caller is on the voice surface and \
                 its origin is voice"
                    .into(),
            ));
        }
        let digest = format!("{:x}", sha2::Sha256::digest(transcript.as_bytes()));
        let (settle, settled) = oneshot::channel();
        let outcome = with_digest(
            Some(digest),
            self.decide(caller, decision_id, version, answer, None, None, false, Some(settle)),
        )
        .await?;
        let settled = matches!(outcome, CommandOutcome::Pending { .. }).then_some(settled);
        Ok(crate::voice::SpokenAnswer { outcome, settled })
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
        settle: Option<oneshot::Sender<Result<CommandOutcome>>>,
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
        let always = self.always_label(&request).await;
        let hint = self.hint(&request).await;
        let facts = self.inner.projector.facts(&request, always, &hint);
        if let Err(violation) = rules::check(caller, &facts, answer, expanded) {
            let error = SurfaceError::Rule(violation);
            self.audit_decide(caller, &request, answer, "refused", Some(&error.code())).await?;
            return Err(error);
        }
        if answer == Answer::Edit && edited_args.is_none() {
            return Err(SurfaceError::Invalid("an edit needs the changed arguments".into()));
        }
        if rules::row(caller) == Row::Voice && answer == Answer::AllowOnce {
            // A spoken answer to a decision that changed since it was read is shown again at
            // once, as on every other surface, never promised and then dropped (S-I2).
            if request.version != version {
                self.audit_decide(
                    caller,
                    &request,
                    answer,
                    "changed",
                    Some("approval.version_conflict"),
                )
                .await?;
                let card = self.card(caller, &request).await?;
                return Ok(CommandOutcome::Changed { card });
            }
            return self.speak_later(caller, request, version, settle).await;
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
        // The answer is on record before it takes effect, and one that cannot be recorded is
        // not given (spec 3.3 §11).
        let mut given = json!({
            "approvalId": request.id,
            "sessionId": request.scope.session_id,
            "tool": request.tool_name,
            "answer": answer,
            "version": version,
        });
        if let Ok(digest) = SPOKEN_DIGEST.try_with(Clone::clone) {
            given["transcriptDigest"] = json!(digest);
        }
        self.record(caller, SURFACE_AUDIT_ANSWER, given).await?;
        match self.inner.source.decide(&request.id, version, decision).await {
            Ok(updated) => {
                // The decision is recorded and cannot be taken back: nothing after this point
                // fails the command or keeps the decided call and the queued input waiting.
                let resolved = updated.state != ApprovalState::Pending;
                // The standing rule only once the request was approved: an approval that a
                // quorum still waits on has not won (spec 3.3 §8.2).
                if let Some(rule) = rule.filter(|_| updated.state == ApprovalState::Approved)
                    && let Err(error) = rule.commit().await
                {
                    tracing::warn!(error = %error, decision.id = %updated.id, "the call was allowed once; its standing rule could not be created");
                }
                if resolved {
                    let record = AnswerRecord {
                        approval_id: updated.id.clone(),
                        by: caller.subject.clone(),
                        via: caller.via(),
                        answer,
                        at: self.now(),
                    };
                    if let Err(error) = self.inner.answers.record(&record).await {
                        tracing::warn!(error = %error, decision.id = %updated.id, "where the answer was given could not be kept");
                    }
                }
                if let Err(error) =
                    self.audit_decide(caller, &updated, answer, "decided", None).await
                {
                    tracing::warn!(error = %error, decision.id = %updated.id, "the answer took effect; its audit record failed");
                }
                if resolved {
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
        let hint = self.hint(request).await;
        let mut card = self.inner.projector.card(request, caller, None, None, self.now(), &hint);
        // When it was decided is not known either: `at` is when this surface found out, and
        // no time is shown.
        let decided = Decided {
            by: String::new(),
            by_you: false,
            via: None,
            at: self.now(),
            at_text: String::new(),
            answer: crate::model::DecidedAnswer::Unknown,
            text: words::DECIDED.to_string(),
            scope_text: None,
        };
        card.status = crate::model::CardStatus::Decided;
        card.status_text = words::DECIDED.to_string();
        card.allowed.options.clear();
        card.allowed.open_in_home = false;
        card.allowed.cancel_window_secs = None;
        card.allowed.note = None;
        if let Some(step) = card.timeline.last_mut() {
            step.text = words::DECIDED.to_string();
        }
        card.decided = Some(decided.clone());
        Ok(CommandOutcome::AlreadyDecided { decided, card })
    }

    /// A spoken "allow once" waits out its cancel window, then commits (spec 3.3 §5.6, §7.2);
    /// how the window ended goes to `settle`.
    async fn speak_later(
        &self,
        caller: &Caller,
        request: ApprovalRequest,
        version: u64,
        settle: Option<oneshot::Sender<Result<CommandOutcome>>>,
    ) -> Result<CommandOutcome> {
        let window = self.inner.voice_window;
        let cancel_until = self.now()
            + chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(5));
        self.audit_decide(caller, &request, Answer::AllowOnce, "pending", None).await?;
        let digest = SPOKEN_DIGEST.try_with(Clone::clone).ok();
        let engine = self.clone();
        let speaker = caller.clone();
        let id = request.id.clone();
        let task_id = id.clone();
        let spoken = request.clone();
        let token = Arc::new(());
        let mine = Arc::clone(&token);
        let (go, entered) = oneshot::channel::<()>();
        let handle = tokio::spawn(with_digest(digest.clone(), async move {
            // Its entry is in place before the window starts, so a cancel always finds it.
            if entered.await.is_err() {
                return;
            }
            tokio::time::sleep(window).await;
            // Cancel and Stop all remove the entry before they abort the task: an answer whose
            // entry is gone, or was replaced, is withdrawn, whatever the abort's timing.
            let withdrawn = {
                let mut spoken = engine.inner.spoken.lock().unwrap_or_else(|e| e.into_inner());
                let current =
                    spoken.get(&task_id).is_some_and(|entry| Arc::ptr_eq(&entry.token, &mine));
                if current {
                    spoken.remove(&task_id);
                }
                !current
            };
            if withdrawn {
                return;
            }
            // Everything stopped meanwhile, here or on another surface: nothing runs.
            let result = if engine.inner.source.stopped().await.unwrap_or(false) {
                if let Err(error) = engine
                    .audit_decide(
                        &speaker,
                        &request,
                        Answer::AllowOnce,
                        "cancelled",
                        Some("surface.stopped"),
                    )
                    .await
                {
                    tracing::warn!(error = %error, decision.id = %task_id, "a spoken answer was cancelled; its audit record failed");
                }
                Err(SurfaceError::NotHere(words::ALL_AGENTS_STOPPED.to_string()))
            } else {
                engine.commit(&speaker, &request, version, Answer::AllowOnce, None, None).await
            };
            match &result {
                Ok(CommandOutcome::Answered { .. }) => {}
                Ok(outcome) => {
                    tracing::info!(decision.id = %task_id, outcome = ?outcome, "a spoken answer was not recorded: the decision changed in its cancel window");
                }
                Err(error) => {
                    tracing::warn!(error = %error, decision.id = %task_id, "a spoken answer could not be recorded");
                }
            }
            if let Some(settle) = settle {
                let _ = settle.send(result);
            }
        }));
        let entry =
            Spoken { handle, token, user: caller.user_id().to_string(), request: spoken, digest };
        if let Some(previous) =
            self.inner.spoken.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), entry)
        {
            previous.handle.abort();
        }
        let _ = go.send(());
        Ok(CommandOutcome::Pending {
            decision_id: id,
            cancel_until,
            text: words::allowing_in(
                u64::try_from(window.as_millis().div_ceil(1000)).unwrap_or(u64::MAX),
            ),
        })
    }

    /// Cancels a spoken answer at once, without reading the source, so a cancel never waits
    /// on a store; like a stop, it is recorded after, and a failed record does not undo it.
    async fn cancel_answer(&self, caller: &Caller, id: &str) -> Result<CommandOutcome> {
        let spoken = {
            let mut spoken = self.inner.spoken.lock().unwrap_or_else(|e| e.into_inner());
            match spoken.get(id) {
                Some(entry) if entry.user == caller.user_id() => spoken.remove(id),
                _ => None,
            }
        };
        let Some(spoken) = spoken else {
            return Err(SurfaceError::not_found(format!("a spoken answer to '{id}' to cancel")));
        };
        spoken.handle.abort();
        if let Err(error) = with_digest(
            spoken.digest,
            self.audit_decide(caller, &spoken.request, Answer::AllowOnce, "cancelled", None),
        )
        .await
        {
            tracing::warn!(error = %error, decision.id = %id, "the spoken answer was cancelled; its audit record failed");
        }
        Ok(CommandOutcome::AnswerCancelled { decision_id: id.to_string() })
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
        let key = (caller.user_id().to_string(), session.clone());
        let open = self.session_open(caller, &session).await?;
        // Earlier queued input goes first, including a message the delivery loop is starting
        // a turn for right now: a new message never overtakes it (S-I4).
        let earlier = {
            let queues = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
            queues.get(&key).is_some_and(|queued| !queued.is_empty())
                || self.inner.draining.lock().unwrap_or_else(|e| e.into_inner()).contains(&key)
        };
        // A running turn takes the message next, as an open decision does (S-I4).
        let busy = !open && !earlier && self.inner.source.busy(caller.user_id(), &session).await?;
        if open || earlier || busy || queue {
            // Recorded before it is queued: a message whose record failed is never sent.
            self.audit_command(
                caller,
                SURFACE_AUDIT_SEND,
                json!({ "sessionId": session, "queued": true }),
            )
            .await?;
            let position = {
                let mut queues = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                let entry = queues.entry(key).or_default();
                entry.push_back(text);
                u32::try_from(entry.len()).unwrap_or(u32::MAX)
            };
            self.spawn_drain(caller.clone(), session.clone());
            let text = if open {
                words::SENT_AFTER_THIS_CARD_IS_ANSWERED
            } else {
                words::SENT_WHEN_YOUR_AGENT_IS_FREE
            };
            return Ok(CommandOutcome::Queued {
                session_id: session,
                position,
                text: text.to_string(),
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

    /// Whether the session has a decision open: one the caller may answer, or a call held for
    /// someone else (§7.4 rule 2).
    async fn session_open(&self, caller: &Caller, session: &str) -> Result<bool> {
        let own =
            self.inner.source.requests(caller, Some(session), true).await?.iter().any(|request| {
                owns(caller, request)
                    && request.state == ApprovalState::Pending
                    && request.scope.session_id == session
            });
        Ok(own || self.inner.source.waiting_on_decision(caller.user_id(), session).await?)
    }

    /// Delivers the session's queued input, oldest first and one turn at a time, once it has
    /// no open decision and no running turn (S-I4). The loop waits for as long as that takes,
    /// whichever surface answers the decision, and keeps a message whose turn could not start
    /// at the front of the queue.
    fn spawn_drain(&self, caller: Caller, session: String) {
        let key = (caller.user_id().to_string(), session.clone());
        {
            let queues = self.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
            if queues.get(&key).is_none_or(VecDeque::is_empty) {
                return;
            }
            if !self.inner.draining.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
                // The loop already runs: have it look again now.
                self.inner.wake.notify_waiters();
                return;
            }
        }
        let engine = self.clone();
        tokio::spawn(async move {
            let poll = engine.inner.poll;
            let mut wait = poll;
            loop {
                let woken = engine.inner.wake.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                {
                    let queues = engine.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                    if queues.get(&key).is_none_or(VecDeque::is_empty) {
                        engine
                            .inner
                            .draining
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&key);
                        return;
                    }
                }
                let free = match engine.session_open(&caller, &session).await {
                    Ok(true) => Ok(false),
                    Ok(false) => {
                        engine.inner.source.busy(caller.user_id(), &session).await.map(|busy| !busy)
                    }
                    Err(error) => Err(error),
                };
                match free {
                    Ok(true) => {}
                    Ok(false) => {
                        tokio::select! {
                            () = engine.inner.source.changed(wait) => {}
                            () = &mut woken => {}
                        }
                        wait = (wait * 2).min(DRAIN_BACKOFF_MAX);
                        continue;
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, session.id = %session, "queued input waits: the session could not be read");
                        tokio::select! {
                            () = tokio::time::sleep(wait) => {}
                            () = &mut woken => {}
                        }
                        wait = (wait * 2).min(DRAIN_BACKOFF_MAX);
                        continue;
                    }
                }
                let next = engine
                    .inner
                    .queues
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&key)
                    .and_then(VecDeque::pop_front);
                let Some(text) = next else { continue };
                match engine.inner.source.start_turn(&caller, &session, &text).await {
                    Ok(()) => {
                        wait = poll;
                        // Let the turn begin before looking again.
                        tokio::time::sleep(poll).await;
                    }
                    Err(error) => {
                        {
                            let mut queues =
                                engine.inner.queues.lock().unwrap_or_else(|e| e.into_inner());
                            queues.entry(key.clone()).or_default().push_front(text);
                            if !transient(&error) {
                                // It stays queued and shown; the next message or answer tries
                                // again.
                                tracing::warn!(error = %error, session.id = %session, "queued input could not be sent; it stays queued");
                                engine
                                    .inner
                                    .draining
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .remove(&key);
                                return;
                            }
                        }
                        tracing::warn!(error = %error, session.id = %session, "queued input could not be sent yet; trying again");
                        tokio::select! {
                            () = tokio::time::sleep(wait) => {}
                            () = &mut woken => {}
                        }
                        wait = (wait * 2).min(DRAIN_BACKOFF_MAX);
                    }
                }
            }
        });
    }

    /// Takes back the newest input queued for the session, before it is sent; `None` when
    /// none waits (spec 3.3 §7.4 rule 2).
    pub fn unqueue_last(&self, caller: &Caller, session: &str) -> Option<String> {
        self.inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&(caller.user_id().to_string(), session.to_string()))
            .and_then(VecDeque::pop_back)
    }

    /// The input queued for the session, oldest first, as the person reads it: an app's
    /// suggestion is its words and label.
    pub fn queued(&self, caller: &Caller, session: &str) -> Vec<String> {
        self.inner
            .queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(caller.user_id().to_string(), session.to_string()))
            .map(|queue| {
                queue
                    .iter()
                    .map(|text| {
                        crate::genui::suggested_message(text).unwrap_or_else(|| text.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Looks at once whether the session's queued input can be sent, after an answer
    /// recorded outside [`execute`](Self::execute), such as a take over (spec 3.3 §7.4 rule 2).
    /// Queued input is sent without it too, once the session is free.
    pub fn deliver_queued(&self, caller: &Caller, session: &str) {
        self.spawn_drain(caller.clone(), session.to_string());
    }

    /// Stop all and Resume all reach every run, so they need the group set with
    /// [`SurfaceEngineBuilder::stops_need`] (spec 3.3 §11 S-I10).
    fn may_stop(&self, caller: &Caller) -> Result<()> {
        match &self.inner.stops_need {
            Some(group) if !caller.groups.iter().any(|held| held == group) => {
                Err(SurfaceError::OperatorOnly { group: group.clone() })
            }
            _ => Ok(()),
        }
    }

    async fn stop_all(&self, caller: &Caller, reason: Option<String>) -> Result<CommandOutcome> {
        let reason = reason.unwrap_or_else(|| words::STOP_ALL.to_string());
        // A spoken "allow once" still in its cancel window is withdrawn first: Stop all is the
        // strongest withdrawal (S-I10).
        let cancelled: Vec<String> = {
            let mut spoken = self.inner.spoken.lock().unwrap_or_else(|e| e.into_inner());
            spoken
                .drain()
                .map(|(id, entry)| {
                    entry.handle.abort();
                    id
                })
                .collect()
        };
        // Stopping never waits on the audit log (spec 3.2 K-I8): record after, warn on failure.
        self.inner.source.stop_all(caller, &reason).await?;
        let mut fields = json!({ "reason": reason });
        if !cancelled.is_empty() {
            fields["cancelledAnswers"] = json!(cancelled);
        }
        if let Err(error) = self.audit_command(caller, SURFACE_AUDIT_STOP_ALL, fields).await {
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
        // In an added agent's own thread the mode can only make it ask more (spec 3.3 §5.7).
        if let Some(agent) = self.inner.source.agent_of_session(caller.user_id(), session).await?
            && let Some(detail) = self.inner.source.agent(caller.user_id(), &agent).await?
            && project::harness_mode(detail.mode.mode).is_stricter_than(target)
        {
            return Err(SurfaceError::NotHere(crate::agents::words::asks_at_most(
                &detail.draft.display_name,
                &detail.mode.label,
            )));
        }
        // A thread with no message yet takes a mode where the source opens it for the caller,
        // as a kernel's `/mode` does; elsewhere only the caller's own session does (S-I8).
        if self.inner.source.transcript(caller.user_id(), session).await?.is_none()
            && !self.inner.source.opens_sessions()
        {
            return Err(SurfaceError::not_found(format!("session '{session}'")));
        }
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
    // Agents a person adds (spec 3.3 §5.7)
    // -----------------------------------------------------------------------

    /// Whether the roster ends with **New agent** for `caller` (spec 3.3 §5.7).
    pub fn can_add_agents(&self, caller: &Caller) -> bool {
        self.inner.source.keeps_agents()
            && !matches!(caller.origin, Origin::Agent { .. })
            && rules::may_change_agents(caller)
    }

    /// One agent `caller` added (spec 3.3 §5.7 "Agent settings").
    ///
    /// # Errors
    ///
    /// `surface.not_found` when the caller has no such agent, or the source's error.
    pub async fn agent(&self, caller: &Caller, id: &str) -> Result<crate::agents::AgentDetail> {
        self.inner
            .source
            .agent(caller.user_id(), id)
            .await?
            .map(|detail| self.with_granted(detail))
            .ok_or_else(|| SurfaceError::not_found(format!("agent '{id}'")))
    }

    /// What `caller` may choose for an agent.
    ///
    /// # Errors
    ///
    /// `surface.not_here` where agents are not changed, or the source's error.
    pub async fn agent_choices(&self, caller: &Caller) -> Result<crate::agents::AgentChoices> {
        Self::may_change_agents(caller)?;
        self.inner.source.agent_choices().await
    }

    /// **Here is what Finance can do**: the preview of `request` (spec 3.3 §5.7).
    ///
    /// # Errors
    ///
    /// `surface.not_here` where agents are not changed, or the kernel's refusal.
    pub async fn preview_agent(
        &self,
        caller: &Caller,
        request: crate::agents::AgentPreviewRequest,
    ) -> Result<crate::agents::AgentPreview> {
        Self::may_change_agents(caller)?;
        self.inner.source.preview_agent(caller, request).await
    }

    /// Refuses a caller who may not change agents here: an agent, or a surface other than the
    /// web home and the terminal (spec 3.3 §5.7, §7.2).
    fn may_change_agents(caller: &Caller) -> Result<()> {
        if let Origin::Agent { agent } = &caller.origin {
            return Err(SurfaceError::NotHere(format!(
                "an agent ({agent}) never adds, changes, pauses or removes an agent; a person \
                 does (spec 3.3 §5.7)"
            )));
        }
        if !rules::may_change_agents(caller) {
            return Err(SurfaceError::NotHere(
                "agents are set up and changed in the home app or the terminal (spec 3.3 §7.2)"
                    .into(),
            ));
        }
        Ok(())
    }

    async fn new_thread(
        &self,
        caller: &Caller,
        agent: &str,
        session: String,
        text: String,
    ) -> Result<CommandOutcome> {
        if text.trim().is_empty() {
            return Err(SurfaceError::Invalid("the message is empty".into()));
        }
        if let Origin::Agent { agent } = &caller.origin {
            return Err(SurfaceError::NotHere(format!(
                "an agent ({agent}) never starts a thread for a person (spec 3.3 §7.4)"
            )));
        }
        self.audit_command(
            caller,
            SURFACE_AUDIT_SEND,
            json!({ "sessionId": session, "queued": false, "agent": agent }),
        )
        .await?;
        self.inner.source.start_turn_with(caller, agent, &session, &text).await?;
        Ok(CommandOutcome::Sent { session_id: session })
    }

    async fn agent_command(&self, caller: &Caller, command: Command) -> Result<CommandOutcome> {
        Self::may_change_agents(caller)?;
        let source = &self.inner.source;
        let agent_of = |command: &Command| match command {
            Command::ChangeAgent { agent, .. }
            | Command::PauseAgent { agent }
            | Command::ResumeAgent { agent }
            | Command::RemoveAgent { agent, .. }
            | Command::CopyRuleToAgent { agent, .. } => Some(agent.clone()),
            _ => None,
        };
        let agent = agent_of(&command);
        let result = match command {
            Command::AddAgent { draft, digest, replaces } => source
                .add_agent(caller, *draft, &digest, replaces)
                .await
                .map(|agent| CommandOutcome::AgentAdded { agent: self.with_granted(agent) }),
            Command::ChangeAgent { agent, version, draft, digest } => source
                .change_agent(caller, &agent, version, *draft, &digest)
                .await
                .map(|agent| CommandOutcome::AgentChanged { agent: self.with_granted(agent) }),
            Command::PauseAgent { agent } => source
                .pause_agent(caller, &agent)
                .await
                .map(|agent| CommandOutcome::AgentPaused { agent: self.with_granted(agent) }),
            Command::ResumeAgent { agent } => {
                // Resuming is the person at the home or a paired local terminal (S-I10).
                if !rules::may_resume_agent(caller) {
                    return Err(SurfaceError::NotHere(
                        words::ALL_AGENTS_STOPPED_RESUME_IN_HOME.to_string(),
                    ));
                }
                source
                    .resume_agent(caller, &agent)
                    .await
                    .map(|agent| CommandOutcome::AgentResumed { agent: self.with_granted(agent) })
            }
            Command::RemoveAgent { agent, version, digest } => source
                .remove_agent(caller, &agent, version, &digest)
                .await
                .map(|ended| CommandOutcome::AgentRemoved { ended }),
            Command::CopyRuleToAgent { agent, rule_id } => source
                .copy_rule_to_agent(caller, &agent, &rule_id)
                .await
                .map(|label| CommandOutcome::RuleCopied { agent, label }),
            _ => return Err(SurfaceError::Invalid("not a command about an agent".into())),
        };
        match (result, agent) {
            // Someone changed it since: shown again with what it is now, never overwritten.
            (Err(error), Some(agent)) if crate::agents::local::out_of_date(&error) => {
                let detail = self.agent(caller, &agent).await?;
                // Where it was changed: "in the terminal", "in the home app", "on your phone".
                let place = detail
                    .changed_on
                    .as_deref()
                    .and_then(|surface| serde_json::from_value::<SurfaceKind>(json!(surface)).ok())
                    .map_or_else(
                        || "elsewhere".to_string(),
                        |surface| crate::origin::Via { surface, origin: Origin::Keyboard }.phrase(),
                    );
                let text = crate::agents::words::changed_in(
                    &detail.draft.display_name,
                    &place,
                    &self.inner.projector.hh_mm(detail.changed_at),
                );
                Ok(CommandOutcome::AgentOutOfDate { agent: detail, text })
            }
            (result, _) => result,
        }
    }

    // -----------------------------------------------------------------------
    // Audit (spec 3.3 §11)
    // -----------------------------------------------------------------------

    fn draft(caller: &Caller, kind: AuditRecordKind, action: &str) -> AuditRecordDraft {
        let mut draft = AuditRecordDraft::new(kind, action)
            .actor(AuditActor::subject(caller.subject.clone()))
            .ext("surface", serde_json::to_value(caller.surface).unwrap_or(Value::Null))
            .ext("origin", serde_json::to_value(&caller.origin).unwrap_or(Value::Null));
        if let Some(device) = &caller.device {
            draft = draft.ext("device", json!(device));
        }
        if caller.presence_confirmed {
            draft = draft.ext("presence", json!("confirmed"));
        }
        draft
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
        // Whose call it was, so the record can say "You let Finance send one email" (§11).
        if let Some(acting) = &request.acting {
            draft = draft.ext("actingAgent", json!(acting.agent.id.to_string()));
        }
        // What the call acts on, as a feature names it, so a denial names its item as the
        // change's own record does: "Finance asked to snooze News digest" (§8, §11).
        if let Some(title) = crate::hints::named(&self.inner.card_hints, request).await {
            draft = draft.ext("title", json!(title)).ext("summary", json!(request.summary));
        }
        if let Some(code) = code {
            draft = draft.ext("code", json!(code));
        }
        if let Ok(digest) = SPOKEN_DIGEST.try_with(Clone::clone) {
            draft = draft.ext("transcriptDigest", json!(digest));
        }
        trail.record(draft).await.map_err(SurfaceError::Source)
    }

    /// The record of a command `caller` gave (spec 3.3 §11): Stop all and Resume all as
    /// governance, a mode change as configuration, every other command through
    /// [`record`](Self::record).
    pub(crate) async fn audit_command(
        &self,
        caller: &Caller,
        action: &str,
        fields: Value,
    ) -> Result<()> {
        let kind = match action {
            SURFACE_AUDIT_STOP_ALL | SURFACE_AUDIT_RESUME_ALL => AuditRecordKind::Governance,
            SURFACE_AUDIT_SET_MODE => AuditRecordKind::Configuration,
            _ => return self.record(caller, action, fields).await,
        };
        self.write_record(caller, kind, action, fields).await
    }

    /// Writes the record `action` of what `caller` did through a surface, with `fields` as its
    /// extensions (spec 3.3 §11): stamped with the caller's surface, origin, device and
    /// presence as every surface record is, of kind `Other("surface")`, which the kernel's
    /// surface-records route accepts. A feature records what a person did with it here, such
    /// as `surface.verb`. Without an audit trail nothing is written.
    pub(crate) async fn record(&self, caller: &Caller, action: &str, fields: Value) -> Result<()> {
        self.write_record(caller, AuditRecordKind::Other("surface".to_string()), action, fields)
            .await
    }

    async fn write_record(
        &self,
        caller: &Caller,
        kind: AuditRecordKind,
        action: &str,
        fields: Value,
    ) -> Result<()> {
        let Some(trail) = &self.inner.audit else {
            return Ok(());
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

/// Whether a turn that could not start may start later: the source was busy, unavailable or
/// over a limit, not refusing the request itself.
fn transient(error: &SurfaceError) -> bool {
    use adk_core::ErrorCategory;
    match error {
        SurfaceError::Source(error) => match error.category {
            ErrorCategory::RateLimited
            | ErrorCategory::Timeout
            | ErrorCategory::Unavailable
            | ErrorCategory::Cancelled
            | ErrorCategory::Internal
            | ErrorCategory::ResourceExhausted
            | ErrorCategory::Conflict => true,
            ErrorCategory::InvalidInput
            | ErrorCategory::Unauthorized
            | ErrorCategory::Forbidden
            | ErrorCategory::NotFound
            | ErrorCategory::Unsupported => false,
        },
        SurfaceError::NotFound { .. }
        | SurfaceError::Rule(_)
        | SurfaceError::NotHere(_)
        | SurfaceError::OperatorOnly { .. }
        | SurfaceError::Unsupported { .. }
        | SurfaceError::Invalid(_) => false,
    }
}

/// `label` with its first letter in lower case, to follow a "·" in a sentence.
fn lower_first(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The session-owner check every read and command makes (spec 3.3 §11 S-I8).
fn owns(caller: &Caller, request: &ApprovalRequest) -> bool {
    request.scope.user_id == caller.user_id()
}

#[cfg(test)]
mod tests {
    use adk_core::audit_log::InMemoryAuditRecorder;
    use adk_core::{InMemoryApprovalStore, StoreApprovalService};
    use adk_session::InMemorySessionService;

    use super::*;
    use crate::LocalSource;

    /// A feature's record carries who did it, where and how, as every surface record does,
    /// under the kind the kernel's surface-records route accepts (spec 3.3 §11).
    #[tokio::test]
    async fn a_feature_record_is_stamped_like_every_surface_record() {
        let source = LocalSource::builder("adk-rust")
            .sessions(Arc::new(InMemorySessionService::new()))
            .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
            .build()
            .unwrap();
        let audit = InMemoryAuditRecorder::new();
        let engine = SurfaceEngine::builder(Arc::new(source))
            .audit(AuditTrail::new(Arc::new(audit.clone())))
            .build();
        let caller = Caller::human("ana", SurfaceKind::Phone, Origin::Touch)
            .with_device("dev-1")
            .with_presence_confirmed();

        engine.record(&caller, "surface.verb", json!({ "verb": "snooze" })).await.unwrap();

        let expected =
            AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), "surface.verb")
                .actor(AuditActor::subject(caller.subject.clone()))
                .ext("surface", json!("phone"))
                .ext("origin", json!({ "kind": "touch" }))
                .ext("device", json!("dev-1"))
                .ext("presence", json!("confirmed"))
                .ext("verb", json!("snooze"));
        assert_eq!(audit.records(), vec![expected]);
    }
}
