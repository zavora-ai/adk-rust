//! Change sets: the held edits of one model turn reviewed as one diff and decided once
//! (spec 3.1 §7.26, §9.2 I66).
//!
//! [`ChangeSet::compose`] applies `write_file`, `edit_file` and `apply_patch` calls in call
//! order to an in-memory overlay of the workspace, read through the file tools' own rules,
//! so successive edits of one file compose. Each composed call carries the `expect_sha256`
//! its file had at that point of the composition — the batch's base for the first call on a
//! file, the intermediate hash for the next — so the call writes only against the content
//! the person reviewed.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_devtools::Workspace;
use adk_devtools::patch::{PatchOp, apply_hunks, parse_patch};
use adk_devtools::sha256_hex;
use serde_json::{Map, Value, json};

use crate::mode::HarnessMode;

/// Most bytes of a change set's rendered diff a prompt shows before `?` expands it.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::CHANGE_SET_MAX_BYTES, 16 * 1024);
/// ```
pub const CHANGE_SET_MAX_BYTES: usize = 16 * 1024;

/// Lines of context around each change of a diff.
const CONTEXT_LINES: usize = 3;
/// Above this many cells the middle of a diff is shown as replaced rather than aligned.
const DIFF_CELLS_MAX: usize = 4_000_000;

/// One call of a [`ChangeSet`], in call order.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ChangeSetCall {
    /// The call's ID.
    pub function_call_id: String,
    /// `write_file`, `edit_file` or `apply_patch`.
    pub tool: String,
    /// The call's arguments.
    pub args: Value,
    /// The files it changes, as the workspace names them.
    pub files: Vec<String>,
    /// Why the call would fail; such a call is answered with this reason and never runs.
    pub failure: Option<String>,
    /// `expect_sha256` as the composition bound it: a digest or `absent`, or a map of them
    /// for `apply_patch`.
    expect: Option<Value>,
}

impl ChangeSetCall {
    /// The arguments the call is approved with: its own, with `expect_sha256` set to the
    /// content its files had at its place in the change set; `None` for a failing call.
    ///
    /// # Example
    ///
    /// ```rust
    /// # async fn example(workspace: adk_devtools::Workspace) {
    /// use adk_harness::ChangeSet;
    /// use serde_json::json;
    ///
    /// let set = ChangeSet::compose(
    ///     &workspace,
    ///     vec![("c1".into(), "write_file".into(), json!({ "path": "new.md", "content": "x" }))],
    /// )
    /// .await;
    /// assert_eq!(set.calls[0].approved_args().unwrap()["expect_sha256"], "absent");
    /// # }
    /// ```
    pub fn approved_args(&self) -> Option<Value> {
        if self.failure.is_some() {
            return None;
        }
        let mut args = self.args.clone();
        if let (Some(object), Some(expect)) = (args.as_object_mut(), &self.expect) {
            object.insert("expect_sha256".to_string(), expect.clone());
        }
        Some(args)
    }
}

/// One file of a [`ChangeSet`]: its content before the batch and after every call of it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ChangeSetFile {
    /// The file, relative to the workspace root.
    pub path: String,
    /// Its sha256 before the batch; `None` when it does not exist yet.
    pub before_sha256: Option<String>,
    /// Its sha256 after the batch; `None` when the batch deletes it.
    pub after_sha256: Option<String>,
    /// A unified diff from before to after.
    pub diff: String,
    /// The calls that change it, in call order.
    pub call_ids: Vec<String>,
}

/// The held edits of one model turn, composed (spec 3.1 §7.26).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ChangeSet {
    /// `sha256:` over the sorted (path, before, after) triples of its files.
    pub digest: String,
    /// The files it changes, in the order the calls first touch them.
    pub files: Vec<ChangeSetFile>,
    /// Its calls, in call order.
    pub calls: Vec<ChangeSetCall>,
}

/// A file as the composition sees it.
struct Entry {
    display: String,
    base: Option<Vec<u8>>,
    current: Option<Vec<u8>>,
}

fn digest_or_absent(bytes: Option<&[u8]>) -> String {
    bytes.map_or_else(|| "absent".to_string(), sha256_hex)
}

