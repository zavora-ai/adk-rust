//! What the terminal shows and what each key does (spec 3.3 §5.2, §7.4): the state both the
//! full-screen and the inline UI draw, rebuilt from the engine on every update.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::Value;

use super::view::needs_details;
use crate::engine::{Command, CommandOutcome, FeedFrame};
use crate::model::{AgentRow, Briefing, DecisionCard, FeedEntry, Mode, StatusLine};
use crate::rules::Answer;
use crate::words;

/// How long a decision box is on screen, and how long after the last keystroke into the
/// message box, before its keys answer it: a key pressed for a message as a box appears types
/// into the message instead (spec 3.3 §7.4 rule 2, S-I4).
pub(crate) const ARM_DELAY: Duration = Duration::from_millis(700);

/// How many notices the thread keeps.
const NOTICES: usize = 4;

/// How long the keys wait for an answer's outcome before they answer again.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

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
    Frame {
        session: String,
        frame: FeedFrame,
    },
    Briefing(Briefing),
    Queued {
        session: String,
        lines: Vec<String>,
    },
    /// Boxed: a command's outcome may carry a whole decision card.
    Outcome(Box<Result<CommandOutcome, Failure>>),
    Error(String),
}

/// What a key asks the runner to do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect {
    Execute(Command),
    Session(String),
    /// Read "While you were away" again now: Today was opened.
    Briefing,
    Quit,
}

/// The right pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Thread,
    Today,
    Permissions,
    Help,
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
    }

    fn insert_str(&mut self, text: &str) {
        for c in text.chars() {
            self.insert(if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c });
        }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let at = self.byte(self.cursor);
            self.text.remove(at);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = self.byte(self.cursor);
            self.text.remove(at);
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
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
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

/// A line the terminal adds under the thread: an answer, a stop, a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub text: String,
    pub tone: Tone,
}

/// The terminal's state.
#[derive(Debug)]
pub(crate) struct App {
    pub session: String,
    pub status: Option<StatusLine>,
    pub agents: Vec<AgentRow>,
    pub entries: Vec<FeedEntry>,
    pub cards: Vec<OpenCard>,
    pub briefing: Option<Briefing>,
    pub queued: Vec<String>,
    pub notices: Vec<Notice>,
    /// Every notice ever added, for the inline UI to print each once.
    pub notice_count: usize,
    pub composer: Composer,
    pub input: Input,
    /// Esc moved the keys to the message box while a decision is open.
    pub typing: bool,
    pub view: View,
    /// Lines scrolled up from the bottom of the thread.
    pub scroll: usize,
    pub model: Option<String>,
    pub place: Option<String>,
    pub app_name: String,
    /// The last keystroke into the message box.
    last_typed: Option<Instant>,
    /// The decision the keys act on, and since when: a decision that takes the focus arms
    /// again, so a repeated key never answers one the person did not have in front of them.
    focus: Option<(String, Instant)>,
    /// A decision answered by key whose outcome has not come back yet, and since when.
    answering: Option<(String, Instant)>,
    /// The focused decision's box is not wholly on screen: its keys do not answer it (§7.1).
    pub box_hidden: bool,
    pub quit: bool,
}

impl App {
    pub(crate) fn new(app_name: &str, session: String) -> Self {
        Self {
            session,
            status: None,
            agents: Vec::new(),
            entries: Vec::new(),
            cards: Vec::new(),
            briefing: None,
            queued: Vec::new(),
            notices: Vec::new(),
            notice_count: 0,
            composer: Composer::default(),
            input: Input::Message,
            typing: false,
            view: View::Thread,
            scroll: 0,
            model: None,
            place: None,
            app_name: app_name.to_string(),
            last_typed: None,
            focus: None,
            answering: None,
            box_hidden: false,
            quit: false,
        }
    }

