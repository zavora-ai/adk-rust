//! A site's release chain across replicas (#1280): two site runtimes, each opening its own
//! pool over one database, as two served processes over one store do, share the drafts, the
//! approval requests and the charters. Ids stay distinct, one of two votes on one version
//! wins, a vote survives a restart and a new preview, a release publishes once, and a
//! charter commits once.
//!
//! The content store, host and site log stand in for the site's shared files. SQLite runs on
//! every pull request; PostgreSQL is `#[ignore]`d and runs in CI's postgres job
//! (`scripts/test-postgres.sh`):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --features website,postgres --run-ignored only \
//!   --test integration -E 'test(/^website_replicas_tests::/)'
//! ```

#![cfg(feature = "website")]

use crate::website_common::{self, OWNER, PUBLISHER};

use std::sync::Arc;

use adk_core::audit_log::InMemoryAuditLogStore;
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalService, ApprovalState, ApprovalStore, GovernanceControl, StoreApprovalService,
};
use adk_harness::website::{SiteDraftStore, SiteRef, SiteRuntime, back_office_tools};
use adk_kernel::charter::{CharterDirectory, CharterStore, CommitDecision, RoleApproval};
use adk_site::content::{ContentStore, FsContentStore, InMemoryContentStore, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, SiteHost as _};
use adk_site::log::SiteLog;
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::Role;
use serde_json::{Value, json};

/// What one replica opens over the shared database.
struct Stores {
    drafts: Arc<dyn SiteDraftStore>,
    approvals: Arc<dyn ApprovalStore>,
    charters: Arc<dyn CharterStore>,
}

/// The site's shared files: its content, host and log.
struct Shared {
    site: SiteRef,
    base: SiteProject,
    store: Arc<InMemoryContentStore>,
    host: Arc<DirectoryHost>,
    log: Arc<SiteLog>,
    _host_dir: tempfile::TempDir,
}

async fn shared() -> Shared {
    let site = SiteRef::new(uuid::Uuid::new_v4(), "bakery", "https://bakery.example");
    let fixture = website_common::fixture_dir();
    let mut base = SiteProject::load(&fixture, chrono::Utc::now()).await.unwrap();
    base.items.clear();
    let store = Arc::new(InMemoryContentStore::default());
    for item in FsContentStore::new(&fixture).list().await.unwrap() {
        store.put(&item).await.unwrap();
    }
    let host_dir = tempfile::tempdir().unwrap();
    let signer = Arc::new(Ed25519ReleaseSigner::from_seed("bakery.example", [7; 32]).unwrap());
    let log = Arc::new(
        SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), "bakery.example", signer)
            .await
            .unwrap(),
    );
    Shared {
        site,
        base,
        store,
        host: Arc::new(DirectoryHost::new(host_dir.path())),
        log,
        _host_dir: host_dir,
    }
}

/// A replica: a runtime over `stores`, building in a work directory of its own, its
/// approval service taking roles from the charters.
fn replica(shared: &Shared, stores: Stores, work: &std::path::Path) -> Arc<SiteRuntime> {
    let directory = CharterDirectory::new(stores.charters.clone()).for_site(shared.site.id);
    let service = StoreApprovalService::new(stores.approvals).with_directory(Arc::new(directory));
    SiteRuntime::builder(
        shared.site.clone(),
        shared.base.clone(),
        shared.store.clone(),
        Arc::new(BuiltinGenerator),
        shared.host.clone(),
        shared.log.clone(),
        work,
    )
    .drafts(stores.drafts)
    .approvals(Arc::new(service) as Arc<dyn ApprovalService>)
    .charters(stores.charters)
    .governance(GovernanceControl::new())
    .require_durable()
    .build()
    .unwrap()
}

