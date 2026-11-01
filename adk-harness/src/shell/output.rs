//! Where the harness keeps command output (spec 3.1 §7.29, §7.33 fix 8): the whole output of
//! a `bash` call whose result was cut, in `<session scratch>/shells/<id>.log`, read back with
//! `shell_output`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_devtools::{INLINE_OUTPUT_CHARS, OutputFile, OutputStore};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::background::{ShellStatus, ShellSupervisor};

/// Bytes read from a log for one answer: enough for every character of it at four bytes
/// each.
const WINDOW_BYTES: u64 = INLINE_OUTPUT_CHARS as u64 * 4;

/// The most output records kept per process; the oldest go first.
const MAX_RECORDS: usize = 4096;

/// What the harness knows about one kept output.
#[derive(Clone, Debug)]
pub(crate) struct OutputRecord {
    pub(crate) command: String,
    pub(crate) started: Instant,
    /// The exit code and how long the command ran, once it has finished.
    pub(crate) finished: Option<(Option<i32>, Duration)>,
}

/// The output files of every session of a harness: `<root>/<session key>/shells/<id>.log`,
/// outside the workspace and outside the sandbox's writable scratch (spec 3.1 §7.29), with
/// ids numbered per session.
///
/// It is the [`OutputStore`] of the harness `bash`: a call whose output does not fit its
/// result keeps the whole of it here, and `shell_output` reads it by id.
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellOutputs;
///
/// let outputs = ShellOutputs::new(std::env::temp_dir().join("adk-harness-output-doc"));
/// let dir = outputs.session_dir("app", "alice", "s1");
/// assert!(dir.ends_with("shells"));
/// assert_ne!(dir, outputs.session_dir("app", "bob", "s1"));
/// ```
#[derive(Debug)]
pub struct ShellOutputs {
    root: PathBuf,
    /// The next id per session directory.
    next: Mutex<HashMap<PathBuf, u64>>,
    records: Mutex<HashMap<(PathBuf, String), OutputRecord>>,
}

impl Default for ShellOutputs {
    /// Output under `adk-harness-output` in the system's temporary directory; on Unix,
    /// `adk-harness-output-<uid>`, so users sharing `/tmp` never share a directory.
    fn default() -> Self {
        #[cfg(unix)]
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        let name = format!("adk-harness-output-{}", unsafe { libc::geteuid() });
        #[cfg(not(unix))]
        let name = "adk-harness-output".to_string();
        Self::new(std::env::temp_dir().join(name))
    }
}

