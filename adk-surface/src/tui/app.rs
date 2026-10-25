//! What the terminal shows and what each key does (spec 3.3 §5.2, §7.4): the state both the
//! full-screen and the inline UI draw, rebuilt from the engine on every update.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::Value;

use super::commands::{Commands, Completion};
use super::view::needs_details;
use crate::engine::{Command, CommandOutcome, FeedFrame};
use crate::model::{
    AgentRow, Briefing, DecidedAnswer, DecisionCard, FeedEntry, Mode, StatusLine, ThreadItem,
};
use crate::origin::Via;
use crate::rules::Answer;
use crate::words;

/// How long a decision box is on screen, and how long after the last keystroke into the
/// message box, before its keys answer it: a key pressed for a message as a box appears types
/// into the message instead (spec 3.3 §7.4 rule 2, S-I4).
pub(crate) const ARM_DELAY: Duration = Duration::from_millis(700);

/// How many notices the thread keeps.
const NOTICES: usize = 4;

/// How long a notice stays under the thread: long enough to read a line of about twenty words,
/// short enough that it never sits there as the conversation moves on.
pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(6);

/// How long the keys wait for an answer's outcome before they answer again.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// How long after a decision was answered somewhere else its keys still do nothing: a key
/// meant for the box that just closed never becomes a message (§7.4 rule 2, S-I4).
pub(crate) const CLOSE_HOLD: Duration = Duration::from_secs(2);

/// How long a Permissions key waits for the same key again before it changes anything.
pub(crate) const CONFIRM_WINDOW: Duration = Duration::from_secs(3);

/// How soon after a decision key another key makes both typing: a console that has no
/// bracketed paste (Windows) delivers a paste as keys, all at once.
pub(crate) const KEY_BURST: Duration = Duration::from_millis(40);

/// How far PageUp and PageDown move.
const PAGE: usize = 10;

/// A command that failed, as the terminal shows it: in the engine's words, never with an inbox
/// id (spec 3.3 §8 "No internal identifiers").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub text: String,
    /// The decision answered is no longer open: it was answered somewhere else.
    pub gone: Option<String>,
}

/// What the engine reports to the terminal.
#[derive(Debug, Clone)]
pub(crate) enum Update {
    Status(StatusLine),
    Agents(Vec<AgentRow>),
    /// A frame of the session shown since the `generation`-th switch: a frame read for a
    /// session shown before is never kept, even when the person came back to it.
    Frame {
        session: String,
        generation: u64,
        frame: FeedFrame,
    },
    /// A decision of the shown session that is no longer open, as it was decided.
    Decided {
        generation: u64,
        card: Box<DecisionCard>,
    },
    Briefing(Briefing),
    Queued {
        session: String,
        lines: Vec<String>,
    },
    /// Boxed: a command's outcome may carry a whole decision card.
    Outcome(Box<Result<CommandOutcome, Failure>>),
    /// What the `serial`-th slash command an embedder added returned, or why it failed.
    CommandDone {
        serial: u64,
        result: Result<String, String>,
    },
    Error(String),
    /// The voice conversation's line (spec 3.3 §5.6).
    #[cfg(feature = "voice")]
    Voice(super::voice::VoiceLine),
    /// A line the application sent while the terminal runs.
    Notice(String),
}

/// What a key asks the runner to do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect {
    Execute(Command),
    Session(String),
    /// `/session`: open this session, or a new one, through the surface's session gate.
    OpenSession(Option<String>),
    /// Take back the newest message queued for this session, before it is sent.
    Unqueue(String),
    /// Read "While you were away" again now: Today was opened.
    Briefing,
    /// Run the slash command `name` an embedder added, with `args`; its output is the
    /// `serial`-th the terminal shows (spec 3.3 §5.2).
    RunCommand {
        serial: u64,
        name: String,
        args: String,
    },
    Quit,
}

/// The right pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Thread,
    Today,
    Permissions,
    Help,
    /// What a slash command an embedder added returned (spec 3.3 §5.2).
    Output,
    /// The person's own agents (spec 3.3 §5.7): Ctrl-G or `/agents`.
    Agents,
}

/// A slash command run from the message box, as the Output view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    /// Which run it is: an answer of an earlier run is never shown for a later one.
    pub serial: u64,
    /// What was typed, as `/rules rm 2`.
    pub line: String,
    /// What it returned, or why it failed; `None` while it runs.
    pub result: Option<Result<String, String>>,
}

/// What the message box holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    /// A message, or a `/` command.
    Message,
    /// The changed arguments of the decision `id`, as JSON, chosen at `version` (S-I2).
    EditArgs { id: String, version: u64 },
    /// Why the decision `id` is denied, optional, chosen at `version` (S-I2).
    DenyReason { id: String, version: u64 },
}

/// The message box: text and a cursor, in characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Composer {
    pub text: String,
    pub cursor: usize,
    /// How many times the text changed: any change disarms the decision keys for a moment.
    edits: u64,
}

impl Composer {
    fn byte(&self, index: usize) -> usize {
        self.text.char_indices().nth(index).map_or(self.text.len(), |(at, _)| at)
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn insert(&mut self, c: char) {
        let at = self.byte(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
        self.edits += 1;
    }

    /// Inserts `text` at the cursor in one pass, so a large paste costs its length, not its
    /// length squared.
    fn insert_str(&mut self, text: &str) {
        let flat: String =
            text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect();
        if flat.is_empty() {
            return;
        }
        let at = self.byte(self.cursor);
        self.text.insert_str(at, &flat);
        self.cursor += flat.chars().count();
        self.edits += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
            self.edits += 1;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte(self.cursor);
            self.text.remove(at);
            self.edits += 1;
        }
    }

    fn delete_word(&mut self) {
        while self.cursor > 0 && self.text.chars().nth(self.cursor - 1) == Some(' ') {
            self.backspace();
        }
        while self.cursor > 0 && self.text.chars().nth(self.cursor - 1) != Some(' ') {
            self.backspace();
        }
    }

    fn set(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
        self.edits += 1;
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        if !self.text.is_empty() {
            self.edits += 1;
        }
        std::mem::take(&mut self.text)
    }
}

/// A decision open in the shown session, drawn from its card (S-I1).
#[derive(Debug, Clone)]
pub(crate) struct OpenCard {
    pub card: DecisionCard,
    /// When it appeared, or last changed.
    pub shown: Instant,
    /// The call is shown in full, with every detail (§7.1).
    pub details: bool,
}

/// How a notice reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Decided,
    Problem,
}

/// How long the full screen shows a notice under the thread; inline prints each one once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lasts {
    /// [`NOTICE_TTL`], or until the next message is sent.
    Moment,
    /// Until the next key, or the next message: a problem, or what the terminal says as it
    /// starts, is never gone before the person has seen it.
    UntilKey,
    /// Not under the thread: the thread shows it at the decision's place (spec 3.3 §8.1).
    InThread,
}

/// A line the terminal adds under the thread: an answer, a stop, a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub text: String,
    pub tone: Tone,
    /// When it was added, or last said again.
    pub at: Instant,
    /// Its place among every notice ever added.
    pub serial: usize,
    pub lasts: Lasts,
}

