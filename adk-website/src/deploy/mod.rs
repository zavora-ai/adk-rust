//! Deployment shapes (spec 3.5 §7.22, §7.17).
//!
//! | Shape | Content | Edge and kernel | Fits |
//! |---|---|---|---|
//! | [`Shape::Local`] | `DirectoryHost`, or any host driver | `adk-rust up --install` on the owner's machine | Trials, developers |
//! | [`Shape::Container`] | Any static host | One container: one process serving the edge and the kernel on two listeners | Most small sites |
//! | [`Shape::Managed`] | Any static host | The managed agent runtime or another agent platform through `adk-deploy` | Owners who want no servers |
//! | [`Shape::Agency`] | Per-site hosts | One harness for many sites, each isolated (A-I22) | Developers running sites for clients |
//!
//! [`plan`] writes what a shape needs into the site project and estimates its monthly
//! running cost before deployment; [`serve`](fn@serve) is what the `adk-website` binary runs: the
//! kernel's control plane and the agent edge for one site, or many in [`agency`] mode.
//!
//! | Module | Contents |
//! |---|---|
//! | [`project`] | The project layout `adk-rust site` uses, delegated key files, released documents, the owner's feeds, slots and payment links |
//! | [`kernel`] | The kernel's durable SQLite stores in `data/kernel.db` |
//! | [`site`] | [`RunningSite`]: one site's runtime, edge, updates, experiments, measurement, review and alarms |
//! | [`updates`] | The update engine's driver, journal, decision cards and alarms |
//! | [`checks`] | The release-delegation gate check, payment links, the log recorder |
//! | [`serve`](mod@serve) | The kernel and the edge's listener for one site or an agency |

pub mod agency;
pub mod checks;
pub mod kernel;
pub mod project;
pub mod serve;
pub mod site;
pub mod updates;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use adk_core::{AdkError, Result};
use adk_deploy::{AgentConfig, DeploymentManifest};
use serde::{Deserialize, Serialize};

use crate::onboarding::cost::{CostInputs, RunningCostEstimate, estimate};
use crate::team::SiteTeam;

pub use agency::{Agency, AgencyRouter};
pub use kernel::KernelStores;
pub use project::ProjectPaths;
pub use serve::{AgencyFile, AgencySite, ServeOptions, Served, serve, serve_agency};
pub use site::{RunningSite, ServedWorkflows, SiteOptions};
pub use updates::{UpdateCards, UpdateDriver, UpdateJournal};

/// A deployment shape (spec 3.5 §7.22).
///
/// # Example
///
/// ```rust
/// use adk_website::deploy::Shape;
///
/// assert_eq!("container".parse::<Shape>().unwrap(), Shape::Container);
/// assert!(Shape::Agency.fits().contains("clients"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// On the owner's machine.
    Local,
    /// One server or container.
    Container,
    /// A managed agent runtime.
    Managed,
    /// Many sites in one harness.
    Agency,
}

impl Shape {
    /// Every shape.
    pub const ALL: [Shape; 4] = [Shape::Local, Shape::Container, Shape::Managed, Shape::Agency];

    /// The shape's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Container => "container",
            Self::Managed => "managed",
            Self::Agency => "agency",
        }
    }

    /// Where the content is served from.
    pub fn content(self) -> &'static str {
        match self {
            Self::Local => "DirectoryHost, or any host driver",
            Self::Container | Self::Managed => "Any static host",
            Self::Agency => "Per-site hosts",
        }
    }

    /// Where the edge and the kernel run.
    pub fn runtime(self) -> &'static str {
        match self {
            Self::Local => "adk-rust up --install on the owner's machine",
            Self::Container => {
                "One container: one process serving the edge and the kernel on two listeners"
            }
            Self::Managed => {
                "The managed agent runtime or another agent platform through adk-deploy"
            }
            Self::Agency => "One harness for many sites",
        }
    }

    /// Whom it fits.
    pub fn fits(self) -> &'static str {
        match self {
            Self::Local => "Trials, developers",
            Self::Container => "Most small sites: personal, business, nonprofit, community",
            Self::Managed => "Owners who want no servers",
            Self::Agency => "Developers running sites for clients",
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Shape {
    type Err = AdkError;

    fn from_str(text: &str) -> Result<Self> {
        Self::ALL.into_iter().find(|shape| shape.as_str() == text.trim()).ok_or_else(|| {
            AdkError::config(format!(
                "unknown shape `{text}`; use local, container, managed or agency"
            ))
        })
    }
}

/// A site's deployment in one shape: the files to write into the project, the commands that
/// start it, and its monthly running cost (spec 3.5 §7.22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlan {
    /// The shape.
    pub shape: Shape,
    /// Files by path relative to the project.
    pub files: BTreeMap<String, String>,
    /// What starts it, in order.
    pub commands: Vec<String>,
    /// The monthly running cost before deployment.
    pub estimate: RunningCostEstimate,
}

