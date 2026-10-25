//! Release manifests and digests (spec 3.5 §7.7) and host limits (spec 3.5 §7.8.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A content digest, `sha256:<64 lowercase hex digits>` (spec 3.5 §7.7).
///
/// Hashing happens elsewhere; this type only carries and validates the form.
///
/// # Example
///
/// ```
/// use awp_types::Digest;
///
/// let digest = Digest::from_sha256_bytes([0xab; 32]);
/// assert_eq!(digest.algorithm(), "sha256");
/// assert_eq!(digest.hex(), "ab".repeat(32));
/// assert_eq!(digest.as_str().parse::<Digest>().unwrap(), digest);
/// assert!("sha256:ABC".parse::<Digest>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(String);

impl Digest {
    /// The only digest algorithm AWP 2.0 names.
    pub const SHA256: &'static str = "sha256";

    /// Parses `sha256:<64 lowercase hex digits>`.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError`] when the algorithm is not `sha256` or the hex part is not 64
    /// lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        let Some((algorithm, hex)) = value.split_once(':') else {
            return Err(DigestError::MissingAlgorithm);
        };
        if algorithm != Self::SHA256 {
            return Err(DigestError::UnsupportedAlgorithm(algorithm.to_owned()));
        }
        Self::from_sha256_hex(hex)
    }

    /// Creates a digest from 64 lowercase hex digits.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError::InvalidHex`] when `hex` is not 64 lowercase hex digits.
    pub fn from_sha256_hex(hex: &str) -> Result<Self, DigestError> {
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DigestError::InvalidHex);
        }
        Ok(Self(format!("{}:{hex}", Self::SHA256)))
    }

    /// Creates a digest from a SHA-256 output.
    pub fn from_sha256_bytes(bytes: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(7 + 64);
        value.push_str("sha256:");
        for byte in bytes {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(value)
    }

    /// Returns the algorithm, `sha256`.
    pub fn algorithm(&self) -> &str {
        Self::SHA256
    }

    /// Returns the hex digits.
    pub fn hex(&self) -> &str {
        &self.0[Self::SHA256.len() + 1..]
    }

    /// Returns `sha256:<hex>`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for Digest {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Digest".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$",
            "description": "A SHA-256 digest: `sha256:` and 64 lowercase hex digits."
        })
    }
}

/// Why a string is not a [`Digest`] (spec 3.5 §7.7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DigestError {
    /// No `algorithm:` prefix.
    #[error("digest has no algorithm prefix; expected `sha256:<64 hex digits>`")]
    MissingAlgorithm,
    /// An algorithm other than `sha256`.
    #[error("digest algorithm `{0}` is not supported; AWP 2.0 digests use `sha256`")]
    UnsupportedAlgorithm(String),
    /// The hex part is not 64 lowercase hex digits.
    #[error("digest must carry 64 lowercase hex digits after `sha256:`")]
    InvalidHex,
}

/// A release manifest, `/.well-known/awp/releases/{seq}.json` (spec 3.5 §7.7, §7.8.3).
///
/// A snapshot lists every file. A delta names a `base` release and lists only added and
/// changed files in `files` and removed paths in `removed`; its full file set is its base's
/// with `removed` taken away and `files` added or replaced. `redirects` is always the full
/// set.
///
/// # Example
///
/// ```
/// use std::collections::BTreeMap;
///
/// use awp_types::{Digest, FileEntry, GeneratorRef, ReleaseManifest, ReleaseSource};
/// use chrono::Utc;
///
/// let file = FileEntry { sha256: Digest::from_sha256_bytes([1; 32]), size: 512, content_type: "text/html".into() };
/// let snapshot = ReleaseManifest {
///     seq: 1,
///     previous: None,
///     base: None,
///     created_at: Utc::now(),
///     files: BTreeMap::from([("/index.html".to_owned(), file)]),
///     removed: Default::default(),
///     redirects: vec![],
///     source: ReleaseSource::Onboarding,
///     generator: GeneratorRef { name: "adk-site".into(), version: "3.0.0".into() },
///     charter_version: 1,
/// };
/// assert!(snapshot.is_snapshot());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReleaseManifest {
    /// Sequence number; releases are serialised per site.
    pub seq: u64,
    /// Digest of the previous release's manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Digest>,
    /// For a delta, the release it lists changes against; `None` for a snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<u64>,
    /// When the release was built.
    pub created_at: DateTime<Utc>,
    /// Path → file. A snapshot lists every file; a delta, the added and changed ones.
    pub files: BTreeMap<String, FileEntry>,
    /// Paths a delta removes from its base; always empty on a snapshot.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed: BTreeSet<String>,
    /// Redirects, in full.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirects: Vec<Redirect>,
    /// What produced the release.
    pub source: ReleaseSource,
    /// Generator name and version, for reproduction.
    pub generator: GeneratorRef,
    /// The charter version in effect when the release was built.
    pub charter_version: u32,
}

