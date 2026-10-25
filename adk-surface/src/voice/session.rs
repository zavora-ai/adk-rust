//! One voice conversation (spec 3.3 §5.6): the realtime session of the harness, the prompted
//! decision, spoken answers through the engine, and the camera and screen gate.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_harness::VoiceSurface;
use adk_realtime::ServerEvent;
use adk_realtime::audio::{AudioChunk, AudioEncoding, AudioFormat};
use base64::Engine as _;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::SpokenAnswer;
use super::source::Registration;
use super::spoken::{self, Spoken};
use super::visual::VisualInputs;
use super::wire::{InputSource, VisualSource, VoiceClientMessage, VoiceUpdate};
use crate::engine::{Command, CommandOutcome, SurfaceEngine};
use crate::error::{Result, SurfaceError};
use crate::model::DecisionCard;
use crate::origin::Caller;
use crate::rules::Answer;
use crate::words;

/// How long a prompt is on screen before speech counts as an answer to it (spec 3.3 §5.6).
pub(crate) const PROMPT_DWELL: Duration = Duration::from_secs(1);

/// How long after the model's audio ends speech is still taken for its echo (spec 3.3 §5.6).
pub(crate) const ECHO_TAIL: Duration = Duration::from_millis(300);

/// How many messages wait between the conversation and its transport.
const CHANNEL: usize = 256;

/// What a transport sends a voice conversation (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceInput {
    /// PCM16 mono at the model's input rate, from the microphone.
    Audio(Vec<u8>),
    /// A camera or screen JPEG.
    Frame {
        /// Which.
        source: VisualSource,
        /// The JPEG.
        jpeg: Vec<u8>,
    },
    /// A text message of the view.
    Message(VoiceClientMessage),
}

/// What a voice conversation sends its transport (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceOutput {
    /// PCM16 mono at the model's output rate, to play.
    Audio(Vec<u8>),
    /// Transient voice state.
    Update(VoiceUpdate),
}

/// An open voice conversation, from its transport's side (spec 3.3 §5.6): send it
/// [`VoiceInput`]s, read its [`VoiceOutput`]s. Dropping `input` ends the conversation, as
/// [`VoiceClientMessage::End`] does.
#[derive(Debug)]
pub struct VoiceLink {
    /// The conversation's own session.
    pub session_id: String,
    /// The sample rate of the audio to send.
    pub input_rate: u32,
    /// The sample rate of the audio to play.
    pub output_rate: u32,
    /// Where the transport sends.
    pub input: mpsc::Sender<VoiceInput>,
    /// What the transport receives.
    pub output: mpsc::Receiver<VoiceOutput>,
}

/// The prompted decision: what a spoken answer applies to.
#[derive(Debug, Clone)]
struct Prompt {
    id: String,
    version: u64,
    since: Instant,
}

/// An utterance from its start: the decision it was bound to, and whether it began over the
/// model's audio.
#[derive(Debug, Clone)]
struct Utterance {
    bound: Option<(String, u64)>,
    echo: bool,
}

/// What [`run`] needs from the hub.
pub(crate) struct Conversation {
    pub(crate) engine: SurfaceEngine,
    pub(crate) surface: Arc<VoiceSurface>,
    pub(crate) registration: Registration,
    pub(crate) wake: Arc<Notify>,
    /// The person, speaking: surface voice, origin voice.
    pub(crate) speaker: Caller,
    /// The person, at the view's controls: End.
    pub(crate) viewer: Caller,
    pub(crate) session_id: String,
    pub(crate) input_rate: u32,
    pub(crate) output_rate: u32,
    pub(crate) visual: VisualInputs,
    pub(crate) poll: Duration,
}

/// Opens the link and runs the conversation on its own task.
pub(crate) fn start(conversation: Conversation) -> VoiceLink {
    let (input, input_rx) = mpsc::channel(CHANNEL);
    let (output_tx, output) = mpsc::channel(CHANNEL);
    let link = VoiceLink {
        session_id: conversation.session_id.clone(),
        input_rate: conversation.input_rate,
        output_rate: conversation.output_rate,
        input,
        output,
    };
    tokio::spawn(run(conversation, input_rx, output_tx));
    link
}

type Settled = Pin<
    Box<
        dyn Future<
                Output = (
                    String,
                    std::result::Result<Result<CommandOutcome>, oneshot::error::RecvError>,
                ),
            > + Send,
    >,
>;

