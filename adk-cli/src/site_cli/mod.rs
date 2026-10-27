//! `adk-rust site`: a site's releases, content and migration outside the web editor
//! (spec 3.5 §7.8, §7.8.2, §7.8.3, §8.1).
//!
//! | Command | Library |
//! |---|---|
//! | `site build`, `site publish` | `BuiltinGenerator`, `LaunchGate`, `ReleaseBuilder`, `SiteLog`, `DirectoryHost`; `PublishingPipeline::run` for named drafts and due schedules |
//! | `site rollback <seq>` | `UpdateEngine::rollback`, then `replay` one change at a time |
//! | `site log` | The site log and the manifests the host serves |
//! | `site content new <kind>` | A draft item in the content repository |
//! | `site schedule`, `site correct`, `site withdraw` | `PublishingPipeline::run`, `correct`, `withdraw` |
//! | `site revert <item> [--to <seq>]` | `UpdateEngine::revert` |
//! | `site batches`, `site feeds` | `UpdateEngine::batches` and `feeds`, through the engine's journal in `data/updates/` |
//! | `site migrate`, `site freeze` | `adk_site::migrate`: sources, detectors, quarantine, conversion, feature mapping, archive, `Freeze` |
//! | `site packs list\|install\|remove\|trust\|sign` | `adk_website::packs`: capability packs as signed bundles (spec 3.5 §7.23) |
//!
//! Every change is the act of the person at the terminal: they confirm it, or pass `--yes`.
//! A site whose charter names an approval chain for publication publishes only through its
//! approvals (A-I23).

mod migrate;
mod packs;
pub(crate) mod project;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use adk_site::content::{ContentHistory as _, ContentStore as _, HistoryMeta, SiteProject};
use adk_site::gate::{GateInput, GateMode, GateReport, LaunchGate, Quarantine};
use adk_site::generator::{BuiltinGenerator, SiteGenerator as _};
use adk_site::host::{HostTarget, SiteHost as _};
use adk_site::pipeline::{
    ApprovalDecision, ApprovalGate, ApprovalRequest, Change, EntryPoint, PageChange, PageDiff,
    PipelineOutcome, Withdrawal,
};
use adk_site::release::{ReleaseBuilder, ReleaseOptions, StagedDelta, manifest_path};
use adk_site::updates::{BatchRecord, FeedStatus, UpdateEngine};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, ReleaseManifest, ReleaseSource,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use project::{Site, SitePaths};

/// Which site a `site` command acts on.
#[derive(clap::Args, Debug, Clone)]
pub struct SiteTarget {
    /// The site project: site.toml, content/ and data/ (default: the current directory)
    #[arg(long, global = true, value_name = "DIR", default_value = ".")]
    pub site: PathBuf,
    /// The directory releases are published to (default: <site>/data/host)
    #[arg(long, global = true, value_name = "DIR")]
    pub host_dir: Option<PathBuf>,
}

/// `adk-rust site` subcommands (spec 3.5 §8.1).
#[derive(clap::Subcommand, Debug, Clone)]
pub enum SiteCommands {
    /// Move an existing site into this project: from its URL, a WordPress export, or a
    /// backup replayed in isolation. Nothing on the old site is changed or deleted.
    ///
    /// Example: `adk-rust site migrate --wxr export.xml`
    Migrate(migrate::MigrateArgs),

    /// Crawl the legacy site from a private address and publish the snapshot as a signed
    /// release, so the old server can leave the public path. Needs a proven domain.
    ///
    /// Example: `adk-rust site freeze https://example.com --vantage http://10.0.0.5:8080`
    Freeze(migrate::FreezeArgs),

    /// Build the site into data/work/build and run the launch gate on it; after
    /// `site migrate`, with the migration parity check.
    Build {
        /// Render drafts, items in review and future schedules, as a preview does
        #[arg(long)]
        preview: bool,
        /// Print the gate's report as JSON
        #[arg(long)]
        json: bool,
        /// After a migration, compare the looks with the legacy site served at this URL (a
        /// replay or a frozen snapshot), in headless Chrome
        #[arg(long, value_name = "URL")]
        legacy: Option<url::Url>,
    },

