//! External generators: Zola, Hugo, Astro and Build Awesome (Eleventy) as sandboxed processes
//! with no network (spec 3.5 §7.1, §7.8 "Generator", §7.23 "Themes", Q3; features `gen-zola`,
//! `gen-hugo`, `gen-astro`, `gen-build-awesome`).
//!
//! An [`ExternalGenerator`] builds a site in three steps:
//!
//! | Step | What happens |
//! |------|--------------|
//! | [`export`](ExternalGenerator::export) | The built-in renderer builds the site with templates that write each page's template context — `site`, `page` and `t`, the data the built-in templates read — instead of HTML. Every other file of that build is kept: the Markdown and structured twins, `llms.txt`, `robots.txt`, `security.txt`, sitemaps, feeds, the charter, media, redirects and the stylesheet. The contexts become a project in the generator's own layout |
//! | [`run`](ExternalGenerator::run) | The generator runs under `adk-sandbox`'s OS enforcer (Seatbelt on macOS, bubblewrap on Linux) with no network, reading only its program, its toolchain and the project, and writing only the project and its output. Without an enforcer the build fails |
//! | [`assemble`](ExternalGenerator::assemble) | Each page's HTML comes from the generator; static assets the generator adds (stylesheets, scripts, images, fonts) are kept where they do not collide; every other file is the built-in build's. A page the generator did not write fails the build |
//!
//! | Generator | Project layout | Page template | Writes each page to |
//! |-----------|----------------|---------------|---------------------|
//! | Zola | `config.toml`, `content/adk-<n>.md` with the context in `extra.ctx` | `templates/adk-page.html` (Tera) | `adk-pages/<n>/index.html` (`path`) |
//! | Hugo | `hugo.toml`, `content/adk-pages/<n>.md` with the context in `params.ctx` | `layouts/adk-page.html` (Go templates) | `adk-pages/<n>/index.html` (`url`) |
//! | Astro | `astro.config.mjs`, `src/adk/pages.json` | `src/pages/adk-pages/[n].astro` | `adk-pages/<n>/index.html` (`getStaticPaths`) |
//! | Build Awesome | `eleventy.config.cjs`, `src/_data/adkPages.json` | `src/adk-page.njk` (Nunjucks, autoescaped) | `adk-pages/<n>/index.html` (pagination) |
//!
//! [`with_project`](ExternalGenerator::with_project) builds from your own generator project —
//! a theme — instead of the starter: its files are copied, the starter files it lacks are
//! added, and the content files are written over it. A page template reads the same context
//! the built-in templates do, so it must link the page's twins (`page.markdown`,
//! `page.machine_view`) and carry its JSON-LD (`page.json_ld`); the launch gate checks both.
//!
//! Page bodies arrive rendered by adk-site's Markdown renderer, which escapes raw HTML and
//! drops unsafe URLs, so every generator publishes the same text; the generator owns layout.
//!
//! # Tolerances
//!
//! Every file but the page HTML is byte-identical to the built-in build, and two builds with
//! the same generator version, project and input give byte-identical page HTML. With the
//! starter templates Zola, Hugo and Build Awesome write the same page HTML; Astro differs in
//! whitespace and writes `<!DOCTYPE html>`, which browsers and the launch gate read the same.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use awp_types::GeneratorRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{BuildReport, BuiltinGenerator, SiteGenerator, list_files};
use crate::content::SiteProject;
use crate::error::{Result, SiteError};

#[cfg(feature = "gen-astro")]
mod astro;
#[cfg(feature = "gen-build-awesome")]
mod build_awesome;
#[cfg(feature = "gen-hugo")]
mod hugo;
#[cfg(feature = "gen-zola")]
mod zola;

/// Where every generator writes page `n`, relative to its output.
const PAGES_DIR: &str = "adk-pages";

/// Extensions of the static assets a generator may add to a site.
const ASSET_EXTENSIONS: &[&str] = &[
    "css", "js", "mjs", "map", "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "ico", "woff",
    "woff2", "ttf", "otf",
];

