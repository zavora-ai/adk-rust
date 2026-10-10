use std::sync::Arc;

use adk_auth::{ContextScopeResolver, ScopeGuard};
use adk_core::Tool;

use crate::kernel::service::{InterventionService, MerchantCheckoutService, TransactionStore};

use super::{
    cancel_checkout_tool, complete_checkout_tool, continue_intervention_tool, create_checkout_tool,
    status_lookup_tool, update_checkout_tool,
};

/// Builder for the canonical payment toolset.
///
/// Produces a set of scope-protected, redaction-safe tools backed by the
/// commerce kernel service traits. Every tool is wrapped in an `adk-auth`
/// [`ScopeGuard`], so a call fails unless the caller holds the scopes the tool
/// declares through `Tool::required_scopes()`. The default guard reads them from
/// `ToolContext::user_scopes()` ([`ContextScopeResolver`]), which is empty unless a
/// request context grants scopes.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::{ScopeGuard, StaticScopeResolver};
/// use adk_payments::tools::PaymentToolsetBuilder;
///
/// let toolset = PaymentToolsetBuilder::new(checkout_service, transaction_store)
///     .with_scope_guard(ScopeGuard::new(StaticScopeResolver::new(vec![
///         "payments:checkout:create".to_string(),
///     ])))
///     .build();
/// ```
pub struct PaymentToolsetBuilder {
    checkout_service: Arc<dyn MerchantCheckoutService>,
    transaction_store: Arc<dyn TransactionStore>,
    intervention_service: Option<Arc<dyn InterventionService>>,
    scope_guard: Option<ScopeGuard>,
}

impl PaymentToolsetBuilder {
    /// Creates a new builder with the required checkout and transaction services.
    #[must_use]
    pub fn new(
        checkout_service: Arc<dyn MerchantCheckoutService>,
        transaction_store: Arc<dyn TransactionStore>,
    ) -> Self {
        Self { checkout_service, transaction_store, intervention_service: None, scope_guard: None }
    }

    /// Enables the intervention continuation tool.
    #[must_use]
    pub fn with_intervention_service(
        mut self,
        intervention_service: Arc<dyn InterventionService>,
    ) -> Self {
        self.intervention_service = Some(intervention_service);
        self
    }

    /// Replaces the guard that enforces each tool's declared scopes.
    ///
    /// Defaults to `ScopeGuard::new(ContextScopeResolver)`. Supply a guard with a
    /// different resolver, or one with an audit sink, to change where scopes come
    /// from or to record each decision.
    #[must_use]
    pub fn with_scope_guard(mut self, scope_guard: ScopeGuard) -> Self {
        self.scope_guard = Some(scope_guard);
        self
    }

    /// Builds the payment toolset containing all configured tools.
    #[must_use]
    pub fn build(self) -> PaymentToolset {
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(create_checkout_tool(self.checkout_service.clone())),
            Arc::new(update_checkout_tool(self.checkout_service.clone())),
            Arc::new(complete_checkout_tool(self.checkout_service.clone())),
            Arc::new(cancel_checkout_tool(self.checkout_service.clone())),
            Arc::new(status_lookup_tool(self.transaction_store.clone())),
        ];
        if let Some(intervention_service) = self.intervention_service {
            tools.push(Arc::new(continue_intervention_tool(intervention_service)));
        }
        let scope_guard = self.scope_guard.unwrap_or_else(|| ScopeGuard::new(ContextScopeResolver));
        PaymentToolset { tools: scope_guard.protect_all(tools) }
    }
}

/// A set of agent-facing payment tools backed by the canonical commerce kernel.
///
/// Every tool is scope-protected; see [`PaymentToolsetBuilder`].
pub struct PaymentToolset {
    tools: Vec<Arc<dyn Tool>>,
}

impl PaymentToolset {
    /// Returns all configured payment tools.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
