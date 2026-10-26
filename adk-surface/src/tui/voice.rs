//! The terminal's voice mode (spec 3.3 §5.2, §5.6; features `tui` and `voice`): a voice
//! conversation beside the thread, its state on one line above the message box, or printed
//! into the scrollback by the inline terminal.
//!
//! Audio comes in and goes out on channels the host supplies, as PCM16 mono — the host
//! records the microphone and plays the speaker. The conversation is the hub's, in process:
//! the same spoken answers through the engine, the same echo rule, the same records as the
//! voice view. A spoken answer applies only to the voice session's own decisions, which the
//! thread shows in full, and the line names the one prompted. `c` cancels a spoken "allow
//! once" while it counts down and the message box is empty; Esc never decides.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use super::app::Update;
use super::safe::safe_line;
use crate::engine::SurfaceEngine;
use crate::model::DecisionCard;
use crate::origin::Caller;
use crate::rules::{Answer, Risk};
use crate::voice::{VoiceClientMessage, VoiceHub, VoiceInput, VoiceOutput, VoiceUpdate};
use crate::words;

/// How long leaving the terminal waits for the conversation to end.
const END_WAIT: Duration = Duration::from_secs(2);

/// A voice conversation for the terminal (spec 3.3 §5.6): the hub, the model, and the host's
/// microphone and speaker as channels of PCM16 mono — up at the model's input rate, down at
/// its output rate.
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::tui::{TerminalSurface, TerminalVoice, UiMode};
/// use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
///
/// # async fn example(engine: SurfaceEngine, hub: adk_surface::voice::VoiceHub) -> std::io::Result<()> {
/// let (microphone, heard) = tokio::sync::mpsc::channel(64);
/// let (speaker, mut played) = tokio::sync::mpsc::channel(64);
/// # let _ = (microphone, played.recv());
/// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
/// let voice = TerminalVoice::new(hub, "gemini", heard, speaker);
/// TerminalSurface::new(engine, me).voice(voice).run(UiMode::FullScreen).await?;
/// # Ok(())
/// # }
/// ```
pub struct TerminalVoice {
    hub: VoiceHub,
    model: String,
    microphone: mpsc::Receiver<Vec<u8>>,
    speaker: mpsc::Sender<Vec<u8>>,
    flush: Option<Arc<Notify>>,
}

impl std::fmt::Debug for TerminalVoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalVoice").field("model", &self.model).finish_non_exhaustive()
    }
}

impl TerminalVoice {
    /// A conversation on `model` of `hub`, hearing `microphone` and speaking to `speaker`.
    pub fn new(
        hub: VoiceHub,
        model: impl Into<String>,
        microphone: mpsc::Receiver<Vec<u8>>,
        speaker: mpsc::Sender<Vec<u8>>,
    ) -> Self {
        Self { hub, model: model.into(), microphone, speaker, flush: None }
    }

    /// Notifies `flush` when the person speaks over the model: the host then drops the audio
    /// it has queued for the speaker. Without it the queued audio plays out, and speech is
    /// taken for the model's echo until it would have ended (spec 3.3 §5.6).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::tui::TerminalVoice;
    ///
    /// # fn example(hub: adk_surface::voice::VoiceHub) {
    /// let (_microphone, heard) = tokio::sync::mpsc::channel(64);
    /// let (speaker, _played) = tokio::sync::mpsc::channel(64);
    /// let flush = Arc::new(tokio::sync::Notify::new());
    /// let voice = TerminalVoice::new(hub, "gemini", heard, speaker).with_flush(flush.clone());
    /// # let _ = voice;
    /// # }
    /// ```
    #[must_use]
    pub fn with_flush(mut self, flush: Arc<Notify>) -> Self {
        self.flush = Some(flush);
        self
    }
}

/// Where the terminal keeps its voice conversation until it runs: taken once.
pub(crate) type VoiceSlot = Arc<Mutex<Option<TerminalVoice>>>;

/// What the voice line shows (spec 3.3 §5.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VoiceLine {
    /// The model speaks.
    pub speaking: bool,
    /// What the person said last.
    pub heard: Option<String>,
    /// A spoken "allow once" counting down: its decision and when it takes effect.
    pub pending: Option<(String, Instant)>,
    /// The heading of the decision counting down.
    pub pending_heading: Option<String>,
    /// The decision a spoken answer applies to now, and the words that answer it.
    pub prompt: Option<String>,
    /// The newest decision voice cannot approve, and where it is decided.
    pub needs_screen: Option<String>,
    /// How the last spoken answer ended, when nothing was decided.
    pub outcome: Option<String>,
    /// What went wrong last, in the conversation's words.
    pub problem: Option<String>,
    /// The conversation ended.
    pub ended: bool,
}