impl ShellOutputs {
    /// Output files under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), next: Mutex::default(), records: Mutex::default() }
    }

    /// The directory of (`app`, `user`, `session`)'s output files.
    pub fn session_dir(&self, app: &str, user: &str, session: &str) -> PathBuf {
        let key = crate::fsutil::sha256_hex(&format!("{app}\u{1f}{user}\u{1f}{session}"));
        self.root.join(&key[..32]).join("shells")
    }

    /// The path of output `id` in `dir`, or `None` for an id the harness never gives: ids
    /// are decimal numbers.
    fn path_of(dir: &Path, id: &str) -> Option<PathBuf> {
        (!id.is_empty() && id.len() <= 20 && id.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| dir.join(format!("{id}.log")))
    }

    /// Creates `dir` private to this user, refusing one another user owns.
    fn prepare(dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let session = dir.parent().unwrap_or(dir);
            for path in [dir, session, session.parent().unwrap_or(session)] {
                let meta = std::fs::symlink_metadata(path)?;
                // SAFETY: `geteuid` takes no arguments and cannot fail.
                let me = unsafe { libc::geteuid() };
                if !meta.is_dir() || meta.uid() != me {
                    return Err(std::io::Error::other(format!(
                        "{} is not a directory this user owns",
                        path.display()
                    )));
                }
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    /// Creates the next output file of (`app`, `user`, `session`) for `command`.
    pub(crate) fn allocate(
        &self,
        app: &str,
        user: &str,
        session: &str,
        command: &str,
    ) -> std::io::Result<OutputFile> {
        let dir = self.session_dir(app, user, session);
        Self::prepare(&dir)?;
        let mut next = self.next.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let counter = next.entry(dir.clone()).or_insert_with(|| highest_id(&dir) + 1);
        // A file an earlier process left under the same number is never reused.
        let (id, path, file) = loop {
            let id = counter.to_string();
            *counter += 1;
            let path = dir.join(format!("{id}.log"));
            let mut options = std::fs::File::options();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => break (id, path, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        };
        drop(next);
        let mut records = self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if records.len() >= MAX_RECORDS {
            let oldest =
                records.iter().min_by_key(|(_, record)| record.started).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                records.remove(&oldest);
            }
        }
        records.insert(
            (dir, id.clone()),
            OutputRecord { command: command.to_string(), started: Instant::now(), finished: None },
        );
        Ok(OutputFile { id, path, file })
    }

    /// Records that the command of output `id` finished with `exit_code` after `duration`.
    pub(crate) fn finish(
        &self,
        (app, user, session): (&str, &str, &str),
        id: &str,
        exit_code: Option<i32>,
        duration: Duration,
    ) {
        let key = (self.session_dir(app, user, session), id.to_string());
        let mut records = self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = records.get_mut(&key) {
            record.finished = Some((exit_code, duration));
        }
    }

    /// What is known about output `id` of the session.
    pub(crate) fn record(
        &self,
        (app, user, session): (&str, &str, &str),
        id: &str,
    ) -> Option<OutputRecord> {
        let key = (self.session_dir(app, user, session), id.to_string());
        self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key).cloned()
    }

    /// Reads `slice` of output `id` of the session.
    ///
    /// # Errors
    ///
    /// `NotFound` when the session has no output `id`, or the I/O error.
    pub(crate) fn read(
        &self,
        (app, user, session): (&str, &str, &str),
        id: &str,
        slice: &Slice,
    ) -> std::io::Result<OutputRead> {
        let dir = self.session_dir(app, user, session);
        let not_found =
            || std::io::Error::new(std::io::ErrorKind::NotFound, format!("no output {id}"));
        let path = Self::path_of(&dir, id).ok_or_else(not_found)?;
        // The directory is this user's own, and a link in it is never followed.
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(not_found()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
            Err(error) => return Err(error),
        }
        let mut file = std::fs::File::open(&path)?;
        let total = file.metadata()?.len();
        read_slice(&mut file, total, slice)
    }
}

impl OutputStore for ShellOutputs {
    fn create(&self, ctx: &dyn ToolContext, command: &str) -> std::io::Result<OutputFile> {
        self.allocate(ctx.app_name(), ctx.user_id(), ctx.session_id(), command)
    }
}

/// The name of the tool that reads kept output.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::SHELL_OUTPUT, "shell_output");
/// ```
pub const SHELL_OUTPUT: &str = "shell_output";

/// `shell_output { shell_id, tail_lines?, offset?, limit?, pattern? }`: reads output the
/// harness kept, by id (spec 3.1 §7.29, §7.33 fix 8).
///
/// Reads a background shell's output, or the whole output of a `bash` call whose result
/// was cut, which names an `output_id`: at most 30,000 characters per call, by default its
/// end; from `offset` the next `limit` bytes; the last `tail_lines` lines; or the lines
/// matching `pattern`, a regular expression, numbered. Reports the status, exit code and
/// duration. It reads only the calling session's output, and a background shell that ended
/// leaves the reminder once read. Class `Read`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::Tool;
/// use adk_harness::{BackgroundShellConfig, ShellOutputTool, ShellOutputs, ShellSupervisor};
///
/// let shells = ShellSupervisor::new(Arc::new(ShellOutputs::default()), BackgroundShellConfig::default());
/// let tool = ShellOutputTool::new(Arc::new(shells));
/// assert_eq!(tool.name(), "shell_output");
/// assert!(tool.is_read_only());
/// ```
#[derive(Debug)]
pub struct ShellOutputTool {
    shells: Arc<ShellSupervisor>,
}

impl ShellOutputTool {
    /// A tool reading the output `shells` keeps.
    pub fn new(shells: Arc<ShellSupervisor>) -> Self {
        Self { shells }
    }
}

fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::InvalidInput, code, message)
}

#[async_trait]
impl Tool for ShellOutputTool {
    fn name(&self) -> &str {
        SHELL_OUTPUT
    }

