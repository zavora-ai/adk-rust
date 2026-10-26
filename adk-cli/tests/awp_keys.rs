//! `adk-rust awp keys rotate|delegate` (spec 3.5 §7.2, §8.1, A-I5) drive a site from no keys
//! to one `awp verify` passes on: the first root, a delegated release key, a publish to
//! `DirectoryHost` and a verification of the served files; a replaced page fails, and an
//! endorsed rotation keeps the pinned verifier's trust. Root operations run only at a
//! terminal, so the test drives them through a pseudo-terminal with generated keys.

#![cfg(all(unix, feature = "website"))]

mod common;

use std::path::Path;
use std::process::Output;

use adk_awp::signing::anchor::root_record;
use adk_site::host::{DirectoryHost, SiteHost as _};
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::Terminal;
use serde_json::Value;

fn project(dir: &Path) {
    std::fs::write(
        dir.join("site.toml"),
        "name = \"Example Solar\"\ndomain = \"example.com\"\nlanguages = [\"en\"]\n",
    )
    .unwrap();
    for page in ["privacy", "consent", "terms", "about"] {
        let folder = dir.join("content/pages").join(page);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("en.md"),
            format!("---\ntitle: {page}\nlegal: {page}\nstate: published\n---\nOur {page} page.\n"),
        )
        .unwrap();
    }
}

/// Runs a root operation at a pseudo-terminal, answering its question with yes.
fn at_terminal(args: &[&str], home: &Path, site: &Path) -> String {
    let mut terminal = Terminal::spawn_in(args, home, site);
    terminal.expect("[y/N]");
    terminal.send("y\r");
    let (code, output) = terminal.wait();
    assert_eq!(code, Some(0), "{output}");
    output
}

fn plain(args: &[&str], home: &Path, site: &Path) -> Output {
    common::run(args, home, site)
}