/// The container's image: `adk-website` from crates.io, and the project, keys excluded.
fn dockerfile() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"# The site's edge and kernel in one container (spec 3.5 §7.22): one process serving the
# agent edge publicly and the kernel's control plane on loopback, each on its own listener.
# The content can be served by any static host as well.
FROM rust:1.95-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential cmake pkg-config && rm -rf /var/lib/apt/lists/*
RUN cargo install adk-website --version {version} --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/cargo/bin/adk-website /usr/local/bin/adk-website
COPY . /site/
# Keys, stores and releases live on the volume, never in the image.
VOLUME ["/site/data", "/site/host"]
EXPOSE 8080
CMD ["adk-website", "start", "/site", "--edge", "0.0.0.0:8080", "--kernel", "127.0.0.1:7340"]
"#
    )
}

fn build_script(image: &str) -> String {
    format!(
        "#!/bin/sh\n# Builds the site's image from this project.\nset -eu\ndocker build -t {image} .\n"
    )
}

/// What a plan needs to know about the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    /// The site's name.
    pub name: String,
    /// Its domain.
    pub domain: String,
    /// The sites an agency runs, by domain; the project's own for the other shapes.
    pub sites: Vec<String>,
}

/// The deployment of `project` in `shape`, with the running cost of `team` under `costs`
/// (spec 3.5 §7.22).
///
/// # Errors
///
/// An invalid managed-runtime manifest.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::deploy::{ProjectRef, Shape, plan};
/// use adk_website::kinds::template;
/// use adk_website::onboarding::cost::{CostInputs, ModelPrice};
/// use adk_website::team::SiteTeam;
/// use awp_types::SiteKind;
///
/// let team = SiteTeam::new(SiteRef::new(uuid::Uuid::nil(), "notes", "https://notes.example"), &template(SiteKind::Personal));
/// let project = ProjectRef { name: "Notes".into(), domain: "notes.example".into(), sites: vec![] };
/// let costs = CostInputs::small(500, 1_200, ModelPrice { input_per_mtok: 30, output_per_mtok: 250 });
/// let container = plan(Shape::Container, &project, &team, &costs).unwrap();
/// assert!(container.files.contains_key("Dockerfile"));
/// assert!(container.estimate.total > 0);
/// ```
pub fn plan(
    shape: Shape,
    project: &ProjectRef,
    team: &SiteTeam,
    costs: &CostInputs,
) -> Result<DeploymentPlan> {
    let image = project.domain.replace('.', "-");
    let mut files = BTreeMap::new();
    let commands = match shape {
        Shape::Local => vec!["adk-rust up --install".into(), "adk-website start .".into()],
        Shape::Container => {
            files.insert("Dockerfile".into(), dockerfile());
            files.insert(".dockerignore".into(), "data\nhost\n".into());
            files.insert("scripts/build-image.sh".into(), build_script(&image));
            vec![
                "sh scripts/build-image.sh".into(),
                format!(
                    "docker run -p 8080:8080 -v \"$PWD/data:/site/data\" -v \"$PWD/host:/site/host\" {image}"
                ),
            ]
        }
        Shape::Managed => {
            let mut manifest = DeploymentManifest {
                agent: AgentConfig::new(image.clone(), "adk-website"),
                ..DeploymentManifest::default()
            };
            manifest.agent.description = Some(format!("The AWP website {}", project.name));
            manifest.build.assets = [
                "site.toml",
                "theme",
                "content",
                "capabilities.json",
                "public-terms.json",
                "charter.draft.json",
            ]
            .into_iter()
            .map(str::to_string)
            .collect();
            let toml = manifest.to_toml_string().map_err(|e| AdkError::config(e.to_string()))?;
            files.insert("adk-deploy.toml".into(), toml);
            vec!["Bundle and push with adk-deploy's BundleBuilder and DeployClient".into()]
        }
        Shape::Agency => {
            let sites = if project.sites.is_empty() {
                vec![project.domain.clone()]
            } else {
                project.sites.clone()
            };
            let listed: String = sites
                .iter()
                .map(|site| format!("[[site]]\ndomain = \"{site}\"\ndir = \"sites/{site}\"\n\n"))
                .collect();
            files.insert("agency.toml".into(), listed);
            vec!["adk-website agency agency.toml".into()]
        }
    };
    Ok(DeploymentPlan { shape, files, commands, estimate: estimate(team, costs) })
}

/// Writes `plan`'s files into the project at `dir`, leaving existing files as they are;
/// returns the files written.
///
/// # Errors
///
/// The file system's error.
pub async fn write_plan(plan: &DeploymentPlan, dir: &Path) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (path, contents) in &plan.files {
        let target = dir.join(path);
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            continue;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                AdkError::config(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        tokio::fs::write(&target, contents)
            .await
            .map_err(|e| AdkError::config(format!("cannot write {}: {e}", target.display())))?;
        written.push(path.clone());
    }
    Ok(written)
}
