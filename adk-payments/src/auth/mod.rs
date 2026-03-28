//! Payment authorization, actor binding, and audit integration points.
//!
//! The types in this module keep authenticated request identity separate from
//! session identity and protocol actor roles while integrating with
//! `adk-auth` for scope checks and audit sink emission across ACP stable
//! `2026-01-30` and AP2 `v0.1-alpha` (`2026-03-22`) flows.

mod audit;
mod binding;
mod error;
mod scopes;

pub use audit::PaymentAuditor;
pub use binding::AuthenticatedPaymentRequest;
pub use error::PaymentsAuthError;
pub use scopes::{
    ALL_PAYMENT_SCOPES, CHECKOUT_CANCEL_SCOPES, CHECKOUT_COMPLETE_SCOPES, CHECKOUT_CREATE_SCOPES,
    CHECKOUT_UPDATE_SCOPES, CREDENTIAL_DELEGATE_SCOPES, INTERVENTION_CONTINUE_SCOPES,
    ORDER_UPDATE_SCOPES, PAYMENT_ADMIN_SCOPE, PAYMENT_CHECKOUT_CANCEL_SCOPE,
    PAYMENT_CHECKOUT_COMPLETE_SCOPE, PAYMENT_CHECKOUT_CREATE_SCOPE, PAYMENT_CHECKOUT_UPDATE_SCOPE,
    PAYMENT_CREDENTIAL_DELEGATE_SCOPE, PAYMENT_INTERVENTION_CONTINUE_SCOPE,
    PAYMENT_ORDER_UPDATE_SCOPE, PAYMENT_SETTLEMENT_SCOPE, PaymentOperation, SETTLEMENT_SCOPES,
    check_payment_operation_scopes,
};
