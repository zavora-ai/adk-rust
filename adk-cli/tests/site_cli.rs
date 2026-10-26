//! `adk-rust site` (spec 3.5 §8.1) against a site project on `DirectoryHost`: drafting,
//! building, publishing, corrections, withdrawals, schedules, per-item revert, batches,
//! rollback, the log, feeds and migration from a WordPress export.

#![cfg(feature = "website")]

use std::path::Path;
use std::process::{Command, Output};

use adk_site::host::{DirectoryHost, SiteHost as _};
use serde_json::Value;

fn adk_rust(args: &[&str], home: &Path, site: &Path) -> Output {
    let mut all: Vec<&str> = args.to_vec();
    let site = site.display().to_string();
    if args.first() == Some(&"site") {
        all.extend(["--site", &site]);
    }
    Command::new(env!("CARGO_BIN_EXE_adk-rust"))
        .args(&all)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USER", "ana")
        .env("RUST_LOG", "off")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("adk-rust runs")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[track_caller]
fn ok(output: Output) -> String {
    let printed = text(&output);
    assert!(output.status.success(), "{printed}");
    printed
}

/// A site project with the legal pages the launch gate requires.
fn project(dir: &Path) {
    std::fs::write(
        dir.join("site.toml"),
        "name = \"Example Solar\"\ndomain = \"example.com\"\nlanguages = [\"en\"]\n",
    )
    .unwrap();
    for page in ["privacy", "consent", "terms"] {
        let folder = dir.join("content/pages").join(page);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("en.md"),
            format!(
                "---\ntitle: {page}\nlegal: {page}\nstate: published\n---\nOur {page} policy.\n"
            ),
        )
        .unwrap();
    }
}

