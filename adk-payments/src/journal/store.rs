use std::sync::Arc;

use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_session::{AppendEventRequest, SessionService};
use async_trait::async_trait;

use crate::domain::{TransactionRecord, TransactionState};
use crate::journal::memory_index::PaymentMemoryIndex;
use crate::journal::session_state::{
    ACTIVE_INDEX_KEY, COMPLETED_INDEX_KEY, TransactionLocator, build_journal_event, parse_locators,
    parse_record, transaction_state_storage_key,
};
use crate::kernel::commands::{ListUnresolvedTransactionsRequest, TransactionLookup};
use crate::kernel::service::TransactionStore;

/// Durable transaction store mirrored into app-scoped session state.
pub struct SessionBackedTransactionStore {
    session_service: Arc<dyn SessionService>,
    memory_index: Option<PaymentMemoryIndex>,
}

impl SessionBackedTransactionStore {
    /// Creates a new transaction store backed by `adk-session`.
    #[must_use]
    pub fn new(session_service: Arc<dyn SessionService>) -> Self {
        Self { session_service, memory_index: None }
    }

    /// Enables semantic indexing of safe summaries through `adk-memory`.
    #[must_use]
    pub fn with_memory_index(mut self, memory_index: PaymentMemoryIndex) -> Self {
        self.memory_index = Some(memory_index);
        self
    }

    fn require_identity<'a>(
        identity: &'a Option<AdkIdentity>,
        code: &'static str,
    ) -> Result<&'a AdkIdentity> {
        identity.as_ref().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::InvalidInput,
                code,
                "transaction journal operations require a session identity",
            )
        })
    }

    fn upsert_locator(index: &mut Vec<TransactionLocator>, locator: &TransactionLocator) {
        index.retain(|existing| existing != locator);
        index.push(locator.clone());
        index
            .sort_by(|left, right| left.transaction_id.as_str().cmp(right.transaction_id.as_str()));
    }

    fn remove_locator(index: &mut Vec<TransactionLocator>, locator: &TransactionLocator) {
        index.retain(|existing| existing != locator);
    }
}

