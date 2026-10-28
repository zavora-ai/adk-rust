//! The terminal (spec 3.3 §5.2): the same home as the web, from the keyboard.
//!
//! The terminal is a client of the [`SurfaceEngine`] like every other surface: it shows what
//! the engine projects, in the engine's words (§8), and changes nothing except through
//! [`Command`](crate::Command)s. It holds no state a reconnect cannot rebuild.
//!
//! | Mode | What it is | Entry point |
//! |---|---|---|
//! | Full screen (default) | A status line; the roster, what is coming up and "While you were away" on the left; the thread on the right; the decision as a box answered by key; footer key hints | [`TerminalSurface::run`] with [`UiMode::FullScreen`] |
//! | Inline | The same thread and boxes printed into the terminal's scrollback | [`TerminalSurface::run`] with [`UiMode::Inline`] |
//! | JSON | One engine event per line and an exit code, for CI | [`TerminalSurface::run_json`] |
//!
//! A decision is answered by key — `y` allow once, `a` always allow, `e` edit, `n` deny, `?`
//! details — and only by those keys, only while the message box is empty, and only once the
//! box has been on screen for a moment, so a keystroke meant for a message never answers a
//! decision that just appeared (§7.4 rules 1 and 2, §11 S-I4). Esc never decides: it moves
//! between answering by key and typing. A message typed while a decision is open is queued and
//! shown as queued. Every string from a model, a tool or a file goes through
//! [`terminal_safe`] first (§11 S-I9).
//!
//! # Exit codes
//!
//! | Code | Constant | Meaning |
//! |---|---|---|
//! | 0 | [`EXIT_DONE`] | The turn finished, or the person left the full-screen or inline UI |
//! | 1 | [`EXIT_ERROR`] | The turn failed, ended without a reply, or every agent is stopped |
//! | 3 | [`EXIT_WAITING`] | A decision waits for a person; it stays in the inbox for any surface |
//! | 4 | [`EXIT_BUDGET`] | Today's spend reached the daily spending limit |
//! | 129, 130, 143 | — | `SIGHUP`, `SIGINT`, `SIGTERM` in the full-screen or inline UI: the terminal was restored first |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_surface::tui::{TerminalSurface, ThemeChoice, UiMode};
//! use adk_surface::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};
//!
//! # async fn example(sessions: Arc<dyn adk_session::SessionService>, approvals: Arc<dyn adk_core::ApprovalService>) -> std::io::Result<()> {
//! let source = LocalSource::builder("adk-rust").sessions(sessions).approvals(approvals).build()
//!     .map_err(std::io::Error::other)?;
//! let engine = SurfaceEngine::builder(Arc::new(source)).build();
//! let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
//! let code = TerminalSurface::new(engine, me).theme(ThemeChoice::Auto).run(UiMode::FullScreen).await?;
//! std::process::exit(code);
//! # }
//! ```

mod agents;
mod app;
mod commands;
mod drawn;
mod full;
mod inline;
mod json;
mod markdown;
mod pump;
mod safe;
mod term;
#[cfg(test)]
mod tests;
mod theme;
mod view;
#[cfg(feature = "voice")]
mod voice;

use std::io::{IsTerminal, Write};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc::UnboundedReceiver;

pub use commands::{CommandContext, TerminalCommand};
pub use safe::terminal_safe;
pub use theme::{Appearance, Theme, ThemeChoice};
#[cfg(feature = "voice")]
pub use voice::TerminalVoice;

use crate::engine::SurfaceEngine;
use crate::origin::Caller;

/// The turn finished, or the person left the terminal (spec 3.3 §5.2).
pub const EXIT_DONE: i32 = 0;
/// The turn failed, ended without a reply, or every agent is stopped (spec 3.3 §5.2).
pub const EXIT_ERROR: i32 = 1;
/// A decision waits for a person (spec 3.3 §7.3): it never expires, and any surface may
/// answer it. The same code `adk-rust code` uses for a held call.
pub const EXIT_WAITING: i32 = 3;
/// Today's spend reached the daily spending limit (spec 3.3 §8.1).
pub const EXIT_BUDGET: i32 = 4;

/// How long the terminal waits between reads of the engine when the source signals nothing.
const POLL: Duration = Duration::from_millis(400);

/// A writer whose bytes outlive the terminal drawing into it.
#[derive(Clone, Default)]
struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Full screen or inline (spec 3.3 §5.2).
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::UiMode;
///
/// assert_eq!("inline".parse::<UiMode>().unwrap(), UiMode::Inline);
/// assert_eq!("full".parse::<UiMode>().unwrap(), UiMode::FullScreen);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiMode {
    /// The whole terminal, in the alternate screen.
    #[default]
    FullScreen,
    /// Lines in the terminal's scrollback.
    Inline,
}