/// Serves the host's files on loopback with their release media types.
async fn serve(root: &Path) -> String {
    let root = root.to_path_buf();
    let app = axum::Router::new().fallback(move |request: Request| {
        let host = DirectoryHost::new(root.clone());
        async move {
            let path = request.uri().path().to_string();
            match host.fetch(&path).await {
                Ok(Some(bytes)) => {
                    let lookup =
                        if path.ends_with('/') { format!("{path}index.html") } else { path };
                    let mut response = (StatusCode::OK, bytes).into_response();
                    let kind = adk_site::release::content_type(&lookup);
                    response
                        .headers_mut()
                        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
                    response
                }
                _ => Response::builder().status(404).body(axum::body::Body::empty()).unwrap(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

fn anchor(site: &Path) -> String {
    let keys: awp_types::KeySet =
        serde_json::from_slice(&std::fs::read(site.join("well-known/awp/keys.json")).unwrap())
            .unwrap();
    root_record(&keys.root.kid, &keys.root.public_key)
}

fn verify(origin: &str, anchor: &str, pins: &Path, home: &Path, site: &Path) -> Value {
    let pins = pins.display().to_string();
    let output = plain(
        &[
            "awp",
            "verify",
            "example.com",
            "--origin",
            origin,
            "--anchor",
            anchor,
            "--pins",
            &pins,
            "--samples",
            "500",
            "--json",
        ],
        home,
        site,
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not a JSON report:\n{}", common::text(&output)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_and_release_keys_make_a_site_anyone_can_verify() {
    let home = tempfile::tempdir().unwrap();
    let site = tempfile::tempdir().unwrap();
    let (home, site) = (home.path(), site.path());
    project(site);
    let root = home.join("keys/root.jwk");
    let root_arg = root.display().to_string();

    // Root operations refuse to run without a terminal.
    let output = plain(&["awp", "keys", "rotate", "--new-root-key", &root_arg], home, site);
    assert!(!output.status.success());
    assert!(common::text(&output).contains("never inside an agent"), "{}", common::text(&output));
    assert!(!root.exists());

    // The first root, then a delegated release key.
    let rotated = at_terminal(
        &["awp", "keys", "rotate", "--new-root-key", &root_arg, "--kid", "site-root-2026"],
        home,
        site,
    );
    assert!(
        rotated.contains("_awp-root.example.com TXT \"v=awp2; kid=site-root-2026; jkt="),
        "{rotated}"
    );
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&root).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
    let delegated = at_terminal(
        &["awp", "keys", "delegate", "--root-key", &root_arg, "--scope", "release"],
        home,
        site,
    );
    assert!(delegated.contains("New key `release-"), "{delegated}");
    assert!(site.join("data/keys/release.jwk").exists());
    let keys = std::fs::read_to_string(site.join("well-known/awp/keys.json")).unwrap();
    assert!(!keys.contains("\"d\""), "keys.json holds public keys only");

    let published = common::text(&plain(&["site", "publish", "--yes"], home, site));
    assert!(published.contains("Published release 1"), "{published}");
    assert!(!published.contains("note:"), "the release key is delegated: {published}");

    // Anyone can check the served site.
    let origin = serve(&site.join("data/host")).await;
    let pins = home.join("pins.json");
    let report = verify(&origin, &anchor(site), &pins, home, site);
    assert_eq!(report["passed"], true, "{report:#}");
    assert_eq!(report["releaseSeq"], 1);

    // An endorsed rotation: the pinned verifier follows the new root.
    let next = home.join("keys/root-next.jwk");
    let next_arg = next.display().to_string();
    let rotated = at_terminal(
        &[
            "awp",
            "keys",
            "rotate",
            "--root-key",
            &root_arg,
            "--new-root-key",
            &next_arg,
            "--kid",
            "site-root-2027",
        ],
        home,
        site,
    );
    assert!(rotated.contains("endorsement"), "{rotated}");
    let published = common::text(&plain(&["site", "publish", "--yes"], home, site));
    assert!(published.contains("Published release 2"), "{published}");
    let report = verify(&origin, &anchor(site), &pins, home, site);
    assert_eq!(
        (report["passed"].as_bool(), report["recovery"].as_bool()),
        (Some(true), Some(false)),
        "{report:#}"
    );

    // Each root operation is in the site's audit trail (spec 3.5 §9.3).
    let trail =
        adk_core::audit_log::FileAuditRecorder::read(site.join("data/audit.jsonl")).unwrap();
    let field = |record: &adk_core::audit_log::AuditRecord, key: &str| {
        record.ext.get(key).cloned().unwrap_or_default()
    };
    let keys: Vec<(&str, serde_json::Value, serde_json::Value)> = trail
        .iter()
        .filter(|record| record.action.starts_with("awp.keys."))
        .map(|record| (record.action.as_str(), field(record, "principal"), field(record, "root")))
        .collect();
    assert_eq!(keys.len(), 3, "{trail:#?}");
    assert_eq!(
        (keys[0].0, keys[0].1.as_str()),
        ("awp.keys.rotated", Some("awp-root:site-root-2026"))
    );
    assert_eq!(keys[0].2, "site-root-2026");
    assert_eq!(keys[1].0, "awp.keys.delegated");
    assert_eq!((keys[2].0, keys[2].2.as_str()), ("awp.keys.rotated", Some("site-root-2027")));
    assert_eq!(keys[2].1, "awp-root:site-root-2026", "the old root endorsed the new one");
    let delegated = trail.iter().find(|record| record.action == "awp.keys.delegated").unwrap();
    assert_eq!(delegated.ext["scopes"], serde_json::json!(["release"]));
    assert!(trail.iter().any(|record| record.action == "site.release.signed"));

    // A page replaced on the host fails at its digest.
    let current = std::fs::canonicalize(site.join("data/host/current")).unwrap();
    let page = current.join("about/index.html");
    std::fs::remove_file(&page).unwrap();
    std::fs::write(&page, "<html>defaced</html>").unwrap();
    let report = verify(&origin, &anchor(site), &home.join("fresh-pins.json"), home, site);
    assert_eq!(report["failure"]["name"], "page.digest", "{report:#}");
}
