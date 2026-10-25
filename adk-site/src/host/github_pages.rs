//! GitHub Pages, or any git-backed pages host: each release is one commit pushed to the
//! pages branch (spec 3.5 §7.8 "Hosts", feature `host-github-pages`).
//!
//! The host keeps a bare repository as a cache, writes a release's changed files into a
//! commit on top of the branch with `git fast-import` (one process, no checkout), and pushes
//! it without force, so a reader of the branch sees the previous commit or the next one
//! ([`Atomicity::Deploy`]) and a concurrent publisher's push is refused rather than
//! overwritten. A rollback commits an earlier release's blobs, read from the branch history
//! and checked against its manifest, with the log as it stands.
//!
//! | Branch file | Why |
//! |-------------|-----|
//! | `.nojekyll` | GitHub Pages skips Jekyll, which would drop `.well-known/` |
//! | `CNAME` | The custom domain, when [`with_cname`](GitPagesHost::with_cname) sets one |
//!
//! GitHub Pages builds from a branch at most about ten times an hour (its documented soft
//! limit), which [`SiteHost::limits`] declares; it has no redirect rules, so redirects are
//! [`RedirectsFormat::HtmlMeta`] pages. [`fetch`](SiteHost::fetch) reads the branch: what the
//! host holds, which Pages serves once its deployment finishes.
//!
//! An HTTPS remote takes a token from a [`HostCredential`] bound to the remote's URL; git
//! receives it as an `http.extraHeader` in its environment, never in a URL or argument.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use awp_types::{Atomicity, FileEntry, HostLimits, ReleaseManifest};
use base64::Engine as _;
use bytes::Bytes;
use sha1::Sha1;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::keys::site_key;
use super::{HostCredential, HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::CHECKPOINT_PATH;
use crate::redirects::RedirectsFormat;
use crate::release::{
    StagedDelta, StagedRelease, file_digest, is_log_path, manifest_digest, manifest_path,
};

const NAME: &str = "github-pages";
const MESSAGE: &str = "adk-site release";
const NOJEKYLL: &str = ".nojekyll";
const CNAME: &str = "CNAME";
const STAGING_REF: &str = "refs/adk-site/staging";

/// A git object id of a blob holding `bytes` (SHA-1 object format).
fn blob_id(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A path in `git fast-import`'s quoted form.
fn quote(path: &str) -> String {
    let mut out = String::from("\"");
    for ch in path.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A pages branch in a git repository as a [`SiteHost`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::host::github_pages::GitPagesHost;
/// use adk_site::host::SiteHost;
///
/// # fn demo(token: adk_site::host::HostCredential) {
/// let host = GitPagesHost::new("https://github.com/example/site.git", "gh-pages")
///     .with_credential(token)
///     .with_preview_branch("gh-pages-preview")
///     .with_cname("www.example.com");
/// assert_eq!(host.limits().max_publishes_per_hour, Some(10));
/// # }
/// ```
#[derive(Debug)]
pub struct GitPagesHost {
    remote: String,
    branch: String,
    preview_branch: Option<String>,
    credential: Option<HostCredential>,
    work_dir: PathBuf,
    committer: (String, String),
    cname: Option<String>,
    git: PathBuf,
    lock: tokio::sync::Mutex<()>,
    fetch_lock: tokio::sync::Mutex<()>,
}

/// What a git command printed.
struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

impl GitPagesHost {
    /// Publishes to `branch` of the repository at `remote` (a URL or a local path).
    pub fn new(remote: impl Into<String>, branch: impl Into<String>) -> Self {
        let remote = remote.into();
        let branch = branch.into();
        let cache = hex::encode(&Sha256::digest(format!("{remote}\n{branch}").as_bytes())[..8]);
        Self {
            work_dir: std::env::temp_dir().join("adk-site-git").join(cache),
            remote,
            branch,
            preview_branch: None,
            credential: None,
            committer: ("adk-site".into(), "adk-site@users.noreply.github.com".into()),
            cname: None,
            git: PathBuf::from("git"),
            lock: tokio::sync::Mutex::new(()),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Authenticates HTTPS pushes and fetches with a token for the remote's URL.
    #[must_use]
    pub fn with_credential(mut self, credential: HostCredential) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Publishes previews to `branch`; without one a preview publish fails.
    #[must_use]
    pub fn with_preview_branch(mut self, branch: impl Into<String>) -> Self {
        self.preview_branch = Some(branch.into());
        self
    }

    /// Keeps the bare cache repository at `dir` instead of under the temporary directory.
    #[must_use]
    pub fn with_work_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.work_dir = dir.into();
        self
    }

    /// Commits as `name <email>`.
    #[must_use]
    pub fn with_committer(mut self, name: impl Into<String>, email: impl Into<String>) -> Self {
        self.committer = (name.into(), email.into());
        self
    }

    /// Writes `domain` to the branch's `CNAME` file.
    #[must_use]
    pub fn with_cname(mut self, domain: impl Into<String>) -> Self {
        self.cname = Some(domain.into());
        self
    }

    /// Runs `git` from `path` instead of the one on `PATH`.
    #[must_use]
    pub fn with_git(mut self, path: impl Into<PathBuf>) -> Self {
        self.git = path.into();
        self
    }

    /// The bare cache repository.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    fn tracking(branch: &str) -> String {
        format!("refs/remotes/pages/{branch}")
    }

    async fn auth_env(&self) -> Result<Vec<(String, String)>> {
        let Some(credential) = &self.credential else { return Ok(Vec::new()) };
        let url: adk_core::Url = self
            .remote
            .parse()
            .map_err(|e| SiteError::host(NAME, format!("a credential needs a URL remote: {e}")))?;
        let token = credential.secret_for(NAME, &url).await?;
        let basic = base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", token.expose()));
        Ok(vec![
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            ("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into()),
            ("GIT_CONFIG_VALUE_0".into(), format!("Authorization: Basic {basic}")),
        ])
    }

    async fn run(&self, args: &[&str], stdin: Option<Vec<u8>>, remote: bool) -> Result<Output> {
        let mut command = tokio::process::Command::new(&self.git);
        command
            .arg("--git-dir")
            .arg(&self.work_dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if remote {
            command.envs(self.auth_env().await?);
        }
        let mut child = command.spawn().map_err(|e| {
            SiteError::host(NAME, format!("cannot run {}: {e}", self.git.display()))
        })?;
        let writer = match (stdin, child.stdin.take()) {
            (Some(bytes), Some(mut pipe)) => Some(tokio::spawn(async move {
                let written = pipe.write_all(&bytes).await;
                drop(pipe);
                written
            })),
            _ => None,
        };
        let output = child.wait_with_output().await.map_err(|e| {
            SiteError::host(NAME, format!("git {}: {e}", args.first().unwrap_or(&"")))
        })?;
        if let Some(writer) = writer {
            writer
                .await
                .map_err(|e| SiteError::host(NAME, e.to_string()))?
                .map_err(|e| SiteError::host(NAME, format!("git {}: stdin: {e}", args[0])))?;
        }
        Ok(Output {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    async fn checked(
        &self,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        remote: bool,
    ) -> Result<Vec<u8>> {
        let output = self.run(args, stdin, remote).await?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(SiteError::host(NAME, format!("git {} failed: {}", args[0], output.stderr)))
        }
    }

    async fn ensure_repo(&self) -> Result<()> {
        if self.work_dir.join("HEAD").exists() {
            return Ok(());
        }
        tokio::fs::create_dir_all(&self.work_dir)
            .await
            .map_err(|e| SiteError::io(&self.work_dir, e))?;
        let output = tokio::process::Command::new(&self.git)
            .args(["init", "--bare", "--quiet"])
            .arg(&self.work_dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| {
                SiteError::host(NAME, format!("cannot run {}: {e}", self.git.display()))
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(SiteError::host(
                NAME,
                format!("git init failed: {}", String::from_utf8_lossy(&output.stderr).trim()),
            ))
        }
    }

    /// The commit `branch` points at on the remote, if the branch exists.
    async fn remote_head(&self, branch: &str) -> Result<Option<String>> {
        let refname = format!("refs/heads/{branch}");
        let out = self.checked(&["ls-remote", "--", &self.remote, &refname], None, true).await?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .find(|line| line.ends_with(&refname))
            .and_then(|line| line.split_whitespace().next())
            .map(String::from))
    }

    /// Fetches `branch` into the cache when its head is not there; returns the head.
    async fn sync(&self, branch: &str) -> Result<Option<String>> {
        self.ensure_repo().await?;
        let Some(head) = self.remote_head(branch).await? else { return Ok(None) };
        let probe = format!("{head}^{{commit}}");
        if !self.run(&["cat-file", "-e", &probe], None, false).await?.success {
            let _guard = self.fetch_lock.lock().await;
            let refspec = format!("+refs/heads/{branch}:{}", Self::tracking(branch));
            self.checked(
                &["fetch", "--quiet", "--no-tags", "--", &self.remote, &refspec],
                None,
                true,
            )
            .await?;
        }
        Ok(Some(head))
    }

    /// Every file in `commit`'s tree: path to blob id.
    async fn tree(&self, commit: &str) -> Result<BTreeMap<String, String>> {
        let out =
            self.checked(&["ls-tree", "-r", "-z", "--full-tree", commit], None, false).await?;
        let mut files = BTreeMap::new();
        for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let Some((meta, path)) = entry.split_once('\t') else { continue };
            let mut parts = meta.split_whitespace();
            if let (Some(_mode), Some("blob"), Some(id)) =
                (parts.next(), parts.next(), parts.next())
            {
                files.insert(format!("/{path}"), id.to_string());
            }
        }
        Ok(files)
    }

    /// The bytes of `path` in `commit`; `None` when the tree has no such file.
    async fn blob(&self, commit: &str, path: &str) -> Result<Option<Bytes>> {
        let object = format!("{commit}:{}", path.trim_start_matches('/'));
        let output = self.run(&["cat-file", "blob", &object], None, false).await?;
        if output.success {
            return Ok(Some(Bytes::from(output.stdout)));
        }
        if output.stderr.contains("does not exist")
            || output.stderr.contains("Not a valid object name")
        {
            return Ok(None);
        }
        Err(SiteError::host(NAME, format!("git cat-file {path}: {}", output.stderr)))
    }

    /// Reads many blobs of `commit` with one `git cat-file --batch`.
    async fn blobs(
        &self,
        commit: &str,
        paths: &[&String],
    ) -> Result<BTreeMap<String, Option<Bytes>>> {
        let mut input = String::new();
        for path in paths {
            input.push_str(&format!("{commit}:{}\n", path.trim_start_matches('/')));
        }
        let mut child = tokio::process::Command::new(&self.git)
            .arg("--git-dir")
            .arg(&self.work_dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SiteError::host(NAME, format!("cannot run git: {e}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| SiteError::host(NAME, "no stdin"))?;
        let writer = tokio::spawn(async move { stdin.write_all(input.as_bytes()).await });
        let mut stdout = child.stdout.take().ok_or_else(|| SiteError::host(NAME, "no stdout"))?;
        let mut out = Vec::new();
        stdout
            .read_to_end(&mut out)
            .await
            .map_err(|e| SiteError::host(NAME, format!("git cat-file: {e}")))?;
        writer
            .await
            .map_err(|e| SiteError::host(NAME, e.to_string()))?
            .map_err(|e| SiteError::host(NAME, e.to_string()))?;
        child.wait().await.map_err(|e| SiteError::host(NAME, e.to_string()))?;
        let mut result = BTreeMap::new();
        let mut at = 0;
        for path in paths {
            let end = out[at..]
                .iter()
                .position(|b| *b == b'\n')
                .map(|p| p + at)
                .ok_or_else(|| SiteError::host(NAME, "git cat-file --batch ended early"))?;
            let header = String::from_utf8_lossy(&out[at..end]).to_string();
            at = end + 1;
            if header.ends_with(" missing") {
                result.insert((*path).clone(), None);
                continue;
            }
            let size: usize =
                header.rsplit(' ').next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                    SiteError::host(NAME, format!("unexpected cat-file header '{header}'"))
                })?;
            result.insert((*path).clone(), Some(Bytes::copy_from_slice(&out[at..at + size])));
            at += size + 1;
        }
        Ok(result)
    }

    /// Commits `writes` and `deletes` on top of `parent` with one `git fast-import`.
    async fn commit(
        &self,
        parent: Option<&str>,
        message: &str,
        writes: &[(String, Write)],
        deletes: &[String],
    ) -> Result<String> {
        let (name, email) = &self.committer;
        let now = chrono::Utc::now().timestamp();
        let mut stream = Vec::new();
        stream.extend_from_slice(
            format!("commit {STAGING_REF}\ncommitter {name} <{email}> {now} +0000\ndata {}\n{message}\n", message.len())
                .as_bytes(),
        );
        if let Some(parent) = parent {
            stream.extend_from_slice(format!("from {parent}\n").as_bytes());
        }
        for (path, write) in writes {
            let path = quote(path.trim_start_matches('/'));
            match write {
                Write::Inline(bytes) => {
                    stream.extend_from_slice(
                        format!("M 100644 inline {path}\ndata {}\n", bytes.len()).as_bytes(),
                    );
                    stream.extend_from_slice(bytes);
                    stream.push(b'\n');
                }
                Write::Blob(id) => {
                    stream.extend_from_slice(format!("M 100644 {id} {path}\n").as_bytes())
                }
            }
        }
        for path in deletes {
            stream.extend_from_slice(
                format!("D {}\n", quote(path.trim_start_matches('/'))).as_bytes(),
            );
        }
        stream.extend_from_slice(b"done\n");
        self.checked(&["fast-import", "--quiet", "--force", "--done"], Some(stream), false).await?;
        let out = self.checked(&["rev-parse", "--verify", STAGING_REF], None, false).await?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    async fn push(&self, commit: &str, branch: &str, force: bool) -> Result<()> {
        let refspec = format!("{}{commit}:refs/heads/{branch}", if force { "+" } else { "" });
        self.checked(&["push", "--quiet", "--", &self.remote, &refspec], None, true).await?;
        self.checked(&["update-ref", &Self::tracking(branch), commit], None, false).await?;
        Ok(())
    }

    fn host_files(&self) -> Vec<(String, Vec<u8>)> {
        let mut files = vec![(format!("/{NOJEKYLL}"), Vec::new())];
        if let Some(domain) = &self.cname {
            files.push((format!("/{CNAME}"), format!("{domain}\n").into_bytes()));
        }
        files
    }

    fn is_host_file(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        path == NOJEKYLL || (path == CNAME && self.cname.is_some())
    }

    async fn publish_locked(
        &self,
        release: &StagedRelease,
        target: HostTarget,
    ) -> Result<PublishReport> {
        let start = Instant::now();
        let seq = release.manifest.seq;
        let branch = match target {
            HostTarget::Production => self.branch.clone(),
            HostTarget::Preview => self.preview_branch.clone().ok_or_else(|| {
                SiteError::host(
                    NAME,
                    "no preview branch is configured; set one with with_preview_branch",
                )
            })?,
        };
        // A preview starts from production, so it carries the log.
        let parent = self.sync(&self.branch).await?;
        let tree = match &parent {
            Some(parent) => self.tree(parent).await?,
            None => BTreeMap::new(),
        };
        let manifest = format!("/{}", manifest_path(seq));
        if target == HostTarget::Production {
            if tree.contains_key(&manifest) {
                return Err(SiteError::Sequence(format!(
                    "release {seq} is already published on this host"
                )));
            }
            if let (Some(parent), Some(previous)) = (&parent, release.manifest.previous.as_ref())
                && let Some(bytes) = self.blob(parent, &manifest_path(seq - 1)).await?
                && file_digest(&bytes) != *previous
            {
                return Err(SiteError::Sequence(format!(
                    "release {seq} follows a manifest this host does not serve as release {}",
                    seq - 1
                )));
            }
        }
        let mut candidates: Vec<(String, Vec<u8>)> = self.host_files();
        for path in release.files.keys() {
            candidates.push((path.clone(), release.read(path)?));
        }
        candidates.push((manifest, release.manifest_bytes.to_vec()));
        let (checkpoint, tiles): (Vec<_>, Vec<_>) = release
            .log
            .iter()
            .flat_map(|log| log.files())
            .map(|(p, b)| (format!("/{p}"), b.to_vec()))
            .partition(|(p, _)| p.trim_start_matches('/') == CHECKPOINT_PATH);
        candidates.extend(tiles);
        candidates.extend(checkpoint);
        let mut writes = Vec::new();
        let mut uploaded = Vec::new();
        for (path, bytes) in candidates {
            if tree.get(&path) != Some(&blob_id(&bytes)) {
                uploaded.push(path.clone());
                writes.push((path, Write::Inline(bytes)));
            }
        }
        let deletes: Vec<String> = tree
            .keys()
            .filter(|p| !is_log_path(p) && !self.is_host_file(p) && !release.files.contains_key(*p))
            .cloned()
            .collect();
        let message = format!("{MESSAGE} {seq} {}", release.digest);
        let commit = self.commit(parent.as_deref(), &message, &writes, &deletes).await?;
        self.push(&commit, &branch, target == HostTarget::Preview).await?;
        tracing::info!(host = NAME, site.release.seq = seq, commit = %commit, "release pushed");
        Ok(PublishReport {
            host: NAME.into(),
            target,
            seq,
            uploaded,
            removed: deletes,
            elapsed: start.elapsed(),
        })
    }

    async fn rollback_locked(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let not_found = |message: &str| SiteError::ReleaseNotFound {
            seq: to.seq,
            message: message.to_string(),
        };
        let head = self
            .sync(&self.branch)
            .await?
            .ok_or_else(|| not_found("the pages branch does not exist"))?;
        let log =
            self.checked(&["log", "--first-parent", "--format=%H %s", &head], None, false).await?;
        let prefix = format!("{MESSAGE} {} ", to.seq);
        let stored = String::from_utf8_lossy(&log)
            .lines()
            .find_map(|line| {
                line.split_once(' ')
                    .filter(|(_, s)| s.starts_with(&prefix))
                    .map(|(c, _)| c.to_string())
            })
            .ok_or_else(|| not_found("no commit of it on the pages branch"))?;
        let read_manifest = |seq: u64| {
            let stored = &stored;
            async move {
                let bytes = self
                    .blob(stored, &manifest_path(seq))
                    .await?
                    .ok_or_else(|| not_found("its commit has no manifest"))?;
                serde_json::from_slice::<ReleaseManifest>(&bytes)
                    .map_err(|e| SiteError::serde(manifest_path(seq), e))
            }
        };
        let own = read_manifest(to.seq).await?;
        let (expected, _) = manifest_digest(to)?;
        let (actual, _) = manifest_digest(&own)?;
        if expected != actual {
            return Err(SiteError::DigestMismatch {
                path: format!("/{}", manifest_path(to.seq)),
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        let mut chain = vec![own];
        while let Some(base) = chain.last().and_then(|m| m.base) {
            chain.push(read_manifest(base).await?);
        }
        let mut files: BTreeMap<String, FileEntry> = BTreeMap::new();
        for manifest in chain.iter().rev() {
            files = manifest.apply_to(&files);
        }
        let paths: Vec<&String> = files.keys().collect();
        let blobs = self.blobs(&stored, &paths).await?;
        let mut writes = Vec::new();
        let current = self.tree(&head).await?;
        for (path, entry) in &files {
            let bytes = blobs.get(path).cloned().flatten();
            let actual = bytes
                .as_ref()
                .map_or_else(|| "missing".to_string(), |b| file_digest(b).to_string());
            if actual != entry.sha256.as_str() {
                return Err(SiteError::DigestMismatch {
                    path: path.clone(),
                    expected: entry.sha256.to_string(),
                    actual,
                });
            }
            let id = blob_id(bytes.as_deref().unwrap_or_default());
            if current.get(path) != Some(&id) {
                writes.push((path.clone(), Write::Blob(id)));
            }
        }
        let deletes: Vec<String> = current
            .keys()
            .filter(|p| !is_log_path(p) && !self.is_host_file(p) && !files.contains_key(*p))
            .cloned()
            .collect();
        let commit = self
            .commit(Some(&head), &format!("adk-site rollback to {}", to.seq), &writes, &deletes)
            .await?;
        self.push(&commit, &self.branch, false).await?;
        tracing::warn!(host = NAME, site.rollback.to = to.seq, commit = %commit, "rolled back");
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded: files.keys().cloned().collect(),
            removed: deletes,
            elapsed: start.elapsed(),
        })
    }
}

/// A file a commit writes: new bytes, or a blob the repository holds.
enum Write {
    Inline(Vec<u8>),
    Blob(String),
}

#[async_trait]
impl SiteHost for GitPagesHost {
    fn name(&self) -> &str {
        NAME
    }

    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.publish_locked(release, target).await
    }

    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.rollback_locked(to).await
    }

    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let key = site_key(NAME, path)?;
        let Some(head) = self.sync(&self.branch).await? else { return Ok(None) };
        self.blob(&head, &key).await
    }

    fn redirects_format(&self) -> RedirectsFormat {
        RedirectsFormat::HtmlMeta
    }

    fn limits(&self) -> HostLimits {
        HostLimits {
            max_publishes_per_hour: Some(10),
            delta_upload: true,
            atomic: Atomicity::Deploy,
        }
    }

    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> Result<PublishReport> {
        self.publish(delta.release(), target).await
    }
}

#[cfg(test)]
mod tests {
    use super::{blob_id, quote};

    #[test]
    fn blob_ids_match_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(blob_id(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a");
        assert_eq!(blob_id(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn paths_are_quoted_for_fast_import() {
        assert_eq!(quote("a b/\"c\".html"), "\"a b/\\\"c\\\".html\"");
        assert_eq!(quote("x\ny"), "\"x\\ny\"");
    }
}