impl FromStr for UiMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" | "full-screen" | "fullscreen" => Ok(Self::FullScreen),
            "inline" => Ok(Self::Inline),
            other => Err(format!("unknown UI '{other}': use full or inline")),
        }
    }
}

/// What the terminal writes: text for a person, or one JSON event per line (spec 3.3 §5.2).
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::OutputFormat;
///
/// assert_eq!("json".parse::<OutputFormat>().unwrap(), OutputFormat::Json);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// For a person.
    #[default]
    Text,
    /// One engine event per line, then an `end` line; the exit code says how it ended.
    Json,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!("unknown output '{other}': use text or json")),
        }
    }
}

/// Opens the session `/session` names, or a new one for `None`: the id to show, or why not, in
/// words the terminal shows as they are.
type Opener =
    Arc<dyn Fn(Option<String>) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// How `/session` opens a session: the application's rule, or any id as given.
#[derive(Clone, Default)]
struct SessionGate(Option<Opener>);

impl std::fmt::Debug for SessionGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "SessionGate(set)" } else { "SessionGate(none)" })
    }
}

/// Notices that arrive while the terminal runs; the first UI to run takes them.
#[derive(Clone, Default)]
struct LateNotices(Arc<Mutex<Option<UnboundedReceiver<String>>>>);

impl LateNotices {
    fn take(&self) -> Option<UnboundedReceiver<String>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

impl std::fmt::Debug for LateNotices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LateNotices")
    }
}

/// The terminal surface over an engine, for one caller (spec 3.3 §5.2).
///
/// The caller is the person at the keyboard: [`SurfaceKind::Terminal`](crate::SurfaceKind)
/// with [`Origin::Keyboard`](crate::Origin), and
/// [`with_paired_local`](Caller::with_paired_local) only when the terminal reaches the kernel
/// on its own machine, which lets it approve high-risk calls and resume after a stop (§7.2,
/// S-I10).
#[derive(Debug, Clone)]
pub struct TerminalSurface {
    engine: SurfaceEngine,
    caller: Caller,
    session: Option<String>,
    theme: ThemeChoice,
    model: Option<String>,
    place: Option<String>,
    poll: Duration,
    notices: Vec<String>,
    late: LateNotices,
    signals: bool,
    gate: SessionGate,
    commands: commands::Commands,
    #[cfg(feature = "voice")]
    voice: voice::VoiceSlot,
}

impl TerminalSurface {
    /// The terminal over `engine`, as `caller`.
    pub fn new(engine: SurfaceEngine, caller: Caller) -> Self {
        Self {
            engine,
            caller,
            session: None,
            theme: ThemeChoice::Auto,
            model: None,
            place: None,
            poll: POLL,
            notices: Vec::new(),
            late: LateNotices::default(),
            signals: true,
            gate: SessionGate::default(),
            commands: commands::Commands::default(),
            #[cfg(feature = "voice")]
            voice: voice::VoiceSlot::default(),
        }
    }

    /// Talks with the agents by voice (spec 3.3 §5.6; feature `voice`): the conversation's own
    /// session is the thread shown, its state is a line above the message box — in the inline
    /// UI, what it heard and each countdown are printed into the scrollback — and `c` cancels a
    /// spoken "allow once" while it counts down. In process only: the conversation runs on the
    /// hub's harness. JSON output and piped input have no voice conversation.
    #[cfg(feature = "voice")]
    #[must_use]
    pub fn voice(self, voice: TerminalVoice) -> Self {
        *self.voice.lock().unwrap_or_else(|e| e.into_inner()) = Some(voice);
        self
    }

    /// Opens `session`. Without one, the terminal opens the session of the oldest decision
    /// waiting for the caller, else a new session.
    #[must_use]
    pub fn session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Light, dark, or detected from the terminal's background (the default).
    #[must_use]
    pub fn theme(mut self, theme: ThemeChoice) -> Self {
        self.theme = theme;
        self
    }

    /// The model, for the status line, such as `claude-sonnet-5-5`.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Where the agents run, for the status line, such as `kernel` or `in process`.
    #[must_use]
    pub fn place(mut self, place: impl Into<String>) -> Self {
        self.place = Some(place.into());
        self
    }