async fn served(site: &Path, path: &str) -> Option<String> {
    DirectoryHost::new(site.join("data/host"))
        .fetch(path)
        .await
        .unwrap()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The JSON a command prints on standard output.
#[track_caller]
fn json(output: Output) -> Value {
    assert!(output.status.success(), "{}", text(&output));
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not JSON:\n{}", text(&output)))
}

fn log(home: &Path, site: &Path) -> Value {
    json(adk_rust(&["site", "log", "--json"], home, site))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_site_is_drafted_published_corrected_reverted_withdrawn_and_rolled_back() {
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    let (home, site) = (home.path(), site.path());
    std::fs::write(
        site.join("site.toml"),
        "name = \"Example Solar\"\ndomain = \"example.com\"\nlanguages = [\"en\"]\n",
    )
    .unwrap();

    // The launch gate's full tier holds a site without its legal pages.
    let output = adk_rust(&["site", "build"], home, site);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("legal-page-missing"), "{}", text(&output));
    project(site);
    let built = ok(adk_rust(&["site", "build"], home, site));
    assert!(built.contains("Launch gate (Full tier): passed"), "{built}");

    // Without a terminal, publishing needs --yes.
    let output = adk_rust(&["site", "publish"], home, site);
    assert!(!output.status.success());
    assert!(text(&output).contains("pass --yes"), "{}", text(&output));
    assert_eq!(log(home, site)["size"], 0);

    let first = ok(adk_rust(&["site", "publish", "--yes"], home, site));
    assert!(first.contains("Published release 1 (snapshot)"), "{first}");
    assert!(first.contains("no AWP key set yet"), "{first}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let key = std::fs::metadata(site.join("data/keys/release.jwk")).unwrap();
        assert_eq!(key.permissions().mode() & 0o777, 0o600, "the release key is the owner's alone");
    }
    assert!(served(site, "/privacy/index.html").await.unwrap().contains("Our privacy policy"));
    let again = ok(adk_rust(&["site", "publish", "--yes"], home, site));
    assert!(again.contains("Nothing changed since release 1"), "{again}");

    // A draft is written to the content repository and published by id.
    let drafted =
        ok(adk_rust(&["site", "content", "new", "post", "--title", "Hello world"], home, site));
    assert!(drafted.contains("Drafted posts/hello-world/en"), "{drafted}");
    assert!(site.join("content/posts/hello-world/en.md").exists());
    let body = site.join("content/posts/hello-world/en.md");
    let text_now =
        std::fs::read_to_string(&body).unwrap().replace("Write the post here.", "Panels are in.");
    std::fs::write(&body, text_now).unwrap();
    assert!(served(site, "/posts/hello-world/index.html").await.is_none(), "a draft is not live");
    ok(adk_rust(&["site", "publish", "posts/hello-world/en", "--yes"], home, site));
    assert!(
        served(site, "/posts/hello-world/index.html").await.unwrap().contains("Panels are in.")
    );

    // A correction shows its note; a revert restores the earlier version and keeps the rest.
    ok(adk_rust(
        &[
            "site",
            "correct",
            "posts/hello-world/en",
            "--note",
            "The date was wrong",
            "--set",
            "title=Hello, world",
            "--yes",
        ],
        home,
        site,
    ));
    let corrected = served(site, "/posts/hello-world/index.html").await.unwrap();
    assert!(
        corrected.contains("The date was wrong") && corrected.contains("Hello, world"),
        "{corrected}"
    );
    let reverted = ok(adk_rust(&["site", "revert", "posts/hello-world/en", "--yes"], home, site));
    assert!(reverted.contains("Published release"), "{reverted}");
    let page = served(site, "/posts/hello-world/index.html").await.unwrap();
    assert!(!page.contains("The date was wrong"), "{page}");
    assert!(served(site, "/privacy/index.html").await.is_some(), "every other change stays");
    let batches = json(adk_rust(&["site", "batches", "--json"], home, site));
    let batch = &batches.as_array().unwrap()[0];
    assert_eq!(batch["published"][0], "posts/hello-world/en");
    assert_eq!(batch["modelCalls"], 0);

    // A withdrawal replaces the page with a notice; a schedule waits for its time.
    ok(adk_rust(
        &["site", "withdraw", "posts/hello-world/en", "--notice", "We moved this post", "--yes"],
        home,
        site,
    ));
    assert!(
        served(site, "/posts/hello-world/index.html").await.unwrap().contains("We moved this post")
    );
    ok(adk_rust(&["site", "content", "new", "page", "--title", "Opening hours"], home, site));
    let scheduled = ok(adk_rust(
        &["site", "schedule", "pages/opening-hours/en", "2030-01-01T09:00:00Z", "--yes"],
        home,
        site,
    ));
    assert!(scheduled.contains("publishes at 2030-01-01T09:00:00"), "{scheduled}");
    assert!(served(site, "/opening-hours/index.html").await.is_none());

    // The log lists every release; a rollback serves an earlier release's files again and
    // lists what changed since.
    let before = log(home, site);
    let size = before["size"].as_u64().unwrap();
    assert!(size >= 5, "{before:#}");
    assert_eq!(before["releases"][0]["snapshot"], true);
    let rolled = ok(adk_rust(&["site", "rollback", "1", "--replay", "none", "--yes"], home, site));
    assert!(
        rolled.contains(&format!("release {} serves the files of release 1", size + 1)),
        "{rolled}"
    );
    assert!(rolled.contains("posts/hello-world/en"), "{rolled}");
    assert!(served(site, "/posts/hello-world/index.html").await.is_none());
    let after = log(home, site);
    assert_eq!(after["size"], size + 1);
    assert_eq!(after["releases"][size as usize]["source"]["kind"], "rollback");
}

/// A charter whose approval chain needs an editor and a publisher refuses a lone publish
/// (spec 3.5 A-I23).
#[test]
fn an_approval_chain_refuses_a_lone_publish() {
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    project(site.path());
    // A draft charter from a 1.x configuration, given a two-person chain for publication.
    let drafts = home.path().join("drafts");
    let business = home.path().join("business.toml");
    let context = awp_types::BusinessContext::core("Example Solar", "Solar", "example.com");
    std::fs::write(&business, adk_awp::business_context_to_toml(&context).unwrap()).unwrap();
    ok(adk_rust(
        &["awp", "migrate-config", business.to_str().unwrap(), "--out", drafts.to_str().unwrap()],
        home.path(),
        site.path(),
    ));
    let mut charter: Value =
        serde_json::from_slice(&std::fs::read(drafts.join("charter.draft.json")).unwrap()).unwrap();
    charter["governance"]["approvalChains"] =
        serde_json::json!([{ "action": "publish", "roles": ["editor", "publisher"] }]);
    std::fs::write(site.path().join("charter.draft.json"), charter.to_string()).unwrap();
    let output = adk_rust(&["site", "publish", "--yes"], home.path(), site.path());
    assert!(!output.status.success());
    assert!(text(&output).contains("editor and publisher"), "{}", text(&output));
    assert!(!site.path().join("data/host").exists(), "nothing was published");
}

/// A WordPress export becomes content with redirects and capability drafts; its server code
/// is quarantined and the export is archived encrypted (spec 3.5 §7.8, A-I1, A-I10).
#[test]
fn migrate_from_a_wordpress_export() {
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    std::fs::write(
        site.path().join("site.toml"),
        "name = \"Writer\"\ndomain = \"writer.example\"\nlanguages = [\"en\"]\n",
    )
    .unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../adk-site/tests/fixtures/migrate");
    let export = fixtures.join("export.xml");
    let uploads = fixtures.join("infected/wp-content/uploads");
    let printed = ok(adk_rust(
        &[
            "site",
            "migrate",
            "--wxr",
            export.to_str().unwrap(),
            "--uploads",
            uploads.to_str().unwrap(),
        ],
        home.path(),
        site.path(),
    ));
    assert!(printed.contains("Archived the legacy export export.xml encrypted"), "{printed}");
    assert!(printed.contains("cache.php — executable"), "{printed}");
    assert!(printed.contains("migration.comments"), "{printed}");
    let report: Value = serde_json::from_slice(
        &std::fs::read(site.path().join("data/migration/report.json")).unwrap(),
    )
    .unwrap();
    assert!(report["items"].as_u64().unwrap() > 0);
    // The owner's design choice (spec 3.5 §7.8.1): keeping the design by default.
    assert_eq!(report["design"]["design"], "keep", "{report}");
    assert!(printed.contains("Design: kept"), "{printed}");
    let config = std::fs::read_to_string(site.path().join("site.toml")).unwrap();
    assert!(config.contains("[[redirects]]"), "{config}");
    assert!(site.path().join("capabilities.json").exists());
    assert!(!site.path().join("static/wp-content/uploads/2026/10/cache.php").exists());
    let archive = site.path().join("data/archive");
    assert!(archive.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(site.path().join("data/keys/archive.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A second run keeps what the project holds; a new theme gates text and metadata only.
    let again = ok(adk_rust(
        &["site", "migrate", "--wxr", export.to_str().unwrap(), "--design", "new-theme"],
        home.path(),
        site.path(),
    ));
    assert!(again.contains("0 written"), "{again}");
    assert!(again.contains("Design: a new theme"), "{again}");
    let report: Value = serde_json::from_slice(
        &std::fs::read(site.path().join("data/migration/report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["design"]["design"], "new_theme");
    assert_eq!(
        report["design"]["gates"],
        serde_json::json!(["url-coverage", "text-similarity", "metadata"])
    );
}

/// `site feeds` lists the feeds the owner configured with their bounds; feeds make no model
/// calls.
#[test]
fn feeds_lists_the_configured_feeds() {
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    project(site.path());
    let printed = ok(adk_rust(&["site", "feeds"], home.path(), site.path()));
    assert!(printed.contains("No feeds"), "{printed}");
    let feeds = serde_json::json!([{
        "mapping": {
            "feed": "supplier", "kind": "product", "language": "en", "key": "sku",
            "fields": { "price": { "from": "price", "ty": "number" } }
        },
        "bounds": {
            "maxChangeFraction": 0.2, "priceRule": { "autoWithinPct": 10.0 },
            "fieldBounds": {}, "maxRate": { "perMinute": 5000 }
        }
    }]);
    std::fs::write(site.path().join("feeds.json"), feeds.to_string()).unwrap();
    let listed = json(adk_rust(&["site", "feeds", "--json"], home.path(), site.path()));
    assert_eq!(listed[0]["mapping"]["feed"], "supplier");
    assert_eq!(listed[0]["modelCalls"], 0);
    let printed = ok(adk_rust(&["site", "feeds"], home.path(), site.path()));
    assert!(printed.contains("supplier → product") && printed.contains("at most 20%"), "{printed}");
}

/// A legacy site on loopback: a home page linking an about page and a contact form.
#[cfg(feature = "scripted-model")]
async fn legacy() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            let html = |body: &str| {
                axum::response::Html(format!(
                    "<!doctype html><html><head><title>Legacy</title></head><body>{body}</body></html>"
                ))
            };
            match request.uri().path() {
                "/" => html(
                    "<h1>Legacy Solar</h1><a href=\"/about/\">About</a>\
                     <form action=\"/wp-admin/admin-post.php\" method=\"post\" class=\"wpcf7-form\">\
                     <input name=\"your-email\" type=\"email\"><textarea name=\"your-message\"></textarea>\
                     <button>Send</button></form>",
                ),
                "/about/" => html("<h1>About</h1><p>We install panels.</p>"),
                _ => html("<h1>Not found</h1>"),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (origin, requests)
}

/// A freeze needs proof of control of the legacy domain; with it, the snapshot is crawled
/// from the vantage address only and published as a signed migration release (spec 3.5
/// §7.8 "Freeze", S4, A-I9).
#[cfg(feature = "scripted-model")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn freeze_needs_a_proven_domain_then_publishes_the_snapshot() {
    use std::sync::atomic::Ordering;
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    let (home, site) = (home.path(), site.path());
    project(site);
    let (vantage, requests) = legacy().await;
    let zone = home.join("zone.json");
    std::fs::write(&zone, "{}").unwrap();
    let zone_arg = zone.display().to_string();
    let args = [
        "site",
        "freeze",
        "https://legacy.example",
        "--vantage",
        &vantage,
        "--yes",
        "--dns-fixture",
        &zone_arg,
    ];

    let refused = adk_rust(&args, home, site);
    assert!(!refused.status.success());
    let printed = text(&refused);
    assert!(printed.contains("_awp-claim.legacy.example TXT"), "{printed}");
    assert!(printed.contains("A-I9"), "{printed}");
    assert_eq!(requests.load(Ordering::SeqCst), 0, "nothing is read before the domain is proven");

    // The owner publishes the claim's record.
    let claim: Value = serde_json::from_slice(
        &std::fs::read(site.join("data/claims/legacy.example.json")).unwrap(),
    )
    .unwrap();
    let token = claim["token"].as_str().unwrap();
    std::fs::write(
        &zone,
        serde_json::json!({ "_awp-claim.legacy.example": [format!("awp-claim={token}")] })
            .to_string(),
    )
    .unwrap();
    let frozen = ok(adk_rust(&args, home, site));
    assert!(frozen.contains("Control of legacy.example is proven"), "{frozen}");
    assert!(frozen.contains("Froze https://legacy.example/: release 1"), "{frozen}");
    assert!(requests.load(Ordering::SeqCst) > 0);
    assert!(served(site, "/about/index.html").await.unwrap().contains("We install panels."));
    let log = log(home, site);
    assert_eq!(log["releases"][0]["source"]["kind"], "migration");
}
