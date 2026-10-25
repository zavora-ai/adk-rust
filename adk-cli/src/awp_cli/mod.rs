//! `adk-rust awp`: verify a domain, run the conformance kit, manage the root and delegated
//! keys, and migrate a 1.x `business.toml` (spec 3.5 §8.1).
//!
//! | Command | Library |
//! |---|---|
//! | `awp verify <domain>` | `adk_awp::verify::Verifier` over `GET` only, no redirects, Web Bot Auth signing, DNS through hickory, pins in a file |
//! | `awp conformance <url>` | `adk_awp::conformance::run` |
//! | `awp keys rotate`, `awp keys delegate` | `adk_awp::signing::keys` and `StaticBundle`: root operations at a terminal, never inside an agent |
//! | `awp migrate-config business.toml` | `adk_awp::v2::migrate::migrate_config` |

pub(crate) mod keyfile;
mod keys;
mod net;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_awp::conformance::{CheckStatus, ConformanceOptions, ConformanceReport};
use adk_awp::verify::pins::FilePinStore;
use adk_awp::verify::{Verifier, VerifyOptions, VerifyReport};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub(crate) use keys::KeysCommands;

/// `adk-rust awp` subcommands (spec 3.5 §8.1).
#[derive(clap::Subcommand, Debug, Clone)]
pub enum AwpCommands {
    /// Verify a site from what it publishes: the contract and keys against the DNS root, the
    /// A2A card, the site log, the current release and sampled pages; then its posture.
    ///
    /// Sends only `GET` requests for published documents. Exits 0 on a pass and 1 naming the
    /// broken link. Example: `adk-rust awp verify example.com`
    Verify(VerifyArgs),

    /// Run the AWP 2.0 conformance kit against a deployment, such as https://example.com or
    /// a local one at http://127.0.0.1:8080.
    ///
    /// Exits 0 when no requirement fails and 1 otherwise.
    Conformance(ConformanceArgs),

    /// Root and delegated keys. Root operations read the root key file at a terminal and
    /// never run inside an agent.
    Keys {
        #[command(subcommand)]
        command: KeysCommands,
    },

    /// Turn an AWP 1.x `business.toml` into site data and a draft charter for onboarding to
    /// confirm.
    ///
    /// Writes site.toml, capabilities.json, public-terms.json, charter.draft.json and
    /// contract.draft.json into --out, refusing to overwrite any of them.
    MigrateConfig(MigrateConfigArgs),
}

/// `adk-rust awp verify`.
#[derive(clap::Args, Debug, Clone)]
pub struct VerifyArgs {
    /// The domain, such as example.com
    pub domain: String,
    /// Read the domain's documents from this origin instead of https://<domain>: a local or
    /// staging deployment, such as http://127.0.0.1:8080
    #[arg(long, value_name = "URL")]
    pub origin: Option<String>,
    /// Check the root against this `_awp-root` record instead of DNS, for a deployment whose
    /// DNS is not live yet: "v=awp2; kid=…; jkt=…"
    #[arg(long, value_name = "RECORD")]
    pub anchor: Option<String>,
    /// Pages sampled against their release digests
    #[arg(long, default_value_t = adk_awp::verify::DEFAULT_SAMPLES)]
    pub samples: usize,
    /// Check live documents as of this time (RFC 3339) instead of now
    #[arg(long, value_name = "TIME")]
    pub at: Option<DateTime<Utc>>,
    /// The file that pins each domain's root and last checkpoint (default:
    /// <config dir>/adk-rust/awp-pins.json)
    #[arg(long, value_name = "FILE")]
    pub pins: Option<PathBuf>,
    /// Sign each request with Web Bot Auth using this private JWK file
    #[arg(long, value_name = "FILE", requires = "signature_agent")]
    pub sign_key: Option<PathBuf>,
    /// The origin whose /.well-known/http-message-signatures-directory publishes --sign-key
    #[arg(long, value_name = "ORIGIN", requires = "sign_key")]
    pub signature_agent: Option<String>,
    /// Print the report as JSON
    #[arg(long)]
    pub json: bool,
}

/// `adk-rust awp conformance`.
#[derive(clap::Args, Debug, Clone)]
pub struct ConformanceArgs {
    /// The deployment: https://example.com, or a local one such as http://127.0.0.1:8080
    pub url: String,
    /// The site's domain, when the deployment is not served at it (default: the contract's
    /// `site.domain`)
    #[arg(long)]
    pub domain: Option<String>,
    /// Check the root against this `_awp-root` record instead of DNS
    #[arg(long, value_name = "RECORD")]
    pub anchor: Option<String>,
    /// Also run the `GET` checks of the edge's routes; only against a deployment on this
    /// machine (A-I9)
    #[arg(long)]
    pub active: bool,
    /// Pages sampled (default: the verifier's 20)
    #[arg(long, default_value_t = 0)]
    pub samples: usize,
    /// Check live documents as of this time (RFC 3339) instead of now
    #[arg(long, value_name = "TIME")]
    pub at: Option<DateTime<Utc>>,
    /// Print the results as JSON
    #[arg(long)]
    pub json: bool,
}

