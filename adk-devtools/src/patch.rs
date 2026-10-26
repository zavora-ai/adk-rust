//! The `*** Begin Patch` envelope `apply_patch` takes: its parser and its matcher
//! (spec 3.1 §7.26, §10 D8).
//!
//! ```text
//! *** Begin Patch
//! *** Add File: docs/new.md
//! +first line
//! *** Delete File: old.txt
//! *** Update File: src/lib.rs
//! *** Move to: src/core.rs
//! @@ fn main()
//!  context line
//! -removed line
//! +added line
//! *** End Patch
//! ```
//!
//! A hunk is located by its context and removed lines, never by line numbers: an exact
//! match first, then one that ignores trailing whitespace and line endings, and never an
//! ambiguous or fuzzy one. A hunk with an `@@` anchor matches at the first place at or
//! after the anchor line — the line equal to the anchor, or starting with it — and the
//! first anchor of a hunk must name one line; a hunk without one must match exactly once
//! after the previous hunk. Each line keeps its own line
//! ending, and added lines take the file's prevailing one.

use std::fmt::Write as _;

/// A parsed patch: its file operations in order.
///
/// # Example
///
/// ```rust
/// use adk_devtools::patch::{PatchOp, parse_patch};
///
/// let patch = parse_patch("*** Begin Patch\n*** Delete File: old.txt\n*** End Patch\n")?;
/// assert_eq!(patch.ops, vec![PatchOp::Delete { path: "old.txt".into() }]);
/// # Ok::<(), adk_devtools::patch::PatchError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Patch {
    /// The file operations, in the order the patch lists them.
    pub ops: Vec<PatchOp>,
}

impl Patch {
    /// Every path the patch names — the source and destination of a move included — in
    /// order, as the headers write them. The harness policy reads these (spec 3.1 §7.26).
    ///
    /// # Example
    ///
    /// ```rust
    /// let patch = adk_devtools::patch::parse_patch(
    ///     "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-x\n+y\n*** End Patch",
    /// )?;
    /// assert_eq!(patch.paths(), vec!["a.rs", "b.rs"]);
    /// # Ok::<(), adk_devtools::patch::PatchError>(())
    /// ```
    pub fn paths(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for op in &self.ops {
            match op {
                PatchOp::Add { path, .. } | PatchOp::Delete { path } => out.push(path.as_str()),
                PatchOp::Update { path, move_to, .. } => {
                    out.push(path.as_str());
                    if let Some(to) = move_to {
                        out.push(to.as_str());
                    }
                }
            }
        }
        out
    }
}

/// One file operation of a [`Patch`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatchOp {
    /// Create a file that does not exist.
    Add {
        /// The new file's path.
        path: String,
        /// Its content, each line ending in `\n`.
        contents: String,
    },
    /// Delete a file.
    Delete {
        /// The file's path.
        path: String,
    },
    /// Change a file, and optionally move it.
    Update {
        /// The file's path.
        path: String,
        /// Where the changed file goes instead, when it moves.
        move_to: Option<String>,
        /// The changes, in file order.
        hunks: Vec<Hunk>,
    },
}

/// One located change of an `Update File`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hunk {
    /// The `@@` anchor lines that precede the change, outermost first.
    pub anchors: Vec<String>,
    /// The change's lines.
    pub lines: Vec<HunkLine>,
    /// Whether the change must sit at the end of the file (`*** End of File`).
    pub end_of_file: bool,
}

impl Hunk {
    /// The lines the hunk expects in the file: context and removed lines.
    fn old_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(text) | HunkLine::Remove(text) => Some(text.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect()
    }
}

/// A line of a [`Hunk`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkLine {
    /// Kept as the file has it.
    Context(String),
    /// Removed.
    Remove(String),
    /// Added.
    Add(String),
}

/// Why a patch does not parse or does not apply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PatchError(pub String);

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";

