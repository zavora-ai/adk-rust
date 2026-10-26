//! The wallet's mandates, holds, owner acts, rails, anomaly pauses, reconciliation and KPI
//! source (spec 3.5 §7.14, D-10, A-I16).

#![cfg(feature = "wallet")]

#[path = "support/wallet.rs"]
mod wallet_support;

use std::sync::Arc;

use adk_core::audit_log::{AuditDecision, AuditRecordOutcome};
use adk_core::principal::AgentId;
use adk_core::{
    AdkError, ApprovalDecision, ErrorCategory, ErrorComponent, Result, SpendKey, SpendLedger,
    SpendPeriod, SubjectRef,
};
use adk_payments::wallet::conformance::{
    payment_rail_conformance, wallet_provider_conformance, wallet_store_conformance,
};
use adk_payments::wallet::{
    AWP_ERROR_DETAIL, AnomalyRule, CategoryPause, Discrepancy, HoldReason, InMemoryBillingAccount,
    InMemoryCardIssuer, InMemoryCommerceOutcomes, InMemoryUseBasedScheme, InMemoryWalletProvider,
    InMemoryWalletStore, Mandate, Payee, PayeeRule, PaymentOutcome, PaymentQuery, PaymentRail,
    PaymentRecord, PaymentRequest, PaymentState, PrepaidCardProvider, ProviderBillingRail,
    RailPayment, RailRef, RailSettlement, ReceiptDetails, ReceiptSigner, ReceiptVerifier,
    SpendCategory, StatementEntry, StatementKind, UseBasedRail, UseBasedScheme,
    WALLET_CATEGORY_PAUSED_CODE, WALLET_CURRENCY_MISMATCH_CODE, WALLET_KEY_REUSED_CODE,
    WALLET_NO_MANDATE_CODE, WALLET_NOT_HELD_CODE, WALLET_OWNER_DECISION_REQUIRED_CODE,
    WALLET_PAYEE_OUTSIDE_MANDATE_CODE, WALLET_PAYMENT_FAILED_CODE, WALLET_PAYMENT_REJECTED_CODE,
    WALLET_RAIL_DATA_MISMATCH_CODE, WALLET_WRONG_SITE_CODE, Wallet, WalletKpiSource, WalletPolicy,
    WalletStore, category_scope, issue_receipt, owner_approved_scope, verify_receipt,
};
use async_trait::async_trait;
use awp_types::{Proof, Urgency};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use wallet_support::*;

fn hosting(key: &str, cents: u64) -> PaymentRequest {
    PaymentRequest::new(key, SpendCategory::Hosting, Payee::new("hostco"), cents, "USD")
        .with_description("hosting")
}

fn ads(key: &str, cents: u64) -> PaymentRequest {
    PaymentRequest::new(key, SpendCategory::Advertising, Payee::new("adsco"), cents, "USD")
        .with_description("ads top-up")
}

fn start() -> chrono::DateTime<chrono::Utc> {
    at(2026, 10, 1, 9)
}

#[tokio::test]
async fn an_in_cap_payment_settles_through_its_rail_and_is_counted_by_the_ledger() {
    let f = Fixture::new(s12_policy(), start());
    let outcome = f.wallet.pay(hosting("h-1", 2_000), &finance()).await.unwrap();
    let PaymentOutcome::Settled(record) = outcome else { panic!("an in-cap payment settles") };
    assert_eq!(record.state, PaymentState::Settled);
    assert_eq!(record.settlement.as_ref().unwrap().amount, 2_000);
    assert!(record.debit.is_none(), "provider billing moves no wallet funds");

    let ledger_units = f
        .ledger
        .spent(&SpendKey::org(category_scope(SITE, SpendCategory::Hosting)), SpendPeriod::Month)
        .await
        .unwrap();
    assert_eq!(ledger_units, 20_000_000, "20 USD in millionths");
    let status = f.wallet.status().await.unwrap();
    let hosting = status.iter().find(|s| s.category == SpendCategory::Hosting).unwrap();
    assert_eq!((hosting.spent, hosting.remaining), (2_000, 3_000));
    assert_eq!(f.provider.available(), 1_000_000, "the treasury was not debited");
    assert_eq!(f.hostco.lines().len(), 1, "the provider bills one charge");

    let challenge = token_challenge(125);
    let outcome =
        f.wallet.pay_challenge("t-1", SpendCategory::Models, &challenge, &finance()).await.unwrap();
    let record = outcome.record();
    assert!(outcome.is_settled());
    assert_eq!(f.provider.available(), 1_000_000 - 125, "the use-based rail spends wallet funds");
    let proof = record.use_based_proof().expect("a metered payment carries its proof");
    assert_eq!(proof.header, "X-PAYMENT");
    assert!(f.scheme.verify(&challenge, &proof.value), "the resource accepts the proof once");
    assert!(!f.scheme.verify(&challenge, &proof.value), "a replayed proof is refused");
}

