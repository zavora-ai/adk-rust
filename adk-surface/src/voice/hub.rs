//! Where voice conversations open (spec 3.3 §5.6).

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(feature = "web")]
use std::sync::Mutex;
use std::time::Duration;

use adk_core::{ErrorCategory, RequestContext};
use adk_harness::{Harness, VoiceConfig, VoiceSurface};
use adk_realtime::BoxedModel;
use adk_session::{CreateRequest, GetRequest};
use serde_json::{Value, json};
#[cfg(feature = "web")]
use tokio::time::Instant;

use super::VoiceError;
use super::session::{self, Conversation, VoiceLink};
use super::source::LiveVoice;
use super::visual::{FramePolicy, VisualInputs};
use super::wire::{VoiceModel, VoiceModels};
use crate::engine::{DEFAULT_POLL_INTERVAL, SurfaceEngine};
use crate::origin::{Caller, Origin, SurfaceKind};

/// The session state key that marks a session as a voice conversation (spec 3.3 §5.6, S-I8).
const VOICE_SESSION_KEY: &str = "surfaceVoice";

/// How long a ticket for the voice socket works (spec 3.3 §5.6).
#[cfg(feature = "web")]
pub(crate) const TICKET_LIFETIME: Duration = Duration::from_secs(30);

/// What the model is told on top of the harness's voice instruction: the person's own words
/// answer decisions, never the model's (spec 3.3 §5.6, §7.4 rule 6).
const NEVER_DECIDE: &str = "Decisions on waiting calls are shown on the person's screen. \
     The app applies the person's own spoken words to the decision on screen: they say \
     \"allow once\" or \"deny\". \"Yes\" and \"no\" decide nothing, so when a call waits, ask \
     them to say \"allow once\" or \"deny\". Never approve or deny a call yourself, never say \
     that a call was approved, and never repeat a call that is waiting.";

/// A model offered for voice.
struct Offered {
    info: VoiceModel,
    model: BoxedModel,
    frames: FramePolicy,
    /// Gemini Live context window compression, since the camera or the screen may be on.
    compress: bool,
}

/// A ticket for the voice socket: single use, for one caller, for 30 seconds.
#[cfg(feature = "web")]
#[derive(Debug, Clone)]
pub(crate) struct Ticket {
    pub(crate) caller: Caller,
    pub(crate) model: String,
    pub(crate) session_id: String,
    expires: Instant,
}

/// The tickets issued and not yet taken.
#[cfg(feature = "web")]
#[derive(Default)]
pub(crate) struct Tickets {
    open: Mutex<HashMap<String, Ticket>>,
}

#[cfg(feature = "web")]
impl Tickets {
    /// A new single-use ticket for `caller`'s conversation on `model` in `session_id`: 256
    /// random bits, good for [`TICKET_LIFETIME`].
    pub(crate) fn issue(&self, caller: &Caller, model: &str, session_id: &str) -> String {
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let now = Instant::now();
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|_, ticket| ticket.expires > now);
        open.insert(
            token.clone(),
            Ticket {
                caller: caller.clone(),
                model: model.to_string(),
                session_id: session_id.to_string(),
                expires: now + TICKET_LIFETIME,
            },
        );
        token
    }

    /// Takes the ticket `token`, once, while it has not expired.
    pub(crate) fn redeem(&self, token: &str) -> Option<Ticket> {
        let ticket = self.open.lock().unwrap_or_else(|e| e.into_inner()).remove(token)?;
        (ticket.expires > Instant::now()).then_some(ticket)
    }
}

struct Hub {
    engine: SurfaceEngine,
    harness: Arc<Harness>,
    live: LiveVoice,
    models: Vec<Offered>,
    agent: String,
    instruction: Option<String>,
    poll: Duration,
    #[cfg(feature = "web")]
    tickets: Tickets,
}

/// Builds a [`VoiceHub`].
pub struct VoiceHubBuilder {
    engine: SurfaceEngine,
    harness: Arc<Harness>,
    live: LiveVoice,
    models: Vec<Offered>,
    agent: Option<String>,
    instruction: Option<String>,
    poll: Duration,
}