/// `adk-rust awp migrate-config`.
#[derive(clap::Args, Debug, Clone)]
pub struct MigrateConfigArgs {
    /// The AWP 1.x configuration
    pub business_toml: PathBuf,
    /// Where to write the site data and the draft charter
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
}

/// Runs an `adk-rust awp` command.
pub async fn run(command: AwpCommands) -> Result<()> {
    match command {
        AwpCommands::Verify(args) => {
            if !verify(args).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        AwpCommands::Conformance(args) => {
            if !conformance(args).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        AwpCommands::Keys { command } => keys::run(command).await,
        AwpCommands::MigrateConfig(args) => migrate_config(&args.business_toml, &args.out),
    }
}

fn default_pins() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("no configuration directory; pass --pins FILE")?
        .join("adk-rust")
        .join("awp-pins.json"))
}

fn normalised_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || domain.contains('/') || domain.contains(':') {
        bail!("`{domain}` is not a domain; give a host name such as example.com");
    }
    Ok(domain)
}

/// `adk-rust awp verify` (spec 3.5 §7.7, S13). Returns whether the site passed.
async fn verify(args: VerifyArgs) -> Result<bool> {
    let domain = normalised_domain(&args.domain)?;
    let identity =
        net::Identity { key: args.sign_key.as_deref(), agent: args.signature_agent.as_deref() };
    let fetcher = net::fetcher(&domain, args.origin.as_deref(), &identity)?;
    let pins = match args.pins {
        Some(path) => path,
        None => default_pins()?,
    };
    let verifier = Verifier::new(
        fetcher,
        net::resolver(&domain, args.anchor.as_deref()),
        Arc::new(FilePinStore::new(pins)),
    )
    .with_options(VerifyOptions { samples: args.samples, now: args.at });
    let report = verifier.verify(&domain).await;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&verify_json(&report))?);
    } else {
        print_verify(&report, args.origin.as_deref(), args.anchor.is_some());
    }
    Ok(report.passed())
}

fn verify_json(report: &VerifyReport) -> Value {
    let posture = &report.posture;
    json!({
        "domain": report.domain,
        "passed": report.passed(),
        "failure": report.failure.as_ref().map(|f| json!({ "name": f.name, "detail": f.detail })),
        "releaseSeq": report.release_seq,
        "sampled": report.sampled,
        "recovery": report.recovery,
        "posture": {
            "profile": posture.profile,
            "serverCode": posture.server_code,
            "identity": posture.identity,
            "a2a": posture.a2a,
            "mcp": posture.mcp,
            "usage": posture.usage,
            "robotsContentUsage": posture.robots_content_usage,
            "securityTxt": posture.security_txt,
            "hsts": posture.hsts,
            "csp": posture.csp,
        },
    })
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_verify(report: &VerifyReport, origin: Option<&str>, offline_anchor: bool) {
    match &report.failure {
        None => println!("awp verify {}: PASS", report.domain),
        Some(failure) => println!("awp verify {}: FAIL at {failure}", report.domain),
    }
    if let Some(origin) = origin {
        println!("  read from {origin}");
    }
    let anchor = if offline_anchor { "the --anchor record (not DNS)" } else { "DNS" };
    println!("  root anchored by {anchor}");
    if report.recovery {
        println!("  the root changed without the pinned root's endorsement: a recovery");
    }
    if let Some(seq) = report.release_seq {
        println!("  release {seq}, {} files sampled", report.sampled.len());
    }
    let posture = &report.posture;
    let profile = posture
        .profile
        .and_then(|profile| serde_json::to_value(profile).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("posture");
    println!("  profile       {profile}");
    match posture.server_code.is_empty() {
        true => println!("  server code   none seen"),
        false => {
            for note in &posture.server_code {
                println!("  server code   {note}");
            }
        }
    }
    let identity =
        if posture.identity.is_empty() { "none".into() } else { posture.identity.join(", ") };
    println!("  identity      {identity}");
    println!("  A2A card      {}", if posture.a2a { "signed" } else { "none" });
    println!("  MCP           {}", if posture.mcp { "server card named" } else { "none" });
    println!("  usage terms   {}", posture.usage.as_deref().unwrap_or("not stated"));
    println!("readiness");
    println!("  robots.txt Content-Usage  {}", yes_no(posture.robots_content_usage));
    println!("  security.txt (current)    {}", yes_no(posture.security_txt));
    println!("  HSTS                      {}", yes_no(posture.hsts));
    println!("  CSP on pages              {}", yes_no(posture.csp));
}

/// `adk-rust awp conformance` (spec 3.5 §7.7 "Conformance kit"). Returns whether no
/// requirement failed.
async fn conformance(args: ConformanceArgs) -> Result<bool> {
    let url = url::Url::parse(&args.url)
        .with_context(|| format!("`{}` is not a URL such as https://example.com", args.url))?;
    if !matches!(url.scheme(), "https" | "http") {
        bail!("conformance reads http or https deployments, not {}", url.scheme());
    }
    let local = net::is_loopback(&url);
    if args.active && !local {
        bail!(
            "--active probes the edge's routes, which runs only against a deployment whose \
             control you proved (spec 3.5 A-I9); run it against the deployment on this \
             machine, such as http://127.0.0.1:8080"
        );
    }
    let origin = url.origin().ascii_serialization();
    let domain = match &args.domain {
        Some(domain) => normalised_domain(domain)?,
        None => contract_domain(&origin)
            .await
            .unwrap_or_else(|| url.host_str().unwrap_or_default().to_ascii_lowercase()),
    };
    let through = (origin != format!("https://{domain}")).then_some(origin.as_str());
    let fetcher = net::fetcher(&domain, through, &net::Identity { key: None, agent: None })?;
    let options =
        ConformanceOptions { active_edge_checks: args.active, samples: args.samples, now: args.at };
    let report = adk_awp::conformance::run(
        fetcher,
        net::resolver(&domain, args.anchor.as_deref()),
        &domain,
        &options,
    )
    .await;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&conformance_json(&report))?);
    } else {
        print_conformance(&report, through);
    }
    Ok(report.passed())
}

