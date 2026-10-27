//! One voice conversation (spec 3.3 §5.6): the realtime session of the harness, the prompted
//! decision, spoken answers through the engine, and the camera and screen gate.

use std::collections::{HashMap, HashSet, VecDeque};
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

/// How many utterances may wait for their transcripts; an older one is dropped.
const UTTERANCES: usize = 8;

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
    /// A spoken answer applies only to the conversation's own decisions: the terminal's
    /// thread shows those, and no other, in full (spec 3.3 §7.1).
    pub(crate) own_only: bool,
    /// The person's screen actions, said aloud: "snooze news digest" (spec 3.3 §5.6).
    #[cfg(feature = "verb-ui")]
    pub(crate) verbs: Option<crate::verbs::VerbGate>,
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

/// A spoken screen action waiting out its cancel window, then run: its id and how it ended.
type Acting = Pin<Box<dyn Future<Output = (String, VoiceUpdate)> + Send>>;

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
    /// A barge-in the transport has not yet answered with `playbackEnded`, and until when
    /// the audio sent since it plays: until the transport flushed, everything sent before
    /// the barge-in may still play.
    barge_in: Option<Option<Instant>>,
    prompt: Option<Prompt>,
    /// Utterances whose transcript has not arrived, oldest first: a provider that
    /// transcribes after the next utterance started completes them in order.
    utterances: VecDeque<Utterance>,
    own_only: bool,
    /// The open decisions of this conversation's own session — the calls it held — oldest
    /// first.
    own: Vec<String>,
    /// Spoken answers in their cancel window.
    pending: HashSet<String>,
    settled: FuturesUnordered<Settled>,
    needs_screen: Vec<String>,
    stopped: bool,
    ended: bool,
    /// Resumes of the conversation's held calls, one at a time.
    resumes: JoinSet<()>,
    resume_lock: Arc<AsyncMutex<()>>,
    /// A resume was asked for while two were in flight: one more follows the next to end.
    resume_again: bool,
    /// Tells the provider pump a client action may have made the provider speak again.
    poke: Arc<Notify>,
    closing: Arc<AtomicBool>,
    /// Spoken screen actions in their cancel window, by id.
    actions: HashMap<String, tokio::task::AbortHandle>,
    /// The same actions, ending.
    acting: FuturesUnordered<Acting>,
    #[cfg(feature = "verb-ui")]
    verbs: Option<crate::verbs::VerbGate>,
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
        own_only,
        #[cfg(feature = "verb-ui")]
        verbs,
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
        barge_in: None,
        prompt: None,
        utterances: VecDeque::new(),
        own_only,
        own: Vec::new(),
        pending: HashSet::new(),
        settled: FuturesUnordered::new(),
        needs_screen: Vec::new(),
        stopped: false,
        ended: false,
        resumes: JoinSet::new(),
        resume_lock: Arc::new(AsyncMutex::new(())),
        resume_again: false,
        poke,
        closing,
        actions: HashMap::new(),
        acting: FuturesUnordered::new(),
        #[cfg(feature = "verb-ui")]
        verbs,
    };
    // While every agent is stopped no conversation runs (spec 3.3 S-I10).
    if state.engine.source().stopped().await.unwrap_or(false) {
        state.stopped = true;
    } else {
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
    }

    let source = Arc::clone(state.engine.source());
    let changes = move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let source = Arc::clone(&source);
        Box::pin(async move { source.changed(poll).await })
    };
    let mut changed = changes();
    while !state.stopped {
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
            Some((id, update)) = state.acting.next(), if !state.acting.is_empty() => {
                state.actions.remove(&id);
                state.send(update).await;
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
            Some(_) = state.resumes.join_next(), if !state.resumes.is_empty() => {
                if std::mem::take(&mut state.resume_again) {
                    state.resume();
                }
            }
        }
        if state.output.is_closed() {
            state.end().await;
            break;
        }
    }
    // Stop all ends the conversation: the model stops listening, answering and spending
    // (spec 3.3 S-I10, §12 phase 5).
    if state.stopped && !state.ended {
        state
            .send(VoiceUpdate::Error {
                code: "surface.stopped".into(),
                message: words::ALL_AGENTS_STOPPED.to_string(),
            })
            .await;
        state.end().await;
    }
    // A decision recorded from here on goes to the wrapped source. One recorded while the
    // conversation was ending woke a signal no one listens to any more: resume it here.
    drop(registration);
    while state.resumes.join_next().await.is_some() {}
    if let Err(error) = state.surface.resume_held_calls().await {
        tracing::debug!(error = %error, "the decided calls could not be resumed at the end");
    }
    state.closing.store(true, Ordering::SeqCst);
    state.poke.notify_one();
    if let Err(error) = state.surface.runner().close().await {
        tracing::debug!(error = %error, "closing the realtime session failed");
    }
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
                    self.playing_until = match self.barge_in.take() {
                        // The transport flushed what the barge-in interrupted: only audio
                        // sent since still plays.
                        Some(since) => Some(since.map_or(now, |until| until.max(now))),
                        None => Some(self.playing_until.map_or(now, |until| until.max(now))),
                    };
                }
                VoiceClientMessage::End => {
                    self.end().await;
                    return Flow::Break;
                }
                // Only the first message of a socket is a hello; a later one is ignored.
                VoiceClientMessage::Hello { .. } => {}
                VoiceClientMessage::CancelAction { action_id } => {
                    if let Some(action) = self.actions.remove(&action_id) {
                        action.abort();
                    }
                }
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
        if self.utterances.len() == UTTERANCES {
            self.utterances.pop_front();
        }
        self.utterances.push_back(Utterance { bound, echo: self.over_playback(now) });
    }

    async fn on_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::SpeechStarted { .. } => {
                let now = Instant::now();
                if self.over_playback(now) {
                    self.send(VoiceUpdate::BargeIn).await;
                    self.barge_in = Some(None);
                }
                self.begin_utterance(now);
            }
            // A provider that reports no speech onset starts the utterance with its transcript.
            ServerEvent::InputTranscriptDelta { .. } if self.utterances.is_empty() => {
                self.begin_utterance(Instant::now());
            }
            ServerEvent::InputTranscriptCompleted { transcript, .. } => {
                let utterance =
                    self.utterances.pop_front().unwrap_or(Utterance { bound: None, echo: true });
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
                if let Some(since) = &mut self.barge_in {
                    let from = since.filter(|until| *until > now).unwrap_or(now);
                    *since = Some(from + length);
                }
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
            ServerEvent::ToolConfirmationRequested { call_id, request } => {
                if let Err(error) = self.surface.record_held(&call_id, &request).await {
                    tracing::warn!(error = %error, "the hold's audit record failed");
                }
                self.refresh().await;
            }
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
        let pending = !self.pending.is_empty() || !self.actions.is_empty();
        let Some(said) = spoken::parse(transcript, pending) else {
            // Not one of the engine's answers: perhaps a screen action, never from the model's
            // own audio (spec 3.3 §5.6).
            if !utterance.echo {
                self.spoken_action(transcript).await;
            }
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
                self.cancel_actions();
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

    /// Ends the conversation, once: a spoken answer in its cancel window is cancelled.
    async fn end(&mut self) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        let viewer = self.viewer.clone();
        self.cancel_pending(&viewer).await;
        self.cancel_actions();
        // The cancelled answers report through their aborted windows.
        while !self.settled.is_empty() {
            match tokio::time::timeout(Duration::from_secs(1), self.settled.next()).await {
                Ok(Some((id, settled))) => self.on_settled(id, settled).await,
                Ok(None) | Err(_) => break,
            }
        }
        self.send(VoiceUpdate::Ended).await;
    }

    /// Cancels every spoken screen action of this conversation in its cancel window.
    fn cancel_actions(&mut self) {
        for (_, action) in self.actions.drain() {
            action.abort();
        }
    }

    /// Reads `transcript` as a screen action on the person's Coming up and, for one, waits out
    /// the engine's cancel window before taking it (spec 3.3 §5.6, owner question 26).
    #[cfg(feature = "verb-ui")]
    async fn spoken_action(&mut self, transcript: &str) {
        use crate::verbs::spoken::{SpokenAction, parse};
        use crate::verbs::{ObjectRef, VerbIntent, problem_line, words as verb_words};

        let Some(gate) = self.verbs.clone() else {
            return;
        };
        let Ok(items) = gate.coming_up(&self.speaker).await else {
            return;
        };
        let (item, title, tomorrow) = match parse(transcript, &items) {
            None => return,
            Some(SpokenAction::Ambiguous { said, matches }) => {
                let text = format!(
                    "{} — say the full name or use the screen",
                    verb_words::things_match(matches.len(), &said)
                );
                self.send(VoiceUpdate::ActionOutcome {
                    action_id: None,
                    done: false,
                    text,
                    heading: None,
                })
                .await;
                return;
            }
            Some(SpokenAction::Snooze { item, title, tomorrow }) => (item, title, tomorrow),
        };
        let Some(found) = items.iter().find(|found| found.id == item) else {
            return;
        };
        let choice = found.choices.iter().find(|choice| choice.tomorrow == tomorrow);
        let Some(choice) = choice.filter(|_| found.why_not.is_none()) else {
            let why = found.why_not.clone().unwrap_or_else(|| verb_words::NOT_ONE_TIME.to_string());
            let text = verb_words::cant_snooze(&why);
            self.send(VoiceUpdate::ActionOutcome {
                action_id: None,
                done: false,
                text,
                heading: None,
            })
            .await;
            return;
        };
        let heading = verb_words::move_to(&title, &choice.at_text);
        let done = verb_words::moved_to(&title, &choice.at_text);
        let said_done = format!("You snoozed {title} by voice");
        let window = gate.voice_cancel_window();
        let id = format!("act-{}", uuid::Uuid::new_v4());
        let cancel_until = chrono::Utc::now()
            + chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::seconds(5));
        let intent = VerbIntent::new("snooze", serde_json::json!({ "to": choice.to }))
            .on(ObjectRef::coming_up(&item));
        let speaker = self.speaker.clone();
        let said = transcript.to_string();
        let (task_id, task_heading) = (id.clone(), heading.clone());
        let handle = tokio::spawn(async move {
            tokio::time::sleep(window).await;
            match gate.call_spoken(&speaker, intent, &said).await {
                Ok(_) => VoiceUpdate::ActionOutcome {
                    action_id: Some(task_id),
                    done: true,
                    text: said_done,
                    heading: Some(done),
                },
                Err(error) => VoiceUpdate::ActionOutcome {
                    action_id: Some(task_id),
                    done: false,
                    text: problem_line(&error),
                    heading: Some(task_heading),
                },
            }
        });
        self.actions.insert(id.clone(), handle.abort_handle());
        let cancelled = VoiceUpdate::ActionOutcome {
            action_id: Some(id.clone()),
            done: false,
            text: verb_words::CANCELLED_NOTHING_CHANGED.to_string(),
            heading: Some(heading.clone()),
        };
        let ending = id.clone();
        self.acting.push(Box::pin(async move {
            match handle.await {
                Ok(update) => (ending, update),
                Err(_) => (ending, cancelled),
            }
        }));
        let seconds = window.as_millis().div_ceil(1000) as u64;
        self.send(VoiceUpdate::PendingAction {
            action_id: id,
            cancel_until,
            text: verb_words::snoozing_in(seconds),
            heading,
        })
        .await;
    }

    /// Without screen actions, no words take one.
    #[cfg(not(feature = "verb-ui"))]
    async fn spoken_action(&mut self, _transcript: &str) {}

    /// Resumes the conversation's held calls that have a decision, on their own task and one
    /// at a time: a resumed call runs its tool, which can take long.
    fn resume(&mut self) {
        while self.resumes.try_join_next().is_some() {}
        // One running and one waiting cover every decision made meanwhile; a task that ends
        // unjoined is counted until it is joined, so the ask is kept for the next to end.
        if self.resumes.len() >= 2 {
            self.resume_again = true;
            return;
        }
        let surface = Arc::clone(&self.surface);
        let lock = Arc::clone(&self.resume_lock);
        let output = self.output.clone();
        self.resumes.spawn(async move {
            let _one_at_a_time = lock.lock().await;
            if let Err(error) = surface.resume_held_calls().await {
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
                Ok(cards) if self.own_only => {
                    cards.into_iter().filter(|card| card.session_id == self.session_id).collect()
                }
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
        // Every decision voice cannot allow once: high risk, money, or a call shown cut short.
        let needs_screen: Vec<String> = cards
            .iter()
            .filter(|card| {
                card.allowed.option(Answer::AllowOnce).is_none_or(|option| option.needs_expanded)
            })
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
