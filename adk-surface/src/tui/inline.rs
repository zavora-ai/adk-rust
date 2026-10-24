//! The inline terminal (spec 3.3 §5.2): the thread and its decision boxes printed into the
//! scrollback, with the message box on the last line.
//!
//! On a terminal, keys work as in the full-screen UI (raw mode and bracketed paste, without the
//! alternate screen), and the terminal is given back on exit, on a panic in the terminal's own
//! code and on a signal. With standard input piped, or on a terminal that names itself
//! `dumb`, each line is a message, a decision is never answered from it, nothing but plain
//! text is written, and the run ends as JSON mode does once input ends and the turn settles.

use std::collections::HashMap;
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

use super::app::{App, Effect, Tone, Update, View};
use super::json::Settler;
use super::pump::Pump;
use super::safe::terminal_safe;
use super::term::{self, Guard, Signals};
use super::theme::{Appearance, Theme};
use super::view::Renderer;
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
    printed_cards: HashMap<String, (u64, bool)>,
    printed_notices: usize,
    printed_queued: Vec<String>,
    status: Option<String>,
    view: View,
    width: Option<usize>,
    /// The message box as last drawn.
    prompt: Option<String>,
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
            prompt: None,
        }
    }

    /// A fixed width instead of the terminal's.
    #[cfg(test)]
    pub(crate) fn with_width(mut self, width: usize) -> Self {
        self.width = Some(width);
        self
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

    /// Prints what changed since the last call, then the message box.
    pub(crate) fn sync(&mut self, app: &App, now: Instant) -> io::Result<()> {
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
        let live = app.keys_answer(now);
        for (index, open) in app.cards.iter().enumerate() {
            let key = (open.card.version, open.details);
            if self.printed_cards.get(&open.card.id) != Some(&key) {
                self.printed_cards.insert(open.card.id.clone(), key);
                lines.push(Line::default());
                lines.extend(self.renderer.decision_box(open, width, index == 0 && live, false));
                lines.extend(self.renderer.also_shown(open, width));
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
                lines.extend(self.renderer.queued(std::slice::from_ref(text), width));
            }
        }
        self.printed_queued.clone_from(&app.queued);
        if app.view != self.view {
            self.view = app.view;
            match app.view {
                View::Thread => {}
                View::Today => lines.extend(self.renderer.today(app, width)),
                View::Permissions => lines.extend(self.renderer.permissions(app, width)),
                View::Help => lines.extend(self.renderer.help()),
            }
        }

        if self.raw {
            let (prompt, text) = self.renderer.prompt(app);
            let drawn = format!(
                "{}{text}{:?}{}",
                prompt.content,
                self.renderer.placeholder(app, now),
                app.composer.cursor
            );
            if lines.is_empty() && self.prompt.as_deref() == Some(drawn.as_str()) {
                return Ok(());
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
        self.out.flush()
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
            None => spans.push(ratatui::text::Span::styled(shown, theme.text())),
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
    let theme = if interactive {
        Theme::detect(surface.theme)
    } else {
        let appearance = match surface.theme {
            super::ThemeChoice::Light => Appearance::Light,
            _ => Appearance::Dark,
        };
        Theme::new(appearance, true)
    };
    let session = surface.initial_session().await;
    let mut app = App::new(surface.engine.source().app_name(), session.clone());
    app.model = surface.model.clone();
    app.place = surface.place.clone();
    for notice in &surface.notices {
        app.notice(notice.clone(), Tone::Info);
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pump = Pump::start(
        surface.engine.clone(),
        surface.caller.clone(),
        session,
        surface.poll,
        tx.clone(),
    );

    if !interactive {
        return piped(surface, app, rx, tx, pump, theme, color).await;
    }

    // Registered before the terminal changes, so a signal at any moment gives it back.
    let mut signals = Signals::install(surface.signals);
    term::enter_inline(&mut io::stdout())?;
    let _guard = Guard;
    let mut printer = Printer::new(io::stdout(), theme, color, true);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    let mut code = EXIT_DONE;
    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            app.update(update, now);
        }
        printer.sync(&app, now)?;
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
            _ = tick.tick() => Vec::new(),
        };
        let mut quit = false;
        for effect in effects {
            match effect {
                Effect::Execute(command) => {
                    Pump::execute(&surface.engine, &surface.caller, command, &tx);
                }
                Effect::Session(session) => pump.switch(session),
                Effect::Briefing => pump.refresh_briefing(),
                Effect::Quit => quit = true,
            }
        }
        if quit {
            break;
        }
    }
    let mut out = printer.into_inner();
    queue!(out, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    out.flush()?;
    term::restore(&mut out);
    Ok(code)
}

/// Standard input piped: each line is a message; once input ends, waits for the turn to
/// settle and returns its exit code.
async fn piped(
    surface: TerminalSurface,
    mut app: App,
    mut rx: mpsc::UnboundedReceiver<Update>,
    tx: mpsc::UnboundedSender<Update>,
    pump: Pump,
    theme: Theme,
    color: bool,
) -> io::Result<i32> {
    let mut printer = Printer::new(io::stdout(), theme, color, false);
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let baseline =
        surface.engine.thread(&surface.caller, &app.session).await.map_or(0, |t| t.len());
    let mut settler = Settler::new(baseline as u64);
    let mut input_open = true;
    let mut tick = tokio::time::interval(surface.poll);
    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            if let Update::Frame { frame: crate::engine::FeedFrame::Entry { entry }, .. } = &update
            {
                settler.observe(entry);
            }
            app.update(update, now);
        }
        printer.sync(&app, now)?;
        tokio::select! {
            line = stdin.next_line(), if input_open => match line? {
                Some(line) if !line.trim().is_empty() => {
                    settler.sent();
                    let command = crate::engine::Command::Send {
                        session_id: app.session.clone(),
                        text: line,
                    };
                    Pump::execute(&surface.engine, &surface.caller, command, &tx);
                }
                Some(_) => {}
                None => input_open = false,
            },
            update = rx.recv() => {
                if let Some(update) = update {
                    if let Update::Frame { frame: crate::engine::FeedFrame::Entry { entry }, .. } = &update {
                        settler.observe(entry);
                    }
                    app.update(update, Instant::now());
                }
            }
            _ = tick.tick(), if !input_open => {
                if let Some(end) = settler.check(&surface.engine, &surface.caller, &app.session).await {
                    let now = Instant::now();
                    while let Ok(update) = rx.try_recv() {
                        app.update(update, now);
                    }
                    printer.sync(&app, now)?;
                    drop(pump);
                    let mut out = printer.into_inner();
                    // The heading names the call's argument, and errors come from sources:
                    // model, tool and file text never reaches the terminal raw (S-I9).
                    writeln!(out, "{}", terminal_safe(&end.text))?;
                    return Ok(end.code);
                }
            }
        }
    }
}
