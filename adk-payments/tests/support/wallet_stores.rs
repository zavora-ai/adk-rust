//! Scenarios every durable wallet store runs (spec 3.5 §7.14, A-I16, S12): the conformance
//! suite, a held payment and a paused category surviving a restart, a cap two instances share
//! under concurrent payments, and a key paid once across instances.
//!
//! A [`Backend`] opens one instance's view of a shared database: a new store and a new spend
//! ledger over new connections, as a restarted or second process would. The provider, the
//! use-based scheme and the hosting account stand for services outside the site, so every
//! instance shares them.

#![allow(dead_code)]

use std::sync::Arc;

use adk_core::{
    ApprovalDecision, Clock, ManualClock, SpendKey, SpendLedger, SpendLimits, SpendPeriod,
    SubjectRef, SystemClock,
};
use adk_payments::wallet::conformance::wallet_store_conformance;
use adk_payments::wallet::{
    AnomalyRule, HoldReason, InMemoryBillingAccount, InMemoryUseBasedScheme,
    InMemoryWalletProvider, Mandate, Payee, PayeeRule, PaymentChallenge, PaymentOutcome,
    PaymentQuery, PaymentRequest, PaymentState, ProviderBillingRail, RailRef, SpendCategory,
    UseBasedRail, WALLET_CATEGORY_PAUSED_CODE, WALLET_PAYMENT_IN_PROGRESS_CODE, Wallet,
    WalletCaller, WalletPolicy, WalletProvider, WalletStore, category_scope,
};
use chrono::{TimeZone, Utc};

pub const OWNER: &str = "owner@example.com";
const MODELS_HOST: &str = "https://api.models.example/v1/generate";
const MODELS_PAYEE: &str = "acct-models";
/// The models cap per month, in cents.
pub const MODELS_CAP: u64 = 2_000;
/// Ledger units per cent of a USD wallet: micro-USD.
const UNITS_PER_CENT: u64 = 10_000;

/// One instance's store and spend ledger.
pub struct Instance {
    pub store: Arc<dyn WalletStore>,
    pub ledger: Arc<dyn SpendLedger>,
}

/// A shared database every instance opens.
#[async_trait::async_trait]
pub trait Backend: Send + Sync {
    /// Opens a new instance: a migrated store, and a migrated spend ledger checking `limits`
    /// on `clock`, both over new connections.
    async fn open(&self, limits: SpendLimits, clock: Arc<dyn Clock>) -> Instance;
}

/// The services outside the site.
#[derive(Clone)]
pub struct Services {
    pub provider: InMemoryWalletProvider,
    pub scheme: InMemoryUseBasedScheme,
    pub hostco: InMemoryBillingAccount,
}

impl Services {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            provider: InMemoryWalletProvider::stored_value("treasury", "USD", 1_000_000)
                .with_clock(clock.clone()),
            scheme: InMemoryUseBasedScheme::new(),
            hostco: InMemoryBillingAccount::new("hostco", "USD").with_clock(clock),
        }
    }
}

/// Models through the use-based rail, capped at [`MODELS_CAP`] with 500 per payment, and
/// hosting billed by `hostco`, 1_000 per payment; a day's spend at three times the day
/// before pauses a category.
pub fn policy() -> WalletPolicy {
    WalletPolicy::new("USD")
        .with_mandate(
            SpendCategory::Models,
            Mandate::new(MODELS_CAP, 500, RailRef::use_based())
                .with_payee(PayeeRule::Domain("models.example".into())),
        )
        .with_mandate(
            SpendCategory::Hosting,
            Mandate::new(5_000, 1_000, RailRef::provider_billing("hostco"))
                .with_payee(PayeeRule::Id("hostco".into())),
        )
        .with_anomaly(AnomalyRule {
            multiple: 3.0,
            trailing_days: 1,
            min_history_days: 1,
            min_amount: 0,
        })
}

/// A wallet `id` over `instance` and the shared `services`.
pub fn wallet(id: &str, services: &Services, instance: &Instance, clock: Arc<dyn Clock>) -> Wallet {
    Wallet::builder(id, policy(), Arc::new(services.provider.clone()))
        .rail(Arc::new(UseBasedRail::new(Arc::new(services.scheme.clone()))))
        .rail(Arc::new(ProviderBillingRail::new(Arc::new(services.hostco.clone()))))
        .spend_ledger(instance.ledger.clone())
        .store(instance.store.clone())
        .owner(SubjectRef::human(OWNER))
        .clock(clock)
        .build()
        .expect("the wallet builds")
}

pub fn tokens(cents: u64) -> PaymentChallenge {
    PaymentChallenge::new(MODELS_HOST, MODELS_PAYEE, cents, "USD")
}

pub fn growth() -> WalletCaller {
    WalletCaller::agent("urn:adk:agent:site/growth")
}

