//! One reader for the terminal, shared by the REPL and every prompt (spec 3.1 §8.1).

use std::collections::{HashSet, VecDeque};
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};

use adk_core::{AdkError, Result};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::complete::{Completions, ConsoleHelper};

/// The terminal's line editor: `rustyline` with the console's completer.
type ConsoleEditor = Editor<ConsoleHelper, DefaultHistory>;

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
/// Tab completes slash commands at the start of a line and `@server:uri` mentions
/// anywhere in it, from the words the surface sets with
/// [`set_command_completions`](Self::set_command_completions) and
/// [`set_mention_completions`](Self::set_mention_completions) (spec 3.1 §7.21, §8.1).
///
/// A prompter deciding a call inside a run that another task drives — a durable goal's
/// worker — would print before the console shows the call, so it prints through
/// [`print_for_call`](Self::print_for_call), which holds the text until the event reader
/// reports the call's line with [`call_shown`](Self::call_shown) (spec 3.1 §8.1).
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
    /// A person denied a call: the turn ends once the call's response is recorded.
    end_turn: AtomicBool,
    /// What a prompter printed for a call, held until the call's line is shown.
    call_prints: Mutex<CallPrints>,
    /// The words Tab completes.
    completions: Arc<Completions>,
}

/// Writes through [`ConsoleIo::print`]; text that is not UTF-8 is printed lossily.
pub(crate) struct ConsoleWriter(ConsoleIo);