impl std::fmt::Debug for VoiceHubBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceHubBuilder")
            .field("models", &self.models.iter().map(|m| &m.info.id).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl VoiceHubBuilder {
    /// Offers `model`, described by `info`, whose camera and screen frames follow `frames`
    /// ([`FramePolicy::GEMINI`] or [`FramePolicy::OPENAI`]). The first model offered is the
    /// default.
    #[must_use]
    pub fn model(mut self, info: VoiceModel, model: BoxedModel, frames: FramePolicy) -> Self {
        self.models.push(Offered { info, model, frames, compress: false });
        self
    }

    /// Turns on Gemini Live context window compression for the model `id`: the camera or the
    /// screen may be switched on in any conversation, and without it a Gemini Live session
    /// sending frames ends after about two minutes (spec 3.3 §5.6). Leave it off for a
    /// provider that does not take the setting.
    #[must_use]
    pub fn context_window_compression(mut self, id: &str) -> Self {
        for offered in self.models.iter_mut().filter(|offered| offered.info.id == id) {
            offered.compress = true;
        }
        self
    }

    /// The agent's name as people see it: "Talking with {name}". Defaults to the harness's
    /// name in plain words.
    #[must_use]
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent = Some(name.into());
        self
    }

    /// Text appended to the voice instruction the model gets.
    #[must_use]
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// How often a conversation reads the open decisions when the source signals nothing
    /// sooner (default [`DEFAULT_POLL_INTERVAL`]).
    #[must_use]
    pub fn poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// The hub.
    pub fn build(self) -> VoiceHub {
        let agent = self
            .agent
            .unwrap_or_else(|| crate::project::humanize(&self.harness.agent().info().name));
        VoiceHub {
            inner: Arc::new(Hub {
                engine: self.engine,
                harness: self.harness,
                live: self.live,
                models: self.models,
                agent,
                instruction: self.instruction,
                poll: self.poll,
                #[cfg(feature = "web")]
                tickets: Tickets::default(),
            }),
        }
    }
}

