//! The back office's drafts live in the draft store and its requests in the approval service
//! (spec 3.5 §7.8.2 step 1, §7.6, §7.11): a content draft and a charter draft made on one
//! runtime, and the charter signing and promotion requests it submits, are read by a second
//! runtime over the same stores, which publishes the draft and numbers new drafts after every
//! number handed out. Two runtimes over one SQLite store get distinct ids, see each other's
//! drafts, and never overwrite each other's edits; signing and promotion requests an earlier
//! version kept in the draft store become approval requests once.

#![cfg(feature = "website")]

use crate::website_common;

use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditLogStore;
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalService, ApprovalState, GovernanceControl, InMemoryApprovalStore, StoreApprovalService,
};
use adk_harness::website::{
    DraftKind, InMemorySiteDraftStore, SiteDraftStore, SiteRef, SiteRuntime, back_office_tools,
};
use adk_kernel::charter::{
    CharterDirectory, CharterStore, CommitDecision, InMemoryCharterStore, RoleApproval,
};
use adk_site::content::{ContentStore, FsContentStore, InMemoryContentStore, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, SiteHost as _};
use adk_site::log::SiteLog;
use adk_site::signer::Ed25519ReleaseSigner;
use serde_json::{Value, json};

struct Parts {
    site: SiteRef,
    base: SiteProject,
    store: Arc<InMemoryContentStore>,
    host: Arc<DirectoryHost>,
    log: Arc<SiteLog>,
    work: tempfile::TempDir,
    _host_dir: tempfile::TempDir,
    /// The approval service every runtime over these parts shares, as replicas share one.
    approvals: Arc<dyn ApprovalService>,
    charters: Arc<InMemoryCharterStore>,
    governance: GovernanceControl,
}

async fn parts() -> Parts {
    let site = SiteRef::new(uuid::Uuid::new_v4(), "bakery", "https://bakery.example");
    let fixture = website_common::fixture_dir();
    let mut base = SiteProject::load(&fixture, chrono::Utc::now()).await.unwrap();
    base.items.clear();
    let store = Arc::new(InMemoryContentStore::default());
    for item in FsContentStore::new(&fixture).list().await.unwrap() {
        store.put(&item).await.unwrap();
    }
    let (host_dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("bakery.example", [7; 32]).unwrap());
    let log = Arc::new(
        SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "bakery.example", signer)
            .await
            .unwrap(),
    );
    let charters = Arc::new(InMemoryCharterStore::new());
    let approvals = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
        .with_directory(Arc::new(CharterDirectory::new(charters.clone()).for_site(site.id)));
    Parts {
        site,
        base,
        store,
        host: Arc::new(DirectoryHost::new(host_dir.path())),
        log,
        work,
        _host_dir: host_dir,
        approvals: Arc::new(approvals),
        charters,
        governance: GovernanceControl::new(),
    }
}

/// A runtime over `parts` keeping its drafts in `drafts`, building in `work`, as a restarted
/// process or a second replica opens it.
fn runtime(
    parts: &Parts,
    drafts: Arc<dyn SiteDraftStore>,
    work: &std::path::Path,
) -> Arc<SiteRuntime> {
    SiteRuntime::builder(
        parts.site.clone(),
        parts.base.clone(),
        parts.store.clone(),
        Arc::new(BuiltinGenerator),
        parts.host.clone(),
        parts.log.clone(),
        work,
    )
    .drafts(drafts)
    .approvals(parts.approvals.clone())
    .charters(parts.charters.clone())
    .governance(parts.governance.clone())
    .build()
    .unwrap()
}

async fn try_call(runtime: &Arc<SiteRuntime>, name: &str, args: Value) -> adk_core::Result<Value> {
    let tool = back_office_tools(runtime)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no back-office tool {name}"));
    let ctx = Arc::new(adk_tool::SimpleToolContext::new("steward"));
    tool.execute(ctx, args).await
}

