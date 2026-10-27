//! Recorded inventories (spec 3.5 §11 "Fixtures": "the PR tier runs the pipeline against
//! recorded inventories"): what each source produces from the WordPress fixture, written to
//! `fixtures/recorded/` by `website-scenarios record`. The
//! scenarios check the inventories they take against these; the nightly tier records from the
//! compose fixture with `--wordpress <origin>` and compares.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use adk_site::migrate::http::{EgressScope, HttpClient};
use adk_site::migrate::sources::{IsolatedReplay, ReplaySpec, WordPressRest, Wxr};
use adk_site::migrate::{SiteInventory, SiteSource, SourceContext};
use anyhow::Result;
use url::Url;

use crate::fixture::legacy::{
    LegacyServer, ORIGIN, RecordedReplayDriver, acting, fixtures, proven_context,
    recorded_wordpress, uploads,
};

/// The inventory of the infected site, replayed in isolation.
///
/// # Errors
///
/// The source's error.
pub async fn replay_infected() -> Result<SiteInventory> {
    let driver = Arc::new(RecordedReplayDriver::new(true));
    let dump_dir = tempfile::tempdir()?;
    let dump = dump_dir.path().join("dump.sql");
    std::fs::write(&dump, "-- recorded dump\n")?;
    let uploads = uploads(true)?;
    let live = LegacyServer::wordpress(true).await?;
    let ctx = proven_context(&live).await?;
    let replay = IsolatedReplay::new(driver, ReplaySpec::wordpress(ORIGIN, &dump, uploads.path()));
    Ok(replay.inventory(&ctx).await?)
}

/// The inventory `WordPressRest` takes of the clean site's public collections.
///
/// # Errors
///
/// The source's error.
pub async fn wordpress_rest(server: &LegacyServer) -> Result<SiteInventory> {
    let ctx = proven_context(server).await?;
    Ok(WordPressRest::new(Url::parse(ORIGIN)?).inventory(&ctx).await?)
}

/// The inventory of the WordPress export with its uploads.
///
/// # Errors
///
/// The source's error.
pub async fn wxr(server: &LegacyServer) -> Result<SiteInventory> {
    let ctx = proven_context(server).await?;
    let uploads = uploads(false)?;
    let export = std::fs::read(recorded_wordpress().join("export.xml"))?;
    Ok(Wxr::from_bytes(export).with_uploads(uploads.path()).inventory(&ctx).await?)
}

fn write(dir: &Path, name: &str, inventory: &SiteInventory) -> Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, serde_json::to_vec_pretty(inventory)?)?;
    println!(
        "  wrote {} ({} URLs, {} items)",
        path.display(),
        inventory.urls.len(),
        inventory.items.len()
    );
    Ok(())
}

/// Records every inventory into `fixtures/recorded/`.
///
/// # Errors
///
/// A source's error, or a file that cannot be written.
pub async fn record() -> Result<()> {
    let dir = fixtures().join("recorded");
    std::fs::create_dir_all(&dir)?;
    let clean = LegacyServer::wordpress(false).await?;
    write(&dir, "inventory-replay-infected.json", &replay_infected().await?)?;
    write(&dir, "inventory-wordpress-rest.json", &wordpress_rest(&clean).await?)?;
    write(&dir, "inventory-wxr.json", &wxr(&clean).await?)?;
    Ok(())
}

/// A client that reaches only the live fixture at `origin`, with the public origin mapped
/// onto it, signing as the site.
fn live_client(origin: &Url) -> Result<Arc<HttpClient>> {
    Ok(Arc::new(
        HttpClient::builder()
            .egress(EgressScope::Only(BTreeSet::from([origin.origin().ascii_serialization()])))
            .map_origin(&Url::parse(ORIGIN)?, origin.clone())
            .min_interval(std::time::Duration::ZERO)
            .signer(crate::fixture::legacy::signer()?)
            .build()?,
    ))
}

fn paths(inventory: &SiteInventory) -> BTreeSet<String> {
    inventory.urls.iter().filter(|u| u.status < 400).map(|u| u.path.clone()).collect()
}

