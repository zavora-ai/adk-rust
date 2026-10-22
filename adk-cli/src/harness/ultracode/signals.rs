//! The signals the review router reads (spec 3.1 §7.10 "Adaptive review"): keyword features
//! of the task, and a metadata snapshot of the workspace before and after the first turn.
//! No file contents and no `git`: a snapshot is file paths, lengths and modification times.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use adk_harness::approvals::matcher::{glob_matches, is_protected_path};
use serde::{Deserialize, Serialize};

/// Most files a [`WorkspaceSnapshot`] records before it stops and sets `truncated`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_cli::harness::ultracode::SNAPSHOT_MAX_FILES, 20_000);
/// ```
pub const SNAPSHOT_MAX_FILES: usize = 20_000;

/// Most changed paths [`ChangeSignals`] lists; the digest covers all of them.
const MAX_LISTED_PATHS: usize = 64;

/// Directories a snapshot never enters: build output, dependencies and VCS internals.
const SKIPPED_DIRS: &[&str] =
    &[".git", "target", "node_modules", ".venv", "venv", "dist", "build", "__pycache__", ".next"];

/// The manifests that mark a package root.
const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod"];

/// Path components that make a change security-sensitive (spec 3.1 §7.10 rule `sensitive`).
pub(crate) const SECURITY_VOCABULARY: &[&str] = &[
    "auth",
    "crypto",
    "secret",
    "secrets",
    "token",
    "tokens",
    "credential",
    "credentials",
    "jwt",
    "oauth",
    "oidc",
    "sso",
    "tls",
    "cert",
    "certs",
    "sandbox",
    "policy",
    "permission",
    "permissions",
    "acl",
    "rbac",
    "password",
    "passwords",
    "keyring",
    "vault",
    "signing",
];

/// Keyword categories of [`TaskFeatures`], each with its words and phrases.
const KEYWORDS: &[(&str, &[&str])] = &[
    ("security", SECURITY_VOCABULARY),
    (
        "crossCutting",
        &[
            "refactor",
            "rename",
            "migrate",
            "redesign",
            "restructure",
            "across",
            "throughout",
            "all callers",
        ],
    ),
    (
        "api",
        &["api", "interface", "public", "breaking", "signature", "schema", "protocol", "endpoint"],
    ),
    (
        "concurrency",
        &[
            "concurrency",
            "concurrent",
            "async",
            "thread",
            "threads",
            "race",
            "deadlock",
            "lock",
            "mutex",
            "atomic",
            "parallel",
        ],
    ),
    (
        "performance",
        &[
            "performance",
            "perf",
            "slow",
            "latency",
            "optimize",
            "allocation",
            "throughput",
            "cache",
        ],
    ),
    ("tests", &["test", "tests", "testing", "coverage", "flaky"]),
    ("trivial", &["typo", "comment", "docstring", "readme", "formatting", "bump"]),
];

/// Keyword features of a task (spec 3.1 §7.10): counts and per-category keyword hits,
/// matched ASCII case-insensitively on word boundaries.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::TaskFeatures;
///
/// let features = TaskFeatures::extract("Rename the public API across all callers in src/lib.rs");
/// assert_eq!(features.hits("crossCutting"), ["across", "all callers", "rename"]);
/// assert_eq!(features.hits("api"), ["api", "public"]);
/// assert_eq!(features.path_mentions, 1);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskFeatures {
    /// Whitespace-separated words.
    pub words: u32,
    /// Words that look like a path or a file name.
    pub path_mentions: u32,
    /// Keyword hits per category, each sorted and unique; categories without hits are left
    /// out.
    pub keywords: BTreeMap<String, Vec<String>>,
}

impl TaskFeatures {
    /// Extracts the features of `task`.
    pub fn extract(task: &str) -> Self {
        let tokens: Vec<String> = task
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|token| !token.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        let mut keywords = BTreeMap::new();
        for (category, words) in KEYWORDS {
            let hits: BTreeSet<String> = words
                .iter()
                .filter(|phrase| {
                    let parts: Vec<&str> = phrase.split(' ').collect();
                    tokens.windows(parts.len()).any(|window| window.iter().eq(parts.iter()))
                })
                .map(|phrase| (*phrase).to_string())
                .collect();
            if !hits.is_empty() {
                keywords.insert((*category).to_string(), hits.into_iter().collect());
            }
        }
        let path_mentions = task
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| "`'\"(),;:".contains(c)))
            .filter(|word| looks_like_path(word))
            .count();
        Self {
            words: u32::try_from(task.split_whitespace().count()).unwrap_or(u32::MAX),
            path_mentions: u32::try_from(path_mentions).unwrap_or(u32::MAX),
            keywords,
        }
    }

    /// The hits of `category` (`security`, `crossCutting`, `api`, `concurrency`,
    /// `performance`, `tests`, `trivial`).
    pub fn hits(&self, category: &str) -> &[String] {
        self.keywords.get(category).map_or(&[], Vec::as_slice)
    }

    /// Whether `category` has any hit.
    pub fn has(&self, category: &str) -> bool {
        !self.hits(category).is_empty()
    }
}

