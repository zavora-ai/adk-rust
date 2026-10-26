//! A served site end to end (spec 3.5 §7.8.3, §7.12, §7.17, §7.22, D-11, A-I5, A-I20, A-I23,
//! A-I27): the project in the layout `adk-rust site` uses, the kernel's durable stores, the
//! update engine with its journal and the owner's decision cards, and the experiments look
//! on its trigger. The tests play the owner: they make the root at a "terminal" and delegate
//! the keys the site made, as `adk-rust awp keys rotate` and `delegate` do.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::Ed25519DocumentSigner;
use adk_core::principal::SubjectRef;
use adk_core::{ApprovalDecision, DurableRunStore, InboxQuery, RunFilter, RunState};
use adk_kernel::charter::{CharterStore, CommitDecision, RoleApproval, SignalFilter, SignalStore};
use adk_site::feed::{FeedMapping, FeedRecord, FieldMapping, FieldType};
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_website::contract::{ContractInputs, SiteKeys, site_documents};
use adk_website::deploy::project::{ConfiguredFeed, KeyFile, ProjectPaths};
use adk_website::deploy::updates::RELEASE_DELIVERY;
use adk_website::deploy::{ServeOptions, Served, serve};
use adk_website::experiments::EXPERIMENTS_WORKFLOW;
use adk_website::onboarding::build::SitePlan;
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::review::ReviewInputs;
use adk_website::team::AgentRole;
use adk_website::themes::ThemeId;
use awp_types::{
    Bound, Charter, ContentId, ContentItem, ContentKind, ContentState, FeedBounds, Markdown,
    PriceRule, PublicTerms, Rate, Role, SiteKind,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const OWNER: &str = "owner";
const DOMAIN: &str = "juniper.example";

async fn project(kind: SiteKind) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let options = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: DOMAIN.into(),
        kind,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &options).await.unwrap();
    dir
}

fn charter(dir: &Path) -> Charter {
    serde_json::from_slice(&std::fs::read(dir.join("charter.draft.json")).unwrap()).unwrap()
}