    /// Release the site: a signed release in the site log, published to the host. With item
    /// ids, publishes those drafts; scheduled items whose time has come publish too.
    ///
    /// Example: `adk-rust site publish posts/hello/en`
    Publish {
        /// Draft items to publish, such as posts/hello/en
        items: Vec<String>,
        /// Approve without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Roll the whole site back to an earlier release for an incident, then replay the
    /// changes made since, one by one.
    Rollback {
        /// The release to serve again
        seq: u64,
        /// Replay every later change without asking (`all`), or none (`none`)
        #[arg(long, value_enum)]
        replay: Option<ReplayChoice>,
        /// Roll back without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// The site log: every release, its source and files, and the checkpoint.
    Log {
        /// Print the releases as JSON
        #[arg(long)]
        json: bool,
    },

    /// Draft content.
    Content {
        #[command(subcommand)]
        command: ContentCommands,
    },

    /// Schedule a draft to publish at a time (RFC 3339, such as 2026-11-02T09:00:00Z).
    Schedule {
        /// The item, such as products/panel-400/en
        item: String,
        /// When it publishes
        time: DateTime<Utc>,
        /// Hold it until this time even if approved
        #[arg(long, value_name = "TIME")]
        embargo_until: Option<DateTime<Utc>>,
        /// Withdraw it at this time
        #[arg(long, value_name = "TIME")]
        expires_at: Option<DateTime<Utc>>,
        /// Put the previous version back at this time
        #[arg(long, value_name = "TIME")]
        revert_at: Option<DateTime<Utc>>,
        /// Approve without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Publish a correction with a visible note; both versions stay in the site log.
    Correct {
        /// The item
        item: String,
        /// The correction note shown on the page
        #[arg(long)]
        note: String,
        /// Set a field: FIELD=VALUE, VALUE as JSON or text; repeatable
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        sets: Vec<String>,
        /// Replace the body with this Markdown file
        #[arg(long, value_name = "FILE")]
        body: Option<PathBuf>,
        /// Approve without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Withdraw an item: its page answers 410 Gone or shows a notice.
    Withdraw {
        /// The item
        item: String,
        /// Answer 410 Gone
        #[arg(long, conflicts_with = "notice", required_unless_present = "notice")]
        gone: bool,
        /// Replace the page with this notice
        #[arg(long, value_name = "TEXT")]
        notice: Option<String>,
        /// Approve without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Publish one item's earlier version and keep every other change.
    Revert {
        /// The item
        item: String,
        /// The release whose version to restore (default: the one before its latest)
        #[arg(long, value_name = "SEQ")]
        to: Option<u64>,
        /// Revert without asking
        #[arg(long, short)]
        yes: bool,
    },

    /// Recent batches: their release, items, superseded versions, held items and why.
    Batches {
        /// Print the batches as JSON
        #[arg(long)]
        json: bool,
    },

    /// Feeds with their mappings, bounds and rates.
    Feeds {
        /// Print the feeds as JSON
        #[arg(long)]
        json: bool,
    },

    /// Capability packs: signed bundles of capabilities, handler configuration, playbooks
    /// and driver manifests, installed from publishers the site trusts.
    Packs {
        #[command(subcommand)]
        command: packs::PacksCommands,
    },
}

/// `adk-rust site content` subcommands.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum ContentCommands {
    /// Write a new draft item to the content repository.
    ///
    /// Example: `adk-rust site content new post --title "Opening hours"`
    New {
        /// The kind of content
        #[arg(value_enum)]
        kind: KindArg,
        /// Its title
        #[arg(long)]
        title: String,
        /// Its slug (default: from the title)
        #[arg(long)]
        slug: Option<String>,
        /// Its language (default: the site's first)
        #[arg(long)]
        language: Option<String>,
    },
}

/// A content kind (spec 3.5 §7.8.2).
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindArg {
    /// A standing page
    Page,
    /// A blog post
    Post,
    /// A news article
    Article,
    /// A product
    Product,
    /// An event
    Event,
    /// A notice
    Notice,
    /// A service
    Service,
    /// A person
    Person,
    /// A collection
    Collection,
}

impl KindArg {
    fn kind(self) -> ContentKind {
        match self {
            Self::Page => ContentKind::Page,
            Self::Post => ContentKind::Post,
            Self::Article => ContentKind::Article,
            Self::Product => ContentKind::Product,
            Self::Event => ContentKind::Event,
            Self::Notice => ContentKind::Notice,
            Self::Service => ContentKind::Service,
            Self::Person => ContentKind::Person,
            Self::Collection => ContentKind::Collection,
        }
    }
}

/// What `site rollback` does with the changes made since the release it restores.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayChoice {
    /// Replay them all
    All,
    /// Replay none
    None,
}

/// Runs an `adk-rust site` command.
pub async fn run(command: SiteCommands, target: SiteTarget) -> Result<()> {
    let paths = SitePaths::new(&target.site).with_host(target.host_dir);
    match command {
        SiteCommands::Migrate(args) => migrate::migrate(&paths, args).await,
        SiteCommands::Freeze(args) => migrate::freeze(&paths, args).await,
        SiteCommands::Build { preview, json, legacy } => build(&paths, preview, json, legacy).await,
        SiteCommands::Publish { items, yes } => publish(&paths, items, yes).await,
        SiteCommands::Rollback { seq, replay, yes } => rollback(&paths, seq, replay, yes).await,
        SiteCommands::Log { json } => log(&paths, json).await,
        SiteCommands::Content { command: ContentCommands::New { kind, title, slug, language } } => {
            content_new(&paths, kind.kind(), &title, slug, language).await
        }
        SiteCommands::Schedule { item, time, embargo_until, expires_at, revert_at, yes } => {
            let schedule = awp_types::Schedule {
                publish_at: Some(time),
                embargo_until,
                expires_at,
                revert_at,
            };
            schedule_item(&paths, &item, schedule, yes).await
        }
        SiteCommands::Correct { item, note, sets, body, yes } => {
            correct(&paths, &item, &note, &sets, body, yes).await
        }
        SiteCommands::Withdraw { item, gone, notice, yes } => {
            let how = match (gone, notice) {
                (_, Some(text)) => Withdrawal::Notice(text),
                _ => Withdrawal::Gone,
            };
            withdraw(&paths, &item, how, yes).await
        }
        SiteCommands::Revert { item, to, yes } => revert(&paths, &item, to, yes).await,
        SiteCommands::Batches { json } => batches(&paths, json),
        SiteCommands::Feeds { json } => feeds(&paths, json).await,
        SiteCommands::Packs { command } => packs::run(paths.root(), command).await,
    }
}

/// The person running the command.
pub(crate) fn person() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "owner".to_string())
}

/// The approver: the person at the terminal (spec 3.5 §7.8.2 step 4).
fn approver() -> String {
    format!("user:{}", person())
}

/// The proposer: the content repository the person edited (spec 3.5 §7.8.2 "Files and git").
fn proposer() -> String {
    format!("repository:{}", person())
}

fn entry() -> EntryPoint {
    EntryPoint::Repository { committer: person() }
}

/// Asks the person to confirm `question`, unless `yes`; without a terminal the answer is no.
pub(crate) fn confirm(question: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "{question} needs your confirmation at a terminal; pass --yes to confirm in a script"
        );
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// Refuses to publish alone when the charter names an approval chain (spec 3.5 A-I23).
fn refuse_chain(paths: &SitePaths) -> Result<()> {
    if let Some(roles) = paths.publish_chain() {
        bail!(
            "this site's charter requires {} to approve a publication, each a different person \
             (spec 3.5 A-I23); publish through the site's approvals, not alone from the CLI",
            roles.join(" and ")
        );
    }
    Ok(())
}

/// The release approval of the person at the terminal (spec 3.5 §7.8.2 step 4): the page
/// diff, then a yes or no.
struct TerminalApproval {
    yes: bool,
}

#[async_trait]
impl ApprovalGate for TerminalApproval {
    async fn decide(&self, request: &ApprovalRequest) -> adk_site::Result<ApprovalDecision> {
        print_diff(&request.diff);
        let question = format!("Publish {}?", describe_items(&request.items));
        match confirm(&question, self.yes) {
            Ok(true) => Ok(ApprovalDecision::Approved {
                approvers: vec![approver()],
                decision: Some(format!("adk-rust site by {}", approver())),
            }),
            Ok(false) => Ok(ApprovalDecision::Rejected {
                by: approver(),
                reason: "declined at the terminal".into(),
            }),
            Err(error) => {
                Ok(ApprovalDecision::Rejected { by: approver(), reason: error.to_string() })
            }
        }
    }
}

fn describe_items(items: &[ContentId]) -> String {
    match items {
        [] => "the site".to_string(),
        [one] => one.to_string(),
        many => format!("{} items", many.len()),
    }
}

fn print_diff(diff: &[PageDiff]) {
    let count = |change: PageChange| diff.iter().filter(|d| d.change == change).count();
    println!(
        "Pages: {} added, {} changed, {} removed",
        count(PageChange::Added),
        count(PageChange::Changed),
        count(PageChange::Removed)
    );
    for page in diff.iter().take(20) {
        let sign = match page.change {
            PageChange::Added => '+',
            PageChange::Changed => '~',
            PageChange::Removed => '-',
        };
        println!("  {sign} {}", page.path);
    }
    if diff.len() > 20 {
        println!("  … and {} more", diff.len() - 20);
    }
}

fn print_gate(report: &GateReport) {
    println!("Launch gate ({:?} tier): {}", report.mode, report.summary());
    for (check, finding) in report.blocking() {
        let place = finding
            .item
            .as_ref()
            .map(ToString::to_string)
            .or_else(|| finding.path.clone())
            .unwrap_or_default();
        println!("  {check:?} {place}: {} ({})", finding.message, finding.rule);
    }
}

fn print_outcome(outcome: &PipelineOutcome) -> Result<()> {
    match outcome {
        PipelineOutcome::Published(published) => {
            println!(
                "Published release {} to {} ({} files uploaded).",
                published.staged.manifest.seq,
                published.report.host,
                published.report.uploaded.len()
            );
            Ok(())
        }
        PipelineOutcome::Held(gate) => {
            print_gate(gate);
            bail!("the launch gate held the change; nothing was published")
        }
        PipelineOutcome::Pending { ticket, .. } => {
            println!("Waiting for approval ({ticket}); nothing was published yet.");
            Ok(())
        }
        PipelineOutcome::Rejected { by, reason } => {
            bail!("not published: {by} declined ({reason})")
        }
        PipelineOutcome::Scheduled { at } => {
            match at {
                Some(at) => println!(
                    "Scheduled. It publishes at {}; the site's scheduler, or the next \
                     `adk-rust site publish` after that time, releases it.",
                    at.to_rfc3339()
                ),
                None => println!("Scheduled."),
            }
            Ok(())
        }
    }
}

async fn open(paths: &SitePaths, yes: bool) -> Result<Site> {
    let site = Site::open(paths.clone(), Arc::new(TerminalApproval { yes })).await?;
    for note in site.key_notes() {
        eprintln!("note: {note}");
    }
    Ok(site)
}

/// The project as it would publish now, with the site's AWP documents.
async fn project(site: &Site, seq: u64, preview: bool) -> Result<SiteProject> {
    let mut project = SiteProject::load(site.paths.root(), Utc::now()).await?;
    project.documents = site.paths.documents()?;
    project.release_seq = seq;
    project.preview = preview;
    project.redirects_format = site.host.redirects_format();
    Ok(project)
}

/// The launch gate over a build, with the migration parity check when the project was
/// migrated (spec 3.5 §7.8.1); `legacy` is where the legacy site's looks are compared.
async fn gate(
    paths: &SitePaths,
    project: &SiteProject,
    out: &std::path::Path,
    report: &adk_site::generator::BuildReport,
    mode: GateMode,
    changed: Option<&BTreeSet<ContentId>>,
    legacy: Option<&url::Url>,
) -> Result<GateReport> {
    let quarantine = Quarantine::default();
    let mut gate = LaunchGate::new();
    if let Some(parity) = migrate::parity(paths, legacy)? {
        gate = gate.with_extension(Arc::new(parity));
    }
    Ok(gate
        .run(mode, &GateInput { project, out, report, changed, quarantine: &quarantine })
        .await?)
}

/// `adk-rust site build`.
async fn build(
    paths: &SitePaths,
    preview: bool,
    json: bool,
    legacy: Option<url::Url>,
) -> Result<()> {
    let site = open(paths, true).await?;
    let seq = site.log.size().await + 1;
    let project = project(&site, seq, preview).await?;
    let out = paths.work().join("build");
    let report = BuiltinGenerator.build(&project, &out).await?;
    let verdict =
        gate(paths, &project, &out, &report, GateMode::Full, None, legacy.as_ref()).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "out": out,
                "pages": report.pages.len(),
                "files": report.outputs.len(),
                "gate": verdict,
            }))?
        );
    } else {
        println!(
            "Built {} pages, {} files into {}{}.",
            report.pages.len(),
            report.outputs.len(),
            out.display(),
            if preview { " (preview: drafts shown)" } else { "" }
        );
        print_gate(&verdict);
    }
    if !verdict.passed() {
        std::process::exit(1);
    }
    Ok(())
}