    /// Reads output the harness kept.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::None
    }

    fn description(&self) -> &str {
        "Read the output of a background shell by its shell_id, or the whole output of a bash \
         call whose result was cut by its output_id. Returns the status, exit code, duration \
         and at most 30,000 characters of the output: by default its end; offset and limit \
         read bytes from a position, such as the bytes a cut result left out; tail_lines reads \
         the last lines; pattern, a regular expression, returns the matching lines with their \
         numbers."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "shell_id": { "type": "string", "description": "The shell_id or output_id a bash result named." },
                "tail_lines": { "type": "integer", "minimum": 1, "description": "Read the last N lines." },
                "offset": { "type": "integer", "minimum": 0, "description": "Read from this byte offset." },
                "limit": { "type": "integer", "minimum": 1, "description": "With offset: how many bytes to read (default and most 30000 characters' worth)." },
                "pattern": { "type": "string", "description": "Return only the lines matching this regular expression." }
            },
            "required": ["shell_id"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let id = match args.get("shell_id") {
            Some(Value::String(id)) => id.trim().to_string(),
            Some(Value::Number(id)) => id.to_string(),
            _ => {
                return Err(invalid("harness.shell_output.no_id", "shell_output needs a shell_id"));
            }
        };
        let pattern = match args.get("pattern").and_then(Value::as_str) {
            Some(pattern) => Some(regex::Regex::new(pattern).map_err(|error| {
                invalid(
                    "harness.shell_output.bad_pattern",
                    format!("the pattern is not a valid regular expression: {error}"),
                )
            })?),
            None => None,
        };
        let slice = Slice {
            pattern,
            offset: args.get("offset").and_then(Value::as_u64),
            limit: args.get("limit").and_then(Value::as_u64),
            tail_lines: args
                .get("tail_lines")
                .and_then(Value::as_u64)
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX)),
        };
        let session = (ctx.app_name(), ctx.user_id(), ctx.session_id());
        let outputs = self.shells.outputs();
        let read = outputs.read(session, &id, &slice).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::NotFound,
                    "harness.shell_output.unknown",
                    format!("this session has no output {id}"),
                )
            } else {
                AdkError::tool(format!("output {id} could not be read: {error}"))
            }
        })?;
        let key = (session.0.to_string(), session.1.to_string(), session.2.to_string());
        let shell = self.shells.read(&key, &id).await;
        let record = outputs.record(session, &id);
        let mut result = json!({
            "shell_id": id,
            "total_bytes": read.total_bytes,
            "output": read.text,
        });
        if slice.pattern.is_some() {
            result["more_matches"] = json!(read.cut);
        } else {
            result["offset"] = json!(read.range.0);
            result["next_offset"] = json!(read.range.1);
            result["truncated"] = json!(read.cut);
        }
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        match (shell, record) {
            (Some(shell), _) => {
                result["status"] = json!(shell.status.name());
                result["command"] = json!(shell.command);
                result["duration_ms"] = json!(millis(shell.elapsed));
                match shell.status {
                    ShellStatus::Exited { exit_code } => result["exit_code"] = json!(exit_code),
                    ShellStatus::Killed { cause } => result["killed_by"] = json!(cause),
                    ShellStatus::Running => {}
                }
            }
            (None, Some(record)) => {
                result["command"] = json!(record.command);
                match record.finished {
                    Some((exit_code, duration)) => {
                        result["status"] = json!("exited");
                        result["exit_code"] = json!(exit_code);
                        result["duration_ms"] = json!(millis(duration));
                    }
                    None => result["status"] = json!("running"),
                }
            }
            (None, None) => result["status"] = json!("exited"),
        }
        Ok(result)
    }
}

/// The highest output id in `dir`, 0 when there is none.
fn highest_id(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".log")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

/// Which part of an output to read; at most [`INLINE_OUTPUT_CHARS`] characters of it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slice {
    /// Lines matching this pattern, with their numbers.
    pub(crate) pattern: Option<regex::Regex>,
    /// Bytes from this offset.
    pub(crate) offset: Option<u64>,
    /// How many bytes from `offset`.
    pub(crate) limit: Option<u64>,
    /// The last lines.
    pub(crate) tail_lines: Option<usize>,
}

