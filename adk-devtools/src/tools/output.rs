//! A command's output: its start and end in memory for the result, and all of it in a file
//! an [`OutputStore`] names (spec 3.1 §7.33 fix 8, §10 D10).

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adk_core::ToolContext;

/// The most characters of output one `bash` result carries, stdout and stderr together
/// (spec 3.1 §7.33 fix 8). A longer stream keeps its first third and its last two thirds of
/// its share, around a marker naming the bytes left out.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_devtools::INLINE_OUTPUT_CHARS, 30_000);
/// ```
pub const INLINE_OUTPUT_CHARS: usize = 30_000;

/// Bytes a stream keeps from its start and from its end in memory: enough for every inline
/// character at four bytes each.
const KEEP_BYTES: usize = INLINE_OUTPUT_CHARS * 4;

/// Output held in memory before an output file is created, so a command whose output fits
/// inline leaves no file behind.
const SPILL_AT: usize = 32 * 1024;

/// The longest last line [`OutputStore`] callers are told about.
const LAST_LINE_CHARS: usize = 200;

/// Keeps the full output of `bash` commands whose result is cut (spec 3.1 §10 D10).
///
/// The harness keeps each file in the session's scratch directory and reads it back with
/// `shell_output`; without a store, a cut result names only the bytes it left out.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolContext;
/// use adk_devtools::{OutputFile, OutputStore};
///
/// struct InDir(std::path::PathBuf);
///
/// impl OutputStore for InDir {
///     fn create(&self, ctx: &dyn ToolContext, _command: &str) -> std::io::Result<OutputFile> {
///         let id = ctx.function_call_id().to_string();
///         let path = self.0.join(format!("{id}.log"));
///         let file = std::fs::File::options().write(true).create_new(true).open(&path)?;
///         Ok(OutputFile { id, path, file })
///     }
/// }
/// ```
pub trait OutputStore: Send + Sync {
    /// Creates a new, empty file for the output of `command`, run by the call `ctx`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the file cannot be created; the result then names only
    /// the bytes it left out.
    fn create(&self, ctx: &dyn ToolContext, command: &str) -> std::io::Result<OutputFile>;
}

/// A file a command's full output goes to, as an [`OutputStore`] creates it.
#[derive(Debug)]
pub struct OutputFile {
    /// The name a caller passes back to read the file, such as `shell_output`'s `shell_id`.
    pub id: String,
    /// Where the file is; it is removed when the result turns out to need none.
    pub path: PathBuf,
    /// The file, open for writing.
    pub file: std::fs::File,
}

/// The stream a chunk of output came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// The start and end of one stream, and how many bytes it had.
#[derive(Default)]
struct Kept {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl Kept {
    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = KEEP_BYTES.saturating_sub(self.head.len()).min(bytes.len());
        let (head, rest) = bytes.split_at(room);
        self.head.extend_from_slice(head);
        let rest = &rest[rest.len().saturating_sub(KEEP_BYTES)..];
        self.tail.extend(rest);
        let over = self.tail.len().saturating_sub(KEEP_BYTES);
        self.tail.drain(..over);
    }

    /// Bytes between the head and the tail that memory did not keep.
    fn omitted(&self) -> u64 {
        self.total - self.head.len() as u64 - self.tail.len() as u64
    }

    /// The stream's characters, or `None` when memory did not keep all of it, which means
    /// more than any inline budget.
    fn chars(&self) -> Option<usize> {
        (self.omitted() == 0).then(|| {
            let (front, back) = self.tail.as_slices();
            char_count(&[self.head.as_slice(), front, back].concat())
        })
    }
}

/// Where the output goes besides memory.
enum Spill {
    /// No store: memory only.
    None,
    /// Not created yet: everything so far, in arrival order.
    Pending(Vec<u8>),
    /// Created.
    Open { file: OutputFile, written: u64, cut: bool },
    /// The store could not create the file, or it could not be written.
    Failed,
}

struct State {
    stdout: Kept,
    stderr: Kept,
    spill: Spill,
    /// Whether the file stays whatever the result needs (a background command's log).
    kept: bool,
    /// The last line of output, and the partial line after it.
    last_line: String,
    partial: Vec<u8>,
}