/// Items whose working copy differs from the last version the history recorded, drafts and
/// items in review left out.
async fn changed_items(site: &Site) -> Result<Vec<ContentItem>> {
    let mut changed = Vec::new();
    for item in site.store.list().await? {
        if matches!(item.state, ContentState::Draft | ContentState::InReview) {
            continue;
        }
        let latest = site.history.versions(&item.id).await?.pop();
        if latest.is_none_or(|entry| entry.item != item) {
            changed.push(item);
        }
    }
    Ok(changed)
}

/// `adk-rust site publish`.
async fn publish(paths: &SitePaths, items: Vec<String>, yes: bool) -> Result<()> {
    refuse_chain(paths)?;
    let site = open(paths, yes).await?;
    // Scheduled items whose time has come.
    for outcome in site.pipeline.run_due(None).await? {
        print_outcome(&outcome)?;
    }
    if !items.is_empty() {
        let mut drafts = Vec::new();
        for id in &items {
            let id = ContentId(id.clone());
            let mut item = site
                .store
                .get(&id)
                .await?
                .with_context(|| format!("no item {id}; ids look like posts/hello/en"))?;
            if matches!(item.state, ContentState::Draft | ContentState::InReview) {
                item.state = ContentState::Approved;
            }
            drafts.push(item);
        }
        let outcome = site.pipeline.run(change(drafts, "publish")).await?;
        return print_outcome(&outcome);
    }
    publish_working_copy(&site, yes).await
}

