//! Any server reachable over SSH: release directories copied with `rsync`, switched by a
//! symbolic link (spec 3.5 §7.8 "Hosts", feature `host-rsync`).
//!
//! | Path under the server root | Holds |
//! |----------------------------|-------|
//! | `releases/<seq>/` | A production release: its files, every manifest so far and the log |
//! | `releases/rollback-<seq>-<n>/` | A rollback: release `<seq>`'s files with the log as it stands |
//! | `previews/<seq>-<digest>/` | A preview release |
//! | `releases/<dir>.index.json` | The digest of every file of a release directory |
//! | `current`, `preview` | Symbolic links the web server's document root follows |
//!
//! A publish sends the release to a staging directory with `rsync --checksum --link-dest`,
//! so unchanged files become hard links to the serving release and only changed files
//! cross the network, links the log forward, renames the directory into place and swaps
//! `current` with one `rename(2)` ([`Atomicity::Deploy`]). Point the web server's document
//! root at `<root>/current`. The server needs `rsync`, a POSIX shell, and `sha256sum` or
//! `shasum`.
//!
//! The SSH identity comes from a [`HostCredential`] holding an OpenSSH private key; `ssh`
//! reads it from a temporary file readable only by this user and removed after each
//! command. Host keys are always checked (`StrictHostKeyChecking=yes`), against
//! [`with_known_hosts`](RsyncHost::with_known_hosts) when set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use awp_types::{Atomicity, Digest, FileEntry, HostLimits, ReleaseManifest};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use super::keys::site_key;
use super::{HostCredential, HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::CHECKPOINT_PATH;
use crate::redirects::RedirectsFormat;
use crate::release::{
    StagedDelta, StagedRelease, file_digest, is_log_path, manifest_digest, manifest_path,
};

const NAME: &str = "rsync";

/// Prints a file under the root; exit 3 when it is missing, 4 when it is not a file.
const READ: &str = r#"f="$1/$2"; if [ -f "$f" ]; then exec cat -- "$f"; elif [ -e "$f" ]; then echo "'$2' is not a file" >&2; exit 4; else exit 3; fi"#;

/// Prints where a link under the root points; exit 3 when there is no link.
const READLINK: &str = r#"if [ -L "$1/$2" ]; then readlink -- "$1/$2"; else exit 3; fi"#;

/// Creates the root's directories; exit 3 when `$2` (a release directory) already exists.
const PREPARE: &str =
    r#"mkdir -p -- "$1/releases" "$1/previews" && if [ -e "$1/$2" ]; then exit 3; fi"#;

/// Writes stdin to a file under the root, replacing it atomically.
const WRITE: &str = r#"set -e; t="$1/$2.tmp.$$"; cat > "$t"; mv -f -- "$t" "$1/$2""#;

/// Links the log and manifests of `$3` (the serving directory, or empty) into the staging
/// directory `$2` where it lacks them, renames it to `$4`, and points the link `$5` at it
/// with one rename: `mv -T` (GNU, BusyBox), else `mv -h` (BSD, macOS).
const FINISH: &str = r#"set -e
root="$1"; stage="$2"; cur="$3"; final="$4"; link="$5"
cd "$root"
if [ -n "$cur" ]; then
  for d in .well-known/awp/releases .well-known/awp/log; do
    if [ -d "$cur/$d" ]; then
      mkdir -p "$stage/$d"
      rsync -r --perms --ignore-existing --link-dest="$root/$cur/$d/" "$cur/$d/" "$stage/$d/"
    fi
  done
fi
if [ "$stage" != "$final" ]; then mv -- "$stage" "$final"; fi
tmp=".$link.$$"
rm -f -- "$tmp"
ln -s -- "$final" "$tmp"
mv -T -- "$tmp" "$link" 2>/dev/null || mv -h -- "$tmp" "$link""#;

/// Hard-links the files listed on stdin from release directory `$2` into a new directory
/// `$3`.
const LINK_FILES: &str = r#"set -e; mkdir -p -- "$1/$3"; rsync -r --perms --files-from=- --link-dest="$1/$2/" "$1/$2/" "$1/$3/""#;

/// Prints `<sha256> <path>` for each path on stdin under directory `$2`, `missing <path>`
/// for an absent one.
const HASH: &str = r#"cd -- "$1/$2" || exit 3
while IFS= read -r p; do
  if [ -f "$p" ]; then
    s=$( (sha256sum -- "$p" 2>/dev/null || shasum -a 256 -- "$p") | cut -d' ' -f1)
    printf '%s %s\n' "$s" "$p"
  else
    printf 'missing %s\n' "$p"
  fi
done"#;

/// Where releases go.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RsyncTarget {
    /// A server over SSH: `destination` is `[user@]host`, `root` an absolute directory.
    Ssh {
        /// `[user@]host`.
        destination: String,
        /// The SSH port, when not 22.
        port: Option<u16>,
        /// The absolute directory the releases live in.
        root: String,
    },
    /// A directory on this machine, such as a mounted server volume; the same commands run
    /// locally.
    Local {
        /// The directory the releases live in.
        root: PathBuf,
    },
}