/// The overlay: every file a call touched, by record key, in first-touch order.
#[derive(Default)]
struct Overlay {
    entries: BTreeMap<String, Entry>,
    order: Vec<String>,
}

impl Overlay {
    /// The key of `path`, loading its content through the workspace on first touch.
    async fn load(&mut self, workspace: &Workspace, path: &str) -> Result<String, String> {
        let key = workspace.record_key(path).map_err(|error| error.to_string())?;
        if !self.entries.contains_key(&key) {
            let content =
                workspace.read_for_review(path).await.map_err(|error| error.to_string())?;
            self.entries.insert(
                key.clone(),
                Entry { display: key.clone(), base: content.clone(), current: content },
            );
        }
        Ok(key)
    }

    fn current(&self, key: &str) -> Option<&Vec<u8>> {
        self.entries.get(key).and_then(|entry| entry.current.as_ref())
    }

    fn set(&mut self, key: &str, content: Option<Vec<u8>>) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.current = content;
        }
        if !self.order.iter().any(|seen| seen == key) {
            self.order.push(key.to_string());
        }
    }
}

/// The text of `bytes`, or why a text edit cannot apply to it.
fn text_of<'a>(bytes: Option<&'a Vec<u8>>, path: &str) -> Result<&'a str, String> {
    let bytes = bytes.ok_or_else(|| format!("'{path}' does not exist"))?;
    std::str::from_utf8(bytes).map_err(|_| format!("'{path}' is not UTF-8 text"))
}

/// The model's own `expect_sha256` for a file, checked against the composed content.
fn check_model_expect(
    expect: Option<&Value>,
    current: Option<&[u8]>,
    path: &str,
) -> Result<(), String> {
    let Some(expect) = expect.and_then(Value::as_str) else {
        return Ok(());
    };
    let found = digest_or_absent(current);
    let wanted = expect.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if wanted == found {
        Ok(())
    } else {
        Err(format!("'{path}' does not have the sha256 the call expects; read it again"))
    }
}

impl ChangeSet {
    /// Composes `calls` — (call ID, tool, arguments) — in order over an overlay of
    /// `workspace`, reading each file once through the file tools' containment, symlink
    /// and exclusion rules (spec 3.1 §7.26).
    ///
    /// A call that would fail — no match, an escape, a hunk that does not apply — is kept
    /// with its [`failure`](ChangeSetCall::failure) and changes nothing.
    ///
    /// # Example
    ///
    /// ```rust
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// use adk_harness::ChangeSet;
    /// use serde_json::json;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    /// let workspace = adk_devtools::Workspace::new(dir.path());
    /// let set = ChangeSet::compose(&workspace, vec![
    ///     ("c1".into(), "edit_file".into(), json!({ "path": "a.txt", "old_string": "one", "new_string": "two" })),
    ///     ("c2".into(), "edit_file".into(), json!({ "path": "a.txt", "old_string": "two", "new_string": "three" })),
    /// ]).await;
    /// assert_eq!(set.files.len(), 1);
    /// assert!(set.files[0].diff.contains("-one\n+three"));
    /// // The second edit is bound to the content the first leaves.
    /// let second = set.calls[1].approved_args().unwrap();
    /// assert_eq!(second["expect_sha256"], adk_devtools::sha256_hex(b"two\n"));
    /// # });
    /// ```
    pub async fn compose(workspace: &Workspace, calls: Vec<(String, String, Value)>) -> ChangeSet {
        let mut overlay = Overlay::default();
        let mut composed = Vec::with_capacity(calls.len());
        for (function_call_id, tool, args) in calls {
            let mut call = ChangeSetCall {
                function_call_id,
                tool: tool.clone(),
                args: args.clone(),
                files: Vec::new(),
                failure: None,
                expect: None,
            };
            let outcome = match tool.as_str() {
                "write_file" | "edit_file" => {
                    compose_file_call(workspace, &mut overlay, &tool, &args).await
                }
                crate::edits::APPLY_PATCH => compose_patch(workspace, &mut overlay, &args).await,
                other => Err(format!("{other} is not an edit a change set can show")),
            };
            match outcome {
                Ok((files, expect)) => {
                    call.files = files;
                    call.expect = Some(expect);
                }
                Err(failure) => call.failure = Some(failure),
            }
            composed.push(call);
        }
        let mut files = Vec::new();
        for key in &overlay.order {
            let Some(entry) = overlay.entries.get(key) else { continue };
            let call_ids = composed
                .iter()
                .filter(|call| call.failure.is_none() && call.files.contains(&entry.display))
                .map(|call| call.function_call_id.clone())
                .collect();
            files.push(ChangeSetFile {
                path: entry.display.clone(),
                before_sha256: entry.base.as_deref().map(sha256_hex),
                after_sha256: entry.current.as_deref().map(sha256_hex),
                diff: file_diff(entry.base.as_deref(), entry.current.as_deref()),
                call_ids,
            });
        }
        let mut triples: Vec<String> = files
            .iter()
            .map(|file| {
                format!(
                    "{}\u{1f}{}\u{1f}{}\n",
                    file.path,
                    file.before_sha256.as_deref().unwrap_or("absent"),
                    file.after_sha256.as_deref().unwrap_or("absent")
                )
            })
            .collect();
        triples.sort();
        let digest = format!("sha256:{}", sha256_hex(triples.concat().as_bytes()));
        ChangeSet { digest, files, calls: composed }
    }