/// Parses the `*** Begin Patch` envelope (spec 3.1 §7.26).
///
/// A shell heredoc around the envelope (`<<'EOF'` … `EOF`) is tolerated, since models
/// trained on a shell-run `apply_patch` sometimes keep it. A `@@ -1,3 +1,4 @@` range header
/// is read as an `@@` line without an anchor: the format has no line numbers.
///
/// # Errors
///
/// Returns a [`PatchError`] naming the line that does not fit the format.
///
/// # Example
///
/// ```rust
/// use adk_devtools::patch::parse_patch;
///
/// let error = parse_patch("*** Update File: a.rs\n").unwrap_err();
/// assert!(error.to_string().contains("*** Begin Patch"));
/// ```
pub fn parse_patch(text: &str) -> Result<Patch, PatchError> {
    let mut lines: Vec<&str> =
        text.trim().lines().map(|line| line.trim_end_matches('\r')).collect();
    if lines.first().is_some_and(|first| first.trim_start().starts_with("<<")) && lines.len() >= 2 {
        lines.remove(0);
        lines.pop();
    }
    let mut lines = lines.into_iter().enumerate().peekable();
    match lines.next() {
        Some((_, first)) if first.trim() == BEGIN => {}
        _ => {
            return Err(PatchError(format!(
                "a patch starts with the line '{BEGIN}' and ends with '{END}'"
            )));
        }
    }
    let mut ops = Vec::new();
    let mut ended = false;
    while let Some((index, line)) = lines.next() {
        let number = index + 1;
        let trimmed = line.trim();
        if trimmed == END {
            ended = true;
            break;
        }
        if let Some(path) = header(line, ADD) {
            let mut contents = String::new();
            while let Some((_, next)) = lines.peek() {
                if next.starts_with("***") {
                    break;
                }
                let (index, next) = lines.next().unwrap_or_default();
                let Some(text) = next.strip_prefix('+') else {
                    return Err(PatchError(format!(
                        "line {}: every line of an added file starts with '+'",
                        index + 1
                    )));
                };
                contents.push_str(text);
                contents.push('\n');
            }
            ops.push(PatchOp::Add { path, contents });
        } else if let Some(path) = header(line, DELETE) {
            ops.push(PatchOp::Delete { path });
        } else if let Some(path) = header(line, UPDATE) {
            let move_to = match lines.peek() {
                Some((_, next)) if header(next, MOVE).is_some() => {
                    lines.next().and_then(|(_, next)| header(next, MOVE))
                }
                _ => None,
            };
            let mut hunks: Vec<Hunk> = Vec::new();
            let mut current = Hunk { anchors: Vec::new(), lines: Vec::new(), end_of_file: false };
            while let Some((_, next)) = lines.peek() {
                let next = *next;
                if next.trim() == EOF_MARKER {
                    lines.next();
                    current.end_of_file = true;
                    continue;
                }
                if next.starts_with("***") {
                    break;
                }
                let (index, next) = lines.next().unwrap_or_default();
                if let Some(rest) = next.strip_prefix("@@") {
                    if !current.lines.is_empty() {
                        hunks.push(std::mem::replace(
                            &mut current,
                            Hunk { anchors: Vec::new(), lines: Vec::new(), end_of_file: false },
                        ));
                    }
                    let anchor = rest.trim();
                    if !anchor.is_empty() && !is_range_header(anchor) {
                        current.anchors.push(anchor.to_string());
                    }
                    continue;
                }
                let line = match next.chars().next() {
                    Some('+') => HunkLine::Add(next[1..].to_string()),
                    Some('-') => HunkLine::Remove(next[1..].to_string()),
                    Some(' ') => HunkLine::Context(next[1..].to_string()),
                    // A blank line in a hunk is an empty context line.
                    None => HunkLine::Context(String::new()),
                    Some(_) => {
                        return Err(PatchError(format!(
                            "line {}: a line of an Update File hunk starts with ' ', '-' or \
                             '+', or is an '@@' line; got: {next}",
                            index + 1
                        )));
                    }
                };
                current.lines.push(line);
            }
            if !current.lines.is_empty() {
                hunks.push(current);
            }
            if hunks.is_empty() && move_to.is_none() {
                return Err(PatchError(format!("line {number}: '{line}' has no changes")));
            }
            ops.push(PatchOp::Update { path, move_to, hunks });
        } else if trimmed.is_empty() {
            continue;
        } else {
            return Err(PatchError(format!(
                "line {number}: expected '{ADD}<path>', '{DELETE}<path>', '{UPDATE}<path>' or \
                 '{END}'; got: {line}"
            )));
        }
    }
    if !ended {
        return Err(PatchError(format!("the patch does not end with '{END}'")));
    }
    if ops.is_empty() {
        return Err(PatchError("the patch changes no file".to_string()));
    }
    Ok(Patch { ops })
}