#[tokio::test]
async fn a_repeated_key_pays_once_and_a_reused_key_for_another_payment_is_refused() {
    let f = Fixture::new(s12_policy(), start());
    let first = f.wallet.pay(hosting("same", 1_000), &finance()).await.unwrap();
    for _ in 0..2 {
        let again = f.wallet.pay(hosting("same", 1_000), &finance()).await.unwrap();
        assert_eq!(again, first, "a retry returns the first payment");
    }
    assert_eq!(f.hostco.lines().len(), 1, "charged once");
    let error = f.wallet.pay(hosting("same", 1_001), &finance()).await.unwrap_err();
    assert_eq!(error.code, WALLET_KEY_REUSED_CODE);
}

#[tokio::test]
async fn payments_outside_the_mandate_are_refused_with_the_awp_code_and_audited() {
    let f = Fixture::new(s12_policy(), start());
    let outside =
        PaymentRequest::new("p-1", SpendCategory::Hosting, Payee::new("cheaphost"), 100, "USD");
    let error = f.wallet.pay(outside, &finance()).await.unwrap_err();
    assert_eq!(error.code, WALLET_PAYEE_OUTSIDE_MANDATE_CODE);
    assert_eq!(error.category, ErrorCategory::Forbidden);
    assert_eq!(error.details.metadata[AWP_ERROR_DETAIL], "payment_policy_rejected");

    let no_mandate = PaymentRequest::new("p-2", SpendCategory::Other, Payee::new("x"), 100, "USD");
    assert_eq!(
        f.wallet.pay(no_mandate, &finance()).await.unwrap_err().code,
        WALLET_NO_MANDATE_CODE
    );
    let euros =
        PaymentRequest::new("p-3", SpendCategory::Hosting, Payee::new("hostco"), 100, "EUR");
    assert_eq!(
        f.wallet.pay(euros, &finance()).await.unwrap_err().code,
        WALLET_CURRENCY_MISMATCH_CODE
    );

    // A challenge from another host than the mandate's domain names an outside payee.
    let elsewhere = adk_payments::wallet::PaymentChallenge::new(
        "https://api.other.example/x",
        MODELS_PAYEE,
        10,
        "USD",
    );
    let error = f
        .wallet
        .pay_challenge("p-4", SpendCategory::Models, &elsewhere, &finance())
        .await
        .unwrap_err();
    assert_eq!(error.code, WALLET_PAYEE_OUTSIDE_MANDATE_CODE);
    // A challenge that disagrees with the payment it is attached to is refused by the rail.
    let request = PaymentRequest::new(
        "p-5",
        SpendCategory::Models,
        Payee::new(MODELS_PAYEE).with_domain("api.models.example"),
        10,
        "USD",
    )
    .with_rail_data(serde_json::to_value(token_challenge(400)).unwrap());
    assert_eq!(
        f.wallet.pay(request, &finance()).await.unwrap_err().code,
        WALLET_RAIL_DATA_MISMATCH_CODE
    );

    assert_eq!(f.provider.available(), 1_000_000, "no refused payment moved money");
    assert!(
        f.wallet.payments(adk_payments::wallet::PaymentQuery::new(SITE)).await.unwrap().is_empty()
    );
    let refusals = f
        .audit
        .records()
        .into_iter()
        .filter(|record| {
            record.action == "wallet.payment" && record.decision == Some(AuditDecision::Denied)
        })
        .count();
    assert_eq!(refusals, 5, "every refusal is audited");
}

