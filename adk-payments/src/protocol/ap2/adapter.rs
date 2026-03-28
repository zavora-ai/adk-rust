use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::domain::{
    CommerceMode, EvidenceReference, InterventionKind, InterventionState, InterventionStatus,
    ProtocolEnvelopeDigest, ProtocolExtensions, TransactionRecord,
};
use crate::kernel::{
    BeginInterventionCommand, CommerceContext, EvidenceLookup, EvidenceStore, InterventionService,
    MerchantCheckoutService, PaymentExecutionOutcome, PaymentExecutionResult,
    PaymentExecutionService, StoreEvidenceCommand, StoredEvidence, TransactionLookup,
    TransactionStore,
};
use crate::protocol::ap2::error::Ap2Error;
use crate::protocol::ap2::mapper::{
    ap2_descriptor, cart_create_checkout_command, cart_update_checkout_command,
    execute_payment_command, intent_create_checkout_command, merge_extensions,
    sync_payment_outcome_command, update_record_extensions, update_record_state_from_receipt,
};
use crate::protocol::ap2::types::{
    AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate, PaymentReceipt,
    PaymentStatusEnvelope,
};
use crate::protocol::ap2::verification::{
    MerchantAuthorizationVerifier, RequireMerchantAuthorization, RequireUserAuthorization,
    UserAuthorizationVerifier, VerifiedAuthorization,
};

struct StoredArtifactSet {
    envelope: crate::domain::ProtocolExtensionEnvelope,
    evidence: Vec<StoredEvidence>,
}

#[derive(Default)]
struct ProtocolRefUpdate {
    intent_ref: Option<String>,
    cart_ref: Option<String>,
    payment_ref: Option<String>,
    receipt_ref: Option<String>,
}

/// AP2 alpha adapter over the canonical commerce kernel.
pub struct Ap2Adapter {
    checkout_service: Arc<dyn MerchantCheckoutService>,
    payment_service: Arc<dyn PaymentExecutionService>,
    transaction_store: Arc<dyn TransactionStore>,
    evidence_store: Arc<dyn EvidenceStore>,
    merchant_verifier: Arc<dyn MerchantAuthorizationVerifier>,
    user_verifier: Arc<dyn UserAuthorizationVerifier>,
    intervention_service: Option<Arc<dyn InterventionService>>,
}

impl Ap2Adapter {
    /// Creates a new AP2 adapter bound to the canonical checkout, journal, and evidence services.
    #[must_use]
    pub fn new(
        checkout_service: Arc<dyn MerchantCheckoutService>,
        payment_service: Arc<dyn PaymentExecutionService>,
        transaction_store: Arc<dyn TransactionStore>,
        evidence_store: Arc<dyn EvidenceStore>,
    ) -> Self {
        Self {
            checkout_service,
            payment_service,
            transaction_store,
            evidence_store,
            merchant_verifier: Arc::new(RequireMerchantAuthorization),
            user_verifier: Arc::new(RequireUserAuthorization),
            intervention_service: None,
        }
    }

    /// Overrides the merchant authorization verifier.
    #[must_use]
    pub fn with_merchant_authorization_verifier(
        mut self,
        verifier: Arc<dyn MerchantAuthorizationVerifier>,
    ) -> Self {
        self.merchant_verifier = verifier;
        self
    }

    /// Overrides the user authorization verifier.
    #[must_use]
    pub fn with_user_authorization_verifier(
        mut self,
        verifier: Arc<dyn UserAuthorizationVerifier>,
    ) -> Self {
        self.user_verifier = verifier;
        self
    }

    /// Enables canonical intervention handling for AP2 return-to-user flows.
    #[must_use]
    pub fn with_intervention_service(
        mut self,
        intervention_service: Arc<dyn InterventionService>,
    ) -> Self {
        self.intervention_service = Some(intervention_service);
        self
    }