struct State {
    engine: SurfaceEngine,
    surface: Arc<VoiceSurface>,
    speaker: Caller,
    viewer: Caller,
    session_id: String,
    input_rate: u32,
    output_rate: u32,
    output: mpsc::Sender<VoiceOutput>,
    visual: VisualInputs,
    mic_on: bool,
    speaking: bool,
    /// Until when the model's audio plays, as far as the server can tell.
    playing_until: Option<Instant>,
    prompt: Option<Prompt>,
    utterance: Option<Utterance>,
    /// The open decisions of this conversation's own session — the calls it held — oldest
    /// first.
    own: Vec<String>,
    /// Spoken answers in their cancel window.
    pending: HashSet<String>,
    settled: FuturesUnordered<Settled>,
    needs_screen: Vec<String>,
    stopped: bool,
    /// Resumes of the conversation's held calls, one at a time.
    resumes: JoinSet<()>,
    resume_lock: Arc<AsyncMutex<()>>,
    /// Tells the provider pump a client action may have made the provider speak again.
    poke: Arc<Notify>,
    closing: Arc<AtomicBool>,
}

async fn run(
    conversation: Conversation,
    mut input: mpsc::Receiver<VoiceInput>,
    output: mpsc::Sender<VoiceOutput>,
) {
    let Conversation {
        engine,
        surface,
        registration,
        wake,
        speaker,
        viewer,
        session_id,
        input_rate,
        output_rate,
        visual,
        poll,
    } = conversation;
    let closing = Arc::new(AtomicBool::new(false));
    let poke = Arc::new(Notify::new());
    let (events_tx, mut events) = mpsc::channel(CHANNEL);
    // `next_event` observes, screens and persists after it reads the provider, so it is never
    // dropped half way: it runs on its own task and forwards what it reads.
    let pump = tokio::spawn(pump(surface.clone(), events_tx, poke.clone(), closing.clone()));
    let mut state = State {
        engine,
        surface,
        speaker,
        viewer,
        session_id,
        input_rate,
        output_rate,
        output,
        visual,
        mic_on: true,
        speaking: false,
        playing_until: None,
        prompt: None,
        utterance: None,
        own: Vec::new(),
        pending: HashSet::new(),
        settled: FuturesUnordered::new(),
        needs_screen: Vec::new(),
        stopped: false,
        resumes: JoinSet::new(),
        resume_lock: Arc::new(AsyncMutex::new(())),
        poke,
        closing,
    };
    state
        .send(VoiceUpdate::Ready {
            session_id: state.session_id.clone(),
            input_rate: state.input_rate,
            output_rate: state.output_rate,
        })
        .await;
    state.send(VoiceUpdate::Listening).await;
    // A call an earlier conversation on this session held may have been decided since.
    state.resume();
    state.refresh().await;

    let source = Arc::clone(state.engine.source());
    let changes = move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let source = Arc::clone(&source);
        Box::pin(async move { source.changed(poll).await })
    };
    let mut changed = changes();
    loop {
        tokio::select! {
            message = input.recv() => match message {
                Some(message) => {
                    if state.on_input(message).await.is_break() {
                        break;
                    }
                }
                None => {
                    state.end().await;
                    break;
                }
            },
            event = events.recv() => match event {
                Some(Ok(event)) => state.on_event(event).await,
                Some(Err(error)) => {
                    state
                        .send(VoiceUpdate::Error {
                            code: "surface.voice.provider".into(),
                            message: error.to_string(),
                        })
                        .await;
                }
                None => {
                    state.end().await;
                    break;
                }
            },
            () = wake.notified() => state.resume(),
            Some((id, settled)) = state.settled.next(), if !state.settled.is_empty() => {
                state.on_settled(id, settled).await;
            }
            () = &mut changed => {
                changed = changes();
                // A decision made outside the engine — in Telegram through the kernel, or with
                // `adk-rust approvals` — reaches no one: look for it while a call is held.
                let holding = !state.own.is_empty();
                state.refresh().await;
                if holding {
                    state.resume();
                }
            }
            Some(_) = state.resumes.join_next(), if !state.resumes.is_empty() => {}
        }
        if state.output.is_closed() {
            state.end().await;
            break;
        }
    }
    drop(registration);
    state.closing.store(true, Ordering::SeqCst);
    state.poke.notify_one();
    if let Err(error) = state.surface.runner().close().await {
        tracing::debug!(error = %error, "closing the realtime session failed");
    }
    while state.resumes.join_next().await.is_some() {}
    if tokio::time::timeout(Duration::from_secs(5), pump).await.is_err() {
        tracing::warn!(session.id = %state.session_id, "the realtime session did not end in time");
    }
}