/// The owner at a terminal: makes the root, delegates the release key and the card and
/// receipt key the site uses, and writes the signed contract and key set into `well-known/`.
async fn owner_signs(dir: &Path) {
    let paths = ProjectPaths::new(dir);
    let release = KeyFile::load_or_create(&paths.key_file("release"), "release-2026-10").unwrap();
    let card = KeyFile::load_or_create(&paths.key_file("card"), "card-2026-10").unwrap();
    let keys = SiteKeys {
        root: Arc::new(Ed25519DocumentSigner::from_seed("site-root-2026", [0x11; 32]).unwrap()),
        release: Arc::new(
            Ed25519DocumentSigner::from_seed(release.kid.clone(), release.seed()).unwrap(),
        ),
        edge: Arc::new(Ed25519DocumentSigner::from_seed(card.kid.clone(), card.seed()).unwrap()),
    };
    let terms: PublicTerms =
        serde_json::from_slice(&std::fs::read(dir.join("public-terms.json")).unwrap()).unwrap();
    let inputs = ContractInputs {
        id: charter(dir).site_id,
        name: "Juniper Bakery".into(),
        domain: DOMAIN.into(),
        languages: vec!["en".parse().unwrap()],
        edge: Some(format!("https://{DOMAIN}")),
        release_seq: 1,
        terms,
    };
    let from = chrono::Utc::now() - chrono::Duration::minutes(5);
    let (documents, _) = site_documents(&keys, &inputs, from).await.unwrap();
    for (path, bytes) in documents {
        if path == awp_types::v2::contract::CHARTER_PATH {
            continue;
        }
        let file = paths.well_known().join(path.trim_start_matches("/.well-known/"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn start(dir: &Path, configure: impl FnOnce(&mut ServeOptions)) -> Served {
    let mut options = ServeOptions::new(dir, OWNER);
    options.edge = loopback();
    options.kernel = loopback();
    options.site.idle = Some(Duration::from_millis(200));
    configure(&mut options);
    serve(options).await.unwrap()
}

/// One HTTP/1.1 exchange: the status and the body.
async fn http(addr: SocketAddr, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    (status, body)
}

fn product(sku: &str, price: u64) -> ContentItem {
    ContentItem {
        id: ContentId(format!("products/{sku}")),
        kind: ContentKind::Product,
        slug: sku.into(),
        language: "en".parse().unwrap(),
        fields: BTreeMap::from([
            ("title".to_string(), json!(sku.replace('-', " "))),
            ("summary".to_string(), json!("Baked this morning.")),
            ("sku".to_string(), json!(sku)),
            ("price".to_string(), json!(price)),
            ("currency".to_string(), json!("KES")),
            ("goal".to_string(), json!("sell")),
        ]),
        volatile: Vec::new(),
        body: Some(Markdown("Baked this morning.\n".into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    }
}

/// Commits the drafted charter as the owner and brings its jobs onto the ladder.
async fn commit(served: &Served, dir: &Path) -> Charter {
    let charter = charter(dir);
    let decision = CommitDecision::new("dec-1", chrono::Utc::now()).with_approval(
        RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
    );
    served.stores.charters.commit(charter.clone(), decision).await.unwrap();
    adk_website::jobs::register(&served.sites[0].ladder, &charter).await.unwrap();
    charter
}

/// Release 1 through the site's pipeline: the plan's pages and `extra`.
async fn launch(served: &Served, charter: &Charter, extra: Vec<ContentItem>) -> PipelineOutcome {
    let site = &served.sites[0];
    let mut items = SitePlan::new(charter, "Juniper Bakery").items(charter, &[], &["KE".into()]);
    items.extend(extra);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    site.runtime.pipeline().run(change).await.unwrap()
}

#[tokio::test]
async fn nothing_publishes_until_the_owner_delegates_the_release_key() {
    let dir = project(SiteKind::Business).await;
    let served = start(dir.path(), |_| {}).await;
    let paths = ProjectPaths::new(dir.path());
    assert!(paths.key_file("release").exists() && paths.key_file("card").exists());
    assert!(
        !std::fs::read_dir(paths.data().join("keys")).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("root")),
        "no root key in the project (A-I5)"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(paths.key_file("release")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let charter = commit(&served, dir.path()).await;
    let refused = launch(&served, &charter, Vec::new()).await;
    let PipelineOutcome::Held(held) = &refused else {
        panic!("an undelegated release key publishes nothing: {refused:?}")
    };
    let summary = format!("{held:?}");
    assert!(summary.contains("adk-rust awp keys delegate"), "{summary}");
    served.shutdown().await.unwrap();

    owner_signs(dir.path()).await;
    let served = start(dir.path(), |_| {}).await;
    let outcome = launch(&served, &charter, Vec::new()).await;
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "{outcome:?}");
    let (status, page) = http(served.edge_addr, "GET", "/", None).await;
    assert_eq!(status, 200);
    assert!(!page.contains("coming soon") || page.contains("Juniper Bakery"), "{page}");
    served.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_served_site_restarts_and_keeps_its_state() {
    let dir = project(SiteKind::Business).await;
    owner_signs(dir.path()).await;
    let served = start(dir.path(), |_| {}).await;
    let code = served.setup_code.clone().expect("a fresh site offers a setup code (A-I20)");
    let claim = json!({ "code": code.code, "name": "Amina's phone" });
    let (status, paired) =
        http(served.kernel_addr, "POST", "/api/kernel/pairing/claim", Some(&claim)).await;
    assert!((200..300).contains(&status), "{status} {paired}");
    let charter = commit(&served, dir.path()).await;
    assert!(matches!(launch(&served, &charter, Vec::new()).await, PipelineOutcome::Published(_)));

    // A visitor's enquiry: a lead, and a signal on the back office's agenda.
    let grant = json!({ "scope": "contact", "policyVersion": "1", "jurisdiction": "KE" });
    let (status, receipt) = http(served.edge_addr, "POST", "/awp/v2/consent", Some(&grant)).await;
    assert_eq!(status, 201, "{receipt}");
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    let enquiry =
        json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "Wedding cakes?" });
    let mut stream = tokio::net::TcpStream::connect(served.edge_addr).await.unwrap();
    let body = enquiry.to_string();
    let request = format!(
        "POST /awp/v2/capabilities/submit_inquiry HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nIdempotency-Key: e-1\r\nAWP-Consent: \"{}\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        receipt["id"].as_str().unwrap(),
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    let job = adk_website::jobs::bindings(
        &adk_website::team::SiteTeam::new(served.sites[0].site.clone(), &served.sites[0].template),
        &served.sites[0].template.jobs(),
    )
    .into_iter()
    .next()
    .expect("the kind runs jobs")
    .job;
    let level = served.sites[0].ladder.level(charter.site_id, &job).await.unwrap();
    assert!(level.is_some());
    let view = json!({ "kind": "pageview", "path": "/" });
    assert_eq!(http(served.edge_addr, "POST", "/awp/v2/beacon", Some(&view)).await.0, 204);
    served.shutdown().await.unwrap();

    let served = start(dir.path(), |_| {}).await;
    assert!(served.setup_code.is_none(), "the paired device stays paired; no second code");
    let current = served.stores.charters.current(charter.site_id).await.unwrap().unwrap();
    assert_eq!(current.version(), 1, "the committed charter survives");
    assert_eq!(served.sites[0].ladder.level(charter.site_id, &job).await.unwrap(), level);
    let signals = served.stores.signals.list(&SignalFilter::default()).await.unwrap();
    assert_eq!(signals.len(), 1, "the enquiry's signal survives");
    let (status, page) = http(served.edge_addr, "GET", "/", None).await;
    assert_eq!(status, 200);
    assert!(page.contains("Juniper Bakery"), "the release is still served");
    // The beacon's counts and the meters' counts survive in `data/edge.db`.
    let midnight = chrono::Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    let tomorrow = midnight + chrono::Duration::days(1);
    let views = served.sites[0].edge.edge.beacon().read("pageviews:/", midnight, tomorrow).await;
    assert_eq!(views.unwrap(), Some(1.0), "the page view survives");
    let pool =
        sqlx::SqlitePool::connect(&ProjectPaths::new(dir.path()).database("edge")).await.unwrap();
    let counts = adk_server::awp::stores::SqliteEdgeStores::new(pool)
        .with_site(format!("{}#measurement", served.sites[0].site.name));
    let calls = adk_server::awp::BeaconStore::total(
        &counts,
        "calls:submit_inquiry",
        midnight.date_naive(),
        tomorrow.date_naive(),
    )
    .await
    .unwrap();
    assert_eq!(calls, 1, "the enquiry's call count survives");
    let next = launch(&served, &charter, Vec::new()).await;
    assert!(
        matches!(&next, PipelineOutcome::Published(p) if p.announcement.release.seq == 2),
        "the next release follows the one the host serves: {next:?}"
    );
    served.shutdown().await.unwrap();
}

fn supplier_feed() -> ConfiguredFeed {
    ConfiguredFeed {
        mapping: FeedMapping::new("supplier", ContentKind::Product, "en".parse().unwrap(), "sku")
            .id_prefix("products/")
            .field("price", FieldMapping::new("price", FieldType::Integer)),
        bounds: FeedBounds {
            max_change_fraction: 0.5,
            price_rule: PriceRule::default(),
            field_bounds: BTreeMap::from([(
                "price".to_string(),
                Bound::Range {
                    minimum: Some(1.0),
                    exclusive_minimum: None,
                    maximum: None,
                    exclusive_maximum: None,
                },
            )]),
            max_rate: Rate { per_minute: 1_000 },
        },
    }
}

#[tokio::test]
async fn a_feed_batch_publishes_into_the_journal_and_a_held_one_becomes_a_card() {
    let dir = project(SiteKind::Ecommerce).await;
    owner_signs(dir.path()).await;
    std::fs::write(dir.path().join("feeds.json"), serde_json::to_vec(&[supplier_feed()]).unwrap())
        .unwrap();
    let served = start(dir.path(), |_| {}).await;
    let charter = commit(&served, dir.path()).await;
    let products = vec![product("rye-loaf", 450), product("sourdough", 500), product("bagel", 120)];
    let launched = launch(&served, &charter, products).await;
    assert!(matches!(launched, PipelineOutcome::Published(_)), "{launched:?}");
    let site = &served.sites[0];

    // One price within the rule: a batch, a release, a line in `site batches`' journal.
    let record =
        |sku: &str, price: u64| FeedRecord::new([("sku", json!(sku)), ("price", json!(price))]);
    let outcome = site.updates.ingest("supplier", vec![record("rye-loaf", 460)]).await.unwrap();
    assert!(!outcome.held && outcome.queued == 1, "{outcome:?}");
    site.updates.drain().await.unwrap();
    let journaled = site.driver.step().await.unwrap();
    assert!(journaled.iter().any(|r| r.release_seq == Some(2)), "{journaled:?}");
    let journal = adk_website::deploy::UpdateJournal::new(ProjectPaths::new(dir.path()).journal());
    let lines = journal.batches().unwrap();
    assert!(
        lines.iter().any(|r| r.published.contains(&ContentId("products/rye-loaf".into()))),
        "{lines:?}"
    );
    let feeds: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("data/updates/feeds.json")).unwrap())
            .unwrap();
    assert_eq!(feeds[0]["mapping"]["feed"], "supplier");
    assert_eq!(feeds[0]["deliveries"], 1);

    // Every price at once breaks the bounds: held whole, and a decision card for the owner.
    let all = vec![record("rye-loaf", 470), record("sourdough", 510), record("bagel", 125)];
    let held = site.updates.ingest("supplier", all).await.unwrap();
    assert!(held.held, "{held:?}");
    site.driver.step().await.unwrap();
    let inbox = served
        .stores
        .approvals
        .inbox(InboxQuery::for_approver(SubjectRef::human(OWNER), Vec::<String>::new()))
        .await
        .unwrap();
    let card = inbox
        .items
        .iter()
        .find(|request| request.tool_name == RELEASE_DELIVERY)
        .expect("the held delivery is a card in the owner's inbox");
    let decision = adk_kernel::channels::DecisionCard::from_request(card, None);
    assert!(decision.summary.contains("bounds held"), "{decision:?}");
    assert!(!decision.answers.is_empty(), "the owner can answer on the phone");
    served
        .stores
        .approvals
        .decide(&card.id, card.version, ApprovalDecision::approve(SubjectRef::human(OWNER)))
        .await
        .unwrap();
    site.driver.step().await.unwrap();
    site.updates.drain().await.unwrap();
    let released = site.driver.step().await.unwrap();
    assert!(released.iter().any(|r| r.published.len() == 3), "{released:?}");

    // The weekly review reports the updates (spec 3.5 §7.8.3, §7.21).
    let monday = chrono::Utc::now().date_naive() - chrono::Duration::days(7);
    let review = site.review.compose(monday, ReviewInputs::default()).await.unwrap();
    let updates = review.updates.clone().expect("the review carries the updates");
    assert_eq!((updates.deliveries, updates.held_deliveries), (2, 1), "{updates:?}");
    assert_eq!(updates.model_calls, 0, "feed updates call no model");
    assert!(review.to_markdown().contains("## Updates"));
    served.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_experiments_look_runs_on_its_trigger() {
    let dir = project(SiteKind::Business).await;
    owner_signs(dir.path()).await;
    let served = start(dir.path(), |options| {
        options.site.experiments_cron = Some("* * * * * *".into());
    })
    .await;
    let mut done = None;
    for _ in 0..80 {
        let runs = served.stores.runs.list(RunFilter::default()).await.unwrap();
        done = runs
            .into_iter()
            .find(|run| run.target == EXPERIMENTS_WORKFLOW && run.state == RunState::Completed);
        if done.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let all = served.stores.runs.list(RunFilter::default()).await.unwrap();
    let run = done.unwrap_or_else(|| panic!("the daily look ran as a kernel run: {all:#?}"));
    assert_eq!(run.input["site"], json!(served.sites[0].site.id));
    served.shutdown().await.unwrap();
}

/// Anonymous callers are limited by their own address, not one bucket every visitor shares:
/// the edge listener hands the peer address to the edge (spec 3.5 §7.3). Two loopback
/// addresses, IPv4 and IPv6, stand for two visitors.
#[tokio::test]
async fn anonymous_callers_are_limited_by_their_own_address() {
    if std::net::TcpListener::bind("[::]:0").is_err() {
        eprintln!("no IPv6 on this host; the check needs two loopback addresses");
        return;
    }
    let dir = project(SiteKind::Business).await;
    let served = start(dir.path(), |options| {
        options.edge = "[::]:0".parse().unwrap();
        options.site.limits = Some(Arc::new(|| {
            adk_server::awp::EdgeLimits::default().tier_default(
                awp_types::TrustTier::Public,
                awp_types::RateClass { requests_per_minute: 1, calls_per_day: 10 },
            )
        }));
    })
    .await;
    let port = served.edge_addr.port();
    let v4 = SocketAddr::from(([127, 0, 0, 1], port));
    let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port));
    assert_eq!(http(v4, "GET", "/awp/v2/manifest", None).await.0, 200);
    assert_eq!(http(v4, "GET", "/awp/v2/manifest", None).await.0, 429, "one a minute");
    assert_eq!(http(v6, "GET", "/awp/v2/manifest", None).await.0, 200, "another visitor's own");
    served.shutdown().await.unwrap();
}
