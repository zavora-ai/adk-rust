//! The SQLite wallet store answers as the in-memory reference store does (spec 3.5 §7.14):
//! for any sequence of inserts, updates, pauses and resumes over two wallets, every insert,
//! update, lookup and listing — windows at nanosecond boundaries included — is the same.
//!
//! ```bash
//! cargo nextest run -p adk-payments --features sqlite --test wallet_store_property_tests
//! ```

#![cfg(feature = "sqlite")]

use adk_payments::wallet::{
    CategoryPause, InMemoryWalletStore, Payee, PaymentQuery, PaymentRecord, PaymentState, RailRef,
    SpendCategory, SqliteWalletStore, WalletStore,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use proptest::prelude::*;

const WALLETS: [&str; 2] = ["site-a", "site-b"];
const CATEGORIES: [SpendCategory; 3] =
    [SpendCategory::Models, SpendCategory::Hosting, SpendCategory::Advertising];
const STATES: [PaymentState; 5] = [
    PaymentState::Held,
    PaymentState::Executing,
    PaymentState::Settled,
    PaymentState::Rejected,
    PaymentState::Failed,
];

fn base() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap()
}

/// An instant within a few microseconds of the base, so windows fall between and on
/// microsecond boundaries.
fn instant(nanos: i64) -> DateTime<Utc> {
    base() + Duration::nanoseconds(nanos)
}

#[derive(Debug, Clone)]
struct Insert {
    wallet: usize,
    key: u8,
    category: usize,
    state: usize,
    created: i64,
    settled: Option<i64>,
}

#[derive(Debug, Clone)]
struct Update {
    payment: usize,
    state: usize,
    settled: Option<i64>,
}

#[derive(Debug, Clone)]
struct Query {
    wallet: usize,
    category: Option<usize>,
    state: Option<usize>,
    window: Option<(i64, i64)>,
}

#[derive(Debug, Clone)]
enum Pause {
    Pause { wallet: usize, category: usize, reason: u8 },
    Resume { wallet: usize, category: usize },
}

fn nanos() -> impl Strategy<Value = i64> {
    0i64..6_000
}

fn inserts() -> impl Strategy<Value = Vec<Insert>> {
    prop::collection::vec(
        (0..2usize, 0..5u8, 0..3usize, 0..5usize, nanos(), prop::option::of(nanos())).prop_map(
            |(wallet, key, category, state, created, settled)| Insert {
                wallet,
                key,
                category,
                state,
                created,
                settled,
            },
        ),
        1..12,
    )
}

fn updates() -> impl Strategy<Value = Vec<Update>> {
    prop::collection::vec(
        (0..12usize, 0..5usize, prop::option::of(nanos()))
            .prop_map(|(payment, state, settled)| Update { payment, state, settled }),
        0..8,
    )
}

fn queries() -> impl Strategy<Value = Vec<Query>> {
    prop::collection::vec(
        (
            0..2usize,
            prop::option::of(0..3usize),
            prop::option::of(0..5usize),
            prop::option::of((nanos(), nanos())),
        )
            .prop_map(|(wallet, category, state, window)| Query {
                wallet,
                category,
                state,
                window,
            }),
        1..8,
    )
}

fn pauses() -> impl Strategy<Value = Vec<Pause>> {
    prop::collection::vec(
        prop_oneof![
            (0..2usize, 0..3usize, 0..3u8).prop_map(|(wallet, category, reason)| Pause::Pause {
                wallet,
                category,
                reason
            }),
            (0..2usize, 0..3usize)
                .prop_map(|(wallet, category)| Pause::Resume { wallet, category }),
        ],
        0..8,
    )
}

fn record(index: usize, insert: &Insert) -> PaymentRecord {
    PaymentRecord {
        id: format!("pay-{index}"),
        wallet: WALLETS[insert.wallet].to_string(),
        key: format!("key-{}", insert.key),
        request_digest: format!("digest-{index}"),
        category: CATEGORIES[insert.category],
        payee: Payee::new("payee"),
        amount: 100 + index as u64,
        currency: "USD".to_string(),
        rail: RailRef::use_based(),
        description: String::new(),
        rail_data: Some(serde_json::json!({ "price": 0.1 + index as f64 / 3.0 })),
        state: STATES[insert.state],
        hold: None,
        agent: None,
        decision: None,
        debit: None,
        debit_outstanding: false,
        settlement: None,
        error: None,
        created_at: instant(insert.created),
        settled_at: insert.settled.map(instant),
    }
}

async fn check(
    inserts: Vec<Insert>,
    updates: Vec<Update>,
    queries: Vec<Query>,
    pauses: Vec<Pause>,
) -> Result<(), TestCaseError> {
    let reference = InMemoryWalletStore::new();
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let durable = SqliteWalletStore::new(pool);
    durable.migrate().await.unwrap();

    let mut records = Vec::new();
    for (index, insert) in inserts.iter().enumerate() {
        let record = record(index, insert);
        let expected = reference.insert(record.clone()).await.unwrap();
        prop_assert_eq!(durable.insert(record.clone()).await.unwrap(), expected);
        records.push(record);
    }
    for update in &updates {
        let Some(record) = records.get(update.payment) else { continue };
        let mut changed = record.clone();
        changed.state = STATES[update.state];
        changed.settled_at = update.settled.map(instant);
        let expected = reference.update(&changed).await;
        let actual = durable.update(&changed).await;
        prop_assert_eq!(actual.is_ok(), expected.is_ok(), "update of {}", changed.id);
        if let Err(error) = actual {
            prop_assert_eq!(error.code, expected.unwrap_err().code);
        }
    }
    for record in &records {
        prop_assert_eq!(
            durable.get(&record.wallet, &record.id).await.unwrap(),
            reference.get(&record.wallet, &record.id).await.unwrap()
        );
        prop_assert_eq!(
            durable.by_key(&record.wallet, &record.key).await.unwrap(),
            reference.by_key(&record.wallet, &record.key).await.unwrap()
        );
    }
    for query in &queries {
        let mut asked = PaymentQuery::new(WALLETS[query.wallet]);
        asked.category = query.category.map(|index| CATEGORIES[index]);
        asked.state = query.state.map(|index| STATES[index]);
        if let Some((from, to)) = query.window {
            asked = asked.between(instant(from), instant(to));
        }
        prop_assert_eq!(
            durable.list(&asked).await.unwrap(),
            reference.list(&asked).await.unwrap(),
            "{:?}",
            asked
        );
    }
    for step in &pauses {
        match step {
            Pause::Pause { wallet, category, reason } => {
                let pause = CategoryPause {
                    wallet: WALLETS[*wallet].to_string(),
                    category: CATEGORIES[*category],
                    reason: format!("reason-{reason}"),
                    paused_at: base(),
                };
                reference.pause(pause.clone()).await.unwrap();
                durable.pause(pause).await.unwrap();
            }
            Pause::Resume { wallet, category } => {
                let expected =
                    reference.resume(WALLETS[*wallet], CATEGORIES[*category]).await.unwrap();
                prop_assert_eq!(
                    durable.resume(WALLETS[*wallet], CATEGORIES[*category]).await.unwrap(),
                    expected
                );
            }
        }
        for wallet in WALLETS {
            prop_assert_eq!(
                durable.pauses(wallet).await.unwrap(),
                reference.pauses(wallet).await.unwrap()
            );
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn the_sqlite_store_answers_as_the_reference_store(
        inserts in inserts(),
        updates in updates(),
        queries in queries(),
        pauses in pauses(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(check(inserts, updates, queries, pauses))?;
    }
}
