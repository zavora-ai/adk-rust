use std::sync::Arc;

use adk_artifact::{ArtifactService, LoadRequest, SaveRequest};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, MAX_INLINE_DATA_SIZE, Part, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::kernel::commands::{EvidenceLookup, StoreEvidenceCommand, StoredEvidence};
use crate::kernel::service::EvidenceStore;

/// Evidence store backed by `adk-artifact`.
pub struct ArtifactBackedEvidenceStore {
    artifact_service: Arc<dyn ArtifactService>,
}

impl ArtifactBackedEvidenceStore {
    /// Creates an artifact-backed evidence store.
    #[must_use]
    pub fn new(artifact_service: Arc<dyn ArtifactService>) -> Self {
        Self { artifact_service }
    }

    fn require_identity<'a>(
        session_identity: &'a Option<adk_core::identity::AdkIdentity>,
        code: &'static str,
    ) -> Result<&'a adk_core::identity::AdkIdentity> {
        session_identity.as_ref().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Artifact,
                ErrorCategory::InvalidInput,
                code,
                "evidence storage requires a session identity",
            )
        })
    }

    fn artifact_key(command: &StoreEvidenceCommand) -> String {
        let reference_hash = hash_text(&command.evidence_ref.evidence_id);
        format!(
            "payments:evidence:{}:{}:{}:{}",
            command.transaction_id.as_str(),
            command.evidence_ref.protocol.name,
            command.evidence_ref.artifact_kind,
            reference_hash
        )
    }
}

#[async_trait]
impl EvidenceStore for ArtifactBackedEvidenceStore {
    async fn store(&self, command: StoreEvidenceCommand) -> Result<StoredEvidence> {
        if command.body.len() > MAX_INLINE_DATA_SIZE {
            return Err(AdkError::new(
                ErrorComponent::Artifact,
                ErrorCategory::InvalidInput,
                "payments.evidence.too_large",
                format!(
                    "evidence payload exceeds inline artifact size limit of {} bytes",
                    MAX_INLINE_DATA_SIZE
                ),
            ));
        }

        let identity = Self::require_identity(
            &command.session_identity,
            "payments.evidence.identity_required",
        )?;
        let artifact_key = Self::artifact_key(&command);
        self.artifact_service
            .save(SaveRequest {
                app_name: identity.app_name.as_ref().to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                session_id: identity.session_id.as_ref().to_string(),
                file_name: artifact_key.clone(),
                part: Part::InlineData {
                    mime_type: command.content_type.clone(),
                    data: command.body.clone(),
                },
                version: None,
            })
            .await?;

        let mut evidence_ref = command.evidence_ref;
        evidence_ref.evidence_id = artifact_key;
        evidence_ref.digest = Some(format!("sha256:{}", hash_bytes(&command.body)));

        Ok(StoredEvidence { evidence_ref, body: command.body, content_type: command.content_type })
    }

    async fn load(&self, lookup: EvidenceLookup) -> Result<Option<StoredEvidence>> {
        let identity = Self::require_identity(
            &lookup.session_identity,
            "payments.evidence.identity_required",
        )?;
        let response = self
            .artifact_service
            .load(LoadRequest {
                app_name: identity.app_name.as_ref().to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                session_id: identity.session_id.as_ref().to_string(),
                file_name: lookup.evidence_ref.evidence_id.clone(),
                version: None,
            })
            .await;

        match response {
            Ok(response) => {
                let Part::InlineData { mime_type, data } = response.part else {
                    return Err(AdkError::new(
                        ErrorComponent::Artifact,
                        ErrorCategory::Internal,
                        "payments.evidence.unsupported_part",
                        "stored payment evidence must use inline artifact data",
                    ));
                };

                Ok(Some(StoredEvidence {
                    evidence_ref: lookup.evidence_ref,
                    body: data,
                    content_type: mime_type,
                }))
            }
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_text(text: &str) -> String {
    hash_bytes(text.as_bytes())[..16].to_string()
}
