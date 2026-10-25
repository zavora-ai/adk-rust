//! The `adk-website` binary (spec 3.5 §7.16 `bin/adk-website`, §7.17).
//!
//! | Command | Does |
//! |---|---|
//! | `adk-website new <dir> [--kind <kind>] [--name <name>] [--domain <domain>] [--language <tag>]... [--theme <theme>] [--currency <code>]` | Scaffolds the site project: content, theme, charter draft, data, holding page |
//! | `adk-website kinds` | Lists the site kinds, the agents each runs and its capabilities |
//! | `adk-website plan [nothing \| site <url> \| presence <url>...]` | Prints the nine-stage onboarding plan for a starting point |
//! | `adk-website start <dir> [--edge <addr>] [--kernel <addr>] [--edge-url <url>] [--owner <name>]` | Serves the site: the kernel's control plane and the agent edge with the content; the setup code is in the logs |
//! | `adk-website agency <file> [--edge <addr>] [--kernel <addr>] [--owner <name>]` | Serves every site an agency file lists from one harness, each isolated (A-I22) |
//! | `adk-website deploy <dir> --shape <shape> [--hosting <minor>] [--domain-cost <minor>] [--price <in>/<out>]` | Writes a deployment shape's files into the project and prints its commands and monthly cost |

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use adk_harness::website::SiteRef;
use adk_website::deploy::{
    ProjectRef, ServeOptions, Served, Shape, serve, serve_agency, write_plan,
};
use adk_website::kinds::template;
use adk_website::onboarding::cost::{CostInputs, ModelPrice};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::onboarding::{StartingPoint, plan};
use adk_website::team::SiteTeam;
use adk_website::themes::ThemeId;
use awp_types::{Charter, SiteKind};

const USAGE: &str = "usage:
  adk-website new <dir> [--kind <kind>] [--name <name>] [--domain <domain>] [--language <tag>]... [--theme paper|harbour|meadow] [--currency <code>]
  adk-website kinds
  adk-website plan [nothing | site <url> | presence <url>...]
  adk-website start <dir> [--edge <addr>] [--kernel <addr>] [--edge-url <url>] [--owner <name>]
  adk-website agency <file> [--edge <addr>] [--kernel <addr>] [--owner <name>]
  adk-website deploy <dir> --shape local|container|managed|agency [--hosting <minor>] [--domain-cost <minor>] [--price <in>/<out>]";

const EDGE: &str = "127.0.0.1:8080";
const KERNEL: &str = "127.0.0.1:7340";

fn kind_of(text: &str) -> Result<SiteKind, String> {
    serde_json::from_value(serde_json::Value::String(text.to_string())).map_err(|_| {
        format!(
            "unknown kind `{text}`; use personal, business, ecommerce, government, nonprofit, \
             education, media, health, community or events"
        )
    })
}

async fn new(args: &[String]) -> Result<(), String> {
    let mut args = args.iter();
    let dir = PathBuf::from(args.next().ok_or("new needs a directory")?);
    let mut options = ScaffoldOptions {
        name: dir.file_name().map_or("My site".into(), |n| n.to_string_lossy().into_owned()),
        domain: "example.com".into(),
        kind: SiteKind::Business,
        languages: Vec::new(),
        theme: ThemeId::Paper,
        currency: "USD".into(),
    };
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--kind" => options.kind = kind_of(value)?,
            "--name" => options.name.clone_from(value),
            "--domain" => options.domain.clone_from(value),
            "--language" => {
                options.languages.push(value.parse().map_err(|e| format!("{value}: {e}"))?);
            }
            "--theme" => options.theme = value.parse()?,
            "--currency" => options.currency = value.to_ascii_uppercase(),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    if options.languages.is_empty() {
        options.languages.push("en".parse().map_err(|e| format!("{e}"))?);
    }
    let written = scaffold(&dir, &options).await.map_err(|e| e.to_string())?;
    println!("Scaffolded a {:?} site in {}:", options.kind, dir.display());
    for file in written {
        println!("  {file}");
    }
    let site = dir.display();
    println!(
        "Next, at a terminal, with the root key kept off the server (spec 3.5 D-11):\n\
         1. adk-rust awp keys rotate --site {site} --new-root-key <root key file>\n\
         2. adk-rust awp keys delegate --site {site} --root-key <root key file> --scope release\n\
         3. adk-rust awp keys delegate --site {site} --root-key <root key file> --scope card,receipt\n\
         4. adk-website start {site}, then enter the setup code from the logs on your phone; \
         onboarding begins. The site publishes nothing until step 2 is done."
    );
    Ok(())
}

fn kinds() {
    for kind in SiteKind::ALL {
        let template = template(kind);
        let agents: Vec<&str> = template.agents.iter().map(|a| a.as_str()).collect();
        println!("{kind:?}: {}", template.title);
        println!("  agents: {}", agents.join(", "));
        println!("  capabilities: {}", template.capabilities.join(", "));
    }
}

fn print_plan(args: &[String]) -> Result<(), String> {
    let start = match args.first().map(String::as_str) {
        None | Some("nothing") => StartingPoint::Nothing,
        Some("site") => StartingPoint::ExistingSite {
            url: args.get(1).cloned().ok_or("plan site needs a URL")?,
        },
        Some("presence") => StartingPoint::Presence { profiles: args[1..].to_vec() },
        Some(other) => return Err(format!("unknown starting point `{other}`\n{USAGE}")),
    };
    for (number, step) in plan(&start, None).iter().enumerate() {
        println!("{}. {} [{:?}]\n   {}", number + 1, step.title, step.gate, step.instruction);
    }
    Ok(())
}

