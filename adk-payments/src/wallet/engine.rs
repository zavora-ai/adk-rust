//! The wallet: payments under the owner's mandates, caps checked before execution
//! (spec 3.5 §7.14, D-10, A-I16).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use adk_core::principal::{AgentId, SubjectKind, SubjectRef};
use adk_core::{
    AdkError, ApprovalAction, ApprovalDecision, Clock, ErrorCategory, ErrorComponent,
    InMemorySpendLedger, ReadonlyContext, ReservationId, Result, SPEND_LIMIT_EXCEEDED_CODE,
    SpendKey, SpendLedger, SystemClock,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::anomaly::{WalletAlarm, WalletAlarmHook, WalletAlarmKind, judge};
use super::error::{
    WALLET_CATEGORY_PAUSED_CODE, WALLET_CURRENCY_MISMATCH_CODE, WALLET_INVALID_AMOUNT_CODE,
    WALLET_INVALID_POLICY_CODE, WALLET_KEY_REUSED_CODE, WALLET_NO_MANDATE_CODE,
    WALLET_NOT_HELD_CODE, WALLET_PAYEE_OUTSIDE_MANDATE_CODE, WALLET_PAYMENT_FAILED_CODE,
    WALLET_PAYMENT_IN_PROGRESS_CODE, WALLET_PAYMENT_NOT_FOUND_CODE, WALLET_PAYMENT_REJECTED_CODE,
    WALLET_RAIL_UNAVAILABLE_CODE, WALLET_UNSUPPORTED_DECISION_CODE, conflict, invalid, not_found,
    owner_required, refused,
};
use super::policy::{
    Mandate, Payee, PayeeRule, RailRef, SpendCategory, WalletPolicy, category_name, category_scope,
    owner_approved_scope, previous_window,
};
use super::provider::{DebitRequest, WalletProvider};
use super::rail::{PaymentRail, RailFunding, RailPayment};
use super::reconcile::{ReconciliationReport, Statements, compare};
use super::store::{
    CategoryPause, HoldReason, InMemoryWalletStore, OwnerDecisionRecord, PaymentQuery,
    PaymentRecord, PaymentState, WalletStore,
};
use super::use_based::PaymentChallenge;

/// A payment the site asks the wallet to make (spec 3.5 §7.14).
///
/// `key` makes it exactly once: a repeated key returns the first payment's outcome, and a
/// repeated key with another payment is refused. Amounts are minor units.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Payee, PaymentRequest, SpendCategory};
///
/// let renewal = PaymentRequest::new("renew-2026", SpendCategory::Domains, Payee::new("Registrar"), 1_500, "USD")
///     .with_description("example.com renewal");
/// assert_eq!(renewal.amount, 1_500);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequest {
    /// The idempotency key.
    pub key: String,
    /// The category it is spent in.
    pub category: SpendCategory,
    /// Who receives it.
    pub payee: Payee,
    /// Amount in minor units of `currency`.
    pub amount: u64,
    /// ISO 4217 currency code; the wallet's.
    pub currency: String,
    /// What it is for.
    pub description: String,
    /// The rail's data: the AP2 cart mandate, the use-based challenge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail_data: Option<Value>,
}

impl PaymentRequest {
    /// A payment of `amount` minor units of `currency` to `payee` in `category`.
    pub fn new(
        key: impl Into<String>,
        category: SpendCategory,
        payee: Payee,
        amount: u64,
        currency: &str,
    ) -> Self {
        Self {
            key: key.into(),
            category,
            payee,
            amount,
            currency: currency.to_ascii_uppercase(),
            description: String::new(),
            rail_data: None,
        }
    }

    /// Sets what it is for.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the rail's data.
    #[must_use]
    pub fn with_rail_data(mut self, data: Value) -> Self {
        self.rail_data = Some(data);
        self
    }

    /// Digest of everything but the key.
    fn digest(&self) -> String {
        adk_core::json_digest(&json!({
            "category": self.category,
            "payee": self.payee,
            "amount": self.amount,
            "currency": self.currency.to_ascii_uppercase(),
            "description": self.description,
            "railData": self.rail_data,
        }))
    }
}

/// What the wallet did with a payment it accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum PaymentOutcome {
    /// Paid within the mandate.
    Settled(PaymentRecord),
    /// Held for the owner: over the per-payment limit or the category cap.
    Held(PaymentRecord),
}

impl PaymentOutcome {
    /// The payment.
    pub fn record(&self) -> &PaymentRecord {
        match self {
            Self::Settled(record) | Self::Held(record) => record,
        }
    }

    /// Whether it settled.
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Settled(_))
    }

    /// Whether it is held for the owner.
    pub fn is_held(&self) -> bool {
        matches!(self, Self::Held(_))
    }
}

/// Who asks the wallet to pay: the agent, the run, the subject, the charter version
/// (spec 3.5 §9.3).
///
/// # Example
///
/// ```
/// use adk_payments::wallet::WalletCaller;
///
/// let finance = WalletCaller::agent("urn:adk:agent:site/finance").with_charter_version(3);
/// assert_eq!(finance.agent.as_deref(), Some("urn:adk:agent:site/finance"));
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WalletCaller {
    /// The paying agent, attributed in the spend ledger.
    pub agent: Option<String>,
    /// The actor audit records name.
    pub actor: AuditActor,
    /// The charter version the payment runs under.
    pub charter_version: Option<u32>,
}

impl WalletCaller {
    /// A caller that is the agent `id`.
    pub fn agent(id: impl Into<String>) -> Self {
        let id = id.into();
        let actor = AuditActor { agent: AgentId::new(id.clone()).ok(), ..AuditActor::default() };
        Self { agent: Some(id), actor, charter_version: None }
    }

    /// The agent running in `ctx`, its run and the subject it acts for.
    pub fn from_context(ctx: &dyn ReadonlyContext) -> Self {
        Self {
            agent: Some(ctx.acting_context().agent.id.to_string()),
            actor: AuditActor::for_context(ctx),
            charter_version: None,
        }
    }

    /// Records the charter version the payment runs under.
    #[must_use]
    pub fn with_charter_version(mut self, version: u32) -> Self {
        self.charter_version = Some(version);
        self
    }
}

