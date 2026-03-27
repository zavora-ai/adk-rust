//! Pluggable verification for AP2 merchant authorization and user authorization
//! artifacts.
//!
//! AP2 artifacts are sensitive and still evolving. This module provides
//! pluggable verification traits rather than baking in one cryptographic
//! assumption, keeping alpha support honest while giving integrators a strong
//! abstraction boundary.
//!
//! Verified results carry [`EvidenceReference`] so callers can persist them as
//! durable evidence records (Requirement 6.7, 9.4).

use adk_core::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::domain::{EvidenceReference, ProtocolDescriptor};

use super::mandates::{CartMandate, PaymentMandate};

// ---------------------------------------------------------------------------
// Mandate envelopes
// ---------------------------------------------------------------------------

/// Wrapper around a [`CartMandate`] plus its raw bytes for verification.
///
/// The raw bytes allow verifiers to check signatures or digests against the
/// original wire representation without re-serializing the parsed mandate.
#[derive(Debug, Clone)]
pub struct CartMandateEnvelope {
    /// The parsed cart mandate.
    pub mandate: CartMandate,
    /// Raw bytes of the mandate as received on the wire.
    pub raw_bytes: Vec<u8>,
}

/// Wrapper around a [`PaymentMandate`] plus its raw bytes for verification.
///
/// The raw bytes allow verifiers to check signatures or digests against the
/// original wire representation without re-serializing the parsed mandate.
#[derive(Debug, Clone)]
pub struct PaymentMandateEnvelope {
    /// The parsed payment mandate.
    pub mandate: PaymentMandate,
    /// Raw bytes of the mandate as received on the wire.
    pub raw_bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Verified mandate results
// ---------------------------------------------------------------------------

/// Result of successful cart mandate verification.
///
/// Contains the verified mandate and an [`EvidenceReference`] that callers
/// should persist as a durable evidence record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedCartMandate {
    /// The verified cart mandate.
    pub mandate: CartMandate,
    /// Evidence reference for the verification artifact.
    pub evidence_ref: EvidenceReference,
}

/// Result of successful payment mandate verification.
///
/// Contains the verified mandate and an [`EvidenceReference`] that callers
/// should persist as a durable evidence record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPaymentMandate {
    /// The verified payment mandate.
    pub mandate: PaymentMandate,
    /// Evidence reference for the verification artifact.
    pub evidence_ref: EvidenceReference,
}

// ---------------------------------------------------------------------------
// Mandate verifier trait
// ---------------------------------------------------------------------------

/// Pluggable verifier for AP2 merchant authorization mandates.
///
/// Implementations may verify cryptographic signatures, check certificate
/// chains, validate timestamps, or apply any domain-specific policy before
/// accepting a mandate as verified.
#[async_trait]
pub trait MandateVerifier: Send + Sync {
    /// Verifies a cart mandate envelope and returns a verified result with an
    /// evidence reference on success.
    async fn verify_cart_mandate(
        &self,
        envelope: &CartMandateEnvelope,
    ) -> Result<VerifiedCartMandate>;

    /// Verifies a payment mandate envelope and returns a verified result with
    /// an evidence reference on success.
    async fn verify_payment_mandate(
        &self,
        envelope: &PaymentMandateEnvelope,
    ) -> Result<VerifiedPaymentMandate>;
}

// ---------------------------------------------------------------------------
// User authorization envelope and verified result
// ---------------------------------------------------------------------------

/// Wrapper for a user authorization presentation (e.g. JWT, signed payload).
///
/// The `kind` field describes the authorization mechanism so verifiers can
/// dispatch appropriately.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAuthorizationEnvelope {
    /// The kind of authorization presentation (e.g. `"jwt"`, `"signed_payload"`).
    pub kind: String,
    /// Raw bytes of the authorization artifact.
    pub raw_bytes: Vec<u8>,
    /// Optional metadata about the authorization source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Result of successful user authorization verification.
///
/// Contains the verified subject identity, granted scopes, and an
/// [`EvidenceReference`] for durable evidence storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUserAuthorization {
    /// The verified subject identity (e.g. user ID, email).
    pub subject: String,
    /// Scopes or permissions granted by the authorization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Evidence reference for the verification artifact.
    pub evidence_ref: EvidenceReference,
}

// ---------------------------------------------------------------------------
// User authorization verifier trait
// ---------------------------------------------------------------------------

/// Pluggable verifier for AP2 user authorization artifacts.
///
/// Implementations may validate JWTs, verify signed payloads, check
/// certificate chains, or apply any domain-specific policy.
#[async_trait]
pub trait UserAuthorizationVerifier: Send + Sync {
    /// Verifies a user authorization envelope and returns a verified result
    /// with an evidence reference on success.
    async fn verify_user_authorization(
        &self,
        envelope: &UserAuthorizationEnvelope,
    ) -> Result<VerifiedUserAuthorization>;
}

// ---------------------------------------------------------------------------
// NoOp mandate verifier (development / testing)
// ---------------------------------------------------------------------------

/// A default [`MandateVerifier`] that accepts all mandates without
/// cryptographic verification.
///
/// Generates placeholder [`EvidenceReference`] values using
/// [`ProtocolDescriptor::ap2("v0.1-alpha")`]. Intended for development and
/// testing only.
#[derive(Debug, Clone, Default)]
pub struct NoOpMandateVerifier;

impl NoOpMandateVerifier {
    /// Creates a new no-op mandate verifier.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    fn make_evidence_ref(mandate_id: &str, artifact_kind: &str) -> EvidenceReference {
        EvidenceReference {
            evidence_id: format!("noop-{mandate_id}"),
            protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
            artifact_kind: artifact_kind.to_string(),
            digest: None,
        }
    }
}

#[async_trait]
impl MandateVerifier for NoOpMandateVerifier {
    async fn verify_cart_mandate(
        &self,
        envelope: &CartMandateEnvelope,
    ) -> Result<VerifiedCartMandate> {
        Ok(VerifiedCartMandate {
            mandate: envelope.mandate.clone(),
            evidence_ref: Self::make_evidence_ref(&envelope.mandate.id, "cart_mandate"),
        })
    }

    async fn verify_payment_mandate(
        &self,
        envelope: &PaymentMandateEnvelope,
    ) -> Result<VerifiedPaymentMandate> {
        Ok(VerifiedPaymentMandate {
            mandate: envelope.mandate.clone(),
            evidence_ref: Self::make_evidence_ref(&envelope.mandate.id, "payment_mandate"),
        })
    }
}
