//! `adk-rust site migrate` and `site freeze`: an existing site moved onto the project, or
//! frozen in hours (spec 3.5 §7.8 "Sources", "Sanitiser", "Feature mapping", "Freeze";
//! §7.18 stages 1–3; A-I1, A-I9, A-I10).
//!
//! Every request goes through adk-site's signed, robots-respecting, rate-limited client
//! with the site's Web Bot Auth key. A domain whose control the owner has not proved gets
//! polite public reads only; a freeze needs the proof. Nothing on the legacy site is changed
//! or deleted, and its export is archived encrypted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{ActingContext, SubjectKind};
use adk_site::content::{ContentStore as _, FsContentStore};
use adk_site::migrate::archive::{ArchiveKey, LegacyArchive};
use adk_site::migrate::claim::{DnsResolver, DomainClaim, SystemDnsResolver};
use adk_site::migrate::convert::{ConvertOptions, to_content};
use adk_site::migrate::detect::Detectors;
use adk_site::migrate::freeze::{Freeze, FreezeConfig};
use adk_site::migrate::http::{CrawlSigner, HttpClient};
use adk_site::migrate::mapping::map_features;
use adk_site::migrate::parity::{
    ChromeScreenshotter, MigrationParity, ParityConfig, SiteDesign, design_choice_fields,
};
use adk_site::migrate::quarantine::QuarantineBook;
use adk_site::migrate::sources::{
    Crawl, DockerCliDriver, IsolatedReplay, ReplaySpec, WordPressRest, Wxr,
};
use adk_site::migrate::{ClaimScope, SiteInventory, SiteSource as _, SourceContext};
use anyhow::{Context, Result, bail};
use awp_types::CapabilityV2;
use chrono::Utc;
use serde_json::json;
use url::Url;

use super::project::{Site, SitePaths};
use super::{confirm, person};

/// The migrated inventory, beside the migration report.
const INVENTORY: &str = "inventory.json";

/// The launch gate's migration parity for a migrated project (spec 3.5 §7.8.1): the inventory
/// and the design choice the last `site migrate` recorded, with the subjects it quarantined
/// exempt; `None` before any migration. With `legacy`, the looks are compared with the legacy
/// site served there, in headless Chrome when it is installed.
pub(super) fn parity(paths: &SitePaths, legacy: Option<&Url>) -> Result<Option<MigrationParity>> {
    let dir = paths.migration();
    let Ok(bytes) = std::fs::read(dir.join("report.json")) else { return Ok(None) };
    let report: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| {
        format!("{} is not a migration report", dir.join("report.json").display())
    })?;
    let inventory = match std::fs::read(dir.join(INVENTORY)) {
        Ok(bytes) => serde_json::from_slice::<SiteInventory>(&bytes).with_context(|| {
            format!("{} is not a site inventory", dir.join(INVENTORY).display())
        })?,
        Err(_) => {
            eprintln!(
                "note: migration parity is not checked: {} predates the recorded inventory; \
                 run `adk-rust site migrate` again to record it",
                dir.join("report.json").display()
            );
            return Ok(None);
        }
    };
    let design: SiteDesign =
        report["design"]["design"].as_str().unwrap_or(SiteDesign::Keep.as_str()).parse()?;
    let quarantine: QuarantineBook =
        serde_json::from_value(report["quarantine"].clone()).unwrap_or_default();
    let mut parity = MigrationParity::new(Arc::new(inventory))
        .excluding(quarantine.excluded())
        .with_design(design);
    if let Some(legacy) = legacy {
        parity = parity.with_legacy(legacy.clone());
        match ChromeScreenshotter::detect() {
            Some(chrome) => parity = parity.with_screenshotter(Arc::new(chrome)),
            None => eprintln!(
                "note: no Chrome or Chromium found; the looks are not compared with {legacy}"
            ),
        }
    }
    Ok(Some(parity))
}