#[tokio::test]
async fn over_limit_and_over_cap_payments_are_held_and_only_an_owner_decides_them() {
    let f = Fixture::new(s12_policy(), start());
    // 35 USD is over the domains mandate's 30 USD per-payment limit.
    let renewal =
        PaymentRequest::new("d-1", SpendCategory::Domains, Payee::new("registrar"), 3_500, "USD");
    let PaymentOutcome::Held(held) = f.wallet.pay(renewal, &finance()).await.unwrap() else {
        panic!("over the per-payment limit is held")
    };
    assert_eq!(
        held.hold,
        Some(HoldReason::OverPaymentLimit { max_per_payment: 3_000, requested: 3_500 })
    );
    assert_eq!(f.registrar.lines().len(), 0, "nothing was charged");

    // The top-up is within the per-payment limit but over the category cap.
    let PaymentOutcome::Held(top_up) = f.wallet.pay(ads("a-1", 30_000), &finance()).await.unwrap()
    else {
        panic!("over the category cap is held")
    };
    assert_eq!(
        top_up.hold,
        Some(HoldReason::OverCategoryCap { cap: 20_000, spent: 0, requested: 30_000 })
    );
    assert_eq!(f.wallet.held().await.unwrap().len(), 2);

    // Neither an agent nor a service decides, nor a person who is not an owner.
    for approver in [
        SubjectRef::agent(&AgentId::new("urn:adk:agent:site-1/finance").unwrap()),
        SubjectRef::service("scheduler"),
        SubjectRef::human("urn:adk:agent:site-1/finance"),
        SubjectRef::human("stranger@example.com"),
    ] {
        let error =
            f.wallet.decide(&top_up.id, &ApprovalDecision::approve(approver)).await.unwrap_err();
        assert_eq!(error.code, WALLET_OWNER_DECISION_REQUIRED_CODE);
    }

    // The owner approves the top-up: paid beyond the mandate, in the owner-approved scope.
    let paid = f.wallet.decide(&top_up.id, &owner_approves()).await.unwrap();
    assert_eq!(paid.state, PaymentState::Settled);
    assert!(paid.owner_approved());
    assert_eq!(paid.decision.as_ref().unwrap().source, "owner");
    let owner_units = f
        .ledger
        .spent(
            &SpendKey::org(owner_approved_scope(SITE, SpendCategory::Advertising)),
            SpendPeriod::Month,
        )
        .await
        .unwrap();
    assert_eq!(owner_units, 300_000_000);
    let status = f.wallet.status().await.unwrap();
    let advertising = status.iter().find(|s| s.category == SpendCategory::Advertising).unwrap();
    assert_eq!(
        (advertising.cap_per_period, advertising.spent, advertising.remaining),
        (20_000, 30_000, 0),
        "the cap is unchanged and the approved spend counts toward it"
    );
    // So the site's next ad spend this month is held, not paid.
    assert!(f.wallet.pay(ads("a-2", 100), &finance()).await.unwrap().is_held());

    // Deciding again is refused; the owner rejects the renewal.
    assert_eq!(
        f.wallet.decide(&top_up.id, &owner_approves()).await.unwrap_err().code,
        WALLET_NOT_HELD_CODE
    );
    let rejected = f.wallet.decide(&held.id, &owner_rejects()).await.unwrap();
    assert_eq!(rejected.state, PaymentState::Rejected);
    let renewal =
        PaymentRequest::new("d-1", SpendCategory::Domains, Payee::new("registrar"), 3_500, "USD");
    assert_eq!(
        f.wallet.pay(renewal, &finance()).await.unwrap_err().code,
        WALLET_PAYMENT_REJECTED_CODE
    );
    assert_eq!(f.registrar.lines().len(), 0);

    let actions = f.audit_actions();
    assert!(actions.iter().filter(|action| *action == "wallet.held").count() >= 4, "{actions:?}");
}

#[tokio::test]
async fn concurrent_payments_never_take_a_category_past_its_cap() {
    let f = Fixture::new(s12_policy(), start());
    let wallet = f.wallet.clone();
    let tasks: Vec<_> = (0..40)
        .map(|i| {
            let wallet = wallet.clone();
            tokio::spawn(
                async move { wallet.pay(hosting(&format!("c-{i}"), 300), &finance()).await },
            )
        })
        .collect();
    let mut settled = 0;
    let mut held = 0;
    for task in tasks {
        match task.await.unwrap().unwrap() {
            PaymentOutcome::Settled(_) => settled += 1,
            PaymentOutcome::Held(_) => held += 1,
        }
    }
    assert_eq!(settled, 16, "16 x 3 USD fit under the 50 USD cap; the 17th would not");
    assert_eq!(held, 24);
    assert_eq!(f.hostco.lines().iter().map(|line| line.amount).sum::<u64>(), 4_800);
}

#[tokio::test]
async fn the_spend_ledger_checks_the_cap_across_wallet_instances() {
    // Two processes: two wallets over one ledger and one store. Each checks its own spend,
    // and the ledger's limits refuse what the pair would take past the cap.
    let f = Fixture::new(s12_policy(), start());
    let second = Wallet::builder(SITE, s12_policy(), Arc::new(f.provider.clone()))
        .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.hostco.clone()))))
        .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.registrar.clone()))))
        .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.adsco.clone()))))
        .rail(Arc::new(UseBasedRail::new(Arc::new(f.scheme.clone()))))
        .spend_ledger(f.ledger.clone() as Arc<dyn SpendLedger>)
        .store(Arc::new(f.store.clone()))
        .clock(Arc::new(f.clock.clone()))
        .build()
        .unwrap();
    let mut tasks = Vec::new();
    for i in 0..30 {
        let wallet = if i % 2 == 0 { f.wallet.clone() } else { second.clone() };
        tasks.push(tokio::spawn(async move {
            wallet.pay(hosting(&format!("x-{i}"), 400), &finance()).await
        }));
    }
    let mut settled = 0;
    for task in tasks {
        if task.await.unwrap().unwrap().is_settled() {
            settled += 1;
        }
    }
    assert_eq!(settled, 12, "12 x 4 USD fit under the 50 USD cap");
    assert_eq!(f.hostco.lines().iter().map(|line| line.amount).sum::<u64>(), 4_800);
}

