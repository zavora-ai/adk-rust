//! Reading a pushed commit's files (spec 3.5 §7.8.2 "Files and git"): the [`CommitSource`]
//! trait, a local git repository ([`LocalGitSource`]) and a forge's raw-file endpoint over
//! HTTP ([`HttpCommitSource`]). No forge SDK: the local source runs the `git` program, the
//! HTTP source fills a URL template the owner writes for their forge.
//!
//! | Source | Reads | Lists a push's changes |
//! |---|---|---|
//! | [`LocalGitSource`] | `git cat-file` in a repository on this machine, fetching from a remote first when the commit is missing | `git diff --name-status` between the push's two commits |
//! | [`HttpCommitSource`] | `GET` on a URL template such as `https://raw.githubusercontent.com/{repository}/{commit}/{path}` | No: the push's own lists |
//!
//! Every source refuses a commit id that is not 40 or 64 hexadecimal digits and a path that
//! is absolute, climbs with `..` or is longer than 1,024 bytes, and reads at most
//! [`MAX_FILE_BYTES`] of one file. Implementations run [`commit_source_conformance`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_core::{AdkError, CredentialSecret, Result};
use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// The most bytes read of one file.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// How long one `git` command or HTTP request may take.
const TIMEOUT: Duration = Duration::from_secs(60);

fn refused(message: impl Into<String>) -> AdkError {
    AdkError::config(message.into())
}

/// One file a push changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// The path, relative to the repository root.
    pub path: String,
    /// Whether the push removes it.
    pub removed: bool,
}

/// Whether `id` is a full commit id: 40 (SHA-1) or 64 (SHA-256) hexadecimal digits.
///
/// # Example
///
/// ```rust
/// use adk_website::repository::source::is_commit_id;
///
/// assert!(is_commit_id(&"a".repeat(40)));
/// assert!(!is_commit_id("HEAD") && !is_commit_id("--upload-pack=x"));
/// ```
pub fn is_commit_id(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Refuses a commit id or path a source must not read.
fn checked<'a>(commit: &str, path: &'a str) -> Result<&'a str> {
    if !is_commit_id(commit) {
        return Err(refused(format!("`{commit}` is not a full commit id")));
    }
    let bad = path.is_empty()
        || path.len() > 1024
        || path.starts_with('/')
        || path.starts_with('-')
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(refused(format!("`{path}` is not a relative path in the repository")));
    }
    Ok(path)
}

/// Reads files from a pushed commit (spec 3.5 §7.8.2 "Files and git").
#[async_trait]
pub trait CommitSource: Send + Sync {
    /// The source's name, for logs.
    fn name(&self) -> &str;

    /// The file at `path` in `commit` of `repository`; `None` when the commit holds no such
    /// file.
    ///
    /// # Errors
    ///
    /// A commit id or path the source refuses, a file over [`MAX_FILE_BYTES`], or a source
    /// that cannot be read, such as a commit the repository does not have.
    async fn read(&self, repository: &str, commit: &str, path: &str) -> Result<Option<Bytes>>;

    /// The files `commit` changes against `before`, when the source can list them; `None`
    /// leaves the push's own lists, which a forge may cut short.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read).
    async fn changes(
        &self,
        _repository: &str,
        _before: Option<&str>,
        _commit: &str,
    ) -> Result<Option<Vec<FileChange>>> {
        Ok(None)
    }
}

/// The checks every [`CommitSource`] passes: `present` reads back as its bytes from
/// `commit`, `absent` reads as `None`, and a short commit id and a climbing path are refused.
///
/// # Panics
///
/// When the source fails a check.
pub async fn commit_source_conformance(
    source: &dyn CommitSource,
    repository: &str,
    commit: &str,
    present: (&str, &[u8]),
    absent: &str,
) {
    let (path, bytes) = present;
    let read = source.read(repository, commit, path).await.expect("a present file reads");
    assert_eq!(read.as_deref(), Some(bytes), "{} reads {path} at {commit}", source.name());
    let missing = source.read(repository, commit, absent).await.expect("an absent file reads");
    assert_eq!(missing, None, "{} reads {absent} as absent", source.name());
    assert!(source.read(repository, &commit[..7], path).await.is_err(), "a short id is refused");
    assert!(source.read(repository, "HEAD", path).await.is_err(), "a ref name is refused");
    assert!(
        source.read(repository, commit, "../site.toml").await.is_err(),
        "a climbing path is refused"
    );
    assert!(source.read(repository, commit, "/etc/passwd").await.is_err(), "an absolute path");
}

/// A git repository on this machine, read with the `git` program: the content repository
/// itself, or a mirror of the forge's (spec 3.5 §7.8.2).
///
/// # Example
///
/// ```rust
/// use adk_website::repository::source::{CommitSource, LocalGitSource};
///
/// let source = LocalGitSource::new("/srv/site-content.git").with_fetch("origin");
/// assert_eq!(source.name(), "git");
/// ```
#[derive(Debug, Clone)]
pub struct LocalGitSource {
    dir: PathBuf,
    fetch: Option<String>,
}