/// One command's output, filled by the pumps of both streams.
pub(crate) struct Collector {
    state: Mutex<State>,
    store: Option<Arc<dyn OutputStore>>,
    ctx: Arc<dyn ToolContext>,
    command: String,
    /// The most bytes the file holds: `Workspace::max_output`, the disk bound.
    cap: u64,
}

/// A rendered result: each stream within its share of [`INLINE_OUTPUT_CHARS`].
pub(crate) struct Rendered {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) truncated: bool,
    pub(crate) output_id: Option<String>,
}

impl Collector {
    pub(crate) fn new(
        store: Option<Arc<dyn OutputStore>>,
        ctx: Arc<dyn ToolContext>,
        command: &str,
        cap: usize,
    ) -> Self {
        let spill = if store.is_some() { Spill::Pending(Vec::new()) } else { Spill::None };
        Self {
            state: Mutex::new(State {
                stdout: Kept::default(),
                stderr: Kept::default(),
                spill,
                kept: false,
                last_line: String::new(),
                partial: Vec::new(),
            }),
            store,
            ctx,
            command: command.to_string(),
            cap: cap as u64,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes `bytes` read from `stream`.
    pub(crate) fn push(&self, stream: Stream, bytes: &[u8]) {
        let mut state = self.lock();
        match stream {
            Stream::Stdout => state.stdout.push(bytes),
            Stream::Stderr => state.stderr.push(bytes),
        }
        note_last_line(&mut state, bytes);
        let spill_now = matches!(&state.spill, Spill::Pending(pending) if pending.len() + bytes.len() > SPILL_AT);
        match &mut state.spill {
            Spill::Pending(pending) if !spill_now => pending.extend_from_slice(bytes),
            Spill::Pending(_) => {
                self.open(&mut state);
                self.write(&mut state, bytes);
            }
            Spill::Open { .. } => self.write(&mut state, bytes),
            Spill::None | Spill::Failed => {}
        }
    }

    /// Creates the file and writes what was pending.
    fn open(&self, state: &mut State) {
        let Spill::Pending(pending) = std::mem::replace(&mut state.spill, Spill::Failed) else {
            return;
        };
        let Some(store) = &self.store else { return };
        match store.create(self.ctx.as_ref(), &self.command) {
            Ok(file) => {
                state.spill = Spill::Open { file, written: 0, cut: false };
                self.write(state, &pending);
            }
            Err(error) => {
                tracing::warn!(error = %error, "could not create the file for a command's output");
            }
        }
    }

    fn write(&self, state: &mut State, bytes: &[u8]) {
        let Spill::Open { file, written, cut } = &mut state.spill else { return };
        let room = usize::try_from(self.cap.saturating_sub(*written)).unwrap_or(usize::MAX);
        let take = room.min(bytes.len());
        if take < bytes.len() {
            *cut = true;
        }
        if take == 0 {
            return;
        }
        match file.file.write_all(&bytes[..take]) {
            Ok(()) => *written += take as u64,
            Err(error) => {
                tracing::warn!(error = %error, output.id = %file.id, "could not write a command's output");
                *cut = true;
                *written = self.cap;
            }
        }
    }

    /// Bytes read from both streams so far.
    pub(crate) fn total(&self) -> u64 {
        let state = self.lock();
        state.stdout.total + state.stderr.total
    }

    /// The last line of output so far, at most 200 characters.
    pub(crate) fn last_line(&self) -> Option<String> {
        let state = self.lock();
        let partial = String::from_utf8_lossy(&state.partial);
        let line =
            if partial.trim().is_empty() { state.last_line.as_str() } else { partial.trim() };
        (!line.is_empty()).then(|| line.chars().take(LAST_LINE_CHARS).collect())
    }

    /// Keeps the output in a file from now on, whatever the result needs, and names it.
    pub(crate) fn keep(&self) -> std::io::Result<String> {
        let mut state = self.lock();
        state.kept = true;
        if matches!(state.spill, Spill::Pending(_)) {
            self.open(&mut state);
        }
        match &state.spill {
            Spill::Open { file, .. } => Ok(file.id.clone()),
            Spill::None => Err(std::io::Error::other("no output store is configured")),
            Spill::Pending(_) | Spill::Failed => {
                Err(std::io::Error::other("the output file could not be created"))
            }
        }
    }

    /// The streams as the result carries them, each within its share of
    /// [`INLINE_OUTPUT_CHARS`]. A cut result names the file that holds the whole output; a
    /// file the result does not need is removed.
    pub(crate) fn render(&self) -> Rendered {
        let mut state = self.lock();
        let (out_share, err_share) = shares(state.stdout.chars(), state.stderr.chars());
        let cut = state.stdout.chars().is_none_or(|chars| chars > out_share)
            || state.stderr.chars().is_none_or(|chars| chars > err_share);
        if cut && matches!(state.spill, Spill::Pending(_)) {
            self.open(&mut state);
        }
        let output = match &state.spill {
            Spill::Open { file, cut: file_cut, .. } if cut || state.kept => {
                let _ = file.file.sync_data();
                Some((file.id.clone(), *file_cut))
            }
            _ => None,
        };
        if !cut
            && !state.kept
            && let Spill::Open { file, .. } = &state.spill
            && let Err(error) = std::fs::remove_file(&file.path)
        {
            tracing::debug!(error = %error, "could not remove an unneeded output file");
        }
        let marker = |left: u64| match &output {
            Some((id, false)) => format!(
                "\n…[{left} bytes left out; shell_output {{\"shell_id\": \"{id}\"}} reads the whole output]\n"
            ),
            Some((id, true)) => format!(
                "\n…[{left} bytes left out; shell_output {{\"shell_id\": \"{id}\"}} reads the first {} bytes of the output]\n",
                self.cap
            ),
            None => format!("\n…[{left} bytes left out]\n"),
        };
        let (stdout, out_cut) = render_stream(&state.stdout, out_share, &marker);
        let (stderr, err_cut) = render_stream(&state.stderr, err_share, &marker);
        Rendered {
            stdout,
            stderr,
            truncated: out_cut || err_cut,
            output_id: output.filter(|_| out_cut || err_cut).map(|(id, _)| id),
        }
    }
}

/// A finished command's output as a `bash` result carries it (spec 3.1 §7.33 fix 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedOutput {
    /// Standard output, within its share of [`INLINE_OUTPUT_CHARS`].
    pub stdout: String,
    /// Standard error, within its share.
    pub stderr: String,
    /// Whether either was cut.
    pub truncated: bool,
    /// The file that holds the whole output when either was cut and `store` kept it.
    pub output_id: Option<String>,
}

/// Bounds the whole output of a finished command as [`BashTool`](crate::BashTool) bounds
/// what it reads: at most [`INLINE_OUTPUT_CHARS`] characters, the whole output in a file
/// from `store` when cut, of at most `cap` bytes. For a runner that receives a command's
/// output at once, such as a container's.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// # struct Ctx;
/// # #[async_trait::async_trait] impl adk_core::ReadonlyContext for Ctx {
/// #     fn invocation_id(&self) -> &str { "i" } fn agent_name(&self) -> &str { "a" }
/// #     fn user_id(&self) -> &str { "u" } fn app_name(&self) -> &str { "app" }
/// #     fn session_id(&self) -> &str { "s" } fn branch(&self) -> &str { "" }
/// #     fn user_content(&self) -> &adk_core::Content { unimplemented!() }
/// # }
/// # #[async_trait::async_trait] impl adk_core::CallbackContext for Ctx {
/// #     fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
/// # }
/// # #[async_trait::async_trait] impl adk_core::ToolContext for Ctx {
/// #     fn function_call_id(&self) -> &str { "c" }
/// #     fn actions(&self) -> adk_core::EventActions { Default::default() }
/// #     fn set_actions(&self, _: adk_core::EventActions) {}
/// #     async fn search_memory(&self, _: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> { Ok(vec![]) }
/// # }
/// let long = "x".repeat(40_000);
/// let bounded = adk_devtools::bound_output(None, Arc::new(Ctx), "make", long.as_bytes(), b"", 1 << 20);
/// assert!(bounded.truncated);
/// assert!(bounded.stdout.contains("[10000 bytes left out]"));
/// ```
pub fn bound_output(
    store: Option<Arc<dyn OutputStore>>,
    ctx: Arc<dyn ToolContext>,
    command: &str,
    stdout: &[u8],
    stderr: &[u8],
    cap: usize,
) -> BoundedOutput {
    let collector = Collector::new(store, ctx, command, cap);
    collector.push(Stream::Stdout, stdout);
    collector.push(Stream::Stderr, stderr);
    let Rendered { stdout, stderr, truncated, output_id } = collector.render();
    BoundedOutput { stdout, stderr, truncated, output_id }
}

/// Records the last complete, non-blank line of `bytes`, keeping the partial line after it.
fn note_last_line(state: &mut State, bytes: &[u8]) {
    state.partial.extend_from_slice(bytes);
    if let Some(end) = state.partial.iter().rposition(|&byte| byte == b'\n') {
        let rest = state.partial.split_off(end + 1);
        if let Some(line) = String::from_utf8_lossy(&state.partial)
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
        {
            state.last_line = line.chars().take(LAST_LINE_CHARS).collect();
        }
        state.partial = rest;
    }
    // A line longer than anything shown is cut to its start.
    if state.partial.len() > LAST_LINE_CHARS * 4 {
        state.partial.truncate(LAST_LINE_CHARS * 4);
    }
}

/// Each stream's share of [`INLINE_OUTPUT_CHARS`], from its characters (`None`: more than
/// any share). A stream within half of it keeps all of it, and the other stream takes the
/// rest.
fn shares(stdout: Option<usize>, stderr: Option<usize>) -> (usize, usize) {
    const HALF: usize = INLINE_OUTPUT_CHARS / 2;
    match (stdout, stderr) {
        (Some(out), Some(err)) if out + err <= INLINE_OUTPUT_CHARS => (out, err),
        (_, Some(err)) if err <= HALF => (INLINE_OUTPUT_CHARS - err, err),
        (Some(out), _) if out <= HALF => (out, INLINE_OUTPUT_CHARS - out),
        _ => (HALF, HALF),
    }
}

/// `kept` within `share` characters: whole when it fits, else its first third and last two
/// thirds around `marker`, which names the bytes left out. Returns whether it was cut.
fn render_stream(kept: &Kept, share: usize, marker: &dyn Fn(u64) -> String) -> (String, bool) {
    let head_chars = share / 3;
    let tail_chars = share - head_chars;
    let (front, back) = kept.tail.as_slices();
    if kept.omitted() == 0 {
        let bytes = [kept.head.as_slice(), front, back].concat();
        if char_count(&bytes) <= share {
            return (String::from_utf8_lossy(&bytes).into_owned(), false);
        }
        let end = prefix_len(&bytes, head_chars);
        let start = suffix_start(&bytes, tail_chars).max(end);
        let text = format!(
            "{}{}{}",
            String::from_utf8_lossy(&bytes[..end]),
            marker((start - end) as u64),
            String::from_utf8_lossy(&bytes[start..])
        );
        return (text, true);
    }
    let tail = [front, back].concat();
    // The tail can begin inside a character that the bytes memory dropped began.
    let skipped = tail.iter().take(3).take_while(|&&byte| byte & 0xC0 == 0x80).count();
    let tail = &tail[skipped..];
    let end = prefix_len(&kept.head, head_chars);
    let start = suffix_start(tail, tail_chars);
    let left = (kept.head.len() - end) as u64 + kept.omitted() + (skipped + start) as u64;
    let text = format!(
        "{}{}{}",
        String::from_utf8_lossy(&kept.head[..end]),
        marker(left),
        String::from_utf8_lossy(&tail[start..])
    );
    (text, true)
}

/// The characters of `bytes` decoded lossily: each invalid sequence is one character.
fn char_count(bytes: &[u8]) -> usize {
    bytes
        .utf8_chunks()
        .map(|chunk| chunk.valid().chars().count() + usize::from(!chunk.invalid().is_empty()))
        .sum()
}

/// The bytes of the first `chars` characters of `bytes` decoded lossily.
fn prefix_len(bytes: &[u8], chars: usize) -> usize {
    let mut left = chars;
    let mut at = 0;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid();
        if let Some((index, _)) = valid.char_indices().nth(left) {
            return at + index;
        }
        left -= valid.chars().count();
        at += valid.len();
        if !chunk.invalid().is_empty() {
            if left == 0 {
                return at;
            }
            left -= 1;
            at += chunk.invalid().len();
        }
    }
    at
}