/// The templates that make the built-in renderer write template contexts.
const EXPORT_TEMPLATES: &[(&str, &str)] = &[
    ("item.html", r#"{{ {"template": "item", "site": site, "page": page, "t": t} | tojson }}"#),
    ("list.html", r#"{{ {"template": "list", "site": site, "page": page, "t": t} | tojson }}"#),
    ("notice.html", r#"{{ {"template": "notice", "site": site, "page": page, "t": t} | tojson }}"#),
];

/// An external static site generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum GeneratorKind {
    /// [Zola](https://www.getzola.org): one binary, Tera templates.
    #[cfg(feature = "gen-zola")]
    Zola,
    /// [Hugo](https://gohugo.io): one binary, Go templates.
    #[cfg(feature = "gen-hugo")]
    Hugo,
    /// [Astro](https://astro.build) on Node.js.
    #[cfg(feature = "gen-astro")]
    Astro,
    /// [Build Awesome](https://www.11ty.dev), formerly Eleventy, on Node.js.
    #[cfg(feature = "gen-build-awesome")]
    BuildAwesome,
}

impl GeneratorKind {
    /// The generator's name in release manifests.
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "gen-zola")]
            Self::Zola => "zola",
            #[cfg(feature = "gen-hugo")]
            Self::Hugo => "hugo",
            #[cfg(feature = "gen-astro")]
            Self::Astro => "astro",
            #[cfg(feature = "gen-build-awesome")]
            Self::BuildAwesome => "build-awesome",
        }
    }

    /// Whether the generator starts other processes (Astro's bundler starts esbuild).
    fn spawns(self) -> bool {
        match self {
            #[cfg(feature = "gen-zola")]
            Self::Zola => false,
            #[cfg(feature = "gen-hugo")]
            Self::Hugo => false,
            #[cfg(feature = "gen-astro")]
            Self::Astro => true,
            #[cfg(feature = "gen-build-awesome")]
            Self::BuildAwesome => true,
        }
    }
}

/// One page of an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedPage {
    /// The page's number, zero-padded: `0001`.
    pub n: String,
    /// The page's URL path.
    pub path: String,
    /// Where the generator writes it, relative to the generator's output.
    pub generated: String,
    /// Where it is published, relative to the site root.
    pub html: String,
}

/// A site exported for an external generator.
#[derive(Debug, Clone)]
pub struct Export {
    /// The generator project.
    pub project: PathBuf,
    /// The built-in build the export came from: its twins, site files and media are
    /// published as they are.
    pub reference: PathBuf,
    /// The built-in build's report.
    pub report: BuildReport,
    /// Every page, in the built-in build's order.
    pub pages: Vec<ExportedPage>,
}

/// A page's template context, as an exported generator project carries it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PageContext {
    pub n: String,
    pub template: String,
    pub site: Value,
    pub page: Value,
    pub t: Value,
}

impl PageContext {
    /// The context as one JSON object: `{n, template, site, page, t}`.
    pub(crate) fn value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The page's title, for generators that require one.
    #[cfg(any(feature = "gen-zola", feature = "gen-hugo"))]
    pub(crate) fn title(&self) -> String {
        self.page.get("title").and_then(Value::as_str).unwrap_or("page").to_string()
    }
}

/// Removes every `null`, empty list and empty object member, so a missing field tests false
/// in every template engine: Nunjucks and JavaScript treat an empty list as true, Tera fails
/// on a missing one.
fn prune(value: Value) -> Value {
    let empty = |v: &Value| match v {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    };
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter().map(|(k, v)| (k, prune(v))).filter(|(_, v)| !empty(v)).collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(prune).collect()),
        other => other,
    }
}

pub(crate) fn generator_error(kind: GeneratorKind, message: impl Into<String>) -> SiteError {
    SiteError::Generator { generator: kind.name().into(), message: message.into() }
}

/// Writes `bytes` to `root/relative`, creating its directories.
pub(crate) fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
    }
    std::fs::write(&path, bytes).map_err(|e| SiteError::io(&path, e))
}

/// Writes a starter file unless the project already has one.
pub(crate) fn write_starter(root: &Path, relative: &str, contents: &str) -> Result<()> {
    if root.join(relative).exists() {
        return Ok(());
    }
    write_file(root, relative, contents.as_bytes())
}

/// YAML front matter followed by an empty body.
#[cfg(feature = "gen-zola")]
pub(crate) fn front_matter(value: &Value) -> Result<Vec<u8>> {
    let yaml = serde_yaml::to_string(value).map_err(|e| SiteError::serde("front matter", e))?;
    Ok(format!("---\n{yaml}---\n").into_bytes())
}

/// A pretty JSON document with a trailing newline.
#[cfg(any(feature = "gen-astro", feature = "gen-build-awesome"))]
pub(crate) fn json_file(value: &Value) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| SiteError::serde("export data", e))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Copies a generator project, leaving out dependencies, version control and output.
fn copy_project(from: &Path, to: &Path) -> Result<()> {
    const SKIP: &[&str] = &["node_modules", ".git", "public", "_site", "dist", "output", ".astro"];
    for relative in list_files(from)? {
        if relative.split('/').next().is_some_and(|first| SKIP.contains(&first)) {
            continue;
        }
        let target = to.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
        }
        std::fs::copy(from.join(&relative), &target).map_err(|e| SiteError::io(&target, e))?;
    }
    Ok(())
}