/// A decision of the shown session that is no longer open, as the thread shows it at its place
/// (spec 3.3 §8.1 "Decided").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecidedLine {
    pub id: String,
    /// What would have happened, as the card says it.
    pub heading: String,
    /// "Decided".
    pub status: String,
    /// "You said yes in the terminal at 06:12".
    pub text: String,
    /// How it was answered, for its mark.
    pub answer: DecidedAnswer,
    /// Where it was answered, as "in Telegram", when the record says.
    pub place: Option<String>,
    /// The thread's last entry when it was decided: where it goes when no line names it.
    pub after_seq: u64,
}

/// What the terminal drew of the box the keys answer (§7.1): the box fits the screen with its
/// keys, and its call is shown a window at a time when the whole call does not fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoxView {
    /// The box, its keys included, is wholly on screen.
    pub on_screen: bool,
    /// How many rows the call has, or its details while `?` shows them.
    pub body_len: usize,
    /// The rows of the call on screen, counted from its first.
    pub body_shown: std::ops::Range<usize>,
    /// PageUp and PageDown move the call's window (the full screen); inline prints the call
    /// once, and `?` prints all of it.
    pub scrolls: bool,
}

/// Which box was drawn: the decision, its version, whether with its details, and how many
/// rows its call has.
type BoxIdentity = (String, u64, bool, usize);

/// The terminal's state.
#[derive(Debug)]
pub(crate) struct App {
    pub session: String,
    /// How many times the shown session changed: frames read before the last change are old.
    pub generation: u64,
    pub status: Option<StatusLine>,
    pub agents: Vec<AgentRow>,
    pub entries: Vec<FeedEntry>,
    pub cards: Vec<OpenCard>,
    pub briefing: Option<Briefing>,
    pub queued: Vec<String>,
    pub notices: Vec<Notice>,
    /// Every notice ever added, for the inline UI to print each once.
    pub notice_count: usize,
    /// Notices before this serial were dismissed by a message sent since.
    notices_from: usize,
    /// Decisions of the shown session no longer open, in the order they were learnt.
    pub decided: Vec<DecidedLine>,
    pub composer: Composer,
    pub input: Input,
    /// Esc moved the keys to the message box while a decision is open.
    pub typing: bool,
    pub view: View,
    /// Lines scrolled up from the bottom of the thread.
    pub scroll: usize,
    /// Rows scrolled down within a box taller than the thread pane, from its top.
    pub box_scroll: usize,
    pub model: Option<String>,
    pub place: Option<String>,
    pub app_name: String,
    /// The last keystroke into the message box.
    last_typed: Option<Instant>,
    /// The last key of any kind.
    last_key: Option<Instant>,
    /// The decision the keys act on, and since when: a decision that takes the focus arms
    /// again, so a repeated key never answers one the person did not have in front of them.
    focus: Option<(String, Instant)>,
    /// A decision answered by key whose outcome has not come back yet, and since when.
    answering: Option<(String, Instant)>,
    /// The focused decision was answered somewhere else: its id, version, and when.
    closed: Option<(String, u64, Instant)>,
    /// A key pressed after a close was swallowed, and said so.
    closed_said: bool,
    /// Decisions open while shown here, and those whose outcome was already said.
    seen_open: HashSet<String>,
    said_decided: HashSet<String>,
    /// A Permissions key waiting for the same key again.
    pending: Option<(char, Instant)>,
    /// A decision key held a moment to see whether more keys follow at once (a paste).
    held: Option<(char, KeyEvent, Instant)>,
    /// Decision keys are held a moment before they answer: on consoles without bracketed paste.
    pub burst_guard: bool,
    /// The time of the update or key being handled: what a notice added now is stamped with.
    now: Instant,
    /// The focused box as last drawn, and which rows of its call have been on screen since it
    /// took the focus or changed.
    box_seen: Option<(BoxIdentity, Vec<bool>)>,
    /// The focused decision's box, its keys included, is not on screen: its keys do not
    /// answer it (§7.1).
    pub box_hidden: bool,
    /// Rows of the focused decision's call have not been on screen yet: it is allowed or
    /// edited only once they have (§7.1); Deny and `?` work.
    pub box_unread: bool,
    /// How many rows of the call the box shows at once, and whether PageDown moves them.
    box_window: usize,
    box_scrolls: bool,
    /// The commands the terminal offers, its own and its embedder's (spec 3.3 §5.2).
    pub commands: Commands,
    /// The completion candidate the arrows chose.
    completion_selected: usize,
    /// The last slash command run from the message box, and what it returned.
    pub output: Option<CommandOutput>,
    /// How many slash commands were run.
    runs: u64,
    /// Rows scrolled down in a view other than the thread.
    pub view_scroll: usize,
    /// The rows one page of a view other than the thread shows, as drawn last: PageDown moves
    /// by it, so no row is skipped.
    pub view_page: usize,
    /// Inline: every view is printed into the scrollback above the box printed last, so the
    /// box's keys answer it whatever view was printed (spec 3.3 §5.2).
    pub inline: bool,
    /// The Agents view (spec 3.3 §5.7).
    pub agents_pane: super::agents::AgentsPane,
    /// An added agent with no thread yet, and the new session opened for it once open: the
    /// first message there starts the agent's thread (spec 3.3 §5.7 "Threads").
    pub fresh: Option<(String, Option<String>)>,
    pub quit: bool,
    /// The voice conversation beside the thread, when there is one (spec 3.3 §5.6).
    #[cfg(feature = "voice")]
    pub voice: Option<super::voice::VoiceLine>,
}

impl App {
    pub(crate) fn new(app_name: &str, session: String) -> Self {
        Self {
            session,
            generation: 0,
            status: None,
            agents: Vec::new(),
            entries: Vec::new(),
            cards: Vec::new(),
            briefing: None,
            queued: Vec::new(),
            notices: Vec::new(),
            notice_count: 0,
            notices_from: 0,
            decided: Vec::new(),
            composer: Composer::default(),
            input: Input::Message,
            typing: false,
            view: View::Thread,
            scroll: 0,
            box_scroll: 0,
            model: None,
            place: None,
            app_name: app_name.to_string(),
            last_typed: None,
            last_key: None,
            focus: None,
            answering: None,
            closed: None,
            closed_said: false,
            seen_open: HashSet::new(),
            said_decided: HashSet::new(),
            pending: None,
            held: None,
            burst_guard: false,
            now: Instant::now(),
            box_seen: None,
            box_hidden: false,
            box_unread: false,
            box_window: 0,
            box_scrolls: false,
            commands: Commands::default(),
            completion_selected: 0,
            output: None,
            runs: 0,
            view_scroll: 0,
            view_page: PAGE,
            inline: false,
            agents_pane: super::agents::AgentsPane::default(),
            fresh: None,
            quit: false,
            #[cfg(feature = "voice")]
            voice: None,
        }
    }

    /// The completion of a command being typed: while the message box holds `/` and the start
    /// of a name, and it holds a message (spec 3.3 §5.2).
    pub(crate) fn completion(&self) -> Option<Completion> {
        if self.input != Input::Message {
            return None;
        }
        let mut completion = Completion::of(&self.composer.text, &self.commands)?;
        completion.selected =
            self.completion_selected.min(completion.candidates.len().saturating_sub(1));
        Some(completion)
    }

