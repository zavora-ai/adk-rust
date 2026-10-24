//! The full-screen terminal (spec 3.3 §5.2): the alternate screen in raw mode, given back on
//! exit, on a panic in its own code and on a signal.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use super::app::{App, Effect, Tone};
use super::pump::Pump;
use super::term::{self, Guard, Signals};
use super::theme::Theme;
use super::view::Renderer;
use super::{EXIT_DONE, TerminalSurface};

/// How often the screen is drawn while nothing happens, so a decision box arms on time.
const TICK: Duration = Duration::from_millis(200);

pub(crate) async fn run(surface: TerminalSurface) -> io::Result<i32> {
    term::scoped(full_screen(surface)).await
}

async fn full_screen(surface: TerminalSurface) -> io::Result<i32> {
    // The background is asked for before raw mode and the alternate screen.
    let theme = Theme::detect(surface.theme);
    let session = surface.initial_session().await;
    let mut app = App::new(surface.engine.source().app_name(), session.clone());
    app.model = surface.model.clone();
    app.place = surface.place.clone();
    for notice in &surface.notices {
        app.notice(notice.clone(), Tone::Info);
    }
    let mut renderer = Renderer::new(theme);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let pump = Pump::start(
        surface.engine.clone(),
        surface.caller.clone(),
        session,
        surface.poll,
        tx.clone(),
    );

    // Registered before the terminal changes, so a signal at any moment gives it back.
    let mut signals = Signals::install(surface.signals);
    term::enter_full(&mut io::stdout())?;
    let _guard = Guard;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    let mut code = EXIT_DONE;

    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            app.update(update, now);
        }
        let mut box_shown = true;
        terminal.draw(|frame| box_shown = renderer.draw(frame, &app, now))?;
        app.set_box_shown(box_shown, now);
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
    drop(terminal);
    drop(pump);
    term::restore(&mut io::stdout());
    Ok(code)
}