fn change(items: Vec<ContentItem>, job: &str) -> Change {
    Change {
        items,
        media: Vec::new(),
        entry: entry(),
        proposer: proposer(),
        job: job.into(),
        action_class: "publish".into(),
    }
}

/// Releases the working copy as it stands: the launch gate's full tier before the first
/// release, its fast tier on the changed items after (spec 3.5 §7.8.1, §7.8.2).
async fn publish_working_copy(site: &Site, yes: bool) -> Result<()> {
    let previous = site.pipeline.previous().await.or_else(|| site.previous.clone());
    let out = site.paths.work().join("build");
    let changed = changed_items(site).await?;
    // Machine views carry the release number, so a build for the served release shows
    // whether anything else changed.
    if let Some(previous) = &previous
        && changed.is_empty()
    {
        let project = project(site, previous.manifest.seq, false).await?;
        let report = BuiltinGenerator.build(&project, &out).await?;
        let options = ReleaseOptions {
            source: ReleaseSource::Edit { job: "publish".into(), decision: None },
            generator: report.generator.clone(),
            charter_version: project.config.charter_version,
            redirects: report.redirects.clone(),
            written: None,
            force_snapshot: false,
        };
        let staged = ReleaseBuilder::new().stage(&out, Some(previous), options).await?;
        if staged.changed.is_empty() && staged.removed.is_empty() {
            println!(
                "Nothing changed since release {}; nothing to publish.",
                previous.manifest.seq
            );
            return Ok(());
        }
    }
    let seq = previous.as_ref().map_or(1, |p| p.manifest.seq + 1);
    let project = project(site, seq, false).await?;
    let report = BuiltinGenerator.build(&project, &out).await?;
    let ids: BTreeSet<ContentId> = changed.iter().map(|item| item.id.clone()).collect();
    let verdict = match &previous {
        None => gate(&site.paths, &project, &out, &report, GateMode::Full, None, None).await?,
        Some(_) => {
            gate(&site.paths, &project, &out, &report, GateMode::Fast, Some(&ids), None).await?
        }
    };
    if !verdict.passed() {
        print_gate(&verdict);
        bail!("the launch gate failed; nothing was published");
    }
    let options = |source: ReleaseSource| ReleaseOptions {
        source,
        generator: report.generator.clone(),
        charter_version: project.config.charter_version,
        redirects: report.redirects.clone(),
        written: None,
        force_snapshot: false,
    };
    let builder = ReleaseBuilder::new();
    let source = |decision: Option<String>| ReleaseSource::Edit { job: "publish".into(), decision };
    let staged = builder.stage(&out, previous.as_ref(), options(source(None))).await?;
    let before = previous.as_ref().map(|p| p.files.clone()).unwrap_or_default();
    let diff = page_diff(&before, &staged.files);
    let approval =
        TerminalApproval { yes }.decide(&approval_request(&ids, &staged.digest, diff)).await?;
    let decision = match approval {
        ApprovalDecision::Approved { decision, .. } => decision,
        ApprovalDecision::Rejected { reason, .. } => bail!("not published: {reason}"),
        ApprovalDecision::Pending { .. } => bail!("not published: waiting for approval"),
    };
    let staged =
        builder.release(&out, previous.as_ref(), options(source(decision)), &site.log).await?;
    site.record_release(&staged, &proposer(), None).await;
    let published = if !staged.manifest.is_snapshot() && site.host.limits().delta_upload {
        site.host.publish_delta(&StagedDelta::new(staged.clone())?, HostTarget::Production).await?
    } else {
        site.host.publish(&staged, HostTarget::Production).await?
    };
    site.record_release(&staged, &proposer(), Some((published.uploaded.len(), changed.len())))
        .await;
    for item in &changed {
        let meta = HistoryMeta {
            at: Some(Utc::now()),
            actor: Some(approver()),
            release_seq: Some(staged.manifest.seq),
            note: None,
        };
        site.history.record(item, meta).await?;
    }
    println!(
        "Published release {} ({}) to {}: {} files uploaded, {} items changed.",
        staged.manifest.seq,
        if staged.manifest.is_snapshot() { "snapshot" } else { "delta" },
        site.paths.host_dir().display(),
        published.uploaded.len(),
        changed.len()
    );
    Ok(())
}