    /// Adds a notice: a problem stays until the next key, anything else for a moment.
    pub(crate) fn notice(&mut self, text: impl Into<String>, tone: Tone) {
        let lasts = if tone == Tone::Problem { Lasts::UntilKey } else { Lasts::Moment };
        self.push_notice(text.into(), tone, lasts);
    }

    /// Adds a notice that stays until the next key: what the terminal says as it starts.
    pub(crate) fn notice_until_key(&mut self, text: impl Into<String>, tone: Tone) {
        self.push_notice(text.into(), tone, Lasts::UntilKey);
    }

    fn push_notice(&mut self, text: String, tone: Tone, lasts: Lasts) {
        let at = self.now;
        // The same words again refresh the notice still shown instead of stacking a copy.
        if let Some(last) = self.notices.last_mut()
            && last.serial >= self.notices_from
            && (last.text.as_str(), last.tone, last.lasts) == (text.as_str(), tone, lasts)
        {
            last.at = at;
            return;
        }
        self.notices.push(Notice { text, tone, at, serial: self.notice_count, lasts });
        self.notice_count += 1;
        if self.notices.len() > NOTICES {
            self.notices.remove(0);
        }
    }

    /// The notices the full screen shows under the thread at `now`: none a message sent since
    /// dismissed, none the thread shows at a decision's place, a passing one only for
    /// [`NOTICE_TTL`], and a problem until the next key.
    pub(crate) fn visible_notices(&self, now: Instant) -> impl Iterator<Item = &Notice> {
        self.notices.iter().filter(move |notice| {
            notice.serial >= self.notices_from
                && match notice.lasts {
                    Lasts::Moment => now.saturating_duration_since(notice.at) < NOTICE_TTL,
                    Lasts::UntilKey => self.last_key.is_none_or(|key| key <= notice.at),
                    Lasts::InThread => false,
                }
        })
    }

    /// The decision keys act on: the oldest open decision of the shown session.
    pub(crate) fn focused(&self) -> Option<&OpenCard> {
        self.cards.first()
    }

    /// Whether the decision keys answer the focused decision now (§7.4 rule 2).
    pub(crate) fn keys_answer(&self, now: Instant) -> bool {
        let Some(open) = self.focused() else {
            return false;
        };
        let focused = match &self.focus {
            Some((id, since)) if *id == open.card.id => (*since).max(open.shown),
            _ => now,
        };
        self.input == Input::Message
            && self.thread_shown()
            && self.composer.is_empty()
            && !self.typing
            && !self.box_hidden
            && !self.awaits_answer(now)
            && now.saturating_duration_since(focused) >= ARM_DELAY
            && self.last_typed.is_none_or(|at| now.saturating_duration_since(at) >= ARM_DELAY)
    }

    /// Whether the person sees the thread and the box the keys answer: the full screen shows
    /// one view at a time; inline prints each view into the scrollback, above the box printed
    /// last, except the Agents view, whose keys are its own.
    fn thread_shown(&self) -> bool {
        self.view == View::Thread || (self.inline && self.view != View::Agents)
    }

    /// An answer given by key is on its way: the keys wait for its outcome.
    fn awaits_answer(&self, now: Instant) -> bool {
        self.answering.as_ref().is_some_and(|(id, at)| {
            self.cards.first().is_some_and(|open| open.card.id == *id)
                && now.saturating_duration_since(*at) < ANSWER_WAIT
        })
    }

    /// Notes which decision has the focus now; one that takes it arms again (§7.4 rule 2).
    fn refocus(&mut self, now: Instant) {
        let first = self.cards.first().map(|open| open.card.id.clone());
        if self.focus.as_ref().map(|(id, _)| id) != first.as_ref() {
            self.focus = first.map(|id| (id, now));
            self.box_scroll = 0;
        }
        for open in &self.cards {
            self.seen_open.insert(open.card.id.clone());
        }
    }

    /// The focused decision arms again from `now`: it just came (back) into view.
    fn rearm(&mut self, now: Instant) {
        if let Some((_, since)) = &mut self.focus {
            *since = now;
        }
    }

    /// What the terminal just drew of the focused box (`None`: no box drawn). Its keys answer
    /// only while the box and its keys are on screen, and one that comes back into view arms
    /// again; its call is allowed or edited only once every row of it has been on screen since
    /// it took the focus or changed (§7.1, §7.4 rule 2).
    pub(crate) fn set_box_view(&mut self, view: Option<BoxView>, now: Instant) {
        let (Some(view), Some(open)) = (view, self.cards.first()) else {
            self.box_seen = None;
            self.box_hidden = false;
            self.box_unread = false;
            self.box_window = 0;
            return;
        };
        let identity = (open.card.id.clone(), open.card.version, open.details, view.body_len);
        if self.box_seen.as_ref().is_none_or(|(known, _)| *known != identity) {
            if self.box_seen.as_ref().is_some_and(|(known, _)| known.0 == identity.0) {
                // The same decision, changed or shown with its details: read it from its top.
                self.box_scroll = 0;
            }
            self.box_seen = Some((identity, vec![false; view.body_len]));
        }
        self.box_window = view.body_shown.len();
        self.box_scrolls = view.scrolls;
        self.box_scroll = self.box_scroll.min(view.body_len.saturating_sub(self.box_window));
        if let Some((_, seen)) = &mut self.box_seen
            && view.on_screen
        {
            for row in view.body_shown.clone() {
                if let Some(cell) = seen.get_mut(row) {
                    *cell = true;
                }
            }
        }
        self.box_unread =
            self.box_seen.as_ref().is_some_and(|(_, seen)| seen.iter().any(|row| !*row));
        if self.box_hidden && view.on_screen {
            self.rearm(now);
        }
        self.box_hidden = !view.on_screen;
    }

    /// Closes an edit or a reason being typed for `id`, which is no longer open, and says so;
    /// returns whether one was.
    fn answered_elsewhere(&mut self, id: &str) -> bool {
        let what = match &self.input {
            Input::EditArgs { id: editing, .. } if editing == id => "edit",
            Input::DenyReason { id: denying, .. } if denying == id => "reason",
            _ => return false,
        };
        self.input = Input::Message;
        self.composer.take();
        self.notice(
            format!("{} — answered somewhere else, so your {what} was not sent.", words::DECIDED),
            Tone::Decided,
        );
        true
    }

    /// The agent whose thread is shown, when the roster names it.
    pub(crate) fn shown_agent(&self) -> Option<&AgentRow> {
        let fresh = self.fresh_agent();
        self.agents
            .iter()
            .find(|row| row.session_id.as_deref() == Some(self.session.as_str()))
            .or_else(|| self.agents.iter().find(|row| Some(row.agent.name.as_str()) == fresh))
    }

    /// The agent whose new thread is shown, before its first message.
    fn fresh_agent(&self) -> Option<&str> {
        match &self.fresh {
            Some((agent, Some(session))) if *session == self.session => Some(agent),
            _ => None,
        }
    }

    /// Opens a new session for `agent`'s first thread.
    fn open_thread_with(&mut self, agent: String) -> Vec<Effect> {
        self.fresh = Some((agent, None));
        vec![Effect::OpenSession(None)]
    }