/// What a read returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputRead {
    /// The text: a byte range, or matching lines.
    pub(crate) text: String,
    /// Bytes in the file.
    pub(crate) total_bytes: u64,
    /// The byte range the text covers; for a pattern, the whole file.
    pub(crate) range: (u64, u64),
    /// Whether more matches, or more lines of the range asked for, were left out.
    pub(crate) cut: bool,
}

fn read_slice(file: &mut std::fs::File, total: u64, slice: &Slice) -> std::io::Result<OutputRead> {
    if let Some(pattern) = &slice.pattern {
        return grep(file, total, pattern);
    }
    if let Some(offset) = slice.offset {
        let offset = offset.min(total);
        let limit = slice.limit.unwrap_or(INLINE_OUTPUT_CHARS as u64).min(WINDOW_BYTES);
        let window = read_at(file, offset, limit.min(total - offset))?;
        // A range starting or ending inside a character leaves those bytes to its neighbour.
        let skipped = if offset > 0 {
            window.iter().take(3).take_while(|&&b| b & 0xC0 == 0x80).count()
        } else {
            0
        };
        let end = if offset + window.len() as u64 == total {
            window.len()
        } else {
            complete_end(&window)
        };
        let body = &window[skipped..end.max(skipped)];
        let used = prefix_len(body, INLINE_OUTPUT_CHARS);
        let start = offset + skipped as u64;
        return Ok(OutputRead {
            text: String::from_utf8_lossy(&body[..used]).into_owned(),
            total_bytes: total,
            range: (start, start + used as u64),
            cut: used < body.len(),
        });
    }
    // The end of the output: its last lines, or its last characters.
    let start = total.saturating_sub(WINDOW_BYTES);
    let window = read_at(file, start, total - start)?;
    let skipped = if start > 0 {
        window.iter().take(3).take_while(|&&b| b & 0xC0 == 0x80).count()
    } else {
        0
    };
    let window = &window[skipped..];
    let mut from = suffix_start(window, INLINE_OUTPUT_CHARS);
    let mut cut = false;
    if let Some(lines) = slice.tail_lines {
        // Lines that begin before the window, or past the characters an answer holds, are cut.
        let line_start = nth_line_from_end(window, lines);
        let wanted = line_start.unwrap_or(0);
        cut = wanted < from || (line_start.is_none() && start > 0);
        from = from.max(wanted);
    }
    let begin = start + (skipped + from) as u64;
    Ok(OutputRead {
        text: String::from_utf8_lossy(&window[from..]).into_owned(),
        total_bytes: total,
        range: (begin, total),
        cut,
    })
}

/// Lines of the file matching `pattern`, numbered from 1, up to [`INLINE_OUTPUT_CHARS`].
fn grep(
    file: &mut std::fs::File,
    total: u64,
    pattern: &regex::Regex,
) -> std::io::Result<OutputRead> {
    use std::io::BufRead;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = std::io::BufReader::new(file);
    let mut text = String::new();
    let mut chars = 0;
    let mut cut = false;
    let mut line = Vec::new();
    let mut number = 0usize;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        number += 1;
        let decoded = String::from_utf8_lossy(&line);
        let decoded = decoded.trim_end_matches(['\n', '\r']);
        if !pattern.is_match(decoded) {
            continue;
        }
        let entry = format!("{number}: {decoded}\n");
        let length = entry.chars().count();
        if chars + length > INLINE_OUTPUT_CHARS {
            cut = true;
            break;
        }
        chars += length;
        text.push_str(&entry);
    }
    Ok(OutputRead { text, total_bytes: total, range: (0, total), cut })
}

