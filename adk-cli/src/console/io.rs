//! One reader for the terminal, shared by the REPL and every prompt (spec 3.1 §8.1).

use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};

use adk_core::{AdkError, Result};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// What one [`ConsoleIo::read_line`] returned.
///
/// Spec 3.1 §8.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleInput {
    /// A line the person entered, without its line ending.
    Line(String),
    /// The person pressed Ctrl-C at the prompt.
    Interrupted,
    /// Input ended (Ctrl-D, a closed stdin, or a terminal error).
    Eof,
}

/// The console's single line reader, shared by the REPL and every prompt (spec 3.1 §8.1).
///
/// A terminal console owns one `rustyline` editor on a dedicated thread. Every read, from
/// the REPL or from a prompt such as [`ConsoleConfirmationHandler`](super::ConsoleConfirmationHandler),
/// is a request to that thread, served in order. No second reader ever blocks on stdin,
/// so Ctrl-C at a prompt cannot leave a read behind that would swallow the next REPL line.
/// A prompt abandoned while the person is typing gets its line discarded, never routed to
/// another reader.
///
/// The console also tracks the run it drives: [`interrupt_run`](Self::interrupt_run)
/// cancels it, which is how Ctrl-C at an approval prompt stops the run.
///
/// [`scripted`](Self::scripted) builds a console that answers from a list and records
/// what was shown, for tests.
///
/// # Example
///
/// ```rust
/// use adk_cli::console::{ConsoleInput, ConsoleIo};
///
/// # futures::executor::block_on(async {
/// let console = ConsoleIo::scripted([ConsoleInput::Line("hello".into())], true);
/// assert_eq!(console.read_line("You > ").await, ConsoleInput::Line("hello".into()));
/// assert_eq!(console.read_line("You > ").await, ConsoleInput::Eof);
/// assert_eq!(console.transcript(), "You > hello\nYou > ");
/// # });
/// ```
#[derive(Clone)]
pub struct ConsoleIo {
    inner: Arc<Inner>,
}

struct Inner {
    backend: Backend,
    run: Mutex<Option<CancellationToken>>,
}

enum Backend {
    Terminal { requests: mpsc::Sender<Request>, is_terminal: bool },
    Scripted { inputs: Mutex<VecDeque<ConsoleInput>>, transcript: Mutex<String>, terminal: bool },
}

enum Request {
    Read { prompt: String, reply: oneshot::Sender<ConsoleInput> },
    History(String),
}

impl std::fmt::Debug for ConsoleIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.inner.backend {
            Backend::Terminal { .. } => "terminal",
            Backend::Scripted { .. } => "scripted",
        };
        f.debug_struct("ConsoleIo").field("backend", &kind).finish_non_exhaustive()
    }
}

static TERMINAL: OnceLock<std::result::Result<ConsoleIo, String>> = OnceLock::new();

impl ConsoleIo {
    /// The process's terminal console.
    ///
    /// Every call returns a handle to the same reader thread, started on the first call,
    /// since a process has one terminal.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the line editor cannot be initialised.
    pub fn terminal() -> Result<Self> {
        TERMINAL
            .get_or_init(start_terminal)
            .clone()
            .map_err(|message| AdkError::config(format!("failed to init readline: {message}")))
    }

    /// A console that answers reads from `inputs`, in order, then reports
    /// [`ConsoleInput::Eof`].
    ///
    /// `terminal` is what [`is_terminal`](Self::is_terminal) reports. Prompts, the lines
    /// read and everything [`print`](Self::print)ed are recorded in the
    /// [`transcript`](Self::transcript).
    pub fn scripted(inputs: impl IntoIterator<Item = ConsoleInput>, terminal: bool) -> Self {
        Self::with_backend(Backend::Scripted {
            inputs: Mutex::new(inputs.into_iter().collect()),
            transcript: Mutex::new(String::new()),
            terminal,
        })
    }

    fn with_backend(backend: Backend) -> Self {
        Self { inner: Arc::new(Inner { backend, run: Mutex::new(None) }) }
    }

    /// Whether a person can answer prompts: stdin is a terminal.
    pub fn is_terminal(&self) -> bool {
        match &self.inner.backend {
            Backend::Terminal { is_terminal, .. } => *is_terminal,
            Backend::Scripted { terminal, .. } => *terminal,
        }
    }

    /// Whether an external editor can take over the terminal: a real terminal, never a
    /// scripted console.
    pub(crate) fn can_open_editor(&self) -> bool {
        matches!(self.inner.backend, Backend::Terminal { is_terminal: true, .. })
    }

