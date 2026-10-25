//! The built-in renderer (spec 3.5 §7.8 "Generator", §7.4 machine views, §7.8.2 step 5):
//! a golden build of a two-language fixture site, twins, site files, and incremental builds
//! that rebuild exactly an item and its dependents.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use adk_site::content::SiteProject;
use adk_site::generator::{BuiltinGenerator, ChangeSet, SiteGenerator};
use awp_types::{ContentId, MachineView};
use chrono::{TimeZone, Utc};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site")
}

async fn project() -> SiteProject {
    SiteProject::load(&fixture(), Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap())
        .await
        .unwrap()
}

fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
                files.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn text(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(files.get(path).unwrap_or_else(|| panic!("{path} is missing")).clone())
        .unwrap()
}

#[tokio::test]
async fn golden_build_of_a_two_language_site() {
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project().await, out.path()).await.unwrap();
    let files = read_tree(out.path());
    assert_eq!(files.keys().cloned().collect::<BTreeSet<_>>(), report.outputs);

    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let _ = std::fs::remove_dir_all(&golden);
        for (path, bytes) in &files {
            let target = golden.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
    }
    let expected = read_tree(&golden);
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the build's files match the golden set (UPDATE_GOLDEN=1 regenerates it)"
    );
    for (path, bytes) in &files {
        assert!(expected[path] == *bytes, "{path} differs from its golden copy");
    }

    // Pages per language, each with both twins linked as alternates.
    for page in [
        "index.html",
        "sw/index.html",
        "about/index.html",
        "sw/about/index.html",
        "posts/hello/index.html",
    ] {
        let html = text(&files, page);
        let dir = page.trim_end_matches("index.html");
        assert!(
            html.contains(&format!(
                "<link rel=\"alternate\" type=\"text/markdown\" href=\"/{dir}index.md\">"
            )),
            "{page}"
        );
        assert!(
            html.contains(&format!("<link rel=\"alternate\" type=\"application/awp+json\" href=\"/{dir}index.awp.json\">")),
            "{page}"
        );
        assert!(files.contains_key(&format!("{dir}index.md")));
        let view: MachineView =
            serde_json::from_slice(&files[&format!("{dir}index.awp.json")]).unwrap();
        assert_eq!(view.path, format!("/{dir}"));
    }
    let sw_home = text(&files, "sw/index.html");
    assert!(sw_home.contains("<html lang=\"sw\">") && sw_home.contains("Ruka hadi maudhui"));
    assert!(
        text(&files, "posts/hello/index.html")
            .contains("hreflang=\"sw\" href=\"https://example.com/sw/posts/hello/\"")
    );

    // Raw HTML in a body is text; the only script is JSON-LD data.
    let post = text(&files, "posts/hello/index.html");
    assert!(post.contains("&lt;script&gt;") && !post.contains("<script>alert"));
    assert_eq!(post.matches("<script").count(), 1);
    assert!(post.contains("\"@type\":\"BlogPosting\""));
    assert!(post.contains("<img src=\"panel.png\" alt=\"A solar panel on a roof\""));
    assert_eq!(
        files["posts/hello/panel.png"],
        std::fs::read(fixture().join("content/posts/hello/panel.png")).unwrap()
    );

    // JSON-LD per kind; a live field shows its last value and time, marked as live and
    // never as a claim (AWP 2.0 §8.2, §14.8).
    let product = text(&files, "products/panel-400/index.html");
    assert!(
        product.contains("\"@type\":\"Product\"") && product.contains("\"priceCurrency\":\"KES\"")
    );
    assert!(product.contains(
        "<dd data-awp-live=\"stock\">12 <small>(as of <time datetime=\"2026-10-20T08:00:00Z\">"
    ));
    assert!(!product.contains("availability"), "no availability claim in JSON-LD");
    let view: MachineView =
        serde_json::from_slice(&files["products/panel-400/index.awp.json"]).unwrap();
    assert_eq!(
        view.resource["stock"],
        serde_json::json!({ "value": 12, "asOf": "2026-10-20T08:00:00Z", "capability": "check_availability" })
    );
    assert_eq!(
        view.capabilities,
        vec!["check_availability", "get_quote", "place_order", "search_site"]
    );
    assert_eq!(view.markdown.as_deref(), Some("https://example.com/products/panel-400/index.md"));
    assert!(text(&files, "events/open-day/index.html").contains("\"@type\":\"Event\""));
    let notice = text(&files, "notices/water/index.html");
    assert!(
        notice.contains("\"@type\":\"SpecialAnnouncement\"") && notice.contains("notice-expired")
    );

    // Withdrawals: one answers 410, one is replaced by a notice; drafts never ship.
    assert!(report.redirects.iter().any(|r| r.from == "/articles/old-offer/" && r.status == 410));
    assert!(!files.contains_key("articles/old-offer/index.html"));
    assert!(text(&files, "articles/recall/index.html").contains("withdrawn on legal advice"));
    assert!(!files.keys().any(|p| p.starts_with("posts/draft")));
    let redirects: Vec<awp_types::Redirect> =
        serde_json::from_slice(&files["_redirects.json"]).unwrap();
    assert_eq!(redirects, report.redirects);

    // Site files.
    let robots = text(&files, "robots.txt");
    assert!(
        robots.contains("Content-Usage: train-ai=n, search=y")
            && robots.contains("Sitemap: https://example.com/sitemap.xml")
    );
    let llms = text(&files, "llms.txt");
    assert!(llms.starts_with("# Example Solar\n\n> Solar panels and installation in Nairobi."));
    assert!(llms.contains("- [Panel 400 W](https://example.com/products/panel-400/index.md): A 400 W monocrystalline panel."));
    let security = text(&files, ".well-known/security.txt");
    assert!(
        security.contains("Contact: mailto:security@example.com\nExpires: 2027-06-30T00:00:00Z")
    );
    let sitemap = text(&files, "sitemap.xml");
    assert!(sitemap.contains(
        "<loc>https://example.com/products/panel-400/</loc><lastmod>2026-10-20T08:00:00Z</lastmod>"
    ));
    assert!(sitemap.contains("hreflang=\"sw\""));
    assert!(text(&files, "feed.xml").contains("<title>Hello, sun</title>"));
    assert!(
        text(&files, ".well-known/awp/charter.json")
            .starts_with("{\"consentScopes\":[\"newsletter\"]")
    );
}

