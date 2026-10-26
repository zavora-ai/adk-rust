//! The full-screen terminal (spec 3.3 §5.2): the alternate screen in raw mode, given back on
//! exit, on a panic in its own code and on a signal.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use super::app::{App, Tone};
use super::pump::{Commands, Pump};
use super::term::{self, Guard, Signals};
use super::theme::Theme;
use super::view::Renderer;
use super::{EXIT_DONE, TerminalSurface};

/// How often the screen is drawn while nothing happens, so a decision box arms on time.
const TICK: Duration = Duration::from_millis(200);

pub(crate) async fn run(surface: TerminalSurface) -> io::Result<i32> {
    term::scoped(full_screen(surface)).await
}

/// Clears the whole screen and has the next frame draw every cell, without asking the
/// terminal where its cursor is: a terminal that never answers that query (a hidden panel, an
/// IDE console) still gets the full screen.
pub(crate) fn clear<B: Backend>(terminal: &mut Terminal<B>) -> Result<(), B::Error> {
    let size = terminal.size()?;
    terminal.resize(Rect::new(0, 0, size.width, size.height))
}

async fn full_screen(surface: TerminalSurface) -> io::Result<i32> {
    // Registered before anything changes the terminal, the background query's raw mode
    // included, so a signal at any moment gives it back.
    let mut signals = Signals::install(surface.signals);
    // The background is asked for before raw mode and the alternate screen.
    let theme = Theme::detect(surface.theme);
    let session = surface.initial_session().await;
    let mut app = App::new(surface.engine.source().app_name(), session.clone());
    app.model = surface.model.clone();
    app.place = surface.place.clone();
    app.commands = surface.commands.clone();
    // Without bracketed paste a paste arrives as keys: a decision key with more keys right
    // behind it is typing (§7.4 rule 2).
    app.burst_guard = cfg!(windows);
    for notice in &surface.notices {
        app.notice_until_key(notice.clone(), Tone::Info);
    }
    let mut renderer = Renderer::new(theme);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut pump = Pump::start(
        surface.engine.clone(),
        surface.caller.clone(),
        session.clone(),
        surface.poll,
        tx.clone(),
    );
    if let Some(notices) = surface.late.take() {
        pump.forward_notices(notices, tx.clone());
    }
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

    term::enter_full(&mut io::stdout())?;
    let guard = Guard;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    clear(&mut terminal)?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    let mut code = EXIT_DONE;

    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            app.update(update, now);
        }
        let mut box_view = None;
        terminal.draw(|frame| box_view = renderer.draw(frame, &app, now))?;
        app.set_box_view(box_view, now);
        app.view_scroll = app.view_scroll.min(renderer.view_max_scroll);
        app.view_page = renderer.view_page;
        let held = app.held_until();
        let effects = tokio::select! {
            exit = signals.recv() => {
                code = exit;
                vec![super::app::Effect::Quit]
            }
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) => app.key(key, Instant::now()),
                Some(Ok(Event::Paste(text))) => {
                    app.paste(&text, Instant::now());
                    Vec::new()
                }
                Some(Ok(_)) => Vec::new(),
                Some(Err(error)) => return Err(error),
                None => vec![super::app::Effect::Quit],
            },
            update = rx.recv() => {
                if let Some(update) = update {
                    app.update(update, Instant::now());
                }
                Vec::new()
            }
            () = sleep_until(held), if held.is_some() => app.release_held(Instant::now()),
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
    drop(terminal);
    drop(pump);
    drop(guard);
    term::restore(&mut io::stdout());
    surface.leave(commands, &shown, &mut io::stdout()).await;
    Ok(code)
}

/// Sleeps until `at`, or forever without one.
pub(crate) async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}