    /// How often the terminal reads the engine when the source signals nothing sooner.
    #[must_use]
    pub fn poll_interval(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// A line the full-screen and inline UIs show under the thread when they start, such as
    /// a workspace that is not trusted (spec 3.3 §5.2). JSON mode writes none.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use adk_surface::tui::TerminalSurface;
    /// # use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
    /// # fn example(engine: SurfaceEngine) {
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// let terminal = TerminalSurface::new(engine, me)
    ///     .notice("Instruction files in this workspace are not loaded: it is not trusted.");
    /// # }
    /// ```
    #[must_use]
    pub fn notice(mut self, text: impl Into<String>) -> Self {
        self.notices.push(text.into());
        self
    }

    /// Lines the full-screen and inline UIs show under the thread as they arrive on
    /// `notices`, as [`notice`](Self::notice) shows one at the start: what work the
    /// application runs beside the terminal found, such as long-term memory catching up on
    /// earlier sessions. JSON mode writes none. Once the terminal has ended, a send fails and
    /// hands the line back, so the application can print it.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use adk_surface::tui::TerminalSurface;
    /// # use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
    /// # fn example(engine: SurfaceEngine) {
    /// let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// let terminal = TerminalSurface::new(engine, me).notices_from(rx);
    /// tokio::spawn(async move {
    ///     let _ = tx.send("2 facts from an earlier session were added".to_string());
    /// });
    /// # }
    /// ```
    #[must_use]
    pub fn notices_from(mut self, notices: UnboundedReceiver<String>) -> Self {
        self.late = LateNotices(Arc::new(Mutex::new(Some(notices))));
        self
    }

    /// How `/session` opens a session in the full-screen and inline UIs: `open` gets the id
    /// typed, or `None` for `/session new`, and returns the session to show, or why it may not
    /// be opened, which the terminal shows. Without it, any id opens as given. An application
    /// whose sessions are bound to something, such as the workspace a harness works in, checks
    /// and records that binding here (spec 3.3 §5.2).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use adk_surface::tui::TerminalSurface;
    /// # use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
    /// # fn example(engine: SurfaceEngine) {
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// let terminal = TerminalSurface::new(engine, me).open_sessions_with(|requested| {
    ///     Box::pin(async move {
    ///         match requested {
    ///             Some(id) if id.starts_with("ops-") => Ok(id),
    ///             Some(id) => Err(format!("session {id} is not one of this workspace's")),
    ///             None => Ok(format!("ops-{}", uuid::Uuid::new_v4())),
    ///         }
    ///     })
    /// });
    /// # }
    /// ```
    #[must_use]
    pub fn open_sessions_with<F>(mut self, open: F) -> Self
    where
        F: Fn(Option<String>) -> BoxFuture<'static, Result<String, String>> + Send + Sync + 'static,
    {
        self.gate = SessionGate(Some(Arc::new(open)));
        self
    }

    /// Adds a slash command: `/name args` in the message box runs it and shows what it
    /// returns, and Tab completes its name (spec 3.3 §5.2). The terminal's own commands win
    /// over one of the same name, and so does the first one added.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use adk_surface::tui::{TerminalCommand, TerminalSurface};
    /// # use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
    /// # fn example(engine: SurfaceEngine, todos: Arc<dyn TerminalCommand>) {
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// let terminal = TerminalSurface::new(engine, me).command(todos);
    /// # }
    /// ```
    #[must_use]
    pub fn command(mut self, command: Arc<dyn TerminalCommand>) -> Self {
        self.commands.add(command);
        self
    }

    /// Whether the full-screen and inline UIs give the terminal back on `SIGINT`, `SIGTERM`
    /// and `SIGHUP` (Ctrl-C elsewhere) and end with 130, 143 or 129: on by default.
    ///
    /// The handlers are tokio's, which last as long as the process: once [`run`](Self::run)
    /// has returned, these signals no longer end the process unless something else handles
    /// them. An application that goes on after the terminal and relies on their default
    /// action turns this off and handles them itself.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use adk_surface::tui::{TerminalSurface, UiMode};
    /// # use adk_surface::{Caller, Origin, SurfaceEngine, SurfaceKind};
    /// # async fn example(engine: SurfaceEngine) -> std::io::Result<()> {
    /// let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
    /// // This application serves on after the terminal and handles its own signals.
    /// TerminalSurface::new(engine, me).restore_on_signals(false).run(UiMode::FullScreen).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn restore_on_signals(mut self, enabled: bool) -> Self {
        self.signals = enabled;
        self
    }

