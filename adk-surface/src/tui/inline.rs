//! The inline terminal (spec 3.3 §5.2): the thread and its decision boxes printed into the
//! scrollback, with the message box on the last line.
//!
//! On a terminal, keys work as in the full-screen UI (raw mode and bracketed paste, without the
//! alternate screen), and the terminal is given back on exit, on a panic in the terminal's own
//! code and on a signal. With standard input piped, or on a terminal that names itself
//! `dumb`, each line is a message, a decision is never answered from it, nothing but plain
//! text is written, and the run ends as JSON mode does once input ends and the turn settles.

use std::collections::{HashMap, VecDeque};
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use crossterm::style::{
    Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, queue};
use futures::StreamExt;
use ratatui::backend::IntoCrossterm;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use super::app::{App, BoxView, Effect, Tone, Update, View};
use super::json::{End, Settler};
use super::pump::{Commands, Pump};
use super::safe::terminal_safe;
use super::term::{self, Guard, Signals};
use super::theme::{Appearance, Theme};
use super::view::{Fit, Renderer};
use super::{EXIT_DONE, TerminalSurface};

/// How often the prompt is redrawn while nothing happens, so a decision box arms on time.
const TICK: Duration = Duration::from_millis(250);

/// What the scrollback already shows.
pub(crate) struct Printer<W: Write> {
    out: W,
    renderer: Renderer,
    color: bool,
    raw: bool,
    session: Option<String>,
    printed_seq: u64,
    /// Each open decision as printed: its version, its details, and whether whole.
    printed_cards: HashMap<String, (u64, bool, bool)>,
    printed_notices: usize,
    printed_queued: Vec<String>,
    status: Option<String>,
    view: View,
    width: Option<usize>,
    height: Option<usize>,
    /// The message box as last drawn.
    prompt: Option<String>,
    /// What was printed of the box the keys answer, by decision.
    focused: Option<(String, BoxView)>,
    /// The slash command whose output was printed last.
    printed_output: Option<u64>,
    /// The Agents view as last printed (spec 3.3 §5.7): printed again when it changes.
    printed_agents: Option<super::agents::AgentsPane>,
    /// The voice line as last printed (spec 3.3 §5.6).
    #[cfg(feature = "voice")]
    printed_voice: super::voice::VoiceLine,
}

impl<W: Write> Printer<W> {
    pub(crate) fn new(out: W, theme: Theme, color: bool, raw: bool) -> Self {
        Self {
            out,
            renderer: Renderer::new(theme),
            color,
            raw,
            session: None,
            printed_seq: 0,
            printed_cards: HashMap::new(),
            printed_notices: 0,
            printed_queued: Vec::new(),
            status: None,
            view: View::Thread,
            width: None,
            height: None,
            prompt: None,
            focused: None,
            printed_output: None,
            printed_agents: None,
            #[cfg(feature = "voice")]
            printed_voice: super::voice::VoiceLine::default(),
        }
    }

    /// A fixed width instead of the terminal's.
    #[cfg(test)]
    pub(crate) fn with_width(mut self, width: usize) -> Self {
        self.width = Some(width);
        self
    }

    /// A fixed height instead of the terminal's.
    #[cfg(test)]
    pub(crate) fn with_height(mut self, height: usize) -> Self {
        self.height = Some(height);
        self
    }

    /// How many rows a decision box may take: the terminal's, less the message box and the
    /// line under the box.
    fn box_rows(&self) -> usize {
        self.height
            .unwrap_or_else(|| {
                crossterm::terminal::size().map_or(40, |(_, rows)| usize::from(rows))
            })
            .saturating_sub(2)
            .max(8)
    }

    fn width(&self) -> usize {
        self.width
            .unwrap_or_else(|| {
                crossterm::terminal::size().map_or(100, |(cols, _)| usize::from(cols))
            })
            .clamp(30, 120)
            - 1
    }

    pub(crate) fn into_inner(self) -> W {
        self.out
    }

