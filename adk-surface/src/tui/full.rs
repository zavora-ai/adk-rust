//! The full-screen terminal (spec 3.3 §5.2): the alternate screen in raw mode, restored on
//! exit, on a panic and on a signal.

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event, EventStream};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use super::app::{App, Effect};
use super::pump::Pump;
use super::theme::Theme;
use super::view::Renderer;
use super::{EXIT_DONE, TerminalSurface};

/// Whether this process holds the terminal in raw mode and the alternate screen.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// How often the screen is drawn while nothing happens, so a decision box arms on time.
const TICK: Duration = Duration::from_millis(200);

/// Writes what leaves the alternate screen and shows the cursor, and turns raw mode off,
/// once per [`enter`]; safe to call from a panic hook.
pub(crate) fn restore(out: &mut impl Write) {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
        let _ = out.flush();
    }
}

/// Installs, once per process, a panic hook that restores the terminal before the panic
/// message prints.
fn restore_on_panic() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(&mut io::stdout());
            previous(info);
        }));
    });
}

/// Takes the terminal: raw mode, the alternate screen, bracketed paste, restored on a panic.
fn enter(out: &mut impl Write) -> io::Result<()> {
    restore_on_panic();
    enable_raw_mode()?;
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(error) = execute!(out, EnterAlternateScreen, EnableBracketedPaste) {
        restore(out);
        return Err(error);
    }
    Ok(())
}

/// Restores the terminal when the full-screen UI ends, however it ends.
struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore(&mut io::stdout());
    }
}

/// A signal that ends the UI: its exit code.
async fn signalled() -> i32 {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
            signal(SignalKind::interrupt()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => 143,
            _ = hup.recv() => 129,
            _ = int.recv() => 130,
        }
    }
    #[cfg(not(unix))]
    {
        match tokio::signal::ctrl_c().await {
            Ok(()) => 130,
            Err(_) => std::future::pending().await,
        }
    }
}

pub(crate) async fn run(surface: TerminalSurface) -> io::Result<i32> {
    // The background is asked for before raw mode and the alternate screen.
    let theme = Theme::detect(surface.theme);
    let session = surface.initial_session().await;
    let mut app = App::new(surface.engine.source().app_name(), session.clone());
    app.model = surface.model.clone();
    app.place = surface.place.clone();
    let mut renderer = Renderer::new(theme);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let pump = Pump::start(
        surface.engine.clone(),
        surface.caller.clone(),
        session,
        surface.poll,
        tx.clone(),
    );

    let mut stdout = io::stdout();
    enter(&mut stdout)?;
    let _guard = Guard;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    let mut events = EventStream::new();
    let signal = signalled();
    tokio::pin!(signal);
    let mut tick = tokio::time::interval(TICK);
    let mut code = EXIT_DONE;

    loop {
        let now = Instant::now();
        while let Ok(update) = rx.try_recv() {
            app.update(update, now);
        }
        terminal.draw(|frame| renderer.draw(frame, &app, now))?;
        let effects = tokio::select! {
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
            exit = &mut signal => {
                code = exit;
                vec![Effect::Quit]
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
    restore(&mut io::stdout());
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, since both parts share the process's terminal state.
    #[test]
    fn the_terminal_is_restored_once_and_on_a_panic() {
        ACTIVE.store(true, Ordering::SeqCst);
        let mut out = Vec::new();
        restore(&mut out);
        let written = String::from_utf8(out).unwrap();
        assert!(written.contains("\x1b[?1049l"), "{written:?}");
        assert!(written.contains("\x1b[?25h"), "{written:?}");
        assert!(written.contains("\x1b[?2004l"), "{written:?}");
        let mut again = Vec::new();
        restore(&mut again);
        assert!(again.is_empty());

        // A panic anywhere while the full screen is up gives the terminal back first.
        restore_on_panic();
        ACTIVE.store(true, Ordering::SeqCst);
        let panicked = std::thread::spawn(|| panic!("a bug while drawing")).join();
        assert!(panicked.is_err());
        assert!(!ACTIVE.load(Ordering::SeqCst), "the panic hook restored the terminal");
    }
}