/// Where the last `chars` characters of `bytes`, decoded lossily, begin.
fn suffix_start(bytes: &[u8], chars: usize) -> usize {
    let total = char_count(bytes);
    if chars >= total { 0 } else { prefix_len(bytes, total - chars) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(bytes: &[u8]) -> Kept {
        let mut kept = Kept::default();
        for chunk in bytes.chunks(8192) {
            kept.push(chunk);
        }
        kept
    }

    fn marker(left: u64) -> String {
        format!("[{left}]")
    }

    #[test]
    fn output_within_its_share_is_unchanged() {
        let (text, cut) = render_stream(&kept("héllo\n".as_bytes()), 100, &marker);
        assert_eq!((text.as_str(), cut), ("héllo\n", false));
    }

    #[test]
    fn a_long_stream_keeps_its_first_third_and_last_two_thirds() {
        let text: String = (0..1000).map(|n| format!("{n:04}\n")).collect();
        let (rendered, cut) = render_stream(&kept(text.as_bytes()), 30, &marker);
        // 10 characters from the start, 20 from the end, 5000 - 30 bytes between.
        assert_eq!(rendered, "0000\n0001\n[4970]0996\n0997\n0998\n0999\n");
        assert!(cut);
    }

    #[test]
    fn cuts_fall_on_character_boundaries() {
        // `€` is three bytes: 30 characters of 90 bytes, cut to 3 and 6.
        let text = "€".repeat(30);
        let (rendered, cut) = render_stream(&kept(text.as_bytes()), 9, &marker);
        assert_eq!(rendered, format!("€€€[63]{}", "€".repeat(6)));
        assert!(cut);
    }

    #[test]
    fn a_stream_larger_than_memory_names_every_byte_it_left_out() {
        let total = 5 * 1024 * 1024;
        let bytes: Vec<u8> = (0..total).map(|i| b'a' + (i % 26) as u8).collect();
        let kept = kept(&bytes);
        assert_eq!(kept.chars(), None);
        let (rendered, cut) = render_stream(&kept, INLINE_OUTPUT_CHARS, &marker);
        let head = String::from_utf8_lossy(&bytes[..10_000]);
        let tail = String::from_utf8_lossy(&bytes[total - 20_000..]);
        assert_eq!(rendered, format!("{head}[{}]{tail}", total - 30_000));
        assert!(cut);
    }

    #[test]
    fn a_tail_starting_inside_a_character_drops_its_rest() {
        // The trailing `b` puts the start of the kept tail one byte into a three-byte `€`.
        let mut bytes = vec![b'a'; KEEP_BYTES + 1];
        bytes.extend("€".repeat(KEEP_BYTES / 3 + 1).as_bytes());
        bytes.push(b'b');
        let kept = kept(&bytes);
        let (rendered, _) = render_stream(&kept, 9, &marker);
        assert!(rendered.starts_with("aaa[") && rendered.ends_with("]€€€€€b"), "{rendered}");
        assert!(!rendered.contains('\u{FFFD}'), "{rendered}");
        let left: u64 = rendered[3..].split(['[', ']']).nth(1).unwrap().parse().unwrap();
        assert_eq!(left, bytes.len() as u64 - 3 - 16, "every byte not shown is counted");
    }

    #[test]
    fn invalid_utf8_counts_one_character_per_sequence() {
        // `0xFF` and `0xFE` begin no sequence, so each is one replacement character.
        let bytes = [b'a', 0xFF, 0xFE, b'b', b'c'];
        assert_eq!(char_count(&bytes), 5);
        assert_eq!(char_count(&[b'a', 0xE2, 0x82, b'b']), 3, "a cut-off `€` is one sequence");
        assert_eq!(prefix_len(&bytes, 2), 2);
        assert_eq!(suffix_start(&bytes, 2), 3);
    }

    #[test]
    fn a_small_stream_keeps_all_and_the_other_takes_the_rest() {
        assert_eq!(shares(Some(10), Some(20)), (10, 20));
        assert_eq!(shares(None, Some(100)), (INLINE_OUTPUT_CHARS - 100, 100));
        assert_eq!(shares(Some(100), None), (100, INLINE_OUTPUT_CHARS - 100));
        assert_eq!(shares(None, None), (15_000, 15_000));
        assert_eq!(shares(Some(20_000), Some(20_000)), (15_000, 15_000));
    }
}
