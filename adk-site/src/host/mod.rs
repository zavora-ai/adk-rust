//! Hosts: where releases are served (spec 3.5 §7.8 "Hosts", §7.8.3 "Hosts at speed";
//! AWP 2.0 §14.6).
//!
//! A host receives a release's files, then its manifest, then the log's tiles, then the
//! checkpoint, then removes what the release no longer contains, so a reader never sees a
//! checkpoint for files that are not there. [`DirectoryHost`] is the default; others arrive
//! as features and every implementation runs [`conformance`].
//!
//! | Feature | Host | Atomicity |
//! |---------|------|-----------|
//! | `host-s3` | `s3::S3Store` under `object::ObjectStoreHost`: any S3-compatible store, CloudFront invalidation | `PrefixPointer`, or `None` in place |
//! | `host-gcs` | `gcs::GcsStore` under `object::ObjectStoreHost` | `PrefixPointer`, or `None` in place |
//! | `host-azure` | `azure::AzureBlobStore` under `object::ObjectStoreHost`: the `$web` static website | `None` in place, or `PrefixPointer` |
//! | `host-cloudflare` | Cloudflare R2 under `object::ObjectStoreHost`, cache purge | `PrefixPointer` |
//! | `host-netlify` | `netlify::NetlifyHost`: the deploy API | `Deploy` |
//! | `host-github-pages` | `github_pages::GitPagesHost`: a pages branch pushed with `git` | `Deploy` |
//! | `host-rsync` | `rsync::RsyncHost`: release directories over SSH, switched by a symlink | `Deploy` |

#[cfg(feature = "host-azure")]
pub mod azure;
#[cfg(feature = "host-cloudflare")]
pub mod cloudflare;
pub mod conformance;
#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify",
    feature = "host-github-pages",
    feature = "host-rsync"
))]
mod credential;
mod directory;
#[cfg(feature = "host-gcs")]
pub mod gcs;
#[cfg(feature = "host-github-pages")]
pub mod github_pages;
#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify"
))]
mod http;
#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify",
    feature = "host-github-pages",
    feature = "host-rsync"
))]
mod keys;
#[cfg(feature = "host-netlify")]
pub mod netlify;
#[cfg(any(feature = "host-s3", feature = "host-gcs", feature = "host-azure"))]
pub mod object;
#[cfg(feature = "host-rsync")]
pub mod rsync;
#[cfg(feature = "host-s3")]
pub mod s3;

use std::time::Duration;

use async_trait::async_trait;
use awp_types::{HostLimits, ReleaseManifest};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::log::{LOG_PREFIX, TileFetch};
use crate::redirects::RedirectsFormat;
use crate::release::{StagedDelta, StagedRelease};

#[cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify",
    feature = "host-github-pages",
    feature = "host-rsync"
))]
pub use credential::HostCredential;
pub use directory::DirectoryHost;

/// Which deployment a publish goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTarget {
    /// A preview the owner reviews before approving.
    Preview,
    /// The public site.
    Production,
}

/// What a publish or rollback did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishReport {
    /// The host's name.
    pub host: String,
    /// Where it went.
    pub target: HostTarget,
    /// The release now served.
    pub seq: u64,
    /// Paths written, in the order the host wrote them: files, manifest, tiles, checkpoint.
    pub uploaded: Vec<String>,
    /// Paths removed after the checkpoint.
    pub removed: Vec<String>,
    /// How long the publish took.
    #[serde(with = "millis")]
    pub elapsed: Duration,
}

mod millis {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_millis)
    }
}

/// Serves a site's releases (spec 3.5 §7.8, §7.8.3).
#[async_trait]
pub trait SiteHost: Send + Sync {
    /// The host's name for reports and logs.
    fn name(&self) -> &str {
        "host"
    }

    /// Publishes a release: files, manifest, tiles, checkpoint, then removals; atomically
    /// where [`limits`](Self::limits) says so.
    ///
    /// # Errors
    ///
    /// Returns an error when the host refuses or fails; the previous release keeps serving.
    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport>;

    /// Serves the files of an earlier release again, with their exact digests; the log and
    /// the release metadata stay as they are (the rollback release that follows records it).
    ///
    /// # Errors
    ///
    /// Returns an error when the host does not hold that release or a stored file no
    /// longer matches its digest.
    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport>;

    /// What the production site serves at `path`, for drift checks; `None` when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the host cannot be read.
    async fn fetch(&self, path: &str) -> Result<Option<Bytes>>;

    /// How the host reads redirects.
    fn redirects_format(&self) -> RedirectsFormat;

    /// The host's publication limits.
    fn limits(&self) -> HostLimits;

    /// Publishes a delta release, uploading only its changed files.
    ///
    /// # Errors
    ///
    /// As [`publish`](Self::publish).
    async fn publish_delta(&self, delta: &StagedDelta, target: HostTarget)
    -> Result<PublishReport>;
}

/// Reads a host's published log tiles, for [`TileProofs`](crate::log::TileProofs).
pub struct HostTiles<'a>(pub &'a dyn SiteHost);

#[async_trait]
impl TileFetch for HostTiles<'_> {
    async fn fetch_tile(&self, path: &str) -> Result<Option<Bytes>> {
        self.0.fetch(&format!("{LOG_PREFIX}/{path}")).await
    }
}