impl VoiceLine {
    /// The line, in plain words; everything a person or a model said is escaped (S-I9).
    pub(crate) fn text(&self, now: Instant) -> String {
        if self.ended {
            return ENDED.to_string();
        }
        if let Some((_, until)) = &self.pending {
            return countdown(*until, now, self.pending_heading.as_deref());
        }
        let said = [&self.outcome, &self.problem, &self.prompt, &self.needs_screen];
        if let Some(text) = said.into_iter().flatten().next() {
            return safe_line(text);
        }
        if let Some(heard) = &self.heard {
            return words::heard(&safe_line(heard));
        }
        if self.speaking { words::SPEAKING } else { words::LISTENING }.to_string()
    }

    /// What the inline terminal prints when the line changes from `before`: what was heard, a
    /// countdown starting, how a spoken answer ended, the decision prompted, a call voice
    /// can't approve, a problem, the end. Listening, speaking and the countdown's ticks print
    /// nothing, since the scrollback would repeat them.
    pub(crate) fn news(&self, before: &VoiceLine, now: Instant) -> Vec<String> {
        let mut news = Vec::new();
        if self.heard != before.heard
            && let Some(heard) = &self.heard
        {
            news.push(words::heard(&safe_line(heard)));
        }
        if let Some((id, until)) = &self.pending
            && before.pending.as_ref().is_none_or(|(was, _)| was != id)
        {
            news.push(countdown(*until, now, self.pending_heading.as_deref()));
        }
        // The prompted decision voice can't approve is printed once, as the prompt.
        let needs_screen =
            if self.needs_screen == self.prompt { &None } else { &self.needs_screen };
        for (now_said, before_said) in [
            (&self.outcome, &before.outcome),
            (&self.prompt, &before.prompt),
            (needs_screen, &before.needs_screen),
            (&self.problem, &before.problem),
        ] {
            if now_said != before_said
                && let Some(text) = now_said
            {
                news.push(safe_line(text));
            }
        }
        if self.ended && !before.ended {
            news.push(ENDED.to_string());
        }
        news
    }
}

/// What the line says once the conversation has ended.
const ENDED: &str = "The voice conversation ended.";

/// A spoken "allow once" of the decision `heading` counting down to `until`.
fn countdown(until: Instant, now: Instant, heading: Option<&str>) -> String {
    let seconds = until.saturating_duration_since(now).as_millis().div_ceil(1000);
    let allowing = words::allowing_in(u64::try_from(seconds).unwrap_or(u64::MAX));
    match heading {
        Some(heading) => format!("{allowing}: {} — c to cancel", safe_line(heading)),
        None => format!("{allowing} — c to cancel"),
    }
}

/// What the line says of a decision voice cannot allow once, as the voice view says it.
fn cant_approve(card: &DecisionCard) -> String {
    let why = if card.money || card.risk == Risk::High {
        words::voice_cant_approve(&card.heading, card.money)
    } else {
        // Shown cut short: approving needs the call in full, on screen.
        words::wants_to(&card.agent.display_name, &card.heading)
    };
    format!("{why} {}", words::SAY_DENY_OR_DECIDE_ON_SCREEN)
}

/// A running voice conversation of the terminal.
pub(crate) struct Running {
    input: mpsc::Sender<VoiceInput>,
    task: JoinHandle<()>,
}

impl Running {
    /// Ends the conversation — a spoken answer in its window is cancelled — and waits a
    /// moment for it to close.
    pub(crate) async fn end(self) {
        let _ = self.input.send(VoiceInput::Message(VoiceClientMessage::End)).await;
        drop(self.input);
        if tokio::time::timeout(END_WAIT, self.task).await.is_err() {
            tracing::warn!("the voice conversation did not end in time");
        }
    }
}

/// Opens the conversation `slot` holds, when it holds one: see [`start`].
pub(crate) async fn open(
    slot: &VoiceSlot,
    engine: &SurfaceEngine,
    caller: &Caller,
    updates: &mpsc::UnboundedSender<Update>,
) -> std::io::Result<Option<(String, Running)>> {
    let voice = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    match voice {
        Some(voice) => {
            Ok(Some(start(voice, engine.clone(), caller.clone(), updates.clone()).await?))
        }
        None => Ok(None),
    }
}

