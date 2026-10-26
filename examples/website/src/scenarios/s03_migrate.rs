//! S3 — migrate and leave the infection behind (spec 3.5 §5 S3, §7.8, §7.8.1, §7.18 stages
//! 2–7, A-I1, A-I10).
//!
//! Onboarding starts from the WordPress fixture with its `infected` overlay: discovery replays
//! the site in isolation, inventories it, quarantines every inert marker with evidence and
//! maps the forms and the shop to capabilities; the owner moves to a new theme and decides the
//! held items; release 1 is built, gated and published.
//!
//! Pass condition: every inventoried URL is served or 301-redirected; zero broken internal
//! links; zero server-side executables (A-I1); hidden spam, crawler-only scripts and a `.php`
//! upload are quarantined with evidence; forms and shop map to capabilities; visual and text
//! parity within thresholds on the top 20 pages.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::audit_log::AuditRecorder;
use adk_site::gate::{CheckStatus, GateCheck};
use adk_site::host::SiteHost;
use adk_site::migrate::convert::legacy_runtime_path;
use adk_site::migrate::detect::{FindingKind, Subject};
use adk_site::migrate::parity::{
    ChromeScreenshotter, MigrationParity, ParityConfig, record_design_choice,
};
use adk_site::migrate::quarantine::QuarantineState;
use adk_site::migrate::sources::{IsolatedReplay, ReplaySpec};
use adk_site::migrate::{SiteInventory, SourceKind};
use adk_site::pipeline::{ApprovalDecision, ApprovalGate, ApprovalRequest, EntryPoint};
use adk_site::release::manifest_path;
use adk_website::onboarding::StartingPoint;
use adk_website::onboarding::discovery::{DesignChoice, Discovery, OwnerCard};
use adk_website::themes::ThemeId;
use anyhow::{Result, anyhow};
use awp_types::{CapabilityEffect, ContentId, ReleaseManifest, Role, SiteKind};

use crate::checks::Checks;
use crate::fixture::legacy::{
    DOMAIN, LegacyServer, ORIGIN, RecordedReplayDriver, proven_context, uploads,
};
use crate::fixture::*;
use crate::live::Mode;

/// The owner, deciding one held item at a time.
#[derive(Default)]
struct OwnerDecides {
    asked: std::sync::Mutex<Vec<ApprovalRequest>>,
}

#[async_trait::async_trait]
impl ApprovalGate for OwnerDecides {
    async fn decide(&self, request: &ApprovalRequest) -> adk_site::Result<ApprovalDecision> {
        self.asked.lock().unwrap().push(request.clone());
        Ok(ApprovalDecision::Approved {
            approvers: vec![OWNER.into()],
            decision: Some(format!("dec-quarantine-{}", self.asked.lock().unwrap().len())),
        })
    }
}

/// The paths of an inventory, as the recording lists them.
pub fn inventory_paths(inventory: &SiteInventory) -> BTreeSet<String> {
    inventory.urls.iter().map(|u| format!("{} {}", u.status, u.path)).collect()
}

