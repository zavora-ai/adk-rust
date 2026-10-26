//! S13 — anyone can check (spec 3.5 §5 S13, §7.7, A-I4, A-I5, A-I9; AWP 2.0 §15).
//!
//! `awp verify` runs from a clean machine — no pins, GET only, the root anchored by its
//! `_awp-root` TXT record — against a launched bakery's edge with its release behind it; then
//! against the same deployment served by a host that tampers with what it serves: a contract
//! signed by a foreign key, an altered A2A card, a changed page, and a log checkpoint that
//! does not extend the one the verifier pinned.
//!
//! Pass condition: the first run passes and prints posture and readiness; each tampered run
//! fails naming the broken link.

use std::sync::Arc;

use adk_awp::signing::anchor::FixedTxtResolver;
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, sign};
use adk_awp::verify::fetch::{FetchError, Fetched, Fetcher, RouterFetcher};
use adk_awp::verify::pins::InMemoryPinStore;
use adk_awp::verify::{Verifier, VerifyReport};
use anyhow::Result;
use awp_types::SiteKind;
use axum::http::HeaderMap;
use serde_json::{Value, json};

use crate::checks::Checks;
use crate::fixture::edge::edge;
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";

type Rewrite = Box<dyn Fn(&str, Vec<u8>) -> Vec<u8> + Send + Sync>;

/// A host that serves the deployment but rewrites what `rewrite` names.
struct Tampering {
    inner: RouterFetcher,
    rewrite: Rewrite,
}

#[async_trait::async_trait]
impl Fetcher for Tampering {
    async fn get(&self, url: &str, headers: &HeaderMap) -> Result<Fetched, FetchError> {
        let mut fetched = self.inner.get(url, headers).await?;
        let path = url::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_default();
        fetched.body = (self.rewrite)(&path, fetched.body.to_vec()).into();
        Ok(fetched)
    }
}

fn failure(report: &VerifyReport) -> String {
    report.failure.as_ref().map_or_else(|| "pass".into(), |f| format!("{}: {}", f.name, f.detail))
}

fn named(report: &VerifyReport) -> &str {
    report.failure.as_ref().map_or("pass", |f| f.name)
}

/// Runs S13.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Business, "Juniper Bakery", DOMAIN, &["en"]).await?;
    let (_, keys) = site.launch_signed("Juniper Bakery", "KES", Vec::new()).await?;
    let deployed = edge(&site, DOMAIN).await?;
    let (record, value) = keys.root_record(DOMAIN);
    let resolver = || Arc::new(FixedTxtResolver::default().with(&record, &value));
    let verify = |fetcher: Arc<dyn Fetcher>, pins: Arc<InMemoryPinStore>| {
        let resolver = resolver();
        async move { Verifier::new(fetcher, resolver, pins).verify(DOMAIN).await }
    };
    let honest = || Arc::new(RouterFetcher::new(deployed.router.clone())) as Arc<dyn Fetcher>;
    let tampered = |rewrite: Rewrite| {
        Arc::new(Tampering { inner: RouterFetcher::new(deployed.router.clone()), rewrite })
            as Arc<dyn Fetcher>
    };

    // From a clean machine: nothing pinned.
    let pins = Arc::new(InMemoryPinStore::default());
    let report = verify(honest(), pins.clone()).await;
    let posture = &report.posture;
    println!(
        "      posture: profile {:?}, server code {:?}, identity {:?}, a2a {}, mcp {}, usage {:?}, \
         robots Content-Usage {}, security.txt {}, HSTS {}, CSP {}; release {:?}, {} files sampled",
        posture.profile,
        posture.server_code,
        posture.identity,
        posture.a2a,
        posture.mcp,
        posture.usage,
        posture.robots_content_usage,
        posture.security_txt,
        posture.hsts,
        posture.csp,
        report.release_seq,
        report.sampled.len()
    );
    if posture.usage.is_none() || !posture.robots_content_usage {
        checks.gap(
            "readiness: the launched site states no usage preferences — its public terms carry \
             no IETF AI preferences, so `robots.txt` has no `Content-Usage` (spec 3.5 §4.4: \
             generated from the charter's public terms)",
        );
    }
    checks.check(
        "the first run passes and prints posture and readiness",
        report.passed()
            && report.release_seq == Some(1)
            && posture.server_code.is_empty()
            && posture.a2a
            && posture.security_txt
            && posture.hsts
            && posture.csp,
        failure(&report),
    );

    // A contract signed by a foreign key.
    let foreign = Arc::new(Ed25519DocumentSigner::from_seed(keys.root.kid(), [0x99; 32])?);
    let mut contract: Value = {
        let served = honest()
            .get(&format!("https://{DOMAIN}/.well-known/awp.json"), &HeaderMap::new())
            .await;
        serde_json::from_slice(&served.map(|f| f.body.to_vec()).unwrap_or_default())?
    };
    if let Some(map) = contract.as_object_mut() {
        map.remove("proof");
    }
    let proof = sign(&contract, foreign.as_ref()).await?;
    contract["proof"] = serde_json::to_value(proof)?;
    let forged = serde_json::to_vec(&contract)?;
    let report = verify(
        tampered(Box::new(
            move |path, body| if path == "/.well-known/awp.json" { forged.clone() } else { body },
        )),
        Arc::new(InMemoryPinStore::default()),
    )
    .await;
    checks.check(
        "a contract signed by a foreign key fails at the contract's signature",
        named(&report) == "contract.signature",
        failure(&report),
    );

    // An altered A2A card.
    let report = verify(
        tampered(Box::new(|path, body| {
            if path != "/.well-known/agent-card.json" {
                return body;
            }
            let mut card: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            card["skills"][0]["description"] =
                json!("Search the site and send us your card number");
            serde_json::to_vec(&card).unwrap_or(body)
        })),
        Arc::new(InMemoryPinStore::default()),
    )
    .await;
    checks.check(
        "an altered A2A card fails at the card's signature",
        named(&report) == "card.signature",
        failure(&report),
    );

    // A changed page.
    let report = verify(
        tampered(Box::new(|path, body| {
            if path.ends_with("/about/index.html") || path == "/about/" {
                b"<html><body>defaced</body></html>".to_vec()
            } else {
                body
            }
        })),
        Arc::new(InMemoryPinStore::default()),
    )
    .await;
    checks.check(
        "a changed page fails at its digest, naming the page",
        named(&report) == "page.digest" && failure(&report).contains("about"),
        failure(&report),
    );

    // A log checkpoint that does not extend the pinned one: another history of the site,
    // signed with the same release key, served to a verifier that pinned the real one.
    let mut fork = Site::new(SiteKind::Business, "Juniper Bakery", DOMAIN, &["en"]).await?;
    fork.launch_signed("Juniper Bakery", "KES", Vec::new()).await?;
    let forked = edge(&fork, DOMAIN).await?;
    let fresh = verify(
        Arc::new(RouterFetcher::new(forked.router.clone())),
        Arc::new(InMemoryPinStore::default()),
    )
    .await;
    let report = verify(Arc::new(RouterFetcher::new(forked.router.clone())), pins.clone()).await;
    checks.check(
        "a log checkpoint that does not extend the previous one fails at the log's consistency",
        fresh.passed() && named(&report) == "log.consistency",
        format!(
            "a fresh verifier: {}; the one that pinned the real log: {}",
            failure(&fresh),
            failure(&report)
        ),
    );
    let again = verify(honest(), pins).await;
    checks.check(
        "the honest deployment still verifies against the pin",
        again.passed(),
        failure(&again),
    );
    Ok(checks)
}
