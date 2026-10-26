//! Sources (spec 3.5 §7.8 "Sources", §7.18, A-I9, A-I19, S2, S3): each source inventories the
//! recorded WordPress site or presence, passes the source conformance suite, and keeps to its
//! limits: robots, signing, proven domains, read-scoped credentials, loopback-only replays.
#![cfg(feature = "migrate")]

mod support;

use std::sync::Arc;

use adk_site::gate::{ClaimCheck, FactStatus};
use adk_site::migrate::sources::{
    Crawl, IsolatedReplay, Presence, ReplaySpec, WordPressRest, Wxr, source_conformance,
};
use adk_site::migrate::{FeatureKind, ItemStatus, MigrateError, SiteSource, SourceKind};
use awp_types::{ContentId, ContentItem, ContentKind, ContentState, FactRef};
use support::*;
use url::Url;

fn origin() -> Url {
    Url::parse(ORIGIN).unwrap()
}

#[tokio::test]
async fn wordpress_rest_reads_public_collections() {
    let server = FixtureServer::start(false).await;
    let ctx = context(&server);
    let inventory = source_conformance(&WordPressRest::new(origin()), &ctx).await;
    assert_eq!(inventory.source, SourceKind::WordPressRest);
    assert_eq!(inventory.site_name.as_deref(), Some("Example Solar"));
    let mut keys: Vec<&str> = inventory.items.iter().map(|i| i.key.as_str()).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["wp:page:2", "wp:page:4", "wp:page:5", "wp:post:7", "wp:post:9", "wp:product:21"]
    );
    let hello = inventory.item("wp:post:7").unwrap();
    assert_eq!(hello.path, "/2026/10/01/hello-sun/");
    assert_eq!(hello.categories, ["News"]);
    assert_eq!(hello.tags, ["Solar"]);
    assert_eq!(hello.author.as_deref(), Some("Amina"));
    let product = inventory.item("wp:product:21").unwrap();
    assert_eq!(product.fields["price"], serde_json::json!(25000.0));
    assert_eq!(product.fields["currency"], "KES");
    assert!(inventory.media.iter().all(|m| m.bytes.is_some()), "media bytes read");
    for kind in
        [FeatureKind::ContactForm, FeatureKind::Shop, FeatureKind::Comments, FeatureKind::Search]
    {
        assert!(inventory.has_feature(kind), "{kind:?}");
    }
    assert!(
        inventory.items.iter().all(|i| i.status == ItemStatus::Published),
        "no drafts without a credential"
    );
    assert!(server.seen().iter().all(|s| s.signed), "every request is signed");
}

#[tokio::test]
async fn drafts_need_a_proven_domain_and_a_read_only_credential() {
    let server = FixtureServer::start(false).await;
    let read = Arc::new(TestBroker { scopes: vec!["read".into()] });
    let source = WordPressRest::new(origin()).with_drafts(read.clone());
    let refused = source.inventory(&context(&server)).await.unwrap_err();
    assert!(matches!(refused, MigrateError::NotProven { .. }), "{refused}");

    let ctx = proven_context(&server).await;
    let inventory = source.inventory(&ctx).await.unwrap();
    let draft = inventory.item("wp:post:11").unwrap();
    assert_eq!(draft.status, ItemStatus::Draft);
    assert!(inventory.urls.iter().all(|u| u.path != draft.path), "a draft has no public URL");
    assert_eq!(inventory.menus[0].entries.len(), 3, "menus read with the credential");

    let broad = Arc::new(TestBroker { scopes: vec!["read".into(), "write".into()] });
    let refused =
        WordPressRest::new(origin()).with_drafts(broad).inventory(&ctx).await.unwrap_err();
    assert!(matches!(refused, MigrateError::Credential(_)), "{refused}");
}