    /// Starts or refreshes a human-not-present transaction from an intent mandate.
    ///
    /// # Errors
    ///
    /// Returns an error when the intent is expired, unsigned, or lacks explicit
    /// autonomous authority constraints.
    pub async fn submit_intent_mandate(
        &self,
        context: CommerceContext,
        intent_mandate: IntentMandate,
        authorization_artifact: Option<AuthorizationArtifact>,
    ) -> Result<TransactionRecord> {
        self.validate_intent_mandate(&context, &intent_mandate, authorization_artifact.as_ref())?;
        let verification = if let Some(artifact) = authorization_artifact.as_ref() {
            Some(self.user_verifier.verify_intent_authorization(&intent_mandate, artifact).await?)
        } else {
            None
        };

        let context = self.normalize_context(context, ProtocolExtensions::default());
        let artifacts = self
            .store_intent_artifacts(
                &context,
                &intent_mandate,
                authorization_artifact.as_ref(),
                verification.as_ref(),
            )
            .await?;
        let intent_ref = artifacts.evidence.first().and_then(|stored| {
            stored
                .evidence_ref
                .digest
                .clone()
                .or_else(|| Some(stored.evidence_ref.evidence_id.clone()))
        });
        let command = intent_create_checkout_command(&intent_mandate, context.clone());
        let record = self.checkout_service.create_checkout(command).await?;

        self.finalize_record(
            &context,
            record,
            artifacts,
            ProtocolRefUpdate { intent_ref, ..ProtocolRefUpdate::default() },
        )
        .await
    }

    /// Creates or updates canonical checkout state from a merchant cart mandate.
    ///
    /// # Errors
    ///
    /// Returns an error when the cart is expired, unsigned, or violates the
    /// stored intent constraints for a human-not-present transaction.
    pub async fn submit_cart_mandate(
        &self,
        context: CommerceContext,
        cart_mandate: CartMandate,
    ) -> Result<TransactionRecord> {
        self.validate_not_expired("cart_expiry", &cart_mandate.contents.cart_expiry)?;
        let context = self.normalize_context(context, ProtocolExtensions::default());
        let existing = self.lookup(&context).await?;
        if let Some(record) = existing.as_ref() {
            self.validate_intent_constraints(&context, record, &cart_mandate).await?;
        }

        let merchant_authorization = cart_mandate
            .merchant_authorization
            .clone()
            .map(|value| AuthorizationArtifact::new("merchant_authorization", value, "text/plain"))
            .ok_or_else(|| {
                AdkError::from(Ap2Error::MissingMerchantAuthorization {
                    cart_id: cart_mandate.contents.id.clone(),
                })
            })?;
        let verification = self
            .merchant_verifier
            .verify_cart_authorization(&cart_mandate, &merchant_authorization)
            .await?;
        let artifacts = self
            .store_cart_artifacts(&context, &cart_mandate, &merchant_authorization, &verification)
            .await?;

        let record = if existing.is_some() {
            self.checkout_service
                .update_checkout(cart_update_checkout_command(&cart_mandate, context.clone()))
                .await?
        } else {
            self.checkout_service
                .create_checkout(cart_create_checkout_command(&cart_mandate, context.clone()))
                .await?
        };

        self.finalize_record(
            &context,
            record,
            artifacts,
            ProtocolRefUpdate {
                cart_ref: Some(cart_mandate.contents.id.clone()),
                ..ProtocolRefUpdate::default()
            },
        )
        .await
    }

