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
//! [`PaymentToolsetBuilder`] wraps every tool in an `adk-auth` `ScopeGuard`. The
//! individual constructors such as [`create_checkout_tool`] return the bare tool,
//! which declares its scopes but does not enforce them; wrap it with
//! `adk_auth::ScopeGuard::protect` before giving it to an agent.
//!
//! Each call records the calling agent as the acting [`CommerceActor`] and binds
//! the transaction to the caller's session identity, both taken from the tool
//! context.
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

use adk_core::ToolContext;
use adk_core::identity::AdkIdentity;

use crate::domain::{CommerceActor, CommerceActorRole, ProtocolExtensions};

/// The calling agent, as the actor a payment tool reports to the commerce kernel.
fn calling_agent(ctx: &dyn ToolContext) -> CommerceActor {
    CommerceActor {
        actor_id: ctx.agent_name().to_string(),
        role: CommerceActorRole::AgentSurface,
        display_name: Some(ctx.agent_name().to_string()),
        tenant_id: None,
        extensions: ProtocolExtensions::default(),
    }
}

/// The caller's session identity, when the context's identifiers are valid.
fn caller_identity(ctx: &dyn ToolContext) -> Option<AdkIdentity> {
    ctx.try_identity().ok()
}

pub use checkout::{
    cancel_checkout_tool, complete_checkout_tool, create_checkout_tool, update_checkout_tool,
};
pub use intervention::continue_intervention_tool;
pub use status::status_lookup_tool;
pub use toolset::{PaymentToolset, PaymentToolsetBuilder};