/// The flags after the positional argument, as `(flag, value)` pairs.
fn flags(args: &[String]) -> Result<Vec<(&str, &str)>, String> {
    args.chunks(2)
        .map(|pair| match pair {
            [flag, value] => Ok((flag.as_str(), value.as_str())),
            [flag] => Err(format!("{flag} needs a value")),
            _ => Err(USAGE.to_string()),
        })
        .collect()
}

fn address(text: &str) -> Result<SocketAddr, String> {
    text.parse().map_err(|e| format!("{text}: {e}"))
}

/// Waits for Ctrl-C, then stops the kernel and the edge.
async fn run_until_stopped(served: Served) -> Result<(), String> {
    eprintln!(
        "Serving {} site(s): edge on http://{}, kernel on http://{}.",
        served.sites.len(),
        served.edge_addr,
        served.kernel_addr,
    );
    match &served.setup_code {
        Some(code) => eprintln!(
            "Setup code: {} (expires {}). Enter it on the device that will own the site.",
            code.code, code.expires_at
        ),
        None => eprintln!("The owner's device is paired; no setup code is offered."),
    }
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    served.shutdown().await.map_err(|e| e.to_string())
}

/// The options `args` set after the positional argument.
fn serve_options(dir: PathBuf, args: &[String]) -> Result<ServeOptions, String> {
    let mut options = ServeOptions::new(dir, "owner");
    options.edge = address(EDGE)?;
    options.kernel = address(KERNEL)?;
    for (flag, value) in flags(args)? {
        match flag {
            "--edge" => options.edge = address(value)?,
            "--kernel" => options.kernel = address(value)?,
            "--edge-url" => options.site.edge_url = Some(value.to_string()),
            "--owner" => options.site.owner = value.to_string(),
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    Ok(options)
}

async fn start(args: &[String]) -> Result<(), String> {
    let dir = PathBuf::from(args.first().ok_or("start needs a site directory")?);
    let options = serve_options(dir, &args[1..])?;
    run_until_stopped(serve(options).await.map_err(|e| e.to_string())?).await
}

async fn agency(args: &[String]) -> Result<(), String> {
    let file = PathBuf::from(args.first().ok_or("agency needs an agency file")?);
    let options = serve_options(PathBuf::from("."), &args[1..])?;
    let served = serve_agency(&file, options).await.map_err(|e| e.to_string())?;
    run_until_stopped(served).await
}

async fn deploy(args: &[String]) -> Result<(), String> {
    let dir = PathBuf::from(args.first().ok_or("deploy needs a site directory")?);
    let mut shape = None;
    let mut costs = CostInputs::small(0, 0, ModelPrice { input_per_mtok: 0, output_per_mtok: 0 });
    for (flag, value) in flags(&args[1..])? {
        let number = || value.parse::<u64>().map_err(|e| format!("{flag} {value}: {e}"));
        match flag {
            "--shape" => shape = Some(value.parse::<Shape>().map_err(|e| e.to_string())?),
            "--hosting" => costs.hosting = number()?,
            "--domain-cost" => costs.domain_per_year = number()?,
            "--price" => {
                let (input, output) = value.split_once('/').ok_or("--price takes <in>/<out>")?;
                costs.model = ModelPrice {
                    input_per_mtok: input.parse().map_err(|e| format!("{input}: {e}"))?,
                    output_per_mtok: output.parse().map_err(|e| format!("{output}: {e}"))?,
                };
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    let shape = shape.ok_or("deploy needs --shape")?;
    let project = adk_site::content::SiteProject::load(&dir, chrono::Utc::now())
        .await
        .map_err(|e| e.to_string())?;
    let kind = charter_kind(&dir).await;
    let config = &project.config;
    let site = SiteRef::new(uuid::Uuid::nil(), config.domain.replace('.', "-"), config.base_url());
    let team = SiteTeam::new(site, &template(kind));
    let project =
        ProjectRef { name: config.name.clone(), domain: config.domain.clone(), sites: Vec::new() };
    let plan =
        adk_website::deploy::plan(shape, &project, &team, &costs).map_err(|e| e.to_string())?;
    let written = write_plan(&plan, &dir).await.map_err(|e| e.to_string())?;
    println!("{shape}: {} — {}", shape.runtime(), shape.fits());
    for file in written {
        println!("  wrote {file}");
    }
    for (number, command) in plan.commands.iter().enumerate() {
        println!("{}. {command}", number + 1);
    }
    println!("Monthly running cost (minor units): {}", plan.estimate.total);
    for (category, amount) in &plan.estimate.monthly {
        println!("  {category:?}: {amount}");
    }
    Ok(())
}

/// The kind of the charter drafted in `dir`, or a business site.
async fn charter_kind(dir: &Path) -> SiteKind {
    tokio::fs::read(dir.join("charter.draft.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Charter>(&bytes).ok())
        .map_or(SiteKind::Business, |charter| charter.kind)
}

#[tokio::main]
async fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("new") => new(&args[1..]).await,
        Some("kinds") => {
            kinds();
            Ok(())
        }
        Some("plan") => print_plan(&args[1..]),
        Some("start") => start(&args[1..]).await,
        Some("agency") => agency(&args[1..]).await,
        Some("deploy") => deploy(&args[1..]).await,
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
