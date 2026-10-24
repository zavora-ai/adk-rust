//! Property tests for listing approval requests by session in the SQL stores, which filter
//! and page in the database (spec 3.3 §10 C1): paging `list_requests` by session returns
//! exactly that session's requests, ordered by creation time and then ID, with no gap and no
//! duplicate, whatever the page size and however the sessions' requests interleave.
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

/// One request of `app` per `(session, offset)` entry, with IDs that order like the entries.
fn requests(app: &str, entries: &[(u8, i64)]) -> Vec<ApprovalRequest> {
    entries
        .iter()
        .enumerate()
        .map(|(index, (session, offset))| {
            let call = ToolConfirmationRequest::new("send_invoice", json!({ "n": index }));
            let mut request = ApprovalRequest::new(
                format!("{app}/r{index:02}"),
                ApprovalScope::new(app, "user", format!("s{session}")),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("owners")]),
            );
            request.created_at = base_time() + chrono::Duration::seconds(*offset);
            request
        })
        .collect()
}

/// Creates the requests, pages through `target`'s session, and compares.
async fn check(
    store: &dyn ApprovalStore,
    app: &str,
    entries: &[(u8, i64)],
    page: usize,
    target: u8,
) -> Result<(), TestCaseError> {
    let created = requests(app, entries);
    for request in &created {
        store.create_request(request).await.expect("create");
    }
    let session = format!("s{target}");
    let mut want: Vec<&ApprovalRequest> =
        created.iter().filter(|request| request.scope.session_id == session).collect();
    want.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let want: Vec<String> = want.into_iter().map(|request| request.id.clone()).collect();

    let mut listed = Vec::new();
    let mut after = None;
    loop {
        let query = ApprovalRequestQuery::default()
            .with_app_name(app)
            .with_session_id(session.clone())
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

fn entries() -> impl Strategy<Value = Vec<(u8, i64)>> {
    prop::collection::vec((0u8..3, 0i64..8), 0..30)
}

#[cfg(feature = "sqlite")]
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn sqlite_pages_one_session_without_gaps_or_duplicates(
        entries in entries(),
        page in 1usize..6,
        target in 0u8..3,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let url = format!("sqlite://{}?mode=rwc", dir.path().join("approvals.db").display());
            let store = adk_session::SqliteApprovalStore::new(&url).await.expect("open");
            store.migrate().await.expect("migrate");
            check(&store, "app", &entries, page, target).await
        })?;
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
        target in 0u8..3,
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