/// Reads the provider until the conversation closes.
async fn pump(
    surface: Arc<VoiceSurface>,
    events: mpsc::Sender<adk_realtime::Result<ServerEvent>>,
    poke: Arc<Notify>,
    closing: Arc<AtomicBool>,
) {
    loop {
        match surface.runner().next_event().await {
            Some(event) => {
                if events.send(event).await.is_err() {
                    return;
                }
            }
            // A provider with nothing to say until the person acts again, such as a scripted
            // one, reports the end of its events; a closed one keeps doing so.
            None => {
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let _ = tokio::time::timeout(Duration::from_secs(1), poke.notified()).await;
            }
        }
    }
}

enum Flow {
    Continue,
    Break,
}

impl Flow {
    fn is_break(&self) -> bool {
        matches!(self, Self::Break)
    }
}

impl State {
    async fn send(&mut self, update: VoiceUpdate) {
        let _ = self.output.send(VoiceOutput::Update(update)).await;
    }

    fn error(&self, error: &SurfaceError) -> VoiceUpdate {
        VoiceUpdate::Error { code: error.code(), message: error.to_string() }
    }

    async fn on_input(&mut self, input: VoiceInput) -> Flow {
        match input {
            VoiceInput::Audio(pcm) => {
                if !self.mic_on || pcm.is_empty() {
                    return Flow::Continue;
                }
                let chunk = AudioChunk::new(
                    pcm,
                    AudioFormat::new(self.input_rate, 1, 16, AudioEncoding::Pcm16),
                );
                if let Err(error) = self.surface.runner().send_audio_chunk(&chunk).await {
                    self.send(VoiceUpdate::Error {
                        code: "surface.voice.provider".into(),
                        message: error.to_string(),
                    })
                    .await;
                    self.end().await;
                    return Flow::Break;
                }
                self.poke.notify_one();
            }
            VoiceInput::Frame { source, jpeg } => {
                if self.visual.offer(source, &jpeg, Instant::now()) {
                    let data = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                    if let Err(error) =
                        self.surface.runner().send_video_frame("image/jpeg", &data).await
                    {
                        tracing::debug!(error = %error, "a frame could not be sent");
                    }
                }
            }
            VoiceInput::Message(message) => match message {
                VoiceClientMessage::Switch { source, on } => match source {
                    InputSource::Mic => self.mic_on = on,
                    InputSource::Camera => self.visual.switch(VisualSource::Camera, on),
                    InputSource::Screen => self.visual.switch(VisualSource::Screen, on),
                },
                VoiceClientMessage::PlaybackEnded => {
                    let now = Instant::now();
                    self.playing_until =
                        Some(self.playing_until.map_or(now, |until| until.max(now)));
                }
                VoiceClientMessage::End => {
                    self.end().await;
                    return Flow::Break;
                }
                // Only the first message of a socket is a hello; a later one is ignored.
                VoiceClientMessage::Hello { .. } => {}
            },
        }
        Flow::Continue
    }

    /// Whether speech starting now begins over the model's audio.
    fn over_playback(&self, now: Instant) -> bool {
        self.playing_until.is_some_and(|until| now < until + ECHO_TAIL)
    }

    /// Starts an utterance, bound to the decision prompted for at least [`PROMPT_DWELL`].
    fn begin_utterance(&mut self, now: Instant) {
        let bound = self
            .prompt
            .as_ref()
            .filter(|prompt| now.saturating_duration_since(prompt.since) >= PROMPT_DWELL)
            .map(|prompt| (prompt.id.clone(), prompt.version));
        self.utterance = Some(Utterance { bound, echo: self.over_playback(now) });
    }

