//! Frame retention (spec 3.7 §7.13, Q13): frames are ephemeral unless they become a wake
//! payload, evidence on a card, or a sample a person kept.

use std::sync::Arc;
use std::time::Duration;

use adk_artifact::{ArtifactService, DeleteRequest, LoadRequest, SaveRequest, VersionsRequest};
use adk_core::{ErrorCategory, Part};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::privacy::RedactedFrame;
use super::records::{VISION_EVIDENCE_KEPT, VISION_EVIDENCE_PURGED, evidence_draft};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::media::{FrameSourceId, ImageRef, MediaScope, MediaStore, Provenance};

/// How long an evidence frame is kept by default: 30 days (spec 3.7 Q13).
pub const EVIDENCE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Why a frame is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeepReason {
    /// It is the frame a wake payload names.
    WakePayload,
    /// It is evidence on a card: an approval, a stopped line.
    CardEvidence,
    /// A person asked to keep it.
    PersonKept,
}

/// How long a source's kept frames live, per [`KeepReason`]: 30 days for each by default
/// (spec 3.7 Q13).
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_vision::stream::{EVIDENCE_RETENTION, KeepReason, RetentionPolicy};
///
/// let policy = RetentionPolicy::default()
///     .with_reason(KeepReason::PersonKept, Duration::from_secs(90 * 86_400));
/// assert_eq!(policy.for_reason(KeepReason::CardEvidence), EVIDENCE_RETENTION);
/// assert_eq!(policy.for_reason(KeepReason::PersonKept).as_secs(), 90 * 86_400);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    /// Frames a wake payload names.
    pub wake_payload: Duration,
    /// Frames on cards.
    pub card_evidence: Duration,
    /// Frames a person kept.
    pub person_kept: Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::evidence(EVIDENCE_RETENTION)
    }
}

impl RetentionPolicy {
    /// The same retention for every reason.
    pub fn evidence(retention: Duration) -> Self {
        Self { wake_payload: retention, card_evidence: retention, person_kept: retention }
    }

    /// Sets the retention of one reason.
    pub fn with_reason(mut self, reason: KeepReason, retention: Duration) -> Self {
        match reason {
            KeepReason::WakePayload => self.wake_payload = retention,
            KeepReason::CardEvidence => self.card_evidence = retention,
            KeepReason::PersonKept => self.person_kept = retention,
        }
        self
    }

    /// The retention of `reason`.
    pub fn for_reason(&self, reason: KeepReason) -> Duration {
        match reason {
            KeepReason::WakePayload => self.wake_payload,
            KeepReason::CardEvidence => self.card_evidence,
            KeepReason::PersonKept => self.person_kept,
        }
    }
}

/// One kept frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionEntry {
    /// The stored image.
    pub image: ImageRef,
    /// Why it is kept; a frame kept twice lists both reasons.
    pub reasons: Vec<KeepReason>,
    /// The frame's index in its stream.
    pub frame_index: u64,
    /// When it was first kept.
    pub kept_at: DateTime<Utc>,
    /// When it is purged: the latest expiry of its reasons.
    pub expires_at: DateTime<Utc>,
    /// The privacy hook the frame passed before its write.
    pub privacy: String,
    /// How many boxes the hook redacted.
    pub redacted: usize,
}

/// Why a kept frame was deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PurgeCause {
    /// Its retention ran out.
    Expired,
    /// Its source's frames were erased.
    Erased,
}

/// A deleted frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgedFrame {
    /// What was kept.
    pub entry: RetentionEntry,
    /// Why it went.
    pub cause: PurgeCause,
}