async fn call(runtime: &Arc<SiteRuntime>, name: &str, args: Value) -> Value {
    try_call(runtime, name, args).await.unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn page(slug: &str) -> Value {
    json!({
        "kind": "page",
        "slug": slug,
        "language": "en",
        "fields": { "title": "Opening hours", "summary": "When the bakery is open." },
        "body": "We open at seven on weekdays.",
        "job": "launch"
    })
}

fn joining(slug: &str, draft: &str) -> Value {
    let mut args = page(slug);
    args["draft"] = json!(draft);
    args
}

#[tokio::test]
async fn drafts_and_requests_survive_a_restart() {
    let parts = parts().await;
    let drafts: Arc<dyn SiteDraftStore> = Arc::new(InMemorySiteDraftStore::default());
    let first = runtime(&parts, drafts.clone(), parts.work.path());
    assert_eq!(first.restore_drafts().await.unwrap(), 0, "a new site has none");
    let drafted = call(&first, "content_draft", page("hours")).await;
    assert_eq!(drafted["draft"], "draft-1", "{drafted}");
    let charter = website_common::charter(&parts.site, false);
    let charter_draft = call(&first, "charter_draft", json!({ "charter": charter })).await;
    assert_eq!(charter_draft["draft"], "charter-1", "{charter_draft}");
    let request = call(&first, "charter_commit_request", json!({ "draft": "charter-1" })).await;
    let signing = request["request"].as_str().unwrap().to_string();
    assert!(signing.starts_with(&format!("site-charter-{}-v1-", parts.site.id)), "{request}");
    let again = call(&first, "charter_commit_request", json!({ "draft": "charter-1" })).await;
    assert_eq!(again["request"], signing.as_str(), "the same content keeps its request");
    let promotion = call(
        &first,
        "job_promotion_request",
        json!({ "job": "freshness", "reason": "20 unchanged approvals" }),
    )
    .await;
    let promoting = promotion["request"].as_str().unwrap().to_string();
    assert!(promoting.starts_with("site-promotion-"), "{promotion}");
    drop(first);

    // The restarted runtime reads the drafts from the store and the requests from the
    // approval service.
    let second = runtime(&parts, drafts.clone(), parts.work.path());
    assert_eq!(second.restore_drafts().await.unwrap(), 2);
    let requests = second.charter_requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        (requests[0].id.as_str(), &requests[0].draft.mission),
        (signing.as_str(), &charter.mission)
    );
    let promotions = second.promotion_requests().await;
    assert_eq!(
        (promotions[0].id.as_str(), promotions[0].job.as_str()),
        (promoting.as_str(), "freshness")
    );
    let joined = call(&second, "content_draft", joining("about", "draft-1")).await;
    assert_eq!(joined["items"], json!(["pages/hours/en", "pages/about/en"]), "{joined}");

    // The draft publishes and leaves the store; its number is not handed out again.
    let published = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    assert_eq!(published["published"], true, "{published}");
    assert!(parts.host.fetch("/hours/").await.unwrap().is_some());
    assert!(parts.host.fetch("/about/").await.unwrap().is_some());
    let next = call(&second, "content_draft", page("menu")).await;
    assert_eq!(next["draft"], "draft-2", "{next}");
    let third = runtime(&parts, drafts, parts.work.path());
    assert_eq!(third.restore_drafts().await.unwrap(), 2, "the published draft is gone");
}

/// #1280: a charter signing request and a promotion request an earlier version kept in the
/// draft store become approval requests once; the owner's signature is carried over and
/// commits the charter, and a second restore converts nothing again.
#[tokio::test]
async fn stored_signing_and_promotion_requests_convert_once() {
    let parts = parts().await;
    let drafts: Arc<dyn SiteDraftStore> = Arc::new(InMemorySiteDraftStore::default());
    let charter = website_common::charter(&parts.site, false);
    let now = chrono::Utc::now();
    let owner =
        RoleApproval::new(awp_types::Role::Owner, SubjectRef::human(website_common::OWNER), now);
    let legacy_signing = json!({
        "id": "",
        "draft": charter,
        "roles": ["owner"],
        "signatures": [owner],
        "requestedBy": "urn:adk:agent:bakery/steward",
        "requestedAt": now,
    });
    let legacy_promotion = json!({
        "id": "",
        "job": "freshness",
        "reason": "20 unchanged approvals",
        "requestedBy": "urn:adk:agent:bakery/steward",
        "requestedAt": now,
    });
    let site = parts.site.id;
    let signing = drafts.create(site, DraftKind::CharterSign, &legacy_signing).await.unwrap();
    drafts.create(site, DraftKind::Promotion, &legacy_promotion).await.unwrap();

    let runtime = runtime(&parts, drafts.clone(), parts.work.path());
    assert_eq!(runtime.restore_drafts().await.unwrap(), 2, "both converted");
    let committed = parts.charters.current(site).await.unwrap().expect("the owner had signed");
    assert_eq!(committed.version(), 1);
    assert_eq!(
        committed.decision.decision_id,
        format!("site-charter-{site}-legacy-{}", signing.id),
        "the commit names the converted request"
    );
    let promotions = runtime.promotion_requests().await;
    assert_eq!(promotions.len(), 1);
    assert_eq!(promotions[0].id, format!("site-promotion-{site}-legacy-promotion-1"));
    assert!(drafts.list(site, DraftKind::CharterSign).await.unwrap().is_empty());
    assert!(drafts.list(site, DraftKind::Promotion).await.unwrap().is_empty());

    assert_eq!(runtime.restore_drafts().await.unwrap(), 0, "nothing converts twice");
    assert_eq!(runtime.promotion_requests().await.len(), 1);
    assert_eq!(parts.charters.current(site).await.unwrap().unwrap().version(), 1);
}

