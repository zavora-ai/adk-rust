//! Conformance suites for the wallet's adapter traits (spec 3.5 §11).
//!
//! | Trait | Suite | Built-in implementations |
//! |-------|-------|--------------------------|
//! | [`WalletProvider`] | [`wallet_provider_conformance`] | [`InMemoryWalletProvider`](super::InMemoryWalletProvider), [`PrepaidCardProvider`](super::PrepaidCardProvider) |
//! | [`WalletStore`] | [`wallet_store_conformance`] | [`InMemoryWalletStore`](super::InMemoryWalletStore) |
//! | [`PaymentRail`] | [`payment_rail_conformance`] | [`ProviderBillingRail`](super::ProviderBillingRail), [`UseBasedRail`](super::UseBasedRail), `Ap2Rail` |
//!
//! Each suite panics naming the broken rule, so it runs inside a `#[tokio::test]` against
//! any implementation.
//!
//! # Example
//!
//! ```
//! use adk_payments::wallet::InMemoryWalletProvider;
//! use adk_payments::wallet::conformance::wallet_provider_conformance;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! wallet_provider_conformance(&InMemoryWalletProvider::stored_value("treasury", "USD", 10_000)).await;
//! # }
//! ```

use chrono::{DateTime, Duration, Utc};

use super::policy::RailRef;
use super::policy::{Payee, SpendCategory};
use super::provider::{DebitRequest, StatementKind, WalletProvider};
use super::rail::{PaymentRail, RailPayment};
use super::store::{CategoryPause, PaymentQuery, PaymentRecord, PaymentState, WalletStore};

fn far_past() -> DateTime<Utc> {
    Utc::now() - Duration::days(3_650)
}

fn far_future() -> DateTime<Utc> {
    Utc::now() + Duration::days(3_650)
}

/// Checks a [`WalletProvider`] that holds at least 1_000 minor units of its currency.
///
/// Debits are idempotent on their reference, a reused reference with another amount and
/// another currency are refused, a debit beyond the funds is refused with
/// `wallet.insufficient_funds`, a void returns the money once, and the statement shows
/// the debit and its reversal under the payment's reference.
///
/// # Panics
///
/// Panics naming the rule the provider breaks.
pub async fn wallet_provider_conformance(provider: &dyn WalletProvider) {
    let currency = provider.currency().to_string();
    let reference = format!("conformance-{}", uuid::Uuid::new_v4().simple());
    let before = provider.balance().await.expect("the provider reports a balance");
    assert!(before >= 1_000, "the suite needs at least 1_000 minor units; {before} available");
    let request = DebitRequest {
        reference: reference.clone(),
        payee: Payee::new("conformance-payee"),
        amount: 250,
        currency: currency.clone(),
        description: "conformance".to_string(),
    };

    let debit = provider.debit(&request).await.expect("a funded debit succeeds");
    assert_eq!(
        (debit.amount, debit.reference.as_str()),
        (250, reference.as_str()),
        "the debit records its amount and reference"
    );
    assert!(
        debit.currency.eq_ignore_ascii_case(&currency),
        "the debit is in the provider's currency"
    );
    assert_eq!(provider.balance().await.unwrap(), before - 250, "a debit lowers the balance");

    let again = provider.debit(&request).await.expect("a repeated reference is answered");
    assert_eq!(again.id, debit.id, "a repeated reference returns the debit already made");
    assert_eq!(provider.balance().await.unwrap(), before - 250, "a repeated reference debits once");

    let other_amount = DebitRequest { amount: 251, ..request.clone() };
    assert!(
        provider.debit(&other_amount).await.is_err(),
        "a reused reference with another amount is refused"
    );

    let other_currency = DebitRequest {
        reference: format!("{reference}-fx"),
        currency: if currency.eq_ignore_ascii_case("EUR") { "USD".into() } else { "EUR".into() },
        ..request.clone()
    };
    assert!(provider.debit(&other_currency).await.is_err(), "another currency is refused");

    let too_much = DebitRequest {
        reference: format!("{reference}-big"),
        amount: before.saturating_add(1),
        ..request.clone()
    };
    let error = provider.debit(&too_much).await.expect_err("a debit beyond the funds is refused");
    assert_eq!(
        error.code,
        super::error::WALLET_INSUFFICIENT_FUNDS_CODE,
        "beyond the funds is insufficient funds"
    );

    provider.void(&debit).await.expect("a debit can be voided");
    assert_eq!(provider.balance().await.unwrap(), before, "a void returns the money");
    provider.void(&debit).await.expect("voiding twice is harmless");
    assert_eq!(provider.balance().await.unwrap(), before, "a second void returns nothing more");

    let statement = provider
        .statement(far_past(), far_future())
        .await
        .expect("the provider reports a statement");
    let ours: Vec<_> =
        statement.iter().filter(|entry| entry.reference.as_deref() == Some(&reference)).collect();
    assert!(
        ours.iter().any(|entry| entry.kind == StatementKind::Debit && entry.amount == 250),
        "the statement shows the debit under the payment's reference"
    );
    assert!(
        ours.iter().any(|entry| entry.kind == StatementKind::Reversal && entry.amount == 250),
        "the statement shows the reversal under the payment's reference"
    );
    assert!(
        !statement.iter().any(|entry| entry.reference.as_deref() == Some(&too_much.reference)),
        "a refused debit leaves no statement line"
    );
}