    /// Executes or escalates a payment mandate against the canonical payment service.
    ///
    /// # Errors
    ///
    /// Returns an error when the mandate does not match the current cart or the
    /// flow lacks user authorization and cannot proceed autonomously.
    pub async fn submit_payment_mandate(
        &self,
        context: CommerceContext,
        payment_mandate: PaymentMandate,
    ) -> Result<PaymentExecutionResult> {
        let context = self.normalize_context(context, ProtocolExtensions::default());
        let existing = self.lookup(&context).await?.ok_or_else(|| {
            AdkError::from(Ap2Error::TransactionNotFound {
                transaction_id: context.transaction_id.as_str().to_string(),
            })
        })?;
        let cart_mandate: CartMandate =
            self.load_latest_json(&context, &existing, "cart_mandate").await?.ok_or_else(|| {
                AdkError::from(Ap2Error::TransactionNotFound {
                    transaction_id: context.transaction_id.as_str().to_string(),
                })
            })?;
        self.validate_payment_mandate_against_cart(&payment_mandate, &cart_mandate)?;

        let (evidence, verification_value) = if let Some(user_authorization) =
            payment_mandate.user_authorization.clone()
        {
            let artifact =
                AuthorizationArtifact::new("user_authorization", user_authorization, "text/plain");
            let verification = self
                .user_verifier
                .verify_payment_authorization(&payment_mandate, &artifact)
                .await?;
            let artifacts = self
                .store_payment_artifacts(
                    &context,
                    &payment_mandate,
                    Some(&artifact),
                    Some(&verification),
                )
                .await?;
            (artifacts.evidence, Some(verification))
        } else {
            let allowed =
                self.evaluate_autonomous_authority(&context, &existing, &cart_mandate).await?;
            let artifacts =
                self.store_payment_artifacts(&context, &payment_mandate, None, None).await?;
            if !allowed {
                return self
                    .begin_user_reconfirmation(
                        context,
                        existing,
                        cart_mandate,
                        payment_mandate,
                        artifacts.envelope,
                        artifacts.evidence,
                    )
                    .await;
            }
            (artifacts.evidence, None)
        };

        let supporting_evidence_refs = self.ap2_evidence_refs(&existing, &evidence);
        let payment_envelope =
            self.payment_envelope(&payment_mandate, verification_value.as_ref(), &evidence);
        let mut command = execute_payment_command(
            &payment_mandate,
            self.normalize_context(
                context.clone(),
                ProtocolExtensions::from(vec![payment_envelope.clone()]),
            ),
            supporting_evidence_refs.clone(),
        );
        command.extensions = ProtocolExtensions::from(vec![payment_envelope.clone()]);

        let result = self.payment_service.execute_payment(command).await?;
        let transaction = self
            .finalize_record(
                &context,
                result.transaction.clone(),
                StoredArtifactSet { envelope: payment_envelope, evidence: evidence.clone() },
                ProtocolRefUpdate {
                    payment_ref: Some(
                        payment_mandate.payment_mandate_contents.payment_mandate_id.clone(),
                    ),
                    ..ProtocolRefUpdate::default()
                },
            )
            .await?;

        let mut generated_evidence_refs = result.generated_evidence_refs;
        for stored in evidence {
            if !generated_evidence_refs.contains(&stored.evidence_ref) {
                generated_evidence_refs.push(stored.evidence_ref);
            }
        }

        Ok(PaymentExecutionResult {
            outcome: result.outcome,
            transaction,
            order: result.order,
            intervention: result.intervention,
            generated_evidence_refs,
        })
    }

    /// Applies a final AP2 payment receipt to canonical transaction state.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying transaction does not exist.
    pub async fn apply_payment_receipt(
        &self,
        context: CommerceContext,
        payment_receipt: PaymentReceipt,
    ) -> Result<TransactionRecord> {
        let context = self.normalize_context(context, ProtocolExtensions::default());
        let existing = self.lookup(&context).await?;
        let artifacts = self.store_receipt_artifacts(&context, &payment_receipt).await?;
        let mut command =
            sync_payment_outcome_command(existing.as_ref(), &payment_receipt, context.clone());
        command.generated_evidence_refs =
            artifacts.evidence.iter().map(|stored| stored.evidence_ref.clone()).collect();
        command.context.extensions = merge_extensions(
            command.context.extensions,
            ProtocolExtensions::from(vec![artifacts.envelope.clone()]),
        );

        let mut record = self.payment_service.sync_payment_outcome(command).await?;
        update_record_state_from_receipt(&mut record, &payment_receipt);
        self.finalize_record(
            &context,
            record,
            artifacts,
            ProtocolRefUpdate {
                payment_ref: Some(payment_receipt.payment_mandate_id.clone()),
                receipt_ref: Some(payment_receipt.payment_id.clone()),
                ..ProtocolRefUpdate::default()
            },
        )
        .await
    }

    async fn begin_user_reconfirmation(
        &self,
        context: CommerceContext,
        existing: TransactionRecord,
        cart_mandate: CartMandate,
        payment_mandate: PaymentMandate,
        envelope: crate::domain::ProtocolExtensionEnvelope,
        evidence: Vec<StoredEvidence>,
    ) -> Result<PaymentExecutionResult> {
        let intervention_service = self.intervention_service.as_ref().ok_or_else(|| {
            AdkError::from(Ap2Error::InterventionServiceRequired {
                transaction_id: context.transaction_id.as_str().to_string(),
            })
        })?;
        let expires_at = self
            .load_latest_json::<IntentMandate>(&context, &existing, "intent_mandate")
            .await?
            .map(|intent| self.parse_timestamp("intent_expiry", &intent.intent_expiry))
            .transpose()?
            .or_else(|| {
                self.parse_timestamp("cart_expiry", &cart_mandate.contents.cart_expiry).ok()
            });
        let intervention = InterventionState {
            intervention_id: format!("ap2-reconfirm-{}", context.transaction_id.as_str()),
            kind: InterventionKind::BuyerReconfirmation,
            status: InterventionStatus::Pending,
            instructions: Some(
                "return to the user for explicit AP2 cart or payment confirmation".to_string(),
            ),
            continuation_token: Some(format!("ap2:return:{}", context.transaction_id.as_str())),
            requested_by: Some(context.actor.clone()),
            expires_at,
            extensions: ProtocolExtensions::default(),
        };
        let record = intervention_service
            .begin_intervention(BeginInterventionCommand {
                context: context.clone(),
                intervention: intervention.clone(),
            })
            .await?;
        let transaction = self
            .finalize_record(
                &context,
                record,
                StoredArtifactSet { envelope, evidence: evidence.clone() },
                ProtocolRefUpdate {
                    payment_ref: Some(payment_mandate.payment_mandate_contents.payment_mandate_id),
                    ..ProtocolRefUpdate::default()
                },
            )
            .await?;

        Ok(PaymentExecutionResult {
            outcome: PaymentExecutionOutcome::InterventionRequired,
            order: transaction.order.clone(),
            transaction,
            intervention: Some(intervention),
            generated_evidence_refs: evidence
                .into_iter()
                .map(|stored| stored.evidence_ref)
                .collect(),
        })
    }

