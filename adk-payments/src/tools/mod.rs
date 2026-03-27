//! Agent-facing payment tool builders backed by the canonical commerce kernel.
//!
//! Every tool in this module returns only masked structured outputs via
//! [`SafeTransactionSummary`](crate::domain::SafeTransactionSummary) and
//! redacted JSON. Raw sensitive payment data never appears in tool results.
//!
//! # Supported operations
//!
//! | Tool | Scope | Description |
//! |------|-------|-------------|
//! | `payments_checkout_create` | `payments:checkout:create` | Create a new checkout session |
//! | `payments_checkout_update` | `payments:checkout:update` | Update cart or fulfillment |
//! | `payments_checkout_complete` | `payments:checkout:complete` | Finalize and produce an order |
//! | `payments_checkout_cancel` | `payments:checkout:cancel` | Cancel a checkout or transaction |
//! | `payments_status_lookup` | `payments:checkout:create` | Look up transaction status |
//! | `payments_intervention_continue` | `payments:intervention:continue` | Resume an intervention |
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_payments::tools::PaymentToolsetBuilder;
//!
//! let toolset = PaymentToolsetBuilder::new(checkout_service, transaction_store)
//!     .with_intervention_service(intervention_service)
//!     .build();
//! let tools = toolset.tools();
//! ```

mod checkout;
mod intervention;
mod status;
mod toolset;

pub use checkout::{
    cancel_checkout_tool, complete_checkout_tool, create_checkout_tool, update_checkout_tool,
};
pub use intervention::continue_intervention_tool;
pub use status::status_lookup_tool;
pub use toolset::{PaymentToolset, PaymentToolsetBuilder};