fn record(wallet: &str, key: &str, category: SpendCategory, at: DateTime<Utc>) -> PaymentRecord {
    PaymentRecord {
        id: format!("conformance-{}", uuid::Uuid::new_v4().simple()),
        wallet: wallet.to_string(),
        key: key.to_string(),
        request_digest: "digest".to_string(),
        category,
        payee: Payee::new("payee"),
        amount: 100,
        currency: "USD".to_string(),
        rail: RailRef::use_based(),
        description: String::new(),
        rail_data: None,
        state: PaymentState::Held,
        hold: None,
        agent: None,
        decision: None,
        debit: None,
        debit_outstanding: false,
        settlement: None,
        error: None,
        created_at: at,
        settled_at: None,
    }
}

/// Checks a [`WalletStore`]: one payment per wallet and key, updates by id, queries by
/// state, category and window, pauses kept until resumed, and no wallet reading
/// another's payments or pauses (A-I22).
///
/// # Panics
///
/// Panics naming the rule the store breaks.
pub async fn wallet_store_conformance(store: &dyn WalletStore) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let (wallet, other) = (format!("wallet-a-{suffix}"), format!("wallet-b-{suffix}"));
    let now = Utc::now();

    let first = record(&wallet, "key-1", SpendCategory::Models, now);
    let (stored, inserted) = store.insert(first.clone()).await.expect("insert");
    assert!(inserted, "a new key is inserted");
    assert_eq!(stored, first, "the stored record is the one given");
    let duplicate = record(&wallet, "key-1", SpendCategory::Hosting, now);
    let (kept, inserted) = store.insert(duplicate).await.expect("insert");
    assert!(!inserted, "a repeated key is not inserted");
    assert_eq!(kept, first, "a repeated key returns the first payment");
    let elsewhere = record(&other, "key-1", SpendCategory::Models, now);
    let (_, inserted) = store.insert(elsewhere.clone()).await.expect("insert");
    assert!(inserted, "the same key in another wallet is another payment");

    assert_eq!(
        store.by_key(&wallet, "key-1").await.unwrap(),
        Some(first.clone()),
        "a payment is found by key"
    );
    assert_eq!(
        store.get(&wallet, &first.id).await.unwrap(),
        Some(first.clone()),
        "a payment is found by id"
    );
    assert_eq!(
        store.get(&other, &first.id).await.unwrap(),
        None,
        "a wallet does not read another's payment"
    );

    let mut settled = first.clone();
    settled.state = PaymentState::Settled;
    settled.settled_at = Some(now);
    store.update(&settled).await.expect("update");
    assert_eq!(
        store.get(&wallet, &first.id).await.unwrap(),
        Some(settled.clone()),
        "an update replaces the payment"
    );
    let mut ghost = record(&wallet, "key-ghost", SpendCategory::Models, now);
    ghost.id = "never-stored".to_string();
    assert!(store.update(&ghost).await.is_err(), "updating an unknown payment fails");

    let old = record(&wallet, "key-2", SpendCategory::Advertising, now - Duration::days(40));
    store.insert(old.clone()).await.expect("insert");
    let held = store.list(&PaymentQuery::new(&wallet).state(PaymentState::Held)).await.unwrap();
    assert_eq!(held, vec![old.clone()], "a query by state finds only that state");
    let models =
        store.list(&PaymentQuery::new(&wallet).category(SpendCategory::Models)).await.unwrap();
    assert_eq!(models, vec![settled.clone()], "a query by category finds only that category");
    let recent = store
        .list(&PaymentQuery::new(&wallet).between(now - Duration::days(1), now + Duration::days(1)))
        .await
        .unwrap();
    assert_eq!(
        recent,
        vec![settled],
        "a window query finds payments by settlement or creation time"
    );
    let theirs = store.list(&PaymentQuery::new(&other)).await.unwrap();
    assert_eq!(theirs, vec![elsewhere], "a wallet lists only its own payments");

    let pause = CategoryPause {
        wallet: wallet.clone(),
        category: SpendCategory::Models,
        reason: "burst".to_string(),
        paused_at: now,
    };
    store.pause(pause.clone()).await.expect("pause");
    store
        .pause(CategoryPause { reason: "second".to_string(), ..pause.clone() })
        .await
        .expect("pause again");
    assert_eq!(
        store.pauses(&wallet).await.unwrap(),
        vec![pause.clone()],
        "the first pause is kept"
    );
    assert!(store.pauses(&other).await.unwrap().is_empty(), "a pause belongs to its wallet");
    assert_eq!(
        store.resume(&other, SpendCategory::Models).await.unwrap(),
        None,
        "another wallet cannot resume it"
    );
    assert_eq!(
        store.resume(&wallet, SpendCategory::Models).await.unwrap(),
        Some(pause),
        "resume returns the pause"
    );
    assert!(store.pauses(&wallet).await.unwrap().is_empty(), "a resumed category is not paused");
}