    /// The change set for a reader: per file its digests and diff, then the calls that fail.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            let before = file.before_sha256.as_deref().map_or("new file".to_string(), short);
            let after = file.after_sha256.as_deref().map_or("deleted".to_string(), short);
            let _ = writeln!(out, "{}  {before} → {after}", display_safe(&file.path));
            for line in file.diff.lines() {
                let _ = writeln!(out, "  {}", display_safe(line));
            }
        }
        for call in &self.calls {
            if let Some(failure) = &call.failure {
                let _ = writeln!(
                    out,
                    "{} {} fails and will not run: {}",
                    display_safe(&call.tool),
                    display_safe(&call.function_call_id),
                    display_safe(failure)
                );
            }
        }
        out
    }

    /// The calls that change only files in `approved`, and do not fail.
    pub(crate) fn calls_within<'a>(
        &'a self,
        approved: &'a HashSet<String>,
    ) -> impl Iterator<Item = &'a ChangeSetCall> {
        self.calls.iter().filter(move |call| {
            call.failure.is_none() && call.files.iter().all(|file| approved.contains(file))
        })
    }
}

/// `sha256:1a2b3c4d5e6f…`
fn short(digest: &str) -> String {
    format!("sha256:{}…", &digest[..12.min(digest.len())])
}

/// Escapes control characters but keeps tabs, so code reads as code.
fn display_safe(text: &str) -> String {
    text.split('\t').map(crate::approvals::escape_control).collect::<Vec<_>>().join("\t")
}

/// Composes one `write_file` or `edit_file` call; returns its file and `expect_sha256`.
async fn compose_file_call(
    workspace: &Workspace,
    overlay: &mut Overlay,
    tool: &str,
    args: &Value,
) -> Result<(Vec<String>, Value), String> {
    let path = args.get("path").and_then(Value::as_str).ok_or("the call names no path")?;
    let key = overlay.load(workspace, path).await?;
    let before = overlay.current(&key).cloned();
    check_model_expect(args.get("expect_sha256"), before.as_deref(), &key)?;
    let after = if tool == "write_file" {
        args.get("content")
            .and_then(Value::as_str)
            .ok_or("write_file needs content")?
            .as_bytes()
            .to_vec()
    } else {
        let text = text_of(before.as_ref(), &key)?;
        adk_devtools::tools::edit::preview(text, args, &key)
            .map_err(|error| error.message.to_string())?
            .into_bytes()
    };
    overlay.set(&key, Some(after));
    Ok((vec![key], Value::String(digest_or_absent(before.as_deref()))))
}