fn owner() -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human(OWNER))
}

fn wallet_id(scenario: &str) -> String {
    format!("site-{scenario}-{}", uuid::Uuid::new_v4().simple())
}

async fn models_spent(instance: &Instance, wallet: &str) -> u64 {
    instance
        .ledger
        .spent(&SpendKey::org(category_scope(wallet, SpendCategory::Models)), SpendPeriod::Month)
        .await
        .expect("the ledger reports spend")
}

/// The wallet store conformance suite on a new instance.
pub async fn conformance(backend: &dyn Backend) {
    let instance = backend.open(SpendLimits::new(), Arc::new(SystemClock)).await;
    wallet_store_conformance(instance.store.as_ref()).await;
}

/// A held payment, a paused category and settled spend survive dropping the store and
/// opening it again, and the owner decides and resumes them on the new instance.
pub async fn held_payment_and_pause_survive_a_restart(backend: &dyn Backend) {
    let id = wallet_id("restart");
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 5, 10, 0, 0).unwrap());
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let services = Services::new(shared.clone());
    let limits = policy().spend_limits(&id).unwrap();

    let first = backend.open(limits.clone(), shared.clone()).await;
    let before = wallet(&id, &services, &first, shared.clone());
    let day_one = before
        .pay_challenge("tokens-day-1", SpendCategory::Models, &tokens(100), &growth())
        .await
        .expect("an in-cap payment settles");
    let PaymentOutcome::Settled(day_one) = day_one else { panic!("expected a settlement") };
    clock.advance(std::time::Duration::from_secs(24 * 3_600));
    let renewal = PaymentRequest::new(
        "hosting-upgrade",
        SpendCategory::Hosting,
        Payee::new("hostco"),
        1_500,
        "USD",
    )
    .with_description("dedicated host");
    let PaymentOutcome::Held(held) = before.pay(renewal, &growth()).await.expect("held") else {
        panic!("a payment over the per-payment limit is held")
    };
    let burst = before
        .pay_challenge("tokens-burst", SpendCategory::Models, &tokens(400), &growth())
        .await
        .expect_err("a burst at four times yesterday's spend is refused");
    assert_eq!(burst.code, WALLET_CATEGORY_PAUSED_CODE);
    drop(before);
    drop(first);

    let second = backend.open(limits, shared.clone()).await;
    let after = wallet(&id, &services, &second, shared);
    assert_eq!(after.held().await.unwrap(), vec![held.clone()], "the held payment survives");
    assert_eq!(
        after.payment(&day_one.id).await.unwrap(),
        Some(day_one.clone()),
        "the settled payment survives"
    );
    let models = after
        .status()
        .await
        .unwrap()
        .into_iter()
        .find(|status| status.category == SpendCategory::Models)
        .expect("models has a mandate");
    assert!(models.paused.is_some(), "the pause survives");
    assert_eq!(models.spent, 100, "the settled spend survives in the ledger");
    let paused = after
        .pay_challenge("tokens-after-restart", SpendCategory::Models, &tokens(10), &growth())
        .await
        .expect_err("a paused category stays paused after a restart");
    assert_eq!(paused.code, WALLET_CATEGORY_PAUSED_CODE);
    let replay = after
        .pay_challenge("tokens-day-1", SpendCategory::Models, &tokens(100), &growth())
        .await
        .expect("a repeated key is answered");
    assert_eq!(replay, PaymentOutcome::Settled(day_one), "a key pays once across a restart");

    let decided = after.decide(&held.id, &owner()).await.expect("the owner decides");
    assert_eq!(decided.state, PaymentState::Settled);
    assert!(decided.owner_approved());
    assert!(after.held().await.unwrap().is_empty());
    assert!(after.resume_category(SpendCategory::Models, &owner()).await.unwrap());
    assert!(second.store.pauses(&id).await.unwrap().is_empty());
    let resumed = after
        .pay_challenge("tokens-after-resume", SpendCategory::Models, &tokens(10), &growth())
        .await
        .expect("a resumed category pays");
    assert!(resumed.is_settled());
    assert_eq!(services.scheme.paid(), 2, "the refused payments never reached the rail");
}

/// A deterministic sequence of payment amounts in cents, 50 to 300.
fn amounts(seed: u64, count: usize) -> Vec<u64> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            50 + (state >> 33) % 251
        })
        .collect()
}

