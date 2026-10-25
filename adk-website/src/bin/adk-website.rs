//! The `adk-website` binary (spec 3.5 §7.16 `bin/adk-website`, §7.17).
//!
//! | Command | Does |
//! |---|---|
//! | `adk-website new <dir> [--kind <kind>] [--name <name>] [--domain <domain>] [--language <tag>]... [--theme <theme>] [--currency <code>]` | Scaffolds the site project: content, theme, charter draft, data, holding page |
//! | `adk-website kinds` | Lists the site kinds, the agents each runs and its capabilities |
//! | `adk-website plan [nothing \| site <url> \| presence <url>...]` | Prints the nine-stage onboarding plan for a starting point |

use std::path::PathBuf;
use std::process::ExitCode;

use adk_website::kinds::template;
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::onboarding::{StartingPoint, plan};
use adk_website::themes::ThemeId;
use awp_types::SiteKind;

const USAGE: &str = "usage:
  adk-website new <dir> [--kind <kind>] [--name <name>] [--domain <domain>] [--language <tag>]... [--theme paper|harbour|meadow] [--currency <code>]
  adk-website kinds
  adk-website plan [nothing | site <url> | presence <url>...]";

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
    println!(
        "Next: start the kernel and the edge, enter the setup code from the logs on your phone, \
         and onboarding begins."
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

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("new") => new(&args[1..]).await,
        Some("kinds") => {
            kinds();
            Ok(())
        }
        Some("plan") => print_plan(&args[1..]),
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