/// A working directory, removed on drop unless the caller chose it.
struct WorkDir {
    path: PathBuf,
    keep: bool,
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// An external static site generator run in a sandbox with no network (spec 3.5 §7.8
/// "Generator").
///
/// See the [module documentation](self) for the export, the run and the assembly.
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::content::SiteProject;
/// use adk_site::generator::SiteGenerator;
/// use adk_site::generator::external::ExternalGenerator;
///
/// # async fn demo(project: &SiteProject) -> adk_site::Result<()> {
/// let zola = ExternalGenerator::zola("/opt/tools/zola").with_project("/srv/site/theme");
/// let out = std::path::Path::new("/srv/site/build");
/// let report = zola.build(project, out).await?;
/// assert_eq!(report.generator.name, "zola");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ExternalGenerator {
    kind: GeneratorKind,
    program: PathBuf,
    toolchain: Option<PathBuf>,
    project: Option<PathBuf>,
    work_dir: Option<PathBuf>,
    timeout: Duration,
}

impl ExternalGenerator {
    fn new(kind: GeneratorKind, program: PathBuf, toolchain: Option<PathBuf>) -> Self {
        Self {
            kind,
            program,
            toolchain,
            project: None,
            work_dir: None,
            timeout: Duration::from_secs(300),
        }
    }

    /// Zola, run from the `zola` binary at `program`.
    #[cfg(feature = "gen-zola")]
    pub fn zola(program: impl Into<PathBuf>) -> Self {
        Self::new(GeneratorKind::Zola, program.into(), None)
    }

    /// Hugo, run from the `hugo` binary at `program`.
    #[cfg(feature = "gen-hugo")]
    pub fn hugo(program: impl Into<PathBuf>) -> Self {
        Self::new(GeneratorKind::Hugo, program.into(), None)
    }

    /// Astro, run by the Node.js binary `node` from `node_modules`, a directory holding the
    /// `astro` package and its dependencies. Nothing is installed during a build.
    #[cfg(feature = "gen-astro")]
    pub fn astro(node: impl Into<PathBuf>, node_modules: impl Into<PathBuf>) -> Self {
        Self::new(GeneratorKind::Astro, node.into(), Some(node_modules.into()))
    }

    /// Build Awesome (Eleventy), run by the Node.js binary `node` from `node_modules`, a
    /// directory holding the `@11ty/eleventy` package and its dependencies.
    #[cfg(feature = "gen-build-awesome")]
    pub fn build_awesome(node: impl Into<PathBuf>, node_modules: impl Into<PathBuf>) -> Self {
        Self::new(GeneratorKind::BuildAwesome, node.into(), Some(node_modules.into()))
    }

    /// Builds from the generator project at `dir` (your theme) instead of the starter.
    #[must_use]
    pub fn with_project(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project = Some(dir.into());
        self
    }