/// A store whose reads are slow, as a shared database under load: two decisions on one
/// payment both read it held before either writes.
struct SlowReads(InMemoryWalletStore);

#[async_trait]
impl WalletStore for SlowReads {
    async fn insert(&self, record: PaymentRecord) -> Result<(PaymentRecord, bool)> {
        self.0.insert(record).await
    }
    async fn update(&self, record: &PaymentRecord) -> Result<()> {
        self.0.update(record).await
    }
    async fn update_if(&self, record: &PaymentRecord, expected: PaymentState) -> Result<bool> {
        self.0.update_if(record, expected).await
    }
    async fn get(&self, wallet: &str, id: &str) -> Result<Option<PaymentRecord>> {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        self.0.get(wallet, id).await
    }
    async fn by_key(&self, wallet: &str, key: &str) -> Result<Option<PaymentRecord>> {
        self.0.by_key(wallet, key).await
    }
    async fn list(&self, query: &PaymentQuery) -> Result<Vec<PaymentRecord>> {
        self.0.list(query).await
    }
    async fn pause(&self, pause: CategoryPause) -> Result<()> {
        self.0.pause(pause).await
    }
    async fn resume(&self, wallet: &str, category: SpendCategory) -> Result<Option<CategoryPause>> {
        self.0.resume(wallet, category).await
    }
    async fn pauses(&self, wallet: &str) -> Result<Vec<CategoryPause>> {
        self.0.pauses(wallet).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_decisions_on_one_held_payment_pay_it_once_across_instances() {
    // Two processes over one store and ledger: the owner approves on the phone while a
    // second approval, or a rejection, races it. Exactly one decision proceeds.
    for second_approves in [true, false] {
        let f = Fixture::new(s12_policy(), start());
        let slow: Arc<dyn WalletStore> = Arc::new(SlowReads(f.store.clone()));
        let instance = |builder: adk_payments::wallet::WalletBuilder| {
            builder
                .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.hostco.clone()))))
                .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.registrar.clone()))))
                .rail(Arc::new(ProviderBillingRail::new(Arc::new(f.adsco.clone()))))
                .rail(Arc::new(UseBasedRail::new(Arc::new(f.scheme.clone()))))
                .spend_ledger(f.ledger.clone() as Arc<dyn SpendLedger>)
                .store(slow.clone())
                .owner(SubjectRef::human(OWNER))
                .clock(Arc::new(f.clock.clone()))
                .build()
                .unwrap()
        };
        let first = instance(Wallet::builder(SITE, s12_policy(), Arc::new(f.provider.clone())));
        let second = instance(Wallet::builder(SITE, s12_policy(), Arc::new(f.provider.clone())));
        let PaymentOutcome::Held(top_up) =
            first.pay(ads("a-race", 30_000), &finance()).await.unwrap()
        else {
            panic!("over the category cap is held")
        };

        let id = top_up.id.clone();
        let approve = tokio::spawn({
            let id = id.clone();
            async move { first.decide(&id, &owner_approves()).await }
        });
        let other = tokio::spawn({
            let id = id.clone();
            let decision = if second_approves { owner_approves() } else { owner_rejects() };
            async move { second.decide(&id, &decision).await }
        });
        let outcomes = [approve.await.unwrap(), other.await.unwrap()];
        let proceeded: Vec<&PaymentRecord> =
            outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        assert_eq!(proceeded.len(), 1, "exactly one decision proceeds: {outcomes:?}");
        let refused = outcomes.iter().find_map(|o| o.as_ref().err()).expect("one is refused");
        assert_eq!(refused.code, WALLET_NOT_HELD_CODE);

        let stored = f.store.get(SITE, &id).await.unwrap().unwrap();
        assert_eq!(&stored, proceeded[0], "the journal keeps the decision that proceeded");
        let approved_units = f
            .ledger
            .spent(
                &SpendKey::org(owner_approved_scope(SITE, SpendCategory::Advertising)),
                SpendPeriod::Month,
            )
            .await
            .unwrap();
        let charged = f.adsco.lines().iter().map(|line| line.amount).sum::<u64>();
        match stored.state {
            PaymentState::Settled => {
                assert_eq!(approved_units, 300_000_000, "the ledger records the approval once");
                assert_eq!(charged, 30_000, "the provider charges once");
            }
            PaymentState::Rejected => {
                assert_eq!((approved_units, charged), (0, 0), "a rejection pays nothing");
            }
            other => panic!("the payment ends settled or rejected, not {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_in_cap_payments_are_never_held() {
    // Twenty payments that together fill the hosting cap exactly, paid at once, round after
    // round: each settles, because a settled payment is never counted both as spent and as
    // in flight.
    for round in 0..25 {
        let f = Fixture::new(s12_policy(), start());
        let tasks: Vec<_> = (0..20)
            .map(|i| {
                let wallet = f.wallet.clone();
                tokio::spawn(async move {
                    wallet.pay(hosting(&format!("r{round}-{i}"), 250), &finance()).await
                })
            })
            .collect();
        let mut held = Vec::new();
        for task in tasks {
            if let PaymentOutcome::Held(record) = task.await.unwrap().unwrap() {
                held.push(record.hold);
            }
        }
        assert!(held.is_empty(), "round {round}: in-cap payments were held: {held:?}");
        assert_eq!(f.hostco.lines().iter().map(|line| line.amount).sum::<u64>(), 5_000);
        assert!(
            f.wallet
                .pay(hosting(&format!("r{round}-over"), 1), &finance())
                .await
                .unwrap()
                .is_held(),
            "round {round}: the cap is reached exactly"
        );
    }
}

/// A rail that fails every payment, as a payee that refuses.
struct FailingRail;

#[async_trait]
impl PaymentRail for FailingRail {
    fn rail(&self) -> RailRef {
        RailRef::new("failing")
    }
    async fn execute(&self, _payment: &RailPayment) -> Result<RailSettlement> {
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unavailable,
            "test.payee_down",
            "payee down",
        ))
    }
}