    fn normalize_context(
        &self,
        mut context: CommerceContext,
        extensions: ProtocolExtensions,
    ) -> CommerceContext {
        context.protocol = ap2_descriptor();
        context.extensions = merge_extensions(context.extensions, extensions);
        context
    }

    async fn lookup(&self, context: &CommerceContext) -> Result<Option<TransactionRecord>> {
        self.transaction_store
            .get(TransactionLookup {
                transaction_id: context.transaction_id.clone(),
                session_identity: context.session_identity.clone(),
            })
            .await
    }

    fn validate_intent_mandate(
        &self,
        context: &CommerceContext,
        intent_mandate: &IntentMandate,
        authorization_artifact: Option<&AuthorizationArtifact>,
    ) -> Result<()> {
        self.validate_not_expired("intent_expiry", &intent_mandate.intent_expiry)?;
        if context.mode == CommerceMode::HumanNotPresent {
            if authorization_artifact.is_none() {
                return Err(Ap2Error::MissingIntentAuthorization {
                    transaction_id: context.transaction_id.as_str().to_string(),
                }
                .into());
            }
            if !self.has_authority_constraints(intent_mandate) {
                return Err(Ap2Error::MissingAuthorityConstraints {
                    transaction_id: context.transaction_id.as_str().to_string(),
                }
                .into());
            }
        }
        Ok(())
    }

    fn has_authority_constraints(&self, intent_mandate: &IntentMandate) -> bool {
        intent_mandate.merchants.as_ref().is_some_and(|merchants| !merchants.is_empty())
            || intent_mandate.skus.as_ref().is_some_and(|skus| !skus.is_empty())
            || intent_mandate.requires_refundability
    }

    async fn validate_intent_constraints(
        &self,
        context: &CommerceContext,
        existing: &TransactionRecord,
        cart_mandate: &CartMandate,
    ) -> Result<()> {
        if context.mode != CommerceMode::HumanNotPresent {
            return Ok(());
        }
        let Some(intent_mandate) =
            self.load_latest_json::<IntentMandate>(context, existing, "intent_mandate").await?
        else {
            return Ok(());
        };
        self.validate_not_expired("intent_expiry", &intent_mandate.intent_expiry)?;
        if let Some(merchants) =
            intent_mandate.merchants.as_ref().filter(|merchants| !merchants.is_empty())
        {
            let merchant_name = cart_mandate.contents.merchant_name.to_ascii_lowercase();
            let allowed =
                merchants.iter().map(|merchant| merchant.to_ascii_lowercase()).collect::<Vec<_>>();
            if !allowed.iter().any(|merchant| merchant == &merchant_name) {
                return Err(Ap2Error::MerchantNotAuthorized {
                    merchant_name: cart_mandate.contents.merchant_name.clone(),
                }
                .into());
            }
        }

        if intent_mandate.skus.as_ref().is_some_and(|skus| !skus.is_empty()) {
            return Err(Ap2Error::SkuConstraintUnverifiable.into());
        }

        if intent_mandate.requires_refundability {
            let all_refundable = cart_mandate
                .contents
                .payment_request
                .details
                .display_items
                .iter()
                .all(|item| item.refund_period > 0);
            if !all_refundable {
                return Err(Ap2Error::RefundabilityRequired.into());
            }
        }

        Ok(())
    }