/// Runs S3 against the recorded WordPress site.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();

    // 2 Discover: replay the infected site in isolation and inventory it (A-I10: the dump and
    // the uploads are read-only; the replay reaches nothing and is destroyed).
    let driver = Arc::new(RecordedReplayDriver::new(true));
    let dump_dir = tempfile::tempdir()?;
    let dump = dump_dir.path().join("dump.sql");
    std::fs::write(&dump, "-- recorded dump\n")?;
    let uploads = uploads(true)?;
    let live = LegacyServer::wordpress(true).await?;
    let recorder = Arc::new(adk_core::audit_log::InMemoryAuditRecorder::new());
    let ctx = proven_context(&live).await?.with_audit(recorder.clone());
    let replay = Arc::new(IsolatedReplay::new(
        driver.clone(),
        ReplaySpec::wordpress(ORIGIN, &dump, uploads.path()),
    ));
    let discovery =
        Discovery::new(StartingPoint::ExistingSite { url: ORIGIN.into() }, "en".parse()?)?
            .with_source(replay.clone());
    let discovered = discovery.run(&ctx).await?;
    let report = replay.last_report().ok_or_else(|| anyhow!("a replay report"))?;
    checks.check(
        "the replay reaches nothing and is destroyed",
        report.destroyed && report.egress_blocked && report.outbound_connections.is_empty(),
        format!("{} replays destroyed", driver.destroyed.lock().unwrap().len()),
    );
    let inventory = discovered.inventory.clone().ok_or_else(|| anyhow!("an inventory"))?;
    let recorded: SiteInventory = serde_json::from_slice(&std::fs::read(
        crate::fixture::legacy::fixtures().join("recorded/inventory-replay-infected.json"),
    )?)?;
    checks.check(
        "the inventory matches the recorded inventory",
        inventory.source == SourceKind::IsolatedReplay
            && inventory_paths(&inventory) == inventory_paths(&recorded)
            && inventory.items.len() == recorded.items.len(),
        format!("{} URLs, {} items", inventory.urls.len(), inventory.items.len()),
    );
    checks.check(
        "a legacy runtime is offered the freeze",
        discovered.legacy_runtime
            && discovered.cards().iter().any(|c| matches!(c, OwnerCard::Freeze { .. })),
        "Freeze card",
    );
    let mut migration = discovered.migration.clone().ok_or_else(|| anyhow!("a migration"))?;

    // The inert markers: hidden spam, obfuscated and crawler-only scripts, a `.php` upload and
    // a disguised file, each held with evidence.
    let entries = &migration.quarantine.entries;
    let held = |kind: FindingKind, subject: &Subject| {
        entries.iter().any(|e| {
            &e.subject == subject
                && e.findings.iter().any(|f| {
                    f.kind == kind
                        && f.evidence.iter().all(|ev| !ev.detail.is_empty() && ev.digest.is_some())
                })
        })
    };
    let hello = Subject::Item("crawl:/2026/10/01/hello-sun/".into());
    let about = Subject::Item("crawl:/about/".into());
    let planted = Subject::Media("/wp-content/uploads/2026/10/cache.php".into());
    let fake = Subject::Media("/wp-content/uploads/2026/10/photo.jpg".into());
    checks.check(
        "hidden spam links are quarantined with evidence",
        held(FindingKind::HiddenLinks, &hello) && held(FindingKind::SpamLinks, &hello),
        "hello-sun: hidden links to cheap-casino.test",
    );
    checks.check(
        "an obfuscated script is quarantined with evidence",
        held(FindingKind::ObfuscatedScript, &about),
        "about: eval over decoded text",
    );
    checks.check(
        "the .php upload and a disguised file are quarantined with evidence",
        held(FindingKind::Executable, &planted) && held(FindingKind::MimeMismatch, &fake),
        "cache.php (server code), photo.jpg (not an image)",
    );
    // The crawler-only script shows only to a crawler: discovery's dual fetch of the live
    // site finds it, since the owner proved the domain (A-I9).
    let cloaked = entries.iter().find(|e| {
        e.findings.iter().any(|f| {
            f.kind == FindingKind::Cloaking
                && f.evidence
                    .iter()
                    .any(|ev| ev.detail.contains("scripts served only to the crawler"))
        })
    });
    checks.check(
        "crawler-only scripts are quarantined with evidence",
        cloaked.is_some_and(|e| e.subject == Subject::Item("crawl:/".into())),
        cloaked
            .and_then(|e| e.findings.first())
            .and_then(|f| f.evidence.first())
            .map(|e| e.detail.clone())
            .unwrap_or_else(|| "no cloaking finding".into()),
    );
    let cloaked_id = cloaked.map(|e| e.content_id.clone());
    let audit: Arc<dyn AuditRecorder> = recorder.clone();
    migration.quarantine.record_findings(&audit, &ctx.acting).await?;

    // Forms and the shop map to capabilities; comments and accounts to owner decisions.
    let features = &migration.features;
    let inquiry = features.capability("submit_inquiry");
    checks.check(
        "forms and the shop map to capabilities",
        inquiry.is_some_and(|c| {
            c.effect == CapabilityEffect::NonIdempotent
                && c.consent.as_ref().is_some_and(|r| r.scope.as_str() == "contact")
        }) && features.capability("get_quote").is_some()
            && features.capability("place_order").is_some_and(|c| c.requires_idempotency())
            && features.capability("search_site").is_some(),
        features
            .capabilities
            .iter()
            .map(|c| c.capability.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );

    // The owner decides the held pages: each is released from the sanitised conversion; the
    // planted executable is never put to a decision (A-I1).
    let owner = OwnerDecides::default();
    let mut decided: Vec<ContentId> =
        ["posts/hello-sun/en", "pages/about/en"].map(|id| ContentId(id.into())).to_vec();
    decided.extend(cloaked_id);
    for id in &decided {
        migration
            .quarantine
            .decide(
                id,
                &owner,
                "migrator",
                EntryPoint::SiteAgent { agent: "steward".into() },
                Some((&audit, &ctx.acting)),
            )
            .await?;
    }
    let executable = migration
        .quarantine
        .decide(
            &ContentId("media:/wp-content/uploads/2026/10/cache.php".into()),
            &owner,
            "migrator",
            EntryPoint::SiteAgent { agent: "steward".into() },
            None,
        )
        .await?;
    checks.check(
        "an executable is refused without a decision; held pages are the owner's",
        executable == QuarantineState::Refused
            && owner.asked.lock().unwrap().len() == decided.len(),
        format!("{} decisions asked", owner.asked.lock().unwrap().len()),
    );
    let excluded = migration.quarantine.excluded();
    let converted = adk_site::migrate::convert::to_content(
        &inventory,
        &adk_site::migrate::convert::ConvertOptions::new("en".parse()?).excluding(excluded.clone()),
    )?;

    // 6 Build with the owner's new theme: the business site, the migrated content, the
    // uploads at their URLs, the legacy redirects, the parity check on the launch gate.
    let design = DesignChoice::NewTheme(ThemeId::Harbour);
    // The owner's design choice is recorded in the migration audit (owner decision of
    // 2026-10-25): a new theme gates URL coverage, text and metadata; its look is approved
    // from the preview.
    record_design_choice(&audit, &ctx.acting, &design.parity()).await?;
    let mut site =
        Site::with(SiteKind::Business, "Example Solar", DOMAIN, &["en"], None, |config| {
            config.redirects = converted.redirects.clone();
        })
        .await?;
    site.base.documents = converted.documents.clone();
    site.base.capabilities = features.capabilities.iter().map(|d| d.capability.clone()).collect();
    let mut parity = MigrationParity::new(Arc::new(inventory.clone()))
        .excluding(excluded)
        .with_config(ParityConfig { top_n: 20, ..design.parity() });
    let chrome = ChromeScreenshotter::detect();
    if let Some(chrome) = chrome.clone() {
        parity = parity.with_legacy(live.url()).with_screenshotter(Arc::new(chrome));
    }
    site.gate = site.gate.clone().with_extension(Arc::new(parity));
    site.quarantine = Some(migration.quarantine.quarantine());
    site.runtime = site.assemble(Some(ThemeId::Harbour));
    let charter = site.charter("Example Solar", "Rooftop solar for homes in Nairobi.", "KES");
    site.commit(&charter, &[(Role::Owner, OWNER)]).await?;
    // A link to the page the crawl could not read (robots-disallowed /private/) became text
    // in conversion, so release 1 publishes as converted.
    checks.check(
        "links to pages the crawl could not read become text",
        converted.notes.iter().any(|n| n.contains("/private/"))
            && converted
                .items
                .iter()
                .all(|i| i.body.as_ref().is_none_or(|b| !b.0.contains("](/private/)"))),
        converted
            .notes
            .iter()
            .filter(|n| n.contains("could not read"))
            .cloned()
            .collect::<Vec<_>>()
            .join("; "),
    );
    let outcome = site
        .runtime
        .pipeline()
        .run(site.steward_change(converted.items.clone(), "migration"))
        .await?;
    let published = match outcome {
        adk_site::pipeline::PipelineOutcome::Published(published) => published,
        other => {
            return Err(anyhow!("release 1 was not published: {other:?}"));
        }
    };
    let gate = site.runtime.run_launch_gate().await?;
    let status = |check: GateCheck| gate.check(check).map(|c| c.status);
    checks.check(
        "zero broken internal links",
        status(GateCheck::Links) == Some(CheckStatus::Passed),
        format!("{:?}", gate.check(GateCheck::Links).map(|c| c.findings.len())),
    );
    let parity_check = gate.check(GateCheck::MigrationParity);
    checks.check(
        "text parity within thresholds on the top 20 pages; the new theme's looks reported, not gated",
        parity_check.is_some_and(|c| {
            c.findings.iter().all(|f| !f.blocking && f.rule == "perceptual-difference")
                && matches!(c.status, CheckStatus::Passed | CheckStatus::NotMeasured)
                && c.note.as_deref().is_some_and(|n| n.starts_with("new theme"))
        }),
        parity_check
            .map(|c| format!("{:?}: {}", c.status, c.note.clone().unwrap_or_default()))
            .unwrap_or_else(|| "no parity check".into()),
    );
    if chrome.is_none() {
        checks.gap("Chrome is not installed here, so the visual half of parity was not measured");
    }

    // The release on the host: zero server-side executables; every inventoried page is served
    // or redirected.
    let seq = published.staged.manifest.seq;
    let manifest: ReleaseManifest = serde_json::from_slice(
        &site.host.fetch(&manifest_path(seq)).await?.ok_or_else(|| anyhow!("a manifest"))?,
    )?;
    let executables: Vec<&String> = manifest
        .files
        .keys()
        .filter(|p| [".php", ".phtml", ".cgi", ".py", ".pl"].iter().any(|x| p.ends_with(x)))
        .collect();
    checks.check(
        "zero server-side executables (A-I1)",
        executables.is_empty()
            && !manifest.files.contains_key("/wp-content/uploads/2026/10/cache.php"),
        format!("{} files released, none executable", manifest.files.len()),
    );
    let redirects: BTreeSet<String> =
        converted.redirects.iter().filter(|r| r.status == 301).map(|r| r.from.clone()).collect();
    let held: BTreeSet<String> = migration
        .quarantine
        .entries
        .iter()
        .filter(|e| !matches!(e.state, QuarantineState::Released { .. }))
        .filter_map(|e| e.path.clone())
        .map(|p| format!("/{}", p.trim_start_matches('/')))
        .collect();
    let gone: BTreeSet<String> =
        converted.redirects.iter().filter(|r| r.status == 410).map(|r| r.from.clone()).collect();
    let mut missing = Vec::new();
    let mut legacy = Vec::new();
    for url in &inventory.urls {
        let path = url.path.split('?').next().unwrap_or_default().to_string();
        if url.status >= 400 || held.contains(&path) {
            continue;
        }
        let file = if path.ends_with('/') { format!("{path}index.html") } else { path.clone() };
        let served = manifest.files.contains_key(&file);
        let redirected = redirects.contains(&path) || url.status == 301;
        if legacy_runtime_path(&path) {
            // The legacy runtime's files and endpoints: redirected or answered 410 Gone.
            legacy.push(path.clone());
            if !redirected && !gone.contains(&path) {
                missing.push(path);
            }
        } else if !served && !redirected {
            missing.push(path);
        }
    }
    checks.check(
        "every inventoried URL is served or 301-redirected; legacy runtime paths answer 410",
        missing.is_empty() && !legacy.is_empty(),
        if missing.is_empty() {
            format!(
                "{} URLs: served or redirected ({} redirects); legacy runtime paths {legacy:?} \
                 redirected or gone; {} held in quarantine",
                inventory.urls.len(),
                redirects.len(),
                held.len(),
            )
        } else {
            format!("missing: {missing:?}")
        },
    );
    let records = recorder.records();
    // One record per quarantined subject, its findings and evidence digest inside.
    let expected = migration.quarantine.entries.len();
    let findings = records.iter().filter(|r| r.action == "site.migration.finding").count();
    let decisions: Vec<&serde_json::Value> = records
        .iter()
        .filter(|r| r.action == "site.migration.decided")
        .filter_map(|r| r.ext.get("site"))
        .collect();
    let quarantine_decisions = decisions.iter().filter(|v| v.get("subject").is_some()).count();
    let design_recorded =
        decisions.iter().any(|v| v["decision"] == "design" && v["design"] == "new_theme");
    checks.check(
        "findings and decisions are audited, the design choice among them",
        findings == expected && findings >= 4 && quarantine_decisions == decided.len() && design_recorded,
        format!(
            "{findings} subjects recorded with their findings, {quarantine_decisions} quarantine decisions, design recorded: {design_recorded}"
        ),
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;
    Ok(checks)
}