#[async_trait]
impl TransactionStore for SessionBackedTransactionStore {
    async fn upsert(&self, mut record: TransactionRecord) -> Result<()> {
        record.recompute_safe_summary();
        let identity =
            Self::require_identity(&record.session_identity, "payments.journal.identity_required")?
                .clone();
        let session = self.session_service.get_for_identity(&identity).await?;
        let mut active = parse_locators(session.state().get(&format!("app:{ACTIVE_INDEX_KEY}")))?;
        let mut completed =
            parse_locators(session.state().get(&format!("app:{COMPLETED_INDEX_KEY}")))?;
        let locator = TransactionLocator {
            identity: identity.clone(),
            transaction_id: record.transaction_id.clone(),
        };

        if record.state.is_terminal() {
            Self::remove_locator(&mut active, &locator);
            Self::upsert_locator(&mut completed, &locator);
        } else {
            Self::upsert_locator(&mut active, &locator);
            Self::remove_locator(&mut completed, &locator);
        }

        let event = build_journal_event(&record, &active, &completed)?;
        self.session_service
            .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
            .await?;

        if let Some(memory_index) = &self.memory_index {
            memory_index.index_summary(&identity, &record.safe_summary).await?;
        }

        Ok(())
    }

    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        let identity =
            Self::require_identity(&lookup.session_identity, "payments.journal.lookup_identity")?;
        let session = self.session_service.get_for_identity(identity).await?;
        let key = transaction_state_storage_key(identity, &lookup.transaction_id);
        match session.state().get(&key) {
            Some(value) => parse_record(value).map(Some),
            None => Ok(None),
        }
    }

    async fn list_unresolved(
        &self,
        request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        let identity = Self::require_identity(
            &request.session_identity,
            "payments.journal.list_identity_required",
        )?;
        let session = self.session_service.get_for_identity(identity).await?;
        let active = parse_locators(session.state().get(&format!("app:{ACTIVE_INDEX_KEY}")))?;
        let mut records = Vec::new();

        for locator in active.into_iter().filter(|locator| &locator.identity == identity) {
            if let Some(record) = self
                .get(TransactionLookup {
                    transaction_id: locator.transaction_id,
                    session_identity: Some(identity.clone()),
                })
                .await?
            {
                if !matches!(
                    record.state,
                    TransactionState::Completed
                        | TransactionState::Canceled
                        | TransactionState::Failed
                ) {
                    records.push(record);
                }
            }
        }

        records
            .sort_by(|left, right| left.transaction_id.as_str().cmp(right.transaction_id.as_str()));
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use adk_artifact::InMemoryArtifactService;
    use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
    use adk_core::{Content, Event, EventCompaction};
    use adk_memory::{InMemoryMemoryService, MemoryService, SearchRequest};
    use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, EvidenceReference,
        InterventionKind, InterventionState, InterventionStatus, MerchantRef, Money,
        ProtocolDescriptor, ProtocolEnvelopeDigest, ProtocolExtensions, TransactionId,
        TransactionState,
    };
    use crate::journal::ArtifactBackedEvidenceStore;
    use crate::kernel::commands::{EvidenceLookup, StoreEvidenceCommand};
    use crate::kernel::service::EvidenceStore;

    async fn create_identity(session_service: &InMemorySessionService) -> AdkIdentity {
        let identity = AdkIdentity::new(
            AppName::try_from("payments-app").unwrap(),
            UserId::try_from("user-1").unwrap(),
            SessionId::try_from("session-1").unwrap(),
        );

        session_service
            .create(CreateRequest {
                app_name: identity.app_name.as_ref().to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                session_id: Some(identity.session_id.as_ref().to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        identity
    }

    fn sample_record(transaction_id: &str, identity: &AdkIdentity) -> TransactionRecord {
        let mut record = TransactionRecord::new(
            TransactionId::from(transaction_id),
            CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-123".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            CommerceMode::HumanPresent,
            Cart {
                cart_id: Some(format!("cart-{transaction_id}")),
                lines: vec![CartLine {
                    line_id: format!("line-{transaction_id}"),
                    merchant_sku: Some("sku-123".to_string()),
                    title: "Widget".to_string(),
                    quantity: 1,
                    unit_price: Money::new("USD", 1_500, 2),
                    total_price: Money::new("USD", 1_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", 1_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", 1_500, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
        );
        record.session_identity = Some(identity.clone());
        record
    }

    #[tokio::test]
    async fn recalls_unresolved_and_completed_transactions_after_compaction_like_history_loss() {
        let session_service = Arc::new(InMemorySessionService::new());
        let identity = create_identity(session_service.as_ref()).await;
        let store = SessionBackedTransactionStore::new(session_service.clone());

        let mut unresolved = sample_record("tx-unresolved", &identity);
        unresolved
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap();
        unresolved
            .transition_to(
                TransactionState::InterventionRequired(Box::new(InterventionState {
                    intervention_id: "int-1".to_string(),
                    kind: InterventionKind::BuyerReconfirmation,
                    status: InterventionStatus::Pending,
                    instructions: Some(
                        "return to the user for explicit reconfirmation".to_string(),
                    ),
                    continuation_token: Some("continue-123".to_string()),
                    requested_by: None,
                    expires_at: None,
                    extensions: ProtocolExtensions::default(),
                })),
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 10, 0).unwrap(),
            )
            .unwrap();
        store.upsert(unresolved.clone()).await.unwrap();

        let mut completed = sample_record("tx-completed", &identity);
        completed
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap();
        completed
            .transition_to(
                TransactionState::AwaitingPaymentMethod,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 6, 0).unwrap(),
            )
            .unwrap();
        completed
            .transition_to(
                TransactionState::Authorized,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 7, 0).unwrap(),
            )
            .unwrap();
        completed
            .transition_to(
                TransactionState::Completed,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 8, 0).unwrap(),
            )
            .unwrap();
        store.upsert(completed.clone()).await.unwrap();

        let mut compaction_event = Event::new("runner.compaction");
        compaction_event.author = "runner".to_string();
        compaction_event.set_content(Content::new("system").with_text("compacted payment history"));
        compaction_event.actions.compaction = Some(EventCompaction {
            start_timestamp: unresolved.created_at,
            end_timestamp: Utc.with_ymd_and_hms(2026, 3, 22, 10, 20, 0).unwrap(),
            compacted_content: Content::new("system").with_text("older conversation compacted"),
        });
        session_service
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: identity.clone(),
                event: compaction_event,
            })
            .await
            .unwrap();

        let recent_session = session_service
            .get(GetRequest {
                app_name: identity.app_name.as_ref().to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                session_id: identity.session_id.as_ref().to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(recent_session.events().len(), 1);

        let unresolved_after = store
            .list_unresolved(ListUnresolvedTransactionsRequest {
                session_identity: Some(identity.clone()),
            })
            .await
            .unwrap();
        assert_eq!(unresolved_after.len(), 1);
        assert_eq!(unresolved_after[0].transaction_id.as_str(), "tx-unresolved");
        assert_eq!(
            unresolved_after[0].safe_summary.state,
            crate::domain::TransactionStateTag::InterventionRequired
        );

        let completed_after = store
            .get(TransactionLookup {
                transaction_id: TransactionId::from("tx-completed"),
                session_identity: Some(identity.clone()),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed_after.state, TransactionState::Completed);
    }

    #[tokio::test]
    async fn keeps_raw_evidence_out_of_state_transcript_and_memory() {
        let session_service = Arc::new(InMemorySessionService::new());
        let artifact_service = Arc::new(InMemoryArtifactService::new());
        let memory_service = Arc::new(InMemoryMemoryService::new());
        let identity = create_identity(session_service.as_ref()).await;
        let store = SessionBackedTransactionStore::new(session_service.clone())
            .with_memory_index(PaymentMemoryIndex::new(memory_service.clone()));
        let evidence_store = ArtifactBackedEvidenceStore::new(artifact_service);

        let raw_secret =
            r#"{"pan":"4111111111111111","cvc":"123","signedAuthorization":"signed_blob"}"#;

        let mut record = sample_record("tx-secret", &identity);
        store.upsert(record.clone()).await.unwrap();

        let stored = evidence_store
            .store(StoreEvidenceCommand {
                transaction_id: record.transaction_id.clone(),
                session_identity: Some(identity.clone()),
                evidence_ref: EvidenceReference {
                    evidence_id: "payment-mandate-raw".to_string(),
                    protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
                    artifact_kind: "payment_mandate".to_string(),
                    digest: None,
                },
                body: raw_secret.as_bytes().to_vec(),
                content_type: "application/json".to_string(),
            })
            .await
            .unwrap();

        record.attach_evidence_ref(stored.evidence_ref.clone());
        record.attach_evidence_digest(ProtocolEnvelopeDigest::new(
            stored.evidence_ref.clone(),
            Utc.with_ymd_and_hms(2026, 3, 22, 10, 11, 0).unwrap(),
        ));
        store.upsert(record.clone()).await.unwrap();

        let session = session_service.get_for_identity(&identity).await.unwrap();
        let state_json = serde_json::to_string(&session.state().all()).unwrap();
        assert!(!state_json.contains("4111111111111111"));
        assert!(!state_json.contains("signed_blob"));

        let transcript_text = session
            .events()
            .all()
            .into_iter()
            .filter_map(|event| event.content().cloned())
            .flat_map(|content| content.parts.into_iter())
            .filter_map(|part| part.text().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!transcript_text.contains("4111111111111111"));
        assert!(!transcript_text.contains("signed_blob"));
        assert!(transcript_text.contains("Merchant Example"));

        let memories = memory_service
            .search(SearchRequest {
                query: "Merchant Example Widget".to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                app_name: identity.app_name.as_ref().to_string(),
                limit: None,
                min_score: None,
            })
            .await
            .unwrap();
        let memory_text = memories
            .memories
            .into_iter()
            .flat_map(|entry| entry.content.parts.into_iter())
            .filter_map(|part| part.text().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!memory_text.contains("4111111111111111"));
        assert!(!memory_text.contains("signed_blob"));
        assert!(memory_text.contains("Merchant Example"));

        let loaded = evidence_store
            .load(EvidenceLookup {
                evidence_ref: stored.evidence_ref.clone(),
                session_identity: Some(identity.clone()),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.body, raw_secret.as_bytes());
        assert_eq!(loaded.content_type, "application/json");
    }

    #[tokio::test]
    async fn redacts_sensitive_summary_data_before_transcript_and_memory_writes() {
        let session_service = Arc::new(InMemorySessionService::new());
        let memory_service = Arc::new(InMemoryMemoryService::new());
        let identity = create_identity(session_service.as_ref()).await;
        let store = SessionBackedTransactionStore::new(session_service.clone())
            .with_memory_index(PaymentMemoryIndex::new(memory_service.clone()));

        let mut record = sample_record("tx-redacted", &identity);
        record.cart.lines[0].title = "Widget 4111 1111 1111 1111".to_string();
        record
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::InterventionRequired(Box::new(InterventionState {
                    intervention_id: "int-summary".to_string(),
                    kind: InterventionKind::BuyerReconfirmation,
                    status: InterventionStatus::Pending,
                    instructions: Some(
                        "billing address: 123 Main St; email payer@example.com; signature=signed_blob"
                            .to_string(),
                    ),
                    continuation_token: Some("continue-123".to_string()),
                    requested_by: None,
                    expires_at: None,
                    extensions: ProtocolExtensions::default(),
                })),
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 6, 0).unwrap(),
            )
            .unwrap();

        store.upsert(record.clone()).await.unwrap();

        let session = session_service.get_for_identity(&identity).await.unwrap();
        let transcript_text = session
            .events()
            .all()
            .into_iter()
            .filter_map(|event| event.content().cloned())
            .flat_map(|content| content.parts.into_iter())
            .filter_map(|part| part.text().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!transcript_text.contains("4111 1111 1111 1111"));
        assert!(!transcript_text.contains("payer@example.com"));
        assert!(!transcript_text.contains("signed_blob"));
        assert!(transcript_text.contains("[CARD ****1111]"));
        assert!(transcript_text.contains("[EMAIL REDACTED]"));
        assert!(transcript_text.contains("[REDACTED sha256:"));

        let memories = memory_service
            .search(SearchRequest {
                query: "Merchant Widget".to_string(),
                user_id: identity.user_id.as_ref().to_string(),
                app_name: identity.app_name.as_ref().to_string(),
                limit: None,
                min_score: None,
            })
            .await
            .unwrap();
        let memory_text = memories
            .memories
            .into_iter()
            .flat_map(|entry| entry.content.parts.into_iter())
            .filter_map(|part| part.text().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!memory_text.contains("4111 1111 1111 1111"));
        assert!(!memory_text.contains("payer@example.com"));
        assert!(!memory_text.contains("signed_blob"));
        assert!(memory_text.contains("[CARD ****1111]"));
        assert!(memory_text.contains("[EMAIL REDACTED]"));
        assert!(memory_text.contains("[REDACTED sha256:"));
    }
}
