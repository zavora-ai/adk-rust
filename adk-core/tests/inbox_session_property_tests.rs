//! Property tests for listing approval requests by session (spec 3.3 §10 C1).
//!
//! - P1: paging `ApprovalStore::list_requests` by session returns exactly that session's
//!   requests, ordered by creation time and then ID, with no gap and no duplicate, whatever
//!   the page size and however the sessions' requests interleave.
//! - P2: paging `ApprovalService::inbox` by session returns the same sequence.

use std::sync::Arc;

use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRequestQuery, ApprovalScope, ApprovalService,
    ApprovalStore, ApproverSelector, InMemoryApprovalStore, InboxQuery, StoreApprovalService,
    ToolConfirmationRequest,
};
use chrono::{DateTime, TimeZone, Utc};
use proptest::prelude::*;
use serde_json::json;

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).single().expect("a valid instant")
}

/// One request per `(session, offset)` entry, with IDs that order like the entries.
fn requests(entries: &[(u8, i64)]) -> Vec<ApprovalRequest> {
    entries
        .iter()
        .enumerate()
        .map(|(index, (session, offset))| {
            let call = ToolConfirmationRequest::new("send_invoice", json!({ "n": index }));
            let mut request = ApprovalRequest::new(
                format!("r{index:02}"),
                ApprovalScope::new("app", "user", format!("s{session}")),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("owners")]),
            );
            request.created_at = base_time() + chrono::Duration::seconds(*offset);
            request
        })
        .collect()
}

/// The IDs of `session`'s requests, by creation time and then ID.
fn expected(requests: &[ApprovalRequest], session: &str) -> Vec<String> {
    let mut mine: Vec<&ApprovalRequest> =
        requests.iter().filter(|request| request.scope.session_id == session).collect();
    mine.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    mine.into_iter().map(|request| request.id.clone()).collect()
}

async fn store_pages(store: &dyn ApprovalStore, session: &str, page: usize) -> Vec<String> {
    let mut listed = Vec::new();
    let mut after = None;
    loop {
        let query = ApprovalRequestQuery::default()
            .with_app_name("app")
            .with_session_id(session)
            .with_after(after.clone())
            .with_limit(page);
        let batch = store.list_requests(&query).await.expect("list_requests");
        assert!(batch.len() <= page, "a page holds at most its limit");
        after = batch.last().map(ApprovalRequest::cursor);
        let short = batch.len() < page;
        listed.extend(batch.into_iter().map(|request| request.id));
        if short {
            return listed;
        }
    }
}

async fn inbox_pages(service: &dyn ApprovalService, session: &str, page: usize) -> Vec<String> {
    let mut listed = Vec::new();
    let mut cursor = None;
    loop {
        let query = InboxQuery::default()
            .with_app_name("app")
            .with_session_id(session)
            .with_cursor(cursor)
            .with_limit(page);
        let batch = service.inbox(query).await.expect("inbox");
        assert!(batch.items.len() <= page, "a page holds at most its limit");
        listed.extend(batch.items.into_iter().map(|request| request.id));
        match batch.next_cursor {
            Some(next) => cursor = Some(next),
            None => return listed,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn paging_by_session_has_no_gap_and_no_duplicate(
        entries in prop::collection::vec((0u8..3, 0i64..8), 0..30),
        page in 1usize..6,
        target in 0u8..3,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let store = Arc::new(InMemoryApprovalStore::new());
            let created = requests(&entries);
            for request in &created {
                store.create_request(request).await.expect("create");
            }
            let session = format!("s{target}");
            let want = expected(&created, &session);
            let service = StoreApprovalService::new(store.clone());
            prop_assert_eq!(store_pages(store.as_ref(), &session, page).await, want.clone());
            prop_assert_eq!(inbox_pages(&service, &session, page).await, want);
            Ok(())
        })?;
    }
}