#[tokio::test]
async fn wxr_reads_the_export_and_every_upload() {
    let server = FixtureServer::start(false).await;
    let ctx = context(&server);
    let export = std::fs::read(fixtures().join("export.xml")).unwrap();
    let uploads = uploads(true);
    let source = Wxr::from_bytes(export).with_uploads(uploads.path()).with_currency("KES");
    let inventory = source_conformance(&source, &ctx).await;
    assert_eq!(server.requests(), 0, "an export is read without a request");
    assert_eq!(inventory.origin.as_deref(), Some(ORIGIN));
    let contact = inventory.items.iter().find(|i| i.slug == "contact").unwrap();
    assert!(contact.html.contains("[contact-form-7"));
    let draft = inventory.items.iter().find(|i| i.slug == "choosing-an-inverter").unwrap();
    assert_eq!(draft.status, ItemStatus::Draft);
    let product = inventory.items.iter().find(|i| i.kind == ContentKind::Product).unwrap();
    assert_eq!(product.fields["sku"], "P-400");
    assert_eq!(product.fields["currency"], "KES");
    for kind in [FeatureKind::ContactForm, FeatureKind::Shop, FeatureKind::Comments] {
        assert!(inventory.has_feature(kind), "{kind:?}");
    }
    let menu = &inventory.menus[0];
    let targets: Vec<&str> = menu.entries.iter().map(|e| e.target.as_str()).collect();
    assert_eq!(targets, ["/", "/about/", "/contact/", "/category/news/", "https://partner.test/"]);
    let planted = inventory.media.iter().find(|m| m.path.ends_with("cache.php")).unwrap();
    assert!(planted.bytes.is_some(), "a file no attachment names is still read");
    let team = inventory.media.iter().find(|m| m.path.ends_with("team.png")).unwrap();
    assert_eq!(team.alt.as_deref(), Some("The installation team on a roof"));
}

#[tokio::test]
async fn crawl_respects_robots_signs_and_records_redirects() {
    let server = FixtureServer::start(false).await;
    let ctx = context(&server);
    let inventory = source_conformance(&Crawl::new(origin()), &ctx).await;
    let seen = server.seen();
    assert!(seen.iter().all(|s| s.signed), "every crawl request is signed");
    assert!(seen.iter().all(|s| s.user_agent.starts_with("adk-site/")));
    assert_eq!(seen[0].path, "/robots.txt", "robots.txt is read first");
    assert!(
        seen.iter().all(|s| !s.path.starts_with("/private/")),
        "a disallowed path is never requested"
    );
    assert!(inventory.notes.iter().any(|n| n.contains("/private/") && n.contains("robots")));
    let redirect = inventory.url("/old-offers/").unwrap();
    assert_eq!(
        (redirect.status, redirect.redirect_to.as_deref()),
        (301, Some("/products/panel-400/"))
    );
    let about = inventory.url("/about/").unwrap();
    let page = about.page.as_ref().unwrap();
    assert_eq!(page.title.as_deref(), Some("About us – Example Solar"));
    assert!(page.main_text.starts_with("We are a team of eight engineers"), "{}", page.main_text);
    assert_eq!(about.item.as_deref(), Some("crawl:/about/"));
    let hello = inventory.item("crawl:/2026/10/01/hello-sun/").unwrap();
    assert_eq!(hello.kind, ContentKind::Post);
    assert_eq!(hello.author.as_deref(), Some("Amina"));
    let product = inventory.item("crawl:/products/panel-400/").unwrap();
    assert_eq!(product.kind, ContentKind::Product);
    assert_eq!(product.fields["sku"], "P-400");
    assert!(inventory.url("/blog/").unwrap().item.is_none(), "a listing is not an item");
    assert!(inventory.item("crawl:/wp-login.php").is_none());
    assert_eq!(inventory.taxonomies[0].terms[0].name, "News");
    assert_eq!(inventory.menus[0].entries.len(), 5);
    for kind in [
        FeatureKind::ContactForm,
        FeatureKind::Search,
        FeatureKind::Comments,
        FeatureKind::Shop,
        FeatureKind::Login,
    ] {
        assert!(inventory.has_feature(kind), "{kind:?}");
    }
    assert_eq!(inventory.top_pages(1)[0].path, "/", "the home page ranks first");
}

