//! What the full-screen and inline terminals change on the terminal, and how it is given back
//! (spec 3.3 §5.2): on exit, on a panic in the terminal's own code, and on a signal.

use std::cell::Cell;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};

/// The full screen holds raw mode, the alternate screen and bracketed paste.
static FULL: AtomicBool = AtomicBool::new(false);

/// The inline terminal holds raw mode and bracketed paste.
static INLINE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The terminal's own code is running on this thread: a panic here ends the terminal.
    static IN_UI: Cell<bool> = const { Cell::new(false) };
}

/// Gives back what [`enter_full`] or [`enter_inline`] took, once; safe to call from a panic
/// hook.
pub(crate) fn restore(out: &mut impl Write) {
    if FULL.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableBracketedPaste, LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
        let _ = out.flush();
    }
    if INLINE.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = out.flush();
    }
}

/// Installs, once per process, a panic hook that gives the terminal back before the message
/// of a panic in the terminal's own code prints. A panic another task or thread catches — a
/// tool's, which the agent reports as an error and goes on — leaves the terminal as it is,
/// since the terminal goes on too.
fn restore_on_panic() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if IN_UI.with(Cell::get) {
                restore(&mut io::stdout());
            }
            previous(info);
        }));
    });
}

/// Takes the terminal for the full screen: raw mode, the alternate screen, bracketed paste.
pub(crate) fn enter_full(out: &mut impl Write) -> io::Result<()> {
    restore_on_panic();
    enable_raw_mode()?;
    FULL.store(true, Ordering::SeqCst);
    if let Err(error) = execute!(out, EnterAlternateScreen, EnableBracketedPaste) {
        restore(out);
        return Err(error);
    }
    Ok(())
}

/// Takes the terminal for the inline UI: raw mode and bracketed paste, so a paste arrives as
/// one paste and never as keys that could answer a decision (§7.4 rule 2, S-I4).
pub(crate) fn enter_inline(out: &mut impl Write) -> io::Result<()> {
    restore_on_panic();
    enable_raw_mode()?;
    INLINE.store(true, Ordering::SeqCst);
    if let Err(error) = execute!(out, EnableBracketedPaste) {
        restore(out);
        return Err(error);
    }
    Ok(())
}

/// Gives the terminal back when the UI ends, however it ends.
pub(crate) struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        restore(&mut io::stdout());
    }
}

/// The terminal's future, polled as the terminal's own code: a panic inside it gives the
/// terminal back first.
pub(crate) struct Scoped<F>(Pin<Box<F>>);

/// `future` as the terminal's own code.
pub(crate) fn scoped<F: Future>(future: F) -> Scoped<F> {
    Scoped(Box::pin(future))
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        /// Puts back what the thread was running, also when the poll unwinds.
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                IN_UI.with(|cell| cell.set(self.0));
            }
        }
        let _reset = Reset(IN_UI.with(|cell| cell.replace(true)));
        self.0.as_mut().poll(cx)
    }
}

/// The signals that end the terminal, registered before it changes anything, so a signal at
/// any moment gives it back: `SIGTERM`, `SIGHUP` and `SIGINT` (Ctrl-C elsewhere).
pub(crate) struct Signals {
    #[cfg(unix)]
    registered: Option<(
        tokio::signal::unix::Signal,
        tokio::signal::unix::Signal,
        tokio::signal::unix::Signal,
    )>,
    #[cfg(windows)]
    registered: Option<tokio::signal::windows::CtrlC>,
}

impl Signals {
    /// Registers the signals now, or none when `enabled` is false.
    pub(crate) fn install(enabled: bool) -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let registered = enabled
                .then(|| {
                    Some((
                        signal(SignalKind::terminate()).ok()?,
                        signal(SignalKind::hangup()).ok()?,
                        signal(SignalKind::interrupt()).ok()?,
                    ))
                })
                .flatten();
            Self { registered }
        }
        #[cfg(windows)]
        {
            let registered = enabled.then(|| tokio::signal::windows::ctrl_c().ok()).flatten();
            Self { registered }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = enabled;
            Self {}
        }
    }

    /// The next signal, as the exit code it ends the terminal with.
    pub(crate) async fn recv(&mut self) -> i32 {
        #[cfg(unix)]
        if let Some((term, hup, int)) = &mut self.registered {
            return tokio::select! {
                _ = term.recv() => 143,
                _ = hup.recv() => 129,
                _ = int.recv() => 130,
            };
        }
        #[cfg(windows)]
        if let Some(ctrl_c) = &mut self.registered {
            ctrl_c.recv().await;
            return 130;
        }
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, since its parts share the process's terminal state.
    #[test]
    fn the_terminal_is_given_back_once_and_on_a_panic_of_its_own() {
        FULL.store(true, Ordering::SeqCst);
        let mut out = Vec::new();
        restore(&mut out);
        let written = String::from_utf8(out).unwrap();
        assert!(written.contains("\x1b[?1049l"), "{written:?}");
        assert!(written.contains("\x1b[?25h"), "{written:?}");
        assert!(written.contains("\x1b[?2004l"), "{written:?}");
        let mut again = Vec::new();
        restore(&mut again);
        assert!(again.is_empty());

        INLINE.store(true, Ordering::SeqCst);
        let mut out = Vec::new();
        restore(&mut out);
        let written = String::from_utf8(out).unwrap();
        assert!(written.contains("\x1b[?2004l"), "{written:?}");
        assert!(!written.contains("\x1b[?1049l"), "inline never took the alternate screen");

        // A panic another thread or task catches, such as a tool's, leaves the full screen up:
        // the UI goes on drawing on it.
        restore_on_panic();
        FULL.store(true, Ordering::SeqCst);
        let caught = std::thread::spawn(|| panic!("a tool's bug, caught")).join();
        assert!(caught.is_err());
        assert!(FULL.load(Ordering::SeqCst), "a caught panic elsewhere took the terminal back");

        // A panic in the terminal's own code gives the terminal back before its message.
        let own = std::thread::spawn(|| {
            futures::executor::block_on(scoped(async { panic!("a bug while drawing") }))
        })
        .join();
        assert!(own.is_err());
        assert!(!FULL.load(Ordering::SeqCst), "the terminal's own panic gave it back");
    }

    /// An application that keeps the signals gets none registered: their handlers would last
    /// as long as the process.
    #[cfg(any(unix, windows))]
    #[test]
    fn no_signal_is_registered_when_the_application_keeps_them() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            assert!(Signals::install(false).registered.is_none());
            assert!(Signals::install(true).registered.is_some());
        });
    }
}