    fn validate_payment_mandate_against_cart(
        &self,
        payment_mandate: &PaymentMandate,
        cart_mandate: &CartMandate,
    ) -> Result<()> {
        let payment_details_id = &payment_mandate.payment_mandate_contents.payment_details_id;
        if payment_details_id != &cart_mandate.contents.payment_request.details.id {
            return Err(Ap2Error::PaymentMandateMismatch {
                payment_mandate_id: payment_mandate
                    .payment_mandate_contents
                    .payment_mandate_id
                    .clone(),
                field: "payment_details_id".to_string(),
            }
            .into());
        }
        if payment_mandate.payment_mandate_contents.payment_response.request_id
            != cart_mandate.contents.payment_request.details.id
        {
            return Err(Ap2Error::PaymentMandateMismatch {
                payment_mandate_id: payment_mandate
                    .payment_mandate_contents
                    .payment_mandate_id
                    .clone(),
                field: "payment_response.request_id".to_string(),
            }
            .into());
        }
        if payment_mandate.payment_mandate_contents.payment_details_total
            != cart_mandate.contents.payment_request.details.total
        {
            return Err(Ap2Error::PaymentMandateMismatch {
                payment_mandate_id: payment_mandate
                    .payment_mandate_contents
                    .payment_mandate_id
                    .clone(),
                field: "payment_details_total".to_string(),
            }
            .into());
        }
        self.validate_not_expired("cart_expiry", &cart_mandate.contents.cart_expiry)
    }

    async fn evaluate_autonomous_authority(
        &self,
        context: &CommerceContext,
        existing: &TransactionRecord,
        cart_mandate: &CartMandate,
    ) -> Result<bool> {
        if context.mode != CommerceMode::HumanNotPresent {
            return Err(Ap2Error::MissingUserAuthorization {
                payment_mandate_id: context.transaction_id.as_str().to_string(),
            }
            .into());
        }
        let Some(intent_mandate) =
            self.load_latest_json::<IntentMandate>(context, existing, "intent_mandate").await?
        else {
            return Err(Ap2Error::MissingIntentAuthorization {
                transaction_id: context.transaction_id.as_str().to_string(),
            }
            .into());
        };
        self.validate_intent_constraints(context, existing, cart_mandate).await?;
        if intent_mandate.user_cart_confirmation_required
            || cart_mandate.contents.user_cart_confirmation_required
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn parse_timestamp(&self, field: &str, value: &str) -> Result<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(value).map(|timestamp| timestamp.with_timezone(&Utc)).map_err(
            |_| {
                AdkError::from(Ap2Error::InvalidTimestamp {
                    field: field.to_string(),
                    value: value.to_string(),
                })
            },
        )
    }

    fn validate_not_expired(&self, field: &str, value: &str) -> Result<()> {
        let timestamp = self.parse_timestamp(field, value)?;
        if timestamp < Utc::now() {
            return Err(Ap2Error::ExpiredArtifact {
                field: field.to_string(),
                expires_at: timestamp.to_rfc3339(),
            }
            .into());
        }
        Ok(())
    }

    async fn store_json(
        &self,
        context: &CommerceContext,
        artifact_kind: &str,
        unique_id: &str,
        payload: &impl serde::Serialize,
    ) -> Result<StoredEvidence> {
        self.evidence_store
            .store(StoreEvidenceCommand {
                transaction_id: context.transaction_id.clone(),
                session_identity: context.session_identity.clone(),
                evidence_ref: EvidenceReference {
                    evidence_id: format!("{artifact_kind}:{unique_id}"),
                    protocol: ap2_descriptor(),
                    artifact_kind: artifact_kind.to_string(),
                    digest: None,
                },
                body: serde_json::to_vec(payload).map_err(|err| {
                    AdkError::new(
                        ErrorComponent::Server,
                        ErrorCategory::InvalidInput,
                        "payments.ap2.serialize_failed",
                        format!("failed to serialize AP2 evidence: {err}"),
                    )
                })?,
                content_type: "application/json".to_string(),
            })
            .await
    }

    async fn store_inline_artifact(
        &self,
        context: &CommerceContext,
        artifact_kind: &str,
        unique_id: &str,
        artifact: &AuthorizationArtifact,
    ) -> Result<StoredEvidence> {
        self.evidence_store
            .store(StoreEvidenceCommand {
                transaction_id: context.transaction_id.clone(),
                session_identity: context.session_identity.clone(),
                evidence_ref: EvidenceReference {
                    evidence_id: format!("{artifact_kind}:{unique_id}"),
                    protocol: ap2_descriptor(),
                    artifact_kind: artifact_kind.to_string(),
                    digest: None,
                },
                body: artifact.value.as_bytes().to_vec(),
                content_type: artifact.content_type.clone(),
            })
            .await
    }