/// Stores the frames of one source that a wake, a card or a person keeps, and deletes them
/// when their retention runs out (spec 3.7 §7.13, Q13).
///
/// Only a [`RedactedFrame`] can be kept, so every stored frame passed its privacy hook
/// before the write (V-I9, S18). Frames go to the media store as PNG, by digest, so a frame
/// kept for a wake and again for a card is one artifact with both reasons. The keeper's
/// index is itself an artifact in the scope, `vision-retention-<source>-<hash>.json`, so a
/// restarted process still purges what an earlier one kept. Every keep writes
/// `vision.evidence.kept`, and every deletion `vision.evidence.purged`, with the frame's
/// digest, never its pixels.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_artifact::InMemoryArtifactService;
/// use adk_vision::stream::{FrameKeeper, FramePrivacy, KeepReason, NoRedaction};
/// use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, MediaScope, VisionCx};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let source = FrameSourceId("line-1".into());
/// let keeper = FrameKeeper::new(
///     Arc::new(InMemoryArtifactService::new()),
///     MediaScope::new("inspector", "u1", "s1"),
///     source.clone(),
/// );
/// let frame = Frame {
///     source,
///     index: 7,
///     pts: Duration::from_millis(700),
///     captured_at: chrono::Utc::now(),
///     pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(8, 8, [40; 4]))),
/// };
/// let cx = VisionCx::new();
/// let redacted = FramePrivacy::new(Arc::new(NoRedaction)).apply(&cx, frame).await.unwrap();
/// let kept = keeper.keep(&cx, &redacted, KeepReason::WakePayload).await.unwrap();
/// assert_eq!(kept.reasons, vec![KeepReason::WakePayload]);
/// // Thirty days on, it is gone.
/// let later = kept.kept_at + chrono::TimeDelta::days(31);
/// assert_eq!(keeper.purge_expired(&cx, later).await.unwrap().len(), 1);
/// # });
/// ```
pub struct FrameKeeper {
    artifacts: Arc<dyn ArtifactService>,
    store: MediaStore,
    scope: MediaScope,
    source: FrameSourceId,
    policy: RetentionPolicy,
    provenance: Provenance,
    index_file: String,
    // Serialises read-modify-write of the index artifact.
    index: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for FrameKeeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameKeeper")
            .field("scope", &self.scope)
            .field("source", &self.source)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl FrameKeeper {
    /// A keeper of `source`'s frames in `scope`, with the default 30-day policy and camera
    /// provenance.
    pub fn new(
        artifacts: Arc<dyn ArtifactService>,
        scope: MediaScope,
        source: FrameSourceId,
    ) -> Self {
        let safe: String = source
            .0
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .take(48)
            .collect();
        let tag = &Digest::of(source.0.as_bytes()).hex()[..8];
        Self {
            store: MediaStore::new(Arc::clone(&artifacts)),
            artifacts,
            scope,
            provenance: Provenance::Camera { source: source.clone() },
            index_file: format!("vision-retention-{safe}-{tag}.json"),
            source,
            policy: RetentionPolicy::default(),
            index: tokio::sync::Mutex::new(()),
        }
    }

    /// Sets the retention policy.
    pub fn with_policy(mut self, policy: RetentionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Sets the provenance kept frames record: [`Provenance::Screen`] or
    /// [`Provenance::Browser`] for those sources.
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    /// The retention policy.
    pub fn policy(&self) -> &RetentionPolicy {
        &self.policy
    }

    /// Keeps `frame` for `reason`, now.
    ///
    /// # Errors
    ///
    /// As [`keep_at`](Self::keep_at).
    pub async fn keep(
        &self,
        cx: &VisionCx,
        frame: &RedactedFrame,
        reason: KeepReason,
    ) -> Result<RetentionEntry> {
        self.keep_at(cx, frame, reason, Utc::now()).await
    }

    /// Keeps `frame` for `reason` as of `at`: stores it, records it in the index, and writes
    /// `vision.evidence.kept`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a frame of another source, the encoder's or the
    /// artifact service's error, or the audit trail's when it fails closed.
    pub async fn keep_at(
        &self,
        cx: &VisionCx,
        frame: &RedactedFrame,
        reason: KeepReason,
        at: DateTime<Utc>,
    ) -> Result<RetentionEntry> {
        if frame.meta().source != self.source {
            return Err(VisionError::InvalidInput(format!(
                "frame of `{}` handed to the keeper of `{}`",
                frame.meta().source.0,
                self.source.0
            )));
        }
        let _guard = self.index.lock().await;
        let (image, _) = self
            .store
            .put_pixels(&self.scope, frame.pixels().clone(), self.provenance.clone())
            .await?;
        let retention = self.policy.for_reason(reason);
        let expires_at =
            at + chrono::TimeDelta::from_std(retention).unwrap_or(chrono::TimeDelta::MAX);
        let mut entries = self.load_index().await?;
        let entry = match entries.iter_mut().find(|e| e.image.digest == image.digest) {
            Some(existing) => {
                if !existing.reasons.contains(&reason) {
                    existing.reasons.push(reason);
                    existing.reasons.sort();
                }
                existing.expires_at = existing.expires_at.max(expires_at);
                existing.clone()
            }
            None => {
                let entry = RetentionEntry {
                    image,
                    reasons: vec![reason],
                    frame_index: frame.meta().index,
                    kept_at: at,
                    expires_at,
                    privacy: frame.hook().to_string(),
                    redacted: frame.redacted().len(),
                };
                entries.push(entry.clone());
                entry
            }
        };
        self.save_index(&entries).await?;
        tracing::debug!(
            frame.source = %self.source.0,
            frame.index = entry.frame_index,
            image.digest = %entry.image.digest,
            "frame kept"
        );
        cx.audit(evidence_draft(
            VISION_EVIDENCE_KEPT,
            &entry.image.digest.to_string(),
            json!({
                "source": self.source.0,
                "reason": reason,
                "reasons": entry.reasons,
                "retentionSecs": retention.as_secs(),
                "expiresAt": entry.expires_at,
                "scope": self.scope,
                "artifact": entry.image.artifact,
                "frameIndex": entry.frame_index,
                "privacy": entry.privacy,
                "redacted": entry.redacted,
            }),
        ))
        .await?;
        Ok(entry)
    }

    /// Every frame kept and not yet purged.
    ///
    /// # Errors
    ///
    /// The artifact service's error, or [`VisionError::Parse`] for an index that does not
    /// parse.
    pub async fn kept(&self) -> Result<Vec<RetentionEntry>> {
        let _guard = self.index.lock().await;
        self.load_index().await
    }

    /// Deletes every frame whose retention ran out by `now`.
    ///
    /// # Errors
    ///
    /// As [`erase`](Self::erase).
    pub async fn purge_expired(
        &self,
        cx: &VisionCx,
        now: DateTime<Utc>,
    ) -> Result<Vec<PurgedFrame>> {
        self.purge(cx, PurgeCause::Expired, |entry| entry.expires_at <= now).await
    }

    /// Deletes every frame of the source in the scope: erasure by scope.
    ///
    /// # Errors
    ///
    /// The artifact service's error, or the audit trail's when it fails closed. Frames
    /// deleted before an error stay deleted and out of the index.
    pub async fn erase(&self, cx: &VisionCx) -> Result<Vec<PurgedFrame>> {
        self.purge(cx, PurgeCause::Erased, |_| true).await
    }

    async fn purge(
        &self,
        cx: &VisionCx,
        cause: PurgeCause,
        due: impl Fn(&RetentionEntry) -> bool,
    ) -> Result<Vec<PurgedFrame>> {
        let _guard = self.index.lock().await;
        let entries = self.load_index().await?;
        let (gone, mut keep): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| due(e));
        let mut purged = Vec::with_capacity(gone.len());
        let mut failure = None;
        let mut gone = gone.into_iter();
        for entry in gone.by_ref() {
            let at = &entry.image.artifact;
            let deleted = self
                .artifacts
                .delete(DeleteRequest {
                    app_name: at.app_name.clone(),
                    user_id: at.user_id.clone(),
                    session_id: at.session_id.clone(),
                    file_name: at.file_name.clone(),
                    version: None,
                })
                .await;
            match deleted {
                Ok(()) => {}
                Err(err) if err.category == ErrorCategory::NotFound => {}
                Err(err) => {
                    keep.push(entry);
                    failure = Some(VisionError::from(err));
                    break;
                }
            }
            purged.push(PurgedFrame { entry, cause });
        }
        keep.extend(gone);
        self.save_index(&keep).await?;
        for frame in &purged {
            cx.audit(evidence_draft(
                VISION_EVIDENCE_PURGED,
                &frame.entry.image.digest.to_string(),
                json!({
                    "source": self.source.0,
                    "cause": cause,
                    "reasons": frame.entry.reasons,
                    "keptAt": frame.entry.kept_at,
                    "expiresAt": frame.entry.expires_at,
                    "scope": self.scope,
                    "artifact": frame.entry.image.artifact,
                }),
            ))
            .await?;
        }
        match failure {
            Some(err) => Err(err),
            None => Ok(purged),
        }
    }

    async fn load_index(&self) -> Result<Vec<RetentionEntry>> {
        let loaded = self
            .artifacts
            .load(LoadRequest {
                app_name: self.scope.app_name.clone(),
                user_id: self.scope.user_id.clone(),
                session_id: self.scope.session_id.clone(),
                file_name: self.index_file.clone(),
                version: None,
            })
            .await;
        let part = match loaded {
            Ok(loaded) => loaded.part,
            Err(err) if err.category == ErrorCategory::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let bytes = match part {
            Part::InlineData { data, .. } => data,
            Part::Text { text } => text.into_bytes(),
            _ => {
                return Err(VisionError::Parse {
                    backend: "frame-keeper".into(),
                    message: format!("retention index {} is not JSON data", self.index_file),
                });
            }
        };
        serde_json::from_slice(&bytes).map_err(|err| VisionError::Parse {
            backend: "frame-keeper".into(),
            message: format!("retention index {}: {err}", self.index_file),
        })
    }

    /// Saves the index as a new version, then deletes the older ones, so a failure between
    /// the two leaves an index that still lists every stored frame.
    async fn save_index(&self, entries: &[RetentionEntry]) -> Result<()> {
        let data = serde_json::to_vec(entries).map_err(|err| VisionError::Parse {
            backend: "frame-keeper".into(),
            message: err.to_string(),
        })?;
        let saved = self
            .artifacts
            .save(SaveRequest {
                app_name: self.scope.app_name.clone(),
                user_id: self.scope.user_id.clone(),
                session_id: self.scope.session_id.clone(),
                file_name: self.index_file.clone(),
                part: Part::InlineData {
                    mime_type: "application/json".into(),
                    data,
                    uri: None,
                    annotations: None,
                },
                version: None,
            })
            .await?;
        let versions = self
            .artifacts
            .versions(VersionsRequest {
                app_name: self.scope.app_name.clone(),
                user_id: self.scope.user_id.clone(),
                session_id: self.scope.session_id.clone(),
                file_name: self.index_file.clone(),
            })
            .await?;
        for version in versions.versions.into_iter().filter(|v| *v != saved.version) {
            self.artifacts
                .delete(DeleteRequest {
                    app_name: self.scope.app_name.clone(),
                    user_id: self.scope.user_id.clone(),
                    session_id: self.scope.session_id.clone(),
                    file_name: self.index_file.clone(),
                    version: Some(version),
                })
                .await?;
        }
        Ok(())
    }
}