#[tokio::test]
async fn a_failed_rail_voids_the_debit_and_frees_the_budget() {
    let policy = WalletPolicy::new("USD").with_mandate(
        SpendCategory::Other,
        Mandate::new(1_000, 1_000, RailRef::new("failing")).with_payee(PayeeRule::Any),
    );
    let f = Fixture::with(policy, start(), |builder| builder.rail(Arc::new(FailingRail)));
    let request =
        PaymentRequest::new("f-1", SpendCategory::Other, Payee::new("anyone"), 900, "USD");
    let error = f.wallet.pay(request.clone(), &finance()).await.unwrap_err();
    assert_eq!(error.code, "test.payee_down");
    assert_eq!(f.provider.available(), 1_000_000, "the debit was voided");
    let failed = f.wallet.payments(adk_payments::wallet::PaymentQuery::new(SITE)).await.unwrap();
    assert_eq!(failed[0].state, PaymentState::Failed);
    assert!(!failed[0].debit_outstanding);
    let status = f.wallet.status().await.unwrap();
    assert_eq!(status[0].spent, 0, "the reservation was released");
    assert_eq!(
        f.wallet.pay(request, &finance()).await.unwrap_err().code,
        WALLET_PAYMENT_FAILED_CODE
    );
}

#[tokio::test]
async fn a_burst_at_three_times_the_trailing_average_pauses_the_category_with_a_p1_alarm() {
    let f = Fixture::new(s12_policy(), at(2026, 9, 1, 9));
    // Fourteen days at 4 USD a day.
    for day in 0..14 {
        for call in 0..4 {
            let key = format!("d{day}-c{call}");
            assert!(
                f.wallet
                    .pay_challenge(key, SpendCategory::Models, &token_challenge(100), &finance())
                    .await
                    .unwrap()
                    .is_settled()
            );
        }
        f.advance_hours(24);
    }
    // Day 15: 11 USD settles; the payment that would make it 12 USD (3x) is refused.
    for call in 0..11 {
        let key = format!("burst-{call}");
        assert!(
            f.wallet
                .pay_challenge(key, SpendCategory::Models, &token_challenge(100), &finance())
                .await
                .unwrap()
                .is_settled(),
            "call {call}"
        );
    }
    let error = f
        .wallet
        .pay_challenge("burst-11", SpendCategory::Models, &token_challenge(100), &finance())
        .await
        .unwrap_err();
    assert_eq!(error.code, WALLET_CATEGORY_PAUSED_CODE);
    let alarms = f.alarms.alarms();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0].urgency(), Urgency::P1);
    assert!(alarms[0].summary().contains("models"), "{}", alarms[0].summary());
    assert!(f.audit_actions().contains(&"wallet.anomaly".to_string()));

    // Paused: refused until the owner resumes; other categories still pay.
    let error = f
        .wallet
        .pay_challenge("after-1", SpendCategory::Models, &token_challenge(10), &finance())
        .await
        .unwrap_err();
    assert_eq!(error.code, WALLET_CATEGORY_PAUSED_CODE);
    assert!(f.wallet.pay(hosting("h", 100), &finance()).await.unwrap().is_settled());
    assert_eq!(f.alarms.alarms().len(), 1, "a paused category raises no second alarm");
    let agent = ApprovalDecision::approve(SubjectRef::agent(
        &AgentId::new("urn:adk:agent:site-1/finance").unwrap(),
    ));
    assert_eq!(
        f.wallet.resume_category(SpendCategory::Models, &agent).await.unwrap_err().code,
        WALLET_OWNER_DECISION_REQUIRED_CODE
    );
    assert!(f.wallet.resume_category(SpendCategory::Models, &owner_approves()).await.unwrap());
    f.advance_hours(24);
    assert!(
        f.wallet
            .pay_challenge("next-day", SpendCategory::Models, &token_challenge(100), &finance())
            .await
            .unwrap()
            .is_settled()
    );
}