/// Composes one `apply_patch` call, all or nothing; returns its files and the per-file
/// `expect_sha256` map.
async fn compose_patch(
    workspace: &Workspace,
    overlay: &mut Overlay,
    args: &Value,
) -> Result<(Vec<String>, Value), String> {
    let text = args.get("patch").and_then(Value::as_str).ok_or("the call carries no patch")?;
    let patch = parse_patch(text).map_err(|error| error.0)?;
    let mut keys = Vec::new();
    for path in patch.paths() {
        let key = overlay.load(workspace, path).await?;
        if keys.contains(&key) {
            return Err(format!("'{path}' appears twice in the patch"));
        }
        keys.push(key);
    }
    let model_expects = args.get("expect_sha256").and_then(Value::as_object);
    let mut expects = Map::new();
    let mut changes: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    let mut keys = keys.into_iter();
    for op in &patch.ops {
        let key = keys.next().unwrap_or_default();
        let current = overlay.current(&key).cloned();
        let model_expect = model_expects.and_then(|map| {
            map.iter()
                .find(|(path, _)| workspace.record_key(path).is_ok_and(|named| named == key))
                .map(|(_, value)| value)
        });
        check_model_expect(model_expect, current.as_deref(), &key)?;
        expects.insert(key.clone(), Value::String(digest_or_absent(current.as_deref())));
        match op {
            PatchOp::Add { contents, .. } => {
                if current.is_some() {
                    return Err(format!("'{key}' already exists; a patch adds only new files"));
                }
                changes.push((key, Some(contents.clone().into_bytes())));
            }
            PatchOp::Delete { .. } => {
                if current.is_none() {
                    return Err(format!("'{key}' does not exist"));
                }
                changes.push((key, None));
            }
            PatchOp::Update { move_to, hunks, .. } => {
                let text = text_of(current.as_ref(), &key)?;
                let updated = if hunks.is_empty() {
                    text.to_string()
                } else {
                    apply_hunks(&key, text, hunks).map_err(|error| error.0)?
                };
                match move_to {
                    Some(_) => {
                        let to = keys.next().unwrap_or_default();
                        let existing = overlay.current(&to).cloned();
                        if existing.is_some() {
                            return Err(format!(
                                "'{to}' already exists; a move does not replace a file"
                            ));
                        }
                        expects.insert(to.clone(), Value::String("absent".into()));
                        changes.push((key, None));
                        changes.push((to, Some(updated.into_bytes())));
                    }
                    None => changes.push((key, Some(updated.into_bytes()))),
                }
            }
            _ => return Err("the patch holds an operation this harness cannot show".to_string()),
        }
    }
    let files = changes.iter().map(|(key, _)| key.clone()).collect();
    for (key, content) in changes {
        overlay.set(&key, content);
    }
    Ok((files, Value::Object(expects)))
}

