//! The workspace walker shared by snapshots (spec 3.1 §7.27) and `adk-rust ultracode`'s
//! change signals (§7.10).
//!
//! The walk is breadth-first, so a large tree deep down cannot hide the files near the root;
//! it reads each directory's entries sorted by name, follows no symlink (a symlink is recorded
//! as itself), starts no process, and stops at [`WalkOptions::max_files`], marking the walk
//! partial. What else it skips is configured:
//!
//! | Option | Snapshots | Ultracode |
//! |---|---|---|
//! | `.git` | never opened | `config`, `config.worktree`, `hooks/` and `info/` recorded |
//! | Build directories ([`DEFAULT_SKIP_DIRS`]) | skipped | skipped unless a `sensitive_paths` glob names them |
//! | `.gitignore` | honoured | not read |
//! | `Workspace::exclude_globs` | skipped, listed as not covered | not read |
//! | Files over `max_file_bytes` | skipped, listed as not covered | recorded |

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Directories a walk skips by name: build output and dependencies.
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::snapshot::DEFAULT_SKIP_DIRS.contains(&"target"));
/// assert!(adk_harness::snapshot::DEFAULT_SKIP_DIRS.contains(&"node_modules"));
/// ```
pub const DEFAULT_SKIP_DIRS: &[&str] =
    &["target", "node_modules", ".venv", "venv", "dist", "build", "__pycache__", ".next"];

/// A path filter: `true` excludes the workspace-relative path.
pub type ExcludeFilter = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// What a [`walk`] records and skips.
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::{WalkOptions, walk};
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
/// std::fs::write(dir.path().join("app.log"), "noise").unwrap();
/// std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
/// let tree = walk(dir.path(), &WalkOptions::default());
/// let paths: Vec<&str> = tree.files.iter().map(|file| file.path.as_str()).collect();
/// assert_eq!(paths, [".gitignore", "main.rs"]);
/// ```
#[derive(Clone)]
pub struct WalkOptions {
    /// Most files recorded before the walk stops and sets [`TreeWalk::partial`].
    pub max_files: usize,
    /// Files larger than this are listed as not covered instead of recorded; `None` records
    /// every file.
    pub max_file_bytes: Option<u64>,
    /// Directory names skipped wherever they appear.
    pub skip_dirs: Vec<String>,
    /// Workspace-relative paths that enter a skipped directory: `dist` enters `dist/`.
    pub enter: Vec<String>,
    /// The entries of a `.git` directory recorded; `None` never opens `.git`.
    pub git_entries: Option<Vec<String>>,
    /// Whether `.gitignore` files are honoured.
    pub gitignore: bool,
    /// Paths skipped and listed as not covered, such as `Workspace::is_excluded`.
    pub exclude: Option<ExcludeFilter>,
}

impl std::fmt::Debug for WalkOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkOptions")
            .field("max_files", &self.max_files)
            .field("max_file_bytes", &self.max_file_bytes)
            .field("skip_dirs", &self.skip_dirs)
            .field("enter", &self.enter)
            .field("git_entries", &self.git_entries)
            .field("gitignore", &self.gitignore)
            .field("exclude", &self.exclude.is_some())
            .finish()
    }
}

impl Default for WalkOptions {
    /// The snapshot walk (spec 3.1 §7.27): 50,000 files, 8 MiB per file, `.gitignore`
    /// honoured, `.git` never opened, the build directories skipped.
    fn default() -> Self {
        Self {
            max_files: 50_000,
            max_file_bytes: Some(8 * 1024 * 1024),
            skip_dirs: DEFAULT_SKIP_DIRS.iter().map(|name| (*name).to_string()).collect(),
            enter: Vec::new(),
            git_entries: None,
            gitignore: true,
            exclude: None,
        }
    }
}

/// What a walked entry is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A symbolic link, never followed.
    Symlink,
}

