use std::sync::Arc;

use adk_core::Tool;

use crate::kernel::service::{InterventionService, MerchantCheckoutService, TransactionStore};

use super::{
    cancel_checkout_tool, complete_checkout_tool, continue_intervention_tool, create_checkout_tool,
    status_lookup_tool, update_checkout_tool,
};

/// Builder for the canonical payment toolset.
///
/// Produces a set of scope-protected, redaction-safe tools backed by the
/// commerce kernel service traits.
pub struct PaymentToolsetBuilder {
    checkout_service: Arc<dyn MerchantCheckoutService>,
    transaction_store: Arc<dyn TransactionStore>,
    intervention_service: Option<Arc<dyn InterventionService>>,
}

impl PaymentToolsetBuilder {
    /// Creates a new builder with the required checkout and transaction services.
    #[must_use]
    pub fn new(
        checkout_service: Arc<dyn MerchantCheckoutService>,
        transaction_store: Arc<dyn TransactionStore>,
    ) -> Self {
        Self { checkout_service, transaction_store, intervention_service: None }
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
        PaymentToolset { tools }
    }
}

/// A set of agent-facing payment tools backed by the canonical commerce kernel.
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