/// Opens voice conversations with a harness marked `Pack::Voice` (spec 3.3 §5.6).
///
/// Each conversation is one realtime session of the harness on a model the hub offers, in its
/// own session of the session store, so it is listed in Threads under the agent. Its calls are
/// governed by the harness as typed ones are; a call it holds is a decision in the inbox like
/// any other, answered by voice through the engine or on any other surface.
///
/// The engine must run on a [`VoiceSource`](super::VoiceSource) whose [`live`] conversations
/// this hub registers, so a decision on a call a conversation holds resumes it there.
///
/// [`live`]: super::VoiceSource::live
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::voice::{FramePolicy, VoiceHub, VoiceModel, VoiceSource};
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(harness: Arc<adk_harness::Harness>, local: LocalSource, live: adk_realtime::BoxedModel) {
/// let source = VoiceSource::new(Arc::new(local));
/// let live_voice = source.live();
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// let gemini = VoiceModel {
///     id: "gemini".into(),
///     name: "Gemini Live".into(),
///     input_rate: 16_000,
///     output_rate: 24_000,
/// };
/// let hub = VoiceHub::builder(engine, harness, live_voice)
///     .model(gemini, live, FramePolicy::GEMINI)
///     .build();
/// # let _ = hub;
/// # }
/// ```
#[derive(Clone)]
pub struct VoiceHub {
    inner: Arc<Hub>,
}

impl std::fmt::Debug for VoiceHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceHub")
            .field("agent", &self.inner.agent)
            .field("models", &self.inner.models.iter().map(|m| &m.info.id).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl VoiceHub {
    /// A builder over `engine` and `harness`, registering conversations in `live`.
    pub fn builder(
        engine: SurfaceEngine,
        harness: Arc<Harness>,
        live: LiveVoice,
    ) -> VoiceHubBuilder {
        VoiceHubBuilder {
            engine,
            harness,
            live,
            models: Vec::new(),
            agent: None,
            instruction: None,
            poll: DEFAULT_POLL_INTERVAL,
        }
    }

    /// The engine spoken answers go through.
    pub fn engine(&self) -> &SurfaceEngine {
        &self.inner.engine
    }

    /// The models offered, the default first, and the agent's name.
    pub fn models(&self) -> VoiceModels {
        VoiceModels {
            agent: self.inner.agent.clone(),
            agent_name: self
                .inner
                .engine
                .source()
                .primary_agent()
                .unwrap_or_else(|| self.inner.harness.agent().info().name.clone()),
            models: self.inner.models.iter().map(|offered| offered.info.clone()).collect(),
        }
    }

    fn offered(&self, model: &str) -> Result<&Offered, VoiceError> {
        self.inner
            .models
            .iter()
            .find(|offered| offered.info.id == model)
            .ok_or_else(|| VoiceError::UnknownModel(model.to_string()))
    }

    /// The voice session `caller` talks in: `session_id` when it is one of the caller's own
    /// voice conversations (S-I8), else a new one.
    ///
    /// # Errors
    ///
    /// [`VoiceError::NotYourSession`] when `session_id` names a session that is not the
    /// caller's voice conversation, whoever's it is; [`VoiceError::Harness`] when the session
    /// store fails.
    pub async fn session_for(
        &self,
        caller: &Caller,
        session_id: Option<&str>,
    ) -> Result<String, VoiceError> {
        let sessions = self.inner.harness.runner().session_service();
        let app = self.inner.engine.source().app_name().to_string();
        if let Some(session_id) = session_id {
            let found = sessions
                .get(GetRequest {
                    app_name: app,
                    user_id: caller.user_id().to_string(),
                    session_id: session_id.to_string(),
                    num_recent_events: Some(0),
                    after: None,
                })
                .await;
            return match found {
                Ok(session)
                    if session.state().get(VOICE_SESSION_KEY) == Some(Value::Bool(true)) =>
                {
                    Ok(session_id.to_string())
                }
                Ok(_) => Err(VoiceError::NotYourSession(session_id.to_string())),
                Err(error)
                    if matches!(
                        error.category,
                        ErrorCategory::NotFound | ErrorCategory::InvalidInput
                    ) =>
                {
                    Err(VoiceError::NotYourSession(session_id.to_string()))
                }
                Err(error) => Err(VoiceError::Harness(error.to_string())),
            };
        }
        let session_id = format!("voice-{}", uuid::Uuid::new_v4().simple());
        sessions
            .create(CreateRequest {
                app_name: app,
                user_id: caller.user_id().to_string(),
                session_id: Some(session_id.clone()),
                state: HashMap::from([(VOICE_SESSION_KEY.to_string(), json!(true))]),
            })
            .await
            .map_err(|error| VoiceError::Harness(error.to_string()))?;
        Ok(session_id)
    }

    /// Opens a voice conversation for `caller` on `model` in `caller`'s voice session
    /// `session_id`, or a new one (spec 3.3 §5.6). `caller` is the person at the transport:
    /// its origin is how **End** is given (a pointer in the web view, a key in the terminal);
    /// what the person says is given by voice.
    ///
    /// # Errors
    ///
    /// | Error | When |
    /// |---|---|
    /// | [`VoiceError::UnknownModel`] | No model of that id is offered |
    /// | [`VoiceError::NotYourSession`] | `session_id` is not one of the caller's voice conversations |
    /// | [`VoiceError::Harness`] | The harness is not marked `Pack::Voice`, or the session store failed |
    /// | [`VoiceError::Provider`] | The realtime provider could not be reached |
    pub async fn open(
        &self,
        caller: &Caller,
        model: &str,
        session_id: Option<&str>,
    ) -> Result<VoiceLink, VoiceError> {
        let offered = self.offered(model)?;
        let session_id = self.session_for(caller, session_id).await?;
        self.connect(caller, offered, session_id, false).await
    }

    /// Opens a voice conversation in a new voice session of `caller`'s for a surface that
    /// shows only that session, such as the terminal's thread: as [`open`](Self::open),
    /// except that only the conversation's own decisions are prompted and answered by voice,
    /// since no other is on that screen in full (spec 3.3 §5.6, §7.1).
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_surface::{Caller, Origin, SurfaceKind};
    ///
    /// # async fn example(hub: adk_surface::voice::VoiceHub) -> Result<(), adk_surface::voice::VoiceError> {
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// let link = hub.open_in_thread(&me, "gemini").await?;
    /// println!("talking in {}", link.session_id);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open_in_thread(
        &self,
        caller: &Caller,
        model: &str,
    ) -> Result<VoiceLink, VoiceError> {
        let offered = self.offered(model)?;
        let session_id = self.session_for(caller, None).await?;
        self.connect(caller, offered, session_id, true).await
    }

    async fn connect(
        &self,
        caller: &Caller,
        offered: &Offered,
        session_id: String,
        own_only: bool,
    ) -> Result<VoiceLink, VoiceError> {
        let hub = &self.inner;
        let user = caller.user_id().to_string();
        let instruction = match &hub.instruction {
            Some(extra) => format!("{NEVER_DECIDE}\n\n{extra}"),
            None => NEVER_DECIDE.to_string(),
        };
        let context = RequestContext {
            user_id: user.clone(),
            scopes: caller.groups.clone(),
            metadata: Default::default(),
        };
        let app = hub.engine.source().app_name();
        let tools = hub.harness.agent().voice_tools(app, &user, &session_id).await;
        // The kill switch Stop all freezes is the harness runner's: every call of the
        // conversation checks it, resumes included (spec 3.3 S-I10).
        let mut config = VoiceConfig::new(app)
            .with_instruction(instruction)
            .with_request_context(context)
            .with_governance(hub.harness.runner().governance().clone())
            .with_tools(tools);
        if offered.compress {
            config = config.with_context_window_compression();
        }
        let surface = VoiceSurface::open(
            Arc::clone(hub.harness.agent()),
            Arc::clone(&offered.model),
            Arc::clone(hub.harness.runner().session_service()),
            user.clone(),
            session_id.clone(),
            config,
        )
        .map_err(|error| VoiceError::Harness(error.to_string()))?;
        surface
            .runner()
            .connect()
            .await
            .map_err(|error| VoiceError::Provider(error.to_string()))?;
        let (registration, wake) = hub.live.register(&user, &session_id);
        let speaker = Caller {
            surface: SurfaceKind::Voice,
            origin: Origin::Voice,
            presence_confirmed: false,
            paired_local: false,
            ..caller.clone()
        };
        let viewer = Caller {
            surface: SurfaceKind::Voice,
            presence_confirmed: false,
            paired_local: false,
            ..caller.clone()
        };
        Ok(session::start(Conversation {
            engine: hub.engine.clone(),
            surface: Arc::new(surface),
            registration,
            wake,
            speaker,
            viewer,
            session_id,
            input_rate: offered.info.input_rate,
            output_rate: offered.info.output_rate,
            visual: VisualInputs::new(offered.frames),
            poll: hub.poll,
            own_only,
        }))
    }

    /// Issues a single-use ticket for `caller`'s conversation on `model` in `session_id`
    /// (spec 3.3 §5.6).
    #[cfg(feature = "web")]
    pub(crate) fn issue(&self, caller: &Caller, model: &str, session_id: &str) -> String {
        self.inner.tickets.issue(caller, model, session_id)
    }

    /// Takes the ticket `token`, once, while it has not expired.
    #[cfg(feature = "web")]
    pub(crate) fn redeem(&self, token: &str) -> Option<Ticket> {
        self.inner.tickets.redeem(token)
    }

    /// Opens the conversation a redeemed ticket was issued for.
    #[cfg(feature = "web")]
    pub(crate) async fn open_ticket(&self, ticket: &Ticket) -> Result<VoiceLink, VoiceError> {
        let offered = self.offered(&ticket.model)?;
        self.connect(&ticket.caller, offered, ticket.session_id.clone(), false).await
    }
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::*;

    /// A ticket works once, for thirty seconds, and carries the caller it was issued to.
    #[tokio::test(start_paused = true)]
    async fn a_ticket_works_once_for_thirty_seconds() {
        let tickets = Tickets::default();
        let ana = Caller::human("ana", SurfaceKind::Voice, Origin::Pointer);
        let once = tickets.issue(&ana, "gemini", "voice-1");
        assert_eq!(once.len(), 64);
        let ticket = tickets.redeem(&once).expect("a fresh ticket works");
        assert_eq!(
            (ticket.caller, ticket.model.as_str(), ticket.session_id.as_str()),
            (ana.clone(), "gemini", "voice-1")
        );
        assert!(tickets.redeem(&once).is_none(), "a ticket works once");

        let late = tickets.issue(&ana, "gemini", "voice-1");
        tokio::time::advance(TICKET_LIFETIME - Duration::from_millis(1)).await;
        let early = tickets.issue(&ana, "gemini", "voice-1");
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(tickets.redeem(&late).is_none(), "a ticket expires after thirty seconds");
        assert!(tickets.redeem(&early).is_some());
        assert!(tickets.redeem("made-up").is_none());
    }
}
