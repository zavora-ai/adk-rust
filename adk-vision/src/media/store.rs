//! [`MediaStore`]: images kept as artifacts, by digest.

use std::sync::Arc;

use adk_artifact::{ArtifactService, LoadRequest, SaveRequest, VersionsRequest};
use adk_core::{ErrorCategory, Part};
use serde::{Deserialize, Serialize};

use super::{
    ArtifactLocator, DecodeLimits, DecodedImage, ImageRef, Provenance, SourceImage, artifact_uri,
};
use crate::digest::Digest;
use crate::error::{Result, VisionError};

/// The app, user and session an image is stored under.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaScope {
    /// Application namespace.
    pub app_name: String,
    /// Owning user.
    pub user_id: String,
    /// Session.
    pub session_id: String,
}

impl MediaScope {
    /// A scope.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self { app_name: app_name.into(), user_id: user_id.into(), session_id: session_id.into() }
    }
}

/// Images kept as versioned artifacts, named by digest.
///
/// [`put_image`](Self::put_image) decodes once under the decode limits, computes the
/// digest, and saves a `Part::InlineData`; identical bytes in one scope are one artifact.
/// [`load`](Self::load) checks the digest of what it reads back.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_artifact::InMemoryArtifactService;
/// use adk_vision::{DecodedImage, MediaScope, MediaStore, Provenance};
///
/// # async fn demo() -> adk_vision::Result<()> {
/// let store = MediaStore::new(Arc::new(InMemoryArtifactService::new()));
/// let scope = MediaScope::new("desk", "u1", "s1");
/// let png = DecodedImage::filled(8, 8, [255, 0, 0, 255]).encode_png()?;
/// let (image, source) = store.put_image(&scope, png.clone(), Provenance::Upload).await?;
/// let (again, _) = store.put_image(&scope, png, Provenance::Upload).await?;
/// assert_eq!(again.artifact, image.artifact);
/// assert_eq!(store.load(&image).await?.digest(), source.digest());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct MediaStore {
    artifacts: Arc<dyn ArtifactService>,
    limits: DecodeLimits,
}

impl std::fmt::Debug for MediaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaStore").field("limits", &self.limits).finish_non_exhaustive()
    }
}

impl MediaStore {
    /// A store over an artifact service, with the default decode limits.
    pub fn new(artifacts: Arc<dyn ArtifactService>) -> Self {
        Self { artifacts, limits: DecodeLimits::default() }
    }

    /// Sets the decode limits.
    pub fn with_limits(mut self, limits: DecodeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The decode limits.
    pub fn limits(&self) -> &DecodeLimits {
        &self.limits
    }

    /// Decodes, checks and stores encoded image bytes.
    ///
    /// # Errors
    ///
    /// A decode refusal ([`DecodedImage::decode`]) before anything is stored, or the
    /// artifact service's error.
    pub async fn put_image(
        &self,
        scope: &MediaScope,
        bytes: Vec<u8>,
        provenance: Provenance,
    ) -> Result<(ImageRef, SourceImage)> {
        let (pixels, mime) = DecodedImage::decode(&bytes, &self.limits)?;
        let digest = Digest::of(&bytes);
        let source = SourceImage::from_parts(digest, mime.to_string(), Arc::new(pixels));
        let image = self.save(scope, bytes, digest, mime, source.size(), provenance).await?;
        Ok((image, source))
    }

    /// Encodes pixels as PNG and stores them.
    ///
    /// # Errors
    ///
    /// The encoder's or the artifact service's error.
    pub async fn put_pixels(
        &self,
        scope: &MediaScope,
        pixels: DecodedImage,
        provenance: Provenance,
    ) -> Result<(ImageRef, SourceImage)> {
        let png = pixels.encode_png()?;
        let digest = Digest::of(&png);
        let size = pixels.size();
        let source = SourceImage::from_parts(digest, "image/png".into(), Arc::new(pixels));
        let image = self.save(scope, png, digest, "image/png", size, provenance).await?;
        Ok((image, source))
    }

    async fn save(
        &self,
        scope: &MediaScope,
        bytes: Vec<u8>,
        digest: Digest,
        mime: &str,
        (width, height): (u32, u32),
        provenance: Provenance,
    ) -> Result<ImageRef> {
        let extension = mime.strip_prefix("image/").unwrap_or("bin");
        let file_name = format!("vision-{}.{extension}", digest.hex());
        let existing = self
            .artifacts
            .versions(VersionsRequest {
                app_name: scope.app_name.clone(),
                user_id: scope.user_id.clone(),
                session_id: scope.session_id.clone(),
                file_name: file_name.clone(),
            })
            .await;
        let version = match existing {
            Ok(found) if !found.versions.is_empty() => found.versions.iter().copied().max(),
            Ok(_) => None,
            Err(err) if err.category == ErrorCategory::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        let version = match version {
            Some(version) => version,
            None => {
                let part = Part::InlineData {
                    mime_type: mime.to_string(),
                    data: bytes,
                    uri: None,
                    annotations: Some(serde_json::json!({ "adkVision": { "digest": digest } })),
                };
                self.artifacts
                    .save(SaveRequest {
                        app_name: scope.app_name.clone(),
                        user_id: scope.user_id.clone(),
                        session_id: scope.session_id.clone(),
                        file_name: file_name.clone(),
                        part,
                        version: None,
                    })
                    .await?
                    .version
            }
        };
        tracing::debug!(image.digest = %digest, artifact.version = version, "image stored");
        Ok(ImageRef {
            artifact: ArtifactLocator::new(
                scope.app_name.clone(),
                scope.user_id.clone(),
                scope.session_id.clone(),
                file_name,
                version,
            ),
            digest,
            mime_type: mime.to_string(),
            width,
            height,
            provenance,
        })
    }

    /// Loads an image's encoded bytes, checking them against its digest.
    ///
    /// # Errors
    ///
    /// The artifact service's error, [`VisionError::InvalidInput`] when the artifact is not
    /// inline image data, or [`VisionError::DigestMismatch`] when the bytes changed.
    pub async fn load_bytes(&self, image: &ImageRef) -> Result<Vec<u8>> {
        let at = &image.artifact;
        let loaded = self
            .artifacts
            .load(LoadRequest {
                app_name: at.app_name.clone(),
                user_id: at.user_id.clone(),
                session_id: at.session_id.clone(),
                file_name: at.file_name.clone(),
                version: Some(at.version),
            })
            .await?;
        let Part::InlineData { data, .. } = loaded.part else {
            return Err(VisionError::InvalidInput(format!(
                "artifact {} does not hold inline image data",
                artifact_uri(at)
            )));
        };
        let actual = Digest::of(&data);
        if actual != image.digest {
            return Err(VisionError::DigestMismatch {
                path: artifact_uri(at).into(),
                expected: image.digest,
                actual,
            });
        }
        Ok(data)
    }

    /// Loads and decodes an image.
    ///
    /// # Errors
    ///
    /// As [`load_bytes`](Self::load_bytes), and the decode refusals.
    pub async fn load(&self, image: &ImageRef) -> Result<SourceImage> {
        let bytes = self.load_bytes(image).await?;
        SourceImage::decode(&bytes, &self.limits)
    }
}