fn approval_request(
    ids: &BTreeSet<ContentId>,
    digest: &awp_types::Digest,
    diff: Vec<PageDiff>,
) -> ApprovalRequest {
    ApprovalRequest {
        items: ids.iter().cloned().collect(),
        action_class: "publish".into(),
        proposer: proposer(),
        entry: entry(),
        preview_digest: digest.clone(),
        diff,
    }
}

fn page_diff(
    before: &BTreeMap<String, awp_types::FileEntry>,
    after: &BTreeMap<String, awp_types::FileEntry>,
) -> Vec<PageDiff> {
    let mut diff = Vec::new();
    for (path, entry) in after.iter().filter(|(path, _)| path.ends_with(".html")) {
        match before.get(path) {
            None => diff.push(PageDiff { path: path.clone(), change: PageChange::Added }),
            Some(old) if old != entry => {
                diff.push(PageDiff { path: path.clone(), change: PageChange::Changed });
            }
            Some(_) => {}
        }
    }
    for path in before.keys().filter(|p| p.ends_with(".html") && !after.contains_key(*p)) {
        diff.push(PageDiff { path: path.clone(), change: PageChange::Removed });
    }
    diff
}

/// The update engine over the site's pipeline, for owner requests: the person at the
/// terminal confirmed the change, so the price rule does not hold it again (A-I25), and its
/// batches go to the journal `site batches` reads.
async fn engine(site: &Site) -> Result<UpdateEngine> {
    let engine = UpdateEngine::builder(
        site.pipeline.clone(),
        Arc::new(adk_site::live::InMemoryLiveStore::default()),
    )
    .price_rule(awp_types::PriceRule { auto_within_pct: f32::MAX })
    .approve_new_items(false)
    .build()?;
    for (mapping, bounds) in configured_feeds(&site.paths)? {
        engine.add_feed(mapping, bounds).await?;
    }
    Ok(engine)
}

