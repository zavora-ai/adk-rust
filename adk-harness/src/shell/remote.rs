//! The remote shell (feature `remote-shell`): `bash` in an environment a
//! [`SandboxClient`] provides off this host — an E2B or Daytona microVM, or any other
//! client.
//!
//! One environment per (app, user, session), provisioned from the manifest on the session's
//! first command, kept across runs and destroyed when idle, on a freeze, a timeout or a
//! cancelled command, and when the harness closes. The environment has its own filesystem:
//! the host workspace is not mounted, so the host file tools and the shell do not share
//! files.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_sandbox::workspace::{Manifest, SandboxClient};
use serde_json::json;

use super::container::DEFAULT_IDLE_TTL;

/// Configuration of the remote shell.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use adk_harness::{RemoteShellConfig, ShellIsolation};
/// use adk_sandbox::workspace::{LocalUnixClient, Manifest, ManifestEntry};
///
/// let client = Arc::new(LocalUnixClient::new(None, std::env::temp_dir().join("snapshots")));
/// let config = RemoteShellConfig::new(client)
///     .with_manifest(Manifest::new(vec![ManifestEntry::GitRepo {
///         url: "https://github.com/zavora-ai/adk-rust".into(),
///         branch: None,
///         path: "repo".into(),
///     }]))
///     .with_idle_ttl(Duration::from_secs(300));
/// assert_eq!(config.client.provider(), "local");
/// let isolation = ShellIsolation::Remote(config);
/// assert_eq!(isolation.requested_name(), "remote");
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct RemoteShellConfig {
    /// The client environments come from.
    pub client: Arc<dyn SandboxClient>,
    /// What each new environment starts with: files, directories, git repositories.
    pub manifest: Manifest,
    /// How long a session's environment stays up after its last command.
    pub idle_ttl: Duration,
}

impl RemoteShellConfig {
    /// A remote shell over `client`, with an empty manifest and a 15-minute idle TTL.
    pub fn new(client: Arc<dyn SandboxClient>) -> Self {
        Self { client, manifest: Manifest::new(Vec::new()), idle_ttl: DEFAULT_IDLE_TTL }
    }

    /// What each new environment starts with.
    #[must_use]
    pub fn with_manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = manifest;
        self
    }

    /// How long a session's environment stays up after its last command.
    #[must_use]
    pub fn with_idle_ttl(mut self, idle_ttl: Duration) -> Self {
        self.idle_ttl = idle_ttl;
        self
    }

    /// Digest of what a command can observe: the provider, whether its environments are off
    /// the network, and the manifest, as `sha256:<hex>`.
    ///
    /// The `bash` declaration's annotations carry it, so standing rules and held calls bound
    /// to one remote configuration never match another, nor a container or the host.
    pub fn profile_digest(&self) -> String {
        let capabilities = self.client.capabilities();
        let manifest = serde_json::to_string(&self.manifest).unwrap_or_default();
        let profile = json!({
            "enforcer": "remote",
            "provider": self.client.provider(),
            "remote": capabilities.remote,
            "networkBlocked": capabilities.network_blocked,
            "manifest": crate::fsutil::sha256_hex(&manifest),
        });
        format!("sha256:{}", crate::fsutil::sha256_hex(&profile.to_string()))
    }

    /// Why remote commands cannot run unapproved, or `None` when they can (spec 3.1 §9.2
    /// I26): a command that cannot reach the host's files may run unapproved only where it
    /// cannot reach the network either.
    pub(crate) fn auto_allow_refusal(&self) -> Option<String> {
        let capabilities = self.client.capabilities();
        if !capabilities.remote {
            return Some(format!(
                "the {} client runs commands on this host",
                self.client.provider()
            ));
        }
        (!capabilities.network_blocked)
            .then(|| format!("the {} sandbox can reach the network", self.client.provider()))
    }
}

impl fmt::Debug for RemoteShellConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteShellConfig")
            .field("provider", &self.client.provider())
            .field("manifest", &self.manifest.entries.len())
            .field("idle_ttl", &self.idle_ttl)
            .finish()
    }
}