    pub(crate) fn switch_session(&mut self, session: String) -> Vec<Effect> {
        // The session opened for an agent's first thread is that thread; any other leaves it.
        self.fresh = match self.fresh.take() {
            Some((agent, None)) => Some((agent, Some(session.clone()))),
            _ => None,
        };
        if session == self.session {
            return Vec::new();
        }
        self.session = session.clone();
        self.generation += 1;
        self.entries.clear();
        self.cards.clear();
        self.queued.clear();
        self.decided.clear();
        self.scroll = 0;
        self.box_scroll = 0;
        if self.input != Input::Message {
            self.composer.take();
        }
        self.input = Input::Message;
        self.typing = false;
        self.focus = None;
        self.answering = None;
        self.closed = None;
        self.box_seen = None;
        self.box_hidden = false;
        self.box_unread = false;
        vec![Effect::Session(session)]
    }

    /// Learns how a decision of the shown session was decided; the thread shows it once, at
    /// its place (spec 3.3 §8.1 "Decided"). Returns whether the terminal had not said it yet.
    fn learn_decided(&mut self, card: &DecisionCard, text: &str) -> bool {
        let status = if card.status_text.is_empty() || card.decided.is_none() {
            words::DECIDED.to_string()
        } else {
            card.status_text.clone()
        };
        let answer = card.decided.as_ref().map_or(DecidedAnswer::Unknown, |decided| decided.answer);
        let place = card.decided.as_ref().and_then(|decided| decided.via.as_ref()).map(Via::phrase);
        let after_seq = self.entries.last().map_or(0, |entry| entry.seq);
        match self.decided.iter_mut().find(|line| line.id == card.id) {
            Some(line) => {
                text.clone_into(&mut line.text);
                if answer != DecidedAnswer::Unknown {
                    line.answer = answer;
                }
                if place.is_some() {
                    line.place = place;
                }
            }
            None => self.decided.push(DecidedLine {
                id: card.id.clone(),
                heading: card.heading.clone(),
                status,
                text: text.to_string(),
                answer,
                place,
                after_seq,
            }),
        }
        self.said_decided.insert(card.id.clone())
    }