/// The digest of every file under a release directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirIndex {
    files: BTreeMap<String, Digest>,
}

/// A temporary file removed on drop.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A name no other temporary file of this process has.
fn unique(prefix: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("adk-site-{prefix}-{}-{}-{n}", std::process::id(), nanos())
}

fn private_file(prefix: &str, contents: &[u8]) -> Result<TempFile> {
    let path = std::env::temp_dir().join(unique(prefix));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| SiteError::io(&path, e))?;
    std::io::Write::write_all(&mut file, contents).map_err(|e| SiteError::io(&path, e))?;
    Ok(TempFile(path))
}

/// `text` as one single-quoted POSIX shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// A server's release directories as a [`SiteHost`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::host::rsync::{RsyncHost, RsyncTarget};
/// use adk_site::host::SiteHost;
/// use awp_types::Atomicity;
///
/// # fn demo(key: adk_site::host::HostCredential) {
/// let host = RsyncHost::new(RsyncTarget::Ssh {
///     destination: "deploy@www.example.com".into(),
///     port: None,
///     root: "/srv/example.com".into(),
/// })
/// .with_identity(key)
/// .with_known_hosts("www.example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI...");
/// assert_eq!(host.limits().atomic, Atomicity::Deploy);
/// # }
/// ```
#[derive(Debug)]
pub struct RsyncHost {
    target: RsyncTarget,
    identity: Option<HostCredential>,
    known_hosts: Option<String>,
    rsync: PathBuf,
    ssh: PathBuf,
    redirects: RedirectsFormat,
    lock: tokio::sync::Mutex<()>,
}

/// The SSH files one command uses, removed when it ends.
struct SshFiles {
    identity: Option<TempFile>,
    known_hosts: Option<TempFile>,
}