impl LocalGitSource {
    /// The repository at `dir`, a working copy or a bare repository.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), fetch: None }
    }

    /// Fetches from `remote` before reading a commit the repository does not have yet.
    #[must_use]
    pub fn with_fetch(mut self, remote: impl Into<String>) -> Self {
        self.fetch = Some(remote.into());
        self
    }

    /// The repository's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    async fn git(&self, args: &[&str]) -> Result<std::process::Output> {
        let mut command = tokio::process::Command::new("git");
        command
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        tokio::time::timeout(TIMEOUT, command.output())
            .await
            .map_err(|_| refused(format!("git {} timed out", args.first().unwrap_or(&""))))?
            .map_err(|e| refused(format!("cannot run git in {}: {e}", self.dir.display())))
    }

    async fn has_commit(&self, commit: &str) -> Result<bool> {
        Ok(self.git(&["cat-file", "-e", &format!("{commit}^{{commit}}")]).await?.status.success())
    }

    /// Makes sure `commit` is in the repository, fetching once when a remote is set.
    async fn ensure(&self, commit: &str) -> Result<()> {
        if self.has_commit(commit).await? {
            return Ok(());
        }
        if let Some(remote) = &self.fetch {
            let fetched = self.git(&["fetch", "--quiet", "--no-tags", "--", remote]).await?;
            if !fetched.status.success() {
                tracing::warn!(repository.remote = %remote, "git fetch failed");
            }
            if self.has_commit(commit).await? {
                return Ok(());
            }
        }
        Err(refused(format!("the repository in {} has no commit {commit}", self.dir.display())))
    }
}

#[async_trait]
impl CommitSource for LocalGitSource {
    fn name(&self) -> &str {
        "git"
    }

    async fn read(&self, _repository: &str, commit: &str, path: &str) -> Result<Option<Bytes>> {
        let path = checked(commit, path)?;
        self.ensure(commit).await?;
        let listed = self.git(&["ls-tree", "--name-only", commit, "--", path]).await?;
        if !listed.status.success() || listed.stdout.is_empty() {
            return Ok(None);
        }
        let object = format!("{commit}:{path}");
        let size = self.git(&["cat-file", "-s", &object]).await?;
        let size: usize = String::from_utf8_lossy(&size.stdout).trim().parse().unwrap_or(0);
        if size > MAX_FILE_BYTES {
            return Err(refused(format!("{path} is {size} bytes, over {MAX_FILE_BYTES}")));
        }
        let blob = self.git(&["cat-file", "blob", &object]).await?;
        if !blob.status.success() {
            // A path that names a directory or a submodule is not a file.
            return Ok(None);
        }
        Ok(Some(Bytes::from(blob.stdout)))
    }

    async fn changes(
        &self,
        _repository: &str,
        before: Option<&str>,
        commit: &str,
    ) -> Result<Option<Vec<FileChange>>> {
        let Some(before) = before.filter(|before| is_commit_id(before)) else { return Ok(None) };
        if !is_commit_id(commit) {
            return Err(refused(format!("`{commit}` is not a full commit id")));
        }
        self.ensure(commit).await?;
        if !self.has_commit(before).await? {
            return Ok(None);
        }
        let diff =
            self.git(&["diff", "--name-status", "--no-renames", "-z", before, commit]).await?;
        if !diff.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&diff.stdout);
        let mut fields = text.split('\0').filter(|field| !field.is_empty());
        let mut changes = Vec::new();
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            changes.push(FileChange { path: path.to_string(), removed: status.starts_with('D') });
        }
        Ok(Some(changes))
    }
}

/// Escapes `text` for a URL: every byte but the unreserved ones, and `/` when `keep_slash`.
fn escape(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A forge's raw-file endpoint over HTTP (spec 3.5 §7.8.2): `GET` on a URL template, `404`
/// for a file the commit does not hold.
///
/// | Placeholder | Becomes |
/// |---|---|
/// | `{repository}`, `{path}` | The repository's and the file's names, `/` kept |
/// | `{repository_escaped}`, `{path_escaped}` | The same with `/` escaped, for APIs that take one path segment |
/// | `{commit}` | The full commit id |
///
/// | Forge | Template |
/// |---|---|
/// | GitHub | `https://raw.githubusercontent.com/{repository}/{commit}/{path}` |
/// | GitLab | `https://gitlab.com/api/v4/projects/{repository_escaped}/repository/files/{path_escaped}/raw?ref={commit}` |
///
/// Redirects are not followed, so a credential never reaches another host.
///
/// # Example
///
/// ```rust
/// use adk_website::repository::source::{CommitSource, HttpCommitSource};
///
/// let source = HttpCommitSource::new("https://raw.githubusercontent.com/{repository}/{commit}/{path}")?;
/// let url = source.url("juniper/site", &"a".repeat(40), "content/pages/hours/en.md")?;
/// assert!(url.ends_with("/juniper/site/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/content/pages/hours/en.md"));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone)]
pub struct HttpCommitSource {
    template: String,
    auth: Option<(reqwest::header::HeaderName, CredentialSecret)>,
    client: reqwest::Client,
}

impl std::fmt::Debug for HttpCommitSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCommitSource")
            .field("template", &self.template)
            .field("auth", &self.auth.as_ref().map(|(header, _)| header.as_str()))
            .finish_non_exhaustive()
    }
}