/// #1280: a release under an editor-and-publisher chain is one approval request bound to
/// the change. The editor's vote survives a restart and a new preview; an edit makes a new
/// request the vote does not count for; a chain satisfied while frozen publishes only when
/// the freeze lifts, once.
#[tokio::test]
async fn a_release_chain_survives_a_restart_and_binds_to_the_change() {
    use website_common::{OWNER, PUBLISHER};

    let parts = parts().await;
    let now = chrono::Utc::now();
    parts
        .charters
        .commit(
            website_common::charter(&parts.site, true),
            CommitDecision::new("dec-charter-1", now).with_approval(RoleApproval::new(
                awp_types::Role::Owner,
                SubjectRef::human(OWNER),
                now,
            )),
        )
        .await
        .unwrap();
    let drafts: Arc<dyn SiteDraftStore> = Arc::new(InMemorySiteDraftStore::default());
    let first = runtime(&parts, drafts.clone(), parts.work.path());
    call(&first, "content_draft", page("hours")).await;
    let pending = call(&first, "site_publish", json!({ "draft": "draft-1" })).await;
    let ticket = pending["pending"].as_str().expect("the chain holds the release").to_string();
    let request = parts.approvals.get(&ticket).await.unwrap().unwrap();
    let editor =
        first.approvals().approve(&ticket, request.version, &SubjectRef::human(OWNER)).await;
    assert_eq!(editor.unwrap().state, ApprovalState::Pending, "an early vote decides nothing");
    drop(first);

    // A restarted process previews the draft again and reaches the same request.
    let second = runtime(&parts, drafts, parts.work.path());
    let again = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    assert_eq!(again["pending"], ticket.as_str(), "{again}");

    // An edit is a new change: the editor's vote does not count for it.
    call(&second, "content_draft", joining("about", "draft-1")).await;
    let edited = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    let edited = edited["pending"].as_str().expect("the edit waits for its chain").to_string();
    assert_ne!(edited, ticket);
    let fresh = parts.approvals.get(&edited).await.unwrap().unwrap();
    assert!(fresh.decisions.is_empty(), "earlier votes stop counting");

    // Votes are recorded while frozen; the release waits until the freeze lifts.
    parts.governance.freeze("incident review");
    let approvals = second.approvals();
    let one = approvals.approve(&edited, fresh.version, &SubjectRef::human(OWNER)).await.unwrap();
    let both =
        approvals.approve(&edited, one.version, &SubjectRef::human(PUBLISHER)).await.unwrap();
    assert_eq!(both.state, ApprovalState::Approved);
    let held = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    assert_eq!((held["published"].clone(), held["held"].clone()), (json!(false), json!("frozen")));
    assert!(parts.host.fetch("/hours/").await.unwrap().is_none(), "nothing published");
    parts.governance.unfreeze();
    let live = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    assert_eq!(live["published"], true, "{live}");
    assert_eq!(live["approvers"], json!([OWNER, PUBLISHER]));
    assert!(
        try_call(&second, "site_publish", json!({ "draft": "draft-1" })).await.is_err(),
        "the release publishes once"
    );
}

/// #1280 (D3): a runtime that must be durable refuses stores it would keep in memory.
#[tokio::test]
async fn a_durable_runtime_refuses_in_memory_stores() {
    let parts = parts().await;
    let refused = SiteRuntime::builder(
        parts.site.clone(),
        parts.base.clone(),
        parts.store.clone(),
        Arc::new(BuiltinGenerator),
        parts.host.clone(),
        parts.log.clone(),
        parts.work.path(),
    )
    .drafts(Arc::new(InMemorySiteDraftStore::default()))
    .require_durable()
    .build()
    .unwrap_err();
    assert_eq!(refused.code, "website.runtime.not_durable");
    assert!(refused.to_string().contains("approvals"), "{refused}");
}