fn read_at(file: &mut std::fs::File, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(len).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Where `bytes` ends without a character cut short at its end.
fn complete_end(bytes: &[u8]) -> usize {
    let n = bytes.len();
    for back in 1..=n.min(4) {
        let byte = bytes[n - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let needs = match byte {
            0xF0..=0xF7 => 4,
            0xE0..=0xEF => 3,
            0xC0..=0xDF => 2,
            _ => 1,
        };
        return if needs > back { n - back } else { n };
    }
    n
}

/// Where the last `lines` lines of `bytes` begin, or `None` when it holds fewer; a final
/// newline ends the last line.
fn nth_line_from_end(bytes: &[u8], lines: usize) -> Option<usize> {
    if lines == 0 {
        return Some(bytes.len());
    }
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    body.iter()
        .enumerate()
        .rev()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(lines - 1)
        .map(|(index, _)| index + 1)
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
    use std::io::Write;

    fn file_with(bytes: &[u8]) -> (tempfile::NamedTempFile, std::fs::File, u64) {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(bytes).unwrap();
        let file = temp.reopen().unwrap();
        (temp, file, bytes.len() as u64)
    }

    fn numbered(lines: usize) -> String {
        (0..lines).map(|i| format!("{i:015}\n")).collect()
    }

    #[test]
    fn an_offset_reads_bytes_from_the_middle() {
        let text = numbered(327_680);
        let (_temp, mut file, total) = file_with(text.as_bytes());
        let slice = Slice { offset: Some(2_621_440), limit: Some(32), ..Slice::default() };
        let read = read_slice(&mut file, total, &slice).unwrap();
        assert_eq!(
            read,
            OutputRead {
                text: format!("{:015}\n{:015}\n", 163_840, 163_841),
                total_bytes: total,
                range: (2_621_440, 2_621_472),
                cut: false,
            }
        );
    }

    #[test]
    fn the_default_is_the_last_characters_and_tail_lines_the_last_lines() {
        let text = numbered(10_000);
        let (_temp, mut file, total) = file_with(text.as_bytes());
        let read = read_slice(&mut file, total, &Slice::default()).unwrap();
        assert_eq!(read.text, text[text.len() - INLINE_OUTPUT_CHARS..]);
        let lines = Slice { tail_lines: Some(2), ..Slice::default() };
        let read = read_slice(&mut file, total, &lines).unwrap();
        assert_eq!(read.text, format!("{:015}\n{:015}\n", 9_998, 9_999));
        assert_eq!(read.range, (total - 32, total));
    }

    #[test]
    fn a_pattern_returns_numbered_matching_lines() {
        let (_temp, mut file, total) = file_with(b"ok\nerror[E0308]: mismatched\nok\nerror: x\n");
        let slice =
            Slice { pattern: Some(regex::Regex::new("^error").unwrap()), ..Slice::default() };
        let read = read_slice(&mut file, total, &slice).unwrap();
        assert_eq!(read.text, "2: error[E0308]: mismatched\n4: error: x\n");
        assert!(!read.cut);
    }

    #[test]
    fn a_range_inside_characters_moves_to_their_boundaries() {
        let text = "€".repeat(10);
        let (_temp, mut file, total) = file_with(text.as_bytes());
        let slice = Slice { offset: Some(1), limit: Some(7), ..Slice::default() };
        let read = read_slice(&mut file, total, &slice).unwrap();
        // Bytes 1..8: the rest of the first `€` is skipped, the third is cut short.
        assert_eq!((read.text.as_str(), read.range), ("€", (3, 6)));
    }

    #[test]
    fn ids_are_numbers_and_never_reuse_an_earlier_file() {
        let root = tempfile::tempdir().unwrap();
        let outputs = ShellOutputs::new(root.path());
        let first = outputs.allocate("app", "alice", "s1", "make").unwrap();
        assert_eq!(first.id, "1");
        let again = ShellOutputs::new(root.path());
        assert_eq!(again.allocate("app", "alice", "s1", "make").unwrap().id, "2");
        assert_eq!(again.allocate("app", "bob", "s1", "make").unwrap().id, "1");
        assert!(ShellOutputs::path_of(Path::new("/x"), "../1").is_none());
        let error = outputs.read(("app", "bob", "s2"), "1", &Slice::default()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_an_output_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let outputs = ShellOutputs::new(root.path());
        let dir = outputs.session_dir("app", "alice", "s1");
        ShellOutputs::prepare(&dir).unwrap();
        let secret = root.path().join("secret");
        std::fs::write(&secret, "token").unwrap();
        std::os::unix::fs::symlink(&secret, dir.join("7.log")).unwrap();
        let error = outputs.read(("app", "alice", "s1"), "7", &Slice::default()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