    /// Keeps the export, the project and the generator's output under `dir` instead of a
    /// temporary directory removed after the build.
    #[must_use]
    pub fn with_work_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.work_dir = Some(dir.into());
        self
    }

    /// Stops the generator after `timeout`; five minutes by default.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Which generator this is.
    pub fn kind(&self) -> GeneratorKind {
        self.kind
    }

    /// Exports `project` for the generator under `work`: the built-in build in
    /// `work/reference` and the generator project in `work/run/project`.
    ///
    /// # Errors
    ///
    /// The built-in build's errors; [`SiteError::Io`] when a file cannot be written.
    pub async fn export(&self, project: &SiteProject, work: &Path) -> Result<Export> {
        let reference = work.join("reference");
        let run = work.join("run");
        let dir = run.join("project");
        for path in [&reference, &dir] {
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(|e| SiteError::io(path, e))?;
            }
            std::fs::create_dir_all(path).map_err(|e| SiteError::io(path, e))?;
        }
        let mut exporting = project.clone();
        for (name, source) in EXPORT_TEMPLATES {
            exporting.theme.templates.insert((*name).to_string(), (*source).to_string());
        }
        let report = BuiltinGenerator.build(&exporting, &reference).await?;
        let mut contexts = Vec::with_capacity(report.pages.len());
        let mut pages = Vec::with_capacity(report.pages.len());
        for (index, page) in report.pages.iter().enumerate() {
            let file = reference.join(&page.html);
            let bytes = std::fs::read(&file).map_err(|e| SiteError::io(&file, e))?;
            let mut context: Value = serde_json::from_slice(&bytes)
                .map_err(|e| SiteError::serde(format!("exported context of {}", page.path), e))?;
            let n = format!("{:04}", index + 1);
            let template =
                context.get("template").and_then(Value::as_str).unwrap_or("item").to_string();
            let mut take =
                |key: &str| prune(context.get_mut(key).map(Value::take).unwrap_or_default());
            contexts.push(PageContext {
                n: n.clone(),
                template,
                site: take("site"),
                page: take("page"),
                t: take("t"),
            });
            pages.push(ExportedPage {
                generated: format!("{PAGES_DIR}/{n}/index.html"),
                html: page.html.clone(),
                path: page.path.clone(),
                n,
            });
        }
        if let Some(own) = &self.project {
            copy_project(own, &dir)?;
        }
        let base_url = project.config.base_url();
        match self.kind {
            #[cfg(feature = "gen-zola")]
            GeneratorKind::Zola => zola::write_project(&dir, &base_url, &contexts)?,
            #[cfg(feature = "gen-hugo")]
            GeneratorKind::Hugo => hugo::write_project(&dir, &base_url, &contexts)?,
            #[cfg(feature = "gen-astro")]
            GeneratorKind::Astro => astro::write_project(&dir, &base_url, &contexts)?,
            #[cfg(feature = "gen-build-awesome")]
            GeneratorKind::BuildAwesome => {
                build_awesome::write_project(&dir, &base_url, &contexts)?
            }
        }
        tracing::info!(generator = self.kind.name(), pages = pages.len(), "site exported");
        Ok(Export { project: dir, reference, report, pages })
    }

    /// A package's `package.json` in the toolchain.
    #[cfg(any(feature = "gen-astro", feature = "gen-build-awesome"))]
    fn package_json(&self, package: &str) -> Result<Value> {
        let file = self.toolchain.clone().unwrap_or_default().join(package).join("package.json");
        let bytes = std::fs::read(&file).map_err(|e| SiteError::io(&file, e))?;
        serde_json::from_slice(&bytes).map_err(|e| SiteError::serde(file.display().to_string(), e))
    }

    /// The command-line entry point a package declares in its `bin` field.
    #[cfg(any(feature = "gen-astro", feature = "gen-build-awesome"))]
    fn package_bin(&self, package: &str, command: &str) -> Result<OsString> {
        let json = self.package_json(package)?;
        let entry = match json.get("bin") {
            Some(Value::String(entry)) => Some(entry.as_str()),
            Some(Value::Object(bins)) => bins.get(command).and_then(Value::as_str),
            _ => None,
        }
        .ok_or_else(|| {
            generator_error(self.kind, format!("{package} declares no `{command}` command"))
        })?;
        let toolchain = self.toolchain.clone().unwrap_or_default();
        Ok(toolchain.join(package).join(entry).into_os_string())
    }

    /// The arguments that build the project into `output`.
    fn command(&self, export: &Export, output: &Path) -> Result<Vec<OsString>> {
        let project = || export.project.as_os_str().to_owned();
        Ok(match self.kind {
            #[cfg(feature = "gen-zola")]
            GeneratorKind::Zola => {
                vec![
                    "--root".into(),
                    project(),
                    "build".into(),
                    "--output-dir".into(),
                    output.into(),
                ]
            }
            #[cfg(feature = "gen-hugo")]
            GeneratorKind::Hugo => vec![
                "--source".into(),
                project(),
                "--destination".into(),
                output.into(),
                "--noBuildLock".into(),
            ],
            #[cfg(feature = "gen-astro")]
            GeneratorKind::Astro => {
                let _ = output;
                vec![
                    self.package_bin("astro", "astro")?,
                    "build".into(),
                    "--root".into(),
                    project(),
                ]
            }
            #[cfg(feature = "gen-build-awesome")]
            GeneratorKind::BuildAwesome => {
                let _ = project;
                let mut flag = OsString::from("--output=");
                flag.push(output);
                vec![
                    self.package_bin("@11ty/eleventy", "eleventy")?,
                    "--config=eleventy.config.cjs".into(),
                    flag,
                ]
            }
        })
    }

    /// The sandbox policy of a run: read the program's installation and the toolchain, write
    /// the run directory, no network.
    fn policy(&self, run: &Path, output: &Path) -> Result<adk_sandbox::SandboxPolicy> {
        let program = std::fs::canonicalize(&self.program).map_err(|e| {
            generator_error(self.kind, format!("cannot find {}: {e}", self.program.display()))
        })?;
        let program_dir = program.parent().unwrap_or(Path::new("/"));
        // Node.js keeps its libraries beside `bin/`.
        let install = if self.toolchain.is_some() {
            program_dir.parent().unwrap_or(program_dir)
        } else {
            program_dir
        };
        let home = run.join("home");
        let tmp = run.join("tmp");
        for dir in [&home, &tmp] {
            std::fs::create_dir_all(dir).map_err(|e| SiteError::io(dir, e))?;
        }
        let path_env = format!("{}:/usr/local/bin:/usr/bin:/bin", program_dir.display());
        let mut builder = adk_sandbox::SandboxPolicyBuilder::new()
            .allow_read(install)
            .allow_read_write(run)
            .env("PATH", path_env)
            .env("HOME", home.display().to_string())
            .env("TMPDIR", tmp.display().to_string())
            .env("LANG", "C.UTF-8")
            .env("ADK_SITE_OUT", output.display().to_string())
            .env("ASTRO_TELEMETRY_DISABLED", "1")
            .env("HUGO_CACHEDIR", tmp.join("hugo").display().to_string())
            .env("NO_COLOR", "1");
        if let Some(toolchain) = &self.toolchain {
            let toolchain = std::fs::canonicalize(toolchain).map_err(|e| {
                generator_error(self.kind, format!("cannot find {}: {e}", toolchain.display()))
            })?;
            builder = builder.allow_read(toolchain);
        }
        if self.kind.spawns() {
            builder = builder.allow_process_spawn();
        }
        Ok(builder.build())
    }

    async fn sandboxed(
        &self,
        args: &[OsString],
        cwd: &Path,
        run: &Path,
        output: &Path,
    ) -> Result<std::process::Output> {
        let enforcer = adk_sandbox::get_enforcer().map_err(|e| {
            generator_error(
                self.kind,
                format!("no OS sandbox is available ({e}); external generators run only sandboxed, with no network"),
            )
        })?;
        let policy = self.policy(run, output)?;
        let mut command = adk_sandbox::sandbox::sandboxed_command(
            enforcer.as_ref(),
            self.program.as_os_str(),
            args,
            &policy,
            cwd,
        )
        .map_err(|e| generator_error(self.kind, format!("cannot sandbox the generator: {e}")))?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|e| {
            generator_error(self.kind, format!("cannot start {}: {e}", self.program.display()))
        })?;
        tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                generator_error(self.kind, format!("did not finish within {:?}", self.timeout))
            })?
            .map_err(|e| generator_error(self.kind, e.to_string()))
    }

    /// The version a generator binary prints for `args`.
    #[cfg(any(feature = "gen-zola", feature = "gen-hugo"))]
    async fn printed_version(
        &self,
        args: &[&str],
        export: &Export,
        run: &Path,
        output: &Path,
    ) -> Result<String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let out = self.sandboxed(&args, &export.project, run, output).await?;
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        Ok(text
            .split_whitespace()
            .find(|w| w.trim_start_matches('v').starts_with(|c: char| c.is_ascii_digit()))
            .map(|w| w.trim_start_matches('v').split(['-', '+']).next().unwrap_or(w).to_string())
            .unwrap_or_else(|| "unknown".into()))
    }

    /// The version of a toolchain package.
    #[cfg(any(feature = "gen-astro", feature = "gen-build-awesome"))]
    fn package_version(&self, package: &str) -> Result<String> {
        let json = self.package_json(package)?;
        Ok(json.get("version").and_then(Value::as_str).unwrap_or("unknown").to_string())
    }

    /// The generator's version.
    async fn version(&self, export: &Export, run: &Path, output: &Path) -> Result<String> {
        let _ = (export, run, output);
        match self.kind {
            #[cfg(feature = "gen-zola")]
            GeneratorKind::Zola => self.printed_version(&["--version"], export, run, output).await,
            #[cfg(feature = "gen-hugo")]
            GeneratorKind::Hugo => self.printed_version(&["version"], export, run, output).await,
            #[cfg(feature = "gen-astro")]
            GeneratorKind::Astro => self.package_version("astro"),
            #[cfg(feature = "gen-build-awesome")]
            GeneratorKind::BuildAwesome => self.package_version("@11ty/eleventy"),
        }
    }

    /// Runs the generator over an export, writing into `output`, and returns its reference
    /// for the release manifest.
    ///
    /// # Errors
    ///
    /// [`SiteError::Generator`] when no OS sandbox is available, the generator cannot start,
    /// exits with an error or runs past the timeout.
    pub async fn run(&self, export: &Export, output: &Path) -> Result<GeneratorRef> {
        // Canonical paths: under Seatbelt a generator that walks a path through a symbolic
        // link (macOS `/var` → `/private/var`) finds nothing.
        let canonical =
            |path: &Path| std::fs::canonicalize(path).map_err(|e| SiteError::io(path, e));
        let project = canonical(&export.project)?;
        let run = project.parent().unwrap_or(&project).to_path_buf();
        let output = match (output.parent(), output.file_name()) {
            (Some(parent), Some(name)) if parent.exists() => canonical(parent)?.join(name),
            _ => output.to_path_buf(),
        };
        if !output.starts_with(&run) {
            return Err(generator_error(
                self.kind,
                format!("the output must sit under the run directory {}", run.display()),
            ));
        }
        if output.exists() {
            std::fs::remove_dir_all(&output).map_err(|e| SiteError::io(&output, e))?;
        }
        let export = &Export { project, ..export.clone() };
        let output = output.as_path();
        #[cfg(feature = "gen-astro")]
        if self.kind == GeneratorKind::Astro {
            astro::link_toolchain(&export.project, self.toolchain.as_deref())?;
        }
        let version = self.version(export, &run, output).await?;
        let args = self.command(export, output)?;
        let started = std::time::Instant::now();
        let result = self.sandboxed(&args, &export.project, &run, output).await?;
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            let stdout = String::from_utf8_lossy(&result.stdout);
            let detail: String = format!("{stderr}\n{stdout}")
                .trim()
                .chars()
                .rev()
                .take(2_000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(generator_error(
                self.kind,
                format!("exited with {}: {detail}", result.status),
            ));
        }
        tracing::info!(generator = self.kind.name(), version = %version, elapsed_ms = started.elapsed().as_millis() as u64, "generator ran");
        Ok(GeneratorRef { name: self.kind.name().into(), version })
    }

    /// Assembles the site in `out` from the generator's output in `generated` and the
    /// export's built-in build, leaving nothing else in `out`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Generator`] when the generator wrote no HTML for a page; I/O errors.
    pub fn assemble(&self, export: &Export, generated: &Path, out: &Path) -> Result<BuildReport> {
        let page_html: BTreeSet<&str> = export.pages.iter().map(|p| p.html.as_str()).collect();
        let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
        for path in &export.report.outputs {
            if !page_html.contains(path.as_str()) {
                files.insert(path.clone(), export.reference.join(path));
            }
        }
        for page in &export.pages {
            let source = generated.join(&page.generated);
            if !source.is_file() {
                return Err(generator_error(
                    self.kind,
                    format!(
                        "the generator wrote no page for {} (expected {})",
                        page.path, page.generated
                    ),
                ));
            }
            files.insert(page.html.clone(), source);
        }
        let mut dropped = Vec::new();
        for relative in list_files(generated)? {
            if files.contains_key(&relative) || relative.starts_with(&format!("{PAGES_DIR}/")) {
                continue;
            }
            let extension = relative.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
            if extension.as_deref().is_some_and(|e| ASSET_EXTENSIONS.contains(&e)) {
                files.insert(relative.clone(), generated.join(&relative));
            } else {
                dropped.push(relative);
            }
        }
        if !dropped.is_empty() {
            tracing::debug!(generator = self.kind.name(), dropped = ?dropped, "generator files left out of the site");
        }
        std::fs::create_dir_all(out).map_err(|e| SiteError::io(out, e))?;
        let removed: BTreeSet<String> =
            list_files(out)?.into_iter().filter(|path| !files.contains_key(path)).collect();
        for path in &removed {
            let full = out.join(path);
            std::fs::remove_file(&full).map_err(|e| SiteError::io(&full, e))?;
        }
        for (path, source) in &files {
            let target = out.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| SiteError::io(parent, e))?;
            }
            std::fs::copy(source, &target).map_err(|e| SiteError::io(source, e))?;
        }
        let mut report = export.report.clone();
        report.generator =
            GeneratorRef { name: self.kind.name().into(), version: "unknown".into() };
        report.outputs = files.keys().cloned().collect();
        report.written = report.outputs.clone();
        report.removed = removed;
        report.incremental = false;
        Ok(report)
    }
}