/// The path of a `prefix` header line, or `None` when `line` is not one.
fn header(line: &str, prefix: &str) -> Option<String> {
    let path = line.trim_start().strip_prefix(prefix)?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Whether `anchor` is a unified-diff range header such as `-1,3 +1,4 @@`.
fn is_range_header(anchor: &str) -> bool {
    let mut parts = anchor.split_whitespace();
    let range = |part: Option<&str>, sign: char| {
        part.and_then(|part| part.strip_prefix(sign)).is_some_and(|numbers| {
            !numbers.is_empty() && numbers.split(',').all(|n| n.parse::<u64>().is_ok())
        })
    };
    range(parts.next(), '-') && range(parts.next(), '+')
}

/// A file split into lines, each with the line ending it had.
struct Lines<'a> {
    lines: Vec<(&'a str, &'a str)>,
    /// The ending added lines take: the file's prevailing one.
    eol: &'static str,
}

impl<'a> Lines<'a> {
    fn split(text: &'a str) -> Self {
        let mut lines = Vec::new();
        let (mut crlf, mut lf) = (0usize, 0usize);
        let mut rest = text;
        while !rest.is_empty() {
            match rest.find('\n') {
                Some(end) => {
                    let (line, eol) = if end > 0 && rest.as_bytes()[end - 1] == b'\r' {
                        crlf += 1;
                        (&rest[..end - 1], &rest[end - 1..=end])
                    } else {
                        lf += 1;
                        (&rest[..end], &rest[end..=end])
                    };
                    lines.push((line, eol));
                    rest = &rest[end + 1..];
                }
                None => {
                    lines.push((rest, ""));
                    rest = "";
                }
            }
        }
        Self { lines, eol: if crlf > lf { "\r\n" } else { "\n" } }
    }
}

/// How two lines are compared: exactly, then ignoring trailing whitespace.
#[derive(Clone, Copy)]
enum Compare {
    Exact,
    TrailingWhitespace,
}

impl Compare {
    fn eq(self, file: &str, patch: &str) -> bool {
        match self {
            Self::Exact => file == patch,
            Self::TrailingWhitespace => file.trim_end() == patch.trim_end(),
        }
    }
}

/// Every start at or after `from` where `needle` matches the file under `compare`.
fn matches_from(lines: &Lines<'_>, needle: &[&str], from: usize, compare: Compare) -> Vec<usize> {
    if needle.is_empty() || needle.len() > lines.lines.len() {
        return Vec::new();
    }
    (from..=lines.lines.len() - needle.len())
        .filter(|start| {
            needle
                .iter()
                .enumerate()
                .all(|(offset, want)| compare.eq(lines.lines[start + offset].0, want))
        })
        .collect()
}