/// One file a [`walk`] recorded: its path and what `lstat` says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkedFile {
    /// Relative to the root, with `/` separators.
    pub path: String,
    /// A file or a symlink.
    pub kind: EntryKind,
    /// Length in bytes.
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch; `0` when unknown.
    pub mtime_ns: u64,
    /// The inode number on Unix; `0` elsewhere.
    pub inode: u64,
    /// The permission bits on Unix; `0` elsewhere.
    pub mode: u32,
}

/// Why a file was left out of a walk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "reason")]
pub enum SkipReason {
    /// Larger than `max_file_bytes`.
    #[serde(rename_all = "camelCase")]
    TooLarge {
        /// Its length.
        len: u64,
    },
    /// Matched an exclusion (`Workspace::exclude_globs`).
    Excluded,
}

/// A file a walk saw and left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotCovered {
    /// Relative to the root, with `/` separators.
    pub path: String,
    /// Why it was left out.
    #[serde(flatten)]
    pub reason: SkipReason,
}

/// The result of a [`walk`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeWalk {
    /// The files, in walk order.
    pub files: Vec<WalkedFile>,
    /// Whether the walk stopped at `max_files`.
    pub partial: bool,
    /// Files seen and left out.
    pub not_covered: Vec<NotCovered>,
}

/// Walks `root` under `options` (see the module documentation).
pub fn walk(root: &Path, options: &WalkOptions) -> TreeWalk {
    let reached = |relative: &str| {
        options.enter.iter().any(|prefix| {
            !prefix.is_empty()
                && (prefix == relative
                    || prefix.strip_prefix(relative).is_some_and(|rest| rest.starts_with('/'))
                    || relative
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/')))
        })
    };
    let mut tree = TreeWalk::default();
    let mut ignore = IgnoreRules::default();
    // (directory, inside `.git`)
    let mut pending: VecDeque<(PathBuf, bool)> = VecDeque::from([(root.to_path_buf(), false)]);
    while let Some((dir, git)) = pending.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        if options.gitignore
            && !git
            && let Some(base) = relative_path(root, &dir)
        {
            ignore.load(&dir.join(".gitignore"), &base);
        }
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if git && !options.git_entries.as_ref().is_some_and(|names| names.contains(&name)) {
                continue;
            }
            // A `.git` file is a worktree's or submodule's pointer: as private as the directory.
            if name == ".git" && options.git_entries.is_none() {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = path.symlink_metadata() else {
                continue;
            };
            let Some(relative) = relative_path(root, &path) else {
                continue;
            };
            let is_dir = metadata.is_dir();
            if options.gitignore && !git && ignore.is_ignored(&relative, is_dir) {
                continue;
            }
            if is_dir {
                if name == ".git" {
                    pending.push_back((path, true));
                } else if !options.skip_dirs.contains(&name) || reached(&relative) {
                    pending.push_back((path, false));
                }
                continue;
            }
            if let Some(exclude) = &options.exclude
                && exclude(Path::new(&relative))
            {
                tree.not_covered.push(NotCovered { path: relative, reason: SkipReason::Excluded });
                continue;
            }
            let symlink = metadata.file_type().is_symlink();
            if !symlink
                && let Some(max) = options.max_file_bytes
                && metadata.len() > max
            {
                tree.not_covered.push(NotCovered {
                    path: relative,
                    reason: SkipReason::TooLarge { len: metadata.len() },
                });
                continue;
            }
            if tree.files.len() >= options.max_files {
                tree.partial = true;
                return tree;
            }
            tree.files.push(WalkedFile {
                path: relative,
                kind: if symlink { EntryKind::Symlink } else { EntryKind::File },
                len: metadata.len(),
                mtime_ns: mtime_ns(&metadata),
                inode: inode(&metadata),
                mode: mode(&metadata),
            });
        }
    }
    tree
}

/// Modification time in nanoseconds since the Unix epoch; `0` when unknown.
pub(crate) fn mtime_ns(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(metadata)
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
pub(crate) fn mode(metadata: &std::fs::Metadata) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o7777
}

#[cfg(not(unix))]
pub(crate) fn mode(_metadata: &std::fs::Metadata) -> u32 {
    0
}

