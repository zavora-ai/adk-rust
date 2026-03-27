use std::sync::Arc;

use adk_core::identity::AdkIdentity;
use adk_core::{Content, Result};
use adk_memory::{MemoryEntry, MemoryService};

use crate::domain::SafeTransactionSummary;
use crate::guardrail::redact_payment_content;

/// Indexes safe payment summaries into semantic memory.
pub struct PaymentMemoryIndex {
    memory_service: Arc<dyn MemoryService>,
}

impl PaymentMemoryIndex {
    /// Creates a new safe-summary memory indexer.
    #[must_use]
    pub fn new(memory_service: Arc<dyn MemoryService>) -> Self {
        Self { memory_service }
    }

    /// Indexes one safe transaction summary under a transaction-scoped memory session key.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying memory service rejects the write.
    pub async fn index_summary(
        &self,
        identity: &AdkIdentity,
        summary: &SafeTransactionSummary,
    ) -> Result<()> {
        let session_key = format!(
            "payments:{}:{}",
            identity.session_id.as_ref(),
            summary.transaction_id.as_str()
        );
        let entry = MemoryEntry {
            content: redact_payment_content(
                &Content::new("system").with_text(summary.transcript_text()),
            ),
            author: "adk-payments".to_string(),
            timestamp: summary.updated_at,
        };

        self.memory_service
            .add_session(
                identity.app_name.as_ref(),
                identity.user_id.as_ref(),
                &session_key,
                vec![entry],
            )
            .await
    }
}