impl HttpCommitSource {
    /// A source reading `template`, which names `{commit}` and `{path}` (or `{path_escaped}`).
    ///
    /// # Errors
    ///
    /// A template without those placeholders, or not an `http` or `https` URL.
    pub fn new(template: impl Into<String>) -> Result<Self> {
        let template = template.into();
        let names_path = template.contains("{path}") || template.contains("{path_escaped}");
        if !names_path || !template.contains("{commit}") {
            return Err(refused(format!(
                "the source URL `{template}` must name {{commit}} and {{path}} (or {{path_escaped}})"
            )));
        }
        if !(template.starts_with("https://") || template.starts_with("http://")) {
            return Err(refused(format!("the source URL `{template}` is not http or https")));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| refused(e.to_string()))?;
        Ok(Self { template, auth: None, client })
    }

    /// Sends `value` in `header` with every request, such as `Authorization` or GitLab's
    /// `PRIVATE-TOKEN`.
    ///
    /// # Errors
    ///
    /// A header name that is not valid.
    pub fn with_auth(mut self, header: &str, value: CredentialSecret) -> Result<Self> {
        let header = reqwest::header::HeaderName::from_bytes(header.as_bytes())
            .map_err(|e| refused(format!("`{header}` is not a header name: {e}")))?;
        self.auth = Some((header, value));
        Ok(self)
    }

    /// The URL a file is read from.
    ///
    /// # Errors
    ///
    /// A commit id or path the source refuses.
    pub fn url(&self, repository: &str, commit: &str, path: &str) -> Result<String> {
        let path = checked(commit, path)?;
        if repository.split('/').any(|part| part == "..") {
            return Err(refused(format!("`{repository}` is not a repository name")));
        }
        Ok(self
            .template
            .replace("{repository_escaped}", &escape(repository, false))
            .replace("{repository}", &escape(repository, true))
            .replace("{path_escaped}", &escape(path, false))
            .replace("{path}", &escape(path, true))
            .replace("{commit}", commit))
    }
}

#[async_trait]
impl CommitSource for HttpCommitSource {
    fn name(&self) -> &str {
        "http"
    }

    async fn read(&self, repository: &str, commit: &str, path: &str) -> Result<Option<Bytes>> {
        let url = self.url(repository, commit, path)?;
        let mut request = self.client.get(&url);
        if let Some((header, value)) = &self.auth {
            let mut value = reqwest::header::HeaderValue::from_str(value.expose())
                .map_err(|_| refused("the source's credential is not a header value"))?;
            value.set_sensitive(true);
            request = request.header(header.clone(), value);
        }
        let mut response = request
            .send()
            .await
            .map_err(|e| refused(format!("cannot read {path} at {commit}: {e}")))?;
        match response.status() {
            status if status.is_success() => {}
            reqwest::StatusCode::NOT_FOUND => return Ok(None),
            status => {
                return Err(refused(format!("reading {path} at {commit} answered {status}")));
            }
        }
        if response.content_length().is_some_and(|len| len > MAX_FILE_BYTES as u64) {
            return Err(refused(format!("{path} is over {MAX_FILE_BYTES} bytes")));
        }
        let mut body = Vec::new();
        while let Some(chunk) =
            response.chunk().await.map_err(|e| refused(format!("reading {path}: {e}")))?
        {
            if body.len() + chunk.len() > MAX_FILE_BYTES {
                return Err(refused(format!("{path} is over {MAX_FILE_BYTES} bytes")));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(Bytes::from(body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_escape_names_for_each_forge() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let gitlab = HttpCommitSource::new(
            "https://gitlab.example/api/v4/projects/{repository_escaped}/repository/files/{path_escaped}/raw?ref={commit}",
        )
        .unwrap();
        assert_eq!(
            gitlab.url("news/site", commit, "content/articles/a b/en.md").unwrap(),
            format!(
                "https://gitlab.example/api/v4/projects/news%2Fsite/repository/files/content%2Farticles%2Fa%20b%2Fen.md/raw?ref={commit}"
            )
        );
        assert!(gitlab.url("news/site", "main", "content/x/en.md").is_err());
        assert!(gitlab.url("../site", commit, "content/x/en.md").is_err());
        assert!(HttpCommitSource::new("https://example/{repository}/{path}").is_err());
        assert!(HttpCommitSource::new("file:///{commit}/{path}").is_err());
    }

    #[test]
    fn only_relative_paths_inside_the_repository_are_read() {
        let commit = "a".repeat(64);
        for path in ["content/pages/a/en.md", "README.md"] {
            assert!(checked(&commit, path).is_ok(), "{path}");
        }
        for path in ["", "/etc/passwd", "../x", "content/../x", "a//b", "-c", "a\\b", "a/./b"] {
            assert!(checked(&commit, path).is_err(), "{path}");
        }
    }
}