    async fn load_latest_json<T>(
        &self,
        context: &CommerceContext,
        record: &TransactionRecord,
        artifact_kind: &str,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let Some(evidence_ref) = record
            .evidence_refs
            .iter()
            .rev()
            .find(|evidence_ref| {
                evidence_ref.protocol.name == "ap2" && evidence_ref.artifact_kind == artifact_kind
            })
            .cloned()
        else {
            return Ok(None);
        };

        let Some(stored) = self
            .evidence_store
            .load(EvidenceLookup {
                evidence_ref,
                session_identity: context.session_identity.clone(),
            })
            .await?
        else {
            return Ok(None);
        };

        serde_json::from_slice(&stored.body).map(Some).map_err(|err| {
            AdkError::new(
                ErrorComponent::Artifact,
                ErrorCategory::Internal,
                "payments.ap2.evidence_decode_failed",
                format!(
                    "failed to decode stored AP2 `{artifact_kind}` evidence for transaction `{}`: {err}",
                    context.transaction_id
                ),
            )
        })
    }

    async fn store_intent_artifacts(
        &self,
        context: &CommerceContext,
        intent_mandate: &IntentMandate,
        authorization_artifact: Option<&AuthorizationArtifact>,
        verification: Option<&VerifiedAuthorization>,
    ) -> Result<StoredArtifactSet> {
        let mandate = self
            .store_json(context, "intent_mandate", context.transaction_id.as_str(), intent_mandate)
            .await?;
        let mut envelope = crate::domain::ProtocolExtensionEnvelope::new(ap2_descriptor())
            .with_field("artifact_kind", json!("intent_mandate"))
            .with_field("intent_expiry", json!(intent_mandate.intent_expiry))
            .with_field(
                "user_cart_confirmation_required",
                json!(intent_mandate.user_cart_confirmation_required),
            )
            .with_field(
                "has_merchant_constraints",
                json!(
                    intent_mandate
                        .merchants
                        .as_ref()
                        .is_some_and(|merchants| !merchants.is_empty())
                ),
            )
            .with_field(
                "has_sku_constraints",
                json!(intent_mandate.skus.as_ref().is_some_and(|skus| !skus.is_empty())),
            )
            .with_field("requires_refundability", json!(intent_mandate.requires_refundability));
        let mut evidence = vec![mandate.clone()];
        envelope.evidence_refs.push(mandate.evidence_ref.clone());

        if let Some(artifact) = authorization_artifact {
            let stored = self
                .store_inline_artifact(
                    context,
                    "intent_authorization",
                    context.transaction_id.as_str(),
                    artifact,
                )
                .await?;
            envelope.evidence_refs.push(stored.evidence_ref.clone());
            envelope.fields.insert(
                "intent_authorization_artifact".to_string(),
                json!({
                    "artifactType": artifact.artifact_type,
                    "contentType": artifact.content_type,
                }),
            );
            evidence.push(stored);
        }
        if let Some(verification) = verification {
            envelope
                .fields
                .insert("intent_authorization_verification".to_string(), json!(verification));
        }

        Ok(StoredArtifactSet { envelope, evidence })
    }

    async fn store_cart_artifacts(
        &self,
        context: &CommerceContext,
        cart_mandate: &CartMandate,
        merchant_authorization: &AuthorizationArtifact,
        verification: &VerifiedAuthorization,
    ) -> Result<StoredArtifactSet> {
        let mandate = self
            .store_json(context, "cart_mandate", &cart_mandate.contents.id, cart_mandate)
            .await?;
        let authorization = self
            .store_inline_artifact(
                context,
                "merchant_authorization",
                &cart_mandate.contents.id,
                merchant_authorization,
            )
            .await?;
        let all_items_refundable = cart_mandate
            .contents
            .payment_request
            .details
            .display_items
            .iter()
            .all(|item| item.refund_period > 0);
        let mut envelope = crate::domain::ProtocolExtensionEnvelope::new(ap2_descriptor())
            .with_field("artifact_kind", json!("cart_mandate"))
            .with_field("cart_id", json!(cart_mandate.contents.id))
            .with_field(
                "payment_details_id",
                json!(cart_mandate.contents.payment_request.details.id),
            )
            .with_field("cart_expiry", json!(cart_mandate.contents.cart_expiry))
            .with_field("merchant_name", json!(cart_mandate.contents.merchant_name))
            .with_field(
                "user_cart_confirmation_required",
                json!(cart_mandate.contents.user_cart_confirmation_required),
            )
            .with_field("total", json!(cart_mandate.contents.payment_request.details.total))
            .with_field("all_items_refundable", json!(all_items_refundable));
        envelope.evidence_refs.push(mandate.evidence_ref.clone());
        envelope.evidence_refs.push(authorization.evidence_ref.clone());
        envelope
            .fields
            .insert("merchant_authorization_verification".to_string(), json!(verification));
        Ok(StoredArtifactSet { envelope, evidence: vec![mandate, authorization] })
    }