/// Opens the conversation as `caller` and carries it to the terminal's app: returns its
/// session, which the terminal shows.
pub(crate) async fn start(
    voice: TerminalVoice,
    engine: SurfaceEngine,
    caller: Caller,
    updates: mpsc::UnboundedSender<Update>,
) -> std::io::Result<(String, Running)> {
    let TerminalVoice { hub, model, mut microphone, speaker, flush } = voice;
    let link = hub.open_in_thread(&caller, &model).await.map_err(std::io::Error::other)?;
    let session = link.session_id.clone();
    let input = link.input.clone();
    let task = tokio::spawn(async move {
        let mut output = link.output;
        let sender = link.input;
        let mut line = VoiceLine::default();
        let mut hearing = true;
        loop {
            tokio::select! {
                pcm = microphone.recv(), if hearing => match pcm {
                    Some(pcm) => {
                        if sender.send(VoiceInput::Audio(pcm)).await.is_err() {
                            break;
                        }
                    }
                    None => hearing = false,
                },
                out = output.recv() => match out {
                    Some(VoiceOutput::Audio(pcm)) => {
                        // A slow speaker drops audio rather than holding the conversation up.
                        let _ = speaker.try_send(pcm);
                    }
                    // Spoken over: the host drops what it queued, and the conversation is
                    // told, so speech counts again from now on.
                    Some(VoiceOutput::Update(VoiceUpdate::BargeIn)) => {
                        if let Some(flush) = &flush {
                            flush.notify_one();
                            let ended = VoiceInput::Message(VoiceClientMessage::PlaybackEnded);
                            if sender.send(ended).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(VoiceOutput::Update(update)) => {
                        apply(&mut line, update, &engine, &caller).await;
                        if updates.send(Update::Voice(line.clone())).is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
    });
    Ok((session, Running { input, task }))
}

/// What one update of the conversation changes on the voice line.
async fn apply(line: &mut VoiceLine, update: VoiceUpdate, engine: &SurfaceEngine, caller: &Caller) {
    match update {
        VoiceUpdate::Ready { .. } | VoiceUpdate::Listening => line.speaking = false,
        VoiceUpdate::Speaking => line.speaking = true,
        VoiceUpdate::Heard { text } => {
            line.heard = Some(text);
            line.problem = None;
            line.outcome = None;
        }
        VoiceUpdate::Pending { decision_id, cancel_until, .. } => {
            let left = (cancel_until - Utc::now()).to_std().unwrap_or_default();
            line.pending_heading =
                engine.decision(caller, &decision_id).await.ok().map(|card| card.heading);
            line.pending = Some((decision_id, Instant::now() + left));
            line.outcome = None;
        }
        VoiceUpdate::Outcome { decision_id, text, .. } => {
            if line.pending.as_ref().is_some_and(|(id, _)| *id == decision_id) {
                line.pending = None;
                line.pending_heading = None;
            }
            line.outcome = text;
        }
        VoiceUpdate::Prompted { decision_id, .. } => {
            let card = match decision_id {
                Some(id) => engine.decision(caller, &id).await.ok(),
                None => None,
            };
            line.prompt = card.map(|card| {
                let allows = card
                    .allowed
                    .option(Answer::AllowOnce)
                    .is_some_and(|option| !option.needs_expanded);
                if allows {
                    let wants = words::wants_to(&card.agent.display_name, &card.heading);
                    format!("{wants} {}", words::SAY_ALLOW_ONCE_OR_DENY)
                } else {
                    cant_approve(&card)
                }
            });
            line.outcome = None;
        }
        VoiceUpdate::NeedsScreen { decision_ids } => {
            line.needs_screen = match decision_ids.last() {
                Some(id) => engine.decision(caller, id).await.ok().map(|card| cant_approve(&card)),
                None => None,
            };
        }
        VoiceUpdate::Ended => line.ended = true,
        VoiceUpdate::Error { message, .. } => line.problem = Some(message),
        VoiceUpdate::BargeIn => {}
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use proptest::prelude::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::engine::Command;
    use crate::tui::app::{App, Effect};
    use crate::tui::inline::Printer;
    use crate::tui::theme::{Appearance, Theme};
    use crate::tui::view::Renderer;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// An app whose voice conversation counts a spoken "allow once" down.
    fn counting_down(now: Instant) -> App {
        let mut app = App::new("adk-rust", "voice-1".into());
        let line = VoiceLine {
            heard: Some("allow once".into()),
            pending: Some(("apr_1".into(), now + Duration::from_secs(5))),
            ..VoiceLine::default()
        };
        app.update(Update::Voice(line), now);
        app
    }

    fn screen(app: &App, appearance: Appearance, now: Instant) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut renderer = Renderer::new(Theme::new(appearance, true));
        terminal
            .draw(|frame| {
                renderer.draw(frame, app, now);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The pending line renders above the message box, in light and dark.
    #[test]
    fn the_pending_line_renders() {
        let now = Instant::now();
        let app = counting_down(now);
        for appearance in [Appearance::Light, Appearance::Dark] {
            let screen = screen(&app, appearance, now);
            assert!(screen.contains("Allowing once in 5 seconds — c to cancel"), "{screen}");
        }
    }

    /// `c` cancels the spoken answer only from an empty message box; typed, it is a letter.
    #[test]
    fn c_cancels_only_from_an_empty_message_box() {
        let now = Instant::now();
        let mut app = counting_down(now);
        let effects = app.key(key(KeyCode::Char('c')), now);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Execute(Command::CancelAnswer { decision_id })] if decision_id == "apr_1"
        ));
        let mut app = counting_down(now);
        app.key(key(KeyCode::Char('o')), now);
        let effects = app.key(key(KeyCode::Char('c')), now);
        assert!(effects.is_empty());
        assert_eq!(app.composer.text, "oc");
        // Nothing counting down: `c` is a letter.
        let mut app = App::new("adk-rust", "voice-1".into());
        app.update(Update::Voice(VoiceLine::default()), now);
        assert!(app.key(key(KeyCode::Char('c')), now).is_empty());
        assert_eq!(app.composer.text, "c");
    }

    #[test]
    fn esc_never_decides_or_cancels() {
        let now = Instant::now();
        let mut app = counting_down(now);
        assert!(app.key(key(KeyCode::Esc), now).is_empty());
        assert!(app.voice.as_ref().is_some_and(|voice| voice.pending.is_some()));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// No key sequence answers a decision from the voice line: it only cancels.
        #[test]
        fn keys_never_decide_from_the_voice_line(keys in prop::collection::vec(prop_oneof![
            Just(KeyCode::Char('c')), Just(KeyCode::Char('y')), Just(KeyCode::Char('n')),
            Just(KeyCode::Char('a')), Just(KeyCode::Char('e')), Just(KeyCode::Esc),
            Just(KeyCode::Enter), Just(KeyCode::Backspace),
        ], 1..20)) {
            let now = Instant::now();
            let mut app = counting_down(now);
            for (step, code) in keys.into_iter().enumerate() {
                let at = now + Duration::from_millis(step as u64 * 50);
                for effect in app.key(key(code), at) {
                    let decides = matches!(effect, Effect::Execute(Command::Decide { .. }));
                    prop_assert!(!decides, "a key decided from the voice line");
                }
            }
        }
    }

    /// The inline terminal prints what was heard, each countdown, the prompt, how a spoken
    /// answer ended, a call voice can't approve and the end once each, escaped; listening,
    /// speaking and the countdown's ticks print nothing.
    #[test]
    fn the_inline_terminal_prints_each_piece_of_news_once() {
        fn step(printer: &mut Printer<Vec<u8>>, app: &mut App, line: &VoiceLine, at: Instant) {
            app.update(Update::Voice(line.clone()), at);
            printer.sync(app, at).unwrap();
        }
        let now = Instant::now();
        let mut printer =
            Printer::new(Vec::new(), Theme::new(Appearance::Light, false), false, false)
                .with_width(90);
        let mut app = App::new("adk-rust", "voice-1".into());
        let mut line = VoiceLine::default();
        step(&mut printer, &mut app, &line, now);
        line.speaking = true;
        step(&mut printer, &mut app, &line, now);
        line.speaking = false;
        line.heard = Some("allow\u{1b}[2J once".into());
        step(&mut printer, &mut app, &line, now);
        line.pending = Some(("apr_1".into(), now + Duration::from_secs(5)));
        line.pending_heading = Some("Send the invoice".into());
        step(&mut printer, &mut app, &line, now);
        step(&mut printer, &mut app, &line, now + Duration::from_secs(2));
        line.pending = None;
        line.outcome = Some(words::CHANGED_WHILE_YOU_SPOKE.into());
        step(&mut printer, &mut app, &line, now + Duration::from_secs(5));
        let release = "\"Publish the release\" is high risk, so voice can't approve it. Say \"deny\", or decide on screen.";
        line.outcome = None;
        line.prompt = Some(release.into());
        line.needs_screen = Some(release.into());
        step(&mut printer, &mut app, &line, now + Duration::from_secs(6));
        line.ended = true;
        step(&mut printer, &mut app, &line, now + Duration::from_secs(7));
        let out = String::from_utf8(printer.into_inner()).unwrap();
        let printed: Vec<&str> = out.lines().filter(|line| !line.trim().is_empty()).collect();
        assert_eq!(
            printed,
            [
                "Heard \"allow\\u{1b}[2J once\"",
                "Allowing once in 5 seconds: Send the invoice — c to cancel",
                words::CHANGED_WHILE_YOU_SPOKE,
                release,
                "The voice conversation ended.",
            ]
        );
    }

    /// The line names the decision a spoken answer applies to, and the one counting down
    /// (spec 3.3 §5.6, §7.1): the person sees what they answer.
    #[test]
    fn the_line_names_the_prompted_decision() {
        let now = Instant::now();
        let mut line = VoiceLine {
            heard: Some("hello".into()),
            prompt: Some(
                "Finance wants to send email to [client email]. Say \"allow once\" or \"deny\"."
                    .into(),
            ),
            ..VoiceLine::default()
        };
        assert_eq!(
            line.text(now),
            "Finance wants to send email to [client email]. Say \"allow once\" or \"deny\"."
        );
        line.pending = Some(("apr_1".into(), now + Duration::from_secs(5)));
        line.pending_heading = Some("Send email to [client email]".into());
        assert_eq!(
            line.text(now),
            "Allowing once in 5 seconds: Send email to [client email] — c to cancel"
        );
    }

    /// How a spoken answer ended is on the line when nothing was decided (spec 3.3 S-I2).
    #[test]
    fn the_line_says_how_a_spoken_answer_ended() {
        let now = Instant::now();
        let mut line = VoiceLine {
            heard: Some("allow once".into()),
            prompt: Some("Finance wants to send email. Say \"allow once\" or \"deny\".".into()),
            ..VoiceLine::default()
        };
        line.outcome = Some(words::VOICE_CANT_APPROVE_THIS.into());
        assert_eq!(line.text(now), words::VOICE_CANT_APPROVE_THIS);
        let before = VoiceLine { outcome: None, ..line.clone() };
        assert_eq!(line.news(&before, now), [words::VOICE_CANT_APPROVE_THIS]);
    }

    /// A call voice cannot allow once says why in the voice view's words: money is not high
    /// risk, and a call shown cut short is neither (spec 3.3 §8).
    #[test]
    fn a_call_voice_cant_approve_says_why() {
        let mut money = crate::tui::tests::card("apr_1", "pay_invoice", serde_json::json!({}));
        money.heading = "Pay the invoice".into();
        money.risk = Risk::Low;
        money.money = true;
        assert_eq!(
            cant_approve(&money),
            "\"Pay the invoice\" spends money, so voice can't approve it. Say \"deny\", or decide on screen."
        );
        let mut high = money.clone();
        high.money = false;
        high.risk = Risk::High;
        assert_eq!(
            cant_approve(&high),
            "\"Pay the invoice\" is high risk, so voice can't approve it. Say \"deny\", or decide on screen."
        );
        let mut cut = high.clone();
        cut.risk = Risk::Low;
        assert_eq!(
            cant_approve(&cut),
            "Finance wants to pay the invoice. Say \"deny\", or decide on screen."
        );
    }

    #[test]
    fn the_line_says_one_thing_and_escapes_what_was_said() {
        let now = Instant::now();
        let mut line = VoiceLine::default();
        assert_eq!(line.text(now), "Listening");
        line.heard = Some("allow\u{1b}[2J once".into());
        assert_eq!(line.text(now), "Heard \"allow\\u{1b}[2J once\"");
        line.needs_screen = Some("\"Publish\u{1b}[2J\" is high risk".into());
        assert_eq!(line.text(now), "\"Publish\\u{1b}[2J\" is high risk");
        line.pending = Some(("apr_1".into(), now + Duration::from_millis(4_200)));
        assert_eq!(line.text(now), "Allowing once in 5 seconds — c to cancel");
    }
}