/// Holds the writes of one runtime until the test lets them through, so another runtime's
/// write lands between its read and its write.
#[cfg(feature = "sqlite")]
struct HeldWrites {
    inner: Arc<dyn SiteDraftStore>,
    waiting: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(feature = "sqlite")]
#[async_trait::async_trait]
impl SiteDraftStore for HeldWrites {
    async fn create(
        &self,
        site: uuid::Uuid,
        kind: adk_harness::website::DraftKind,
        value: &Value,
    ) -> adk_core::Result<adk_harness::website::StoredRecord> {
        self.inner.create(site, kind, value).await
    }

    async fn get(
        &self,
        site: uuid::Uuid,
        kind: adk_harness::website::DraftKind,
        id: &str,
    ) -> adk_core::Result<Option<adk_harness::website::StoredRecord>> {
        self.inner.get(site, kind, id).await
    }

    async fn replace(
        &self,
        site: uuid::Uuid,
        kind: adk_harness::website::DraftKind,
        id: &str,
        value: &Value,
        expected_version: u64,
    ) -> adk_core::Result<bool> {
        self.waiting.notify_one();
        self.release.notified().await;
        self.inner.replace(site, kind, id, value, expected_version).await
    }

    async fn remove(
        &self,
        site: uuid::Uuid,
        kind: adk_harness::website::DraftKind,
        id: &str,
    ) -> adk_core::Result<()> {
        self.inner.remove(site, kind, id).await
    }

    async fn list(
        &self,
        site: uuid::Uuid,
        kind: adk_harness::website::DraftKind,
    ) -> adk_core::Result<Vec<adk_harness::website::StoredRecord>> {
        self.inner.list(site, kind).await
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn two_runtimes_over_one_sqlite_store_share_drafts_without_overwriting() {
    use adk_harness::website::SqliteSiteDraftStore;

    let parts = parts().await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("edge.db").display());
    let open = || async {
        let store = SqliteSiteDraftStore::new(sqlx::SqlitePool::connect(&url).await.unwrap());
        store.migrate().await.unwrap();
        Arc::new(store) as Arc<dyn SiteDraftStore>
    };
    let (first_store, second_store) = (open().await, open().await);
    let second_work = tempfile::tempdir().unwrap();
    let first = runtime(&parts, first_store.clone(), parts.work.path());
    let second = runtime(&parts, second_store.clone(), second_work.path());

    // Drafts made at once on both replicas get distinct ids.
    let slugs = ["a", "b", "c", "d", "e", "f"];
    let made = futures::future::join_all(slugs.iter().enumerate().map(|(n, slug)| {
        let replica = if n % 2 == 0 { first.clone() } else { second.clone() };
        async move { call(&replica, "content_draft", page(slug)).await["draft"].clone() }
    }))
    .await;
    let distinct: std::collections::BTreeSet<String> =
        made.iter().map(|id| id.as_str().unwrap().to_string()).collect();
    assert_eq!(distinct.len(), slugs.len(), "{made:?}");

    // Each replica reads the other's drafts.
    let theirs = made[1].as_str().unwrap();
    let joined = call(&first, "content_draft", joining("menu", theirs)).await;
    assert_eq!(joined["items"].as_array().unwrap().len(), 2, "{joined}");

    // A replica whose edit lands after the other's, from the same read, is refused.
    let waiting = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let held = runtime(
        &parts,
        Arc::new(HeldWrites {
            inner: second_store.clone(),
            waiting: waiting.clone(),
            release: release.clone(),
        }),
        second_work.path(),
    );
    let target = made[0].as_str().unwrap().to_string();
    let late = tokio::spawn({
        let target = target.clone();
        async move { try_call(&held, "content_draft", joining("late", &target)).await }
    });
    waiting.notified().await;
    let early = call(&first, "content_draft", joining("early", &target)).await;
    assert_eq!(early["draft"], target.as_str());
    release.notify_one();
    let refused = late.await.unwrap().expect_err("the later edit read a version that changed");
    assert_eq!(refused.code, "website.drafts.conflict", "{refused}");
    let stored = first_store.get(parts.site.id, adk_harness::website::DraftKind::Content, &target);
    let stored = stored.await.unwrap().unwrap();
    assert_eq!(stored.version, 2, "only the earlier edit was written");
    assert!(stored.value.to_string().contains("pages/early/en"));
    assert!(!stored.value.to_string().contains("pages/late/en"));
}