#[async_trait]
impl SiteGenerator for ExternalGenerator {
    async fn build(&self, project: &SiteProject, out: &Path) -> Result<BuildReport> {
        let work = match &self.work_dir {
            Some(dir) => WorkDir { path: dir.clone(), keep: true },
            None => {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                WorkDir {
                    path: std::env::temp_dir().join(format!(
                        "adk-site-{}-{}-{n}",
                        self.kind.name(),
                        std::process::id()
                    )),
                    keep: false,
                }
            }
        };
        std::fs::create_dir_all(&work.path).map_err(|e| SiteError::io(&work.path, e))?;
        let export = self.export(project, &work.path).await?;
        let generated = work.path.join("run").join("output");
        let generator = self.run(&export, &generated).await?;
        let mut report = self.assemble(&export, &generated, out)?;
        report.generator = generator;
        Ok(report)
    }
}

/// The Jinja-family page template shared by Zola (Tera) and Build Awesome (Nunjucks): the
/// built-in default templates with the context in `c`, `s` (`site`), `p` (`page`) and `t`.
/// Every loop is guarded, since Tera refuses to iterate a missing value.
#[cfg(any(feature = "gen-zola", feature = "gen-build-awesome"))]
pub(crate) const JINJA_PAGE: &str = r##"<!doctype html>
<html lang="{{ p.language }}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{% if p.title %}{% if p.title != s.name %}{{ p.title }} · {% endif %}{% endif %}{{ s.name }}</title>
{% if p.description %}<meta name="description" content="{{ p.description }}">
{% endif %}{% if p.noindex %}<meta name="robots" content="noindex">
{% endif %}<link rel="canonical" href="{{ p.url }}">
{% if p.translations %}{% for alt in p.translations %}<link rel="alternate" hreflang="{{ alt.language }}" href="{{ alt.url }}">
{% endfor %}{% endif %}{% if s.contract %}<link rel="awp" href="{{ s.contract }}">
{% endif %}{% if p.markdown %}<link rel="alternate" type="text/markdown" href="{{ p.markdown }}">
{% endif %}{% if p.machine_view %}<link rel="alternate" type="application/awp+json" href="{{ p.machine_view }}">
{% endif %}<link rel="stylesheet" href="{{ s.stylesheet }}">
{% if p.json_ld %}<script type="application/ld+json">{{ p.json_ld | safe }}</script>
{% endif %}</head>
<body>
<a class="skip-link" href="#content">{{ t.skip_to_content }}</a>
<header class="site-header">
<a class="site-name" href="{{ s.home }}">{{ s.name }}</a>
{% if s.nav %}<nav aria-label="{{ t.main_navigation }}"><ul>{% for link in s.nav %}<li><a href="{{ link.url }}"{% if link.current %} aria-current="page"{% endif %}>{{ link.title }}</a></li>{% endfor %}</ul></nav>
{% endif %}</header>
<main id="content">
{% if c.template == "item" %}<article>
<header>
<h1>{{ p.title }}</h1>
{% if p.byline or p.date %}<p class="meta">{% if p.byline %}<span class="byline">{{ t.by }} {{ p.byline }}</span> {% endif %}{% if p.date %}<time datetime="{{ p.date }}">{{ p.date }}</time>{% endif %}</p>
{% endif %}</header>
{% if p.notices %}{% for notice in p.notices %}<aside class="notice notice-{{ notice.class }}" aria-label="{{ notice.title }}"><p><strong>{{ notice.title }}</strong>{% if notice.date %} <time datetime="{{ notice.date }}">{{ notice.date }}</time>{% endif %}</p>{% if notice.text %}<p>{{ notice.text }}</p>{% endif %}</aside>
{% endfor %}{% endif %}{% if p.figures %}{% for figure in p.figures %}<figure><img src="{{ figure.src }}" alt="{{ figure.alt }}"{% if figure.width %} width="{{ figure.width }}" height="{{ figure.height }}"{% endif %}{% if not loop.first %} loading="lazy"{% endif %}>{% if figure.caption %}<figcaption>{{ figure.caption }}</figcaption>{% endif %}</figure>
{% endfor %}{% endif %}{% if p.fields %}<dl class="facts">{% for field in p.fields %}<dt>{{ field.label }}</dt><dd>{{ field.value }}</dd>{% endfor %}</dl>
{% endif %}{% if p.live %}<dl class="live">{% for field in p.live %}<dt>{{ field.label }}</dt><dd data-awp-live="{{ field.name }}">{% if field.value %}{{ field.value }} <small>({{ t.as_of }} <time datetime="{{ field.as_of }}">{{ field.as_of_date }}</time>)</small>{% else %}{{ t.check_availability }}{% endif %}</dd>{% endfor %}</dl>
{% endif %}{% if p.body %}<div class="body">
{{ p.body | safe }}</div>
{% endif %}{% if p.stream %}<section class="stream" data-awp-stream="{{ p.stream.id }}" aria-label="{{ t.live_updates }}"><h2>{{ t.live_updates }}</h2><ol reversed>{% if p.stream.entries %}{% for entry in p.stream.entries %}<li id="entry-{{ entry.n }}"><time datetime="{{ entry.at }}">{{ entry.time }}</time>{% if entry.corrects %} <small>({{ t.corrects }} #{{ entry.corrects }})</small>{% endif %}{{ entry.html | safe }}</li>{% endfor %}{% endif %}</ol></section>
{% endif %}{% if p.tags %}<p class="tags">{{ t.tags }}: {% for tag in p.tags %}<a href="{{ tag.url }}">{{ tag.name }}</a>{% if not loop.last %}, {% endif %}{% endfor %}</p>
{% endif %}{% if p.sections %}{% for section in p.sections %}<section aria-labelledby="section-{{ loop.index }}">
<h2 id="section-{{ loop.index }}">{{ section.title }}</h2>
<ul class="items">{% if section.items %}{% for entry in section.items %}<li><a href="{{ entry.url }}">{{ entry.title }}</a>{% if entry.date %} <time datetime="{{ entry.date }}">{{ entry.date }}</time>{% endif %}{% if entry.summary %}<p>{{ entry.summary }}</p>{% endif %}</li>{% endfor %}{% endif %}</ul>
{% if section.more %}<p><a href="{{ section.more }}">{{ t.all }} {{ section.title }}</a></p>{% endif %}
</section>
{% endfor %}{% endif %}</article>
{% elif c.template == "list" %}<h1>{{ p.title }}</h1>
{% if p.body %}<div class="body">
{{ p.body | safe }}</div>
{% endif %}<ul class="items">{% if p.items %}{% for entry in p.items %}<li><h2><a href="{{ entry.url }}">{{ entry.title }}</a></h2>{% if entry.date %}<p class="meta"><time datetime="{{ entry.date }}">{{ entry.date }}</time></p>{% endif %}{% if entry.summary %}<p>{{ entry.summary }}</p>{% endif %}</li>
{% endfor %}{% endif %}</ul>
{% if p.pagination %}<nav aria-label="{{ t.pagination }}"><p>{{ t.page }} {{ p.pagination.current }} {{ t.of }} {{ p.pagination.total }}</p><ul>{% if p.pagination.prev %}<li><a href="{{ p.pagination.prev }}" rel="prev">{{ t.previous }}</a></li>{% endif %}{% if p.pagination.next %}<li><a href="{{ p.pagination.next }}" rel="next">{{ t.next }}</a></li>{% endif %}</ul></nav>
{% endif %}{% else %}<h1>{{ p.title }}</h1>
{% if p.notices %}{% for notice in p.notices %}<div class="notice notice-{{ notice.class }}"><p><strong>{{ notice.title }}</strong>{% if notice.date %} <time datetime="{{ notice.date }}">{{ notice.date }}</time>{% endif %}</p>{% if notice.text %}<p>{{ notice.text }}</p>{% endif %}</div>
{% endfor %}{% endif %}<p><a href="{{ s.home }}">{{ t.home }}</a></p>
{% endif %}</main>
<footer class="site-footer">
{% if s.legal %}<nav aria-label="{{ t.legal_navigation }}"><ul>{% for link in s.legal %}<li><a href="{{ link.url }}">{{ link.title }}</a></li>{% endfor %}</ul></nav>
{% endif %}{% if p.translations %}<nav aria-label="{{ t.languages }}"><ul>{% for alt in p.translations %}<li><a href="{{ alt.url }}" hreflang="{{ alt.language }}" lang="{{ alt.language }}">{{ alt.name }}</a></li>{% endfor %}</ul></nav>
{% endif %}<p>{{ s.name }}</p>
</footer>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_and_empty_members_are_removed_at_every_depth() {
        let value = serde_json::json!({
            "a": null, "b": {"c": null, "d": 1}, "e": [{"f": null, "g": "x"}], "h": [], "i": {"j": []},
            "k": "", "l": 0, "m": false
        });
        assert_eq!(
            prune(value),
            serde_json::json!({"b": {"d": 1}, "e": [{"g": "x"}], "k": "", "l": 0, "m": false})
        );
    }
}