fn looks_like_path(word: &str) -> bool {
    if word.contains('/') && word.chars().any(|c| c.is_ascii_alphanumeric()) {
        return true;
    }
    match word.rsplit_once('.') {
        Some((stem, extension)) => {
            !stem.is_empty()
                && (1..=5).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
                && extension.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// One file of a [`WorkspaceSnapshot`]: its length and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// Length in bytes.
    pub len: u64,
    /// Modification time in nanoseconds since the Unix epoch; `0` when unknown.
    pub mtime_ns: u64,
}

/// A metadata snapshot of a workspace (spec 3.1 §7.10): every file path, relative and with
/// `/` separators, with its length and modification time. The walk follows no symlink (a
/// symlink is recorded as itself), skips build output, dependencies and `.git` but not
/// `.github` or `.adk`, stops at [`SNAPSHOT_MAX_FILES`], and starts no process.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::WorkspaceSnapshot;
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
/// let before = WorkspaceSnapshot::capture(dir.path());
/// std::fs::write(dir.path().join("b.rs"), "fn b() {}").unwrap();
/// let after = WorkspaceSnapshot::capture(dir.path());
/// assert_eq!(before.diff(&after, &[]).changed_paths, ["b.rs"]);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    /// The files, by relative path.
    pub files: BTreeMap<String, FileStamp>,
    /// Whether the walk stopped at the file cap.
    pub truncated: bool,
}

impl WorkspaceSnapshot {
    /// Captures `root`.
    pub fn capture(root: &Path) -> Self {
        let mut snapshot = Self::default();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let Ok(metadata) = entry.path().symlink_metadata() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if metadata.is_dir() {
                    if !SKIPPED_DIRS.contains(&name.as_str()) {
                        pending.push(entry.path());
                    }
                    continue;
                }
                if snapshot.files.len() >= SNAPSHOT_MAX_FILES {
                    snapshot.truncated = true;
                    return snapshot;
                }
                let Ok(relative) = entry.path().strip_prefix(root).map(Path::to_path_buf) else {
                    continue;
                };
                let relative = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let mtime_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX));
                snapshot.files.insert(relative, FileStamp { len: metadata.len(), mtime_ns });
            }
        }
        snapshot
    }

    /// What changed from this snapshot to `after`: added, removed and modified files, with
    /// the protected (spec 3.1 §9.2 I29), security-vocabulary and `sensitive_paths` matches.
    pub fn diff(&self, after: &WorkspaceSnapshot, sensitive_paths: &[String]) -> ChangeSignals {
        let changed: Vec<&String> = self
            .files
            .keys()
            .chain(after.files.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| self.files.get(*path) != after.files.get(*path))
            .collect();
        let mut packages = BTreeSet::new();
        for path in &changed {
            if let Some(package) = nearest_package(path, self, after) {
                packages.insert(package);
            }
        }
        let listed = |keep: &dyn Fn(&str) -> bool| -> Vec<String> {
            changed
                .iter()
                .filter(|path| keep(path))
                .take(MAX_LISTED_PATHS)
                .map(|path| (*path).clone())
                .collect()
        };
        let all: Vec<&str> = changed.iter().map(|path| path.as_str()).collect();
        ChangeSignals {
            first_turn: TurnStatus::Completed,
            changed_files: u32::try_from(changed.len()).unwrap_or(u32::MAX),
            changed_paths: listed(&|_| true),
            changed_paths_digest: super::sha256_digest(all.join("\n").as_bytes()),
            packages_touched: u32::try_from(packages.len()).unwrap_or(u32::MAX),
            protected: listed(&is_protected_path),
            security: listed(&has_security_component),
            sensitive_matched: listed(&|path| {
                sensitive_paths.iter().any(|pattern| glob_matches(pattern, path))
            }),
            truncated: self.truncated || after.truncated,
        }
    }
}