impl ReleaseManifest {
    /// Returns `true` for a full snapshot, `false` for a delta.
    pub fn is_snapshot(&self) -> bool {
        self.base.is_none()
    }

    /// Returns the release's full file set given the full file set of its `base`.
    ///
    /// A snapshot ignores `base_files` and returns its own files; a delta removes `removed`
    /// from `base_files` and then adds or replaces its `files`.
    pub fn apply_to(
        &self,
        base_files: &BTreeMap<String, FileEntry>,
    ) -> BTreeMap<String, FileEntry> {
        if self.is_snapshot() {
            return self.files.clone();
        }
        let mut files: BTreeMap<String, FileEntry> = base_files
            .iter()
            .filter(|(path, _)| !self.removed.contains(*path))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect();
        files.extend(self.files.iter().map(|(path, entry)| (path.clone(), entry.clone())));
        files
    }
}

/// One file of a release (spec 3.5 §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// SHA-256 digest of the file's bytes.
    pub sha256: Digest,
    /// Size in bytes.
    pub size: u64,
    /// Media type.
    pub content_type: String,
}

/// A redirect in the host's format (spec 3.5 §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Redirect {
    /// Source path.
    pub from: String,
    /// Target path or URL.
    pub to: String,
    /// HTTP status, such as 301.
    pub status: u16,
}

/// What produced a release (spec 3.5 §7.7).
///
/// Serialized with a `kind` tag: `{"kind": "edit", "job": "...", "decision": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum ReleaseSource {
    /// Release 1, built during onboarding.
    Onboarding,
    /// A migration from an existing site.
    Migration,
    /// An edit by a job.
    Edit {
        /// The job that made the edit.
        job: String,
        /// The decision that approved it; absent when the job acted within its autonomy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
    /// A rollback to an earlier release.
    Rollback {
        /// The release rolled back to.
        to_seq: u64,
    },
}

/// The generator that built a release (spec 3.5 §7.7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GeneratorRef {
    /// Generator name.
    pub name: String,
    /// Generator version.
    pub version: String,
}

/// A host's publication limits (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct HostLimits {
    /// Most publishes the host accepts per hour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_publishes_per_hour: Option<u32>,
    /// Whether the host accepts delta uploads.
    pub delta_upload: bool,
    /// How the host switches releases.
    pub atomic: Atomicity,
}

/// How a host switches from one release to the next (spec 3.5 §7.8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Atomicity {
    /// The host deploys a release atomically.
    Deploy,
    /// Each release lives under its own prefix and one pointer switches last.
    PrefixPointer,
    /// No atomic switch; batch windows must be a minute or more.
    None,
}

/// A site log entry: the leaf the log records for one release (spec 3.5 §7.7).
///
/// The leaf is the JCS form of `{"seq", "manifest"}`, at leaf index `seq − 1`, so the tree
/// size is the latest release number. Unknown members are refused, since they would change
/// the leaf.
///
/// # Example
///
/// ```
/// use awp_types::{Digest, LogEntry};
///
/// let entry = LogEntry { seq: 101, manifest: Digest::from_sha256_bytes([7; 32]) };
/// assert_eq!(entry.leaf_index(), Some(100));
/// assert!(serde_json::from_str::<LogEntry>(r#"{"seq":1,"manifest":"sha256:00","x":1}"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogEntry {
    /// The release number.
    pub seq: u64,
    /// Digest of the release manifest's JCS form.
    pub manifest: Digest,
}

impl LogEntry {
    /// Returns the entry's leaf index, `seq − 1`; `None` for the invalid release 0.
    pub fn leaf_index(&self) -> Option<u64> {
        self.seq.checked_sub(1)
    }
}
