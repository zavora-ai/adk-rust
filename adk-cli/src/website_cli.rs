//! `adk-rust website`: spin up an AWP 2.0 website and look after it from the terminal
//! (spec 3.5 §7.17, §7.11, §7.21, §8.1, D-14).
//!
//! | Command | Library |
//! |---|---|
//! | `website new [dir] [--agency]` | `adk_website::onboarding::scaffold`, then `adk_website::deploy::serve` (or `serve_agency`), and the website views of `adk-surface` over a `WebsiteDesk` on a loopback home |
//! | `website status` | `KernelStores` and the site log in the project's `data/` |
//! | `website review` | `RunningSite::review`: the weekly review, composed from the stores |
//! | `website charter` | The charter store: the charter in force and its history |
//! | `website jobs [promote\|demote <job>]` | The autonomy ladder; a promotion is the owner's decision and grants its standing rules, a demotion revokes them (A-I13) |
//!
//! The project layout is `adk-rust site`'s; no root key lives in the project.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_harness::website::{LadderCompiler, apply_demotion, job_levels, promote_job};
use adk_kernel::charter::CharterStore as _;
use adk_kernel::charter::OwnerDecision;
use adk_website::deploy::{KernelStores, ProjectPaths, RunningSite, ServeOptions, SiteOptions};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use anyhow::{Context, Result, bail};
use awp_types::{AutonomyLevel, Charter, Role, SiteKind};
use chrono::{Datelike, Utc};
use serde_json::json;

use crate::site_cli::{confirm, person};

/// `adk-rust website` subcommands (spec 3.5 §8.1).
#[derive(clap::Subcommand, Debug, Clone)]
pub enum WebsiteCommands {
    /// Scaffold a site project, start its kernel and edge on this machine, print the setup
    /// code, and open onboarding in the browser.
    ///
    /// Example: `adk-rust website new bakery --kind business --domain bakery.example`
    New(Box<NewArgs>),

    /// The site: its kind, charter, releases, paired devices and jobs.
    Status {
        #[command(flatten)]
        target: Target,
        /// Print the status as JSON
        #[arg(long)]
        json: bool,
    },

    /// The weekly review: KPIs against baselines, work, cost, incidents, alarms, promotions
    /// and questions.
    Review {
        #[command(flatten)]
        target: Target,
        /// The week, as any day in it (default: this week)
        #[arg(long, value_name = "YYYY-MM-DD")]
        week: Option<chrono::NaiveDate>,
        /// Print the review as JSON
        #[arg(long)]
        json: bool,
    },

    /// The charter in force, signed, and every earlier version.
    Charter {
        #[command(flatten)]
        target: Target,
        /// Print the charter and its history as JSON
        #[arg(long)]
        json: bool,
    },

