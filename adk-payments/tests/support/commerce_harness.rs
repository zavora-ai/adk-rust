use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_memory::{InMemoryMemoryService, MemoryService, SearchRequest};
use adk_payments::ACP_STABLE_BASELINE;
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, EvidenceReference, MerchantRef, Money,
    OrderSnapshot, OrderState, PaymentProcessorRef, ProtocolDescriptor, ProtocolEnvelopeDigest,
    ProtocolExtensionEnvelope, ProtocolExtensions, ReceiptState, TransactionId, TransactionRecord,
    TransactionState,
};
use adk_payments::journal::{
    ArtifactBackedEvidenceStore, PaymentMemoryIndex, SessionBackedTransactionStore,
};
use adk_payments::kernel::{
    BeginInterventionCommand, CancelCheckoutCommand, CommerceContext, ContinueInterventionCommand,
    DelegatePaymentCommand, DelegatedPaymentResult, EvidenceLookup, EvidenceStore,
    ExecutePaymentCommand, InterventionService, ListUnresolvedTransactionsRequest,
    MerchantCheckoutService, OrderUpdateCommand, PaymentExecutionOutcome, PaymentExecutionResult,
    PaymentExecutionService, StoreEvidenceCommand, StoredEvidence, SyncPaymentOutcomeCommand,
    TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
#[cfg(feature = "acp")]
use adk_payments::protocol::acp::AcpContextTemplate;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct MultiActorHarnessConfig {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub merchant_of_record: MerchantRef,
    #[cfg(feature = "ap2")]
    pub payment_processor_ref: PaymentProcessorRef,
}

impl MultiActorHarnessConfig {
    #[cfg(feature = "acp")]
    #[must_use]
    pub fn acp_defaults() -> Self {
        Self {
            app_name: "payments-app".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            merchant_of_record: MerchantRef {
                merchant_id: "merchant-123".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            #[cfg(feature = "ap2")]
            payment_processor_ref: PaymentProcessorRef {
                processor_id: "stripe-test".to_string(),
                name: "Stripe Test".to_string(),
                processor_type: Some("psp".to_string()),
                extensions: ProtocolExtensions::default(),
            },
        }
    }

    #[cfg(feature = "ap2")]
    #[must_use]
    pub fn ap2_defaults() -> Self {
        Self {
            app_name: "payments-app".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            merchant_of_record: MerchantRef {
                merchant_id: "merchant-ap2".to_string(),
                legal_name: "AP2 Merchant LLC".to_string(),
                display_name: Some("AP2 Merchant".to_string()),
                statement_descriptor: Some("AP2*MERCHANT".to_string()),
                country_code: Some("US".to_string()),
                website: Some("https://ap2-merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            payment_processor_ref: PaymentProcessorRef {
                processor_id: "ap2-processor".to_string(),
                name: "AP2 Test PSP".to_string(),
                processor_type: Some("psp".to_string()),
                extensions: ProtocolExtensions::default(),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct MultiActorHarnessActors {
    pub shopper: CommerceActor,
    #[cfg(feature = "ap2")]
    pub merchant: CommerceActor,
    #[cfg(feature = "ap2")]
    pub payment_processor: CommerceActor,
    #[cfg(feature = "acp")]
    pub webhook: CommerceActor,
    pub merchant_of_record: MerchantRef,
    #[cfg(feature = "ap2")]
    pub payment_processor_ref: PaymentProcessorRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessActorKind {
    Shopper,
    Merchant,
    CredentialsProvider,
    PaymentProcessor,
    Webhook,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessAction {
    pub actor: HarnessActorKind,
    pub transaction_id: String,
    pub action: String,
}

pub struct MultiActorHarness {
    pub identity: AdkIdentity,
    pub session_service: Arc<InMemorySessionService>,
    pub memory_service: Arc<InMemoryMemoryService>,
    pub transaction_store: Arc<dyn TransactionStore>,
    pub evidence_store: Arc<dyn EvidenceStore>,
    pub backend: Arc<MockCommerceKernel>,
    pub actors: MultiActorHarnessActors,
}

impl MultiActorHarness {
    pub async fn new(config: MultiActorHarnessConfig) -> Self {
        let session_service = Arc::new(InMemorySessionService::new());
        let identity = AdkIdentity::new(
            AppName::try_from(config.app_name.as_str()).unwrap(),
            UserId::try_from(config.user_id.as_str()).unwrap(),
            SessionId::try_from(config.session_id.as_str()).unwrap(),
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

        let memory_service = Arc::new(InMemoryMemoryService::new());
        let artifact_service = Arc::new(InMemoryArtifactService::new());
        let transaction_store: Arc<dyn TransactionStore> = Arc::new(
            SessionBackedTransactionStore::new(session_service.clone())
                .with_memory_index(PaymentMemoryIndex::new(memory_service.clone())),
        );
        let evidence_store: Arc<dyn EvidenceStore> =
            Arc::new(ArtifactBackedEvidenceStore::new(artifact_service));

        let actors = MultiActorHarnessActors {
            shopper: CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: config.tenant_id.clone(),
                extensions: ProtocolExtensions::default(),
            },
            #[cfg(feature = "ap2")]
            merchant: CommerceActor {
                actor_id: config.merchant_of_record.merchant_id.clone(),
                role: CommerceActorRole::Merchant,
                display_name: config
                    .merchant_of_record
                    .display_name
                    .clone()
                    .or_else(|| Some(config.merchant_of_record.legal_name.clone())),
                tenant_id: config.tenant_id.clone(),
                extensions: ProtocolExtensions::default(),
            },
            #[cfg(feature = "ap2")]
            payment_processor: CommerceActor {
                actor_id: config.payment_processor_ref.processor_id.clone(),
                role: CommerceActorRole::PaymentProcessor,
                display_name: Some(config.payment_processor_ref.name.clone()),
                tenant_id: config.tenant_id.clone(),
                extensions: ProtocolExtensions::default(),
            },
            #[cfg(feature = "acp")]
            webhook: CommerceActor {
                actor_id: "merchant-webhook".to_string(),
                role: CommerceActorRole::Custom("webhook".to_string()),
                display_name: Some("merchant-webhook".to_string()),
                tenant_id: config.tenant_id,
                extensions: ProtocolExtensions::default(),
            },
            merchant_of_record: config.merchant_of_record,
            #[cfg(feature = "ap2")]
            payment_processor_ref: config.payment_processor_ref,
        };

        let backend =
            Arc::new(MockCommerceKernel::new(transaction_store.clone(), evidence_store.clone()));

        Self {
            identity,
            session_service,
            memory_service,
            transaction_store,
            evidence_store,
            backend,
            actors,
        }
    }

    fn context_for(
        &self,
        transaction_id: &str,
        actor: CommerceActor,
        payment_processor: Option<PaymentProcessorRef>,
        mode: CommerceMode,
        protocol: ProtocolDescriptor,
    ) -> CommerceContext {
        CommerceContext {
            transaction_id: TransactionId::from(transaction_id),
            session_identity: Some(self.identity.clone()),
            actor,
            merchant_of_record: self.actors.merchant_of_record.clone(),
            payment_processor,
            mode,
            protocol,
            extensions: ProtocolExtensions::default(),
        }
    }

    #[cfg(feature = "ap2")]
    pub fn shopper_context(
        &self,
        transaction_id: &str,
        mode: CommerceMode,
        protocol: ProtocolDescriptor,
    ) -> CommerceContext {
        self.context_for(transaction_id, self.actors.shopper.clone(), None, mode, protocol)
    }

    #[cfg(feature = "ap2")]
    pub fn merchant_context(
        &self,
        transaction_id: &str,
        mode: CommerceMode,
        protocol: ProtocolDescriptor,
    ) -> CommerceContext {
        self.context_for(transaction_id, self.actors.merchant.clone(), None, mode, protocol)
    }

    #[cfg(feature = "ap2")]
    pub fn payment_processor_context(
        &self,
        transaction_id: &str,
        mode: CommerceMode,
        protocol: ProtocolDescriptor,
    ) -> CommerceContext {
        self.context_for(
            transaction_id,
            self.actors.payment_processor.clone(),
            Some(self.actors.payment_processor_ref.clone()),
            mode,
            protocol,
        )
    }

    #[cfg(feature = "acp")]
    pub fn webhook_context(
        &self,
        transaction_id: &str,
        mode: CommerceMode,
        protocol: ProtocolDescriptor,
    ) -> CommerceContext {
        self.context_for(transaction_id, self.actors.webhook.clone(), None, mode, protocol)
    }

    #[cfg(feature = "acp")]
    #[must_use]
    pub fn acp_context_template(&self, mode: CommerceMode) -> AcpContextTemplate {
        AcpContextTemplate {
            session_identity: Some(self.identity.clone()),
            actor: self.actors.shopper.clone(),
            merchant_of_record: self.actors.merchant_of_record.clone(),
            payment_processor: None,
            mode,
        }
    }

    pub async fn transaction(&self, transaction_id: &str) -> TransactionRecord {
        self.transaction_store
            .get(TransactionLookup {
                transaction_id: TransactionId::from(transaction_id),
                session_identity: Some(self.identity.clone()),
            })
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn load_evidence(&self, evidence_ref: &EvidenceReference) -> StoredEvidence {
        self.evidence_store
            .load(EvidenceLookup {
                evidence_ref: evidence_ref.clone(),
                session_identity: Some(self.identity.clone()),
            })
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn session_state_dump(&self) -> String {
        let session = self.session_service.get_for_identity(&self.identity).await.unwrap();
        serde_json::to_string(&session.state().all()).unwrap()
    }

    #[cfg(feature = "acp")]
    pub async fn session_events_dump(&self) -> String {
        let session = self.session_service.get_for_identity(&self.identity).await.unwrap();
        serde_json::to_string(&session.events().all()).unwrap()
    }

    pub async fn memory_text(&self, query: &str) -> String {
        let result = self
            .memory_service
            .search(SearchRequest {
                query: query.to_string(),
                user_id: self.identity.user_id.as_ref().to_string(),
                app_name: self.identity.app_name.as_ref().to_string(),
                limit: Some(10),
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap();
        result
            .memories
            .iter()
            .flat_map(|entry| entry.content.parts.iter())
            .filter_map(adk_core::Part::text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(feature = "ap2")]
    pub async fn issue_credentials_provider_artifact(
        &self,
        transaction_id: &str,
        artifact_kind: &str,
        value: &str,
    ) -> String {
        self.backend
            .record_external_action(
                HarnessActorKind::CredentialsProvider,
                transaction_id,
                &format!("issue_{artifact_kind}"),
            )
            .await;
        value.to_string()
    }

    pub async fn recorded_actions(&self) -> Vec<HarnessAction> {
        self.backend.actions().await
    }
}

pub struct MockCommerceKernel {
    transaction_store: Arc<dyn TransactionStore>,
    evidence_store: Arc<dyn EvidenceStore>,
    now: RwLock<i64>,
    actions: RwLock<Vec<HarnessAction>>,
}

impl MockCommerceKernel {
    #[must_use]
    pub fn new(
        transaction_store: Arc<dyn TransactionStore>,
        evidence_store: Arc<dyn EvidenceStore>,
    ) -> Self {
        Self {
            transaction_store,
            evidence_store,
            now: RwLock::new(0),
            actions: RwLock::new(Vec::new()),
        }
    }

    async fn timestamp(&self) -> chrono::DateTime<Utc> {
        let mut now = self.now.write().await;
        *now += 1;
        Utc::now() + Duration::seconds(*now)
    }

    async fn load_transaction(&self, lookup: TransactionLookup) -> Result<TransactionRecord> {
        self.transaction_store.get(lookup).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "payments.harness.transaction_not_found",
                "test harness transaction was not found",
            )
        })
    }

    async fn record_action_from_context(&self, context: &CommerceContext, action: &str) {
        self.record_external_action(
            classify_actor(&context.actor),
            context.transaction_id.as_str(),
            action,
        )
        .await;
    }

    pub async fn record_external_action(
        &self,
        actor: HarnessActorKind,
        transaction_id: &str,
        action: &str,
    ) {
        self.actions.write().await.push(HarnessAction {
            actor,
            transaction_id: transaction_id.to_string(),
            action: action.to_string(),
        });
    }

    pub async fn actions(&self) -> Vec<HarnessAction> {
        self.actions.read().await.clone()
    }

    fn enrich_acp_checkout(
        record: &mut TransactionRecord,
        shipping_minor: i64,
        selected_option: &str,
    ) {
        for line in &mut record.cart.lines {
            let unit = 300_i64;
            let total = unit * i64::from(line.quantity);
            line.unit_price = Money::new("usd", unit, 2);
            line.total_price = Money::new("usd", total, 2);
        }

        let subtotal: i64 =
            record.cart.lines.iter().map(|line| line.total_price.amount_minor).sum();
        let tax_minor = 30_i64;
        record.cart.subtotal = Some(Money::new("usd", subtotal, 2));
        record.cart.adjustments = vec![
            adk_payments::domain::PriceAdjustment {
                adjustment_id: "tax".to_string(),
                kind: adk_payments::domain::PriceAdjustmentKind::Tax,
                label: "Tax".to_string(),
                amount: Money::new("usd", tax_minor, 2),
                extensions: ProtocolExtensions::default(),
            },
            adk_payments::domain::PriceAdjustment {
                adjustment_id: "fulfillment".to_string(),
                kind: adk_payments::domain::PriceAdjustmentKind::Shipping,
                label: "Fulfillment".to_string(),
                amount: Money::new("usd", shipping_minor, 2),
                extensions: ProtocolExtensions::default(),
            },
        ];
        record.cart.total = Money::new("usd", subtotal + tax_minor + shipping_minor, 2);

        let envelope = ProtocolExtensionEnvelope::new(ProtocolDescriptor::acp(ACP_STABLE_BASELINE))
            .with_field(
                "capabilities",
                json!({
                    "payment": {
                        "handlers": [
                            {
                                "id": "card_tokenized",
                                "name": "dev.acp.tokenized.card",
                                "version": "2026-01-30",
                                "spec": "https://merchant.example/specs/tokenized-card",
                                "requires_delegate_payment": true,
                                "requires_pci_compliance": false,
                                "psp": "stripe",
                                "config_schema": "https://merchant.example/schemas/config.json",
                                "instrument_schemas": [
                                    "https://merchant.example/schemas/instrument.json"
                                ],
                                "config": {
                                    "merchant_id": record.merchant_of_record.merchant_id,
                                    "psp": "stripe",
                                    "accepted_brands": ["visa", "mastercard"]
                                }
                            }
                        ]
                    },
                    "interventions": {
                        "supported": ["3ds"],
                        "required": [],
                        "enforcement": "conditional"
                    }
                }),
            )
            .with_field(
                "fulfillment_options",
                json!([
                    {
                        "type": "shipping",
                        "id": "fulfillment_option_123",
                        "title": "Standard",
                        "description": "Arrives in 4-5 days",
                        "totals": [{"type": "total", "display_text": "Shipping", "amount": 100}]
                    },
                    {
                        "type": "shipping",
                        "id": "fulfillment_option_456",
                        "title": "Express",
                        "description": "Arrives in 1-2 days",
                        "totals": [{"type": "total", "display_text": "Express Shipping", "amount": 500}]
                    }
                ]),
            )
            .with_field(
                "selected_fulfillment_options",
                json!([
                    {
                        "type": "shipping",
                        "option_id": selected_option,
                        "item_ids": record
                            .cart
                            .lines
                            .iter()
                            .map(|line| line.merchant_sku.clone().unwrap_or_else(|| line.line_id.clone()))
                            .collect::<Vec<_>>()
                    }
                ]),
            )
            .with_field(
                "links",
                json!([
                    {"type": "terms_of_use", "url": "https://merchant.example/legal/terms"},
                    {"type": "return_policy", "url": "https://merchant.example/legal/returns"}
                ]),
            );

        record.attach_extension(envelope);
    }

    async fn store_json_evidence(
        &self,
        context: &CommerceContext,
        artifact_kind: &str,
        payload: &Value,
    ) -> Result<EvidenceReference> {
        let stored = self
            .evidence_store
            .store(StoreEvidenceCommand {
                transaction_id: context.transaction_id.clone(),
                session_identity: context.session_identity.clone(),
                evidence_ref: EvidenceReference {
                    evidence_id: format!("{artifact_kind}:{}", context.transaction_id.as_str()),
                    protocol: context.protocol.clone(),
                    artifact_kind: artifact_kind.to_string(),
                    digest: None,
                },
                body: serde_json::to_vec(payload).unwrap(),
                content_type: "application/json".to_string(),
            })
            .await?;
        Ok(stored.evidence_ref)
    }

    fn request_payload(extensions: &ProtocolExtensions) -> Option<Value> {
        extensions
            .as_slice()
            .iter()
            .rev()
            .find(|envelope| envelope.protocol.name == "acp")
            .and_then(|envelope| envelope.fields.get("request").cloned())
    }
}

#[async_trait]
impl MerchantCheckoutService for MockCommerceKernel {
    async fn create_checkout(
        &self,
        command: adk_payments::kernel::CreateCheckoutCommand,
    ) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "create_checkout").await;

        let now = self.timestamp().await;
        let mut record = TransactionRecord::new(
            command.context.transaction_id.clone(),
            command.context.actor.clone(),
            command.context.merchant_of_record.clone(),
            command.context.mode,
            command.cart.clone(),
            now,
        );
        record.session_identity = command.context.session_identity.clone();
        record.payment_processor = command.context.payment_processor.clone();
        record.fulfillment = command.fulfillment.clone();

        if command.context.protocol.name == "acp" {
            record.protocol_refs.acp_checkout_session_id =
                Some(command.context.transaction_id.as_str().to_string());
        }

        record
            .transition_to(TransactionState::Negotiating, self.timestamp().await)
            .map_err(AdkError::from)?;
        record
            .transition_to(TransactionState::AwaitingPaymentMethod, self.timestamp().await)
            .map_err(AdkError::from)?;

        if command.context.protocol.name == "acp" {
            Self::enrich_acp_checkout(&mut record, 100, "fulfillment_option_123");
        }

        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn update_checkout(&self, command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "update_checkout").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;

        if let Some(cart) = command.cart {
            record.cart = cart;
        }
        if let Some(fulfillment) = command.fulfillment.clone() {
            if command.context.protocol.name == "acp" {
                let shipping_minor =
                    if fulfillment.fulfillment_id == "fulfillment_option_456" { 500 } else { 100 };
                Self::enrich_acp_checkout(
                    &mut record,
                    shipping_minor,
                    if shipping_minor == 500 {
                        "fulfillment_option_456"
                    } else {
                        "fulfillment_option_123"
                    },
                );
            }
            record.fulfillment = Some(fulfillment);
        }
        record.last_updated_at = self.timestamp().await;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.transaction_store.get(lookup).await
    }

    async fn complete_checkout(
        &self,
        command: adk_payments::kernel::CompleteCheckoutCommand,
    ) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "complete_checkout").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        if let Some(payload) = Self::request_payload(&command.extensions) {
            let evidence_ref =
                self.store_json_evidence(&command.context, "complete_request", &payload).await?;
            record.attach_evidence_ref(evidence_ref.clone());
            record.attach_evidence_digest(ProtocolEnvelopeDigest::new(
                evidence_ref,
                self.timestamp().await,
            ));
        }
        record
            .transition_to(TransactionState::Authorized, self.timestamp().await)
            .map_err(AdkError::from)?;
        record
            .transition_to(TransactionState::Completed, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.protocol_refs.acp_order_id = Some("ord_abc123".to_string());
        record.order = Some(OrderSnapshot {
            order_id: Some("ord_abc123".to_string()),
            receipt_id: Some("receipt_123".to_string()),
            state: OrderState::Completed,
            receipt_state: ReceiptState::Settled,
            extensions: ProtocolExtensions::default(),
        });
        Self::enrich_acp_checkout(&mut record, 500, "fulfillment_option_456");
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn cancel_checkout(&self, command: CancelCheckoutCommand) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "cancel_checkout").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        record
            .transition_to(TransactionState::Canceled, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn apply_order_update(&self, command: OrderUpdateCommand) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "apply_order_update").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        if command.context.protocol.name == "acp" {
            record.protocol_refs.acp_order_id = command.order.order_id.clone();
        }
        record.order = Some(command.order);
        record.last_updated_at = self.timestamp().await;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }
}

#[async_trait]
impl adk_payments::kernel::DelegatedPaymentService for MockCommerceKernel {
    async fn delegate_payment(
        &self,
        command: DelegatePaymentCommand,
    ) -> Result<DelegatedPaymentResult> {
        self.record_action_from_context(&command.context, "delegate_payment").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        if let Some(payload) = Self::request_payload(&command.extensions) {
            let evidence_ref = self
                .store_json_evidence(&command.context, "delegate_payment_request", &payload)
                .await?;
            record.attach_evidence_ref(evidence_ref.clone());
            record.attach_evidence_digest(ProtocolEnvelopeDigest::new(
                evidence_ref.clone(),
                self.timestamp().await,
            ));
        }
        record.protocol_refs.acp_delegate_payment_id = Some("vt_01J8Z3WXYZ9ABC".to_string());
        record.last_updated_at = self.timestamp().await;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;

        Ok(DelegatedPaymentResult {
            delegated_payment_id: "vt_01J8Z3WXYZ9ABC".to_string(),
            created_at: self.timestamp().await,
            transaction: Some(record),
            generated_evidence_refs: Vec::new(),
            metadata: BTreeMap::from([
                ("source".to_string(), "agent_checkout".to_string()),
                ("merchant_id".to_string(), command.context.merchant_of_record.merchant_id.clone()),
            ]),
            extensions: ProtocolExtensions::default(),
        })
    }
}

#[async_trait]
impl PaymentExecutionService for MockCommerceKernel {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> Result<PaymentExecutionResult> {
        self.record_action_from_context(&command.context, "execute_payment").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        record.payment_processor = command.context.payment_processor.clone();
        if record.state.can_transition_to(&TransactionState::Authorized) {
            record
                .transition_to(TransactionState::Authorized, self.timestamp().await)
                .map_err(AdkError::from)?;
        }
        record.order = Some(OrderSnapshot {
            order_id: Some(format!("order-{}", command.context.transaction_id.as_str())),
            receipt_id: None,
            state: OrderState::Authorized,
            receipt_state: ReceiptState::Pending,
            extensions: ProtocolExtensions::default(),
        });
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(PaymentExecutionResult {
            outcome: PaymentExecutionOutcome::Completed,
            transaction: record.clone(),
            order: record.order.clone(),
            intervention: None,
            generated_evidence_refs: Vec::new(),
        })
    }

    async fn sync_payment_outcome(
        &self,
        command: SyncPaymentOutcomeCommand,
    ) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "sync_payment_outcome").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        record.payment_processor = command.context.payment_processor.clone();
        if let Some(order) = command.order {
            record.order = Some(order);
        }
        match command.outcome {
            PaymentExecutionOutcome::Authorized
                if record.state.can_transition_to(&TransactionState::Authorized) =>
            {
                record
                    .transition_to(TransactionState::Authorized, self.timestamp().await)
                    .map_err(AdkError::from)?;
            }
            PaymentExecutionOutcome::Completed
                if record.state.can_transition_to(&TransactionState::Completed) =>
            {
                record
                    .transition_to(TransactionState::Completed, self.timestamp().await)
                    .map_err(AdkError::from)?;
            }
            PaymentExecutionOutcome::Failed
                if record.state.can_transition_to(&TransactionState::Failed) =>
            {
                record
                    .transition_to(TransactionState::Failed, self.timestamp().await)
                    .map_err(AdkError::from)?;
            }
            PaymentExecutionOutcome::InterventionRequired => {
                if let Some(intervention) = command.intervention
                    && record.state.can_transition_to(&TransactionState::InterventionRequired(
                        Box::new(intervention.clone()),
                    ))
                {
                    record
                        .transition_to(
                            TransactionState::InterventionRequired(Box::new(intervention)),
                            self.timestamp().await,
                        )
                        .map_err(AdkError::from)?;
                }
            }
            _ => {}
        }
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }
}

#[async_trait]
impl InterventionService for MockCommerceKernel {
    async fn begin_intervention(
        &self,
        command: BeginInterventionCommand,
    ) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "begin_intervention").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        record
            .transition_to(
                TransactionState::InterventionRequired(Box::new(command.intervention)),
                self.timestamp().await,
            )
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }

    async fn continue_intervention(
        &self,
        command: ContinueInterventionCommand,
    ) -> Result<TransactionRecord> {
        self.record_action_from_context(&command.context, "continue_intervention").await;

        let mut record = self
            .load_transaction(TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            })
            .await?;
        record
            .transition_to(TransactionState::AwaitingPaymentMethod, self.timestamp().await)
            .map_err(AdkError::from)?;
        record.recompute_safe_summary();
        self.transaction_store.upsert(record.clone()).await?;
        Ok(record)
    }
}

#[async_trait]
impl TransactionStore for MockCommerceKernel {
    async fn upsert(&self, record: TransactionRecord) -> Result<()> {
        self.transaction_store.upsert(record).await
    }

    async fn get(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.transaction_store.get(lookup).await
    }

    async fn list_unresolved(
        &self,
        request: ListUnresolvedTransactionsRequest,
    ) -> Result<Vec<TransactionRecord>> {
        self.transaction_store.list_unresolved(request).await
    }
}

#[async_trait]
impl EvidenceStore for MockCommerceKernel {
    async fn store(&self, command: StoreEvidenceCommand) -> Result<StoredEvidence> {
        self.evidence_store.store(command).await
    }

    async fn load(&self, lookup: EvidenceLookup) -> Result<Option<StoredEvidence>> {
        self.evidence_store.load(lookup).await
    }
}

fn classify_actor(actor: &CommerceActor) -> HarnessActorKind {
    match &actor.role {
        CommerceActorRole::Shopper | CommerceActorRole::AgentSurface => HarnessActorKind::Shopper,
        CommerceActorRole::Merchant => HarnessActorKind::Merchant,
        CommerceActorRole::CredentialsProvider => HarnessActorKind::CredentialsProvider,
        CommerceActorRole::PaymentProcessor => HarnessActorKind::PaymentProcessor,
        CommerceActorRole::Custom(value) if value.contains("webhook") => HarnessActorKind::Webhook,
        CommerceActorRole::System => HarnessActorKind::Webhook,
        CommerceActorRole::Custom(value) => HarnessActorKind::Unknown(value.clone()),
    }
}
