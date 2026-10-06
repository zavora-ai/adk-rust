use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use thiserror::Error;

use crate::domain::MoneyError;

/// AP2 adapter errors that map into the ADK structured error envelope.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum Ap2Error {
    #[error(
        "merchant authorization is required for cart mandate `{cart_id}`. Provide the merchant-signed authorization artifact before continuing."
    )]
    MissingMerchantAuthorization { cart_id: String },

    #[error(
        "user authorization is required for payment mandate `{payment_mandate_id}`. Obtain explicit user approval or a valid autonomous intent before continuing."
    )]
    MissingUserAuthorization { payment_mandate_id: String },

    #[error(
        "a detached user authorization artifact is required for the human-not-present intent on transaction `{transaction_id}`. Provide the signed intent authorization before continuing."
    )]
    MissingIntentAuthorization { transaction_id: String },

    #[error(
        "timestamp field `{field}` contains an invalid RFC 3339 value `{value}`. Normalize the AP2 artifact timestamp before retrying."
    )]
    InvalidTimestamp { field: String, value: String },

    #[error(
        "the AP2 artifact in `{field}` expired at `{expires_at}`. Refresh the mandate or obtain a new authorization before retrying."
    )]
    ExpiredArtifact { field: String, expires_at: String },

    #[error(
        "no AP2 verifier is configured for `{artifact_kind}`, so the artifact cannot be verified and is rejected. Configure a cryptographic verifier on `Ap2Adapter`, or call `allow_unverified_authorizations()` for local development only."
    )]
    AuthorizationVerifierNotConfigured { artifact_kind: String },

    #[error(
        "human-not-present transaction `{transaction_id}` lacks explicit authority constraints. Add merchant or SKU constraints before autonomous execution; a refundability requirement alone does not bound the agent's authority."
    )]
    MissingAuthorityConstraints { transaction_id: String },

    #[error(
        "cart mandate names merchant `{claimed}`, but its authorization was verified for merchant `{verified}`. Reject the cart or have the verified merchant sign it."
    )]
    MerchantIdentityMismatch { claimed: String, verified: String },

    #[error(
        "merchant `{merchant_name}` is outside the intent mandate authority constraints. Narrow the checkout or obtain fresh user approval."
    )]
    MerchantNotAuthorized { merchant_name: String },

    #[error(
        "the intent mandate constrains SKUs, but the cart mandate does not expose verifiable SKU identifiers. Use merchant constraints or include exact SKUs in cart metadata."
    )]
    SkuConstraintUnverifiable,

    #[error(
        "the cart includes SKU `{sku}` which is outside the signed intent authority constraints. Obtain fresh approval before continuing."
    )]
    SkuNotAuthorized { sku: String },

    #[error(
        "the intent mandate requires refundable items, but the cart contains non-refundable items. Return to the user or rebuild the cart with refundable items."
    )]
    RefundabilityRequired,

    #[error(
        "payment mandate `{payment_mandate_id}` does not match the current cart for field `{field}`. Rebuild the payment mandate from the latest cart state before retrying."
    )]
    PaymentMandateMismatch { payment_mandate_id: String, field: String },

    #[error(
        "payment mandate `{payment_mandate_id}` was already executed for transaction `{transaction_id}`. Payment mandates are single-use; create a new mandate for another payment."
    )]
    PaymentMandateReplayed { payment_mandate_id: String, transaction_id: String },

    #[error(
        "transaction `{transaction_id}` already executed payment mandate `{executed_payment_mandate_id}`. Start a new transaction for another payment."
    )]
    TransactionAlreadyPaid { transaction_id: String, executed_payment_mandate_id: String },

    #[error(
        "transaction `{transaction_id}` is `{state}` and no longer accepts payment mandates. Start a new transaction for another payment."
    )]
    TransactionNotPayable { transaction_id: String, state: String },

    #[error(
        "transaction `{transaction_id}` is bound to payment mandate `{bound_payment_mandate_id}`, not `{payment_mandate_id}`. Resubmit the bound mandate or start a new transaction."
    )]
    PaymentMandateRebind {
        transaction_id: String,
        bound_payment_mandate_id: String,
        payment_mandate_id: String,
    },

    #[error("invalid AP2 amount: {0}")]
    InvalidAmount(#[from] MoneyError),

    #[error(
        "canonical transaction `{transaction_id}` was not found. Create or resume the AP2 transaction before continuing."
    )]
    TransactionNotFound { transaction_id: String },

    #[error(
        "transaction `{transaction_id}` requires a return-to-user intervention, but no intervention service is configured. Wire an intervention backend or require explicit user authorization."
    )]
    InterventionServiceRequired { transaction_id: String },

    #[error(
        "the AP2 AgentCard extension URI `{uri}` is not supported. Use `{expected}` for the AP2 alpha baseline."
    )]
    InvalidExtensionUri { uri: String, expected: String },

    #[error(
        "the AP2 AgentCard extension must declare at least one AP2 role. Advertise one or more of shopper, merchant, credentials-provider, or payment-processor."
    )]
    MissingA2aRoles,
}

impl From<Ap2Error> for AdkError {
    fn from(value: Ap2Error) -> Self {
        let message = value.to_string();

        match value {
            Ap2Error::MissingMerchantAuthorization { .. }
            | Ap2Error::MissingUserAuthorization { .. }
            | Ap2Error::MissingIntentAuthorization { .. }
            | Ap2Error::InvalidTimestamp { .. }
            | Ap2Error::ExpiredArtifact { .. }
            | Ap2Error::MissingAuthorityConstraints { .. }
            | Ap2Error::MerchantNotAuthorized { .. }
            | Ap2Error::SkuConstraintUnverifiable
            | Ap2Error::SkuNotAuthorized { .. }
            | Ap2Error::RefundabilityRequired
            | Ap2Error::PaymentMandateMismatch { .. }
            | Ap2Error::InvalidExtensionUri { .. }
            | Ap2Error::MissingA2aRoles => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.ap2.invalid_input",
                message,
            ),
            Ap2Error::MerchantIdentityMismatch { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                "payments.ap2.merchant_identity_mismatch",
                message,
            ),
            Ap2Error::AuthorizationVerifierNotConfigured { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Unauthorized,
                "payments.ap2.verifier_not_configured",
                message,
            ),
            Ap2Error::PaymentMandateReplayed { .. }
            | Ap2Error::TransactionAlreadyPaid { .. }
            | Ap2Error::TransactionNotPayable { .. }
            | Ap2Error::PaymentMandateRebind { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                "payments.ap2.payment_mandate_refused",
                message,
            ),
            Ap2Error::InvalidAmount(_) => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.ap2.invalid_amount",
                message,
            ),
            Ap2Error::TransactionNotFound { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "payments.ap2.not_found",
                message,
            ),
            Ap2Error::InterventionServiceRequired { .. } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Unavailable,
                "payments.ap2.intervention_required",
                message,
            ),
        }
    }
}