#[tokio::test]
async fn presence_lists_every_fact_with_its_source() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("site")).unwrap();
    copy_tree(&fixtures().join("presence"), &dir.path().join("site"));
    let server = FixtureServer::start_at(dir.path().to_path_buf(), false).await;
    let ctx = context(&server);
    let ctx = adk_site::migrate::SourceContext {
        http: Arc::new(
            adk_site::migrate::http::HttpClient::builder()
                .egress(adk_site::migrate::http::EgressScope::Only([server.origin.clone()].into()))
                .min_interval(std::time::Duration::ZERO)
                .build()
                .unwrap(),
        ),
        ..ctx
    };
    let listing = Url::parse(&format!("{}/listing.html", server.origin)).unwrap();
    let social = Url::parse(&format!("{}/social.html", server.origin)).unwrap();
    let inventory =
        source_conformance(&Presence::new([listing.clone(), social.clone()]), &ctx).await;
    let fact = |key: &str| inventory.facts.iter().find(|f| f.key == key).unwrap();
    assert_eq!(fact("telephone").value, "+254 700 000 000");
    assert_eq!(fact("telephone").source.url, listing.as_str());
    assert_eq!(fact("telephone").source.locator, "json-ld LocalBusiness.telephone");
    assert_eq!(fact("address").value, "12 Ngong Road, Nairobi, KE");
    assert!(fact("openingHours").value.contains("08:00-17:00"));
    assert_eq!(fact("aggregateRating").value, "4.7 from 31 reviews");
    let from_social: Vec<&str> = inventory
        .facts
        .iter()
        .filter(|f| f.source.url == social.as_str())
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(from_social, ["name", "description"]);
    assert!(inventory.facts.iter().all(|f| f.status == FactStatus::Assumed));

    // S2: a review is quoted only with a public attribution and the owner's approval.
    assert_eq!(inventory.reviews.len(), 2);
    let mut attributed = inventory.reviews[0].clone();
    assert_eq!(attributed.author.as_deref(), Some("Wanjiru K."));
    assert!(!attributed.usable());
    attributed.approved_by = Some("decision-7".into());
    assert!(attributed.usable());
    let mut anonymous = inventory.reviews[1].clone();
    anonymous.approved_by = Some("decision-8".into());
    assert!(!anonymous.usable(), "no public attribution, no quote");

    // S2: a page citing a found fact fails the claim check until the owner confirms it.
    let page = ContentItem {
        id: ContentId("pages/contact/en".into()),
        kind: ContentKind::Page,
        slug: "contact".into(),
        language: "en".parse().unwrap(),
        fields: Default::default(),
        volatile: Vec::new(),
        body: None,
        media: Vec::new(),
        sources: vec![FactRef(fact("telephone").id.clone())],
        schedule: None,
        state: ContentState::Published,
    };
    let unsupported = inventory.fact_table().unsupported_claims(&page).await.unwrap();
    assert_eq!(unsupported.len(), 1);
    assert!(unsupported[0].reason.contains("Assumed"));
    assert_eq!(fact("@type").value, "LocalBusiness", "the subject's type is a fact");
}

/// A listing typed with a schema.org subtype — `Bakery` under `FoodEstablishment` under
/// `LocalBusiness` — yields its facts and its type (spec 3.5 §7.8 `Presence`, S2).
#[tokio::test]
async fn presence_reads_schema_org_subtypes_and_records_the_type() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(
        site.join("bakery.html"),
        r#"<!doctype html><html><head><title>Juniper Bakery</title>
<script type="application/ld+json">
{"@context":"https://schema.org","@graph":[
 {"@type":"WebSite","name":"Listings"},
 {"@type":"Organization","name":"Listings Ltd"},
 {"@type":["LocalBusiness","Bakery"],"name":"Juniper Bakery","telephone":"+254 700 000 001"}]}
</script></head><body><h1>Juniper Bakery</h1></body></html>"#,
    )
    .unwrap();
    let server = FixtureServer::start_at(dir.path().to_path_buf(), false).await;
    let ctx = adk_site::migrate::SourceContext {
        http: Arc::new(
            adk_site::migrate::http::HttpClient::builder()
                .egress(adk_site::migrate::http::EgressScope::Only([server.origin.clone()].into()))
                .min_interval(std::time::Duration::ZERO)
                .build()
                .unwrap(),
        ),
        ..context(&server)
    };
    let listing = Url::parse(&format!("{}/bakery.html", server.origin)).unwrap();
    let inventory = source_conformance(&Presence::new([listing]), &ctx).await;
    let fact = |key: &str| inventory.facts.iter().find(|f| f.key == key).unwrap();
    assert_eq!(fact("@type").value, "Bakery", "the most specific type: {:#?}", inventory.facts);
    assert_eq!(fact("name").value, "Juniper Bakery", "the bakery, not the listing site");
    assert_eq!(fact("telephone").source.locator, "json-ld Bakery.telephone");
}