    /// Prints what changed since the last call, then the message box; returns what was
    /// printed of the box the keys answer (§7.1).
    pub(crate) fn sync(&mut self, app: &App, now: Instant) -> io::Result<Option<BoxView>> {
        let width = self.width();
        let mut lines: Vec<Line<'static>> = Vec::new();

        if self.session.as_deref() != Some(app.session.as_str()) {
            self.session = Some(app.session.clone());
            self.printed_seq = 0;
            self.printed_cards.clear();
        }
        if app.entries.is_empty() {
            self.printed_seq = 0;
        }
        let summary = self.status_summary(app);
        if summary.is_some() && summary != self.status {
            self.status.clone_from(&summary);
            lines.push(self.renderer.status_line(app));
        }
        for entry in &app.entries {
            if entry.seq > self.printed_seq {
                self.printed_seq = entry.seq;
                lines.extend(self.renderer.entry_lines(entry, None, width));
            }
        }
        // Only the decision the keys answer is printed as a box, and it is the last box
        // printed: the others wait as one line each until they take the focus (§7.1). The box
        // fits the terminal with its keys; `?` prints the whole call. Printed once, its keys
        // are drawn lit: the message box says when they answer.
        for (index, open) in app.cards.iter().enumerate() {
            let printed = self.printed_cards.get(&open.card.id).copied();
            if index == 0 {
                let key = (open.card.version, open.details, true);
                if printed != Some(key) {
                    self.printed_cards.insert(open.card.id.clone(), key);
                    let rows = (!open.details).then(|| self.box_rows());
                    let fit = Fit { rows, scroll: 0, scrolls: false };
                    let drawn = self.renderer.decision_box(open, width, true, false, fit);
                    let view = BoxView {
                        on_screen: true,
                        body_len: drawn.body_len,
                        body_shown: drawn.body_shown,
                        scrolls: false,
                    };
                    self.focused = Some((open.card.id.clone(), view));
                    lines.push(Line::default());
                    lines.extend(drawn.lines);
                    lines.extend(self.renderer.also_shown(open, width));
                }
            } else if printed.is_none_or(|(version, _, _)| version != open.card.version) {
                self.printed_cards.insert(open.card.id.clone(), (open.card.version, false, false));
                lines.extend(self.renderer.waiting_line(open, width));
            }
        }
        let fresh = app.notice_count.saturating_sub(self.printed_notices);
        let start = app.notices.len().saturating_sub(fresh);
        for notice in &app.notices[start..] {
            lines.extend(self.renderer.notice(&notice.text, notice.tone, width));
        }
        self.printed_notices = app.notice_count;
        for text in &app.queued {
            if !self.printed_queued.contains(text) {
                lines.extend(self.renderer.queued(std::slice::from_ref(text), width, false));
            }
        }
        self.printed_queued.clone_from(&app.queued);
        // The voice conversation's news, each once (spec 3.3 §5.6).
        #[cfg(feature = "voice")]
        if let Some(voice) = &app.voice {
            for news in voice.news(&self.printed_voice, now) {
                lines.push(Line::styled(news, self.renderer.theme.accent()));
            }
            self.printed_voice.clone_from(voice);
        }
        if app.view != self.view {
            self.view = app.view;
            match app.view {
                View::Thread => {}
                View::Today => lines.extend(self.renderer.today(app, width)),
                View::Permissions => lines.extend(self.renderer.permissions(app, width)),
                View::Help => lines.extend(self.renderer.help(app, width)),
                // Printed once it has returned, below.
                View::Output => {}
                // Printed once the kernel answered, below.
                View::Agents => {}
            }
        }
        // The Agents view, each time what it shows changes (spec 3.3 §5.7): `/agents` lists
        // the agents, and changes use `/agent`.
        if app.view == View::Agents
            && app.agents_pane.waiting.is_none()
            && self.printed_agents.as_ref() != Some(&app.agents_pane)
        {
            self.printed_agents = Some(app.agents_pane.clone());
            lines.extend(super::agents::lines(&app.agents_pane, &self.renderer.theme, width));
        }
        // A slash command's output, once, when it returns (spec 3.3 §5.2).
        if let Some(output) = &app.output
            && output.result.is_some()
            && self.printed_output != Some(output.serial)
        {
            self.printed_output = Some(output.serial);
            lines.extend(self.renderer.output(app, width));
        }

        if self.raw {
            let (prompt, text) = self.renderer.prompt(app);
            let drawn = format!(
                "{}{text}{:?}{}{:?}",
                prompt.content,
                self.renderer.placeholder(app, now),
                app.composer.cursor,
                app.completion().map(|completion| completion.selected)
            );
            if lines.is_empty() && self.prompt.as_deref() == Some(drawn.as_str()) {
                return Ok(self.focused_view(app));
            }
            self.prompt = Some(drawn);
            queue!(self.out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        }
        for line in &lines {
            self.write_line(line)?;
            self.out.write_all(if self.raw { b"\r\n" } else { b"\n" })?;
        }
        if self.raw {
            self.prompt(app, now, width)?;
        }
        self.out.flush()?;
        Ok(self.focused_view(app))
    }

    /// What was printed of the box the keys answer now.
    fn focused_view(&self, app: &App) -> Option<BoxView> {
        let open = app.focused()?;
        self.focused.as_ref().filter(|(id, _)| *id == open.card.id).map(|(_, view)| view.clone())
    }

    /// What prints the status line again: the mode, or a stop. Counts and spend change too
    /// often for the scrollback; the decision boxes and the thread show them.
    fn status_summary(&self, app: &App) -> Option<String> {
        let status = app.status.as_ref()?;
        Some(format!("{:?}|{}", status.mode.as_ref().map(|mode| &mode.label), status.stopped,))
    }

    fn prompt(&mut self, app: &App, now: Instant, width: usize) -> io::Result<()> {
        let (prompt, text) = self.renderer.prompt(app);
        let prompt_width = prompt.content.width();
        let room = width.saturating_sub(prompt_width + 1);
        let before: String = text.chars().take(app.composer.cursor).collect();
        let skip = before.width().saturating_sub(room);
        let mut skipped = 0;
        let shown: String = text
            .chars()
            .skip_while(|c| {
                let w = UnicodeWidthStr::width(c.encode_utf8(&mut [0; 4]) as &str);
                if skip > 0 && skipped + w <= skip {
                    skipped += w;
                    true
                } else {
                    false
                }
            })
            .collect();
        let theme = self.renderer.theme;
        let mut spans = vec![prompt];
        match self.renderer.placeholder(app, now) {
            Some(placeholder) => {
                spans.push(ratatui::text::Span::styled(placeholder, theme.label()))
            }
            None => spans.push(ratatui::text::Span::styled(shown.clone(), theme.text())),
        }
        // The commands a `/` and the start of a name may be, after the cursor, on the same
        // line: the chosen one marked (spec 3.3 §5.2).
        if let Some(completion) = app.completion() {
            let used = prompt_width + shown.width();
            let room = width.saturating_sub(used + 3);
            let mut hint: Vec<ratatui::text::Span<'static>> = Vec::new();
            let mut left = room;
            if completion.candidates.is_empty() {
                let text = super::view::fit("no command starts with that · /help lists them", left);
                hint.push(ratatui::text::Span::styled(text, theme.label()));
            }
            for (index, (name, _)) in completion.candidates.iter().enumerate() {
                let word = format!("/{}", super::safe::safe_line(name));
                let cost = word.width() + 3;
                if cost > left {
                    hint.push(ratatui::text::Span::styled("…", theme.label()));
                    break;
                }
                left -= cost;
                let style = if index == completion.selected {
                    theme.accent().add_modifier(Modifier::BOLD)
                } else {
                    theme.label()
                };
                hint.push(ratatui::text::Span::styled(word, style));
                hint.push(ratatui::text::Span::styled(" · ", theme.label()));
            }
            if hint.last().is_some_and(|span| span.content == " · ") {
                hint.pop();
            }
            if !hint.is_empty() {
                spans.push(ratatui::text::Span::raw("   "));
                spans.extend(hint);
            }
        }
        self.write_line(&Line::from(spans))?;
        let column = u16::try_from(prompt_width + before.width() - skipped).unwrap_or(0);
        queue!(self.out, cursor::MoveToColumn(column))
    }

    fn write_line(&mut self, line: &Line<'_>) -> io::Result<()> {
        // Neighbouring spans of one style are written as one run.
        let mut runs: Vec<(Style, String)> = Vec::new();
        for span in &line.spans {
            let style = line.style.patch(span.style);
            match runs.last_mut() {
                Some((last, text)) if *last == style => text.push_str(&span.content),
                _ => runs.push((style, span.content.to_string())),
            }
        }
        for (style, text) in runs {
            if self.color {
                self.set_style(style)?;
            }
            queue!(self.out, Print(text))?;
            if self.color {
                queue!(self.out, SetAttribute(Attribute::Reset), ResetColor)?;
            }
        }
        Ok(())
    }

    fn set_style(&mut self, style: Style) -> io::Result<()> {
        if let Some(fg) = style.fg {
            queue!(self.out, SetForegroundColor(fg.into_crossterm()))?;
        }
        if let Some(bg) = style.bg {
            queue!(self.out, SetBackgroundColor(bg.into_crossterm()))?;
        }
        let modifier = style.add_modifier;
        for (flag, attribute) in [
            (Modifier::BOLD, Attribute::Bold),
            (Modifier::DIM, Attribute::Dim),
            (Modifier::ITALIC, Attribute::Italic),
            (Modifier::UNDERLINED, Attribute::Underlined),
            (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        ] {
            if modifier.contains(flag) {
                queue!(self.out, SetAttribute(attribute))?;
            }
        }
        Ok(())
    }
}

pub(crate) async fn run(surface: TerminalSurface) -> io::Result<i32> {
    term::scoped(inline(surface)).await
}

async fn inline(surface: TerminalSurface) -> io::Result<i32> {
    // A terminal that names itself `dumb` takes no escape sequences: plain lines, as piped.
    let dumb = std::env::var("TERM").is_ok_and(|term| term == "dumb");
    let interactive = !dumb && io::stdin().is_terminal() && io::stdout().is_terminal();
    let color = !dumb && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    // Registered before anything changes the terminal, the background query's raw mode
    // included, so a signal at any moment gives it back.
    let mut signals = Signals::install(interactive && surface.signals);
    let theme = if interactive {
        Theme::detect(surface.theme)
    } else {
        let appearance = match surface.theme {
            super::ThemeChoice::Light => Appearance::Light,
            _ => Appearance::Dark,
        };
        Theme::new(appearance, true)
    };
    if !interactive {
        let input = tokio::io::BufReader::new(tokio::io::stdin());
        return piped(surface, theme, color, input, io::stdout()).await;
    }
    let session = surface.initial_session().await;
    let (mut app, pump, tx, mut rx) = start(&surface, session.clone());
    app.inline = true;
    let mut commands = Commands::default();
    let mut shown = vec![session];
    // A voice conversation shows its own session (spec 3.3 §5.6).
    #[cfg(feature = "voice")]
    let voice = super::voice::open(&surface.voice, &surface.engine, &surface.caller, &tx).await?;
    #[cfg(feature = "voice")]
    if let Some((session, _)) = &voice {
        for effect in app.switch_session(session.clone()) {
            surface.apply(effect, &mut app, &pump, &mut commands, &tx, &mut shown).await;
        }
    }

    term::enter_inline(&mut io::stdout())?;
    let guard = Guard;
    let mut printer = Printer::new(io::stdout(), theme, color, true);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    let mut code = EXIT_DONE;
    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            app.update(update, now);
        }
        let view = printer.sync(&app, now)?;
        app.set_box_view(view, now);
        let held = app.held_until();
        let effects = tokio::select! {
            exit = signals.recv() => {
                code = exit;
                vec![Effect::Quit]
            }
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => app.key(key, Instant::now()),
                Some(Ok(Event::Paste(text))) => {
                    app.paste(&text, Instant::now());
                    Vec::new()
                }
                Some(Ok(_)) => Vec::new(),
                Some(Err(error)) => return Err(error),
                None => vec![Effect::Quit],
            },
            update = rx.recv() => {
                if let Some(update) = update {
                    app.update(update, Instant::now());
                }
                Vec::new()
            }
            () = super::full::sleep_until(held), if held.is_some() => app.release_held(Instant::now()),
            _ = tick.tick() => Vec::new(),
        };
        let mut quit = false;
        for effect in effects {
            quit |= surface.apply(effect, &mut app, &pump, &mut commands, &tx, &mut shown).await;
        }
        if quit {
            break;
        }
    }
    #[cfg(feature = "voice")]
    if let Some((_, running)) = voice {
        running.end().await;
    }
    drop(pump);
    let mut out = printer.into_inner();
    queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    out.flush()?;
    drop(guard);
    term::restore(&mut out);
    surface.leave(commands, &shown, &mut out).await;
    Ok(code)
}