    /// Whether standard input and output are a terminal the full-screen UI (spec 3.3 §5.2) can
    /// draw on: both are terminals, `TERM` is not `dumb`, and the window is at least 60 by 16.
    pub fn capable_terminal() -> bool {
        std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").map_or(cfg!(windows), |term| term != "dumb")
            && crossterm::terminal::size().is_ok_and(|(cols, rows)| cols >= 60 && rows >= 16)
    }

    /// Runs the terminal until the person leaves, or a signal ends it; returns the exit code
    /// (spec 3.3 §5.2).
    ///
    /// Full screen takes the alternate screen, raw mode and bracketed paste; inline takes raw
    /// mode and bracketed paste and prints into the scrollback. Both give the terminal back on
    /// exit, on a panic in the terminal's own code (a panic another task catches, such as a
    /// tool's, leaves it), and on `SIGINT`, `SIGTERM` and `SIGHUP` unless
    /// [`restore_on_signals`](Self::restore_on_signals) turns that off. With standard input
    /// not a terminal, or `TERM=dumb`, inline writes plain lines, each line read is a message,
    /// and the run ends as [`run_json`](Self::run_json) does once input ends and the turn
    /// settles.
    ///
    /// # Errors
    ///
    /// Returns the terminal's I/O error, or the engine's error from the first read.
    pub async fn run(self, ui: UiMode) -> std::io::Result<i32> {
        match ui {
            UiMode::FullScreen => full::run(self).await,
            UiMode::Inline => inline::run(self).await,
        }
    }

    /// Sends `message` (when given) and writes the session's engine events to `out`, one
    /// JSON object per line, until the turn settles; then an `end` line, and returns the exit
    /// code (spec 3.3 §5.2).
    ///
    /// | Line `type` | What |
    /// |---|---|
    /// | `command` | The outcome of sending the message: `sent` or `queued` |
    /// | `entry`, `decisions`, `reset` | A [`FeedFrame`](crate::FeedFrame) of the session, as the engine projects it |
    /// | `agent` | An agent a person added changed while the run lasted (spec 3.3 §5.7): `change` is `added`, `changed`, `paused`, `resumed` or `removed`, `agent` its roster row |
    /// | `end` | `outcome` (`done`, `error`, `waitingForYou`, `budgetReached`), `exitCode`, `sessionId`, `text` |
    ///
    /// # Errors
    ///
    /// Returns the writer's I/O error.
    pub async fn run_json<W: Write + Send>(
        self,
        message: Option<String>,
        out: W,
    ) -> std::io::Result<i32> {
        json::run(self, message, out).await
    }