    async fn store_payment_artifacts(
        &self,
        context: &CommerceContext,
        payment_mandate: &PaymentMandate,
        user_authorization: Option<&AuthorizationArtifact>,
        verification: Option<&VerifiedAuthorization>,
    ) -> Result<StoredArtifactSet> {
        let mandate = self
            .store_json(
                context,
                "payment_mandate",
                &payment_mandate.payment_mandate_contents.payment_mandate_id,
                payment_mandate,
            )
            .await?;
        let mut envelope =
            self.payment_envelope(payment_mandate, verification, std::slice::from_ref(&mandate));
        let mut evidence = vec![mandate];
        if let Some(user_authorization) = user_authorization {
            let authorization = self
                .store_inline_artifact(
                    context,
                    "user_authorization",
                    &payment_mandate.payment_mandate_contents.payment_mandate_id,
                    user_authorization,
                )
                .await?;
            envelope.evidence_refs.push(authorization.evidence_ref.clone());
            evidence.push(authorization);
        }

        Ok(StoredArtifactSet { envelope, evidence })
    }

    async fn store_receipt_artifacts(
        &self,
        context: &CommerceContext,
        payment_receipt: &PaymentReceipt,
    ) -> Result<StoredArtifactSet> {
        let receipt = self
            .store_json(context, "payment_receipt", &payment_receipt.payment_id, payment_receipt)
            .await?;
        let status = match payment_receipt.payment_status {
            PaymentStatusEnvelope::Success(_) => "success",
            PaymentStatusEnvelope::Error(_) => "error",
            PaymentStatusEnvelope::Failure(_) => "failure",
        };
        let method_name = payment_receipt
            .payment_method_details
            .as_ref()
            .and_then(|details| details.get("method_name"))
            .and_then(serde_json::Value::as_str);
        let mut envelope = crate::domain::ProtocolExtensionEnvelope::new(ap2_descriptor())
            .with_field("artifact_kind", json!("payment_receipt"))
            .with_field("payment_mandate_id", json!(payment_receipt.payment_mandate_id))
            .with_field("timestamp", json!(payment_receipt.timestamp))
            .with_field("payment_id", json!(payment_receipt.payment_id))
            .with_field("amount", json!(payment_receipt.amount))
            .with_field("status", json!(status));
        if let Some(method_name) = method_name {
            envelope = envelope.with_field("method_name", json!(method_name));
        }
        envelope.evidence_refs.push(receipt.evidence_ref.clone());
        Ok(StoredArtifactSet { envelope, evidence: vec![receipt] })
    }

    fn payment_envelope(
        &self,
        payment_mandate: &PaymentMandate,
        verification: Option<&VerifiedAuthorization>,
        evidence: &[StoredEvidence],
    ) -> crate::domain::ProtocolExtensionEnvelope {
        let mut envelope = crate::domain::ProtocolExtensionEnvelope::new(ap2_descriptor())
            .with_field("artifact_kind", json!("payment_mandate"))
            .with_field(
                "payment_mandate_id",
                json!(payment_mandate.payment_mandate_contents.payment_mandate_id),
            )
            .with_field(
                "payment_details_id",
                json!(payment_mandate.payment_mandate_contents.payment_details_id),
            )
            .with_field(
                "payment_request_id",
                json!(payment_mandate.payment_mandate_contents.payment_response.request_id),
            )
            .with_field(
                "payment_details_total",
                json!(payment_mandate.payment_mandate_contents.payment_details_total),
            )
            .with_field(
                "method_name",
                json!(payment_mandate.payment_mandate_contents.payment_response.method_name),
            )
            .with_field(
                "merchant_agent",
                json!(payment_mandate.payment_mandate_contents.merchant_agent),
            )
            .with_field("timestamp", json!(payment_mandate.payment_mandate_contents.timestamp))
            .with_field(
                "user_authorization_present",
                json!(payment_mandate.user_authorization.is_some()),
            );
        envelope.evidence_refs.extend(evidence.iter().map(|stored| stored.evidence_ref.clone()));
        if let Some(verification) = verification {
            envelope
                .fields
                .insert("user_authorization_verification".to_string(), json!(verification));
        }
        envelope
    }