/// Two wallet instances over one store and one spend ledger never settle past the models
/// cap under `payments` concurrent payments per round; filling the rest of the cap one
/// payment at a time then settles exactly to it, and either instance holds the next cent.
pub async fn instances_never_settle_past_the_cap(
    backend: &dyn Backend,
    rounds: u64,
    payments: usize,
) {
    for round in 0..rounds {
        let id = wallet_id("cap");
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let services = Services::new(clock.clone());
        let limits = policy().spend_limits(&id).unwrap();
        let a = backend.open(limits.clone(), clock.clone()).await;
        let b = backend.open(limits, clock.clone()).await;
        let wallets =
            [wallet(&id, &services, &a, clock.clone()), wallet(&id, &services, &b, clock)];

        let mut tasks = Vec::new();
        for (index, cents) in amounts(round, payments).into_iter().enumerate() {
            let wallet = wallets[index % 2].clone();
            tasks.push(tokio::spawn(async move {
                let key = format!("tokens-{index}");
                wallet.pay_challenge(&key, SpendCategory::Models, &tokens(cents), &growth()).await
            }));
        }
        let mut settled = 0u64;
        for task in tasks {
            match task
                .await
                .expect("the payment task finishes")
                .expect("a payment settles or is held")
            {
                PaymentOutcome::Settled(record) => settled += record.amount,
                PaymentOutcome::Held(record) => assert!(
                    matches!(record.hold, Some(HoldReason::OverCategoryCap { .. })),
                    "only the cap holds: {:?}",
                    record.hold
                ),
            }
        }
        assert!(
            settled <= MODELS_CAP,
            "round {round}: {settled} settled past the cap of {MODELS_CAP}"
        );
        assert_eq!(models_spent(&a, &id).await, settled * UNITS_PER_CENT, "the ledger agrees");

        let mut fills = 0;
        while settled < MODELS_CAP {
            let cents = (MODELS_CAP - settled).min(500);
            let outcome = wallets[fills % 2]
                .pay_challenge(
                    &format!("fill-{fills}"),
                    SpendCategory::Models,
                    &tokens(cents),
                    &growth(),
                )
                .await
                .expect("a payment within the cap is answered");
            assert!(
                outcome.is_settled(),
                "round {round}: {cents} fits under the cap with {settled} settled"
            );
            settled += cents;
            fills += 1;
        }
        for (index, wallet) in wallets.iter().enumerate() {
            let over = wallet
                .pay_challenge(
                    &format!("over-{index}"),
                    SpendCategory::Models,
                    &tokens(1),
                    &growth(),
                )
                .await
                .expect("a payment over the cap is answered");
            assert!(
                matches!(over.record().hold, Some(HoldReason::OverCategoryCap { .. })),
                "round {round}: instance {index} holds a cent past the shared cap"
            );
        }
        assert_eq!(models_spent(&b, &id).await, MODELS_CAP * UNITS_PER_CENT);

        let journal = a.store.list(&PaymentQuery::new(&id)).await.unwrap();
        assert_eq!(
            journal.len(),
            payments + fills + 2,
            "every payment is in the shared journal once"
        );
        assert_eq!(b.store.list(&PaymentQuery::new(&id)).await.unwrap(), journal);
        let journal_settled: u64 = journal
            .iter()
            .filter(|record| record.state == PaymentState::Settled)
            .map(|record| record.amount)
            .sum();
        assert_eq!(journal_settled, MODELS_CAP);
    }
}

/// Instances racing on one key make one payment: each answers it settled or in progress,
/// the journal holds it once, and the ledger and provider count it once.
pub async fn a_key_pays_once_across_instances(backend: &dyn Backend, attempts: usize) {
    let id = wallet_id("key");
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let services = Services::new(clock.clone());
    let limits = policy().spend_limits(&id).unwrap();
    let a = backend.open(limits.clone(), clock.clone()).await;
    let b = backend.open(limits, clock.clone()).await;
    let wallets = [wallet(&id, &services, &a, clock.clone()), wallet(&id, &services, &b, clock)];
    let balance = services.provider.balance().await.unwrap();

    let mut tasks = Vec::new();
    for attempt in 0..attempts {
        let wallet = wallets[attempt % 2].clone();
        tasks.push(tokio::spawn(async move {
            wallet.pay_challenge("renewal", SpendCategory::Models, &tokens(120), &growth()).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        match task.await.expect("the payment task finishes") {
            Ok(PaymentOutcome::Settled(record)) => ids.push(record.id),
            Ok(PaymentOutcome::Held(record)) => panic!("an in-cap payment is not held: {record:?}"),
            Err(error) => assert_eq!(error.code, WALLET_PAYMENT_IN_PROGRESS_CODE, "{error}"),
        }
    }
    assert!(!ids.is_empty(), "one attempt settles the payment");
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 1, "every settled answer is the one payment");
    let journal = a.store.list(&PaymentQuery::new(&id)).await.unwrap();
    assert_eq!(journal.len(), 1, "the journal holds the key once");
    assert_eq!(journal[0].state, PaymentState::Settled);
    assert_eq!(models_spent(&b, &id).await, 120 * UNITS_PER_CENT, "the ledger counts it once");
    assert_eq!(
        services.provider.balance().await.unwrap(),
        balance - 120,
        "the provider is debited once"
    );
}