#[tokio::test]
async fn isolated_replay_crawls_on_loopback_only_and_is_destroyed() {
    let driver = Arc::new(FakeDriver::new(true));
    let dump_dir = tempfile::tempdir().unwrap();
    let dump = dump_dir.path().join("dump.sql");
    std::fs::write(&dump, "-- recorded dump\n").unwrap();
    let uploads = uploads(true);
    let spec = ReplaySpec::wordpress(ORIGIN, &dump, uploads.path());
    let replay = IsolatedReplay::new(driver.clone(), spec);

    let server = FixtureServer::start(false).await;
    let refused = replay.inventory(&context(&server)).await.unwrap_err();
    assert!(matches!(refused, MigrateError::NotProven { .. }), "{refused}");
    assert!(
        driver.started.lock().unwrap().is_empty(),
        "nothing starts before the domain is proven"
    );

    let ctx = proven_context(&server).await;
    let inventory = source_conformance(&replay, &ctx).await;
    assert_eq!(inventory.source, SourceKind::IsolatedReplay);
    assert!(inventory.item("crawl:/about/").is_some());
    assert!(inventory.media.iter().any(|m| m.path == "/wp-content/uploads/2026/10/cache.php"));
    let report = replay.last_report().unwrap();
    assert!(report.outbound_connections.is_empty(), "{:?}", report.outbound_connections);
    assert!(report.connections.iter().all(|c| c.loopback && c.origin == report.origin));
    assert!(report.egress_blocked);
    assert!(report.destroyed);
    assert_eq!(driver.destroyed.lock().unwrap().len(), 1, "destroyed after the run");
    assert_eq!(server.requests(), 0, "the public client is never used");
    assert!(
        dump.exists() && uploads.path().join("2026/10/cache.php").exists(),
        "legacy files are kept (A-I10)"
    );
}

#[tokio::test]
async fn the_replay_client_reaches_nothing_but_loopback() {
    let server = FixtureServer::start(false).await;
    let client = client(&server);
    for target in [
        "https://cdn-stats.test/c.js",
        "http://10.0.0.5/",
        "http://169.254.169.254/latest/meta-data",
    ] {
        let error = client
            .fetch(&Url::parse(target).unwrap(), &adk_site::migrate::http::FetchOptions::crawler())
            .await
            .unwrap_err();
        assert!(matches!(error, MigrateError::Egress { .. }), "{target}: {error}");
    }
    assert!(client.outbound_connections().is_empty());
    assert_eq!(client.connections().iter().filter(|c| !c.allowed).count(), 3);
    let public = adk_site::migrate::http::HttpClient::builder().build().unwrap();
    for target in ["http://127.0.0.1:9/", "http://localhost/", "http://192.168.1.1/"] {
        let error = public
            .fetch(&Url::parse(target).unwrap(), &adk_site::migrate::http::FetchOptions::crawler())
            .await
            .unwrap_err();
        assert!(
            matches!(error, MigrateError::Egress { .. }),
            "public egress refuses {target}: {error}"
        );
    }
}

/// The Docker driver against a real daemon: needs Docker and the WordPress, MariaDB and socat
/// images. Run with `--ignored` where Docker runs.
#[tokio::test]
#[ignore = "needs a running Docker daemon"]
async fn docker_replay_is_isolated_and_destroyed() {
    use adk_site::migrate::sources::DockerCliDriver;
    let dump_dir = tempfile::tempdir().unwrap();
    let dump = dump_dir.path().join("dump.sql");
    std::fs::write(&dump, "-- empty dump: WordPress installs itself\n").unwrap();
    let uploads = uploads(true);
    let driver = Arc::new(DockerCliDriver::default());
    let replay = IsolatedReplay::new(driver, ReplaySpec::wordpress(ORIGIN, &dump, uploads.path()));
    let server = FixtureServer::start(false).await;
    let ctx = proven_context(&server).await;
    let result = replay.inventory(&ctx).await;
    let report = replay.last_report().expect("a report even on failure");
    assert!(report.destroyed, "containers and network removed");
    assert!(report.egress_blocked, "the network is internal");
    assert!(report.outbound_connections.is_empty());
    result.unwrap();
}