/// The terminal's state and the tasks reading the engine for `session`.
fn start(
    surface: &TerminalSurface,
    session: String,
) -> (App, Pump, mpsc::UnboundedSender<Update>, mpsc::UnboundedReceiver<Update>) {
    let mut app = App::new(surface.engine.source().app_name(), session.clone());
    app.agents_pane.granted =
        surface.engine.granted().iter().map(|granted| granted.text.clone()).collect();
    app.model = surface.model.clone();
    app.place = surface.place.clone();
    app.commands = surface.commands.clone();
    // Without bracketed paste a paste arrives as keys: a decision key with more keys right
    // behind it is typing (§7.4 rule 2).
    app.burst_guard = cfg!(windows);
    for notice in &surface.notices {
        app.notice_until_key(notice.clone(), Tone::Info);
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let mut pump = Pump::start(
        surface.engine.clone(),
        surface.caller.clone(),
        session,
        surface.poll,
        tx.clone(),
    );
    if let Some(notices) = surface.late.take() {
        pump.forward_notices(notices, tx.clone());
    }
    (app, pump, tx, rx)
}

/// Input piped: each line is a message, sent once the turn before it has finished, so two
/// lines never run as two turns at once; once input ends and the last turn settles, returns
/// its exit code. A turn that does not finish — it waits for a decision, fails, or reaches the
/// daily spending limit — ends the run, and the lines after it are not sent.
pub(crate) async fn piped<R, W>(
    surface: TerminalSurface,
    theme: Theme,
    color: bool,
    input: R,
    out: W,
) -> io::Result<i32>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: Write,
{
    // Lines read as messages go to a session of their own unless one is named: never into the
    // thread of a decision this run did not cause.
    let session = surface.session.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (mut app, pump, _tx, mut rx) = start(&surface, session);
    let engine = &surface.engine;
    let caller = &surface.caller;
    let mut printer = Printer::new(out, theme, color, false);
    let mut stdin = input.lines();
    let mut lines: VecDeque<String> = VecDeque::new();
    let mut input_open = true;
    // The turn of the line last sent, until it settles.
    let mut turn: Option<Settler> = None;
    let mut last: Option<End> = None;
    let mut tick = tokio::time::interval(surface.poll);
    let end = loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            observe(&mut turn, &update);
            app.update(update, now);
        }
        printer.sync(&app, now)?;
        if turn.is_none()
            && let Some(line) = lines.pop_front()
        {
            let baseline = engine.thread(caller, &app.session).await.map_or(0, |t| t.len());
            let mut settler = Settler::new(baseline as u64);
            settler.sent();
            let command =
                crate::engine::Command::Send { session_id: app.session.clone(), text: line };
            match engine.execute(caller, command).await {
                Ok(outcome) => app.update(Update::Outcome(Box::new(Ok(outcome))), Instant::now()),
                Err(error) => break End::error(error.to_string()),
            }
            turn = Some(settler);
            continue;
        }
        if turn.is_none() && lines.is_empty() && !input_open {
            match last.take() {
                Some(end) => break end,
                // Nothing was sent: how the session stands.
                None => {
                    let mut settler = Settler::new(0);
                    if let Some(end) = settler.check(engine, caller, &app.session).await {
                        break end;
                    }
                }
            }
        }
        tokio::select! {
            line = stdin.next_line(), if input_open => match line? {
                Some(line) if !line.trim().is_empty() => lines.push_back(line),
                Some(_) => {}
                None => input_open = false,
            },
            update = rx.recv() => {
                if let Some(update) = update {
                    observe(&mut turn, &update);
                    app.update(update, Instant::now());
                }
            }
            _ = tick.tick(), if turn.is_some() => {
                if let Some(settler) = &mut turn
                    && let Some(end) = settler.check(engine, caller, &app.session).await
                {
                    turn = None;
                    if end.outcome != "done" {
                        break end;
                    }
                    last = Some(end);
                }
            }
        }
    };
    let now = Instant::now();
    while let Ok(update) = rx.try_recv() {
        app.update(update, now);
    }
    printer.sync(&app, now)?;
    drop(pump);
    let mut out = printer.into_inner();
    // The heading names the call's argument, and errors come from sources: model, tool and
    // file text never reaches the terminal raw (S-I9).
    writeln!(out, "{}", terminal_safe(&end.text))?;
    if !lines.is_empty() || input_open {
        writeln!(out, "Not sent: the input after it.")?;
    }
    Ok(end.code)
}

/// A feed entry of the session, for the turn being watched.
fn observe(turn: &mut Option<Settler>, update: &Update) {
    if let (Some(settler), Update::Frame { frame: crate::engine::FeedFrame::Entry { entry }, .. }) =
        (turn, update)
    {
        settler.observe(entry);
    }
}
