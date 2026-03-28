//! Agent Payments Protocol adapter scaffolding.
//!
//! This module targets AP2 `v0.1-alpha` as of `2026-03-22`. A2A-oriented
//! surfaces are additive behind `ap2-a2a`, and MCP-oriented surfaces are
//! additive behind `ap2-mcp`.

mod adapter;
mod error;
mod mapper;
mod types;
mod verification;

#[cfg(feature = "ap2-a2a")]
#[cfg_attr(docsrs, doc(cfg(feature = "ap2-a2a")))]
pub mod a2a;

#[cfg(feature = "ap2-mcp")]
#[cfg_attr(docsrs, doc(cfg(feature = "ap2-mcp")))]
pub mod mcp;

pub use adapter::Ap2Adapter;
pub use error::Ap2Error;
pub use types::{
    AP2_A2A_EXTENSION_URI, AP2_CART_MANDATE_DATA_KEY, AP2_CONTACT_ADDRESS_DATA_KEY,
    AP2_INTENT_MANDATE_DATA_KEY, AP2_PAYMENT_MANDATE_DATA_KEY, AP2_PAYMENT_METHOD_DATA_KEY,
    AP2_PAYMENT_RECEIPT_DATA_KEY, Ap2Role, Ap2RoleMetadata, AuthorizationArtifact, CartContents,
    CartMandate, ContactAddress, IntentMandate, PaymentCurrencyAmount, PaymentDetailsInit,
    PaymentDetailsModifier, PaymentErrorStatus, PaymentFailureStatus, PaymentItem, PaymentMandate,
    PaymentMandateContents, PaymentMethodData, PaymentOptions, PaymentReceipt, PaymentResponse,
    PaymentShippingOption, PaymentStatusEnvelope, PaymentSuccessStatus,
};
pub use verification::{
    MerchantAuthorizationVerifier, RequireMerchantAuthorization, RequireUserAuthorization,
    UserAuthorizationVerifier, VerifiedAuthorization,
};

#[cfg(feature = "ap2-a2a")]
pub use a2a::{Ap2A2aArtifact, Ap2A2aMessage, Ap2A2aPart, Ap2AgentCardExtension};

#[cfg(feature = "ap2-mcp")]
pub use mcp::{
    Ap2McpInterventionStatus, Ap2McpMandateStatus, Ap2McpPaymentContinuation, Ap2McpReceiptStatus,
    Ap2ReceiptStatusKind,
};