/// `adk-rust site migrate`.
#[derive(clap::Args, Debug, Clone)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["url", "wxr", "backup"])))]
pub struct MigrateArgs {
    /// The legacy site's URL: its WordPress REST API when it has one, else a crawl
    pub url: Option<String>,
    /// A WordPress export file (WXR)
    #[arg(long, value_name = "FILE")]
    pub wxr: Option<PathBuf>,
    /// With --wxr: the uploads directory (wp-content/uploads), so files no attachment names
    /// come too
    #[arg(long, value_name = "DIR", requires = "wxr")]
    pub uploads: Option<PathBuf>,
    /// A backup replayed in isolated containers: a directory with the SQL dump (*.sql) and
    /// uploads/; needs Docker and a proven domain
    #[arg(long, value_name = "DIR", requires = "origin")]
    pub backup: Option<PathBuf>,
    /// With --backup: the legacy site's public origin, such as <https://example.com>
    #[arg(long, value_name = "URL")]
    pub origin: Option<String>,
    /// The currency of a shop's prices in a WordPress export
    #[arg(long)]
    pub currency: Option<String>,
    /// Replace items and files the project holds already
    #[arg(long)]
    pub replace: bool,
    /// The owner's design choice (spec 3.5 §7.8.1): `keep` gates the launch on looking the
    /// same (perceptual difference under 0.05); `new-theme` gates URL coverage, text and
    /// metadata only, and you approve the new look from the preview
    #[arg(long, value_name = "CHOICE", default_value = "keep", value_parser = ["keep", "new-theme"])]
    pub design: String,
    /// Test fixtures only: TXT records for domain claims, as JSON `{"name": ["value"]}`
    #[cfg(feature = "scripted-model")]
    #[arg(long, hide = true, value_name = "FILE")]
    pub dns_fixture: Option<PathBuf>,
}

/// `adk-rust site freeze`.
#[derive(clap::Args, Debug, Clone)]
pub struct FreezeArgs {
    /// The legacy site's public origin, such as <https://example.com>
    pub url: String,
    /// Where the crawler reaches the legacy server privately, such as <http://10.0.0.5:8080>;
    /// it reaches nothing else
    #[arg(long, value_name = "URL")]
    pub vantage: String,
    /// Where the edge serves capabilities: frozen forms post to
    /// {edge}/awp/v2/capabilities/{name} (default: the site's origin)
    #[arg(long, value_name = "URL")]
    pub edge: Option<String>,
    /// Publish without asking
    #[arg(long, short)]
    pub yes: bool,
    /// Test fixtures only: TXT records for domain claims, as JSON `{"name": ["value"]}`
    #[cfg(feature = "scripted-model")]
    #[arg(long, hide = true, value_name = "FILE")]
    pub dns_fixture: Option<PathBuf>,
}

fn acting(config: &adk_site::content::SiteConfig) -> ActingContext {
    ActingContext::derived(&config.domain, "migrator", &person(), SubjectKind::Human)
}

/// The site's crawler: signed with its Web Bot Auth key, published at its origin.
fn signer(paths: &SitePaths, origin: &str) -> Result<Arc<dyn CrawlSigner>> {
    let key = Arc::new(paths.web_bot_auth_key()?.web_bot_auth()?);
    let signer = adk_auth::web_bot_auth::HttpSignatureSigner::builder(key, origin)
        .build()
        .map_err(|error| anyhow::anyhow!("the site's Web Bot Auth signer: {error}"))?;
    Ok(Arc::new(signer))
}

/// The DNS a claim is checked against: the system's, or a test fixture's.
fn dns(fixture: Option<&Path>) -> Result<Arc<dyn DnsResolver>> {
    let Some(path) = fixture else {
        return Ok(Arc::new(SystemDnsResolver));
    };
    let zone: std::collections::BTreeMap<String, Vec<String>> = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    )?;
    Ok(Arc::new(FixtureDns(zone)))
}

#[derive(Debug)]
struct FixtureDns(std::collections::BTreeMap<String, Vec<String>>);

#[async_trait::async_trait]
impl DnsResolver for FixtureDns {
    async fn lookup(
        &self,
        name: &str,
        _kind: adk_site::migrate::claim::RecordKind,
    ) -> adk_site::migrate::Result<Vec<String>> {
        Ok(self.0.get(name).cloned().unwrap_or_default())
    }
}

/// Proves control of `domain` with the claim kept in `data/claims/`, made on first use
/// (spec 3.5 §7.18 stage 1, A-I9): by its DNS record, else its well-known file.
async fn claim(
    paths: &SitePaths,
    domain: &str,
    dns: &dyn DnsResolver,
    http: Option<&HttpClient>,
) -> Result<ClaimScope> {
    let path = paths.claims().join(format!("{domain}.json"));
    let claim = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<DomainClaim>(&bytes)
            .with_context(|| format!("{} is not a domain claim", path.display()))?,
        Err(_) => {
            let claim = DomainClaim::new(domain, Utc::now())?;
            std::fs::create_dir_all(paths.claims())?;
            std::fs::write(&path, serde_json::to_vec_pretty(&claim)?)
                .with_context(|| format!("cannot write {}", path.display()))?;
            claim
        }
    };
    let now = Utc::now();
    let proven = match claim.verify_dns(dns, now).await {
        Ok(proven) => Some(proven),
        Err(_) => match http {
            Some(http) => claim.verify_well_known(http, now).await.ok(),
            None => None,
        },
    };
    match proven {
        Some(proven) => {
            println!("Control of {domain} is proven ({:?}).", proven.method());
            Ok(ClaimScope::Proven(proven))
        }
        None => {
            let (name, value) = claim.dns_record();
            let (file, body) = claim.well_known();
            println!("Control of {domain} is not proven yet; only polite public reads run.");
            println!("To prove it, publish either:");
            println!("  {name} TXT \"{value}\"");
            println!("  https://{domain}{file} serving `{body}`");
            Ok(ClaimScope::Polite)
        }
    }
}