    /// The site's jobs and their autonomy levels; promoting or demoting one is the owner's
    /// decision.
    Jobs {
        #[command(flatten)]
        target: Target,
        #[command(subcommand)]
        command: Option<JobsCommand>,
        /// Print the jobs as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Which site a `website` command looks after.
#[derive(clap::Args, Debug, Clone)]
pub struct Target {
    /// The site project (default: the current directory)
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub site: PathBuf,
    /// The owner acting (default: the charter's owner)
    #[arg(long)]
    pub owner: Option<String>,
}

/// `adk-rust website jobs` actions.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum JobsCommand {
    /// Raise a job one level and grant the standing rules its new level needs: the owner's
    /// decision, never the site's (A-I13).
    Promote {
        /// The job, such as review-replies
        job: String,
        /// Decide without asking
        #[arg(long, short)]
        yes: bool,
    },
    /// Lower a job one level and revoke the standing rules it no longer warrants.
    Demote {
        /// The job
        job: String,
        /// Why, for the job's record
        #[arg(long, default_value = "the owner lowered it")]
        reason: String,
        /// Decide without asking
        #[arg(long, short)]
        yes: bool,
    },
}

/// `adk-rust website new`.
#[derive(clap::Args, Debug, Clone)]
pub struct NewArgs {
    /// The site project to create, or an existing one to start (default: the current
    /// directory); with --agency, the agency's directory
    pub dir: Option<PathBuf>,
    /// One harness for many sites: the site goes into <dir>/<domain> and is listed in
    /// <dir>/agency.toml, and every listed site is served, each isolated (A-I22)
    #[arg(long)]
    pub agency: bool,
    /// The site's kind
    #[arg(long, default_value = "business")]
    pub kind: String,
    /// The site's name (default: the directory's name)
    #[arg(long)]
    pub name: Option<String>,
    /// The domain the site is served at
    #[arg(long, default_value = "example.com")]
    pub domain: String,
    /// The site's languages, the first the default; repeatable
    #[arg(long = "language", value_name = "TAG")]
    pub languages: Vec<String>,
    /// The theme: paper, harbour or meadow
    #[arg(long, default_value = "paper")]
    pub theme: String,
    /// The budget's currency
    #[arg(long, default_value = "USD")]
    pub currency: String,
    /// The person the setup code pairs (default: your user name)
    #[arg(long)]
    pub owner: Option<String>,
    /// Where the edge listens: the site and its agent edge
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub edge: SocketAddr,
    /// Where the kernel's control plane listens
    #[arg(long, default_value = "127.0.0.1:7340")]
    pub kernel: SocketAddr,
    /// Where the web home with onboarding listens; loopback only
    #[arg(long, default_value = "127.0.0.1:8090")]
    pub home: SocketAddr,
    /// The edge's public URL (default: the site's origin)
    #[arg(long, value_name = "URL")]
    pub edge_url: Option<String>,
    /// Do not open the browser
    #[arg(long)]
    pub no_open: bool,
}

/// Runs an `adk-rust website` command.
pub async fn run(command: WebsiteCommands) -> Result<()> {
    match command {
        WebsiteCommands::New(args) => new(*args).await,
        WebsiteCommands::Status { target, json } => status(&target, json).await,
        WebsiteCommands::Review { target, week, json } => review(&target, week, json).await,
        WebsiteCommands::Charter { target, json } => charter(&target, json).await,
        WebsiteCommands::Jobs { target, command, json } => jobs(&target, command, json).await,
    }
}

fn kind_of(text: &str) -> Result<SiteKind> {
    serde_json::from_value(serde_json::Value::String(text.to_ascii_lowercase())).map_err(|_| {
        anyhow::anyhow!(
            "unknown kind `{text}`; use personal, business, ecommerce, government, nonprofit, \
             education, media, health, community or events"
        )
    })
}

fn scaffold_options(args: &NewArgs, dir: &Path) -> Result<ScaffoldOptions> {
    let mut languages = Vec::new();
    for tag in &args.languages {
        languages.push(tag.parse().map_err(|error| anyhow::anyhow!("--language {tag}: {error}"))?);
    }
    if languages.is_empty() {
        languages.push("en".parse().map_err(|error| anyhow::anyhow!("{error}"))?);
    }
    let name = args.name.clone().unwrap_or_else(|| {
        dir.canonicalize()
            .ok()
            .and_then(|dir| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "My site".into())
    });
    Ok(ScaffoldOptions {
        name,
        domain: args.domain.to_ascii_lowercase(),
        kind: kind_of(&args.kind)?,
        languages,
        theme: args.theme.parse().map_err(|error: String| anyhow::anyhow!("--theme: {error}"))?,
        currency: args.currency.to_ascii_uppercase(),
    })
}

/// Creates `dir` as a site project unless it is one already.
async fn ensure_project(args: &NewArgs, dir: &Path) -> Result<()> {
    if dir.join("site.toml").exists() {
        println!("{} holds a site project already; starting it.", dir.display());
        return Ok(());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let options = scaffold_options(args, dir)?;
    let written = scaffold(dir, &options).await?;
    println!("Scaffolded a {} site in {}:", args.kind, dir.display());
    for file in written {
        println!("  {file}");
    }
    Ok(())
}

/// Adds `domain` at `dir` to the agency file unless it lists it.
fn list_in_agency(file: &Path, domain: &str, dir: &str) -> Result<()> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let listed: toml::Table = toml::from_str(&text)
        .with_context(|| format!("{} is not an agency file", file.display()))?;
    let known = listed.get("site").and_then(toml::Value::as_array).is_some_and(|sites| {
        sites.iter().any(|s| s.get("domain").and_then(|d| d.as_str()) == Some(domain))
    });
    if known {
        return Ok(());
    }
    let entry = format!(
        "\n[[site]]\ndomain = {}\ndir = {}\n",
        toml::Value::String(domain.to_string()),
        toml::Value::String(dir.to_string())
    );
    std::fs::write(file, format!("{text}{entry}"))
        .with_context(|| format!("cannot write {}", file.display()))
}

/// `adk-rust website new` (spec 3.5 §7.17): scaffold, serve, setup code, onboarding.
async fn new(args: NewArgs) -> Result<()> {
    if !args.home.ip().is_loopback() {
        bail!(
            "the web home serves onboarding to this machine only; give --home a loopback address"
        );
    }
    let owner = args.owner.clone().unwrap_or_else(person);
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut options = ServeOptions::new(dir.clone(), owner.clone());
    options.edge = args.edge;
    options.kernel = args.kernel;
    options.site = SiteOptions {
        owner: owner.clone(),
        edge_url: args.edge_url.clone(),
        ..SiteOptions::default()
    };
    let (served, site_dir) = if args.agency {
        let domain = args.domain.to_ascii_lowercase();
        let site_dir = dir.join(&domain);
        ensure_project(&args, &site_dir).await?;
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("agency.toml");
        list_in_agency(&file, &domain, &domain)?;
        (adk_website::deploy::serve_agency(&file, options).await?, site_dir)
    } else {
        ensure_project(&args, &dir).await?;
        (adk_website::deploy::serve(options).await?, dir.clone())
    };
    let site = served
        .sites
        .iter()
        .find(|site| site.paths.root() == ProjectPaths::new(&site_dir).root())
        .or_else(|| served.sites.first())
        .context("the server started no site")?;
    let desk = home::desk(site, &served.stores, &owner).await?;
    let home_url = home::serve(desk, args.home, &owner).await?;
    let onboarding = format!("{home_url}/ui/website");
    println!();
    println!("{} ({}) is running:", site.site.name, site.domain);
    println!("  edge    http://{}  the site and its agent edge", served.edge_addr);
    println!("  kernel  http://{}  the control plane", served.kernel_addr);
    println!("  home    {onboarding}  onboarding, the charter, the site and the review");
    if args.agency {
        let names: Vec<String> = served.sites.iter().map(|s| s.domain.clone()).collect();
        println!("  agency  {} site(s): {}", names.len(), names.join(", "));
    }
    match &served.setup_code {
        Some(code) => println!(
            "Setup code: {}  — enter it on the setup screen to pair your device; nothing else \
             can claim the site (A-I20).",
            code.code
        ),
        None => println!("A device is paired already; sign in from it."),
    }
    let site_arg = site_dir.display();
    let root = format!("~/keys/{}-root.jwk", site.domain);
    println!("Your keys — run these yourself, in your own terminal, never inside an agent:");
    println!("  1. adk-rust awp keys rotate --site {site_arg} --new-root-key {root}");
    println!("     and publish the _awp-root DNS record it prints");
    println!(
        "  2. adk-rust awp keys delegate --site {site_arg} --root-key {root} --scope release --key {site_arg}/data/keys/release.jwk"
    );
    println!(
        "  3. adk-rust awp keys delegate --site {site_arg} --root-key {root} --scope card,receipt --key {site_arg}/data/keys/card.jwk"
    );
    println!("  Keep the root key file off this machine's site directory.");
    println!("Press Ctrl+C to stop.");
    if !args.no_open {
        open_browser(&onboarding);
    }
    tokio::signal::ctrl_c().await.context("cannot wait for Ctrl+C")?;
    served.shutdown().await?;
    Ok(())
}

/// Opens `url` in the person's browser; a failure is only printed.
fn open_browser(url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    if std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_err()
    {
        println!("Open {url} in your browser.");
    }
}

/// The site's project and the kernel's stores, opened without serving or changing them.
struct Opened {
    paths: ProjectPaths,
    site: adk_harness::website::SiteRef,
    domain: String,
    template: adk_website::kinds::KindTemplate,
    stores: KernelStores,
    owner: String,
}

/// The site's id and kind as `adk-website` derives them: from `charter.json`, else
/// `charter.draft.json`.
fn identity(paths: &ProjectPaths) -> Option<Charter> {
    ["charter.json", "charter.draft.json"]
        .iter()
        .filter_map(|name| std::fs::read(paths.root().join(name)).ok())
        .find_map(|bytes| serde_json::from_slice::<Charter>(&bytes).ok())
}

async fn open(target: &Target) -> Result<Opened> {
    let paths = ProjectPaths::new(&target.site);
    let site_toml = paths.root().join("site.toml");
    let config = adk_site::content::SiteConfig::from_toml(
        &std::fs::read_to_string(&site_toml).with_context(|| {
            format!(
                "no site project at {}: it needs a site.toml (`adk-rust website new` makes one); \
                 pass --site DIR",
                target.site.display()
            )
        })?,
    )?;
    let drafted = identity(&paths)
        .with_context(|| format!("{} holds no charter draft", target.site.display()))?;
    let owner = target.owner.clone().unwrap_or_else(person);
    std::fs::create_dir_all(paths.data())?;
    let stores = KernelStores::open(&paths.database("kernel"), &owner).await?;
    let site = adk_harness::website::SiteRef::new(
        drafted.site_id,
        config.domain.replace('.', "-"),
        config.base_url(),
    );
    Ok(Opened {
        paths,
        site,
        domain: config.domain.clone(),
        template: adk_website::kinds::template(drafted.kind),
        stores,
        owner,
    })
}

/// The charter in force, else the project's draft.
async fn charter_of(opened: &Opened) -> Result<(Option<Charter>, bool)> {
    if let Some(current) = opened.stores.charters.current(opened.site.id).await? {
        return Ok((Some(current.charter), true));
    }
    Ok((identity(&opened.paths), false))
}

/// The owner acting: `--owner`, else the charter's first owner, else the person.
fn acting_owner(target: &Target, charter: Option<&Charter>) -> String {
    target
        .owner
        .clone()
        .or_else(|| {
            charter.and_then(|c| c.governance.holders(Role::Owner).next().map(str::to_string))
        })
        .unwrap_or_else(person)
}

fn compiler(opened: &Opened) -> LadderCompiler {
    let team = adk_website::team::SiteTeam::new(opened.site.clone(), &opened.template);
    let mut compiler = LadderCompiler::new(opened.site.clone());
    for binding in adk_website::jobs::bindings(&team, &opened.template.jobs()) {
        compiler = compiler.job(binding);
    }
    compiler
}

/// The ladder over the site's stores, checked against its charters.
fn ladder(opened: &Opened) -> adk_kernel::charter::AutonomyLadder {
    let charters: Arc<dyn adk_kernel::charter::CharterStore> = opened.stores.charters.clone();
    adk_kernel::charter::AutonomyLadder::new(opened.stores.autonomy.clone()).with_charters(charters)
}

fn level_name(level: AutonomyLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{level:?}"))
}

/// The latest release the site log holds.
async fn latest_release(opened: &Opened) -> Result<u64> {
    use adk_core::audit_log::AuditLogStore as _;
    let log = adk_session::SqliteAuditLogStore::new(&opened.paths.database("site-log")).await?;
    log.migrate().await?;
    Ok(log.size(&format!("awp-site-log:{}", opened.domain)).await?)
}

/// `adk-rust website status`.
async fn status(target: &Target, json: bool) -> Result<()> {
    let opened = open(target).await?;
    let (charter, committed) = charter_of(&opened).await?;
    let devices =
        adk_kernel::pairing::Pairing::new(opened.stores.devices.clone()).devices().await?;
    let levels =
        job_levels(&compiler(&opened), opened.stores.autonomy.as_ref(), opened.site.id).await?;
    let release = latest_release(&opened).await?;
    let launched = release > 0;
    if json {
        let value = json!({
            "site": opened.site.name,
            "domain": opened.domain,
            "kind": opened.template.kind,
            "charter": charter.as_ref().map(|c| json!({ "version": c.version, "committed": committed })),
            "launched": launched,
            "release": (release > 0).then_some(release),
            "pairedDevices": devices.len(),
            "jobs": levels.iter().map(|(job, level)| json!({ "job": job, "level": level })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!("{} — {} ({})", opened.domain, opened.template.title, opened.site.name);
    match &charter {
        Some(c) if committed => println!("  charter   version {}, signed and in force", c.version),
        Some(c) => println!("  charter   draft version {}, not signed yet", c.version),
        None => println!("  charter   none"),
    }
    match launched {
        true => println!("  site      live, release {release}"),
        false => println!("  site      not launched; the holding page is served"),
    }
    println!("  devices   {} paired", devices.len());
    println!("  jobs      {} — `adk-rust website jobs` lists their levels", levels.len());
    Ok(())
}

/// The Monday of the week holding `day`.
fn week_start(day: chrono::NaiveDate) -> chrono::NaiveDate {
    day - chrono::Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// `adk-rust website review` (spec 3.5 §7.21).
async fn review(target: &Target, week: Option<chrono::NaiveDate>, json: bool) -> Result<()> {
    let opened = open(target).await?;
    // The composer reads the KPIs, alarms, experiments and updates of the site's running
    // parts, which `RunningSite` opens over the same stores.
    let options = SiteOptions { owner: opened.owner.clone(), ..SiteOptions::default() };
    let site = RunningSite::open(opened.paths.clone(), &opened.stores, &options).await?;
    let start = week_start(week.unwrap_or_else(|| Utc::now().date_naive()));
    let review = site.review.compose(start, Default::default()).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&review)?);
        return Ok(());
    }
    println!("Weekly review of {}, week of {start}", opened.domain);
    match review.charter_version {
        Some(version) => println!("  charter version {version}"),
        None => println!("  no charter in force yet"),
    }
    println!("KPIs");
    if review.kpis.is_empty() {
        println!("  none measured yet");
    }
    for kpi in &review.kpis {
        println!("  {}", serde_json::to_string(kpi)?);
    }
    for (title, lines) in [
        ("Experiments", &review.experiments),
        ("Handed off", &review.handed_off),
        ("Questions", &review.questions),
    ] {
        if !lines.is_empty() {
            println!("{title}");
            for line in lines {
                println!("  - {line}");
            }
        }
    }
    for (role, work) in &review.work {
        println!("{role:?}: {}", work.join("; "));
    }
    if !review.incidents.is_empty() {
        println!("Incidents: {}", review.incidents.len());
    }
    if !review.alarms.is_empty() {
        println!("Alarms waiting: {}", review.alarms.len());
    }
    for promotion in &review.promotions {
        println!("Promotion offered: {}", serde_json::to_string(promotion)?);
    }
    for demotion in &review.demotions {
        println!("Demoted: {}", serde_json::to_string(demotion)?);
    }
    if let Some(money) = &review.money {
        println!("Money: {}", serde_json::to_string(money)?);
    }
    Ok(())
}

/// `adk-rust website charter`.
async fn charter(target: &Target, json: bool) -> Result<()> {
    let opened = open(target).await?;
    let id = opened.site.id;
    let history = opened.stores.charters.history(id).await?;
    let current = opened.stores.charters.current(id).await?;
    if json {
        let value = json!({
            "current": current,
            "history": history,
            "draft": if current.is_none() { charter_of(&opened).await?.0 } else { None },
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    match &current {
        Some(version) => {
            let charter = &version.charter;
            println!(
                "Charter version {} of {}, in force since {}",
                charter.version,
                opened.domain,
                version.committed_at.format("%Y-%m-%d %H:%M UTC")
            );
            println!("  mission   {}", charter.mission);
            println!(
                "  signed    {}",
                charter
                    .signed
                    .as_ref()
                    .map_or("not signed".to_string(), |s| serde_json::to_string(s)
                        .unwrap_or_default())
            );
            println!(
                "  goals     {}",
                charter.goals.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join("; ")
            );
            println!(
                "  KPIs      {}",
                charter.kpis.iter().map(|k| k.name.as_str()).collect::<Vec<_>>().join("; ")
            );
            println!(
                "  approvals {} by decision {}",
                version.decision.approvals.len(),
                version.decision.decision_id
            );
        }
        None => {
            let (draft, _) = charter_of(&opened).await?;
            match draft {
                Some(charter) => println!(
                    "No charter is in force yet: charter.draft.json is version {}, waiting for its \
                     signers in onboarding.",
                    charter.version
                ),
                None => println!("No charter yet."),
            }
        }
    }
    if history.len() > 1 {
        println!("History");
        for version in &history {
            println!(
                "  version {}  {}  decision {}",
                version.charter.version,
                version.committed_at.format("%Y-%m-%d %H:%M"),
                version.decision.decision_id
            );
        }
    }
    Ok(())
}

/// The standing rules the site's back office runs under, in `data/kernel.db`.
async fn rules(paths: &ProjectPaths) -> Result<adk_harness::SqliteStandingRuleStore> {
    let pool = sqlx::SqlitePool::connect(&paths.database("kernel"))
        .await
        .context("cannot open the site's kernel database")?;
    let store = adk_harness::SqliteStandingRuleStore::new(pool);
    store.migrate().await?;
    Ok(store)
}

/// `adk-rust website jobs` (spec 3.5 §7.11, A-I13).
async fn jobs(target: &Target, command: Option<JobsCommand>, json: bool) -> Result<()> {
    let opened = open(target).await?;
    let compiler = compiler(&opened);
    let (charter, committed) = charter_of(&opened).await?;
    let owner = acting_owner(target, charter.as_ref());
    let domain = &opened.domain;
    // The jobs of the charter in force enter the ladder at its levels; none is lowered or
    // raised here.
    if let (true, Some(charter)) = (committed, &charter) {
        let ladder = ladder(&opened);
        for authority in &charter.authority {
            let area = adk_website::jobs::job(&authority.job).map(|job| job.area.as_str());
            ladder.register(opened.site.id, &authority.job, authority.level, area).await?;
        }
    }
    match command {
        None => {
            let levels =
                job_levels(&compiler, opened.stores.autonomy.as_ref(), opened.site.id).await?;
            if json {
                let value: Vec<_> = levels
                    .iter()
                    .map(|(job, level)| json!({ "job": job, "level": level }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&value)?);
                return Ok(());
            }
            println!("Jobs of {domain} (promotion and demotion are the owner's decisions):");
            for (job, level) in &levels {
                println!("  {job:<24} {}", level_name(*level));
            }
            Ok(())
        }
        Some(JobsCommand::Promote { job, yes }) => {
            if compiler.binding(&job).is_none() {
                bail!("{domain} has no job `{job}`; `adk-rust website jobs` lists them");
            }
            if !confirm(&format!("As the owner {owner}, raise `{job}` one level?"), yes)? {
                bail!("nothing changed");
            }
            let decision = OwnerDecision::new(
                format!("cli-promote-{}", uuid::Uuid::new_v4()),
                SubjectRef::human(owner.clone()),
                Utc::now(),
            );
            let store = rules(&opened.paths).await?;
            let promotion =
                promote_job(&ladder(&opened), &compiler, &store, &job, &decision, &owner).await?;
            println!(
                "Promoted `{job}` to {} by your decision {}; {} standing rule(s) granted.",
                level_name(promotion.job.level),
                decision.decision_id,
                promotion.rules.len()
            );
            Ok(())
        }
        Some(JobsCommand::Demote { job, reason, yes }) => {
            if compiler.binding(&job).is_none() {
                bail!("{domain} has no job `{job}`; `adk-rust website jobs` lists them");
            }
            if !confirm(&format!("As the owner {owner}, lower `{job}` one level?"), yes)? {
                bail!("nothing changed");
            }
            let lowered = ladder(&opened).lower(opened.site.id, &job, &reason).await?;
            let Some(record) = lowered else {
                println!("`{job}` is at observe already.");
                return Ok(());
            };
            let store = rules(&opened.paths).await?;
            let revoked = apply_demotion(&compiler, &store, &job, record.level).await?;
            println!(
                "Demoted `{job}` to {}; {} standing rule(s) revoked.",
                level_name(record.level),
                revoked.len()
            );
            Ok(())
        }
    }
}

/// The web home that serves the website views beside the edge (spec 3.5 §8.2): a loopback
/// server with the desk under `/api/surface/website`, the setup routes and the page at
/// `/ui/`.
mod home {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use adk_core::{RequestContext, SingleAgentLoader};
    use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
    use adk_server::{ServerBuilder, ServerConfig};
    use adk_session::{InMemorySessionService, SessionService};
    use adk_website::deploy::{KernelStores, RunningSite};
    use adk_website::onboarding::SetupGate;
    use adk_website::ui::WebsiteDesk;
    use anyhow::{Context, Result};
    use async_trait::async_trait;

    /// Admits every request as the owner: the home listens on loopback only, and onboarding
    /// itself needs the paired device's token (A-I20).
    struct Owner(String);

    #[async_trait]
    impl RequestContextExtractor for Owner {
        async fn extract(
            &self,
            _parts: &axum::http::request::Parts,
        ) -> std::result::Result<RequestContext, RequestContextError> {
            Ok(RequestContext {
                user_id: self.0.clone(),
                scopes: vec!["owner".into(), "operator".into()],
                metadata: Default::default(),
            })
        }
    }

    /// The site's back office, which takes no turn on the home.
    struct Steward;

    #[async_trait]
    impl adk_core::Agent for Steward {
        fn name(&self) -> &str {
            "steward"
        }

        fn description(&self) -> &str {
            "Leads the site's back office"
        }

        fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    /// The owner's desk over the served site and the kernel's stores.
    pub(super) async fn desk(
        site: &RunningSite,
        stores: &KernelStores,
        owner: &str,
    ) -> Result<WebsiteDesk> {
        let gate = SetupGate::new(adk_kernel::pairing::Pairing::new(stores.devices.clone()), owner);
        let rules: Arc<dyn adk_harness::StandingRuleStore> =
            Arc::new(super::rules(&site.paths).await?);
        let config = adk_site::content::SiteConfig::from_toml(&std::fs::read_to_string(
            site.paths.root().join("site.toml"),
        )?)?;
        Ok(WebsiteDesk::builder(
            site.site.clone(),
            config.name.clone(),
            owner,
            site.runtime.clone(),
        )
        .languages(config.languages.clone())
        .setup(Arc::new(gate))
        .charters(stores.charters.clone())
        .ladder(site.ladder.clone(), rules)
        .signals(stores.signals.clone(), site.partitions.back_office().id.clone())
        .updates(site.updates.clone())
        .alarms(site.alarms.clone())
        .shape("Local")
        .build())
    }

    /// Serves `desk` on `addr`; returns the home's origin.
    pub(super) async fn serve(desk: WebsiteDesk, addr: SocketAddr, owner: &str) -> Result<String> {
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let config =
            ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(Steward))), sessions)
                .with_request_context(Arc::new(Owner(owner.to_string())));
        let app = ServerBuilder::new(config)
            .add_api_routes(adk_surface::web::website::router(desk.clone()))
            .add_root_routes(adk_surface::web::website::setup_router(desk))
            .add_root_routes(adk_surface::web::home())
            .build();
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("the home cannot listen on {addr}"))?;
        let bound = listener.local_addr()?;
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(error = %error, "the web home stopped");
            }
        });
        Ok(format!("http://{bound}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_week_starts_on_its_monday() {
        let sunday = chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(week_start(sunday), chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        let monday = chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert_eq!(week_start(monday), monday);
    }

    #[test]
    fn an_agency_file_lists_each_site_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agency.toml");
        list_in_agency(&file, "bakery.example", "bakery.example").unwrap();
        list_in_agency(&file, "bakery.example", "bakery.example").unwrap();
        list_in_agency(&file, "notes.example", "notes.example").unwrap();
        let listed: adk_website::deploy::AgencyFile =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let domains: Vec<&str> = listed.sites.iter().map(|s| s.domain.as_str()).collect();
        assert_eq!(domains, ["bakery.example", "notes.example"]);
    }

    #[test]
    fn kinds_parse_by_name() {
        assert_eq!(kind_of("Government").unwrap(), SiteKind::Government);
        assert!(kind_of("castle").is_err());
    }
}
