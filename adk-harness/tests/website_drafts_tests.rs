//! The back office's drafts survive a restart (spec 3.5 §7.8.2 step 1, §7.6, §7.11): a
//! content draft, a charter draft, a charter signing request and a promotion request made on
//! one runtime are read back by a second runtime over the same draft store, which publishes
//! the draft and numbers new drafts after the restored ones.

#![cfg(feature = "website")]

use crate::website_common;

use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditLogStore;
use adk_harness::website::{
    InMemorySiteDraftStore, SiteDraftStore, SiteRef, SiteRuntime, back_office_tools,
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
    Parts {
        site,
        base,
        store,
        host: Arc::new(DirectoryHost::new(host_dir.path())),
        log,
        work,
        _host_dir: host_dir,
    }
}

/// A runtime over `parts` keeping its drafts in `drafts`, as a restarted process opens it.
fn runtime(parts: &Parts, drafts: &Arc<InMemorySiteDraftStore>) -> Arc<SiteRuntime> {
    SiteRuntime::builder(
        parts.site.clone(),
        parts.base.clone(),
        parts.store.clone(),
        Arc::new(BuiltinGenerator),
        parts.host.clone(),
        parts.log.clone(),
        parts.work.path(),
    )
    .drafts(drafts.clone() as Arc<dyn SiteDraftStore>)
    .build()
}

async fn call(runtime: &Arc<SiteRuntime>, name: &str, args: Value) -> Value {
    let tool = back_office_tools(runtime)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no back-office tool {name}"));
    let ctx = Arc::new(adk_tool::SimpleToolContext::new("steward"));
    tool.execute(ctx, args).await.unwrap_or_else(|error| panic!("{name}: {error}"))
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

#[tokio::test]
async fn drafts_and_requests_survive_a_restart() {
    let parts = parts().await;
    let drafts = Arc::new(InMemorySiteDraftStore::default());
    let first = runtime(&parts, &drafts);
    assert_eq!(first.restore_drafts().await.unwrap(), 0, "a new site has none");
    let drafted = call(&first, "content_draft", page("hours")).await;
    assert_eq!(drafted["draft"], "draft-1", "{drafted}");
    let charter = website_common::charter(&parts.site, false);
    let charter_draft = call(&first, "charter_draft", json!({ "charter": charter })).await;
    assert_eq!(charter_draft["draft"], "charter-2", "{charter_draft}");
    let request = call(&first, "charter_commit_request", json!({ "draft": "charter-2" })).await;
    assert_eq!(request["request"], "charter-sign-3", "{request}");
    let promotion = call(
        &first,
        "job_promotion_request",
        json!({ "job": "freshness", "reason": "20 unchanged approvals" }),
    )
    .await;
    assert_eq!(promotion["request"], "promotion-4", "{promotion}");
    drop(first);

    // The restarted runtime reads every one back.
    let second = runtime(&parts, &drafts);
    assert_eq!(second.restore_drafts().await.unwrap(), 4);
    assert_eq!(second.charter_requests().await.len(), 1);
    assert_eq!(second.charter_requests().await[0].draft.mission, charter.mission);
    assert_eq!(second.promotion_requests().await[0].job, "freshness");
    let mut about = page("about");
    about["draft"] = json!("draft-1");
    let joined = call(&second, "content_draft", about).await;
    assert_eq!(joined["items"], json!(["pages/hours/en", "pages/about/en"]), "{joined}");

    // The restored draft publishes, and leaves the store; new ids follow the restored ones.
    let published = call(&second, "site_publish", json!({ "draft": "draft-1" })).await;
    assert_eq!(published["published"], true, "{published}");
    assert!(parts.host.fetch("/hours/").await.unwrap().is_some());
    assert!(parts.host.fetch("/about/").await.unwrap().is_some());
    let next = call(&second, "content_draft", page("menu")).await;
    assert_eq!(next["draft"], "draft-5", "{next}");
    let third = runtime(&parts, &drafts);
    assert_eq!(third.restore_drafts().await.unwrap(), 4, "the published draft is gone");
}