    pub(crate) fn notice(&mut self, text: impl Into<String>, tone: Tone) {
        self.notices.push(Notice { text: text.into(), tone });
        self.notice_count += 1;
        if self.notices.len() > NOTICES {
            self.notices.remove(0);
        }
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
            && self.view == View::Thread
            && self.composer.is_empty()
            && !self.typing
            && !self.box_hidden
            && !self.awaits_answer(now)
            && now.saturating_duration_since(focused) >= ARM_DELAY
            && self.last_typed.is_none_or(|at| now.saturating_duration_since(at) >= ARM_DELAY)
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
        }
    }

    /// Whether the focused box was wholly on screen in the frame just drawn; one that comes
    /// back into view arms again.
    pub(crate) fn set_box_shown(&mut self, shown: bool, now: Instant) {
        if self.box_hidden
            && shown
            && let Some((_, since)) = &mut self.focus
        {
            *since = now;
        }
        self.box_hidden = !shown;
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
        self.agents.iter().find(|row| row.session_id.as_deref() == Some(self.session.as_str()))
    }

    fn switch_session(&mut self, session: String) -> Vec<Effect> {
        if session == self.session {
            return Vec::new();
        }
        self.session = session.clone();
        self.entries.clear();
        self.cards.clear();
        self.queued.clear();
        self.scroll = 0;
        self.input = Input::Message;
        self.typing = false;
        self.focus = None;
        self.answering = None;
        vec![Effect::Session(session)]
    }

    // -----------------------------------------------------------------------
    // Updates from the engine
    // -----------------------------------------------------------------------

    pub(crate) fn update(&mut self, update: Update, now: Instant) {
        match update {
            Update::Status(status) => self.status = Some(status),
            Update::Agents(agents) => self.agents = agents,
            Update::Briefing(briefing) => self.briefing = Some(briefing),
            Update::Queued { session, lines } if session == self.session => self.queued = lines,
            Update::Queued { .. } => {}
            Update::Frame { session, frame } if session == self.session => self.frame(frame, now),
            Update::Frame { .. } => {}
            Update::Outcome(outcome) => {
                self.answering = None;
                match *outcome {
                    Ok(outcome) => self.outcome(outcome, now),
                    Err(Failure { text, gone: Some(id) }) => {
                        self.cards.retain(|open| open.card.id != id);
                        self.refocus(now);
                        if !self.answered_elsewhere(&id) {
                            self.notice(text, Tone::Decided);
                        }
                    }
                    Err(Failure { text, gone: None }) => self.notice(text, Tone::Problem),
                }
            }
            Update::Error(error) => self.notice(error, Tone::Problem),
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
                let text = card.decided.as_ref().map(|decided| decided.text.clone());
                if card.decided.is_some() {
                    self.cards.retain(|open| open.card.id != card.id);
                    self.refocus(now);
                }
                if let Some(text) = text {
                    self.notice(text, Tone::Decided);
                }
            }
            CommandOutcome::AlreadyDecided { decided, card } => {
                self.cards.retain(|open| open.card.id != card.id);
                self.refocus(now);
                // A source that no longer lists the decision cannot say how it was decided.
                let text = if decided.text == words::DECIDED {
                    format!("{} — answered somewhere else.", words::DECIDED)
                } else {
                    format!("{} — {}", words::DECIDED, decided.text)
                };
                self.notice(text, Tone::Decided);
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
        }
    }

    // -----------------------------------------------------------------------
    // Keys
    // -----------------------------------------------------------------------

    /// Pasted text goes into the message box, never to a decision.
    pub(crate) fn paste(&mut self, text: &str, now: Instant) {
        self.composer.insert_str(text);
        self.last_typed = Some(now);
    }

    pub(crate) fn key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
                self.toggle_view(View::Today);
                if self.view == View::Today { vec![Effect::Briefing] } else { Vec::new() }
            }
            (KeyCode::Char('r'), true) => {
                self.toggle_view(View::Permissions);
                Vec::new()
            }
            (KeyCode::Char('u'), true) => {
                self.composer.take();
                self.last_typed = Some(now);
                Vec::new()
            }
            (KeyCode::Char('w'), true) => {
                self.composer.delete_word();
                self.last_typed = Some(now);
                Vec::new()
            }
            (KeyCode::Char(_), true) => Vec::new(),
            (KeyCode::Tab, _) => self.next_agent(1),
            (KeyCode::BackTab, _) => self.next_agent(-1),
            (KeyCode::PageUp, _) => {
                self.scroll = self.scroll.saturating_add(10);
                Vec::new()
            }
            (KeyCode::PageDown, _) => {
                self.scroll = self.scroll.saturating_sub(10);
                Vec::new()
            }
            (KeyCode::Esc, _) => {
                self.escape();
                Vec::new()
            }
            (KeyCode::Enter, _) => self.enter(now),
            (KeyCode::Backspace, _) => {
                self.composer.backspace();
                self.last_typed = Some(now);
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
            (KeyCode::Char(c), false) => self.char(c, key, now),
            _ => Vec::new(),
        }
    }

    fn toggle_view(&mut self, view: View) {
        self.view = if self.view == view { View::Thread } else { view };
    }

    /// Esc never decides (§7.4 rule 1): it cancels an edit or a reason, closes a view or the
    /// details, and otherwise moves between answering by key and typing.
    fn escape(&mut self) {
        if matches!(self.input, Input::EditArgs { .. } | Input::DenyReason { .. }) {
            self.input = Input::Message;
            self.composer.take();
        } else if self.view != View::Thread {
            self.view = View::Thread;
        } else if self.cards.first().is_some_and(|open| open.details) {
            if let Some(open) = self.cards.first_mut() {
                open.details = false;
            }
        } else if !self.cards.is_empty() && self.composer.is_empty() {
            self.typing = !self.typing;
        }
    }

    fn next_agent(&mut self, step: isize) -> Vec<Effect> {
        let sessions: Vec<String> =
            self.agents.iter().filter_map(|row| row.session_id.clone()).collect();
        if sessions.is_empty() {
            return Vec::new();
        }
        let at = sessions.iter().position(|session| *session == self.session);
        let len = sessions.len() as isize;
        let next = match at {
            Some(at) => (at as isize + step).rem_euclid(len),
            None => 0,
        };
        self.switch_session(sessions[next as usize].clone())
    }

    fn char(&mut self, c: char, key: KeyEvent, now: Instant) -> Vec<Effect> {
        if self.view == View::Permissions && self.composer.is_empty() {
            let mode = match c {
                '1' => Some(Mode::JustLook),
                '2' => Some(Mode::Suggest),
                '3' => Some(Mode::AskFirst),
                '4' => Some(Mode::GoAhead),
                _ => None,
            };
            if let Some(mode) = mode {
                return vec![Effect::Execute(Command::SetMode {
                    session_id: self.session.clone(),
                    mode,
                })];
            }
            if c == 'r' {
                return vec![Effect::Execute(Command::ResumeAll)];
            }
        }
        if key.kind == KeyEventKind::Press && matches!(c, 'y' | 'a' | 'e' | 'n' | '?') {
            if self.keys_answer(now) {
                return self.decide_by_key(c, now);
            }
            // A key repeated while its answer is on its way answers nothing and types nothing.
            if self.composer.is_empty() && self.awaits_answer(now) {
                return Vec::new();
            }
        }
        self.composer.insert(c);
        self.last_typed = Some(now);
        Vec::new()
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
        let card = &open.card;
        let Some(option) = card.allowed.option(answer) else {
            let note = card.allowed.note.clone().unwrap_or_else(|| {
                "That answer is not available in the terminal for this decision.".to_string()
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
        self.last_typed = Some(now);
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
                let text = self.composer.take();
                let text = text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                if let Some(command) = text.strip_prefix('/') {
                    return self.slash(command);
                }
                self.scroll = 0;
                vec![Effect::Execute(Command::Send {
                    session_id: self.session.clone(),
                    text: text.to_string(),
                })]
            }
        }
    }

    /// `/` commands.
    fn slash(&mut self, line: &str) -> Vec<Effect> {
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
                self.view = View::Today;
                vec![Effect::Briefing]
            }
            "permissions" => {
                self.view = View::Permissions;
                Vec::new()
            }
            "help" => {
                self.view = View::Help;
                Vec::new()
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
            "session" => {
                let session = if rest.is_empty() || rest == "new" {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    rest.to_string()
                };
                self.switch_session(session)
            }
            other => {
                self.notice(format!("No command /{other}. /help lists them."), Tone::Problem);
                Vec::new()
            }
        }
    }
}

/// A mode from its §8.1 words, in any case.
fn parse_mode(text: &str) -> Option<Mode> {
    let text = text.to_lowercase();
    [
        (words::JUST_LOOK, Mode::JustLook),
        (words::SUGGEST, Mode::Suggest),
        (words::ASK_FIRST, Mode::AskFirst),
        (words::GO_AHEAD, Mode::GoAhead),
    ]
    .into_iter()
    .find(|(word, _)| word.to_lowercase() == text)
    .map(|(_, mode)| mode)
}