    async fn on_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::SpeechStarted { .. } => {
                let now = Instant::now();
                if self.over_playback(now) {
                    self.send(VoiceUpdate::BargeIn).await;
                }
                self.begin_utterance(now);
            }
            // A provider that reports no speech onset starts the utterance with its transcript.
            ServerEvent::InputTranscriptDelta { .. } if self.utterance.is_none() => {
                self.begin_utterance(Instant::now());
            }
            ServerEvent::InputTranscriptCompleted { transcript, .. } => {
                let utterance =
                    self.utterance.take().unwrap_or(Utterance { bound: None, echo: true });
                self.send(VoiceUpdate::Heard { text: transcript.clone() }).await;
                self.on_utterance(&transcript, utterance).await;
            }
            ServerEvent::AudioDelta { delta, .. } => {
                let now = Instant::now();
                let bytes_per_second = u64::from(self.output_rate.max(1)) * 2;
                let length =
                    Duration::from_micros(delta.len() as u64 * 1_000_000 / bytes_per_second);
                let from = self.playing_until.filter(|until| *until > now).unwrap_or(now);
                self.playing_until = Some(from + length);
                if !self.speaking {
                    self.speaking = true;
                    self.send(VoiceUpdate::Speaking).await;
                }
                let _ = self.output.send(VoiceOutput::Audio(delta)).await;
            }
            ServerEvent::ResponseDone { .. } if self.speaking => {
                self.speaking = false;
                self.send(VoiceUpdate::Listening).await;
            }
            // The conversation's own call was just held: it is offered first.
            ServerEvent::ToolConfirmationRequested { .. } => self.refresh().await,
            ServerEvent::Error { error, .. } => {
                self.send(VoiceUpdate::Error {
                    code: "surface.voice.provider".into(),
                    message: error.message,
                })
                .await;
            }
            _ => {}
        }
    }

    async fn on_utterance(&mut self, transcript: &str, utterance: Utterance) {
        let Some(said) = spoken::parse(transcript, !self.pending.is_empty()) else {
            return;
        };
        // The model's own words, played back into the microphone, never answer.
        if utterance.echo {
            return;
        }
        match said {
            Spoken::StopAll => {
                let stop = Command::StopAll { reason: None };
                if let Err(error) = self.engine.execute(&self.speaker, stop).await {
                    let update = match &error {
                        SurfaceError::OperatorOnly { .. } => VoiceUpdate::Error {
                            code: error.code(),
                            message: words::STOPPING_NEEDS_THE_OPERATOR.to_string(),
                        },
                        _ => self.error(&error),
                    };
                    self.send(update).await;
                }
                self.refresh().await;
            }
            Spoken::Cancel => {
                let speaker = self.speaker.clone();
                self.cancel_pending(&speaker).await;
            }
            Spoken::AllowOnce | Spoken::Deny => {
                let answer =
                    if said == Spoken::AllowOnce { Answer::AllowOnce } else { Answer::Deny };
                self.answer(answer, transcript, utterance.bound).await;
            }
        }
    }

    async fn answer(&mut self, answer: Answer, transcript: &str, bound: Option<(String, u64)>) {
        let Some((id, version)) = bound else {
            return;
        };
        let current = self.prompt.as_ref().map(|prompt| (prompt.id.as_str(), prompt.version));
        if current != Some((id.as_str(), version)) {
            self.send(VoiceUpdate::Outcome {
                decision_id: id,
                decided: false,
                text: Some(words::CHANGED_WHILE_YOU_SPOKE.to_string()),
            })
            .await;
            return;
        }
        match self.engine.decide_spoken(&self.speaker, &id, version, answer, transcript).await {
            Ok(SpokenAnswer { outcome, settled }) => {
                if let CommandOutcome::Pending { decision_id, cancel_until, text } = &outcome {
                    self.pending.insert(decision_id.clone());
                    if let Some(settled) = settled {
                        let id = decision_id.clone();
                        self.settled.push(Box::pin(async move { (id, settled.await) }));
                    }
                    self.send(VoiceUpdate::Pending {
                        decision_id: decision_id.clone(),
                        cancel_until: *cancel_until,
                        text: text.clone(),
                    })
                    .await;
                } else {
                    self.send(outcome_update(&id, Ok(outcome))).await;
                }
            }
            Err(error) => self.send(outcome_update(&id, Err(error))).await,
        }
        self.refresh().await;
    }

    async fn on_settled(
        &mut self,
        id: String,
        settled: std::result::Result<Result<CommandOutcome>, oneshot::error::RecvError>,
    ) {
        self.pending.remove(&id);
        let update = match settled {
            Ok(result) => outcome_update(&id, result),
            // The window's task was aborted: cancelled, ended or stopped.
            Err(_) => VoiceUpdate::Outcome {
                decision_id: id,
                decided: false,
                text: Some(words::SPOKEN_ANSWER_CANCELLED.to_string()),
            },
        };
        self.send(update).await;
        self.refresh().await;
    }

    /// Cancels every spoken answer of this conversation in its cancel window, as `caller`.
    async fn cancel_pending(&mut self, caller: &Caller) {
        let pending: Vec<String> = self.pending.iter().cloned().collect();
        for id in pending {
            let cancel = Command::CancelAnswer { decision_id: id.clone() };
            match self.engine.execute(caller, cancel).await {
                Ok(_) => {}
                // Its window ended meanwhile; its outcome follows.
                Err(SurfaceError::NotFound { .. }) => {}
                Err(error) => self.send(self.error(&error)).await,
            }
        }
    }

    /// Ends the conversation: a spoken answer in its cancel window is cancelled.
    async fn end(&mut self) {
        let viewer = self.viewer.clone();
        self.cancel_pending(&viewer).await;
        // The cancelled answers report through their aborted windows.
        while !self.settled.is_empty() {
            match tokio::time::timeout(Duration::from_secs(1), self.settled.next()).await {
                Ok(Some((id, settled))) => self.on_settled(id, settled).await,
                Ok(None) | Err(_) => break,
            }
        }
        self.send(VoiceUpdate::Ended).await;
    }

    /// Resumes the conversation's held calls that have a decision, on their own task and one
    /// at a time: a resumed call runs its tool, which can take long.
    fn resume(&mut self) {
        // One running and one waiting cover every decision made meanwhile.
        if self.resumes.len() >= 2 {
            return;
        }
        let surface = Arc::clone(&self.surface);
        let lock = Arc::clone(&self.resume_lock);
        let output = self.output.clone();
        self.resumes.spawn(async move {
            let _one_at_a_time = lock.lock().await;
            if let Err(error) = surface.runner().resume_held_calls().await {
                tracing::warn!(error = %error, "a decided call could not be resumed");
                let _ = output
                    .send(VoiceOutput::Update(VoiceUpdate::Error {
                        code: "surface.voice.provider".into(),
                        message: error.to_string(),
                    }))
                    .await;
            }
        });
    }

    /// Reads the open decisions again and says what is prompted and what needs a screen.
    async fn refresh(&mut self) {
        let stopped = self.engine.source().stopped().await.unwrap_or(false);
        let cards = if stopped {
            Vec::new()
        } else {
            match self.engine.decisions(&self.speaker).await {
                Ok(cards) => cards,
                Err(error) => {
                    tracing::warn!(error = %error, "the open decisions could not be read");
                    return;
                }
            }
        };
        self.stopped = stopped;
        // Every call of the conversation's own session is one it held: its realtime session
        // is the only runner of that session.
        self.own = cards
            .iter()
            .filter(|card| card.session_id == self.session_id)
            .map(|card| card.id.clone())
            .collect();
        let choice = choose(&cards, &self.own, &self.pending);
        let now = Instant::now();
        let changed = match (&self.prompt, choice) {
            (Some(prompt), Some(card)) => prompt.id != card.id || prompt.version != card.version,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.prompt = choice.map(|card| Prompt {
                id: card.id.clone(),
                version: card.version,
                since: now,
            });
            self.send(VoiceUpdate::Prompted {
                decision_id: choice.map(|card| card.id.clone()),
                version: choice.map(|card| card.version),
            })
            .await;
        }
        let needs_screen: Vec<String> = cards
            .iter()
            .filter(|card| card.allowed.option(Answer::AllowOnce).is_none())
            .map(|card| card.id.clone())
            .collect();
        if needs_screen != self.needs_screen {
            self.needs_screen = needs_screen.clone();
            self.send(VoiceUpdate::NeedsScreen { decision_ids: needs_screen }).await;
        }
    }
}