    /// One frame of the full-screen UI (spec 3.3 §5.2) as a terminal of `cols` by `rows`
    /// receives it — the escape sequences and text, drawn from what the engine shows now,
    /// with the decision keys live — for tests, screenshots and documentation. `Auto` draws
    /// the dark set, in 24-bit colour.
    ///
    /// # Errors
    ///
    /// Returns the engine's error from reading the shown session's thread.
    pub async fn screen(&self, cols: u16, rows: u16) -> std::io::Result<Vec<u8>> {
        use ratatui::backend::CrosstermBackend;
        use ratatui::layout::Rect;
        use ratatui::{Terminal, TerminalOptions, Viewport};

        let session = self.initial_session().await;
        let engine = &self.engine;
        let caller = &self.caller;
        let mut app = app::App::new(engine.source().app_name(), session.clone());
        app.agents_pane.granted = engine.granted().to_vec();
        app.model = self.model.clone();
        app.place = self.place.clone();
        app.commands = self.commands.clone();
        let now = std::time::Instant::now();
        let armed = now.checked_sub(app::ARM_DELAY * 2).unwrap_or(now);
        let status = match engine.status(caller, Some(&session)).await {
            Ok(status) => Some(status),
            Err(_) => engine.status(caller, None).await.ok(),
        };
        if let Some(status) = status {
            app.update(app::Update::Status(status), now);
        }
        if let Ok(agents) = engine.agents(caller).await {
            app.update(app::Update::Agents(agents), now);
        }
        if let Ok(removed) = engine.removed_agents(caller).await {
            app.update(app::Update::Removed(removed), now);
        }
        if let Ok(briefing) = engine.briefing(caller, pump::start_of_today()).await {
            app.update(app::Update::Briefing(briefing), now);
        }
        let thread = match engine.thread(caller, &session).await {
            Ok(thread) => thread,
            Err(crate::SurfaceError::NotFound { .. }) => Vec::new(),
            Err(error) => return Err(std::io::Error::other(error)),
        };
        for entry in thread {
            let frame = crate::FeedFrame::Entry { entry };
            app.update(app::Update::Frame { session: session.clone(), generation: 0, frame }, now);
        }
        let cards = engine
            .decisions(caller)
            .await
            .map_err(std::io::Error::other)?
            .into_iter()
            .filter(|card| card.session_id == session)
            .collect();
        let frame = crate::FeedFrame::Decisions { cards };
        app.update(app::Update::Frame { session: session.clone(), generation: 0, frame }, armed);
        app.queued = engine.queued(caller, &session);

        let appearance = match self.theme {
            ThemeChoice::Light => Appearance::Light,
            ThemeChoice::Dark | ThemeChoice::Auto => Appearance::Dark,
        };
        let mut renderer = view::Renderer::new(Theme::new(appearance, true));
        let options = TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)) };
        let bytes = Shared::default();
        let mut terminal = Terminal::with_options(CrosstermBackend::new(bytes.clone()), options)?;
        terminal.draw(|frame| {
            renderer.draw(frame, &app, now);
        })?;
        drop(terminal);
        Ok(std::mem::take(&mut *bytes.0.lock().unwrap_or_else(|e| e.into_inner())))
    }

    /// The session `/session` asked for, through the application's gate.
    async fn open_session(&self, requested: Option<String>) -> Result<String, String> {
        match &self.gate.0 {
            Some(open) => open(requested).await,
            None => Ok(requested.unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
        }
    }

    /// What the full-screen and inline loops do with an effect of a key; returns whether the
    /// person is leaving.
    async fn apply(
        &self,
        effect: app::Effect,
        app: &mut app::App,
        pump: &pump::Pump,
        commands: &mut pump::Commands,
        tx: &tokio::sync::mpsc::UnboundedSender<app::Update>,
        shown: &mut Vec<String>,
    ) -> bool {
        match effect {
            app::Effect::Execute(command) => {
                commands.execute(&self.engine, &self.caller, command, tx);
            }
            app::Effect::Session(session) => {
                if !shown.contains(&session) {
                    shown.push(session.clone());
                }
                pump.switch(session);
            }
            app::Effect::OpenSession(requested) => match self.open_session(requested).await {
                Ok(session) => {
                    for effect in app.switch_session(session) {
                        if let app::Effect::Session(session) = effect {
                            if !shown.contains(&session) {
                                shown.push(session.clone());
                            }
                            pump.switch(session);
                        }
                    }
                }
                Err(why) => app.notice(why, app::Tone::Problem),
            },
            app::Effect::Unqueue(session) => {
                app.took_back(self.engine.unqueue_last(&self.caller, &session));
            }
            app::Effect::Briefing => pump.refresh_briefing(),
            app::Effect::RunCommand { serial, name, args } => {
                let found = self.commands.find(&name).or_else(|| self.commands.shadowed(&name));
                match found {
                    Some(command) => {
                        let context = CommandContext::new(app.session.clone());
                        commands.run_added(command, args, context, serial, tx);
                    }
                    None => app.update(
                        app::Update::CommandDone {
                            serial,
                            result: Err(format!("No command /{name}. /help lists them.")),
                        },
                        std::time::Instant::now(),
                    ),
                }
            }
            app::Effect::Quit => return true,
        }
        false
    }

    /// What leaving does not deliver, said once the terminal is given back: input queued in
    /// this process, and commands that had not finished (spec 3.3 §7.4 rule 2, S-I10).
    async fn leave(&self, commands: pump::Commands, shown: &[String], out: &mut impl Write) {
        if commands.running() {
            let _ = writeln!(out, "Finishing what you sent…");
        }
        let left = commands.finish(pump::FINISH_WAIT).await;
        if !left.is_empty() {
            let _ = writeln!(out, "Left before this finished: {}.", left.join(", "));
        }
        let queued: Vec<String> = shown
            .iter()
            .flat_map(|session| self.engine.queued(&self.caller, session))
            .map(|text| format!("\"{}\"", safe::safe_line(&text)))
            .collect();
        if !queued.is_empty() {
            let _ = writeln!(
                out,
                "Not sent — queued in this terminal, which has closed: {}",
                queued.join(", ")
            );
        }
        let _ = out.flush();
    }

    /// The session to open: the one set, the session of the oldest decision waiting, or new.
    async fn initial_session(&self) -> String {
        if let Some(session) = &self.session {
            return session.clone();
        }
        if let Ok(cards) = self.engine.decisions(&self.caller).await
            && let Some(card) = cards.first()
        {
            return card.session_id.clone();
        }
        uuid::Uuid::new_v4().to_string()
    }
}