#[tokio::test]
async fn identical_projects_build_identical_files() {
    let project = project().await;
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    BuiltinGenerator.build(&project, a.path()).await.unwrap();
    BuiltinGenerator.build(&project, b.path()).await.unwrap();
    assert_eq!(read_tree(a.path()), read_tree(b.path()));
}

#[tokio::test]
async fn incremental_builds_rebuild_exactly_an_item_and_its_dependents() {
    let mut project = project().await;
    let out = tempfile::tempdir().unwrap();
    let first = BuiltinGenerator.build(&project, out.path()).await.unwrap();

    // Edit one post's body: its page and twins, its listing, its tag pages, the home page
    // section, sitemap, feed and llms.txt; nothing in Swahili, no product, no other page.
    let id = ContentId::from("posts/hello/en");
    let item = project.items.iter_mut().find(|i| i.id == id).unwrap();
    item.body = Some(awp_types::Markdown("A new body.\n".into()));
    project.release_seq = 2;
    let changes = ChangeSet { items: BTreeSet::from([id.clone()]), previous: first.graph.clone() };
    let second = BuiltinGenerator.build_incremental(&project, out.path(), &changes).await.unwrap();
    let expected: BTreeSet<String> = [
        "posts/hello/index.html",
        "posts/hello/index.md",
        "posts/hello/index.awp.json",
        "posts/hello/panel.png",
        "posts/index.html",
        "posts/index.md",
        "posts/index.awp.json",
        "tags/solar/index.html",
        "tags/solar/index.md",
        "tags/solar/index.awp.json",
        "tags/news/index.html",
        "tags/news/index.md",
        "tags/news/index.awp.json",
        "index.html",
        "index.md",
        "index.awp.json",
        "sitemap.xml",
        "llms.txt",
        "feed.xml",
        "_redirects.json",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(second.written, expected);
    assert!(second.removed.is_empty());

    // The incremental result equals a full build, except that machine views of untouched
    // pages keep the release they were built in.
    let full = tempfile::tempdir().unwrap();
    BuiltinGenerator.build(&project, full.path()).await.unwrap();
    let (inc, all) = (read_tree(out.path()), read_tree(full.path()));
    assert_eq!(inc.keys().collect::<Vec<_>>(), all.keys().collect::<Vec<_>>());
    for (path, bytes) in &inc {
        if all[path] != *bytes {
            assert!(
                path.ends_with(".awp.json") && !second.written.contains(path),
                "{path} differs"
            );
            let (mut a, mut b): (serde_json::Value, serde_json::Value) = (
                serde_json::from_slice(bytes).unwrap(),
                serde_json::from_slice(&all[path]).unwrap(),
            );
            a["releaseSeq"] = 0.into();
            b["releaseSeq"] = 0.into();
            assert_eq!(a, b, "{path} differs beyond releaseSeq");
        }
    }

    // Withdrawing the post as gone removes its page, adds a 410 rule, and drops the empty tag page.
    let item = project.items.iter_mut().find(|i| i.id == id).unwrap();
    item.state = awp_types::ContentState::Withdrawn;
    let changes = ChangeSet { items: BTreeSet::from([id]), previous: second.graph.clone() };
    let third = BuiltinGenerator.build_incremental(&project, out.path(), &changes).await.unwrap();
    assert!(
        third.removed.contains("posts/hello/index.html")
            && third.removed.contains("tags/news/index.html")
    );
    assert!(third.redirects.iter().any(|r| r.from == "/posts/hello/" && r.status == 410));
    assert!(!out.path().join("posts/hello").exists());
    assert!(
        third.written.contains("sw/posts/hello/index.html"),
        "the translation loses its alternate"
    );
}

#[tokio::test]
async fn documents_join_the_release_and_pages_link_the_contract() {
    let mut project = project().await;
    project
        .documents
        .insert(".well-known/awp.json".into(), bytes::Bytes::from_static(b"{\"awp\":\"2.0\"}"));
    let out = tempfile::tempdir().unwrap();
    let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    assert!(report.outputs.contains(".well-known/awp.json"));
    let files = read_tree(out.path());
    assert_eq!(files[".well-known/awp.json"], b"{\"awp\":\"2.0\"}");
    assert!(
        text(&files, "about/index.html")
            .contains("<link rel=\"awp\" href=\"/.well-known/awp.json\">")
    );
}