/// A unified diff of a file's content before and after.
fn file_diff(before: Option<&[u8]>, after: Option<&[u8]>) -> String {
    match (before.map(std::str::from_utf8).transpose(), after.map(std::str::from_utf8).transpose())
    {
        (Ok(before), Ok(after)) => unified_diff(before.unwrap_or(""), after.unwrap_or("")),
        _ => format!(
            "binary content: {} → {} bytes\n",
            before.map_or(0, <[u8]>::len),
            after.map_or(0, <[u8]>::len)
        ),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

/// A unified diff of `old` and `new` with `@@ -a,b +c,d @@` hunk headers and three lines
/// of context.
pub(crate) fn unified_diff(old: &str, new: &str) -> String {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    // Align the changed middle; the common prefix and suffix are equal by construction.
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);
    let mut ops = vec![Op::Equal; prefix];
    if a.len().saturating_mul(b.len()) <= DIFF_CELLS_MAX {
        // Longest common subsequence, filled from the end.
        let width = b.len() + 1;
        let mut table = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                table[i * width + j] = if a[i] == b[j] {
                    table[(i + 1) * width + j + 1] + 1
                } else {
                    table[(i + 1) * width + j].max(table[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                ops.push(Op::Equal);
                i += 1;
                j += 1;
            } else if j < b.len()
                && (i == a.len() || table[i * width + j + 1] > table[(i + 1) * width + j])
            {
                ops.push(Op::Insert);
                j += 1;
            } else {
                ops.push(Op::Delete);
                i += 1;
            }
        }
    } else {
        ops.extend(std::iter::repeat_n(Op::Delete, a.len()));
        ops.extend(std::iter::repeat_n(Op::Insert, b.len()));
    }
    ops.extend(std::iter::repeat_n(Op::Equal, suffix));

    // Each op's line numbers (1-based) in old and new.
    let mut positions = Vec::with_capacity(ops.len());
    let (mut o, mut n) = (0usize, 0usize);
    for op in &ops {
        positions.push((o, n));
        match op {
            Op::Equal => {
                o += 1;
                n += 1;
            }
            Op::Delete => o += 1,
            Op::Insert => n += 1,
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&index| ops[index] != Op::Equal).collect();
    let mut out = String::new();
    let mut index = 0;
    while index < changed.len() {
        let start = changed[index].saturating_sub(CONTEXT_LINES);
        let mut end = changed[index];
        while index < changed.len() && changed[index] <= end + 2 * CONTEXT_LINES {
            end = changed[index];
            index += 1;
        }
        let end = (end + CONTEXT_LINES + 1).min(ops.len());
        let (old_start, new_start) = positions[start];
        let old_count = ops[start..end].iter().filter(|op| **op != Op::Insert).count();
        let new_count = ops[start..end].iter().filter(|op| **op != Op::Delete).count();
        let _ = writeln!(
            out,
            "@@ -{},{old_count} +{},{new_count} @@",
            if old_count == 0 { old_start } else { old_start + 1 },
            if new_count == 0 { new_start } else { new_start + 1 },
        );
        for at in start..end {
            let (o, n) = positions[at];
            match ops[at] {
                Op::Equal => {
                    let _ = writeln!(out, " {}", old[o]);
                }
                Op::Delete => {
                    let _ = writeln!(out, "-{}", old[o]);
                }
                Op::Insert => {
                    let _ = writeln!(out, "+{}", new[n]);
                }
            }
        }
    }
    out
}

/// A change set as a prompt shows it (spec 3.1 §7.26).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ChangeSetPrompt {
    /// The composed calls and files.
    pub change_set: ChangeSet,
    /// The session's mode.
    pub mode: HarnessMode,
    /// The acting agent.
    pub agent: String,
    /// Edits of the same turn a standing rule allows: they run without being in the card.
    pub under_rules: usize,
    /// Edits of the same turn asked about on their own after the card: protected paths
    /// (I29), and edits of a file a call outside the card also changes.
    pub asked_separately: usize,
    /// The change set rendered for display.
    pub rendered: RenderedChangeSet,
}

impl ChangeSetPrompt {
    pub(crate) fn new(
        change_set: ChangeSet,
        mode: HarnessMode,
        agent: String,
        under_rules: usize,
        asked_separately: usize,
    ) -> Self {
        let full = change_set.render();
        let (body, truncated) = if full.len() > CHANGE_SET_MAX_BYTES {
            let end = full.floor_char_boundary(CHANGE_SET_MAX_BYTES);
            (format!("{}… [cut: {end} of {} bytes shown]\n", &full[..end], full.len()), true)
        } else {
            (full.clone(), false)
        };
        Self {
            change_set,
            mode,
            agent,
            under_rules,
            asked_separately,
            rendered: RenderedChangeSet {
                body,
                truncated,
                full,
                expanded: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    /// One line naming the change set: its edits, files and digest.
    pub fn title(&self) -> String {
        let edits = self.change_set.calls.len();
        let files = self.change_set.files.len();
        format!(
            "change set: {edits} edit{} to {files} file{} · {}",
            if edits == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
            short(self.change_set.digest.trim_start_matches("sha256:"))
        )
    }

    /// The document an editor opens for "edit": the diff as comment lines, then the
    /// calls' arguments as JSON; [`ChangeSetAnswer::from_edit_document`] reads it back.
    pub fn edit_document(&self) -> String {
        let mut out = format!(
            "# {}\n# Edit the calls' arguments below; lines starting with # are ignored.\n",
            self.title()
        );
        for line in self.rendered.full.lines() {
            let _ = writeln!(out, "# {line}");
        }
        let calls: Vec<Value> = self
            .change_set
            .calls
            .iter()
            .map(|call| json!({ "call": call.function_call_id, "tool": call.tool, "args": call.args }))
            .collect();
        out.push_str(&serde_json::to_string_pretty(&calls).unwrap_or_default());
        out.push('\n');
        out
    }
}

/// A change set rendered for a person; approval needs the whole diff shown (I21).
#[derive(Clone, Debug)]
pub struct RenderedChangeSet {
    /// The diff, cut to [`CHANGE_SET_MAX_BYTES`].
    pub body: String,
    /// Whether [`body`](Self::body) was cut.
    pub truncated: bool,
    full: String,
    expanded: Arc<AtomicBool>,
}

impl RenderedChangeSet {
    /// The whole diff. Showing it lets a cut change set be approved.
    pub fn expand(&self) -> &str {
        self.expanded.store(true, Ordering::SeqCst);
        &self.full
    }

    /// Whether an approval may stand: the whole diff was shown.
    pub fn can_approve(&self) -> bool {
        !self.truncated || self.expanded.load(Ordering::SeqCst)
    }
}

/// A person's one answer to a [`ChangeSetPrompt`] (spec 3.1 §7.26).
///
/// # Example
///
/// ```rust
/// use adk_harness::ChangeSetAnswer;
///
/// let answer = ChangeSetAnswer::ApproveFiles(vec!["src/lib.rs".into()]);
/// assert!(matches!(answer, ChangeSetAnswer::ApproveFiles(_)));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum ChangeSetAnswer {
    /// Run every call that does not fail.
    ApproveAll,
    /// Run the calls that change only these files; deny the others.
    ApproveFiles(Vec<String>),
    /// Run none; the message is reported to the model.
    DenyAll {
        /// Reported to the model.
        message: Option<String>,
    },
    /// Replace calls' arguments, by call ID, and show the change set again; the policy
    /// checks the edited calls again.
    Edit(Vec<(String, Value)>),
}

impl ChangeSetAnswer {
    /// Reads an edited [`ChangeSetPrompt::edit_document`] back as [`Edit`](Self::Edit).
    ///
    /// # Errors
    ///
    /// Returns why the document is not a JSON array of `{ "call", "args" }` objects.
    pub fn from_edit_document(text: &str) -> Result<Self, String> {
        let json: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let calls: Vec<Value> = serde_json::from_str(&json)
            .map_err(|error| format!("not a JSON array of calls: {error}"))?;
        calls
            .into_iter()
            .map(|call| {
                let id =
                    call.get("call").and_then(Value::as_str).ok_or("each call needs \"call\"")?;
                let args = call.get("args").cloned().ok_or("each call needs \"args\"")?;
                Ok((id.to_string(), args))
            })
            .collect::<Result<_, &str>>()
            .map(Self::Edit)
            .map_err(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_shows_each_change_with_context_and_line_numbers() {
        let old = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        let new = old.replace("line 3\n", "line three\n").replace("line 18\n", "");
        let diff = unified_diff(&old, &new);
        assert!(
            diff.starts_with("@@ -1,6 +1,6 @@\n line 1\n line 2\n-line 3\n+line three\n"),
            "{diff}"
        );
        assert!(
            diff.contains("@@ -15,6 +15,5 @@\n line 15\n line 16\n line 17\n-line 18\n"),
            "{diff}"
        );
        assert_eq!(unified_diff("same\n", "same\n"), "");
        assert_eq!(unified_diff("", "a\nb\n"), "@@ -0,0 +1,2 @@\n+a\n+b\n");
        assert_eq!(unified_diff("a\n", ""), "@@ -1,1 +0,0 @@\n-a\n");
    }

    #[test]
    fn an_edit_document_round_trips() {
        let text = "# change set\n# @@ -1 +1 @@\n[\n {\"call\": \"c1\", \"tool\": \"edit_file\", \"args\": {\"path\": \"a\"}}\n]\n";
        assert_eq!(
            ChangeSetAnswer::from_edit_document(text).unwrap(),
            ChangeSetAnswer::Edit(vec![("c1".into(), json!({ "path": "a" }))])
        );
        assert!(ChangeSetAnswer::from_edit_document("# only comments").is_err());
    }
}
