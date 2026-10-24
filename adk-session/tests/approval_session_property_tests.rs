//! Property tests for listing approval requests by session in the SQL stores, which filter
//! and page in the database (spec 3.3 §10 C1): paging `list_requests` by session —
//! application, user and session ID — returns exactly that session's requests, ordered by
//! creation time and then ID, with no gap and no duplicate, whatever the page size and
//! however the sessions' requests interleave, other users' sessions with the same ID
//! included.
//!
//! The SQLite property runs by default; the PostgreSQL one uses the server at
//! `DATABASE_URL`:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features sqlite,postgres --run-ignored all \
//!   --test approval_session_property_tests
//! ```

#![cfg(any(feature = "sqlite", feature = "postgres"))]

use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery, ApprovalScope, ApprovalStore,
    ApproverSelector, ToolConfirmationRequest,
};
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;
use serde_json::json;

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).single().expect("a valid instant")
}

/// One request of `app` per `(user, session, offset)` entry, with IDs that order like the
/// entries.
fn requests(app: &str, entries: &[(u8, u8, i64)]) -> Vec<ApprovalRequest> {
    entries
        .iter()
        .enumerate()
        .map(|(index, (user, session, offset))| {
            let call = ToolConfirmationRequest::new("send_invoice", json!({ "n": index }));
            let mut request = ApprovalRequest::new(
                format!("{app}/r{index:02}"),
                ApprovalScope::new(app, format!("u{user}"), format!("s{session}")),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("owners")]),
            );
            request.created_at = base_time() + chrono::Duration::seconds(*offset);
            request
        })
        .collect()
}

/// Creates the requests, pages through the `(user, session)` target's session, and compares.
async fn check(
    store: &dyn ApprovalStore,
    app: &str,
    entries: &[(u8, u8, i64)],
    page: usize,
    (user, session): (u8, u8),
) -> Result<(), TestCaseError> {
    let created = requests(app, entries);
    for request in &created {
        store.create_request(request).await.expect("create");
    }
    let scope = ApprovalScope::new(app, format!("u{user}"), format!("s{session}"));
    let mut want: Vec<&ApprovalRequest> =
        created.iter().filter(|request| request.scope == scope).collect();
    want.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let want: Vec<String> = want.into_iter().map(|request| request.id.clone()).collect();

    let mut listed = Vec::new();
    let mut after = None;
    loop {
        let query = ApprovalRequestQuery::default()
            .with_session(&scope)
            .with_after(after.clone())
            .with_limit(page);
        let batch = store.list_requests(&query).await.expect("list_requests");
        prop_assert!(batch.len() <= page, "a page holds at most its limit");
        after = batch.last().map(ApprovalRequest::cursor);
        let short = batch.len() < page;
        listed.extend(batch.into_iter().map(|request| request.id));
        if short {
            break;
        }
    }
    prop_assert_eq!(listed, want);
    Ok(())
}

fn entries() -> impl Strategy<Value = Vec<(u8, u8, i64)>> {
    prop::collection::vec((0u8..2, 0u8..3, 0i64..8), 0..30)
}

/// The 100 cases share one file database, each writing under an application of its own, as
/// the PostgreSQL property shares its server. The property is about paging, not durability,
/// so commits skip the sync (`synchronous = OFF`): a database and its migration per case,
/// with a synchronized commit per request, took 62–90 s on Windows.
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_pages_one_session_without_gaps_or_duplicates() {
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("approvals.db").display());
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
        .expect("a valid URL")
        .synchronous(sqlx::sqlite::SqliteSynchronous::Off);
    let store = runtime.block_on(async {
        let pool = sqlx::SqlitePool::connect_with(options).await.expect("open");
        let store = adk_session::SqliteApprovalStore::from_pool(pool);
        store.migrate().await.expect("migrate");
        store
    });
    let case = AtomicUsize::new(0);
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(100));
    let outcome =
        runner.run(&(entries(), 1usize..6, (0u8..2, 0u8..3)), |(entries, page, target)| {
            let app = format!("app-{}", case.fetch_add(1, Ordering::Relaxed));
            runtime.block_on(check(&store, &app, &entries, page, target))
        });
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
}

#[cfg(feature = "postgres")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    #[ignore = "requires a PostgreSQL server at DATABASE_URL"]
    fn postgres_pages_one_session_without_gaps_or_duplicates(
        entries in entries(),
        page in 1usize..6,
        target in (0u8..2, 0u8..3),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let url = std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
            let store = adk_session::PostgresApprovalStore::new(&url).await.expect("open");
            store.migrate().await.expect("migrate");
            let app = format!("property-{}", uuid::Uuid::new_v4());
            let checked = check(&store, &app, &entries, page, target).await;
            store.pool().close().await;
            checked
        })?;
    }
}