/// The `site.domain` of the contract a deployment serves.
async fn contract_domain(origin: &str) -> Option<String> {
    use adk_awp::verify::fetch::Fetcher as _;
    let fetcher = adk_awp::verify::fetch::HttpFetcher::new().ok()?;
    let url = format!("{origin}{}", awp_types::v2::contract::CONTRACT_PATH);
    let fetched = fetcher.get(&url, &axum::http::HeaderMap::new()).await.ok()?;
    let contract: Value = serde_json::from_slice(&fetched.body).ok()?;
    contract["site"]["domain"].as_str().map(str::to_ascii_lowercase)
}

fn status_label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "pass",
        CheckStatus::Fail => "FAIL",
        _ => "skipped",
    }
}

fn conformance_json(report: &ConformanceReport) -> Value {
    json!({
        "domain": report.domain,
        "profile": report.profile,
        "passed": report.passed(),
        "results": report.results.iter().map(|result| json!({
            "id": result.id,
            "requirement": result.requirement,
            "status": status_label(result.status).to_ascii_lowercase(),
            "detail": result.detail,
        })).collect::<Vec<_>>(),
    })
}

fn print_conformance(report: &ConformanceReport, through: Option<&str>) {
    let verdict = if report.passed() { "PASS" } else { "FAIL" };
    let profile = report
        .profile
        .and_then(|profile| serde_json::to_value(profile).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown profile".into());
    println!("awp conformance {} ({profile}): {verdict}", report.domain);
    if let Some(origin) = through {
        println!("  read from {origin}");
    }
    for result in &report.results {
        let detail = if result.detail.is_empty() {
            String::new()
        } else {
            format!(" — {}", result.detail)
        };
        println!(
            "  {:<4} {:<7} {}{detail}",
            result.id,
            status_label(result.status),
            result.requirement
        );
    }
}

/// `adk-rust awp migrate-config` (spec 3.5 §4.1, §8.1).
fn migrate_config(business_toml: &Path, out: &Path) -> Result<()> {
    let loader = adk_awp::BusinessContextLoader::from_file(business_toml)
        .with_context(|| format!("cannot read {}", business_toml.display()))?;
    let context = loader.load();
    let migration = adk_awp::v2::migrate::migrate_config(&context, uuid::Uuid::new_v4());
    let config = adk_site::content::SiteConfig {
        description: Some(context.site_description.clone()).filter(|d| !d.trim().is_empty()),
        ..adk_site::content::SiteConfig::new(
            migration.site.name.clone(),
            migration.site.domain.clone(),
            match migration.site.languages.is_empty() {
                true => vec!["en".parse().map_err(|e| anyhow::anyhow!("{e}"))?],
                false => migration.site.languages.clone(),
            },
        )
    };
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("site.toml", toml::to_string_pretty(&config)?.into_bytes()),
        ("capabilities.json", pretty(&migration.capabilities)?),
        ("public-terms.json", pretty(&migration.charter.public)?),
        ("charter.draft.json", pretty(&migration.charter)?),
        ("contract.draft.json", pretty(&migration.contract)?),
    ];
    for (name, _) in &files {
        let path = out.join(name);
        if path.exists() {
            bail!("{} exists; migrate-config never overwrites a file", path.display());
        }
    }
    std::fs::create_dir_all(out).with_context(|| format!("cannot create {}", out.display()))?;
    for (name, bytes) in &files {
        let path = out.join(name);
        std::fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    }
    println!(
        "Migrated {} ({}) to AWP 2.0 site data in {}:",
        migration.site.name,
        migration.site.domain,
        out.display()
    );
    for (name, _) in &files {
        println!("  {name}");
    }
    println!(
        "{} capabilities; the charter is a version 0 draft with no owner, and the contract is \
         unsigned until onboarding confirms them.",
        migration.capabilities.len()
    );
    if !migration.warnings.is_empty() {
        println!("Confirm during onboarding:");
        for warning in &migration.warnings {
            println!("  - {warning}");
        }
    }
    Ok(())
}

/// Pretty JSON with a final newline.
pub(crate) fn pretty<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
