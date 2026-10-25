//! Consent receipts (spec 3.5 §7.5).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::capability::ConsentRequirement;
use super::identity::Proof;
use super::release::Digest;
use super::signal::Channel;

/// A consent receipt, signed by the edge and returned to the subject (spec 3.5 §7.5).
///
/// A consent-bound action refuses without a live receipt for its scope and policy version
/// (A-I8).
///
/// # Example
///
/// ```
/// use awp_types::{Channel, ConsentReceipt, ConsentRequirement, ConsentScope, Digest, LegalBasis, Proof};
/// use chrono::{Duration, Utc};
/// use uuid::Uuid;
///
/// let now = Utc::now();
/// let receipt = ConsentReceipt {
///     id: Uuid::nil(),
///     subject_ref: "subj_8f3a".into(),
///     scope: ConsentScope::Newsletter,
///     policy_version: "2026-10".into(),
///     policy_digest: Digest::from_sha256_bytes([7; 32]),
///     legal_basis: LegalBasis::ExplicitConsent,
///     jurisdiction: "KE".into(),
///     granted_at: now,
///     expires_at: Some(now + Duration::days(365)),
///     channel: Channel::Web,
///     proof: Proof::eddsa("edge-2026-10", "e30..c2ln"),
/// };
/// let needed = ConsentRequirement { scope: ConsentScope::Newsletter, policy_version: "2026-10".into() };
/// assert!(receipt.covers(&needed, now));
/// assert!(!receipt.covers(&needed, now + Duration::days(400)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ConsentReceipt {
    /// Receipt id.
    pub id: Uuid,
    /// Pseudonymous reference to the subject; never personal data.
    pub subject_ref: String,
    /// The consented scope.
    pub scope: ConsentScope,
    /// Version of the policy the subject agreed to.
    pub policy_version: String,
    /// Digest of the policy text the subject agreed to.
    pub policy_digest: Digest,
    /// Legal basis for processing.
    pub legal_basis: LegalBasis,
    /// ISO 3166-1 alpha-2 code of the subject's jurisdiction.
    pub jurisdiction: String,
    /// When consent was given.
    pub granted_at: DateTime<Utc>,
    /// When consent lapses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The channel consent was given on.
    pub channel: Channel,
    /// The edge's detached JWS, by a delegated `receipt` key, over the JCS form of the receipt
    /// without `proof`.
    pub proof: Proof,
}

impl ConsentReceipt {
    /// Returns `true` when the receipt covers `requirement` and is live at `at`. Revocation is
    /// recorded by the consent store, not on the receipt, so callers check it separately.
    pub fn covers(&self, requirement: &ConsentRequirement, at: DateTime<Utc>) -> bool {
        self.scope == requirement.scope
            && self.policy_version == requirement.policy_version
            && self.granted_at <= at
            && self.expires_at.is_none_or(|expires_at| at < expires_at)
    }
}

open_enum! {
    /// A consent scope (spec 3.5 §7.5): the four scopes of the AWP 1.0 draft, `newsletter`,
    /// and custom scopes.
    ///
    /// # Example
    ///
    /// ```
    /// use awp_types::ConsentScope;
    ///
    /// assert_eq!(ConsentScope::ProactiveOutreach.as_str(), "proactive_outreach");
    /// assert_eq!(ConsentScope::from("contact"), ConsentScope::Custom("contact".into()));
    /// assert!(!ConsentScope::from("contact").is_known());
    /// ```
    pub enum ConsentScope {
        /// Link sessions across channels for the same visitor.
        SessionContinuity => "session_continuity",
        /// Send proactive messages to the visitor.
        ProactiveOutreach => "proactive_outreach",
        /// Store visitor data beyond the session.
        PersistentProfile => "persistent_profile",
        /// Store payment preferences.
        PaymentData => "payment_data",
        /// Send the site's newsletter.
        Newsletter => "newsletter",
        @other
        /// A scope the site defines, such as `contact`.
        Custom(String),
    }
}

/// Legal basis for processing under a consent receipt (spec 3.5 §7.5).
///
/// The first three are the AWP 1.0 draft's values; the rest complete the six bases of GDPR
/// Article 6(1), which public bodies rely on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LegalBasis {
    /// Explicit consent; required for `proactive_outreach`.
    ExplicitConsent,
    /// Legitimate interest.
    LegitimateInterest,
    /// Necessary for a contract with the subject.
    ContractualNecessity,
    /// Required by law.
    LegalObligation,
    /// Protects someone's vital interests.
    VitalInterest,
    /// A task in the public interest or official authority.
    PublicTask,
}