impl RsyncHost {
    /// Publishes to `target`.
    pub fn new(target: RsyncTarget) -> Self {
        Self {
            target,
            identity: None,
            known_hosts: None,
            rsync: PathBuf::from("rsync"),
            ssh: PathBuf::from("ssh"),
            redirects: RedirectsFormat::HtmlMeta,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Authenticates with the OpenSSH private key `credential` holds; without one, `ssh`
    /// uses the agent and its own configuration.
    #[must_use]
    pub fn with_identity(mut self, credential: HostCredential) -> Self {
        self.identity = Some(credential);
        self
    }

    /// Accepts only these host keys (`known_hosts` lines) for the server.
    #[must_use]
    pub fn with_known_hosts(mut self, lines: impl Into<String>) -> Self {
        self.known_hosts = Some(lines.into());
        self
    }

    /// Runs `rsync` and `ssh` from these paths instead of the ones on `PATH`.
    #[must_use]
    pub fn with_programs(mut self, rsync: impl Into<PathBuf>, ssh: impl Into<PathBuf>) -> Self {
        self.rsync = rsync.into();
        self.ssh = ssh.into();
        self
    }

    /// The redirect format the web server reads; [`RedirectsFormat::HtmlMeta`] by default,
    /// which needs no server rules.
    #[must_use]
    pub fn with_redirects_format(mut self, format: RedirectsFormat) -> Self {
        self.redirects = format;
        self
    }

    fn root(&self) -> Result<String> {
        match &self.target {
            RsyncTarget::Ssh { root, .. } => {
                if root.starts_with('/') {
                    Ok(root.trim_end_matches('/').to_string())
                } else {
                    Err(SiteError::host(
                        NAME,
                        format!("the server root '{root}' must be an absolute path"),
                    ))
                }
            }
            RsyncTarget::Local { root } => {
                std::fs::create_dir_all(root).map_err(|e| SiteError::io(root, e))?;
                let root = root.canonicalize().map_err(|e| SiteError::io(root, e))?;
                Ok(root.to_string_lossy().trim_end_matches('/').to_string())
            }
        }
    }

    async fn ssh_files(&self) -> Result<SshFiles> {
        let identity = match (&self.identity, &self.target) {
            (Some(credential), RsyncTarget::Ssh { destination, port, root }) => {
                let host = destination.rsplit('@').next().unwrap_or(destination);
                let url: adk_core::Url = format!("ssh://{host}:{}{root}", port.unwrap_or(22))
                    .parse()
                    .map_err(|e| SiteError::host(NAME, format!("bad SSH resource: {e}")))?;
                let key = credential.secret_for(NAME, &url).await?;
                let mut key = key.expose().trim_end().to_string();
                key.push('\n');
                Some(private_file("ssh-key", key.as_bytes())?)
            }
            _ => None,
        };
        let known_hosts = self
            .known_hosts
            .as_ref()
            .map(|lines| private_file("known-hosts", lines.as_bytes()))
            .transpose()?;
        Ok(SshFiles { identity, known_hosts })
    }

    /// The `ssh` arguments before the destination.
    fn ssh_args(&self, files: &SshFiles) -> Vec<String> {
        let mut args: Vec<String> =
            ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"].map(String::from).to_vec();
        if let Some(known) = &files.known_hosts {
            args.extend(["-o".into(), format!("UserKnownHostsFile={}", known.0.display())]);
        }
        if let Some(identity) = &files.identity {
            args.extend([
                "-o".into(),
                "IdentitiesOnly=yes".into(),
                "-i".into(),
                identity.0.display().to_string(),
            ]);
        }
        if let RsyncTarget::Ssh { port: Some(port), .. } = &self.target {
            args.extend(["-p".into(), port.to_string()]);
        }
        args
    }

    /// Runs `script` with `args` on the server (or locally), feeding `stdin`.
    async fn script(
        &self,
        script: &str,
        args: &[&str],
        stdin: Option<Vec<u8>>,
    ) -> Result<std::process::Output> {
        let files = self.ssh_files().await?;
        let mut command = match &self.target {
            RsyncTarget::Local { .. } => {
                let mut command = tokio::process::Command::new("sh");
                command.arg("-c").arg(script).arg("sh").args(args);
                command
            }
            RsyncTarget::Ssh { destination, .. } => {
                let mut remote = format!("sh -c {} sh", shell_quote(script));
                for arg in args {
                    remote.push(' ');
                    remote.push_str(&shell_quote(arg));
                }
                let mut command = tokio::process::Command::new(&self.ssh);
                command.args(self.ssh_args(&files)).arg("--").arg(destination).arg(remote);
                command
            }
        };
        command
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| SiteError::host(NAME, format!("cannot start a command: {e}")))?;
        let writer = match (stdin, child.stdin.take()) {
            (Some(bytes), Some(mut pipe)) => Some(tokio::spawn(async move {
                let written = pipe.write_all(&bytes).await;
                drop(pipe);
                written
            })),
            _ => None,
        };
        let output =
            child.wait_with_output().await.map_err(|e| SiteError::host(NAME, e.to_string()))?;
        if let Some(writer) = writer {
            writer
                .await
                .map_err(|e| SiteError::host(NAME, e.to_string()))?
                .map_err(|e| SiteError::host(NAME, e.to_string()))?;
        }
        drop(files);
        Ok(output)
    }

    async fn checked(
        &self,
        script: &str,
        args: &[&str],
        stdin: Option<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let output = self.script(script, args, stdin).await?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(SiteError::host(
                NAME,
                format!(
                    "server command failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ))
        }
    }

    /// A file under the root; `None` when missing.
    async fn read(&self, root: &str, path: &str) -> Result<Option<Bytes>> {
        let output = self.script(READ, &[root, path], None).await?;
        match output.status.code() {
            Some(0) => Ok(Some(Bytes::from(output.stdout))),
            Some(3) => Ok(None),
            _ => Err(SiteError::host(
                NAME,
                format!("read '{path}': {}", String::from_utf8_lossy(&output.stderr).trim()),
            )),
        }
    }

    async fn link(&self, root: &str, link: &str) -> Result<Option<String>> {
        let output = self.script(READLINK, &[root, link], None).await?;
        match output.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string())),
            Some(3) => Ok(None),
            _ => Err(SiteError::host(
                NAME,
                format!("readlink {link}: {}", String::from_utf8_lossy(&output.stderr).trim()),
            )),
        }
    }