fn host_of(url: &Url) -> Result<String> {
    Ok(url.host_str().context("the URL names no host")?.to_ascii_lowercase())
}

/// `adk-rust site migrate`.
pub(super) async fn migrate(paths: &SitePaths, args: MigrateArgs) -> Result<()> {
    let config = paths.config()?;
    let design: SiteDesign = args.design.parse()?;
    #[cfg(feature = "scripted-model")]
    let fixture = args.dns_fixture.as_deref();
    #[cfg(not(feature = "scripted-model"))]
    let fixture = None;
    let site_origin = config.base_url();
    let signer = signer(paths, &site_origin)?;
    let http = Arc::new(HttpClient::builder().signer(signer.clone()).build()?);
    let mut context = SourceContext::new(acting(&config), http.clone());
    let mut export: Option<(String, Vec<u8>)> = None;
    let inventory: SiteInventory = match (&args.url, &args.wxr, &args.backup) {
        (Some(url), _, _) => {
            let url = Url::parse(url).with_context(|| format!("`{url}` is not a URL"))?;
            let scope = claim(paths, &host_of(&url)?, dns(fixture)?.as_ref(), Some(&http)).await?;
            context = context.with_scope(scope);
            match WordPressRest::new(url.clone()).inventory(&context).await {
                Ok(inventory) if !inventory.items.is_empty() => inventory,
                _ => {
                    println!("No WordPress REST API answered; crawling {url} instead.");
                    Crawl::new(url).inventory(&context).await?
                }
            }
        }
        (None, Some(file), _) => {
            let bytes =
                std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
            let mut source = Wxr::from_bytes(bytes.clone());
            if let Some(uploads) = &args.uploads {
                source = source.with_uploads(uploads);
            }
            if let Some(currency) = &args.currency {
                source = source.with_currency(currency);
            }
            let name =
                file.file_name().map_or("export.xml".into(), |n| n.to_string_lossy().into_owned());
            export = Some((name, bytes));
            source.inventory(&context).await?
        }
        (None, None, Some(dir)) => {
            let origin = args.origin.as_deref().context("--backup needs --origin")?;
            let public = Url::parse(origin).with_context(|| format!("`{origin}` is not a URL"))?;
            let scope =
                claim(paths, &host_of(&public)?, dns(fixture)?.as_ref(), Some(&http)).await?;
            context = context.with_scope(scope);
            let dump = std::fs::read_dir(dir)
                .with_context(|| format!("cannot read {}", dir.display()))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .find(|path| path.extension().is_some_and(|e| e == "sql"))
                .with_context(|| format!("{} holds no SQL dump (*.sql)", dir.display()))?;
            let uploads = dir.join("uploads");
            let spec = ReplaySpec::wordpress(origin, &dump, &uploads);
            let replay = IsolatedReplay::new(Arc::new(DockerCliDriver::default()), spec)
                .with_signer(signer.clone());
            let inventory = replay.inventory(&context).await?;
            let name =
                dump.file_name().map_or("dump.sql".into(), |n| n.to_string_lossy().into_owned());
            export = Some((
                name,
                std::fs::read(&dump).with_context(|| format!("cannot read {}", dump.display()))?,
            ));
            inventory
        }
        (None, None, None) => bail!("give the legacy site's URL, --wxr FILE or --backup DIR"),
    };

    let detectors = Detectors::new();
    let mut findings = detectors.scan_inventory(&inventory).await;
    // The dual fetch reads the live site, so it runs for a proven domain only (A-I9).
    let cloaking = detectors.scan_cloaking(&context, &inventory, 20).await;
    findings.extend(cloaking.findings);
    let options = ConvertOptions::new(config.default_language().clone());
    let first = to_content(&inventory, &options)?;
    let book = QuarantineBook::new(findings, &first);
    let converted = to_content(&inventory, &options.clone().excluding(book.excluded()))?;
    let features = map_features(&inventory);

    let store = FsContentStore::new(paths.root());
    let (mut written, mut kept) = (0, 0);
    for item in &converted.items {
        if !args.replace && store.get(&item.id).await?.is_some() {
            kept += 1;
            continue;
        }
        store.put(item).await?;
        written += 1;
    }
    let mut files = 0;
    for (path, bytes) in &converted.documents {
        let relative = path.trim_start_matches('/');
        if relative.is_empty() || relative.split('/').any(|part| part == "..") {
            continue;
        }
        let target = paths.static_dir().join(relative);
        if target.exists() && !args.replace {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)
            .with_context(|| format!("cannot write {}", target.display()))?;
        files += 1;
    }
    let redirects = merge_redirects(paths, &converted.redirects)?;
    let capabilities = merge_capabilities(
        paths,
        features.capabilities.iter().map(|draft| draft.capability.clone()),
    )?;
    std::fs::create_dir_all(paths.migration())?;
    let report = json!({
        "migratedAt": Utc::now(),
        "design": design_choice_fields(&ParityConfig::for_design(design)),
        "source": inventory.source,
        "origin": inventory.origin,
        "items": converted.items.len(),
        "quarantine": book,
        "decisions": features.decisions,
        "capabilities": features.capabilities,
        "dropped": converted.dropped,
        "notes": converted.notes,
        "inventoryNotes": inventory.notes,
        "cloaking": { "checked": cloaking.checked, "notes": cloaking.notes },
    });
    let report_path = paths.migration().join("report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    // The inventory is what the launch gate's parity check compares the new site with.
    std::fs::write(paths.migration().join(INVENTORY), serde_json::to_vec(&inventory)?)?;
    if let Some((name, bytes)) = &export {
        let receipt = archive(paths, &config.domain, name, bytes).await?;
        println!(
            "Archived the legacy export {name} encrypted (key {}); it is never deleted (A-I10).",
            receipt.key_id
        );
    }
    println!(
        "Migrated {} items ({written} written, {kept} kept as they were), {files} files, \
         {redirects} new redirects, {capabilities} new capability drafts.",
        converted.items.len()
    );
    if !book.entries.is_empty() {
        println!("Quarantined, held out until you decide each one:");
        for entry in &book.entries {
            let kinds: BTreeSet<String> = entry
                .findings
                .iter()
                .map(|finding| {
                    serde_json::to_value(finding.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect();
            println!(
                "  {} — {}",
                entry.content_id,
                kinds.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
    }
    if !features.decisions.is_empty() {
        println!("Decisions for you:");
        for decision in &features.decisions {
            println!(
                "  {} — {} (until you decide: {})",
                decision.id, decision.question, decision.default
            );
        }
    }
    println!(
        "Design: {}.",
        if design.gates_looks() {
            "kept; the launch is gated on looking the same (perceptual difference under 0.05)"
        } else {
            "a new theme; the launch is gated on URL coverage, text and metadata, and you approve the look from the preview"
        }
    );
    println!(
        "Report: {}. Next: `adk-rust site build`, then `adk-rust site publish`.",
        report_path.display()
    );
    Ok(())
}

/// Adds `redirects` the configuration lacks to `site.toml`; returns how many.
fn merge_redirects(paths: &SitePaths, redirects: &[awp_types::Redirect]) -> Result<usize> {
    let mut config = paths.config()?;
    let known: BTreeSet<String> = config.redirects.iter().map(|r| r.from.clone()).collect();
    let new: Vec<awp_types::Redirect> =
        redirects.iter().filter(|r| !known.contains(&r.from)).cloned().collect();
    if new.is_empty() {
        return Ok(0);
    }
    config.redirects.extend(new.iter().cloned());
    config.validate()?;
    std::fs::write(paths.site_toml(), toml::to_string_pretty(&config)?)
        .with_context(|| format!("cannot write {}", paths.site_toml().display()))?;
    Ok(new.len())
}

/// Adds capability drafts `capabilities.json` lacks; returns how many.
fn merge_capabilities(
    paths: &SitePaths,
    drafts: impl Iterator<Item = CapabilityV2>,
) -> Result<usize> {
    let path = paths.root().join("capabilities.json");
    let mut capabilities: Vec<CapabilityV2> = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a list of capabilities", path.display()))?,
        Err(_) => Vec::new(),
    };
    let before = capabilities.len();
    for draft in drafts {
        if !capabilities.iter().any(|c| c.name == draft.name) {
            capabilities.push(draft);
        }
    }
    let added = capabilities.len() - before;
    if added > 0 {
        std::fs::write(&path, crate::awp_cli::pretty(&capabilities)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(added)
}

/// Stores the legacy export encrypted under `data/archive/` with the site's archive key,
/// made on first use (A-I10).
async fn archive(
    paths: &SitePaths,
    domain: &str,
    name: &str,
    bytes: &[u8],
) -> Result<adk_site::migrate::archive::ArchiveReceipt> {
    use std::io::Write as _;
    let key_path = paths.archive_key();
    let key = match std::fs::read_to_string(&key_path) {
        Ok(text) => {
            let raw = zeroize::Zeroizing::new(
                (0..text.trim().len() / 2)
                    .map(|i| u8::from_str_radix(&text.trim()[i * 2..i * 2 + 2], 16))
                    .collect::<std::result::Result<Vec<u8>, _>>()
                    .with_context(|| format!("{} is not a hex key", key_path.display()))?,
            );
            let raw: [u8; 32] = raw
                .as_slice()
                .try_into()
                .with_context(|| format!("{} is not a 32-byte key", key_path.display()))?;
            ArchiveKey::from_bytes(raw)
        }
        Err(_) => {
            let raw = crate::awp_cli::keyfile::random_secret()?;
            if let Some(parent) = key_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = crate::awp_cli::keyfile::create_owner_only(&key_path)?;
            let hex: zeroize::Zeroizing<String> =
                zeroize::Zeroizing::new(raw.iter().map(|b| format!("{b:02x}")).collect());
            writeln!(file, "{}", hex.as_str())?;
            ArchiveKey::from_bytes(*raw)
        }
    };
    let service = Arc::new(
        adk_artifact::FileArtifactService::new(paths.data().join("archive"))
            .context("cannot open the legacy archive")?,
    );
    let archive = LegacyArchive::new(service, domain, person(), "migration", key);
    Ok(archive.store(name, bytes, Utc::now(), None).await?)
}

/// `adk-rust site freeze` (spec 3.5 §7.8 "Freeze", S4).
pub(super) async fn freeze(paths: &SitePaths, args: FreezeArgs) -> Result<()> {
    super::refuse_chain(paths)?;
    let config = paths.config()?;
    #[cfg(feature = "scripted-model")]
    let fixture = args.dns_fixture.as_deref();
    #[cfg(not(feature = "scripted-model"))]
    let fixture = None;
    let public = Url::parse(&args.url).with_context(|| format!("`{}` is not a URL", args.url))?;
    let vantage =
        Url::parse(&args.vantage).with_context(|| format!("`{}` is not a URL", args.vantage))?;
    let edge = match &args.edge {
        Some(edge) => Url::parse(edge).with_context(|| format!("`{edge}` is not a URL"))?,
        None => Url::parse(&config.base_url())?,
    };
    let signer = signer(paths, &config.base_url())?;
    let scope = claim(paths, &host_of(&public)?, dns(fixture)?.as_ref(), None).await?;
    if matches!(scope, ClaimScope::Polite) {
        bail!(
            "a freeze crawls the legacy server privately, so it needs proof that you control {} \
             (spec 3.5 A-I9); publish the record above and run the freeze again",
            host_of(&public)?
        );
    }
    if !confirm(
        &format!("Freeze {public} from {vantage} and publish the snapshot as a signed release?"),
        args.yes,
    )? {
        bail!("nothing changed");
    }
    let http = Arc::new(HttpClient::builder().signer(signer.clone()).build()?);
    let context = SourceContext::new(acting(&config), http).with_scope(scope);
    let site =
        Site::open(paths.clone(), Arc::new(adk_site::pipeline::AutoApprove(super::approver())))
            .await?;
    for note in site.key_notes() {
        eprintln!("note: {note}");
    }
    let out = paths.work().join(format!("freeze-{}", Utc::now().timestamp()));
    let report =
        Freeze::new(FreezeConfig { public_origin: public.clone(), vantage, edge_origin: edge })
            .with_signer(signer)
            .run(&context, &out, site.previous.as_ref(), &site.log, site.host.as_ref())
            .await?;
    let added = merge_capabilities(paths, report.capabilities.iter().cloned())?;
    println!(
        "Froze {public}: release {} with {} files, {} redirects; {} forms ({} now post to the edge), \
         {added} new capability drafts.",
        report.release.manifest.seq,
        report.files,
        report.redirects.len(),
        report.forms.len(),
        report.forms.iter().filter(|form| form.capability.is_some()).count()
    );
    for (path, why) in report.excluded.iter().take(20) {
        println!("  left out {path}: {why}");
    }
    println!(
        "Every request went to {} only; the host now serves the snapshot and the old server can \
         leave the public path.",
        report.connections.first().map_or("the vantage origin", |c| c.origin.as_str())
    );
    Ok(())
}