fn report_batches(paths: &SitePaths, records: &[BatchRecord]) -> Result<()> {
    journal_batches(paths, records)?;
    for record in records {
        match (&record.refused, record.release_seq) {
            (Some(reason), _) => println!("Batch {} refused: {reason}", record.batch),
            (None, Some(seq)) => println!(
                "Published release {seq}: {}",
                record.published.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            ),
            (None, None) => {}
        }
        for (item, reason) in &record.held {
            println!("Held {item}: {}", serde_json::to_string(reason).unwrap_or_default());
        }
    }
    if records.iter().any(|record| !record.held.is_empty() || record.refused.is_some()) {
        bail!("part of the change was held or refused");
    }
    Ok(())
}

/// `adk-rust site rollback` (spec 3.5 §7.8.3 "Undo per item", A-I11).
async fn rollback(
    paths: &SitePaths,
    seq: u64,
    replay: Option<ReplayChoice>,
    yes: bool,
) -> Result<()> {
    let site = open(paths, true).await?;
    let latest = site.log.size().await;
    if seq == 0 || seq >= latest {
        bail!(
            "roll back to an earlier release than the latest ({latest}); `adk-rust site log` lists them"
        );
    }
    if !confirm(&format!("Serve release {seq} again in place of release {latest}?"), yes)? {
        bail!("nothing changed");
    }
    let engine = engine(&site).await?;
    let (published, candidates) = engine.rollback(seq).await?;
    println!(
        "Rolled back: release {} serves the files of release {seq}.",
        published.staged.manifest.seq
    );
    if candidates.is_empty() {
        println!("No content changed since release {seq}.");
        return Ok(());
    }
    println!("Changed since release {seq}:");
    for candidate in &candidates {
        println!(
            "  {} (release {}, by {})",
            candidate.id,
            candidate.release_seq,
            candidate.actor.as_deref().unwrap_or("unknown")
        );
    }
    let mut records = Vec::new();
    for candidate in &candidates {
        let replay_it = match replay {
            Some(ReplayChoice::All) => true,
            Some(ReplayChoice::None) => false,
            None => confirm(&format!("Replay {}?", candidate.id), false).unwrap_or(false),
        };
        if replay_it {
            records.extend(engine.replay(candidate, &proposer(), entry()).await?);
        }
    }
    report_batches(paths, &records)
}

/// `adk-rust site log`.
async fn log(paths: &SitePaths, json: bool) -> Result<()> {
    let site = open(paths, true).await?;
    let size = site.log.size().await;
    let mut releases = Vec::new();
    for seq in 1..=size {
        let manifest = match site.host.fetch(&manifest_path(seq)).await? {
            Some(bytes) => serde_json::from_slice::<ReleaseManifest>(&bytes).ok(),
            None => None,
        };
        let entry = site.log.entry(seq).await?;
        releases.push((seq, manifest, entry));
    }
    let checkpoint = site.log.latest_checkpoint().await.ok();
    if json {
        let value: Vec<Value> = releases
            .iter()
            .map(|(seq, manifest, entry)| {
                json!({
                    "seq": seq,
                    "digest": entry.as_ref().map(|e| e.manifest.to_string()),
                    "createdAt": manifest.as_ref().map(|m| m.created_at),
                    "source": manifest.as_ref().map(|m| &m.source),
                    "snapshot": manifest.as_ref().map(ReleaseManifest::is_snapshot),
                    "files": manifest.as_ref().map(|m| m.files.len()),
                    "removed": manifest.as_ref().map(|m| m.removed.len()),
                    "charterVersion": manifest.as_ref().map(|m| m.charter_version),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "domain": site.config.domain,
                "size": size,
                "checkpointSize": checkpoint.as_ref().map(|c| c.size),
                "releases": value,
            }))?
        );
        return Ok(());
    }
    println!("Site log of {}: {size} releases", site.config.domain);
    for (seq, manifest, entry) in &releases {
        let digest = entry.as_ref().map(|e| e.manifest.to_string()).unwrap_or_default();
        match manifest {
            Some(manifest) => println!(
                "  {seq:>4}  {}  {:<8} {:>5} files  {}  {}",
                manifest.created_at.format("%Y-%m-%d %H:%M:%S"),
                if manifest.is_snapshot() { "snapshot" } else { "delta" },
                manifest.files.len(),
                source_label(&manifest.source),
                digest
            ),
            None => println!("  {seq:>4}  (manifest not on the host)  {digest}"),
        }
    }
    if let Some(checkpoint) = checkpoint {
        println!("Checkpoint at size {}, signed by the release key.", checkpoint.size);
    }
    Ok(())
}