#[tokio::test]
async fn owner_approved_spend_never_trips_the_anomaly_rule() {
    let policy =
        s12_policy().with_anomaly(AnomalyRule { min_history_days: 1, ..AnomalyRule::default() });
    let f = Fixture::new(policy, at(2026, 9, 1, 9));
    assert!(f.wallet.pay(ads("small", 100), &finance()).await.unwrap().is_settled());
    f.advance_hours(24);
    let held = f.wallet.pay(ads("big", 25_000), &finance()).await.unwrap();
    assert!(held.is_held());
    let paid = f.wallet.decide(&held.record().id, &owner_approves()).await.unwrap();
    assert_eq!(paid.state, PaymentState::Settled);
    assert!(f.alarms.alarms().is_empty(), "the owner's own payment is not an anomaly");
}

#[tokio::test]
async fn reconciliation_matches_the_month_and_reports_what_the_providers_disagree_on() {
    let f = Fixture::new(s12_policy(), at(2026, 9, 1, 9));
    for day in 0..30 {
        let key = format!("tok-{day}");
        f.wallet
            .pay_challenge(key, SpendCategory::Models, &token_challenge(250), &finance())
            .await
            .unwrap();
        if day == 2 {
            f.wallet.pay(hosting("host-sep", 2_000), &finance()).await.unwrap();
        }
        f.advance_hours(24);
    }
    // 1 October: the monthly reconciliation of September is clean.
    let report = f.wallet.reconcile_due().await.unwrap();
    assert_eq!(report.from, at(2026, 9, 1, 0));
    assert_eq!(report.to, at(2026, 10, 1, 0));
    assert!(report.is_clean(), "{:?}", report.discrepancies);
    assert_eq!((report.payments, report.matched), (31, 31));
    assert_eq!(report.wallet_total, 30 * 250 + 2_000);
    assert_eq!(
        report.sources,
        vec![
            "provider:treasury".to_string(),
            "provider_billing:adsco".to_string(),
            "provider_billing:hostco".to_string(),
            "provider_billing:registrar".to_string(),
        ]
    );
    assert!(f.alarms.alarms().is_empty());
    assert!(f.audit_actions().contains(&"wallet.reconciled".to_string()));

    // A charge the wallet never made shows on the provider's statement.
    f.provider.push_statement_entry(StatementEntry {
        id: "treasury-x".into(),
        reference: None,
        kind: StatementKind::Debit,
        amount: 999,
        currency: "USD".into(),
        payee: Some("unknown".into()),
        at: at(2026, 9, 15, 12),
    });
    let report = f.wallet.reconcile(at(2026, 9, 1, 0), at(2026, 10, 1, 0)).await.unwrap();
    assert_eq!(
        report.discrepancies,
        vec![Discrepancy::NotInWallet {
            entry_id: "treasury-x".into(),
            source: "provider:treasury".into(),
            reference: None,
            amount: 999,
        }]
    );
    let alarms = f.alarms.alarms();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0].urgency(), Urgency::P2);
}