/// Where a category stands this period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryStatus {
    /// The category.
    pub category: SpendCategory,
    /// The mandate's cap per period.
    pub cap_per_period: u64,
    /// The mandate's limit per payment.
    pub max_per_payment: u64,
    /// What the category spent this period, owner-approved payments included.
    pub spent: u64,
    /// What the site may still spend this period without the owner.
    pub remaining: u64,
    /// The rail payments go through.
    pub rail: RailRef,
    /// The payees the site may pay.
    pub payees: Vec<PayeeRule>,
    /// Why the category is paused, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<String>,
}

/// Where a decision on a held payment came from.
enum DecisionSource {
    /// An owner through [`Wallet::decide`].
    Owner,
    /// The run's approval sources, through the governed path.
    GovernedPath,
}

impl DecisionSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::GovernedPath => "governed_path",
        }
    }
}

struct Inner {
    id: String,
    policy: WalletPolicy,
    provider: Arc<dyn WalletProvider>,
    rails: BTreeMap<RailRef, Arc<dyn PaymentRail>>,
    ledger: Arc<dyn SpendLedger>,
    store: Arc<dyn WalletStore>,
    alarms: Option<Arc<dyn WalletAlarmHook>>,
    audit: Option<AuditTrail>,
    owners: Vec<SubjectRef>,
    clock: Arc<dyn Clock>,
    /// Serialises the cap check with the reservation; holds each category's autonomous
    /// payments in flight, in ledger units.
    gate: Mutex<BTreeMap<SpendCategory, u64>>,
}