/// `path` relative to `root`, with `/` separators; `""` for the root itself.
pub(crate) fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    Some(
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// The `.gitignore` rules read so far, in the order git applies them: a deeper file after
/// its parents, each file's lines in order, the last match deciding.
#[derive(Default)]
struct IgnoreRules {
    rules: Vec<IgnoreRule>,
}

/// One `.gitignore` line.
struct IgnoreRule {
    /// The directory of the `.gitignore`, relative to the root (`""` at the root).
    base: String,
    negate: bool,
    dir_only: bool,
    /// Whether the pattern names a path from `base` (it has a `/` before its end) rather
    /// than a name at any depth.
    anchored: bool,
    segments: Vec<String>,
}

impl IgnoreRules {
    fn load(&mut self, file: &Path, base: &str) {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        for line in text.lines() {
            if let Some(rule) = IgnoreRule::parse(line, base) {
                self.rules.push(rule);
            }
        }
    }

    fn is_ignored(&self, relative: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(relative, is_dir) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

impl IgnoreRule {
    fn parse(line: &str, base: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        // Trailing spaces are ignored unless escaped.
        let line = if line.ends_with("\\ ") { line } else { line.trim_end_matches(' ') };
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (negate, line) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (
                false,
                line.strip_prefix('\\').filter(|rest| rest.starts_with(['#', '!'])).unwrap_or(line),
            ),
        };
        let (dir_only, line) = match line.strip_suffix('/') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        if line.is_empty() {
            return None;
        }
        let anchored = line.contains('/');
        let line = line.strip_prefix('/').unwrap_or(line);
        let segments: Vec<String> =
            line.split('/').filter(|part| !part.is_empty()).map(str::to_string).collect();
        if segments.is_empty() {
            return None;
        }
        Some(Self { base: base.to_string(), negate, dir_only, anchored, segments })
    }

    fn matches(&self, relative: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let below = if self.base.is_empty() {
            relative
        } else {
            match relative.strip_prefix(self.base.as_str()).and_then(|rest| rest.strip_prefix('/'))
            {
                Some(rest) => rest,
                None => return false,
            }
        };
        let path: Vec<&str> = below.split('/').collect();
        if self.anchored {
            segments_match(&self.segments, &path)
        } else {
            path.last().is_some_and(|name| segment_matches(&self.segments[0], name))
        }
    }
}

/// Whether `pattern` matches `path` segment by segment; `**` matches any number of segments.
fn segments_match(pattern: &[String], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| segments_match(rest, &path[skip..]))
        }
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(name, tail)| segment_matches(first, name) && segments_match(rest, tail)),
    }
}

/// `fnmatch` on one path segment: `*`, `?`, `[...]` (with `!` or `^` negation and ranges)
/// and `\` escapes.
fn segment_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_from(&pattern, &name)
}

fn match_from(pattern: &[char], name: &[char]) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return name.is_empty();
    };
    match first {
        '*' => (0..=name.len()).any(|skip| match_from(rest, &name[skip..])),
        '?' => !name.is_empty() && match_from(rest, &name[1..]),
        '[' => {
            let Some((&c, tail)) = name.split_first() else {
                return false;
            };
            match class_matches(rest, c) {
                Some((matched, after)) => matched && match_from(after, tail),
                // An unclosed bracket is a literal `[`.
                None => c == '[' && match_from(rest, tail),
            }
        }
        '\\' => match rest.split_first() {
            Some((&literal, after)) => {
                name.first() == Some(&literal) && match_from(after, &name[1..])
            }
            None => name.first() == Some(&'\\') && name.len() == 1,
        },
        literal => name.first() == Some(&literal) && match_from(rest, &name[1..]),
    }
}