async fn call(runtime: &Arc<SiteRuntime>, name: &str, args: Value) -> adk_core::Result<Value> {
    let tool = back_office_tools(runtime)
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no back-office tool {name}"));
    tool.execute(Arc::new(adk_tool::SimpleToolContext::new("steward")), args).await
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

/// The release chain and a charter signature across two replicas over the stores `open`
/// returns, each call a new handle on one database.
async fn release_chain_across_replicas<F, Fut>(open: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Stores>,
{
    let shared = shared().await;
    let (work_a, work_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let now = chrono::Utc::now();
    let first = open().await;
    first
        .charters
        .commit(
            website_common::charter(&shared.site, true),
            CommitDecision::new("dec-charter-1", now).with_approval(RoleApproval::new(
                Role::Owner,
                SubjectRef::human(OWNER),
                now,
            )),
        )
        .await
        .unwrap();
    let a = replica(&shared, first, work_a.path());
    let b = replica(&shared, open().await, work_b.path());

    // Drafts made at once on both replicas get distinct ids.
    let (hours, about) = tokio::join!(
        call(&a, "content_draft", page("hours")),
        call(&b, "content_draft", page("about"))
    );
    let (hours, about) = (hours.unwrap()["draft"].clone(), about.unwrap()["draft"].clone());
    let mut ids = [hours.clone(), about];
    ids.sort_by_key(ToString::to_string);
    assert_eq!(ids, [json!("draft-1"), json!("draft-2")]);
    let first_page = if hours == "draft-1" { "/hours/" } else { "/about/" };

    // The release waits for its chain; two votes on one version, one on each replica: one
    // wins, the other is told the request moved on.
    let pending = call(&a, "site_publish", json!({ "draft": "draft-1" })).await.unwrap();
    let ticket = pending["pending"].as_str().expect("the chain holds it").to_string();
    let version = a.approvals().get(&ticket).await.unwrap().unwrap().version;
    let (amina, baraka) = (SubjectRef::human(OWNER), SubjectRef::human(PUBLISHER));
    let (owner, publisher) = tokio::join!(
        a.approvals().approve(&ticket, version, &amina),
        b.approvals().approve(&ticket, version, &baraka),
    );
    let (won, lost) = match (owner, publisher) {
        (Ok(won), Err(lost)) => (won, (lost, PUBLISHER)),
        (Err(lost), Ok(won)) => (won, (lost, OWNER)),
        other => panic!("exactly one vote wins: {other:?}"),
    };
    assert_eq!(lost.0.code, "approval.version_conflict");
    assert_eq!(won.state, ApprovalState::Pending, "one role is not the chain");

    // A restarted replica previews the draft again and reaches the same request; the loser
    // votes again there.
    drop(a);
    let a = replica(&shared, open().await, work_a.path());
    let again = call(&a, "site_publish", json!({ "draft": "draft-1" })).await.unwrap();
    assert_eq!(again["pending"], ticket.as_str(), "{again}");
    let current = a.approvals().get(&ticket).await.unwrap().unwrap();
    let approved =
        a.approvals().approve(&ticket, current.version, &SubjectRef::human(lost.1)).await.unwrap();
    assert_eq!(approved.state, ApprovalState::Approved);

    // Both replicas publish the approved draft at once: it is released once.
    let (left, right) = tokio::join!(
        call(&a, "site_publish", json!({ "draft": "draft-1" })),
        call(&b, "site_publish", json!({ "draft": "draft-1" })),
    );
    let published: Vec<&Value> = [&left, &right]
        .into_iter()
        .filter_map(|r| r.as_ref().ok())
        .filter(|v| v["published"] == true)
        .collect();
    assert_eq!(published.len(), 1, "one release: {left:?} {right:?}");
    assert_eq!(published[0]["approvers"], json!([OWNER, PUBLISHER]));
    assert!(shared.host.fetch(first_page).await.unwrap().is_some());

    // A charter signing request asked on one replica and signed on the other commits once.
    let mut next = website_common::charter(&shared.site, true);
    next.mission = "Fresh bread and coffee for Nakuru every morning.".into();
    let request = a.request_charter(&next, None, "urn:adk:agent:bakery/steward").await.unwrap();
    let owner = RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), now);
    let committed = b.sign_charter(&request.id, owner.clone()).await.unwrap().expect("one owner");
    assert_eq!(committed.version(), 2);
    assert!(a.sign_charter(&request.id, owner).await.is_err(), "the request is decided");
    assert!(a.settle_charters().await.unwrap().is_empty(), "nothing commits twice");
    let history = open().await.charters.history(shared.site.id).await.unwrap();
    let versions: Vec<u32> = history.iter().map(|version| version.version()).collect();
    assert_eq!(versions, [1, 2]);
    assert_eq!(history[1].decision.decision_id, request.id, "the commit names the request");
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_release_chain_holds_across_replicas_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("edge.db").display());
    release_chain_across_replicas(|| async {
        let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
        let drafts = adk_harness::website::SqliteSiteDraftStore::new(pool.clone());
        drafts.migrate().await.unwrap();
        let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
        approvals.migrate().await.unwrap();
        let charters = adk_kernel::charter::SqliteCharterStore::new(pool);
        charters.migrate().await.unwrap();
        Stores {
            drafts: Arc::new(drafts),
            approvals: Arc::new(approvals),
            charters: Arc::new(charters),
        }
    })
    .await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs PostgreSQL at DATABASE_URL"]
async fn the_release_chain_holds_across_replicas_on_postgres() {
    use std::str::FromStr;

    use sqlx::Connection as _;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    let schema = format!("adk_website_replicas_{}", uuid::Uuid::new_v4().simple());
    let mut admin = sqlx::PgConnection::connect(&url).await.expect("connect");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await
        .expect("create schema");
    let options = PgConnectOptions::from_str(&url)
        .expect("parse DATABASE_URL")
        .options([("search_path", schema.as_str())]);
    release_chain_across_replicas(|| async {
        let pool =
            PgPoolOptions::new().max_connections(4).connect_with(options.clone()).await.unwrap();
        let drafts = adk_harness::website::PostgresSiteDraftStore::new(pool.clone());
        drafts.migrate().await.unwrap();
        let approvals = adk_session::PostgresApprovalStore::from_pool(pool.clone());
        approvals.migrate().await.unwrap();
        let charters = adk_kernel::charter::PostgresCharterStore::new(pool);
        charters.migrate().await.unwrap();
        Stores {
            drafts: Arc::new(drafts),
            approvals: Arc::new(approvals),
            charters: Arc::new(charters),
        }
    })
    .await;
}