/// An owner-funded wallet the site pays from under mandates (spec 3.5 §7.14, D-10).
///
/// Every payment goes through [`pay`](Self::pay), which checks before any money moves:
///
/// | Check | Fails as |
/// |-------|----------|
/// | the category has a mandate | refused, `wallet.no_mandate` |
/// | the wallet's currency, an amount above zero | refused |
/// | a payee rule allows the payee | refused, `wallet.payee_outside_mandate` |
/// | the mandate's rail is held and its data describes the payment | refused |
/// | the category is not paused | refused, `wallet.category_paused` |
/// | the amount is within `max_per_payment` | [held](PaymentOutcome::Held) |
/// | the category stays within `cap_per_period`, by the spend ledger | [held](PaymentOutcome::Held) |
/// | the anomaly rule | the category is paused, a P1 alarm raised, the payment refused |
///
/// Then it reserves in the spend ledger, debits the provider for a wallet-funded rail,
/// pays through the rail and commits the reservation; a failed rail voids the debit and
/// releases the reservation. Refusals carry `payment_policy_rejected` for the AWP edge.
///
/// The site cannot open accounts, add payment methods, change payees outside the mandate
/// or raise a cap (A-I16): the policy is read-only once built, neither
/// [`WalletProvider`] nor [`PaymentRail`] has such a method, and the only acts beyond the
/// mandate — approving a held payment, resuming a paused category — take a person's
/// [`ApprovalDecision`].
///
/// ```compile_fail
/// # fn raise(wallet: &adk_payments::wallet::Wallet) {
/// // The policy is shared read-only; there is no way to raise a cap through the wallet.
/// wallet.policy().categories.clear();
/// # }
/// ```
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_payments::wallet::{
///     InMemoryUseBasedScheme, InMemoryWalletProvider, Mandate, PayeeRule, PaymentChallenge,
///     RailRef, SpendCategory, UseBasedRail, Wallet, WalletCaller, WalletPolicy,
/// };
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let policy = WalletPolicy::new("USD").with_mandate(
///     SpendCategory::Models,
///     Mandate::new(30_000, 500, RailRef::use_based())
///         .with_payee(PayeeRule::Domain("models.example".into())),
/// );
/// let wallet = Wallet::builder("site-1", policy, Arc::new(InMemoryWalletProvider::stored_value("treasury", "USD", 50_000)))
///     .rail(Arc::new(UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()))))
///     .build()?;
///
/// let challenge = PaymentChallenge::new("https://api.models.example/v1/generate", "acct-models", 12, "USD");
/// let outcome = wallet
///     .pay_challenge("call-1", SpendCategory::Models, &challenge, &WalletCaller::agent("urn:adk:agent:site/growth"))
///     .await?;
/// assert!(outcome.is_settled());
/// assert!(outcome.record().use_based_proof().is_some());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Wallet {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wallet")
            .field("id", &self.inner.id)
            .field("provider", &self.inner.provider.name())
            .field("rails", &self.inner.rails.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Builds a [`Wallet`]: the owner's act, with the owner's policy, provider and rails.
pub struct WalletBuilder {
    id: String,
    policy: WalletPolicy,
    provider: Arc<dyn WalletProvider>,
    rails: Vec<Arc<dyn PaymentRail>>,
    ledger: Option<Arc<dyn SpendLedger>>,
    store: Option<Arc<dyn WalletStore>>,
    alarms: Option<Arc<dyn WalletAlarmHook>>,
    audit: Option<AuditTrail>,
    owners: Vec<SubjectRef>,
    clock: Arc<dyn Clock>,
}

impl WalletBuilder {
    /// Adds a rail adapter.
    #[must_use]
    pub fn rail(mut self, rail: Arc<dyn PaymentRail>) -> Self {
        self.rails.push(rail);
        self
    }

    /// Reserves and records spend in `ledger`. Give it the policy's
    /// [`spend_limits`](WalletPolicy::spend_limits) so it refuses an over-cap reservation
    /// atomically across processes. Without one, the wallet uses an
    /// [`InMemorySpendLedger`] with those limits.
    #[must_use]
    pub fn spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    /// Keeps the journal in `store`; an [`InMemoryWalletStore`] by default.
    #[must_use]
    pub fn store(mut self, store: Arc<dyn WalletStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Raises alarms through `hook`.
    #[must_use]
    pub fn alarm_hook(mut self, hook: Arc<dyn WalletAlarmHook>) -> Self {
        self.alarms = Some(hook);
        self
    }

    /// Records `wallet.payment`, `wallet.held`, `wallet.anomaly` and `wallet.reconciled`
    /// in `trail` (spec 3.5 §9.3). In [`AuditFailureMode::Block`](adk_core::audit_log::AuditFailureMode::Block)
    /// a payment whose record cannot be written does not run.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Names a person who may decide held payments and resume categories. Without any,
    /// every person's decision is accepted; an agent's or a service's never is.
    #[must_use]
    pub fn owner(mut self, owner: SubjectRef) -> Self {
        self.owners.push(owner);
        self
    }

    /// Sets the clock for periods, days and timestamps.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Builds the wallet.
    ///
    /// # Errors
    ///
    /// Returns `wallet.invalid_policy` when the policy does not validate, the provider
    /// holds another currency, two rails share a name, or a mandate names a rail the wallet
    /// does not hold.
    pub fn build(self) -> Result<Wallet> {
        self.policy.validate()?;
        if !self.provider.currency().eq_ignore_ascii_case(&self.policy.currency) {
            return Err(invalid(
                WALLET_INVALID_POLICY_CODE,
                format!(
                    "the policy is in {} but {} holds {}",
                    self.policy.currency,
                    self.provider.name(),
                    self.provider.currency()
                ),
            ));
        }
        let mut rails = BTreeMap::new();
        for rail in self.rails {
            let name = rail.rail();
            if rails.insert(name.clone(), rail).is_some() {
                return Err(invalid(
                    WALLET_INVALID_POLICY_CODE,
                    format!("two rails are named {name}"),
                ));
            }
        }
        for (category, mandate) in &self.policy.categories {
            if !rails.contains_key(&mandate.rail) {
                return Err(invalid(
                    WALLET_INVALID_POLICY_CODE,
                    format!(
                        "the {} mandate pays through {}, which the wallet does not hold; add the rail",
                        category_name(*category),
                        mandate.rail
                    ),
                ));
            }
        }
        let ledger = match self.ledger {
            Some(ledger) => ledger,
            None => Arc::new(
                InMemorySpendLedger::new(self.policy.spend_limits(&self.id)?)
                    .with_clock(self.clock.clone()),
            ),
        };
        Ok(Wallet {
            inner: Arc::new(Inner {
                id: self.id,
                policy: self.policy,
                provider: self.provider,
                rails,
                ledger,
                store: self.store.unwrap_or_else(|| Arc::new(InMemoryWalletStore::new())),
                alarms: self.alarms,
                audit: self.audit,
                owners: self.owners,
                clock: self.clock,
                gate: Mutex::new(BTreeMap::new()),
            }),
        })
    }
}

/// How a reserved payment leaves the category's in-flight total.
enum Landing<'a> {
    /// Paid: the settled units are committed and the settled record is stored.
    Settled { record: &'a PaymentRecord, units: u64 },
    /// Not paid: the reservation is released.
    Released(SpendCategory),
}

/// A reservation the wallet took for a payment it is about to execute.
struct Reserved {
    reservation: ReservationId,
    units: u64,
    /// Counted in the category's autonomous in-flight total until settled.
    autonomous: bool,
}

impl Wallet {
    /// Starts a wallet `id` (the site's id) under `policy`, funded through `provider`.
    pub fn builder(
        id: impl Into<String>,
        policy: WalletPolicy,
        provider: Arc<dyn WalletProvider>,
    ) -> WalletBuilder {
        WalletBuilder {
            id: id.into(),
            policy,
            provider,
            rails: Vec::new(),
            ledger: None,
            store: None,
            alarms: None,
            audit: None,
            owners: Vec::new(),
            clock: Arc::new(SystemClock),
        }
    }

    /// The wallet's id.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The owner's policy, read-only.
    pub fn policy(&self) -> &WalletPolicy {
        &self.inner.policy
    }

    /// The funds the provider reports available, in minor units.
    ///
    /// # Errors
    ///
    /// Returns the provider's error.
    pub async fn balance(&self) -> Result<u64> {
        self.inner.provider.balance().await
    }

    /// Pays `request` for `caller` if the mandate allows it, holds it for the owner when it
    /// is over a limit or cap, and refuses it otherwise. See [`Wallet`] for the checks.
    ///
    /// # Errors
    ///
    /// Returns a refusal (`wallet.*`, category `Forbidden`, with `awpError` set to
    /// `payment_policy_rejected`), `wallet.key_reused` for a reused key, the provider's or
    /// rail's error when the payment failed, or `audit.record_failed` when a blocking audit
    /// trail cannot record it.
    pub async fn pay(
        &self,
        request: PaymentRequest,
        caller: &WalletCaller,
    ) -> Result<PaymentOutcome> {
        let inner = &self.inner;
        if request.key.trim().is_empty() {
            return Err(invalid(WALLET_INVALID_AMOUNT_CODE, "a payment needs an idempotency key"));
        }
        let digest = request.digest();
        if let Some(existing) = inner.store.by_key(&inner.id, &request.key).await? {
            return replay(existing, &digest);
        }
        let mandate = inner.policy.mandate(request.category).cloned();
        let rail_ref = mandate.as_ref().map_or_else(|| RailRef::new("none"), |m| m.rail.clone());
        let mut record = self.proposed(&request, &digest, rail_ref, caller);
        let Some(mandate) = mandate else {
            let error = refused(
                WALLET_NO_MANDATE_CODE,
                format!(
                    "the owner gave no mandate for {} spending, so the site cannot pay in it",
                    category_name(request.category)
                ),
            );
            return Err(self.refuse(&record, caller, error).await);
        };
        let (rail, units) = match self.validate(&request, &mandate, &record) {
            Ok(checked) => checked,
            Err(error) => return Err(self.refuse(&record, caller, error).await),
        };

        let mut gate = inner.gate.lock().await;
        if let Some(existing) = inner.store.by_key(&inner.id, &request.key).await? {
            return replay(existing, &digest);
        }
        if let Some(pause) = self.pause_of(request.category).await? {
            drop(gate);
            let error = refused(
                WALLET_CATEGORY_PAUSED_CODE,
                format!(
                    "{} spending is paused until the owner resumes it: {}",
                    category_name(request.category),
                    pause.reason
                ),
            );
            return Err(self.refuse(&record, caller, error).await);
        }
        if request.amount > mandate.max_per_payment {
            drop(gate);
            let reason = HoldReason::OverPaymentLimit {
                max_per_payment: mandate.max_per_payment,
                requested: request.amount,
            };
            return self.hold(record, reason, caller).await;
        }
        let in_flight = gate.get(&request.category).copied().unwrap_or(0);
        let spent_units =
            self.period_spend_units(request.category).await?.saturating_add(in_flight);
        let cap_units = inner.policy.to_ledger_units(mandate.cap_per_period).unwrap_or(u64::MAX);
        if spent_units.saturating_add(units) > cap_units {
            drop(gate);
            let reason = HoldReason::OverCategoryCap {
                cap: mandate.cap_per_period,
                spent: inner.policy.minor_from_ledger(spent_units),
                requested: request.amount,
            };
            return self.hold(record, reason, caller).await;
        }
        if let Some(anomaly) = self.anomaly(request.category, request.amount, in_flight).await? {
            drop(gate);
            let error =
                self.pause_for_anomaly(request.category, request.amount, anomaly, caller).await;
            return Err(self.refuse(&record, caller, error).await);
        }
        let key = SpendKey::org(category_scope(&inner.id, request.category))
            .with_vendor(request.payee.id.clone());
        let key = match &caller.agent {
            Some(agent) => key.with_agent(agent.clone()),
            None => key,
        };
        let reservation = match inner.ledger.reserve(&key, units).await {
            Ok(reservation) => reservation,
            Err(error) if error.code == SPEND_LIMIT_EXCEEDED_CODE => {
                drop(gate);
                let reason = HoldReason::OverCategoryCap {
                    cap: mandate.cap_per_period,
                    spent: inner.policy.minor_from_ledger(spent_units),
                    requested: request.amount,
                };
                return self.hold(record, reason, caller).await;
            }
            Err(error) => return Err(error),
        };
        record.state = PaymentState::Executing;
        let (stored, inserted) = inner.store.insert(record).await?;
        if !inserted {
            drop(gate);
            let _ = inner.ledger.release(reservation).await;
            return replay(stored, &digest);
        }
        *gate.entry(request.category).or_insert(0) += units;
        drop(gate);

        let reserved = Reserved { reservation, units, autonomous: true };
        if let Err(error) = self
            .audit("wallet.payment", &stored, caller, Some(AuditDecision::Allowed), None, &[])
            .await
        {
            self.abandon(stored, reserved, &error).await;
            return Err(error);
        }
        self.execute(stored, rail, reserved, caller).await.map(PaymentOutcome::Settled)
    }

    /// Pays a use-based `challenge` from a metered resource in `category`
    /// (spec 3.5 §7.14): the challenge's payee and price go through [`pay`](Self::pay), and a
    /// settled payment carries the proof to retry the request with
    /// ([`PaymentRecord::use_based_proof`]).
    ///
    /// # Errors
    ///
    /// As [`pay`](Self::pay).
    pub async fn pay_challenge(
        &self,
        key: impl Into<String>,
        category: SpendCategory,
        challenge: &PaymentChallenge,
        caller: &WalletCaller,
    ) -> Result<PaymentOutcome> {
        let description = challenge
            .description
            .clone()
            .unwrap_or_else(|| format!("metered use of {}", challenge.resource));
        let request = PaymentRequest::new(
            key,
            category,
            challenge.payee(),
            challenge.amount,
            &challenge.currency,
        )
        .with_description(description)
        .with_rail_data(serde_json::to_value(challenge).unwrap_or(Value::Null));
        self.pay(request, caller).await
    }

    /// Decides a held payment by a person's `decision` (spec 3.5 §7.14, A-I16): an
    /// approval pays it beyond the mandate, as the owner's act; a rejection closes it.
    ///
    /// An approved payment is recorded in the category's owner-approved scope
    /// ([`owner_approved_scope`]) and counts toward the category's spend, so the site's own
    /// remaining budget for the period shrinks by it; the cap is unchanged.
    ///
    /// # Errors
    ///
    /// Returns `wallet.owner_decision_required` when the decision is not a person's or the
    /// person is not an owner, `wallet.payment_not_found`, `wallet.not_held`, an
    /// `InvalidInput` error for an edit or response, or the rail's error when the approved
    /// payment fails.
    pub async fn decide(
        &self,
        payment_id: &str,
        decision: &ApprovalDecision,
    ) -> Result<PaymentRecord> {
        self.check_owner(&decision.approver, decision.via_delegation.as_ref())?;
        let approved = match decision.kind {
            ApprovalAction::Approve => true,
            ApprovalAction::Reject => false,
            ApprovalAction::Edit | ApprovalAction::Respond => {
                return Err(invalid(
                    WALLET_UNSUPPORTED_DECISION_CODE,
                    "a held payment is approved or rejected; to pay a different amount, reject it and ask for a new payment",
                ));
            }
        };
        let record = OwnerDecisionRecord {
            decided_by: decision.approver.clone(),
            approved,
            source: DecisionSource::Owner.as_str().to_string(),
            message: decision.reason.clone(),
            decided_at: decision.decided_at,
        };
        let caller = WalletCaller {
            agent: None,
            actor: AuditActor::subject(decision.approver.clone()),
            charter_version: None,
        };
        self.settle_held(payment_id, record, &caller).await
    }

    /// Decides a held payment from a confirmation the run's approval sources gave on the
    /// governed path: the approval store, a fingerprint approval or the confirmation
    /// handler, which only people feed.
    pub(crate) async fn decide_governed(
        &self,
        payment_id: &str,
        approved: bool,
        subject: SubjectRef,
        message: Option<String>,
        caller: &WalletCaller,
    ) -> Result<PaymentRecord> {
        let record = OwnerDecisionRecord {
            decided_by: subject,
            approved,
            source: DecisionSource::GovernedPath.as_str().to_string(),
            message,
            decided_at: self.inner.clock.now(),
        };
        self.settle_held(payment_id, record, caller).await
    }

    async fn settle_held(
        &self,
        payment_id: &str,
        decision: OwnerDecisionRecord,
        caller: &WalletCaller,
    ) -> Result<PaymentRecord> {
        let inner = &self.inner;
        let gate = inner.gate.lock().await;
        let mut record = inner.store.get(&inner.id, payment_id).await?.ok_or_else(|| {
            not_found(
                WALLET_PAYMENT_NOT_FOUND_CODE,
                format!("wallet {} has no payment {payment_id}", inner.id),
            )
        })?;
        if record.state != PaymentState::Held {
            return Err(conflict(
                WALLET_NOT_HELD_CODE,
                format!("payment {payment_id} is not held; it is {:?}", record.state),
            ));
        }
        let approved = decision.approved;
        record.decision = Some(decision);
        let not_held = || {
            conflict(
                WALLET_NOT_HELD_CODE,
                format!("payment {payment_id} is not held; another decision settled it"),
            )
        };
        if !approved {
            record.state = PaymentState::Rejected;
            // Another wallet instance over the store may decide the payment at the same
            // time: only the decision that moves it out of `Held` stands.
            if !inner.store.update_if(&record, PaymentState::Held).await? {
                return Err(not_held());
            }
            drop(gate);
            self.audit(
                "wallet.held",
                &record,
                caller,
                Some(AuditDecision::Rejected),
                Some(AuditRecordOutcome::NotRun),
                &[],
            )
            .await?;
            return Ok(record);
        }
        let rail = self.rail_for(&record.rail)?;
        let units = inner.policy.to_ledger_units(record.amount).ok_or_else(|| {
            invalid(WALLET_INVALID_AMOUNT_CODE, "the amount does not fit the spend ledger")
        })?;
        let key = SpendKey::org(owner_approved_scope(&inner.id, record.category))
            .with_vendor(record.payee.id.clone());
        let key = match &record.agent {
            Some(agent) => key.with_agent(agent.clone()),
            None => key,
        };
        let reservation = inner.ledger.reserve(&key, units).await?;
        record.state = PaymentState::Executing;
        let moved = match inner.store.update_if(&record, PaymentState::Held).await {
            Ok(moved) => moved,
            Err(error) => {
                let _ = inner.ledger.release(reservation).await;
                return Err(error);
            }
        };
        if !moved {
            // Another instance's decision moved it first; this one pays nothing.
            let _ = inner.ledger.release(reservation).await;
            return Err(not_held());
        }
        drop(gate);
        let reserved = Reserved { reservation, units, autonomous: false };
        if let Err(error) = self
            .audit("wallet.held", &record, caller, Some(AuditDecision::Approved), None, &[])
            .await
        {
            self.abandon(record, reserved, &error).await;
            return Err(error);
        }
        self.execute(record, rail, reserved, caller).await
    }

    /// Lifts an anomaly pause by a person's `decision` (spec 3.5 §7.14). Returns whether the
    /// category was paused.
    ///
    /// # Errors
    ///
    /// Returns `wallet.owner_decision_required` when the decision is not an owner's, or the
    /// store's error.
    pub async fn resume_category(
        &self,
        category: SpendCategory,
        decision: &ApprovalDecision,
    ) -> Result<bool> {
        self.check_owner(&decision.approver, decision.via_delegation.as_ref())?;
        let Some(pause) = self.inner.store.resume(&self.inner.id, category).await? else {
            return Ok(false);
        };
        if let Some(trail) = &self.inner.audit {
            let draft = AuditRecordDraft::new(AuditRecordKind::Payment, "wallet.anomaly")
                .actor(AuditActor::subject(decision.approver.clone()))
                .decision(AuditDecision::Approved)
                .policy_ref("wallet_policy")
                .ext("wallet", Value::String(self.inner.id.clone()))
                .ext("category", Value::String(category_name(category)))
                .ext("resolution", Value::String("resumed".to_string()))
                .ext("pausedAt", Value::String(pause.paused_at.to_rfc3339()));
            trail.record(draft).await?;
        }
        Ok(true)
    }

    /// The payments held for the owner, oldest first.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn held(&self) -> Result<Vec<PaymentRecord>> {
        self.inner.store.list(&PaymentQuery::new(&self.inner.id).state(PaymentState::Held)).await
    }

    /// The payment `id`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn payment(&self, id: &str) -> Result<Option<PaymentRecord>> {
        self.inner.store.get(&self.inner.id, id).await
    }

    /// The payments `query` matches; its wallet is always this wallet's.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn payments(&self, query: PaymentQuery) -> Result<Vec<PaymentRecord>> {
        let query = PaymentQuery { wallet: self.inner.id.clone(), ..query };
        self.inner.store.list(&query).await
    }

    /// Where each category with a mandate stands this period.
    ///
    /// # Errors
    ///
    /// Returns the ledger's or store's error.
    pub async fn status(&self) -> Result<Vec<CategoryStatus>> {
        let inner = &self.inner;
        let pauses = inner.store.pauses(&inner.id).await?;
        let mut statuses = Vec::new();
        for (category, mandate) in &inner.policy.categories {
            let spent = inner.policy.minor_from_ledger(self.period_spend_units(*category).await?);
            statuses.push(CategoryStatus {
                category: *category,
                cap_per_period: mandate.cap_per_period,
                max_per_payment: mandate.max_per_payment,
                spent,
                remaining: mandate.cap_per_period.saturating_sub(spent),
                rail: mandate.rail.clone(),
                payees: mandate.payees.clone(),
                paused: pauses
                    .iter()
                    .find(|pause| pause.category == *category)
                    .map(|pause| pause.reason.clone()),
            });
        }
        Ok(statuses)
    }

    /// What settled in `[from, to)`, in minor units: in `category`, or in every category.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn spent_between(
        &self,
        category: Option<SpendCategory>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<u64> {
        let mut query =
            PaymentQuery::new(&self.inner.id).state(PaymentState::Settled).between(from, to);
        query.category = category;
        let payments = self.inner.store.list(&query).await?;
        Ok(payments.iter().fold(0u64, |total, record| total.saturating_add(settled_amount(record))))
    }

    /// Reconciles `[from, to)` against the provider's statement and every rail that keeps
    /// its own (spec 3.5 §7.14), records `wallet.reconciled`, and raises a P2 alarm when
    /// they disagree.
    ///
    /// # Errors
    ///
    /// Returns a provider's, rail's or store's error.
    pub async fn reconcile(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<ReconciliationReport> {
        let inner = &self.inner;
        let provider_source = format!("provider:{}", inner.provider.name());
        let mut by_source = BTreeMap::new();
        by_source.insert(provider_source.clone(), inner.provider.statement(from, to).await?);
        let mut billing = BTreeSet::new();
        for (name, rail) in &inner.rails {
            if let Some(entries) = rail.statement(from, to).await? {
                by_source.insert(name.to_string(), entries);
            }
            if rail.funding() == RailFunding::ProviderBilling {
                billing.insert(name.clone());
            }
        }
        let payments = inner.store.list(&PaymentQuery::new(&inner.id).between(from, to)).await?;
        let in_window: BTreeSet<&str> = payments.iter().map(|record| record.id.as_str()).collect();
        let mut known_elsewhere = BTreeSet::new();
        for entries in by_source.values() {
            for reference in entries.iter().filter_map(|entry| entry.reference.as_deref()) {
                if !in_window.contains(reference)
                    && let Some(record) = inner.store.get(&inner.id, reference).await?
                    && record.state == PaymentState::Settled
                {
                    known_elsewhere.insert(reference.to_string());
                }
            }
        }
        let statements = Statements { by_source };
        let report = compare(
            &inner.id,
            from,
            to,
            &payments,
            &statements,
            |record| {
                if billing.contains(&record.rail) {
                    record.rail.to_string()
                } else {
                    provider_source.clone()
                }
            },
            &known_elsewhere,
        );
        if let Some(trail) = &inner.audit {
            let draft = AuditRecordDraft::new(AuditRecordKind::Payment, "wallet.reconciled")
                .decision(AuditDecision::Allowed)
                .outcome(if report.is_clean() {
                    AuditRecordOutcome::Succeeded
                } else {
                    AuditRecordOutcome::Failed
                })
                .policy_ref("wallet_policy")
                .ext("wallet", Value::String(inner.id.clone()))
                .ext("from", Value::String(from.to_rfc3339()))
                .ext("to", Value::String(to.to_rfc3339()))
                .ext("payments", Value::from(report.payments))
                .ext("matched", Value::from(report.matched))
                .ext("discrepancies", Value::from(report.discrepancies.len()));
            trail.record(draft).await?;
        }
        if !report.is_clean() {
            self.raise(WalletAlarm {
                wallet: inner.id.clone(),
                kind: WalletAlarmKind::ReconciliationMismatch {
                    from,
                    to,
                    discrepancies: report.discrepancies.len(),
                },
                raised_at: inner.clock.now(),
            })
            .await;
        }
        Ok(report)
    }

    /// Reconciles the last full window of the policy's cadence: last month by default.
    ///
    /// # Errors
    ///
    /// As [`reconcile`](Self::reconcile).
    pub async fn reconcile_due(&self) -> Result<ReconciliationReport> {
        let (from, to) = previous_window(self.inner.policy.reconciliation, self.inner.clock.now());
        self.reconcile(from, to).await
    }

    // ----- internals -----

    fn proposed(
        &self,
        request: &PaymentRequest,
        digest: &str,
        rail: RailRef,
        caller: &WalletCaller,
    ) -> PaymentRecord {
        PaymentRecord {
            id: format!("wpay_{}", uuid::Uuid::new_v4().simple()),
            wallet: self.inner.id.clone(),
            key: request.key.clone(),
            request_digest: digest.to_string(),
            category: request.category,
            payee: request.payee.clone(),
            amount: request.amount,
            currency: request.currency.to_ascii_uppercase(),
            rail,
            description: request.description.clone(),
            rail_data: request.rail_data.clone(),
            state: PaymentState::Executing,
            hold: None,
            agent: caller.agent.clone(),
            decision: None,
            debit: None,
            debit_outstanding: false,
            settlement: None,
            error: None,
            created_at: self.inner.clock.now(),
            settled_at: None,
        }
    }

    /// The mandate's checks that need no state: currency, amount, payee, rail and its data.
    fn validate(
        &self,
        request: &PaymentRequest,
        mandate: &Mandate,
        record: &PaymentRecord,
    ) -> Result<(Arc<dyn PaymentRail>, u64)> {
        let policy = &self.inner.policy;
        if !request.currency.eq_ignore_ascii_case(&policy.currency) {
            return Err(refused(
                WALLET_CURRENCY_MISMATCH_CODE,
                format!("the wallet pays in {}, not {}", policy.currency, request.currency),
            ));
        }
        let units = match policy.to_ledger_units(request.amount) {
            Some(units) if request.amount > 0 => units,
            _ => {
                return Err(invalid(
                    WALLET_INVALID_AMOUNT_CODE,
                    format!("{} is not an amount the wallet can pay", request.amount),
                ));
            }
        };
        if !mandate.allows_payee(&request.payee) {
            return Err(refused(
                WALLET_PAYEE_OUTSIDE_MANDATE_CODE,
                format!(
                    "payee {} is outside the {} mandate; only the owner adds payees",
                    request.payee.id,
                    category_name(request.category)
                ),
            ));
        }
        let rail = self.rail_for(&mandate.rail)?;
        rail.check(&rail_payment(record))?;
        Ok((rail, units))
    }

    fn rail_for(&self, name: &RailRef) -> Result<Arc<dyn PaymentRail>> {
        self.inner.rails.get(name).cloned().ok_or_else(|| {
            refused(WALLET_RAIL_UNAVAILABLE_CODE, format!("the wallet holds no {name} rail"))
        })
    }

    async fn pause_of(&self, category: SpendCategory) -> Result<Option<CategoryPause>> {
        let pauses = self.inner.store.pauses(&self.inner.id).await?;
        Ok(pauses.into_iter().find(|pause| pause.category == category))
    }

    /// Committed spend this period, autonomous and owner-approved, in ledger units.
    async fn period_spend_units(&self, category: SpendCategory) -> Result<u64> {
        let inner = &self.inner;
        let period = inner.policy.period;
        let own =
            inner.ledger.spent(&SpendKey::org(category_scope(&inner.id, category)), period).await?;
        let approved = inner
            .ledger
            .spent(&SpendKey::org(owner_approved_scope(&inner.id, category)), period)
            .await?;
        Ok(own.saturating_add(approved))
    }

    async fn anomaly(
        &self,
        category: SpendCategory,
        amount: u64,
        in_flight_units: u64,
    ) -> Result<Option<super::anomaly::Anomaly>> {
        let inner = &self.inner;
        let rule = &inner.policy.anomaly;
        let now = inner.clock.now();
        let today = now.date_naive();
        let window = i64::from(rule.trailing_days.max(rule.min_history_days));
        let since = super::policy::midnight(today - Duration::days(window));
        let query = PaymentQuery::new(&inner.id)
            .category(category)
            .state(PaymentState::Settled)
            .between(since, now + Duration::seconds(1));
        let mut daily: BTreeMap<NaiveDate, u64> = BTreeMap::new();
        for record in inner.store.list(&query).await? {
            if record.owner_approved() {
                continue;
            }
            let day = record.settled_at.unwrap_or(record.created_at).date_naive();
            *daily.entry(day).or_insert(0) += settled_amount(&record);
        }
        if in_flight_units > 0 {
            *daily.entry(today).or_insert(0) += inner.policy.minor_from_ledger(in_flight_units);
        }
        Ok(judge(rule, &daily, today, amount))
    }

    async fn pause_for_anomaly(
        &self,
        category: SpendCategory,
        requested: u64,
        anomaly: super::anomaly::Anomaly,
        caller: &WalletCaller,
    ) -> AdkError {
        let inner = &self.inner;
        let multiple = inner.policy.anomaly.multiple;
        let alarm = WalletAlarm {
            wallet: inner.id.clone(),
            kind: WalletAlarmKind::Anomaly {
                category,
                today: anomaly.today,
                requested,
                trailing_average: anomaly.trailing_average,
                multiple,
            },
            raised_at: inner.clock.now(),
        };
        let reason = alarm.summary();
        let paused = inner
            .store
            .pause(CategoryPause {
                wallet: inner.id.clone(),
                category,
                reason: reason.clone(),
                paused_at: alarm.raised_at,
            })
            .await;
        if let Err(error) = &paused {
            tracing::error!(error = %error, wallet.id = %inner.id, "the anomaly pause could not be stored");
        }
        tracing::warn!(wallet.id = %inner.id, category = %category_name(category), "spending anomaly; category paused");
        if let Some(trail) = &inner.audit {
            let draft = AuditRecordDraft::new(AuditRecordKind::Payment, "wallet.anomaly")
                .actor(caller.actor.clone())
                .decision(AuditDecision::Denied)
                .outcome(AuditRecordOutcome::NotRun)
                .policy_ref("wallet_policy")
                .ext("wallet", Value::String(inner.id.clone()))
                .ext("category", Value::String(category_name(category)))
                .ext("todayMinor", Value::from(anomaly.today))
                .ext("requestedMinor", Value::from(requested))
                .ext("trailingAverageMinor", json!(anomaly.trailing_average))
                .ext("multiple", json!(multiple));
            if let Err(error) = trail.record(draft).await {
                tracing::error!(error = %error, "the anomaly could not be audited");
            }
        }
        self.raise(alarm).await;
        refused(WALLET_CATEGORY_PAUSED_CODE, format!("{reason}; this payment was not made"))
    }

    async fn raise(&self, alarm: WalletAlarm) {
        let Some(hook) = &self.inner.alarms else {
            tracing::warn!(wallet.id = %self.inner.id, alarm = %alarm.summary(), "wallet alarm with no alarm hook");
            return;
        };
        if let Err(error) = hook.raise(&alarm).await {
            tracing::error!(error = %error, alarm = %alarm.summary(), "wallet alarm could not be raised");
        }
    }

    async fn hold(
        &self,
        mut record: PaymentRecord,
        reason: HoldReason,
        caller: &WalletCaller,
    ) -> Result<PaymentOutcome> {
        record.state = PaymentState::Held;
        record.hold = Some(reason);
        let (stored, inserted) = self.inner.store.insert(record.clone()).await?;
        if !inserted {
            return replay(stored, &record.request_digest);
        }
        tracing::info!(wallet.id = %self.inner.id, payment.id = %stored.id, "payment held for the owner");
        let reason_text = stored.hold.as_ref().map(ToString::to_string).unwrap_or_default();
        self.audit(
            "wallet.held",
            &stored,
            caller,
            Some(AuditDecision::ApprovalRequired),
            Some(AuditRecordOutcome::NotRun),
            &[("holdReason", Value::String(reason_text))],
        )
        .await?;
        Ok(PaymentOutcome::Held(stored))
    }

    async fn refuse(
        &self,
        record: &PaymentRecord,
        caller: &WalletCaller,
        error: AdkError,
    ) -> AdkError {
        tracing::info!(wallet.id = %self.inner.id, code = %error.code, "payment refused");
        let reason = Value::String(error.code.to_string());
        if let Err(audit_error) = self
            .audit(
                "wallet.payment",
                record,
                caller,
                Some(AuditDecision::Denied),
                Some(AuditRecordOutcome::NotRun),
                &[("refusal", reason)],
            )
            .await
        {
            tracing::warn!(error = %audit_error, "a refused payment could not be audited");
        }
        error
    }

    /// Gives up a reserved payment before any money moved.
    async fn abandon(&self, mut record: PaymentRecord, reserved: Reserved, error: &AdkError) {
        let _ = self.land(&reserved, Landing::Released(record.category)).await;
        record.state = PaymentState::Failed;
        record.error = Some(error.message.clone());
        if let Err(store_error) = self.inner.store.update(&record).await {
            tracing::error!(error = %store_error, "an abandoned payment could not be recorded");
        }
    }

    /// Takes a payment out of the category's in-flight total in one step under the gate with
    /// its outcome in the spend ledger and, once settled, in the journal (spec 3.5 §7.14): a
    /// payment checking the cap or the anomaly rule meanwhile sees it either in flight or
    /// recorded, never in both and never in neither, so in-cap payments are not held for
    /// spend counted twice.
    async fn land(&self, reserved: &Reserved, landing: Landing<'_>) -> Result<()> {
        let inner = &self.inner;
        let mut gate = inner.gate.lock().await;
        let reservation = reserved.reservation.clone();
        let (category, stored) = match landing {
            Landing::Settled { record, units } => {
                if let Err(error) = inner.ledger.commit(reservation, units).await {
                    tracing::error!(error = %error, payment.id = %record.id, "settled spend could not be committed");
                }
                (record.category, inner.store.update(record).await)
            }
            Landing::Released(category) => {
                let _ = inner.ledger.release(reservation).await;
                (category, Ok(()))
            }
        };
        if reserved.autonomous
            && let Some(units) = gate.get_mut(&category)
        {
            *units = units.saturating_sub(reserved.units);
        }
        stored
    }

    async fn execute(
        &self,
        mut record: PaymentRecord,
        rail: Arc<dyn PaymentRail>,
        reserved: Reserved,
        caller: &WalletCaller,
    ) -> Result<PaymentRecord> {
        let inner = &self.inner;
        if rail.funding() == RailFunding::Wallet {
            let request = DebitRequest {
                reference: record.id.clone(),
                payee: record.payee.clone(),
                amount: record.amount,
                currency: record.currency.clone(),
                description: record.description.clone(),
            };
            match inner.provider.debit(&request).await {
                Ok(debit) => record.debit = Some(debit),
                Err(error) => {
                    self.fail(record, reserved, &error, caller).await;
                    return Err(error);
                }
            }
        }
        match rail.execute(&rail_payment(&record)).await {
            Ok(settlement) => {
                let units =
                    inner.policy.to_ledger_units(settlement.amount).unwrap_or(reserved.units);
                if settlement.amount != record.amount {
                    tracing::warn!(payment.id = %record.id, requested = record.amount, settled = settlement.amount, "rail settled another amount");
                }
                record.state = PaymentState::Settled;
                record.settlement = Some(settlement);
                record.settled_at = Some(inner.clock.now());
                self.land(&reserved, Landing::Settled { record: &record, units }).await?;
                tracing::info!(wallet.id = %inner.id, payment.id = %record.id, "payment settled");
                if let Err(error) = self
                    .audit(
                        "wallet.payment",
                        &record,
                        caller,
                        None,
                        Some(AuditRecordOutcome::Succeeded),
                        &[],
                    )
                    .await
                {
                    tracing::error!(error = %error, "a settled payment could not be audited");
                }
                Ok(record)
            }
            Err(error) => {
                self.fail(record, reserved, &error, caller).await;
                Err(error)
            }
        }
    }

    /// Records a payment whose rail or provider failed: voids the debit, releases the
    /// reservation.
    async fn fail(
        &self,
        mut record: PaymentRecord,
        reserved: Reserved,
        error: &AdkError,
        caller: &WalletCaller,
    ) {
        let inner = &self.inner;
        if let Some(debit) = &record.debit
            && let Err(void_error) = inner.provider.void(debit).await
        {
            tracing::error!(error = %void_error, payment.id = %record.id, "a failed payment's debit could not be voided");
            record.debit_outstanding = true;
        }
        let _ = self.land(&reserved, Landing::Released(record.category)).await;
        record.state = PaymentState::Failed;
        record.error = Some(error.message.clone());
        if let Err(store_error) = inner.store.update(&record).await {
            tracing::error!(error = %store_error, "a failed payment could not be recorded");
        }
        if let Err(audit_error) = self
            .audit(
                "wallet.payment",
                &record,
                caller,
                None,
                Some(AuditRecordOutcome::Failed),
                &[("error", Value::String(error.code.to_string()))],
            )
            .await
        {
            tracing::warn!(error = %audit_error, "a failed payment could not be audited");
        }
    }

    fn check_owner(&self, approver: &SubjectRef, via: Option<&SubjectRef>) -> Result<()> {
        let is_person = |subject: &SubjectRef| {
            subject.kind == SubjectKind::Human
                && !subject.id.trim().is_empty()
                && !subject.id.starts_with("urn:adk:agent:")
        };
        if !is_person(approver) {
            return Err(owner_required(format!(
                "{} is not a person: only an owner decides held payments and resumes spending (A-I16)",
                approver.id
            )));
        }
        let owners = &self.inner.owners;
        if !owners.is_empty()
            && !owners.contains(approver)
            && !via.is_some_and(|delegator| owners.contains(delegator))
        {
            return Err(owner_required(format!(
                "{} is not an owner of wallet {}",
                approver.id, self.inner.id
            )));
        }
        Ok(())
    }

    async fn audit(
        &self,
        action: &'static str,
        record: &PaymentRecord,
        caller: &WalletCaller,
        decision: Option<AuditDecision>,
        outcome: Option<AuditRecordOutcome>,
        extra: &[(&str, Value)],
    ) -> Result<()> {
        let Some(trail) = &self.inner.audit else {
            return Ok(());
        };
        let mut draft = AuditRecordDraft::new(AuditRecordKind::Payment, action)
            .actor(caller.actor.clone())
            .args_digest(record.request_digest.clone())
            .policy_ref("wallet_policy")
            .ext("wallet", Value::String(self.inner.id.clone()))
            .ext("paymentId", Value::String(record.id.clone()))
            .ext("category", Value::String(category_name(record.category)))
            .ext("payee", Value::String(record.payee.id.clone()))
            .ext("amountMinor", Value::from(record.amount))
            .ext("currency", Value::String(record.currency.clone()))
            .ext("rail", Value::String(record.rail.to_string()));
        if let Some(version) = caller.charter_version {
            draft = draft.ext("charterVersion", Value::from(version));
        }
        if record.owner_approved() {
            draft = draft.ext("ownerApproved", Value::Bool(true));
        }
        for (key, value) in extra {
            draft = draft.ext(*key, value.clone());
        }
        if let Some(decision) = decision {
            draft = draft.decision(decision);
        }
        if let Some(outcome) = outcome {
            draft = draft.outcome(outcome);
        }
        trail.record(draft).await
    }
}

fn settled_amount(record: &PaymentRecord) -> u64 {
    record.settlement.as_ref().map_or(record.amount, |settlement| settlement.amount)
}

fn rail_payment(record: &PaymentRecord) -> RailPayment {
    RailPayment {
        payment_id: record.id.clone(),
        category: record.category,
        payee: record.payee.clone(),
        amount: record.amount,
        currency: record.currency.clone(),
        description: record.description.clone(),
        rail_data: record.rail_data.clone(),
        debit: record.debit.clone(),
    }
}

/// The outcome of a payment asked for again with its key.
fn replay(existing: PaymentRecord, digest: &str) -> Result<PaymentOutcome> {
    if existing.request_digest != digest {
        return Err(conflict(
            WALLET_KEY_REUSED_CODE,
            format!(
                "key {} already paid payment {}; a different payment needs a new key",
                existing.key, existing.id
            ),
        ));
    }
    match existing.state {
        PaymentState::Settled => Ok(PaymentOutcome::Settled(existing)),
        PaymentState::Held => Ok(PaymentOutcome::Held(existing)),
        PaymentState::Executing => Err(conflict(
            WALLET_PAYMENT_IN_PROGRESS_CODE,
            format!("payment {} is being paid; ask again once it settles", existing.id),
        )),
        PaymentState::Rejected => Err(refused(
            WALLET_PAYMENT_REJECTED_CODE,
            format!("the owner refused payment {}", existing.id),
        )),
        PaymentState::Failed => Err(AdkError::new(
            ErrorComponent::Guardrail,
            ErrorCategory::Unavailable,
            WALLET_PAYMENT_FAILED_CODE,
            format!(
                "payment {} failed: {}; a new attempt needs a new key",
                existing.id,
                existing.error.as_deref().unwrap_or("no reason recorded")
            ),
        )),
    }
}