#[tokio::test]
async fn the_kpi_source_reports_spend_profit_and_costs_per_lead_and_sale() {
    let f = Fixture::new(s12_policy(), at(2026, 9, 1, 9));
    f.wallet.pay(hosting("h", 2_000), &finance()).await.unwrap();
    f.wallet
        .pay_challenge("t", SpendCategory::Models, &token_challenge(500), &finance())
        .await
        .unwrap();
    let outcomes = InMemoryCommerceOutcomes::new();
    for _ in 0..5 {
        outcomes.record_lead(at(2026, 9, 3, 10));
    }
    outcomes.record_sale(at(2026, 9, 4, 10), 10_000);
    outcomes.record_sale(at(2026, 9, 5, 10), 5_000);
    let site = uuid::Uuid::new_v4();
    let source = WalletKpiSource::new(f.wallet.clone(), site).with_outcomes(Arc::new(outcomes));
    let (from, to) = (at(2026, 9, 1, 0), at(2026, 10, 1, 0));
    let read = |metric: &'static str| source.read(site, metric, from, to);
    assert_eq!(source.name(), "spend");
    assert_eq!(read("spend").await.unwrap(), Some(25.0));
    assert_eq!(read("spend.hosting").await.unwrap(), Some(20.0));
    assert_eq!(read("spend.models").await.unwrap(), Some(5.0));
    assert_eq!(read("spend.nonsense").await.unwrap(), None);
    assert_eq!(read("revenue").await.unwrap(), Some(150.0));
    assert_eq!(read("profit").await.unwrap(), Some(125.0));
    assert_eq!(read("cost_per_lead").await.unwrap(), Some(5.0));
    assert_eq!(read("cost_per_sale").await.unwrap(), Some(12.5));
    let error = source.read(uuid::Uuid::new_v4(), "spend", from, to).await.unwrap_err();
    assert_eq!(error.code, WALLET_WRONG_SITE_CODE, "a wallet answers only for its site (A-I22)");
    let bare = WalletKpiSource::new(f.wallet.clone(), site);
    assert_eq!(bare.read(site, "cost_per_lead", from, to).await.unwrap(), None);
}

#[tokio::test]
async fn payments_carry_the_agent_run_and_charter_version_in_their_audit_records() {
    let f = Fixture::new(s12_policy(), start());
    f.wallet.pay(hosting("h", 100), &finance()).await.unwrap();
    let records = f.audit.records();
    let settled = records
        .iter()
        .find(|record| {
            record.action == "wallet.payment"
                && record.outcome == Some(AuditRecordOutcome::Succeeded)
        })
        .expect("the settled payment is audited");
    assert_eq!(settled.actor.agent.as_ref().unwrap().as_str(), "urn:adk:agent:site-1/finance");
    assert_eq!(settled.ext["charterVersion"], 1);
    assert_eq!(settled.ext["category"], "hosting");
    assert_eq!(settled.ext["amountMinor"], 100);
    assert!(records.iter().any(|record| record.action == "wallet.payment"
        && record.decision == Some(AuditDecision::Allowed)));
}

#[tokio::test]
async fn the_policy_and_the_providers_offer_no_way_to_raise_a_cap_or_add_a_payment_method() {
    let f = Fixture::new(s12_policy(), start());
    let before = f.wallet.policy().clone();
    // A copy of the policy is the agent's to change; the wallet's is not.
    let mut copy = f.wallet.policy().clone();
    copy.categories.get_mut(&SpendCategory::Advertising).unwrap().cap_per_period = u64::MAX;
    copy.categories.get_mut(&SpendCategory::Advertising).unwrap().payees.push(PayeeRule::Any);
    assert!(f.wallet.pay(ads("a", 25_000), &finance()).await.unwrap().is_held());
    let outside =
        PaymentRequest::new("b", SpendCategory::Advertising, Payee::new("newads"), 10, "USD");
    assert_eq!(
        f.wallet.pay(outside, &finance()).await.unwrap_err().code,
        WALLET_PAYEE_OUTSIDE_MANDATE_CODE
    );
    assert_eq!(f.wallet.policy(), &before, "nothing changed the wallet's policy");
    let funding = f
        .provider
        .entries()
        .into_iter()
        .filter(|entry| entry.kind == StatementKind::Funding)
        .count();
    assert_eq!(funding, 0, "the site added no funds or payment method");
}

#[tokio::test]
async fn wallet_providers_pass_the_conformance_suite() {
    wallet_provider_conformance(&InMemoryWalletProvider::stored_value("treasury", "USD", 10_000))
        .await;
    let issuer = Arc::new(InMemoryCardIssuer::new("issuer", "USD", 10_000));
    wallet_provider_conformance(&PrepaidCardProvider::new(issuer.clone(), "USD")).await;

    issuer.freeze(true);
    let card = PrepaidCardProvider::new(issuer, "USD");
    assert_eq!(adk_payments::wallet::WalletProvider::balance(&card).await.unwrap(), 0);
}

