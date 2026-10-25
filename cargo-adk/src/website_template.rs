//! The `website` template (spec 3.5 §7.17, §8.2): the same site project `adk-rust website
//! new` and `adk-website new` scaffold — content, theme, draft charter, capabilities, public
//! terms, data and the holding page — plus an edge binary that serves the published release
//! and the site's capabilities, and a smoke test.
//!
//! The project files are `adk-website`'s business-kind scaffold with the `paper` theme;
//! `{site_id}` gives each generated project its own site id.

use crate::addon::DependencySpec;
use crate::codegen::ADK_VERSION;
use crate::template::{AgentCodeFragments, AgentTemplate, FileFragment, TemplateCategory};

/// The `website` template.
pub fn template() -> AgentTemplate {
    AgentTemplate {
        name: "website",
        description: "AWP 2.0 website: content, theme, draft charter, data and an edge binary serving the published site and its capabilities",
        category: TemplateCategory::AgentType,
        default_provider: "gemini",
        required_features: vec!["minimal", "website"],
        // The edge binary replaces the generated main.rs, so no addon's code would run.
        incompatible_addons: vec![
            "telemetry",
            "auth",
            "sessions",
            "memory",
            "mcp",
            "guardrails",
            "eval",
            "browser",
            "server",
            "docker",
        ],
        additional_deps: vec![
            DependencySpec { crate_name: "awp-types", version: ADK_VERSION, features: vec![] },
            DependencySpec { crate_name: "axum", version: "0.8", features: vec![] },
            DependencySpec { crate_name: "async-trait", version: "0.1", features: vec![] },
            DependencySpec { crate_name: "chrono", version: "0.4", features: vec![] },
            DependencySpec { crate_name: "serde_json", version: "1", features: vec![] },
        ],
        code_fragments: AgentCodeFragments {
            imports: vec![],
            agent_construction: "",
            additional_files: vec![
                FileFragment { path: "src/main.rs", content: MAIN_RS },
                FileFragment { path: "tests/site_smoke.rs", content: SMOKE_TEST },
                FileFragment { path: "site.toml", content: SITE_TOML },
                FileFragment { path: "theme/theme.toml", content: THEME_TOML },
                FileFragment { path: "theme/style.css", content: STYLE_CSS },
                FileFragment { path: "charter.draft.json", content: CHARTER_DRAFT_JSON },
                FileFragment { path: "capabilities.json", content: CAPABILITIES_JSON },
                FileFragment { path: "public-terms.json", content: PUBLIC_TERMS_JSON },
                FileFragment { path: "content/pages/about/en.md", content: ABOUT_MD },
                FileFragment { path: "content/pages/privacy/en.md", content: PRIVACY_MD },
                FileFragment { path: "content/pages/consent/en.md", content: CONSENT_MD },
                FileFragment { path: "content/pages/terms/en.md", content: TERMS_MD },
                FileFragment { path: "data/.keep", content: "" },
                FileFragment { path: "public/index.html", content: HOLDING_PAGE },
                FileFragment { path: ".env.example", content: ENV_EXAMPLE },
                FileFragment { path: ".gitignore", content: GITIGNORE },
                FileFragment { path: "README.md", content: README },
            ],
        },
    }
}

/// The edge binary: `cargo run` serves the published release from `data/host` and the
/// site's capabilities over AWP 2.0 on loopback; `cargo run -- check` builds the working
/// copy and runs the launch gate.
const MAIN_RS: &str = r##"//! {name}: an AWP 2.0 website (spec 3.5).
//!
//! `cargo run` serves the release `adk-rust site publish` put in `data/host`, with the
//! site's capabilities on the AWP edge, at http://127.0.0.1:8080 (`PORT` to change it).
//! `cargo run -- check` builds the working copy into `data/work/check` and runs the launch
//! gate.

use std::path::Path;
use std::sync::Arc;