/// Matches `c` against the class that starts `pattern` (after its `[`); returns whether it
/// matched and the pattern after the closing `]`, or `None` when the class is unclosed.
fn class_matches(pattern: &[char], c: char) -> Option<(bool, &[char])> {
    let (negated, mut index) = match pattern.first() {
        Some('!' | '^') => (true, 1),
        _ => (false, 0),
    };
    let mut matched = false;
    let mut first = true;
    while index < pattern.len() {
        let current = pattern[index];
        if current == ']' && !first {
            return Some((matched != negated, &pattern[index + 1..]));
        }
        first = false;
        if index + 2 < pattern.len() && pattern[index + 1] == '-' && pattern[index + 2] != ']' {
            if (current..=pattern[index + 2]).contains(&c) {
                matched = true;
            }
            index += 3;
        } else {
            if current == c {
                matched = true;
            }
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(line: &str) -> IgnoreRule {
        IgnoreRule::parse(line, "").unwrap()
    }

    #[test]
    fn gitignore_patterns_follow_git() {
        assert!(rule("*.log").matches("a/b/app.log", false));
        assert!(!rule("*.log").matches("a/app.logs", false));
        assert!(rule("/build").matches("build", true));
        assert!(!rule("/build").matches("src/build", true));
        assert!(rule("out/").matches("src/out", true));
        assert!(!rule("out/").matches("src/out", false), "a directory pattern needs a directory");
        assert!(rule("docs/*.md").matches("docs/a.md", false));
        assert!(!rule("docs/*.md").matches("docs/x/a.md", false), "`*` stays in one segment");
        assert!(rule("docs/**/*.md").matches("docs/x/y/a.md", false));
        assert!(rule("**/tmp").matches("a/b/tmp", true));
        assert!(rule("[ab]?.txt").matches("bz.txt", false));
        assert!(!rule("[!ab]?.txt").matches("bz.txt", false));
        assert!(rule("[a-c]*").matches("cat", false));
        assert!(IgnoreRule::parse("# comment", "").is_none());
        assert!(IgnoreRule::parse("   ", "").is_none());
        assert!(rule("\\#notes").matches("#notes", false));
    }

    #[test]
    fn the_last_matching_rule_decides_and_nested_files_apply_below_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub/keep")).unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\n!important.tmp\nsecret/\n").unwrap();
        std::fs::write(root.join("sub/.gitignore"), "*.rs\n").unwrap();
        for path in ["a.tmp", "important.tmp", "lib.rs", "sub/x.rs", "sub/keep/y.txt"] {
            std::fs::write(root.join(path), "x").unwrap();
        }
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/key"), "x").unwrap();
        let tree = walk(root, &WalkOptions::default());
        let paths: Vec<&str> = tree.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [".gitignore", "important.tmp", "lib.rs", "sub/.gitignore", "sub/keep/y.txt"]
        );
    }

    #[test]
    fn git_build_dirs_large_and_excluded_files_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [".git/config", "target/debug/out", "node_modules/x/index.js", "src/main.rs"] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), "x").unwrap();
        }
        std::fs::write(root.join(".env"), "TOKEN=1").unwrap();
        std::fs::write(root.join("big.bin"), vec![0_u8; 64]).unwrap();
        let options = WalkOptions {
            max_file_bytes: Some(32),
            exclude: Some(Arc::new(|path: &Path| path == Path::new(".env"))),
            ..WalkOptions::default()
        };
        let tree = walk(root, &options);
        let paths: Vec<&str> = tree.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["src/main.rs"]);
        assert_eq!(
            tree.not_covered,
            [
                NotCovered { path: ".env".into(), reason: SkipReason::Excluded },
                NotCovered { path: "big.bin".into(), reason: SkipReason::TooLarge { len: 64 } },
            ]
        );
        assert!(!tree.partial);
    }

    #[test]
    fn the_walk_stops_at_max_files_and_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..5 {
            std::fs::write(dir.path().join(format!("f{index}")), "").unwrap();
        }
        let tree = walk(dir.path(), &WalkOptions { max_files: 3, ..WalkOptions::default() });
        assert_eq!(tree.files.len(), 3);
        assert!(tree.partial);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_recorded_as_itself_and_never_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let tree = walk(dir.path(), &WalkOptions::default());
        assert_eq!(tree.files.len(), 1);
        assert_eq!(
            (tree.files[0].path.as_str(), &tree.files[0].kind),
            ("link", &EntryKind::Symlink)
        );
    }
}
