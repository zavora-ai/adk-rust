//! S4 — freeze in hours (spec 3.5 §5 S4, §7.8 "Freeze", §7.18 stage 3, A-I1, A-I9).
//!
//! The legacy WordPress site must come off the public path before the migration: onboarding
//! offers the freeze, the owner accepts, and the site is crawled from a private vantage point
//! and published as a signed release behind the site's agent edge, its forms posting to edge
//! capabilities.
//!
//! Pass condition: a crawled snapshot is served by the static host; zero requests reach the
//! upstream at request time; the old server is reachable only from the crawler.

use std::collections::BTreeSet;
use std::time::Duration;

use adk_site::migrate::freeze::{Freeze, FreezeConfig};
use adk_site::migrate::http::{EgressScope, FetchOptions, HttpClient};
use adk_site::migrate::{FeatureKind, MigrateError};
use adk_website::onboarding::StartingPoint;
use adk_website::onboarding::discovery::{Discovery, OwnerCard};
use anyhow::Result;
use awp_types::{ReleaseSource, SiteKind};
use axum::http::StatusCode;
use serde_json::json;
use url::Url;

use crate::checks::Checks;
use crate::fixture::edge::edge;
use crate::fixture::legacy::{DOMAIN, LegacyServer, ORIGIN, context, proven_context, signer};
use crate::fixture::*;
use crate::live::Mode;

/// Runs S4.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let upstream = LegacyServer::wordpress(false).await?;

    // 2–3 Discover and protect: a legacy runtime is offered the freeze.
    let discovered =
        Discovery::new(StartingPoint::ExistingSite { url: ORIGIN.into() }, "en".parse()?)?
            .run(&proven_context(&upstream).await?)
            .await?;
    checks.check(
        "onboarding offers the freeze for a legacy runtime",
        discovered.legacy_runtime
            && discovered
                .cards()
                .iter()
                .any(|c| matches!(c, OwnerCard::Freeze { url } if url == ORIGIN)),
        "the Freeze card names the legacy site",
    );

    // The owner accepts. The crawl reads the old server from a private vantage point only.
    let site = Site::new(SiteKind::Business, "Example Solar", DOMAIN, &["en"]).await?;
    let edge_origin = Url::parse(&site.site.origin)?;
    let freeze = Freeze::new(FreezeConfig {
        public_origin: Url::parse(ORIGIN)?,
        vantage: upstream.url(),
        edge_origin: edge_origin.clone(),
    })
    .with_min_interval(Duration::ZERO)
    .with_signer(signer()?);
    let out = tempfile::tempdir()?;
    let before_proof = upstream.requests();
    let refused =
        freeze.run(&context(&upstream)?, out.path(), None, &site.log, site.host.as_ref()).await;
    checks.check(
        "A-I9: nothing is frozen before the domain is proven",
        matches!(refused, Err(MigrateError::NotProven { .. }))
            && upstream.requests() == before_proof,
        "NotProven, zero requests",
    );
    let recorder = std::sync::Arc::new(adk_core::audit_log::InMemoryAuditRecorder::new());
    let ctx = proven_context(&upstream).await?.with_audit(recorder.clone());
    let report = freeze.run(&ctx, out.path(), None, &site.log, site.host.as_ref()).await?;
    checks.check(
        "the crawl reaches only the vantage point and is signed",
        report.connections.iter().all(|c| c.origin == upstream.origin && c.allowed)
            && upstream.seen().iter().all(|s| s.signed),
        format!("{} connections, {} requests", report.connections.len(), upstream.requests()),
    );
    checks.check(
        "the snapshot is a signed release with no server code (A-I1)",
        report.release.manifest.seq == 1
            && report.release.manifest.source == ReleaseSource::Migration
            && report.release.files.keys().all(|p| !p.ends_with(".php"))
            && report
                .excluded
                .iter()
                .any(|(path, why)| path == "/wp-login.php" && why.contains("A-I1")),
        format!("{} files, {} excluded", report.release.files.len(), report.excluded.len()),
    );
    let verified = verify_served(&site.host, DOMAIN, &site.release_key).await;
    checks.check(
        "the static host serves the crawled snapshot, verified from its own files",
        matches!(verified, Ok(1)),
        format!("{verified:?}"),
    );
    let contact = report
        .forms
        .iter()
        .find(|f| f.page == "/contact/" && f.feature == Some(FeatureKind::ContactForm));
    checks.check(
        "forms post to edge capabilities; comments wait for the owner",
        contact.is_some_and(|f| f.capability.as_deref() == Some("submit_inquiry"))
            && report
                .forms
                .iter()
                .any(|f| f.feature == Some(FeatureKind::Comments) && f.capability.is_none()),
        report
            .forms
            .iter()
            .map(|f| format!("{} → {:?}", f.page, f.capability))
            .collect::<Vec<_>>()
            .join(", "),
    );

    // The public path: the edge with the frozen release behind it. Every page and the legacy
    // redirect answer; the upstream sees nothing.
    let site_edge = edge(&site, DOMAIN).await?;
    let before = upstream.requests();
    let mut pages = 0;
    for path in report.release.files.keys().filter(|p| p.ends_with("index.html")) {
        let (status, _, _) = site_edge.send(site_edge.get(path)).await;
        if status == StatusCode::OK {
            pages += 1;
        }
    }
    let frozen_contact = std::fs::read_to_string(out.path().join("contact/index.html"))?;
    let action = format!("action=\"{}awp/v2/capabilities/submit_inquiry\"", edge_origin);
    let receipt = site_edge.consent("contact").await?;
    let enquiry =
        json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "A quote for 8 panels?" });
    let (status, _, received) = site_edge
        .send(site_edge.call("submit_inquiry", "freeze-1", Some(&receipt), &enquiry))
        .await;
    checks.check(
        "the frozen contact form posts to the edge, which takes the enquiry",
        frozen_contact.contains(&action)
            && status == StatusCode::OK
            && received["received"] == true,
        format!("{status}"),
    );
    checks.check(
        "zero requests reach the upstream at request time",
        pages >= 7 && upstream.requests() == before,
        format!(
            "{pages} pages served from the host, {} upstream requests",
            upstream.requests() - before
        ),
    );

    // The old server is reachable only from the crawler: any client without the crawler's
    // scope refuses its private address.
    let public = HttpClient::builder().min_interval(Duration::ZERO).build()?;
    let reached = public.fetch(&upstream.url(), &FetchOptions::browser()).await;
    let scoped = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from(["https://agent.example".to_string()])))
        .min_interval(Duration::ZERO)
        .build()?
        .fetch(&upstream.url(), &FetchOptions::browser())
        .await;
    checks.check(
        "the old server is reachable only from the crawler",
        matches!(reached, Err(MigrateError::Egress { .. }))
            && matches!(scoped, Err(MigrateError::Egress { .. }))
            && upstream.requests() == before,
        "a public client and another scope both refuse the private vantage point",
    );
    checks.check(
        "the freeze is audited",
        recorder.records().iter().any(|r| r.action == "site.freeze"),
        "site.freeze",
    );
    Ok(checks)
}