use adk_rust::awp::signing::jws::Ed25519DocumentSigner;
use adk_rust::awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_rust::server::awp::{AwpEdge, EdgeKeys};
use adk_rust::site::content::SiteProject;
use adk_rust::site::gate::{GateInput, GateMode, LaunchGate, Quarantine};
use adk_rust::site::generator::{BuiltinGenerator, SiteGenerator};
use adk_rust::site::host::{DirectoryHost, SiteHost};
use awp_types::{CapabilityEffect, CapabilityV2, ContentState};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    match std::env::args().nth(1).as_deref() {
        None | Some("serve") => serve().await,
        Some("check") => check().await,
        Some(other) => anyhow::bail!("unknown command `{other}`; use `serve` or `check`"),
    }
}

/// Builds the working copy and runs the launch gate's full tier (spec 3.5 §7.8.1).
async fn check() -> anyhow::Result<()> {
    let project = SiteProject::load(Path::new("."), chrono::Utc::now()).await?;
    let out = Path::new("data/work/check");
    let report = BuiltinGenerator.build(&project, out).await?;
    let quarantine = Quarantine::default();
    let input = GateInput { project: &project, out, report: &report, changed: None, quarantine: &quarantine };
    let gate = LaunchGate::new().run(GateMode::Full, &input).await?;
    println!("Built {} pages into {}.", report.pages.len(), out.display());
    println!("Launch gate: {}", gate.summary());
    for (check, finding) in gate.blocking() {
        println!("  {check:?}: {}", finding.message);
    }
    if !gate.passed() {
        std::process::exit(1);
    }
    Ok(())
}

/// `search_site`: the published items whose title or body holds the query.
struct SearchSite {
    items: Vec<(String, String, String)>,
}

#[async_trait::async_trait]
impl CapabilityHandler for SearchSite {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let query = call.input["query"].as_str().unwrap_or_default().to_lowercase();
        let results: Vec<Value> = self
            .items
            .iter()
            .filter(|(_, title, body)| {
                title.to_lowercase().contains(&query) || body.to_lowercase().contains(&query)
            })
            .map(|(id, title, _)| json!({ "id": id, "title": title }))
            .collect();
        Ok(json!({ "query": query, "results": results }))
    }
}

/// Serves the published release and the AWP edge on loopback (spec 3.5 §7.9, §7.22 "Local").
async fn serve() -> anyhow::Result<()> {
    let project = SiteProject::load(Path::new("."), chrono::Utc::now()).await?;
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let items = project
        .items
        .iter()
        .filter(|item| matches!(item.state, ContentState::Published | ContentState::Corrected))
        .map(|item| {
            let title = item.fields.get("title").and_then(Value::as_str).unwrap_or_default();
            let body = item.body.as_ref().map(|body| body.0.clone()).unwrap_or_default();
            (item.id.to_string(), title.to_string(), body)
        })
        .collect();
    let declared: Vec<CapabilityV2> = match std::fs::read("capabilities.json") {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(_) => Vec::new(),
    };
    let search = declared.into_iter().find(|c| c.name == "search_site").unwrap_or_else(|| {
        CapabilityV2::new("search_site", "Search the site", CapabilityEffect::ReadOnly)
    });
    let mut registry = CapabilityRegistry::new(&project.config.domain);
    registry.declare(search, Arc::new(SearchSite { items }))?;
    // Keys for this process only: delegate card and receipt keys with `adk-rust awp keys
    // delegate --scope card,receipt` before the edge faces the public.
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::generate("edge-card-local")?),
        receipt: Arc::new(Ed25519DocumentSigner::generate("edge-receipt-local")?),
    };
    let edge_url = adk_rust::Url::parse(&format!("http://127.0.0.1:{port}"))?;
    let edge = AwpEdge::builder(registry, keys, edge_url).build()?;
    let app = edge.router().fallback(static_file);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("{name} on http://127.0.0.1:{port}: the site at /, its capabilities at /awp/v2/manifest");
    axum::serve(listener, app).await?;
    Ok(())
}