/// The nightly tier's checks against the WordPress compose fixture at `origin` (spec 3.5 §11):
/// the live site still produces what the recordings the pull-request tier runs against hold;
/// the infected overlay's markers are found and quarantined; the freeze serves it with no
/// server code.
///
/// # Errors
///
/// When the live site cannot be read.
pub async fn against_live(origin: Url, infected: bool) -> Result<crate::checks::Checks> {
    use adk_site::migrate::ClaimScope;
    use adk_site::migrate::detect::{Detectors, FindingKind, Subject};
    use adk_site::migrate::sources::Crawl;
    let mut checks = crate::checks::Checks::default();
    let http = live_client(&origin)?;
    let ctx = SourceContext::new(acting(), http.clone()).with_scope(ClaimScope::Proven(
        crate::fixture::legacy::proven(crate::fixture::legacy::DOMAIN).await?,
    ));
    let rest = WordPressRest::new(Url::parse(ORIGIN)?).inventory(&ctx).await?;
    let recorded: SiteInventory = serde_json::from_slice(&std::fs::read(
        fixtures().join("recorded/inventory-wordpress-rest.json"),
    )?)?;
    let missing: Vec<String> = paths(&recorded).difference(&paths(&rest)).cloned().collect();
    checks.check(
        "the live WordPress serves every URL the REST recording holds",
        missing.is_empty(),
        format!("{} URLs live; missing {missing:?}", rest.urls.len()),
    );
    let legacy = http
        .fetch(
            &Url::parse(&format!("{ORIGIN}/old-offers/"))?,
            &adk_site::migrate::http::FetchOptions::crawler(),
        )
        .await?;
    checks.check(
        "the live WordPress keeps the legacy redirect the recordings hold",
        legacy.status == 301
            && legacy.location.as_ref().is_some_and(|l| l.path() == "/products/panel-400/"),
        format!("{} to {:?}", legacy.status, legacy.location.as_ref().map(Url::as_str)),
    );
    let crawl = Crawl::new(Url::parse(&format!("{ORIGIN}/"))?).inventory(&ctx).await?;
    let detectors = Detectors::new();
    let mut findings = detectors.scan_inventory(&crawl).await;
    if let Some(cloaking) = detectors
        .check_cloaking(&ctx, &Subject::Url("/".into()), &Url::parse(&format!("{ORIGIN}/"))?)
        .await?
    {
        findings.push(cloaking);
    }
    let kinds: BTreeSet<String> = findings.iter().map(|f| format!("{:?}", f.kind)).collect();
    let details: Vec<String> = findings
        .iter()
        .map(|f| {
            let evidence: Vec<&str> = f.evidence.iter().map(|e| e.detail.as_str()).collect();
            format!("{:?} in {:?}: {}", f.kind, f.subject, evidence.join("; "))
        })
        .collect();
    if infected {
        let expected =
            [FindingKind::HiddenLinks, FindingKind::ObfuscatedScript, FindingKind::Cloaking];
        checks.check(
            "the infected overlay's markers are found with evidence",
            expected.iter().all(|k| {
                findings
                    .iter()
                    .any(|f| f.kind == *k && f.evidence.iter().all(|e| e.digest.is_some()))
            }),
            format!("{kinds:?}"),
        );
    } else {
        checks.check("the clean site raises no finding", findings.is_empty(), format!("{details:?}"));
    }

    // The freeze of the live site: served from a static host with no server code.
    let host_dir = tempfile::tempdir()?;
    let host = adk_site::host::DirectoryHost::new(host_dir.path());
    let signer =
        Arc::new(adk_site::signer::Ed25519ReleaseSigner::generate(crate::fixture::legacy::DOMAIN)?);
    let log = adk_site::log::SiteLog::open(
        Arc::new(adk_core::audit_log::InMemoryAuditLogStore::new()),
        crate::fixture::legacy::DOMAIN,
        signer,
    )
    .await?;
    let freeze = adk_site::migrate::freeze::Freeze::new(adk_site::migrate::freeze::FreezeConfig {
        public_origin: Url::parse(ORIGIN)?,
        vantage: origin.clone(),
        edge_origin: Url::parse("https://agent.solar.test")?,
    })
    .with_min_interval(std::time::Duration::ZERO)
    .with_signer(crate::fixture::legacy::signer()?);
    let out = tempfile::tempdir()?;
    let report = freeze.run(&ctx, out.path(), None, &log, &host).await?;
    checks.check(
        "the freeze of the live site releases no server code",
        report.release.files.keys().all(|p| !p.ends_with(".php")) && report.release.files.len() > 5,
        format!("{} files, {} excluded", report.release.files.len(), report.excluded.len()),
    );
    Ok(checks)
}