#[tokio::test]
async fn the_wallet_store_passes_the_conformance_suite() {
    wallet_store_conformance(&InMemoryWalletStore::new()).await;
}

fn rail_payment(payee: Payee, amount: u64, rail_data: Option<Value>) -> RailPayment {
    RailPayment {
        payment_id: format!("pay-{}", uuid::Uuid::new_v4().simple()),
        category: SpendCategory::Hosting,
        payee,
        amount,
        currency: "USD".into(),
        description: "conformance".into(),
        rail_data,
        debit: None,
    }
}

#[tokio::test]
async fn the_built_in_rails_pass_the_conformance_suite() {
    let billing = ProviderBillingRail::new(Arc::new(InMemoryBillingAccount::new("hostco", "USD")));
    payment_rail_conformance(
        &billing,
        rail_payment(Payee::new("hostco"), 700, None),
        rail_payment(Payee::new("otherhost"), 700, None),
    )
    .await;

    let use_based = UseBasedRail::new(Arc::new(InMemoryUseBasedScheme::new()));
    let challenge = token_challenge(40);
    let payee = challenge.payee();
    payment_rail_conformance(
        &use_based,
        rail_payment(payee.clone(), 40, Some(serde_json::to_value(&challenge).unwrap())),
        rail_payment(payee, 41, Some(serde_json::to_value(&challenge).unwrap())),
    )
    .await;
}

#[tokio::test]
async fn the_use_based_scheme_reads_x402_challenges() {
    let scheme = InMemoryUseBasedScheme::new();
    let challenges = scheme
        .challenges(&json!({
            "x402Version": 1,
            "error": "X-PAYMENT header is required",
            "accepts": [{
                "scheme": "exact",
                "network": "local",
                "resource": MODELS_HOST,
                "payTo": MODELS_PAYEE,
                "maxAmountRequired": "125",
                "asset": "usd",
                "maxTimeoutSeconds": 60
            }]
        }))
        .unwrap();
    assert_eq!(challenges.len(), 1);
    assert_eq!(challenges[0].amount, 125);
    assert_eq!(challenges[0].currency, "USD");
    assert_eq!(challenges[0].payee().domain.as_deref(), Some("api.models.example"));
    assert!(scheme.challenges(&json!({ "accepts": [{ "payTo": "x" }] })).is_err());
}

/// A stand-in for the delegated receipt key: HMAC-SHA256 over the document's JSON.
struct TestReceiptKey(&'static [u8]);

impl TestReceiptKey {
    fn mac(&self, document: &Value) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.0).unwrap();
        mac.update(document.to_string().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

#[async_trait]
impl ReceiptSigner for TestReceiptKey {
    async fn sign(&self, document: &Value) -> Result<Proof> {
        Ok(Proof::eddsa("receipt-2026-10", self.mac(document)))
    }
}

#[async_trait]
impl ReceiptVerifier for TestReceiptKey {
    async fn verify(&self, document: &Value, proof: &Proof) -> Result<()> {
        if proof.kid == "receipt-2026-10" && proof.jws == self.mac(document) {
            Ok(())
        } else {
            Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "receipt.invalid",
                "the receipt does not verify",
            ))
        }
    }
}

#[tokio::test]
async fn a_receipt_issued_through_the_hook_verifies_and_tampering_fails() {
    let key = TestReceiptKey(b"delegated-receipt-key");
    let details = ReceiptDetails::new("book_session", "booking-17", 12_000, "usd").with_rail("ap2");
    let receipt = issue_receipt(&key, details, start()).await.unwrap();
    assert_eq!(receipt.currency, "USD");
    assert_eq!(receipt.proof.kid, "receipt-2026-10");
    verify_receipt(&key, &receipt).await.expect("an issued receipt verifies");

    let mut tampered = receipt.clone();
    tampered.amount = 1_200;
    assert!(verify_receipt(&key, &tampered).await.is_err(), "a changed amount fails");
    let other = TestReceiptKey(b"not-the-sites-key");
    assert!(verify_receipt(&other, &receipt).await.is_err(), "another key fails");
}

#[tokio::test]
async fn a_wallet_refuses_a_policy_it_cannot_enforce() {
    let provider = Arc::new(InMemoryWalletProvider::stored_value("treasury", "USD", 0));
    let missing_rail = WalletPolicy::new("USD").with_mandate(
        SpendCategory::Hosting,
        Mandate::new(100, 100, RailRef::provider_billing("hostco")).with_payee(PayeeRule::Any),
    );
    assert!(Wallet::builder(SITE, missing_rail, provider.clone()).build().is_err());
    let other_currency = WalletPolicy::new("EUR");
    assert!(Wallet::builder(SITE, other_currency, provider).build().is_err());
}