/// Checks a [`PaymentRail`] with a `payment` its data describes and a `mismatched` one whose
/// data does not: the first passes `check` and settles at its amount with a reference
/// (shown in the rail's own statement, when it keeps one); the second is refused by
/// `check` with `wallet.rail_data_mismatch`.
///
/// # Panics
///
/// Panics naming the rule the rail breaks.
pub async fn payment_rail_conformance(
    rail: &dyn PaymentRail,
    payment: RailPayment,
    mismatched: RailPayment,
) {
    rail.check(&payment).expect("a payment its data describes passes the check");
    let error = rail.check(&mismatched).expect_err("data describing another payment is refused");
    assert_eq!(
        error.code,
        super::error::WALLET_RAIL_DATA_MISMATCH_CODE,
        "a mismatch is wallet.rail_data_mismatch"
    );
    let settlement = rail.execute(&payment).await.expect("the payment settles");
    assert_eq!(settlement.amount, payment.amount, "the rail settles the amount allowed");
    assert!(!settlement.reference.is_empty(), "a settlement carries a reference");
    if let Some(entries) =
        rail.statement(far_past(), far_future()).await.expect("the statement reads")
    {
        assert!(
            entries.iter().any(|entry| entry.reference.as_deref() == Some(&payment.payment_id)
                && entry.amount == payment.amount),
            "a rail that keeps a statement shows the payment under its id"
        );
    }
}