/// The anchor line at or after `from`: the line equal to the anchor, else equal ignoring
/// surrounding whitespace, else starting with it (`@@ fn main()` names `fn main() {`).
/// The first anchor of a hunk must name one line; a later one takes the first after it.
fn find_anchor(
    lines: &Lines<'_>,
    anchor: &str,
    from: usize,
    unique: bool,
) -> Result<usize, String> {
    let wanted = anchor.trim();
    let tests: [&dyn Fn(&str) -> bool; 3] =
        [&|line| line == anchor, &|line| line.trim() == wanted, &|line| {
            line.trim_start().starts_with(wanted)
        }];
    let mut found = Vec::new();
    for test in tests {
        found = (from..lines.lines.len()).filter(|&index| test(lines.lines[index].0)).collect();
        if !found.is_empty() {
            break;
        }
    }
    match found.as_slice() {
        [] => Err(format!("its anchor '@@ {anchor}' was not found")),
        [one] => Ok(*one),
        [first, ..] if !unique => Ok(*first),
        many => Err(format!(
            "its anchor '@@ {anchor}' occurs {} times (lines {}); add an outer anchor or more \
             context",
            many.len(),
            many.iter().take(5).map(|index| (index + 1).to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Applies `hunks` to `text`, returning the new text (spec 3.1 §7.26).
///
/// # Errors
///
/// Returns a [`PatchError`] naming `path`, the hunk that does not apply, its lines and the
/// nearest candidate lines in the file.
///
/// # Example
///
/// ```rust
/// use adk_devtools::patch::{PatchOp, apply_hunks, parse_patch};
///
/// let patch = parse_patch("*** Begin Patch\n*** Update File: a.txt\n@@\n one\n-two\n+2\n*** End Patch")?;
/// let PatchOp::Update { hunks, .. } = &patch.ops[0] else { unreachable!() };
/// assert_eq!(apply_hunks("a.txt", "one\r\ntwo\r\n", hunks)?, "one\r\n2\r\n");
/// # Ok::<(), adk_devtools::patch::PatchError>(())
/// ```
pub fn apply_hunks(path: &str, text: &str, hunks: &[Hunk]) -> Result<String, PatchError> {
    let lines = Lines::split(text);
    // Each hunk's replaced range in the original file, in order.
    let mut edits: Vec<(usize, usize, &Hunk)> = Vec::new();
    let mut cursor = 0usize;
    for (number, hunk) in hunks.iter().enumerate() {
        let fail = |why: String| {
            PatchError(describe_failure(path, number + 1, hunks.len(), hunk, &lines, &why))
        };
        let mut from = cursor;
        let mut anchored = false;
        for (index, anchor) in hunk.anchors.iter().enumerate() {
            from = find_anchor(&lines, anchor, from, index == 0).map_err(&fail)?;
            anchored = true;
        }
        let old = hunk.old_lines();
        let start = if old.is_empty() {
            // A pure addition goes after its anchor line, or at the end of the file.
            if anchored { from + 1 } else { lines.lines.len() }
        } else {
            let mut found = Vec::new();
            for compare in [Compare::Exact, Compare::TrailingWhitespace] {
                found = matches_from(&lines, &old, from, compare);
                if hunk.end_of_file {
                    found.retain(|start| start + old.len() == lines.lines.len());
                }
                if !found.is_empty() {
                    break;
                }
            }
            match found.as_slice() {
                [] => {
                    return Err(fail(if hunk.end_of_file {
                        "its lines were not found at the end of the file".to_string()
                    } else {
                        "its lines were not found".to_string()
                    }));
                }
                [one] => *one,
                [first, ..] if anchored => *first,
                many => {
                    return Err(fail(format!(
                        "its lines occur {} times (at lines {}); add an '@@' anchor or more \
                         context so it matches once",
                        many.len(),
                        many.iter()
                            .take(5)
                            .map(|start| (start + 1).to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
            }
        };
        edits.push((start, start + old.len(), hunk));
        cursor = start + old.len();
    }

    let mut out = String::with_capacity(text.len());
    let mut position = 0usize;
    let push = |out: &mut String, (line, eol): (&str, &str)| {
        out.push_str(line);
        out.push_str(eol);
    };
    for (start, end, hunk) in edits {
        for &line in &lines.lines[position..start] {
            push(&mut out, line);
        }
        // Inserting after a last line that had no ending gives it one.
        if start == lines.lines.len()
            && start > 0
            && lines.lines[start - 1].1.is_empty()
            && hunk.lines.iter().any(|line| matches!(line, HunkLine::Add(_)))
        {
            out.push_str(lines.eol);
        }
        let mut file_line = start;
        for line in &hunk.lines {
            match line {
                HunkLine::Context(_) => {
                    push(&mut out, lines.lines[file_line]);
                    file_line += 1;
                }
                HunkLine::Remove(_) => file_line += 1,
                HunkLine::Add(text) => {
                    out.push_str(text);
                    out.push_str(lines.eol);
                }
            }
        }
        debug_assert_eq!(file_line, end);
        position = end;
    }
    for &line in &lines.lines[position..] {
        push(&mut out, line);
    }
    Ok(out)
}

/// The failure message: the file, the hunk, its lines and the nearest candidate lines.
fn describe_failure(
    path: &str,
    number: usize,
    of: usize,
    hunk: &Hunk,
    lines: &Lines<'_>,
    why: &str,
) -> String {
    const SHOWN: usize = 8;
    let mut out = format!("hunk {number} of {of} for {path} does not apply: {why}.\nThe hunk:\n");
    for anchor in &hunk.anchors {
        let _ = writeln!(out, "  @@ {anchor}");
    }
    for line in hunk.lines.iter().take(SHOWN) {
        let (sign, text) = match line {
            HunkLine::Context(text) => (' ', text),
            HunkLine::Remove(text) => ('-', text),
            HunkLine::Add(text) => ('+', text),
        };
        let _ = writeln!(out, "  {sign}{text}");
    }
    if hunk.lines.len() > SHOWN {
        let _ = writeln!(out, "  … {} more lines", hunk.lines.len() - SHOWN);
    }
    let old = hunk.old_lines();
    let best = (0..lines.lines.len())
        .map(|start| {
            let score = old
                .iter()
                .enumerate()
                .filter(|(offset, want)| {
                    lines.lines.get(start + offset).is_some_and(|line| line.0.trim() == want.trim())
                })
                .count();
            (score, start)
        })
        .filter(|(score, _)| *score > 0)
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    match best {
        Some((_, start)) => {
            let _ = writeln!(out, "The nearest candidate in the file, from line {}:", start + 1);
            for (index, line) in
                lines.lines.iter().enumerate().skip(start).take(old.len().clamp(1, SHOWN))
            {
                let _ = writeln!(out, "  {:>5} | {}", index + 1, line.0);
            }
        }
        None => out.push_str("No line of the file resembles the hunk; read the file again.\n"),
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(patch: &str) -> Vec<Hunk> {
        match parse_patch(patch).unwrap().ops.remove(0) {
            PatchOp::Update { hunks, .. } => hunks,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_every_operation() {
        let patch = parse_patch(
            "*** Begin Patch\n*** Add File: new.txt\n+a\n+b\n*** Delete File: old.txt\n\
             *** Update File: src/a.rs\n*** Move to: src/b.rs\n@@ fn main()\n x\n-y\n+z\n\
             *** End of File\n*** End Patch\n",
        )
        .unwrap();
        assert_eq!(
            patch.ops,
            vec![
                PatchOp::Add { path: "new.txt".into(), contents: "a\nb\n".into() },
                PatchOp::Delete { path: "old.txt".into() },
                PatchOp::Update {
                    path: "src/a.rs".into(),
                    move_to: Some("src/b.rs".into()),
                    hunks: vec![Hunk {
                        anchors: vec!["fn main()".into()],
                        lines: vec![
                            HunkLine::Context("x".into()),
                            HunkLine::Remove("y".into()),
                            HunkLine::Add("z".into()),
                        ],
                        end_of_file: true,
                    }],
                },
            ]
        );
        assert_eq!(patch.paths(), vec!["new.txt", "old.txt", "src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn a_heredoc_wrapper_and_range_headers_are_tolerated() {
        let hunks = update(
            "<<'EOF'\n*** Begin Patch\n*** Update File: a\n@@ -1,2 +1,2 @@\n-x\n+y\n*** End Patch\nEOF",
        );
        assert!(hunks[0].anchors.is_empty());
    }

    #[test]
    fn malformed_patches_name_the_problem() {
        for (patch, says) in [
            ("*** Update File: a\n", "starts with"),
            ("*** Begin Patch\n*** Update File: a\n-x\n", "does not end"),
            ("*** Begin Patch\n*** Add File: a\nno plus\n*** End Patch", "starts with '+'"),
            ("*** Begin Patch\n*** Update File: a\n*** End Patch", "has no changes"),
            ("*** Begin Patch\n*** Update File: a\n@@\nxyz\n*** End Patch", "starts with ' '"),
            ("*** Begin Patch\n*** End Patch", "changes no file"),
            ("*** Begin Patch\n*** Rename: a\n*** End Patch", "expected"),
        ] {
            let error = parse_patch(patch).unwrap_err().to_string();
            assert!(error.contains(says), "{patch:?}: {error}");
        }
    }

    #[test]
    fn an_exact_match_applies_and_keeps_context_lines() {
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@\n a\n-b\n+B\n c\n*** End Patch");
        assert_eq!(apply_hunks("f", "a\nb\nc\nd\n", &hunks).unwrap(), "a\nB\nc\nd\n");
    }

    #[test]
    fn trailing_whitespace_is_tolerated_and_the_file_keeps_its_own() {
        let hunks = update("*** Begin Patch\n*** Update File: f\n@@\n a\n-b\n+B\n*** End Patch");
        assert_eq!(apply_hunks("f", "a  \nb\t\nc\n", &hunks).unwrap(), "a  \nB\nc\n");
    }

    #[test]
    fn leading_whitespace_is_not_tolerated() {
        let hunks = update("*** Begin Patch\n*** Update File: f\n@@\n-b\n+B\n*** End Patch");
        let error = apply_hunks("f", "  b\n", &hunks).unwrap_err().to_string();
        assert!(error.contains("not found"), "{error}");
        assert!(error.contains("nearest candidate"), "{error}");
    }

    #[test]
    fn an_ambiguous_hunk_is_refused_and_an_anchor_settles_it() {
        let file = "fn a() {\n    x\n}\nfn b() {\n    x\n}\n";
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@\n-    x\n+    y\n*** End Patch");
        let error = apply_hunks("f", file, &hunks).unwrap_err().to_string();
        assert!(error.contains("occur 2 times"), "{error}");
        let hunks = update(
            "*** Begin Patch\n*** Update File: f\n@@ fn b() {\n-    x\n+    y\n*** End Patch",
        );
        assert_eq!(
            apply_hunks("f", file, &hunks).unwrap(),
            "fn a() {\n    x\n}\nfn b() {\n    y\n}\n"
        );
    }

    #[test]
    fn an_anchor_names_a_line_by_its_start_and_must_name_one() {
        let file = "fn a() {\n    x\n}\nfn b() {\n    x\n}\n";
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@ fn b()\n-    x\n+    y\n*** End Patch");
        assert_eq!(
            apply_hunks("f", file, &hunks).unwrap(),
            "fn a() {\n    x\n}\nfn b() {\n    y\n}\n"
        );
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@ fn\n-    x\n+    y\n*** End Patch");
        let error = apply_hunks("f", file, &hunks).unwrap_err().to_string();
        assert!(error.contains("occurs 2 times"), "{error}");
    }

    #[test]
    fn line_endings_are_kept_per_line() {
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@\n a\n-b\n+B\n+C\n*** End Patch");
        assert_eq!(apply_hunks("f", "a\r\nb\r\nc\n", &hunks).unwrap(), "a\r\nB\r\nC\r\nc\n");
        assert_eq!(apply_hunks("f", "a\nb", &hunks).unwrap(), "a\nB\nC\n");
    }

    #[test]
    fn hunks_apply_in_order_and_a_later_failure_names_its_hunk() {
        let hunks =
            update("*** Begin Patch\n*** Update File: f\n@@\n-a\n+A\n@@\n-c\n+C\n*** End Patch");
        assert_eq!(apply_hunks("f", "a\nb\nc\n", &hunks).unwrap(), "A\nb\nC\n");
        let error = apply_hunks("f", "a\nb\n", &hunks).unwrap_err().to_string();
        assert!(error.starts_with("hunk 2 of 2 for f"), "{error}");
    }

    #[test]
    fn end_of_file_hunks_match_only_at_the_end() {
        let hunks = update(
            "*** Begin Patch\n*** Update File: f\n@@\n-x\n+y\n*** End of File\n*** End Patch",
        );
        assert_eq!(apply_hunks("f", "x\nmid\nx\n", &hunks).unwrap(), "x\nmid\ny\n");
    }

    #[test]
    fn pure_additions_go_after_the_anchor_or_at_the_end() {
        let at_end = update("*** Begin Patch\n*** Update File: f\n@@\n+z\n*** End Patch");
        assert_eq!(apply_hunks("f", "a\nb", &at_end).unwrap(), "a\nb\nz\n");
        let anchored = update("*** Begin Patch\n*** Update File: f\n@@ a\n+z\n*** End Patch");
        assert_eq!(apply_hunks("f", "a\nb\n", &anchored).unwrap(), "a\nz\nb\n");
    }
}