/// Whether a component of `path` is in the security vocabulary, ignoring case and a file
/// name's extensions (`src/auth/token.rs` has two).
pub(crate) fn has_security_component(path: &str) -> bool {
    path.split('/').any(|component| {
        let lower = component.to_ascii_lowercase();
        let stem = lower.split('.').next().unwrap_or_default();
        SECURITY_VOCABULARY.contains(&lower.as_str()) || SECURITY_VOCABULARY.contains(&stem)
    })
}

/// The directory of the manifest nearest to `path` in either snapshot.
fn nearest_package(
    path: &str,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> Option<String> {
    let mut dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    loop {
        for manifest in MANIFESTS {
            let candidate =
                if dir.is_empty() { (*manifest).to_string() } else { format!("{dir}/{manifest}") };
            if after.files.contains_key(&candidate) || before.files.contains_key(&candidate) {
                return Some(dir.to_string());
            }
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
    }
}

/// How the first implement turn ended (spec 3.1 §7.10 rule `unchanged`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    /// The turn finished.
    #[default]
    Completed,
    /// The turn left calls held for a decision.
    Held,
    /// The turn failed or ended on a denial.
    Failed,
    /// The turn was interrupted.
    Interrupted,
}

/// What the first turn changed (spec 3.1 §7.10): paths and counts only.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{TurnStatus, WorkspaceSnapshot};
///
/// let dir = tempfile::tempdir().unwrap();
/// let before = WorkspaceSnapshot::capture(dir.path());
/// std::fs::create_dir_all(dir.path().join("src/auth")).unwrap();
/// std::fs::write(dir.path().join("src/auth/token.rs"), "pub fn expiry() {}").unwrap();
/// let change = before.diff(&WorkspaceSnapshot::capture(dir.path()), &[]);
/// assert_eq!(change.first_turn, TurnStatus::Completed);
/// assert_eq!(change.security, ["src/auth/token.rs"]);
/// assert!(change.is_sensitive());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangeSignals {
    /// How the first turn ended; [`WorkspaceSnapshot::diff`] reports `Completed` and the
    /// caller sets the turn's outcome.
    pub first_turn: TurnStatus,
    /// Files added, removed or modified.
    pub changed_files: u32,
    /// The first 64 changed paths, sorted.
    pub changed_paths: Vec<String>,
    /// `sha256:` digest of every changed path, one per line, sorted.
    pub changed_paths_digest: String,
    /// Distinct packages (nearest `Cargo.toml`, `package.json`, `pyproject.toml` or
    /// `go.mod`) the changed files belong to.
    pub packages_touched: u32,
    /// Changed paths that are protected (spec 3.1 §9.2 I29).
    pub protected: Vec<String>,
    /// Changed paths with a security-vocabulary component.
    pub security: Vec<String>,
    /// Changed paths matching the operator's `sensitive_paths` globs.
    pub sensitive_matched: Vec<String>,
    /// Whether either snapshot stopped at the file cap.
    pub truncated: bool,
}

impl ChangeSignals {
    /// Whether the change is sensitive: a protected path, a security-vocabulary component,
    /// or a `sensitive_paths` match.
    pub fn is_sensitive(&self) -> bool {
        !(self.protected.is_empty()
            && self.security.is_empty()
            && self.sensitive_matched.is_empty())
    }
}

/// One language of [`RepoSignals`] with its file count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LanguageCount {
    /// The language.
    pub language: String,
    /// Its files.
    pub files: u32,
}

/// What the repository is (spec 3.1 §7.10), from the after-snapshot with no extra walk.
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{RepoSignals, WorkspaceSnapshot};
///
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
/// std::fs::create_dir(dir.path().join("tests")).unwrap();
/// std::fs::write(dir.path().join("tests/it.rs"), "").unwrap();
/// let repo = RepoSignals::from_snapshot(&WorkspaceSnapshot::capture(dir.path()));
/// assert_eq!(repo.manifests, ["Cargo.toml"]);
/// assert!(repo.has_tests);
/// assert_eq!(repo.languages[0].language, "rust");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepoSignals {
    /// The five most common languages by file count.
    pub languages: Vec<LanguageCount>,
    /// Package manifests, sorted, at most 16.
    pub manifests: Vec<String>,
    /// Whether any test file or test directory exists.
    pub has_tests: bool,
}

