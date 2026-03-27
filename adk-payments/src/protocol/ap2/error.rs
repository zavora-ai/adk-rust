use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use thiserror::Error;

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
        "human-not-present transaction `{transaction_id}` lacks explicit authority constraints. Add merchant, SKU, or refundability constraints before autonomous execution."
    )]
    MissingAuthorityConstraints { transaction_id: String },

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