fn source_label(source: &ReleaseSource) -> String {
    match source {
        ReleaseSource::Onboarding => "onboarding".into(),
        ReleaseSource::Migration => "migration".into(),
        ReleaseSource::Edit { job, decision } => match decision {
            Some(decision) => format!("edit ({job}; {decision})"),
            None => format!("edit ({job})"),
        },
        ReleaseSource::Rollback { to_seq } => format!("rollback to {to_seq}"),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// `adk-rust site content new`.
async fn content_new(
    paths: &SitePaths,
    kind: ContentKind,
    title: &str,
    slug: Option<String>,
    language: Option<String>,
) -> Result<()> {
    let config = paths.config()?;
    let language = match language {
        Some(tag) => tag.parse().map_err(|error| anyhow::anyhow!("--language {tag}: {error}"))?,
        None => config.default_language().clone(),
    };
    if !config.languages.contains(&language) {
        bail!("{language} is not one of the site's languages in site.toml");
    }
    let slug = slug.unwrap_or_else(|| slugify(title));
    if slug.is_empty() || slug.contains("..") || slug.starts_with('/') {
        bail!("give a slug of letters, digits and dashes with --slug");
    }
    let id = ContentId(format!("{}/{slug}/{language}", adk_site::content::kind_directory(kind)));
    let store = adk_site::content::FsContentStore::new(paths.root());
    if store.get(&id).await?.is_some() {
        bail!("{id} exists already");
    }
    let item = ContentItem {
        id: id.clone(),
        kind,
        slug,
        language,
        fields: BTreeMap::from([("title".to_string(), Value::String(title.to_string()))]),
        volatile: Vec::new(),
        body: Some(awp_types::Markdown(format!(
            "Write the {} here.\n",
            adk_site::content::kind_name(kind)
        ))),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    };
    store.put(&item).await?;
    let file = paths.root().join(adk_site::content::FsContentStore::item_path(&item));
    println!("Drafted {id} at {}.", file.display());
    println!("Edit it, then `adk-rust site publish {id}` or `adk-rust site schedule {id} <time>`.");
    Ok(())
}

async fn item(site: &Site, id: &str) -> Result<ContentItem> {
    let id = ContentId(id.to_string());
    site.store
        .get(&id)
        .await?
        .with_context(|| format!("no item {id}; ids look like posts/hello/en"))
}

/// `adk-rust site schedule`.
async fn schedule_item(
    paths: &SitePaths,
    id: &str,
    schedule: awp_types::Schedule,
    yes: bool,
) -> Result<()> {
    refuse_chain(paths)?;
    let site = open(paths, yes).await?;
    let mut item = item(&site, id).await?;
    if schedule.publish_at.is_some_and(|at| at <= Utc::now()) {
        bail!(
            "{} has passed; publish now with `adk-rust site publish {id}`",
            schedule.publish_at.map(|at| at.to_rfc3339()).unwrap_or_default()
        );
    }
    item.schedule = Some(schedule);
    item.state = ContentState::Scheduled;
    let outcome = site.pipeline.run(change(vec![item], "schedule")).await?;
    print_outcome(&outcome)
}

/// Applies `FIELD=VALUE` settings: VALUE as JSON when it parses, as text otherwise.
fn apply_sets(item: &mut ContentItem, sets: &[String]) -> Result<()> {
    for set in sets {
        let (field, value) = set
            .split_once('=')
            .with_context(|| format!("--set {set}: give FIELD=VALUE, such as price=24000"))?;
        let value =
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        item.fields.insert(field.trim().to_string(), value);
    }
    Ok(())
}

/// `adk-rust site correct`.
async fn correct(
    paths: &SitePaths,
    id: &str,
    note: &str,
    sets: &[String],
    body: Option<PathBuf>,
    yes: bool,
) -> Result<()> {
    refuse_chain(paths)?;
    let site = open(paths, yes).await?;
    let mut probe = item(&site, id).await?;
    apply_sets(&mut probe, sets)?;
    let body = match body {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?,
        ),
        None => None,
    };
    let fields = probe.fields.clone();
    let outcome = site
        .pipeline
        .correct(
            &ContentId(id.to_string()),
            move |item| {
                item.fields = fields;
                if let Some(body) = body {
                    item.body = Some(awp_types::Markdown(body));
                }
            },
            note,
            &proposer(),
            entry(),
        )
        .await?;
    print_outcome(&outcome)
}

/// `adk-rust site withdraw`.
async fn withdraw(paths: &SitePaths, id: &str, how: Withdrawal, yes: bool) -> Result<()> {
    refuse_chain(paths)?;
    let site = open(paths, yes).await?;
    item(&site, id).await?;
    let outcome =
        site.pipeline.withdraw(&ContentId(id.to_string()), how, &proposer(), entry()).await?;
    print_outcome(&outcome)
}

/// `adk-rust site revert` (spec 3.5 §7.8.3 "Undo per item").
async fn revert(paths: &SitePaths, id: &str, to: Option<u64>, yes: bool) -> Result<()> {
    refuse_chain(paths)?;
    let site = open(paths, true).await?;
    item(&site, id).await?;
    let target = match to {
        Some(seq) => format!("the version release {seq} served"),
        None => "the version before its latest release".to_string(),
    };
    if !confirm(&format!("Publish {target} of {id}, keeping every other change?"), yes)? {
        bail!("nothing changed");
    }
    let engine = engine(&site).await?;
    let records = engine.revert(&ContentId(id.to_string()), to, &proposer(), entry()).await?;
    report_batches(paths, &records)
}

/// Appends closed batches to `data/updates/batches.jsonl`.
fn journal_batches(paths: &SitePaths, records: &[BatchRecord]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let dir = paths.journal();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join("batches.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    for record in records {
        writeln!(file, "{}", serde_json::to_string(record)?)?;
    }
    Ok(())
}

/// `adk-rust site batches` (spec 3.5 §7.8.3): the engine's journal.
fn batches(paths: &SitePaths, json: bool) -> Result<()> {
    paths.config()?;
    let path = paths.journal().join("batches.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let records = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<BatchRecord>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("{} holds a line that is not a batch", path.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }
    if records.is_empty() {
        println!("No batches yet.");
        return Ok(());
    }
    for record in records.iter().rev().take(50).rev() {
        let release = record
            .release_seq
            .map_or_else(|| "no release".to_string(), |seq| format!("release {seq}"));
        let latency =
            record.latencies_ms.iter().max().map(|ms| format!(", approval-to-live ≤ {ms} ms"));
        println!(
            "batch {} {} ({:?}): {} published, {} superseded, {} held{}",
            record.batch,
            release,
            record.reason,
            record.published.len(),
            record.superseded,
            record.held.len(),
            latency.unwrap_or_default()
        );
        for (item, reason) in &record.held {
            println!("  held {item}: {}", serde_json::to_string(reason).unwrap_or_default());
        }
        if let Some(reason) = &record.refused {
            println!("  refused: {reason}");
        }
    }
    Ok(())
}

/// The feeds the owner configured in `feeds.json`: mapping and bounds each (A-I24, A-I27).
fn configured_feeds(
    paths: &SitePaths,
) -> Result<Vec<(adk_site::feed::FeedMapping, awp_types::FeedBounds)>> {
    #[derive(serde::Deserialize)]
    struct Configured {
        mapping: adk_site::feed::FeedMapping,
        bounds: awp_types::FeedBounds,
    }
    let path = paths.root().join("feeds.json");
    match std::fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Vec<Configured>>(&bytes)
            .with_context(|| format!("{} is not a list of {{mapping, bounds}}", path.display()))?
            .into_iter()
            .map(|feed| (feed.mapping, feed.bounds))
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// `adk-rust site feeds` (spec 3.5 §7.8.3): the configured feeds through the engine, with
/// the counters the site's running engine journals in `data/updates/feeds.json`.
async fn feeds(paths: &SitePaths, json: bool) -> Result<()> {
    let site = open(paths, true).await?;
    let engine = engine(&site).await?;
    let journaled: Vec<FeedStatus> = std::fs::read(paths.journal().join("feeds.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let feeds: Vec<FeedStatus> = engine
        .feeds()
        .await
        .into_iter()
        .map(|status| {
            journaled
                .iter()
                .find(|j| j.mapping.feed == status.mapping.feed)
                .map(|j| FeedStatus {
                    mapping: status.mapping.clone(),
                    bounds: status.bounds.clone(),
                    ..j.clone()
                })
                .unwrap_or(status)
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&feeds)?);
        return Ok(());
    }
    if feeds.is_empty() {
        println!("No feeds; the owner configures them in feeds.json.");
        return Ok(());
    }
    for feed in &feeds {
        println!(
            "{} → {}: {} deliveries, {} records ({} in the last minute), {} held, {} model calls",
            feed.mapping.feed,
            adk_site::content::kind_name(feed.mapping.kind),
            feed.deliveries,
            feed.records,
            feed.records_last_minute,
            feed.held,
            feed.model_calls
        );
        println!(
            "  bounds: at most {:.0}% of the collection per batch, {} records a minute, prices within {}%",
            feed.bounds.max_change_fraction * 100.0,
            feed.bounds.max_rate.per_minute,
            feed.bounds.price_rule.auto_within_pct
        );
    }
    Ok(())
}