    fn ap2_evidence_refs(
        &self,
        existing: &TransactionRecord,
        new_evidence: &[StoredEvidence],
    ) -> Vec<EvidenceReference> {
        let mut refs = existing
            .evidence_refs
            .iter()
            .filter(|evidence_ref| evidence_ref.protocol.name == "ap2")
            .cloned()
            .collect::<Vec<_>>();
        for stored in new_evidence {
            if !refs.contains(&stored.evidence_ref) {
                refs.push(stored.evidence_ref.clone());
            }
        }
        refs
    }

    async fn finalize_record(
        &self,
        context: &CommerceContext,
        mut record: TransactionRecord,
        artifacts: StoredArtifactSet,
        refs: ProtocolRefUpdate,
    ) -> Result<TransactionRecord> {
        if let Some(existing) = self.lookup(context).await? {
            self.merge_existing(&mut record, existing);
        }
        record.session_identity = context.session_identity.clone();
        update_record_extensions(&mut record, artifacts.envelope);
        for stored in artifacts.evidence {
            if !record.evidence_refs.contains(&stored.evidence_ref) {
                record.attach_evidence_ref(stored.evidence_ref.clone());
            }
            if !record
                .evidence_digests
                .iter()
                .any(|digest| digest.evidence_ref == stored.evidence_ref)
            {
                record.attach_evidence_digest(ProtocolEnvelopeDigest::new(
                    stored.evidence_ref.clone(),
                    Utc::now(),
                ));
            }
        }
        if let Some(intent_ref) = refs.intent_ref {
            record.protocol_refs.ap2_intent_mandate_id = Some(intent_ref);
        }
        if let Some(cart_ref) = refs.cart_ref {
            record.protocol_refs.ap2_cart_mandate_id = Some(cart_ref);
        }
        if let Some(payment_ref) = refs.payment_ref {
            record.protocol_refs.ap2_payment_mandate_id = Some(payment_ref);
        }
        if let Some(receipt_ref) = refs.receipt_ref {
            record.protocol_refs.ap2_payment_receipt_id = Some(receipt_ref);
        }
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    fn merge_existing(&self, record: &mut TransactionRecord, existing: TransactionRecord) {
        if record.fulfillment.is_none() {
            record.fulfillment = existing.fulfillment;
        }
        if record.order.is_none() {
            record.order = existing.order;
        }
        if record.payment_processor.is_none() {
            record.payment_processor = existing.payment_processor;
        }
        if record.protocol_refs.ap2_intent_mandate_id.is_none() {
            record.protocol_refs.ap2_intent_mandate_id =
                existing.protocol_refs.ap2_intent_mandate_id;
        }
        if record.protocol_refs.ap2_cart_mandate_id.is_none() {
            record.protocol_refs.ap2_cart_mandate_id = existing.protocol_refs.ap2_cart_mandate_id;
        }
        if record.protocol_refs.ap2_payment_mandate_id.is_none() {
            record.protocol_refs.ap2_payment_mandate_id =
                existing.protocol_refs.ap2_payment_mandate_id;
        }
        if record.protocol_refs.ap2_payment_receipt_id.is_none() {
            record.protocol_refs.ap2_payment_receipt_id =
                existing.protocol_refs.ap2_payment_receipt_id;
        }
        for reference in existing.protocol_refs.additional {
            if !record.protocol_refs.additional.contains(&reference) {
                record.protocol_refs.additional.push(reference);
            }
        }
        for envelope in existing.extensions.0 {
            if !record.extensions.as_slice().contains(&envelope) {
                record.attach_extension(envelope);
            }
        }
        for evidence_ref in existing.evidence_refs {
            if !record.evidence_refs.contains(&evidence_ref) {
                record.attach_evidence_ref(evidence_ref);
            }
        }
        for digest in existing.evidence_digests {
            if !record.evidence_digests.contains(&digest) {
                record.attach_evidence_digest(digest);
            }
        }
    }
}