impl Write for ConsoleWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.print(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Text printed for calls whose line the event reader has not shown yet (spec 3.1 §8.1).
#[derive(Default)]
struct CallPrints {
    /// Calls whose line the reader showed in this run.
    shown: HashSet<String>,
    /// `(call id, text)` waiting for the call's line, in print order.
    pending: Vec<(String, String)>,
    /// Calls whose decision a prompter printed, so their response does not print it again.
    decided: HashSet<String>,
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
        Self::with_backend(
            Backend::Scripted {
                inputs: Mutex::new(inputs.into_iter().collect()),
                transcript: Mutex::new(String::new()),
                terminal,
            },
            Arc::new(Completions::default()),
        )
    }

    fn with_backend(backend: Backend, completions: Arc<Completions>) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                run: Mutex::new(None),
                end_turn: AtomicBool::new(false),
                call_prints: Mutex::new(CallPrints::default()),
                completions,
            }),
        }
    }

    /// Offers `commands`, each with its slash, for Tab completion at the start of a line.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::console::ConsoleIo;
    ///
    /// let console = ConsoleIo::scripted([], true);
    /// console.set_command_completions(["/mode".to_string(), "/mcp".to_string()]);
    /// assert_eq!(console.completions_for("/mo", 3), (0, vec!["/mode".to_string()]));
    /// ```
    pub fn set_command_completions(&self, commands: impl IntoIterator<Item = String>) {
        self.inner.completions.set_commands(commands.into_iter().collect());
    }

    /// Offers `mentions` (`@server:uri`) for Tab completion anywhere in a line
    /// (spec 3.1 §7.21).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::console::ConsoleIo;
    ///
    /// let console = ConsoleIo::scripted([], true);
    /// console.set_mention_completions(["@docs:file:///guide.md".to_string()]);
    /// assert_eq!(
    ///     console.completions_for("read @docs:", 11),
    ///     (5, vec!["@docs:file:///guide.md".to_string()])
    /// );
    /// ```
    pub fn set_mention_completions(&self, mentions: impl IntoIterator<Item = String>) {
        self.inner.completions.set_mentions(mentions.into_iter().collect());
    }

    /// What Tab offers for `line` with the cursor at byte `pos`: the start of the word
    /// being completed and the candidates that extend it.
    pub fn completions_for(&self, line: &str, pos: usize) -> (usize, Vec<String>) {
        self.inner.completions.complete(line, pos)
    }

    /// Prints `text` for the call `call_id`: at once when the event reader has shown the
    /// call's line (or when the text belongs to no call), otherwise once it does
    /// ([`call_shown`](Self::call_shown)), so a decision printed inside a run another task
    /// drives follows the call it belongs to (spec 3.1 §8.1).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::console::ConsoleIo;
    ///
    /// let console = ConsoleIo::scripted([], true);
    /// console.print_for_call(Some("call-1"), "[approved by --yes] bash\n");
    /// assert_eq!(console.transcript(), "");
    /// console.call_shown("call-1");
    /// assert_eq!(console.transcript(), "[approved by --yes] bash\n");
    /// ```
    pub fn print_for_call(&self, call_id: Option<&str>, text: &str) {
        let Some(id) = call_id else {
            return self.print(text);
        };
        let waiting = {
            let mut prints = lock(&self.inner.call_prints);
            if prints.shown.contains(id) {
                false
            } else {
                prints.pending.push((id.to_string(), text.to_string()));
                true
            }
        };
        if !waiting {
            self.print(text);
        }
    }

    /// Prints the decision a prompter took for the call `call_id`, as
    /// [`print_for_call`](Self::print_for_call) does, and records it, so the call's
    /// response does not print the decision again after the call's output (spec 3.1
    /// §7.33 fix 2a).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::console::ConsoleIo;
    ///
    /// let console = ConsoleIo::scripted([], true);
    /// console.call_shown("call-1");
    /// console.print_decision(Some("call-1"), "[approved] bash\n");
    /// assert_eq!(console.transcript(), "[approved] bash\n");
    /// ```
    pub fn print_decision(&self, call_id: Option<&str>, text: &str) {
        if let Some(id) = call_id {
            lock(&self.inner.call_prints).decided.insert(id.to_string());
        }
        self.print_for_call(call_id, text);
    }

    /// Whether a prompter printed the decision for the call `call_id`, forgetting it.
    pub(crate) fn take_printed_decision(&self, call_id: &str) -> bool {
        lock(&self.inner.call_prints).decided.remove(call_id)
    }

    /// Records that the event reader showed the line of call `call_id` and prints what
    /// was waiting for it, in the order it was printed.
    pub fn call_shown(&self, call_id: &str) {
        let texts: Vec<String> = {
            let mut prints = lock(&self.inner.call_prints);
            prints.shown.insert(call_id.to_string());
            let pending = std::mem::take(&mut prints.pending);
            let (mine, rest): (Vec<_>, Vec<_>) =
                pending.into_iter().partition(|(id, _)| id == call_id);
            prints.pending = rest;
            mine.into_iter().map(|(_, text)| text).collect()
        };
        for text in texts {
            self.print(&text);
        }
    }

    /// Prints what still waits for a call whose line never came, and forgets the run's
    /// shown calls; the event reader calls it when a run ends.
    pub fn flush_call_prints(&self) {
        let pending = {
            let mut prints = lock(&self.inner.call_prints);
            prints.shown.clear();
            prints.decided.clear();
            std::mem::take(&mut prints.pending)
        };
        for (_, text) in pending {
            self.print(&text);
        }
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

    /// A writer that prints through [`print`](Self::print), so a printer's output and the
    /// console's own lines reach the terminal, or a scripted console's transcript, in the
    /// order they were written.
    pub(crate) fn writer(&self) -> ConsoleWriter {
        ConsoleWriter(self.clone())
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
        self.inner.end_turn.store(false, Ordering::SeqCst);
        *lock(&self.inner.run) = run;
    }

    /// Asks the console to end the run it drives once the decided calls' responses are
    /// recorded, before the next model call (spec 3.1 §9.2 I19).
    #[cfg(feature = "harness")]
    pub(crate) fn end_turn_after_responses(&self) {
        self.inner.end_turn.store(true, Ordering::SeqCst);
    }

    /// Whether [`end_turn_after_responses`](Self::end_turn_after_responses) was asked for
    /// since the run started.
    pub(crate) fn end_turn_requested(&self) -> bool {
        self.inner.end_turn.load(Ordering::SeqCst)
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
    let completions = Arc::new(Completions::default());
    let helper = ConsoleHelper::new(completions.clone());
    std::thread::Builder::new()
        .name("adk-console".into())
        .spawn(move || {
            let mut editor = match ConsoleEditor::new() {
                Ok(editor) => {
                    let _ = ready.send(Ok(()));
                    editor
                }
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            editor.set_helper(Some(helper));
            serve_reads(editor, &queue);
        })
        .map_err(|error| error.to_string())?;
    started.recv().map_err(|error| error.to_string())??;
    Ok(ConsoleIo::with_backend(
        Backend::Terminal { requests, is_terminal: std::io::stdin().is_terminal() },
        completions,
    ))
}

fn serve_reads(mut editor: ConsoleEditor, queue: &mpsc::Receiver<Request>) {
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
    fn a_print_for_a_call_waits_for_the_calls_line_and_keeps_its_order() {
        let console = ConsoleIo::scripted([], true);
        console.print_for_call(Some("c1"), "[approved by --yes] write_file\n");
        console.print_for_call(Some("c2"), "[approved by --yes] bash\n");
        console.print_for_call(None, "note\n");
        assert_eq!(console.transcript(), "note\n", "a print for no call goes out at once");

        console.call_shown("c2");
        assert_eq!(console.transcript(), "note\n[approved by --yes] bash\n");
        // A call already shown prints at once.
        console.print_for_call(Some("c2"), "[tool-response] bash\n");
        assert_eq!(console.transcript(), "note\n[approved by --yes] bash\n[tool-response] bash\n");

        // The run ends: what waited for a line that never came is printed, and the next
        // run's calls start unseen.
        console.flush_call_prints();
        assert!(console.transcript().ends_with("[approved by --yes] write_file\n"));
        console.print_for_call(Some("c2"), "again\n");
        assert!(!console.transcript().ends_with("again\n"));
        console.call_shown("c2");
        assert!(console.transcript().ends_with("again\n"));
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