    /// Shows `prompt` and reads one line.
    ///
    /// Reads are served one at a time, in the order they were asked for. Dropping the
    /// returned future gives up the read; a line typed for it afterwards is discarded.
    pub async fn read_line(&self, prompt: &str) -> ConsoleInput {
        match &self.inner.backend {
            Backend::Terminal { requests, .. } => {
                let (reply, answer) = oneshot::channel();
                if requests.send(Request::Read { prompt: prompt.to_string(), reply }).is_err() {
                    return ConsoleInput::Eof;
                }
                answer.await.unwrap_or(ConsoleInput::Eof)
            }
            Backend::Scripted { inputs, transcript, .. } => {
                let input = lock(inputs).pop_front().unwrap_or(ConsoleInput::Eof);
                let mut transcript = lock(transcript);
                transcript.push_str(prompt);
                match &input {
                    ConsoleInput::Line(line) => {
                        transcript.push_str(line);
                        transcript.push('\n');
                    }
                    ConsoleInput::Interrupted => transcript.push_str("^C\n"),
                    ConsoleInput::Eof => {}
                }
                input
            }
        }
    }

    /// Adds `line` to the editor's history, so the Up arrow recalls it.
    pub fn add_history(&self, line: &str) {
        if let Backend::Terminal { requests, .. } = &self.inner.backend {
            let _ = requests.send(Request::History(line.to_string()));
        }
    }

    /// Writes `text` to the console as given.
    ///
    /// Escape model- or tool-controlled text before passing it; see
    /// [`StreamPrinter`](crate::launcher::StreamPrinter).
    pub fn print(&self, text: &str) {
        match &self.inner.backend {
            Backend::Terminal { .. } => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
            }
            Backend::Scripted { transcript, .. } => lock(transcript).push_str(text),
        }
    }

    /// Everything a scripted console showed and read. Empty for the terminal.
    pub fn transcript(&self) -> String {
        match &self.inner.backend {
            Backend::Terminal { .. } => String::new(),
            Backend::Scripted { transcript, .. } => lock(transcript).clone(),
        }
    }

    /// Records the run the console is driving, or `None` once it ends.
    pub fn set_run(&self, run: Option<CancellationToken>) {
        *lock(&self.inner.run) = run;
    }

    /// Cancels the run the console is driving, if any.
    pub fn interrupt_run(&self) {
        if let Some(run) = lock(&self.inner.run).as_ref() {
            run.cancel();
        }
    }

    /// Whether the run the console is driving was interrupted.
    pub fn run_interrupted(&self) -> bool {
        lock(&self.inner.run).as_ref().is_some_and(CancellationToken::is_cancelled)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn start_terminal() -> std::result::Result<ConsoleIo, String> {
    let (requests, queue) = mpsc::channel::<Request>();
    let (ready, started) = mpsc::sync_channel::<std::result::Result<(), String>>(1);
    std::thread::Builder::new()
        .name("adk-console".into())
        .spawn(move || {
            let editor = match DefaultEditor::new() {
                Ok(editor) => {
                    let _ = ready.send(Ok(()));
                    editor
                }
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            serve_reads(editor, &queue);
        })
        .map_err(|error| error.to_string())?;
    started.recv().map_err(|error| error.to_string())??;
    Ok(ConsoleIo::with_backend(Backend::Terminal {
        requests,
        is_terminal: std::io::stdin().is_terminal(),
    }))
}

fn serve_reads(mut editor: DefaultEditor, queue: &mpsc::Receiver<Request>) {
    while let Ok(request) = queue.recv() {
        match request {
            Request::Read { prompt, reply } => {
                // A prompt given up before its turn came is never shown.
                if reply.is_closed() {
                    continue;
                }
                let input = match editor.readline(&prompt) {
                    Ok(line) => ConsoleInput::Line(line),
                    Err(ReadlineError::Interrupted) => ConsoleInput::Interrupted,
                    Err(ReadlineError::Eof) => ConsoleInput::Eof,
                    Err(error) => {
                        tracing::error!(error = %error, "readline error");
                        ConsoleInput::Eof
                    }
                };
                if let Err(ConsoleInput::Line(_)) = reply.send(input) {
                    let mut out = std::io::stdout().lock();
                    let _ = out.write_all(b"(that prompt had already closed; input ignored)\n");
                    let _ = out.flush();
                }
            }
            Request::History(line) => {
                let _ = editor.add_history_entry(line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scripted_console_answers_in_order_then_reports_eof() {
        let console = ConsoleIo::scripted(
            [ConsoleInput::Line("one".into()), ConsoleInput::Interrupted],
            false,
        );
        assert!(!console.is_terminal());
        assert_eq!(console.read_line("> ").await, ConsoleInput::Line("one".into()));
        assert_eq!(console.read_line("> ").await, ConsoleInput::Interrupted);
        assert_eq!(console.read_line("> ").await, ConsoleInput::Eof);
        console.print("done\n");
        assert_eq!(console.transcript(), "> one\n> ^C\n> done\n");
    }

    #[test]
    fn interrupting_cancels_only_the_run_being_driven() {
        let console = ConsoleIo::scripted([], true);
        console.interrupt_run();
        assert!(!console.run_interrupted());

        let run = CancellationToken::new();
        console.set_run(Some(run.clone()));
        console.clone().interrupt_run();
        assert!(run.is_cancelled());
        assert!(console.run_interrupted());

        console.set_run(None);
        assert!(!console.run_interrupted());
    }
}