    async fn index(&self, root: &str, dir: &str) -> Result<DirIndex> {
        match self.read(root, &format!("{dir}.index.json")).await? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| SiteError::serde(format!("{dir}.index.json"), e)),
            None => Ok(DirIndex::default()),
        }
    }

    async fn write_index(&self, root: &str, dir: &str, index: &DirIndex) -> Result<()> {
        let bytes = serde_json::to_vec(index).map_err(|e| SiteError::serde("release index", e))?;
        self.checked(WRITE, &[root, &format!("{dir}.index.json")], Some(bytes)).await.map(drop)
    }

    /// Sends the local directory `from` to `<root>/<to>/`, hard-linking files equal to those
    /// in `<root>/<link_dest>/`.
    async fn rsync(
        &self,
        from: &Path,
        root: &str,
        to: &str,
        link_dest: Option<&str>,
    ) -> Result<()> {
        let files = self.ssh_files().await?;
        let mut command = tokio::process::Command::new(&self.rsync);
        command.args(["-r", "--perms", "--checksum"]);
        if let Some(dir) = link_dest {
            command.arg(format!("--link-dest={root}/{dir}/"));
        }
        let destination = match &self.target {
            RsyncTarget::Local { .. } => format!("{root}/{to}/"),
            RsyncTarget::Ssh { destination, .. } => {
                let ssh = std::iter::once(self.ssh.display().to_string())
                    .chain(self.ssh_args(&files))
                    .collect::<Vec<_>>();
                if ssh.iter().any(|a| a.contains(char::is_whitespace)) {
                    return Err(SiteError::host(NAME, "ssh paths must not contain whitespace"));
                }
                command.arg("-e").arg(ssh.join(" "));
                format!("{destination}:{root}/{to}/")
            }
        };
        let output = command
            .arg(format!("{}/", from.display()))
            .arg(destination)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| {
                SiteError::host(NAME, format!("cannot run {}: {e}", self.rsync.display()))
            })?;
        drop(files);
        if output.status.success() {
            Ok(())
        } else {
            Err(SiteError::host(
                NAME,
                format!("rsync failed: {}", String::from_utf8_lossy(&output.stderr).trim()),
            ))
        }
    }

    async fn publish_locked(
        &self,
        release: &StagedRelease,
        target: HostTarget,
    ) -> Result<PublishReport> {
        let start = Instant::now();
        let root = self.root()?;
        let seq = release.manifest.seq;
        let (link, dir) = match target {
            HostTarget::Production => ("current", format!("releases/{seq}")),
            HostTarget::Preview => {
                ("preview", format!("previews/{seq}-{}", &release.digest.hex()[..12]))
            }
        };
        let report = |uploaded, removed| PublishReport {
            host: NAME.into(),
            target,
            seq,
            uploaded,
            removed,
            elapsed: start.elapsed(),
        };
        let prepared = self.script(PREPARE, &[&root, &dir], None).await?;
        match prepared.status.code() {
            Some(0) => {}
            Some(3) if target == HostTarget::Preview => {
                self.checked(FINISH, &[&root, &dir, "", &dir, link], None).await?;
                return Ok(report(Vec::new(), Vec::new()));
            }
            Some(3) => {
                return Err(SiteError::Sequence(format!(
                    "release {seq} is already published on this host"
                )));
            }
            _ => {
                return Err(SiteError::host(
                    NAME,
                    format!("prepare: {}", String::from_utf8_lossy(&prepared.stderr).trim()),
                ));
            }
        }
        // A preview links the production log forward.
        let current = self.link(&root, "current").await?;
        let current_index = match &current {
            Some(cur) => self.index(&root, cur).await?,
            None => DirIndex::default(),
        };
        if target == HostTarget::Production
            && let (Some(cur), Some(previous)) = (&current, release.manifest.previous.as_ref())
            && let Some(bytes) =
                self.read(&root, &format!("{cur}/{}", manifest_path(seq - 1))).await?
            && file_digest(&bytes) != *previous
        {
            return Err(SiteError::Sequence(format!(
                "release {seq} follows a manifest this host does not serve as release {}",
                seq - 1
            )));
        }
        let stage = tempfile_dir()?;
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for path in release.files.keys() {
            entries.push((path.clone(), release.read(path)?));
        }
        entries.push((format!("/{}", manifest_path(seq)), release.manifest_bytes.to_vec()));
        let (checkpoint, tiles): (Vec<_>, Vec<_>) = release
            .log
            .iter()
            .flat_map(|log| log.files())
            .map(|(p, b)| (format!("/{p}"), b.to_vec()))
            .partition(|(p, _)| p.trim_start_matches('/') == CHECKPOINT_PATH);
        entries.extend(tiles);
        entries.extend(checkpoint);
        let mut index = DirIndex::default();
        let mut uploaded = Vec::new();
        let mut dirs = std::collections::BTreeSet::from([stage.0.clone()]);
        for (path, bytes) in &entries {
            let local = stage.0.join(path.trim_start_matches('/'));
            if let Some(parent) = local.parent() {
                std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
                dirs.extend(
                    parent
                        .ancestors()
                        .take_while(|d| d.starts_with(&stage.0))
                        .map(Path::to_path_buf),
                );
            }
            std::fs::write(&local, bytes).map_err(|e| SiteError::io(&local, e))?;
            served_mode(&local, 0o644)?;
            let digest = file_digest(bytes);
            if current_index.files.get(path) != Some(&digest) {
                uploaded.push(path.clone());
            }
            index.files.insert(path.clone(), digest);
        }
        for (path, digest) in &current_index.files {
            if is_log_path(path) && !index.files.contains_key(path) {
                index.files.insert(path.clone(), digest.clone());
            }
        }
        for dir in &dirs {
            served_mode(dir, 0o755)?;
        }
        let staging = format!(
            "{}/.staging-{seq}-{}",
            if target == HostTarget::Production { "releases" } else { "previews" },
            nanos()
        );
        self.rsync(&stage.0, &root, &staging, current.as_deref()).await?;
        self.write_index(&root, &dir, &index).await?;
        self.checked(
            FINISH,
            &[&root, &staging, current.as_deref().unwrap_or(""), &dir, link],
            None,
        )
        .await?;
        let removed = current_index
            .files
            .keys()
            .filter(|p| !is_log_path(p) && !release.files.contains_key(*p))
            .cloned()
            .collect();
        tracing::info!(
            host = NAME,
            site.release.seq = seq,
            uploaded = uploaded.len(),
            "release published"
        );
        Ok(report(uploaded, removed))
    }

    async fn rollback_locked(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let root = self.root()?;
        let stored = format!("releases/{}", to.seq);
        let not_found = || SiteError::ReleaseNotFound {
            seq: to.seq,
            message: format!("not under {root}/{stored}"),
        };
        let read_manifest = |seq: u64| {
            let (root, stored) = (&root, &stored);
            async move {
                let bytes = self
                    .read(root, &format!("{stored}/{}", manifest_path(seq)))
                    .await?
                    .ok_or_else(not_found)?;
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
        if files.keys().any(|p| p.contains('\n')) {
            return Err(SiteError::host(
                NAME,
                "a file name holds a line break; roll back from the server's shell",
            ));
        }
        let list: String =
            files.keys().map(|p| format!("{}\n", p.trim_start_matches('/'))).collect();
        let hashes = self.checked(HASH, &[&root, &stored], Some(list.clone().into_bytes())).await?;
        let actual: BTreeMap<String, String> = String::from_utf8_lossy(&hashes)
            .lines()
            .filter_map(|line| line.split_once(' ').map(|(h, p)| (format!("/{p}"), h.to_string())))
            .collect();
        for (path, entry) in &files {
            let found = actual.get(path).map_or("missing", String::as_str);
            if found != entry.sha256.hex() {
                return Err(SiteError::DigestMismatch {
                    path: path.clone(),
                    expected: entry.sha256.to_string(),
                    actual: if found == "missing" {
                        found.to_string()
                    } else {
                        format!("sha256:{found}")
                    },
                });
            }
        }
        let current = self.link(&root, "current").await?.ok_or_else(not_found)?;
        let current_index = self.index(&root, &current).await?;
        let dir = format!("releases/rollback-{}-{}", to.seq, nanos());
        self.checked(LINK_FILES, &[&root, &stored, &dir], Some(list.into_bytes())).await?;
        let mut index = DirIndex::default();
        for (path, entry) in &files {
            index.files.insert(path.clone(), entry.sha256.clone());
        }
        for (path, digest) in current_index.files.iter().filter(|(p, _)| is_log_path(p)) {
            index.files.insert(path.clone(), digest.clone());
        }
        self.write_index(&root, &dir, &index).await?;
        self.checked(FINISH, &[&root, &dir, &current, &dir, "current"], None).await?;
        tracing::warn!(host = NAME, site.rollback.to = to.seq, "rolled back");
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded: files.keys().cloned().collect(),
            removed: current_index
                .files
                .keys()
                .filter(|p| !is_log_path(p) && !files.contains_key(*p))
                .cloned()
                .collect(),
            elapsed: start.elapsed(),
        })
    }
}

/// Gives a staged file or directory the mode the web server reads it with; `--perms`
/// carries it to the server whatever this process's umask.
fn served_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| SiteError::io(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// A local staging directory removed on drop.
struct StageDir(PathBuf);

impl Drop for StageDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempfile_dir() -> Result<StageDir> {
    let dir = std::env::temp_dir().join(unique("rsync"));
    std::fs::create_dir_all(&dir).map_err(|e| SiteError::io(&dir, e))?;
    Ok(StageDir(dir))
}

#[async_trait]
impl SiteHost for RsyncHost {
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
        self.read(&self.root()?, &format!("current/{key}")).await
    }

    fn redirects_format(&self) -> RedirectsFormat {
        self.redirects
    }

    fn limits(&self) -> HostLimits {
        HostLimits { max_publishes_per_hour: None, delta_upload: true, atomic: Atomicity::Deploy }
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
    use super::shell_quote;

    #[test]
    fn shell_words_survive_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
