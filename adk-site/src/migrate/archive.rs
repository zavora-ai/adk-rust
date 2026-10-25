//! The legacy archive (spec 3.5 A-I10, §7.8.2 "legal hold"): the legacy site's export, its
//! database dump or its uploads stored as an AES-256-GCM encrypted artifact through
//! `adk-artifact`. Storing appends a version; nothing here deletes, because deleting a legacy
//! site, its database or its export is the owner's act, never a step.

use std::fmt;
use std::sync::Arc;

use adk_artifact::{ArtifactService, LoadRequest, SaveRequest};
use adk_core::audit_log::AuditRecorder;
use adk_core::{ActingContext, Part};
use awp_types::Digest;
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::audit::record;
use super::error::{MigrateError, Result};
use crate::release::file_digest;

/// The media type of an archived export.
pub const ARCHIVE_MEDIA_TYPE: &str = "application/vnd.adk-site.archive+aes256gcm";

const MAGIC: &[u8; 8] = b"AWPARC1\0";

/// The archive key: 32 bytes held by the owner's secret store, never logged.
#[derive(Clone)]
pub struct ArchiveKey([u8; 32]);

impl fmt::Debug for ArchiveKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ArchiveKey").field(&self.id()).finish()
    }
}

impl ArchiveKey {
    /// A key from its bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A fresh random key.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Archive`] when no randomness is available.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        aws_lc_rs::rand::fill(&mut bytes)
            .map_err(|_| MigrateError::Archive("no randomness".into()))?;
        Ok(Self(bytes))
    }

    /// A public identifier: the first 8 bytes of the key's SHA-256, in hex.
    pub fn id(&self) -> String {
        hex::encode(&Sha256::digest(self.0)[..8])
    }

    fn sealing(&self) -> Result<LessSafeKey> {
        UnboundKey::new(&AES_256_GCM, &self.0)
            .map(LessSafeKey::new)
            .map_err(|_| MigrateError::Archive("the key is not an AES-256 key".into()))
    }
}

impl Drop for ArchiveKey {
    fn drop(&mut self) {
        self.0 = [0; 32];
    }
}

/// What storing an export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveReceipt {
    /// The artifact's name.
    pub file_name: String,
    /// The version written.
    pub version: i64,
    /// The digest of the export.
    pub sha256: Digest,
    /// Its size.
    pub size: u64,
    /// The digest of the stored ciphertext.
    pub stored_sha256: Digest,
    /// The key's identifier.
    pub key_id: String,
    /// When.
    pub stored_at: DateTime<Utc>,
}

/// The encrypted archive of a legacy site (A-I10).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_artifact::InMemoryArtifactService;
/// use adk_site::migrate::archive::{ArchiveKey, LegacyArchive};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let archive = LegacyArchive::new(Arc::new(InMemoryArtifactService::new()), "site", "owner", "migration", ArchiveKey::generate().unwrap());
/// let receipt = archive.store("export.xml", b"<rss/>", chrono::Utc::now(), None).await.unwrap();
/// assert_eq!(archive.load(&receipt.file_name, Some(receipt.version)).await.unwrap().as_ref(), b"<rss/>");
/// # }
/// ```
pub struct LegacyArchive {
    service: Arc<dyn ArtifactService>,
    app_name: String,
    user_id: String,
    session_id: String,
    key: ArchiveKey,
}

impl fmt::Debug for LegacyArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LegacyArchive")
            .field("app_name", &self.app_name)
            .field("session_id", &self.session_id)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

fn artifact_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    format!("legacy-{}.awparc", safe.replace("..", "_"))
}

impl LegacyArchive {
    /// An archive in `service` under `app_name`, `user_id` and `session_id`, sealed with `key`.
    pub fn new(
        service: Arc<dyn ArtifactService>,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        key: ArchiveKey,
    ) -> Self {
        Self {
            service,
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            key,
        }
    }

    /// Encrypts `export` and stores it as a new version of the artifact for `name`; records
    /// `site.archive`.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Archive`] when sealing or storing fails; the recorder's error.
    pub async fn store(
        &self,
        name: &str,
        export: &[u8],
        now: DateTime<Utc>,
        audit: Option<(&Arc<dyn AuditRecorder>, &ActingContext)>,
    ) -> Result<ArchiveReceipt> {
        let file_name = artifact_name(name);
        let key_id = self.key.id();
        let mut nonce = [0u8; NONCE_LEN];
        aws_lc_rs::rand::fill(&mut nonce)
            .map_err(|_| MigrateError::Archive("no randomness".into()))?;
        let aad = [MAGIC.as_slice(), file_name.as_bytes()].concat();
        let mut sealed = export.to_vec();
        self.key
            .sealing()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad.as_slice()),
                &mut sealed,
            )
            .map_err(|_| MigrateError::Archive("sealing failed".into()))?;
        let mut stored =
            Vec::with_capacity(MAGIC.len() + 1 + key_id.len() + NONCE_LEN + sealed.len());
        stored.extend_from_slice(MAGIC);
        stored.push(key_id.len() as u8);
        stored.extend_from_slice(key_id.as_bytes());
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&sealed);
        let stored_sha256 = file_digest(&stored);
        let response = self
            .service
            .save(SaveRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: file_name.clone(),
                part: Part::inline_data(ARCHIVE_MEDIA_TYPE, stored),
                version: None,
            })
            .await
            .map_err(|e| MigrateError::Archive(e.to_string()))?;
        let receipt = ArchiveReceipt {
            file_name,
            version: response.version,
            sha256: file_digest(export),
            size: export.len() as u64,
            stored_sha256,
            key_id,
            stored_at: now,
        };
        tracing::info!(archive = %receipt.file_name, version = receipt.version, size = receipt.size, "legacy export archived");
        if let Some((recorder, acting)) = audit {
            let value =
                serde_json::to_value(&receipt).map_err(|e| MigrateError::Archive(e.to_string()))?;
            record(recorder, acting, "site.archive", None, value).await?;
        }
        Ok(receipt)
    }

    /// Reads and decrypts a stored export.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Archive`] when it is missing, sealed with another key, or altered.
    pub async fn load(&self, file_name: &str, version: Option<i64>) -> Result<Bytes> {
        let response = self
            .service
            .load(LoadRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                session_id: self.session_id.clone(),
                file_name: file_name.to_string(),
                version,
            })
            .await
            .map_err(|e| MigrateError::Archive(e.to_string()))?;
        let Part::InlineData { data, .. } = response.part else {
            return Err(MigrateError::Archive("the artifact is not an archive".into()));
        };
        let bad = || MigrateError::Archive(format!("{file_name} is not an archive this key opens"));
        let rest = data.strip_prefix(MAGIC.as_slice()).ok_or_else(bad)?;
        let (&id_len, rest) = rest.split_first().ok_or_else(bad)?;
        let (key_id, rest) = rest.split_at_checked(usize::from(id_len)).ok_or_else(bad)?;
        if key_id != self.key.id().as_bytes() {
            return Err(MigrateError::Archive(format!(
                "{file_name} was sealed with key {}, not {}",
                String::from_utf8_lossy(key_id),
                self.key.id()
            )));
        }
        let (nonce, sealed) = rest.split_at_checked(NONCE_LEN).ok_or_else(bad)?;
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| bad())?;
        let aad = [MAGIC.as_slice(), file_name.as_bytes()].concat();
        let mut buffer = sealed.to_vec();
        let plain = self
            .key
            .sealing()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad.as_slice()),
                &mut buffer,
            )
            .map_err(|_| bad())?;
        Ok(Bytes::copy_from_slice(plain))
    }
}