impl RepoSignals {
    /// The repository signals of `snapshot`.
    pub fn from_snapshot(snapshot: &WorkspaceSnapshot) -> Self {
        let mut languages: BTreeMap<&str, u32> = BTreeMap::new();
        let mut manifests = Vec::new();
        let mut has_tests = false;
        for path in snapshot.files.keys() {
            let name = path.rsplit('/').next().unwrap_or(path);
            if MANIFESTS.contains(&name) && manifests.len() < 16 {
                manifests.push(path.clone());
            }
            if let Some(language) = name.rsplit_once('.').and_then(|(_, ext)| language_of(ext)) {
                *languages.entry(language).or_default() += 1;
            }
            has_tests |= path.split('/').any(|part| part == "tests" || part == "test")
                || name.contains("_test.")
                || name.contains(".test.")
                || name.contains(".spec.")
                || name.starts_with("test_");
        }
        let mut languages: Vec<LanguageCount> = languages
            .into_iter()
            .map(|(language, files)| LanguageCount { language: language.to_string(), files })
            .collect();
        languages.sort_by(|a, b| b.files.cmp(&a.files).then_with(|| a.language.cmp(&b.language)));
        languages.truncate(5);
        Self { languages, manifests, has_tests }
    }
}

fn language_of(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "php" => "php",
        "scala" => "scala",
        "sh" | "bash" => "shell",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_match_on_word_boundaries() {
        let features = TaskFeatures::extract("Fix the AUTHorization typo; tokenizer is fine");
        assert_eq!(features.hits("trivial"), ["typo"]);
        assert!(!features.has("security"), "neither 'authorization' nor 'tokenizer' is a hit");
        let features = TaskFeatures::extract("rotate the JWT signing key in auth.rs");
        assert_eq!(features.hits("security"), ["auth", "jwt", "signing"]);
        assert_eq!(features.path_mentions, 1);
    }

    #[test]
    fn snapshot_sees_bash_writes_and_protected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("README.md"), "a project").unwrap();
        std::fs::write(root.join("target/debug/out"), "build output").unwrap();
        let before = WorkspaceSnapshot::capture(root);
        assert_eq!(before, WorkspaceSnapshot::capture(root), "two captures are equal");
        assert!(!before.files.contains_key("target/debug/out"), "target/ is skipped");

        // What a shell command writes is seen like any tool write.
        std::fs::write(root.join(".github/workflows/ci.yml"), "on: push").unwrap();
        std::fs::write(root.join("notes.txt"), "made by bash").unwrap();
        std::fs::write(root.join("target/debug/out"), "rebuilt").unwrap();
        let change = before.diff(&WorkspaceSnapshot::capture(root), &["*.txt".to_string()]);
        assert_eq!(change.changed_paths, [".github/workflows/ci.yml", "notes.txt"]);
        assert_eq!(change.protected, [".github/workflows/ci.yml"]);
        assert_eq!(change.sensitive_matched, ["notes.txt"]);
        assert!(change.security.is_empty());
        assert!(change.is_sensitive());
        assert!(!change.truncated);

        let big = tempfile::tempdir().unwrap();
        for index in 0..=SNAPSHOT_MAX_FILES {
            std::fs::write(big.path().join(format!("f{index}")), "").unwrap();
        }
        let snapshot = WorkspaceSnapshot::capture(big.path());
        assert!(snapshot.truncated);
        assert_eq!(snapshot.files.len(), SNAPSHOT_MAX_FILES);
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_follows_no_symlink() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let snapshot = WorkspaceSnapshot::capture(dir.path());
        assert_eq!(snapshot.files.keys().collect::<Vec<_>>(), ["link"]);
    }

    #[test]
    fn packages_are_the_nearest_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for package in ["crates/a", "crates/b"] {
            std::fs::create_dir_all(root.join(package).join("src")).unwrap();
            std::fs::write(root.join(package).join("Cargo.toml"), "[package]").unwrap();
        }
        std::fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
        let before = WorkspaceSnapshot::capture(root);
        std::fs::write(root.join("crates/a/src/lib.rs"), "pub fn a() {}").unwrap();
        std::fs::write(root.join("crates/b/src/lib.rs"), "pub fn b() {}").unwrap();
        let change = before.diff(&WorkspaceSnapshot::capture(root), &[]);
        assert_eq!(change.packages_touched, 2);
        assert!(!change.is_sensitive());
        let repo = RepoSignals::from_snapshot(&WorkspaceSnapshot::capture(root));
        assert_eq!(repo.manifests, ["Cargo.toml", "crates/a/Cargo.toml", "crates/b/Cargo.toml"]);
        assert!(!repo.has_tests);
    }
}