/// A file of the published release, or the holding page before the first release.
async fn static_file(uri: Uri) -> Response {
    let host = DirectoryHost::new("data/host");
    let path = uri.path();
    match host.fetch(path).await {
        Ok(Some(bytes)) => {
            let lookup = if path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
            let kind = adk_rust::site::release::content_type(&lookup);
            let mut response = (StatusCode::OK, bytes).into_response();
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
            response
        }
        _ if path == "/" => match std::fs::read_to_string("public/index.html") {
            Ok(page) => Html(page).into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
"##;

/// Loads the project, builds it, and checks the draft charter, with no network and no key.
const SMOKE_TEST: &str = r##"//! The site project loads, builds with the built-in generator, and its draft charter is
//! valid AWP 2.0 (spec 3.5 §7.6, §7.8).

use std::path::Path;

use adk_rust::site::content::SiteProject;
use adk_rust::site::generator::{BuiltinGenerator, SiteGenerator};

#[tokio::test]
async fn the_site_project_builds_and_its_charter_is_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let project = SiteProject::load(root, chrono::Utc::now()).await.expect("site.toml and content load");
    let out = std::env::temp_dir().join(format!("{name}-smoke-{}", std::process::id()));
    let report = BuiltinGenerator.build(&project, &out).await.expect("the site builds");
    assert!(out.join("index.html").exists(), "the home page is built");
    assert!(report.outputs.contains("index.awp.json"), "every page has its machine view");
    let _ = std::fs::remove_dir_all(&out);

    let charter: awp_types::Charter =
        serde_json::from_slice(&std::fs::read(root.join("charter.draft.json")).unwrap()).unwrap();
    charter.validate().expect("the draft charter is valid");
    assert!(charter.signed.is_none(), "the owner signs the charter during onboarding");
}
"##;

const ABOUT_MD: &str = r#"---
title: About
state: draft
---
Who runs this site, what it offers and how to reach you. Publish it with
`adk-rust site publish pages/about/en`.
"#;

const PRIVACY_MD: &str = r#"---
title: Privacy
legal: privacy
state: draft
---
What the site collects, why, for how long, and how to ask for it to be deleted.
"#;

const CONSENT_MD: &str = r#"---
title: Consent
legal: consent
state: draft
---
What visitors agree to when they contact you or subscribe, and how to withdraw consent.
"#;

const TERMS_MD: &str = r#"---
title: Terms
legal: terms
state: draft
---
The terms on which the site and its agents serve visitors and other agents.
"#;

const ENV_EXAMPLE: &str = r#"# The edge binary needs no model key. The site's agents run on the kernel
# (`adk-rust up`), which reads the provider key from its own setup.
PORT=8080
RUST_LOG=info
"#;

const GITIGNORE: &str = r#"/target
.env
# Keys, the site log, builds and published releases: back data/ up, never commit it.
/data/*
!/data/.keep
"#;

const README: &str = r#"# {name}

An AWP 2.0 website (spec 3.5): content as files, a theme, a draft charter, the site's
capabilities and public terms, and an edge binary.

| Path | Holds |
|---|---|
| `site.toml` | Name, domain, languages, capabilities; set `domain` to the site's own |
| `content/` | Pages and posts as Markdown with front matter; the legal pages start as drafts |
| `theme/` | The `paper` theme's tokens and style sheet |
| `charter.draft.json` | Charter version 1 from the business template, unsigned until the owner signs it |
| `capabilities.json`, `public-terms.json` | What agents may call, and the published terms |
| `data/` | Keys, the site log, builds and releases (`adk-rust site`); not committed |
| `public/index.html` | The holding page served before the first release |
| `src/main.rs` | The edge: the published release and `search_site` over AWP 2.0 |

```bash
cargo run -- check                     # build and run the launch gate
adk-rust site publish --yes            # release the working copy into data/host
cargo run                              # serve it with the edge on 127.0.0.1:8080
adk-rust awp keys rotate --new-root-key ~/keys/{name}-root.jwk
adk-rust awp keys delegate --root-key ~/keys/{name}-root.jwk --scope release --key data/keys/release.jwk
```

`adk-rust` comes from `cargo install adk-cli --features website`. The launch gate fails
until the privacy, consent and terms pages are written and published. Root key operations
run only at a terminal; keep the root key file off the host.
"#;
const SITE_TOML: &str = r#"name = "{name}"
domain = "example.com"
languages = ["en"]
jurisdictions = []
redirects = []
site_capabilities = [
    "submit_inquiry",
    "get_quote",
    "check_availability",
    "book",
    "search_site",
]
home_sections = [
    "post",
    "article",
    "notice",
    "event",
]
page_size = 50
charter_version = 0

[security]
contact = []

[performance]
critical_path_bytes = 200000
lcp_ms = 2500

[kind_capabilities]

[live_capabilities]

[strings]

[translation]
required = []
default_approval = "spot_check"
simultaneous = false
lag_seconds = 600
pre_approved = []

[translation.approval]
"#;

const THEME_TOML: &str = r##"name = "paper"

[tokens]
font_body = 'Charter, "Bitstream Charter", Georgia, "Times New Roman", serif'
font_heading = 'system-ui, -apple-system, "Segoe UI", Roboto, sans-serif'
font_mono = 'ui-monospace, "SF Mono", Menlo, Consolas, monospace'
measure = "42rem"
radius = "2px"

[tokens.colors]
text = "#1b1d21"
background = "#ffffff"
muted = "#555b65"
link = "#0b57d0"
accent = "#1b1d21"
accent_text = "#ffffff"
border = "#d0d4da"
notice_background = "#fff4d6"
notice_text = "#4a3500"
"##;

const STYLE_CSS: &str = r#":root { color-scheme: light dark; }
@media (prefers-color-scheme: dark) {
  :root {
    --color-text: #e8eaed;
    --color-background: #16181c;
    --color-muted: #a7adb6;
    --color-link: #8ab4f8;
    --color-accent: #e8eaed;
    --color-accent-text: #16181c;
    --color-border: #33373d;
    --color-notice-background: #3d3000;
    --color-notice-text: #fde7a6;
  }
}
*, *::before, *::after { box-sizing: border-box; }
html { color: var(--color-text); background: var(--color-background); font-family: var(--font-body); line-height: 1.6; }
body { margin: 0; }
h1, h2, h3, h4 { font-family: var(--font-heading); line-height: 1.25; }
a { color: var(--color-link); text-underline-offset: 0.15em; }
a:focus-visible { outline: 3px solid var(--color-accent); outline-offset: 2px; }
code, pre { font-family: var(--font-mono); }
pre { overflow-x: auto; padding: 1rem; border: 1px solid var(--color-border); border-radius: var(--radius); }
img { max-width: 100%; height: auto; }
.skip-link { position: absolute; left: -9999px; }
.skip-link:focus { position: static; display: inline-block; padding: 0.5rem 1rem; background: var(--color-accent); color: var(--color-accent-text); }
.site-header, main, .site-footer { max-width: var(--measure); margin: 0 auto; padding: 1rem; }
.site-header { display: flex; flex-wrap: wrap; gap: 1rem; align-items: baseline; justify-content: space-between; border-bottom: 1px solid var(--color-border); }
.site-name { font-weight: 700; text-decoration: none; color: var(--color-text); }
nav ul { list-style: none; display: flex; flex-wrap: wrap; gap: 1rem; margin: 0; padding: 0; }
.site-footer { border-top: 1px solid var(--color-border); color: var(--color-muted); }
.meta, time { color: var(--color-muted); }
.notice { background: var(--color-notice-background); color: var(--color-notice-text); padding: 0.75rem 1rem; border-radius: var(--radius); margin: 1rem 0; }
.items { list-style: none; padding: 0; }
.items li { margin: 0 0 1.25rem; }
.items h2 { font-size: 1.25rem; margin: 0; }
dl.facts, dl.live { display: grid; grid-template-columns: max-content 1fr; gap: 0.25rem 1rem; }
dt { font-weight: 600; }
dd { margin: 0; }
figure { margin: 1rem 0; }
figcaption { color: var(--color-muted); font-size: 0.9rem; }
table { border-collapse: collapse; }
th, td { border: 1px solid var(--color-border); padding: 0.25rem 0.5rem; }
"#;

const CHARTER_DRAFT_JSON: &str = r#"{
  "version": 1,
  "siteId": "{site_id}",
  "kind": "business",
  "governance": {
    "roles": [
      {
        "principal": "owner",
        "roles": [
          "owner",
          "editor",
          "publisher"
        ]
      }
    ],
    "approvalChains": [
      {
        "action": "charter_change",
        "roles": [
          "owner"
        ]
      }
    ]
  },
  "mission": "What {name} is for, in the owner's words.",
  "strategy": [
    {
      "name": "Win qualified enquiries"
    },
    {
      "name": "Fill the calendar"
    },
    {
      "name": "Earn good reviews"
    },
    {
      "name": "Stay trustworthy, accessible and current"
    }
  ],
  "goals": [
    {
      "id": "enquiries",
      "name": "Win qualified enquiries",
      "kpis": [
        "qualified_leads",
        "response_time"
      ]
    },
    {
      "id": "bookings",
      "name": "Fill the calendar",
      "kpis": [
        "bookings"
      ]
    },
    {
      "id": "reputation",
      "name": "Earn good reviews",
      "kpis": [
        "review_rating"
      ]
    },
    {
      "id": "trust",
      "name": "Stay trustworthy, accessible and current",
      "kpis": [
        "freshness",
        "accessibility"
      ]
    }
  ],
  "kpis": [
    {
      "id": "qualified_leads",
      "name": "Qualified enquiries",
      "metric": "leads.qualified",
      "source": "commerce",
      "direction": "up",
      "cadence": "daily"
    },
    {
      "id": "response_time",
      "name": "Hours to first reply",
      "metric": "leads.response_hours",
      "source": "commerce",
      "direction": "down",
      "cadence": "daily"
    },
    {
      "id": "bookings",
      "name": "Bookings",
      "metric": "bookings.count",
      "source": "commerce",
      "direction": "up",
      "cadence": "daily"
    },
    {
      "id": "review_rating",
      "name": "Average review rating",
      "metric": "reviews.rating",
      "source": "reviews",
      "direction": "up",
      "cadence": "daily"
    },
    {
      "id": "freshness",
      "name": "Pages current",
      "metric": "content.fresh_share",
      "source": "site",
      "direction": "up",
      "cadence": "daily"
    },
    {
      "id": "accessibility",
      "name": "Pages passing WCAG 2.2 AA",
      "metric": "a11y.passing_share",
      "source": "site",
      "direction": "up",
      "cadence": "daily"
    }
  ],
  "guardrails": [
    {
      "id": "unsubscribes",
      "metric": "email.unsubscribe_rate",
      "threshold": 0.02,
      "breach": "above",
      "window": 604800,
      "action": "pause_job",
      "jobs": [
        "email",
        "nurture"
      ]
    }
  ],
  "authority": [
    {
      "job": "onboarding",
      "level": "act_with_approval"
    },
    {
      "job": "protect",
      "level": "propose"
    },
    {
      "job": "alarms",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "freshness",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "hosting",
      "level": "propose"
    },
    {
      "job": "seo",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "aeo",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "agent-search",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "multilingual",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "email",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "social",
      "level": "propose"
    },
    {
      "job": "advertising",
      "level": "propose"
    },
    {
      "job": "nurture",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "handoff",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "sell",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "experiments",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "playbook-learning",
      "level": "propose"
    },
    {
      "job": "running-costs",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "cost-and-return",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "goals",
      "level": "propose"
    },
    {
      "job": "weekly-review",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    },
    {
      "job": "owner-questions",
      "level": "propose",
      "limits": {
        "actionsPerDay": 100
      }
    }
  ],
  "budget": {
    "period": "monthly",
    "currency": "USD",
    "caps": {
      "hosting": 2000,
      "models": 5000,
      "advertising": 0,
      "domains": 300,
      "other": 1000
    }
  },
  "voice": {
    "tone": "Warm, clear and practical",
    "languages": [
      "en"
    ],
    "disclosure": "Written by {name}'s assistant, an AI agent, on behalf of the site's owner."
  },
  "redLines": [
    {
      "kind": "action",
      "text": "Never invent facts, testimonials, team members, prices or credentials"
    },
    {
      "kind": "action",
      "text": "Generated images only as labelled illustration, never the owner's people or premises"
    },
    {
      "kind": "action",
      "text": "Nothing copied from competitors"
    }
  ],
  "reporting": {
    "review": "weekly",
    "routes": [],
    "quietHours": {
      "start": "21:00:00",
      "end": "07:00:00",
      "timezone": "UTC"
    }
  },
  "public": {
    "policies": [
      {
        "name": "privacy",
        "url": "/privacy/",
        "version": "1"
      },
      {
        "name": "terms",
        "url": "/terms/",
        "version": "1"
      },
      {
        "name": "consent",
        "url": "/consent/",
        "version": "1"
      }
    ],
    "consentScopes": [
      "contact"
    ],
    "contact": {
      "security": "/.well-known/security.txt",
      "page": "/contact/"
    }
  },
  "priceRule": {
    "autoWithinPct": 10.0
  }
}"#;

const CAPABILITIES_JSON: &str = r#"[
  {
    "name": "submit_inquiry",
    "description": "Send the site's owner an enquiry",
    "effect": "non_idempotent",
    "minTrust": "public",
    "inputSchema": {
      "additionalProperties": false,
      "properties": {
        "email": {
          "format": "email",
          "maxLength": 254,
          "type": "string"
        },
        "message": {
          "maxLength": 5000,
          "type": "string"
        },
        "name": {
          "maxLength": 200,
          "type": "string"
        }
      },
      "required": [
        "name",
        "email",
        "message"
      ],
      "type": "object"
    },
    "consent": {
      "scope": "contact",
      "policyVersion": "1"
    },
    "approval": {
      "mode": "never"
    },
    "idempotency": "binding"
  },
  {
    "name": "get_quote",
    "description": "Quote products or services at their published prices",
    "effect": "idempotent",
    "minTrust": "known",
    "inputSchema": {
      "additionalProperties": false,
      "properties": {
        "items": {
          "items": {
            "properties": {
              "quantity": {
                "minimum": 1,
                "type": "integer"
              },
              "sku": {
                "maxLength": 100,
                "type": "string"
              }
            },
            "required": [
              "sku",
              "quantity"
            ],
            "type": "object"
          },
          "maxItems": 100,
          "type": "array"
        }
      },
      "required": [
        "items"
      ],
      "type": "object"
    },
    "approval": {
      "mode": "never"
    },
    "idempotency": "binding"
  },
  {
    "name": "check_availability",
    "description": "List open booking slots",
    "effect": "read_only",
    "minTrust": "public",
    "inputSchema": {
      "additionalProperties": false,
      "properties": {
        "from": {
          "format": "date",
          "type": "string"
        },
        "to": {
          "format": "date",
          "type": "string"
        }
      },
      "required": [],
      "type": "object"
    },
    "approval": {
      "mode": "never"
    },
    "idempotency": "binding"
  },
  {
    "name": "book",
    "description": "Book a slot",
    "effect": "non_idempotent",
    "minTrust": "known",
    "inputSchema": {
      "additionalProperties": false,
      "properties": {
        "email": {
          "format": "email",
          "maxLength": 254,
          "type": "string"
        },
        "name": {
          "maxLength": 200,
          "type": "string"
        },
        "slot": {
          "maxLength": 100,
          "type": "string"
        }
      },
      "required": [
        "slot",
        "name",
        "email"
      ],
      "type": "object"
    },
    "consent": {
      "scope": "contact",
      "policyVersion": "1"
    },
    "approval": {
      "mode": "never"
    },
    "idempotency": "binding"
  },
  {
    "name": "search_site",
    "description": "Search the site's pages",
    "effect": "read_only",
    "minTrust": "public",
    "inputSchema": {
      "additionalProperties": false,
      "properties": {
        "query": {
          "maxLength": 200,
          "type": "string"
        }
      },
      "required": [
        "query"
      ],
      "type": "object"
    },
    "approval": {
      "mode": "never"
    },
    "idempotency": "binding"
  }
]"#;

const PUBLIC_TERMS_JSON: &str = r#"{
  "policies": [
    {
      "name": "privacy",
      "url": "/privacy/",
      "version": "1"
    },
    {
      "name": "terms",
      "url": "/terms/",
      "version": "1"
    },
    {
      "name": "consent",
      "url": "/consent/",
      "version": "1"
    }
  ],
  "consentScopes": [
    "contact"
  ],
  "contact": {
    "security": "/.well-known/security.txt",
    "page": "/contact/"
  }
}"#;

const HOLDING_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{name}</title>
</head>
<body>
<main>
<h1>{name}</h1>
<p>This site is being set up. Please come back soon.</p>
</main>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use crate::codegen::generate_project_with_registry;
    use crate::composition::resolve_composition;
    use crate::registry::TemplateRegistry;

    fn generate(name: &str) -> Vec<(String, String)> {
        let registry = TemplateRegistry::builtin();
        let manifest = resolve_composition(&registry, "website", &[], "gemini").unwrap();
        generate_project_with_registry(&registry, &manifest, name)
            .into_iter()
            .map(|file| (file.path, file.content))
            .collect()
    }

    fn file<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
        files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.as_str())
            .unwrap_or_else(|| panic!("{path}"))
    }

    #[test]
    fn the_website_template_generates_the_site_project_and_the_edge() {
        let files = generate("juniper-bakery");
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        for path in [
            "Cargo.toml",
            "src/main.rs",
            "tests/site_smoke.rs",
            "site.toml",
            "theme/theme.toml",
            "theme/style.css",
            "charter.draft.json",
            "capabilities.json",
            "public-terms.json",
            "content/pages/privacy/en.md",
            "data/.keep",
            "public/index.html",
        ] {
            assert!(paths.contains(&path), "{path} in {paths:?}");
        }
        assert_eq!(
            paths.iter().filter(|p| **p == "src/main.rs").count(),
            1,
            "the edge replaces main.rs"
        );
        let cargo = file(&files, "Cargo.toml");
        assert!(cargo.contains(r#""minimal", "website"]"#), "{cargo}");
        assert!(cargo.contains("awp-types = "), "{cargo}");
        assert!(file(&files, "src/main.rs").contains("AwpEdge::builder"));
        let site: toml::Value = file(&files, "site.toml").parse().unwrap();
        assert_eq!(site["name"].as_str(), Some("juniper-bakery"));
        let charter: serde_json::Value =
            serde_json::from_str(file(&files, "charter.draft.json")).unwrap();
        let id = charter["siteId"].as_str().unwrap();
        assert_eq!((id.len(), &id[14..15]), (36, "4"), "a version 4 UUID: {id}");
        let again = generate("juniper-bakery");
        let other: serde_json::Value =
            serde_json::from_str(file(&again, "charter.draft.json")).unwrap();
        assert_ne!(other["siteId"], charter["siteId"], "each project has its own site id");
        assert!(file(&files, ".gitignore").contains("/data/*"), "keys are never committed");
        for (path, content) in &files {
            assert!(!content.contains("{name}") && !content.contains("{site_id}"), "{path}");
        }
    }

    #[test]
    fn addons_cannot_join_the_website_template() {
        let registry = TemplateRegistry::builtin();
        for addon in ["server", "docker", "telemetry", "sessions"] {
            assert!(
                resolve_composition(&registry, "website", &[addon], "gemini").is_err(),
                "{addon}"
            );
        }
    }
}
