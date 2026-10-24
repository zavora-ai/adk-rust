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

mod app;
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

use std::io::{IsTerminal, Write};
use std::str::FromStr;
use std::time::Duration;

pub use safe::terminal_safe;
pub use theme::{Appearance, Theme, ThemeChoice};

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
    signals: bool,
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
            signals: true,
        }
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
        app.model = self.model.clone();
        app.place = self.place.clone();
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
            app.update(app::Update::Frame { session: session.clone(), frame }, now);
        }
        let cards = engine
            .decisions(caller)
            .await
            .map_err(std::io::Error::other)?
            .into_iter()
            .filter(|card| card.session_id == session)
            .collect();
        let frame = crate::FeedFrame::Decisions { cards };
        app.update(app::Update::Frame { session: session.clone(), frame }, armed);
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