    /// Says how a decision was decided: in the thread, and once as a notice that inline prints
    /// and the full screen leaves to the thread.
    fn decided_notice(&mut self, card: &DecisionCard, text: &str) {
        if self.learn_decided(card, text) {
            self.push_notice(
                format!("{} — {text}", words::DECIDED),
                Tone::Decided,
                Lasts::InThread,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Updates from the engine
    // -----------------------------------------------------------------------

    pub(crate) fn update(&mut self, update: Update, now: Instant) {
        self.now = now;
        match update {
            Update::Status(status) => self.status = Some(status),
            Update::Agents(agents) => self.agents = agents,
            Update::Briefing(briefing) => self.briefing = Some(briefing),
            Update::Queued { session, lines } if session == self.session => self.queued = lines,
            Update::Queued { .. } => {}
            Update::Frame { session, generation, frame }
                if session == self.session && generation == self.generation =>
            {
                self.frame(frame, now);
            }
            Update::Frame { .. } => {}
            Update::Decided { generation, card } if generation == self.generation => {
                let Some(decided) = card.decided.clone() else {
                    return;
                };
                if self.cards.iter().any(|open| open.card.id == card.id) {
                    return;
                }
                if self.seen_open.contains(&card.id) {
                    self.decided_notice(&card, &decided.text);
                } else {
                    // Decided before this terminal showed it: in the thread only.
                    self.learn_decided(&card, &decided.text);
                }
            }
            Update::Decided { .. } => {}
            Update::Outcome(outcome) => {
                self.answering = None;
                match *outcome {
                    Ok(outcome) => self.outcome(outcome, now),
                    Err(Failure { text, gone: Some(id) }) => {
                        let card = self.cards.iter().find(|open| open.card.id == id).cloned();
                        self.cards.retain(|open| open.card.id != id);
                        self.refocus(now);
                        if !self.answered_elsewhere(&id) {
                            match card {
                                Some(open) => {
                                    let said = format!("{} — ", words::DECIDED);
                                    let text = text.strip_prefix(&said).unwrap_or(&text);
                                    self.decided_notice(&open.card, text);
                                }
                                None => self.notice(text, Tone::Decided),
                            }
                        }
                    }
                    Err(Failure { text, gone: None }) => self.notice(text, Tone::Problem),
                }
            }
            Update::CommandDone { serial, result } if self.agents_pane.takes(serial) => {
                self.agents_pane.done(result);
            }
            Update::CommandDone { serial, result } => {
                if let Some(output) = &mut self.output
                    && output.serial == serial
                {
                    output.result = Some(result);
                }
            }
            Update::Error(error) => self.notice(error, Tone::Problem),
            #[cfg(feature = "voice")]
            Update::Voice(line) => self.voice = Some(line),
            Update::Notice(text) => self.notice_until_key(text, Tone::Info),
        }
    }

    fn frame(&mut self, frame: FeedFrame, now: Instant) {
        match frame {
            FeedFrame::Entry { entry } => {
                if self.entries.last().is_none_or(|last| entry.seq > last.seq) {
                    self.entries.push(entry);
                }
            }
            FeedFrame::Decisions { cards } => {
                let focused =
                    self.cards.first().map(|open| (open.card.id.clone(), open.card.version));
                let mut open = Vec::with_capacity(cards.len());
                for card in cards {
                    let kept = self.cards.iter().find(|open| open.card.id == card.id);
                    let (shown, details) = match kept {
                        // A card that changed is new to the person: it arms again.
                        Some(kept) if kept.card.version == card.version => {
                            (kept.shown, kept.details)
                        }
                        Some(kept) => (now, kept.details),
                        None => (now, false),
                    };
                    open.push(OpenCard { card, shown, details });
                }
                self.cards = open;
                self.refocus(now);
                if self.cards.is_empty() {
                    self.typing = false;
                }
                // The box the keys answered closed, and not by a key here: its keys do nothing
                // for a moment, so a key meant for it never becomes a message.
                if let Some((id, version)) = focused
                    && !self.cards.iter().any(|open| open.card.id == id)
                    && self.answering.as_ref().is_none_or(|(answering, _)| *answering != id)
                {
                    self.closed = Some((id, version, now));
                    self.closed_said = false;
                }
                // An edit or a reason being typed for a decision answered somewhere else.
                if let Input::EditArgs { id, .. } | Input::DenyReason { id, .. } = &self.input
                    && !self.cards.iter().any(|open| open.card.id == *id)
                {
                    let id = id.clone();
                    self.answered_elsewhere(&id);
                }
            }
            FeedFrame::Reset { .. } => {
                self.entries.clear();
                self.scroll = 0;
            }
        }
    }

    fn outcome(&mut self, outcome: CommandOutcome, now: Instant) {
        match outcome {
            CommandOutcome::Answered { card } => {
                if card.decided.is_some() {
                    self.cards.retain(|open| open.card.id != card.id);
                    self.refocus(now);
                }
                if let Some(decided) = card.decided.clone() {
                    self.decided_notice(&card, &decided.text);
                }
            }
            CommandOutcome::AlreadyDecided { decided, card } => {
                self.cards.retain(|open| open.card.id != card.id);
                self.refocus(now);
                // A source that no longer lists the decision cannot say how it was decided.
                let text = if decided.text == words::DECIDED {
                    "answered somewhere else.".to_string()
                } else {
                    decided.text
                };
                self.decided_notice(&card, &text);
            }
            CommandOutcome::Changed { card } => {
                if let Some(open) = self.cards.iter_mut().find(|open| open.card.id == card.id) {
                    open.card = card;
                    open.shown = now;
                }
                self.notice(
                    "This decision changed since you saw it. Look again before you answer.",
                    Tone::Problem,
                );
            }
            // The queue is drawn above the message box, from the engine.
            CommandOutcome::Queued { .. } => {}
            CommandOutcome::Stopped { text } => self.notice(text, Tone::Problem),
            CommandOutcome::Resumed => self.notice(words::RESUME_ALL, Tone::Info),
            CommandOutcome::ModeSet { mode } => self.notice(mode.status, Tone::Info),
            CommandOutcome::Pending { text, .. } => self.notice(text, Tone::Info),
            CommandOutcome::Sent { .. } | CommandOutcome::AnswerCancelled { .. } => {}
            // Changes to agents made through the engine (spec 3.3 §5.7); the Agents view
            // makes its own through `/agent` and says what each did.
            CommandOutcome::AgentAdded { agent } => self.notice(
                format!("You added {}.", super::safe::safe_line(&agent.draft.display_name)),
                Tone::Info,
            ),
            CommandOutcome::AgentChanged { agent } => self.notice(
                format!("Saved {}.", super::safe::safe_line(&agent.draft.display_name)),
                Tone::Info,
            ),
            CommandOutcome::AgentPaused { agent } => self.notice(
                crate::agents::words::is_paused(&super::safe::safe_line(&agent.draft.display_name)),
                Tone::Info,
            ),
            CommandOutcome::AgentResumed { agent } => self.notice(
                format!("{} runs again.", super::safe::safe_line(&agent.draft.display_name)),
                Tone::Info,
            ),
            CommandOutcome::AgentRemoved { ended } => self.notice(ended.text, Tone::Info),
            CommandOutcome::AgentOutOfDate { text, .. } => self.notice(text, Tone::Problem),
            CommandOutcome::RuleCopied { label, .. } => self.notice(label, Tone::Info),
        }
    }

    // -----------------------------------------------------------------------
    // Keys
    // -----------------------------------------------------------------------

    /// Pasted text goes into the message box, never to a decision.
    pub(crate) fn paste(&mut self, text: &str, now: Instant) {
        self.now = now;
        self.last_key = Some(now);
        self.type_held();
        self.composer.insert_str(text);
        self.last_typed = Some(now);
        self.completion_selected = 0;
    }

    /// When a held decision key answers, if no other key comes first.
    pub(crate) fn held_until(&self) -> Option<Instant> {
        self.held.as_ref().map(|(_, _, at)| *at + KEY_BURST)
    }

    /// A held decision key that no other key followed at once: it answers now, if its box
    /// still takes keys.
    pub(crate) fn release_held(&mut self, now: Instant) -> Vec<Effect> {
        self.now = now;
        match self.held.take() {
            Some((c, _, _)) if self.keys_answer(now) => self.decide_by_key(c, now),
            Some((c, _, _)) => {
                self.composer.insert(c);
                self.last_typed = Some(now);
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// A held decision key with more keys right behind it was typing: it goes into the box.
    fn type_held(&mut self) {
        if let Some((c, _, _)) = self.held.take() {
            self.composer.insert(c);
        }
    }

    pub(crate) fn key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.now = now;
        self.last_key = Some(now);
        self.type_held();
        let before = (self.composer.edits, self.input.clone());
        let effects = self.handle_key(key, now);
        // Any change to the message box disarms the decision keys for a moment (S-I4).
        if (self.composer.edits, &self.input) != (before.0, &before.1) {
            self.last_typed = Some(now);
            self.completion_selected = 0;
        }
        effects
    }

    fn handle_key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if !matches!(key.code, KeyCode::Char(_)) || ctrl {
            self.pending = None;
        }
        // Esc and the next key in one read arrive as Alt and that key (tmux, screen, a slow
        // link): Esc, then the key typed, never a decision key (§7.4 rules 1 and 2).
        if let (KeyCode::Char(c), false) = (key.code, ctrl)
            && key.modifiers.contains(KeyModifiers::ALT)
        {
            self.escape(now);
            self.pending = None;
            self.composer.insert(c);
            return Vec::new();
        }
        // The Agents view takes its own keys; none of them answers a decision (spec 3.3 §5.7).
        if self.view == View::Agents && !ctrl {
            let roster = self.agents.clone();
            if let Some(asks) = self.agents_pane.key(key, &roster) {
                return self.agents_asks(asks, now);
            }
        }
        // While a command's name is typed, Tab completes it, the arrows choose among the
        // commands it may be, and Esc clears it (spec 3.3 §5.2).
        if !ctrl && let Some(completion) = self.completion() {
            match key.code {
                KeyCode::Tab => {
                    if let Some(text) = completion.accept() {
                        self.composer.set(text);
                    }
                    return Vec::new();
                }
                KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    let mut moved = completion;
                    moved.step(if key.code == KeyCode::Down { 1 } else { -1 });
                    self.completion_selected = moved.selected;
                    return Vec::new();
                }
                KeyCode::Esc => {
                    self.composer.take();
                    return Vec::new();
                }
                _ => {}
            }
        }
        match (key.code, ctrl) {
            (KeyCode::Char('c'), true) => {
                if self.composer.is_empty() && self.input == Input::Message {
                    self.quit = true;
                    return vec![Effect::Quit];
                }
                self.composer.take();
                self.input = Input::Message;
                Vec::new()
            }
            (KeyCode::Char('d'), true) => {
                if self.composer.is_empty() && self.input == Input::Message {
                    self.quit = true;
                    return vec![Effect::Quit];
                }
                self.composer.delete();
                Vec::new()
            }
            // Stopping is one action from any surface (S-I10).
            (KeyCode::Char('k'), true) => vec![Effect::Execute(Command::StopAll { reason: None })],
            (KeyCode::Char('t'), true) => {
                self.toggle_view(View::Today, now);
                if self.view == View::Today { vec![Effect::Briefing] } else { Vec::new() }
            }
            (KeyCode::Char('r'), true) => {
                self.toggle_view(View::Permissions, now);
                Vec::new()
            }
            (KeyCode::Char('g'), true) => self.toggle_agents(now),
            (KeyCode::Char('u'), true) => {
                self.composer.take();
                Vec::new()
            }
            (KeyCode::Char('w'), true) => {
                self.composer.delete_word();
                Vec::new()
            }
            (KeyCode::Char(_), true) => Vec::new(),
            // An edit or a reason belongs to its decision: Tab never carries it to another
            // agent's thread.
            (KeyCode::Tab | KeyCode::BackTab, _) if self.input != Input::Message => Vec::new(),
            // Nor does a command being typed, its arguments included (spec 3.3 §5.2).
            (KeyCode::Tab | KeyCode::BackTab, _) if self.composer.text.starts_with('/') => {
                Vec::new()
            }
            (KeyCode::Tab, _) => self.next_agent(1),
            (KeyCode::BackTab, _) => self.next_agent(-1),
            // Help, Today and a command's output scroll by a page.
            (KeyCode::PageUp, _) if self.view != View::Thread => {
                self.view_scroll = self.view_scroll.saturating_sub(self.view_page.max(1));
                Vec::new()
            }
            (KeyCode::PageDown, _) if self.view != View::Thread => {
                self.view_scroll = self.view_scroll.saturating_add(self.view_page.max(1));
                Vec::new()
            }
            // Within a box whose call shows a window at a time, the keys move the window by
            // its own height, so no row of the call is ever skipped (§7.1).
            (KeyCode::PageUp, _) => {
                if self.scroll == 0 && self.box_scroll > 0 {
                    self.box_scroll = self.box_scroll.saturating_sub(self.box_window.max(1));
                } else {
                    self.scroll = self.scroll.saturating_add(PAGE);
                }
                Vec::new()
            }
            (KeyCode::PageDown, _) => {
                if self.scroll > 0 {
                    self.scroll = self.scroll.saturating_sub(PAGE);
                } else if self.box_scrolls {
                    self.box_scroll = self.box_scroll.saturating_add(self.box_window.max(1));
                }
                Vec::new()
            }
            // The newest message queued for the shown thread comes back into the empty message
            // box, before it is sent: to change it, or to clear it with Ctrl-U.
            (KeyCode::Up, _)
                if self.composer.is_empty()
                    && self.input == Input::Message
                    && !self.queued.is_empty() =>
            {
                vec![Effect::Unqueue(self.session.clone())]
            }
            (KeyCode::Esc, _) => {
                self.escape(now);
                Vec::new()
            }
            (KeyCode::Enter, _) => self.enter(now),
            (KeyCode::Backspace, _) => {
                self.composer.backspace();
                Vec::new()
            }
            (KeyCode::Delete, _) => {
                self.composer.delete();
                Vec::new()
            }
            (KeyCode::Left, _) => {
                self.composer.cursor = self.composer.cursor.saturating_sub(1);
                Vec::new()
            }
            (KeyCode::Right, _) => {
                self.composer.cursor = (self.composer.cursor + 1).min(self.composer.len());
                Vec::new()
            }
            (KeyCode::Home, _) => {
                self.composer.cursor = 0;
                Vec::new()
            }
            (KeyCode::End, _) => {
                self.composer.cursor = self.composer.len();
                Vec::new()
            }
            // `c` cancels a spoken "allow once" counting down, from an empty message box only
            // (spec 3.3 §5.6, S-I4).
            #[cfg(feature = "voice")]
            (KeyCode::Char('c'), false)
                if self.composer.is_empty()
                    && self.input == Input::Message
                    && self.voice.as_ref().is_some_and(|voice| voice.pending.is_some()) =>
            {
                let pending = self.voice.as_ref().and_then(|voice| voice.pending.clone());
                pending.map_or_else(Vec::new, |(decision_id, _)| {
                    vec![Effect::Execute(Command::CancelAnswer { decision_id })]
                })
            }
            (KeyCode::Char(c), false) => self.char(c, key, now),
            _ => Vec::new(),
        }
    }

    /// Shows `view`; the thread coming back into view arms its decision again, so a key
    /// pressed as it reappears never answers a box the person has not seen (§7.4 rule 2).
    fn set_view(&mut self, view: View, now: Instant) {
        if view == View::Thread && self.view != View::Thread {
            self.rearm(now);
        }
        if view != self.view {
            self.view_scroll = 0;
        }
        self.view = view;
        self.pending = None;
    }

    fn toggle_view(&mut self, view: View, now: Instant) {
        self.set_view(if self.view == view { View::Thread } else { view }, now);
    }

    /// Esc never decides (§7.4 rule 1): it cancels an edit or a reason, closes a view or the
    /// details, and otherwise moves between answering by key and typing.
    fn escape(&mut self, now: Instant) {
        if matches!(self.input, Input::EditArgs { .. } | Input::DenyReason { .. }) {
            self.input = Input::Message;
            self.composer.take();
        } else if self.view != View::Thread {
            self.set_view(View::Thread, now);
        } else if self.cards.first().is_some_and(|open| open.details) {
            if let Some(open) = self.cards.first_mut() {
                open.details = false;
            }
        } else if !self.cards.is_empty() && self.composer.is_empty() {
            self.typing = !self.typing;
        }
    }

    /// Moves through the roster's agents: each one's thread, or a new one for an agent the
    /// person added that has none yet (spec 3.3 §5.7).
    fn next_agent(&mut self, step: isize) -> Vec<Effect> {
        let reachable: Vec<&AgentRow> =
            self.agents.iter().filter(|row| row.session_id.is_some() || row.added).collect();
        if reachable.is_empty() {
            return Vec::new();
        }
        let fresh = self.fresh_agent();
        let at = reachable.iter().position(|row| {
            row.session_id.as_deref() == Some(self.session.as_str())
                || Some(row.agent.name.as_str()) == fresh
        });
        let len = reachable.len() as isize;
        let next = match at {
            Some(at) => (at as isize + step).rem_euclid(len),
            None => 0,
        };
        let row = reachable[next as usize];
        match row.session_id.clone() {
            Some(session) => self.switch_session(session),
            None => {
                let agent = row.agent.name.clone();
                self.open_thread_with(agent)
            }
        }
    }

    /// What a Permissions key does, once pressed twice: the mode, or Resume all while stopped.
    fn permission_command(&self, c: char) -> Option<Command> {
        let mode = match c {
            '1' => Some(Mode::JustLook),
            '2' => Some(Mode::Suggest),
            '3' => Some(Mode::AskFirst),
            '4' => Some(Mode::GoAhead),
            _ => None,
        };
        if let Some(mode) = mode {
            return Some(Command::SetMode { session_id: self.session.clone(), mode });
        }
        let stopped = self.status.as_ref().is_some_and(|status| status.stopped);
        (c == 'r' && stopped).then_some(Command::ResumeAll)
    }

    /// The Permissions view's keys act only when pressed twice, so the first letter of a
    /// message typed there never resumes every agent or changes the mode; a different key
    /// after one makes both typing.
    fn permissions_key(&mut self, c: char, now: Instant) -> Option<Vec<Effect>> {
        if let Some((first, at)) = self.pending.take() {
            if first == c && now.saturating_duration_since(at) <= CONFIRM_WINDOW {
                return self.permission_command(c).map(|command| vec![Effect::Execute(command)]);
            }
            self.composer.insert(first);
            return None;
        }
        if self.composer.is_empty() && self.permission_command(c).is_some() {
            self.pending = Some((c, now));
            return Some(Vec::new());
        }
        None
    }

    /// The Permissions key waiting for its second press, as the message box shows it.
    pub(crate) fn pending_permission(&self, now: Instant) -> Option<String> {
        let (c, at) = self.pending?;
        if now.saturating_duration_since(at) > CONFIRM_WINDOW {
            return None;
        }
        let what = match c {
            '1' => words::JUST_LOOK,
            '2' => words::SUGGEST,
            '3' => words::ASK_FIRST,
            '4' => words::GO_AHEAD,
            _ => words::RESUME_ALL,
        };
        Some(format!("Press {c} again: {what}"))
    }

    fn char(&mut self, c: char, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if self.view == View::Permissions
            && let Some(effects) = self.permissions_key(c, now)
        {
            return effects;
        }
        if key.kind == KeyEventKind::Press && matches!(c, 'y' | 'a' | 'e' | 'n' | '?') {
            if self.keys_answer(now) {
                if self.burst_guard {
                    self.held = Some((c, key, now));
                    return Vec::new();
                }
                return self.decide_by_key(c, now);
            }
            // A key repeated while its answer is on its way answers nothing and types nothing.
            if self.composer.is_empty() && self.awaits_answer(now) {
                return Vec::new();
            }
            if let Some(effects) = self.just_closed(c, now) {
                return effects;
            }
            // A box waits for its keys: one not on screen, or not lit yet, never takes them
            // as text either (§7.4 rule 2, S-I4).
            if self.box_waits(now) {
                let why = if self.box_hidden {
                    "The decision is not on screen: PageDown to it, then answer."
                } else {
                    "The decision just appeared: answer once its keys light up, or Esc to type \
                     a message."
                };
                self.notice(why, Tone::Info);
                return Vec::new();
            }
        }
        self.composer.insert(c);
        Vec::new()
    }

    /// The focused decision's box is in the thread, the message box is empty and the person is
    /// not typing: a decision key is meant for the box, whether or not it answers yet.
    fn box_waits(&self, now: Instant) -> bool {
        !self.cards.is_empty()
            && self.input == Input::Message
            && self.thread_shown()
            && self.composer.is_empty()
            && !self.typing
            && self.last_typed.is_none_or(|at| now.saturating_duration_since(at) >= ARM_DELAY)
    }

    /// The newest queued message, taken back before it was sent: it is in the message box.
    pub(crate) fn took_back(&mut self, text: Option<String>) {
        match text {
            Some(text) => {
                if let Some(at) = self.queued.iter().rposition(|queued| *queued == text) {
                    self.queued.remove(at);
                }
                self.composer.set(text);
                self.last_typed = Some(self.now);
                self.notice("Taken back from the queue; Enter queues it again.", Tone::Info);
            }
            None => self.notice("It was sent already.", Tone::Info),
        }
    }

    /// A decision key pressed just after the box it was meant for was answered somewhere else
    /// never becomes a message: `n` opens a reason that is never sent, and the others do
    /// nothing; the terminal says so once.
    fn just_closed(&mut self, c: char, now: Instant) -> Option<Vec<Effect>> {
        let (id, version, at) = self.closed.clone()?;
        if now.saturating_duration_since(at) >= CLOSE_HOLD
            || !self.composer.is_empty()
            || self.input != Input::Message
            || !self.thread_shown()
            || self.typing
        {
            return None;
        }
        if !self.closed_said {
            // A decision also closes with no answer: cancelled, expired, or its agent removed
            // (S-I16); only an answer is said to be one.
            let text = match self.decided.iter().find(|line| line.id == id) {
                Some(line) if line.answer == DecidedAnswer::Ended => {
                    format!("{}.", words::ENDED_WITHOUT_AN_ANSWER)
                }
                Some(line) => format!(
                    "Already answered {}.",
                    line.place.as_deref().unwrap_or("somewhere else")
                ),
                None => "No longer waiting for you.".to_string(),
            };
            self.notice(text, Tone::Decided);
        }
        // `n` opens a reason as it would have, so the reason typed next goes nowhere instead
        // of into a message; Enter then says it was not sent.
        if c == 'n' {
            self.input = Input::DenyReason { id, version };
        }
        self.closed_said = true;
        Some(Vec::new())
    }

    /// A decision key on the focused decision (spec 3.3 §5.2, §7.1, §7.2).
    fn decide_by_key(&mut self, c: char, now: Instant) -> Vec<Effect> {
        let Some(open) = self.cards.first_mut() else {
            return Vec::new();
        };
        if c == '?' {
            open.details = !open.details;
            return Vec::new();
        }
        let answer = match c {
            'y' => Answer::AllowOnce,
            'a' => Answer::AlwaysAllow,
            'e' => Answer::Edit,
            _ => Answer::Deny,
        };
        // The box cuts a long call short: only `?` shows all of it, and only a call shown in
        // full is answered other than by Deny (§7.1).
        let cut_short = needs_details(open);
        if cut_short && !open.details && answer != Answer::Deny {
            self.notice("Press ? to see the whole call before you answer.", Tone::Problem);
            return Vec::new();
        }
        // The call shows a window at a time: every row of it is read before it is allowed.
        if self.box_unread && answer != Answer::Deny {
            let how = if self.box_scrolls {
                "PageDown to read the whole call before you answer."
            } else {
                "Press ? to see the whole call before you answer."
            };
            self.notice(how, Tone::Problem);
            return Vec::new();
        }
        let card = &open.card;
        let Some(option) = card.allowed.option(answer) else {
            let note = card.allowed.note.clone().unwrap_or_else(|| {
                if answer == Answer::AlwaysAllow {
                    "Always allow is not offered in the terminal for this decision; the home app \
                     offers it where it can."
                        .to_string()
                } else {
                    "That answer is not available in the terminal for this decision.".to_string()
                }
            });
            self.notice(note, Tone::Problem);
            return Vec::new();
        };
        // A call shown cut short is approved only once shown in full (§7.1).
        if option.needs_expanded && !open.details {
            self.notice("Press ? to see the whole call before you answer.", Tone::Problem);
            return Vec::new();
        }
        let (id, version) = (card.id.clone(), card.version);
        let expanded = open.details || !cut_short;
        match answer {
            Answer::Edit => {
                let args = serde_json::to_string(&card.call.args).unwrap_or_default();
                self.input = Input::EditArgs { id, version };
                self.composer.set(args);
                Vec::new()
            }
            Answer::Deny => {
                self.input = Input::DenyReason { id, version };
                self.composer.take();
                Vec::new()
            }
            Answer::AllowOnce | Answer::AlwaysAllow => {
                self.answering = Some((id.clone(), now));
                vec![Effect::Execute(Command::Decide {
                    decision_id: id,
                    version,
                    answer,
                    edited_args: None,
                    reason: None,
                    expanded,
                })]
            }
        }
    }

    fn enter(&mut self, now: Instant) -> Vec<Effect> {
        // What Enter changes in the message box disarms the keys (`key`); Enter on an empty
        // box changes nothing, so a habitual Enter after a decision key never makes the next
        // key text.
        match self.input.clone() {
            Input::EditArgs { id, version } => {
                if !self.cards.iter().any(|open| open.card.id == id) {
                    self.answered_elsewhere(&id);
                    return Vec::new();
                }
                let edited: Value = match serde_json::from_str(&self.composer.text) {
                    Ok(value) => value,
                    Err(error) => {
                        self.notice(
                            format!("The arguments are not JSON: {error}. Fix them, or Esc."),
                            Tone::Problem,
                        );
                        return Vec::new();
                    }
                };
                self.answering = Some((id.clone(), now));
                let command = Command::Decide {
                    decision_id: id,
                    version,
                    answer: Answer::Edit,
                    edited_args: Some(edited),
                    reason: None,
                    expanded: true,
                };
                self.input = Input::Message;
                self.composer.take();
                vec![Effect::Execute(command)]
            }
            Input::DenyReason { id, version } => {
                let Some(open) = self.cards.iter().find(|open| open.card.id == id) else {
                    self.answered_elsewhere(&id);
                    return Vec::new();
                };
                let expanded = open.details || !needs_details(open);
                let reason = self.composer.take().trim().to_string();
                self.answering = Some((id.clone(), now));
                let command = Command::Decide {
                    decision_id: id,
                    version,
                    answer: Answer::Deny,
                    edited_args: None,
                    reason: (!reason.is_empty()).then_some(reason),
                    expanded,
                };
                self.input = Input::Message;
                vec![Effect::Execute(command)]
            }
            Input::Message => {
                // Enter on the start of a name runs the command the completion chose.
                if let Some(completion) = self.completion()
                    && !completion.whole()
                    && let Some(chosen) = completion.chosen()
                {
                    let chosen = chosen.to_string();
                    self.composer.take();
                    return self.slash(&chosen, now);
                }
                let text = self.composer.take();
                let text = text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                if let Some(command) = text.strip_prefix('/') {
                    return self.slash(command, now);
                }
                self.scroll = 0;
                // The next message clears what the terminal said before it.
                self.notices_from = self.notice_count;
                if let Some(agent) = self.fresh_agent().map(str::to_string) {
                    self.fresh = None;
                    return vec![Effect::Execute(Command::NewThread {
                        agent,
                        session_id: self.session.clone(),
                        text: text.to_string(),
                    })];
                }
                vec![Effect::Execute(Command::Send {
                    session_id: self.session.clone(),
                    text: text.to_string(),
                })]
            }
        }
    }

    /// `/` commands: the terminal's own, then those its embedder added (spec 3.3 §5.2).
    fn slash(&mut self, line: &str, now: Instant) -> Vec<Effect> {
        let (name, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match name {
            "quit" | "exit" => {
                self.quit = true;
                vec![Effect::Quit]
            }
            "stop" => vec![Effect::Execute(Command::StopAll { reason: None })],
            "resume" => vec![Effect::Execute(Command::ResumeAll)],
            "today" => {
                self.set_view(View::Today, now);
                vec![Effect::Briefing]
            }
            "permissions" => {
                self.set_view(View::Permissions, now);
                Vec::new()
            }
            "agents" => {
                self.set_view(View::Agents, now);
                let asks = self.agents_pane.open();
                self.agents_asks(asks, now)
            }
            "help" => {
                self.set_view(View::Help, now);
                Vec::new()
            }
            // The bare `/mode` and `/session` show what the embedder's command of that name
            // shows, as the REPL's do.
            "mode" | "session" if rest.is_empty() && self.commands.shadowed(name).is_some() => {
                self.run_command(name, rest, now)
            }
            "mode" => match parse_mode(rest) {
                Some(mode) => vec![Effect::Execute(Command::SetMode {
                    session_id: self.session.clone(),
                    mode,
                })],
                None => {
                    self.notice(
                        format!(
                            "/mode takes {}, {}, {} or {}.",
                            words::JUST_LOOK,
                            words::SUGGEST,
                            words::ASK_FIRST,
                            words::GO_AHEAD
                        ),
                        Tone::Problem,
                    );
                    Vec::new()
                }
            },
            "session" if rest.is_empty() => {
                self.notice(
                    format!(
                        "This session is {}. /session new opens another; /session ID opens one.",
                        self.session
                    ),
                    Tone::Info,
                );
                Vec::new()
            }
            "session" => {
                let session = (rest != "new").then(|| rest.to_string());
                vec![Effect::OpenSession(session)]
            }
            other if self.commands.find(other).is_some() => self.run_command(other, rest, now),
            other => {
                self.notice(format!("No command /{other}. /help lists them."), Tone::Problem);
                Vec::new()
            }
        }
    }

    /// Opens or closes the Agents view (spec 3.3 §5.7).
    fn toggle_agents(&mut self, now: Instant) -> Vec<Effect> {
        if self.view == View::Agents {
            self.set_view(View::Thread, now);
            return Vec::new();
        }
        self.set_view(View::Agents, now);
        let asks = self.agents_pane.open();
        self.agents_asks(asks, now)
    }

    /// What the Agents view asked for: `/agent` runs, a thread opened, or back to the thread.
    fn agents_asks(&mut self, asks: Vec<super::agents::Ask>, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        for ask in asks {
            match ask {
                super::agents::Ask::Run { args, purpose } => {
                    self.runs += 1;
                    self.agents_pane.sent(self.runs, purpose);
                    effects.push(Effect::RunCommand {
                        serial: self.runs,
                        name: super::agents::AGENT_COMMAND.to_string(),
                        args,
                    });
                }
                super::agents::Ask::Describe { text } => {
                    self.set_view(View::Thread, now);
                    self.input = Input::Message;
                    self.composer.set(text);
                    effects.push(Effect::OpenSession(None));
                }
                super::agents::Ask::Open { session } => {
                    self.set_view(View::Thread, now);
                    effects.extend(self.switch_session(session));
                }
                super::agents::Ask::Start { agent } => {
                    self.set_view(View::Thread, now);
                    effects.extend(self.open_thread_with(agent));
                }
                super::agents::Ask::Close => self.set_view(View::Thread, now),
            }
        }
        effects
    }

    /// Runs the embedder's command `name` with `args`; the Output view shows it running, then
    /// what it returned.
    fn run_command(&mut self, name: &str, args: &str, now: Instant) -> Vec<Effect> {
        self.runs += 1;
        let line = if args.is_empty() { format!("/{name}") } else { format!("/{name} {args}") };
        self.output = Some(CommandOutput { serial: self.runs, line, result: None });
        self.set_view(View::Output, now);
        vec![Effect::RunCommand {
            serial: self.runs,
            name: name.to_string(),
            args: args.to_string(),
        }]
    }
}

/// A mode from its §8.1 words, or the harness's own (`read-only`, `plan`, `ask`, `auto`), in
/// any case.
fn parse_mode(text: &str) -> Option<Mode> {
    let text = text.to_lowercase();
    [
        (words::JUST_LOOK, Mode::JustLook),
        (words::SUGGEST, Mode::Suggest),
        (words::ASK_FIRST, Mode::AskFirst),
        (words::GO_AHEAD, Mode::GoAhead),
        ("read-only", Mode::JustLook),
        ("plan", Mode::Suggest),
        ("ask", Mode::AskFirst),
        ("auto", Mode::GoAhead),
    ]
    .into_iter()
    .find(|(word, _)| word.to_lowercase() == text)
    .map(|(_, mode)| mode)
}

/// The decisions a feed entry names.
pub(crate) fn named_decisions(entry: &FeedEntry) -> impl Iterator<Item = &str> {
    entry.items.iter().filter_map(|item| match item {
        ThreadItem::Decision { decision_id: Some(id), .. } => Some(id.as_str()),
        _ => None,
    })
}