/// The decision a spoken answer would apply to: the conversation's own call held last, else
/// the oldest open decision voice may answer, never one whose spoken answer is pending.
fn choose<'a>(
    cards: &'a [DecisionCard],
    own: &[String],
    pending: &HashSet<String>,
) -> Option<&'a DecisionCard> {
    let answerable = |card: &&DecisionCard| {
        !pending.contains(&card.id)
            && card.allowed.options.iter().any(|option| {
                matches!(option.answer, Answer::AllowOnce | Answer::Deny) && !option.needs_expanded
            })
    };
    own.iter()
        .rev()
        .find_map(|id| cards.iter().filter(answerable).find(|card| &card.id == id))
        .or_else(|| cards.iter().find(answerable))
}

/// What the view is told once a spoken answer ended.
fn outcome_update(id: &str, result: Result<CommandOutcome>) -> VoiceUpdate {
    let (decided, text) = match result {
        Ok(CommandOutcome::Answered { card }) => (card.decided.is_some(), None),
        Ok(CommandOutcome::Changed { .. }) => {
            (false, Some(words::CHANGED_WHILE_YOU_SPOKE.to_string()))
        }
        Ok(CommandOutcome::AlreadyDecided { decided, .. }) => (false, Some(decided.text)),
        Ok(CommandOutcome::AnswerCancelled { .. }) => {
            (false, Some(words::SPOKEN_ANSWER_CANCELLED.to_string()))
        }
        Ok(_) => (false, None),
        // A high-risk or money call: approving needs the person to confirm it is them.
        Err(SurfaceError::Rule(_)) => (false, Some(words::VOICE_CANT_APPROVE_THIS.to_string())),
        Err(error) => (false, Some(error.to_string())),
    };
    VoiceUpdate::Outcome { decision_id: id.to_string(), decided, text }
}
