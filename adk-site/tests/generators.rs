//! External generators (spec 3.5 §7.8 "Generator"): each builds the shared fixture site and
//! its output passes the launch gate.
//!
//! The hermetic tests export the fixture site, check the generator project, and assemble the
//! site from output each generator wrote in a real run, recorded under
//! `tests/fixtures/generators/<generator>/`. The `#[ignore]`d tests run the real generators
//! sandboxed with no network; set `ADK_SITE_GEN_RECORD=1` to record their output again
//! after a change to the fixture site, the renderer or a starter template.
#![cfg(any(
    feature = "gen-zola",
    feature = "gen-hugo",
    feature = "gen-astro",
    feature = "gen-build-awesome"
))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_site::content::SiteProject;
use adk_site::gate::{FactStatus, FactTable, GateInput, GateMode, LaunchGate, Quarantine};
use adk_site::generator::external::{Export, ExternalGenerator};
use adk_site::generator::{BuildReport, BuiltinGenerator, SiteGenerator};
use awp_types::FactRef;
use chrono::{TimeZone, Utc};

async fn project() -> SiteProject {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site");
    SiteProject::load(&root, Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap()).await.unwrap()
}

fn recorded(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/generators").join(name)
}

/// Runs the full launch gate and fails with its blocking findings.
async fn gate_passes(project: &SiteProject, out: &Path, report: &BuildReport) {
    let gate = LaunchGate::new().with_claim_check(Arc::new(FactTable::new([(
        FactRef("fact-founded".into()),
        FactStatus::Known,
    )])));
    let quarantine = Quarantine::default();
    let verdict = gate
        .run(
            GateMode::Full,
            &GateInput { project, out, report, changed: None, quarantine: &quarantine },
        )
        .await
        .unwrap();
    assert!(
        verdict.passed(),
        "{}: {:#?}",
        verdict.summary(),
        verdict.blocking().collect::<Vec<_>>()
    );
}

fn files(dir: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.insert(path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found
}

/// Every file but the page HTML is the built-in build's, byte for byte.
async fn machine_files_match_the_builtin_build(
    project: &SiteProject,
    out: &Path,
    report: &BuildReport,
) {
    let builtin = tempfile::tempdir().unwrap();
    let reference = BuiltinGenerator.build(project, builtin.path()).await.unwrap();
    let pages: BTreeSet<&str> = reference.pages.iter().map(|p| p.html.as_str()).collect();
    for path in reference.outputs.iter().filter(|p| !pages.contains(p.as_str())) {
        assert_eq!(
            std::fs::read(out.join(path)).unwrap(),
            std::fs::read(builtin.path().join(path)).unwrap(),
            "{path} is the built-in build's"
        );
    }
    for page in &reference.pages {
        assert!(out.join(&page.html).is_file(), "{} has its page", page.path);
    }
    assert_eq!(report.pages, reference.pages);
    assert_eq!(report.redirects, reference.redirects);
}

/// Exports the fixture site, assembles it from recorded generator output and gates it.
async fn assembles_from_recorded_output(generator: ExternalGenerator, name: &str, layout: &[&str]) {
    let project = project().await;
    let work = tempfile::tempdir().unwrap();
    let export: Export = generator.export(&project, work.path()).await.unwrap();
    for file in layout {
        assert!(export.project.join(file).is_file(), "the {name} project has {file}");
    }
    assert_eq!(export.pages.len(), export.report.pages.len());
    let out = tempfile::tempdir().unwrap();
    let report = generator.assemble(&export, &recorded(name), out.path()).unwrap();
    assert_eq!(report.generator.name, name);
    gate_passes(&project, out.path(), &report).await;
    machine_files_match_the_builtin_build(&project, out.path(), &report).await;
    assert!(
        files(out.path()).iter().all(|f| !f.starts_with("adk-pages/")),
        "the generator's page paths are not published"
    );
}

/// Builds the fixture site with a real generator twice, gates it, compares the builds, and
/// records the output when `ADK_SITE_GEN_RECORD=1`.
async fn real_build(generator: ExternalGenerator, name: &str) {
    let project = project().await;
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let work = tempfile::tempdir().unwrap();
        // `ADK_SITE_GEN_KEEP=<dir>` keeps the export, project and generator output there.
        let work_path = std::env::var("ADK_SITE_GEN_KEEP")
            .map(PathBuf::from)
            .unwrap_or_else(|_| work.path().to_path_buf());
        let out = tempfile::tempdir().unwrap();
        let report =
            generator.clone().with_work_dir(&work_path).build(&project, out.path()).await.unwrap();
        assert_eq!(report.generator.name, name);
        assert_ne!(report.generator.version, "unknown");
        gate_passes(&project, out.path(), &report).await;
        machine_files_match_the_builtin_build(&project, out.path(), &report).await;
        let generated = work_path.join("run/output");
        if std::env::var("ADK_SITE_GEN_RECORD").is_ok_and(|v| v == "1") && outputs.is_empty() {
            let target = recorded(name);
            let _ = std::fs::remove_dir_all(&target);
            for file in files(&generated) {
                // The pages and the assets assembly keeps; what it leaves out is not recorded.
                let asset = report.outputs.contains(&file)
                    && !std::path::Path::new(&file)
                        .extension()
                        .is_some_and(|e| e == "html" || e == "xml" || e == "txt");
                if file.starts_with("adk-pages/") || asset {
                    let to = target.join(&file);
                    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                    std::fs::copy(generated.join(&file), to).unwrap();
                }
            }
        }
        let snapshot: Vec<(String, Vec<u8>)> = files(out.path())
            .into_iter()
            .map(|f| (f.clone(), std::fs::read(out.path().join(&f)).unwrap()))
            .collect();
        outputs.push((report.generator.version.clone(), snapshot));
    }
    assert_eq!(outputs[0], outputs[1], "two builds with the same input are byte-identical");
}

fn tool(var: &str) -> PathBuf {
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| panic!("set {var}")))
}

#[cfg(feature = "gen-zola")]
mod zola {
    use super::*;

    #[tokio::test]
    async fn zola_output_assembles_and_passes_the_gate() {
        let generator = ExternalGenerator::zola("zola");
        assembles_from_recorded_output(
            generator,
            "zola",
            &["config.toml", "templates/adk-page.html", "content/adk-0001.md"],
        )
        .await;
    }

    /// `ADK_SITE_ZOLA=/path/to/zola cargo nextest run -p adk-site --features gen-zola --run-ignored only zola_builds`
    #[tokio::test]
    #[ignore = "needs Zola: set ADK_SITE_ZOLA to the zola binary"]
    async fn zola_builds_the_fixture_site_sandboxed() {
        real_build(ExternalGenerator::zola(tool("ADK_SITE_ZOLA")), "zola").await;
    }
}

#[cfg(feature = "gen-hugo")]
mod hugo {
    use super::*;

    #[tokio::test]
    async fn hugo_output_assembles_and_passes_the_gate() {
        let generator = ExternalGenerator::hugo("hugo");
        assembles_from_recorded_output(
            generator,
            "hugo",
            &["hugo.toml", "layouts/adk-page.html", "content/adk-pages/0001.md"],
        )
        .await;
    }

    /// `ADK_SITE_HUGO=/path/to/hugo cargo nextest run -p adk-site --features gen-hugo --run-ignored only hugo_builds`
    #[tokio::test]
    #[ignore = "needs Hugo: set ADK_SITE_HUGO to the hugo binary"]
    async fn hugo_builds_the_fixture_site_sandboxed() {
        real_build(ExternalGenerator::hugo(tool("ADK_SITE_HUGO")), "hugo").await;
    }
}

#[cfg(feature = "gen-astro")]
mod astro {
    use super::*;

    #[tokio::test]
    async fn astro_output_assembles_and_passes_the_gate() {
        let generator = ExternalGenerator::astro("node", "node_modules");
        assembles_from_recorded_output(
            generator,
            "astro",
            &["astro.config.mjs", "src/pages/adk-pages/[n].astro", "src/adk/pages.json"],
        )
        .await;
    }

    /// `ADK_SITE_NODE=$(command -v node) ADK_SITE_ASTRO_MODULES=/path/to/node_modules cargo nextest run -p adk-site --features gen-astro --run-ignored only astro_builds`
    #[tokio::test]
    #[ignore = "needs Node.js and Astro: set ADK_SITE_NODE and ADK_SITE_ASTRO_MODULES"]
    async fn astro_builds_the_fixture_site_sandboxed() {
        real_build(
            ExternalGenerator::astro(tool("ADK_SITE_NODE"), tool("ADK_SITE_ASTRO_MODULES")),
            "astro",
        )
        .await;
    }
}

#[cfg(feature = "gen-build-awesome")]
mod build_awesome {
    use super::*;

    #[tokio::test]
    async fn build_awesome_output_assembles_and_passes_the_gate() {
        let generator = ExternalGenerator::build_awesome("node", "node_modules");
        assembles_from_recorded_output(
            generator,
            "build-awesome",
            &["eleventy.config.cjs", "src/adk-page.njk", "src/_data/adkPages.json"],
        )
        .await;
    }

    /// `ADK_SITE_NODE=$(command -v node) ADK_SITE_ELEVENTY_MODULES=/path/to/node_modules cargo nextest run -p adk-site --features gen-build-awesome --run-ignored only build_awesome_builds`
    #[tokio::test]
    #[ignore = "needs Node.js and Build Awesome: set ADK_SITE_NODE and ADK_SITE_ELEVENTY_MODULES"]
    async fn build_awesome_builds_the_fixture_site_sandboxed() {
        real_build(
            ExternalGenerator::build_awesome(
                tool("ADK_SITE_NODE"),
                tool("ADK_SITE_ELEVENTY_MODULES"),
            ),
            "build-awesome",
        )
        .await;
    }
}

#[cfg(feature = "gen-zola")]
mod rules {
    use super::*;

    fn copy_dir(from: &Path, to: &Path) {
        for file in files(from) {
            let target = to.join(&file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(from.join(&file), target).unwrap();
        }
    }

    /// A generator runs with no network and writes nowhere but its run directory; without an
    /// OS sandbox it does not run at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_generator_runs_without_network_and_writes_only_its_run_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir().unwrap();
        // The program's own directory is mounted read-only, so a write there fails in every
        // sandbox; a write to the host's temporary directory may land in the sandbox's private
        // root (bubblewrap), so only its absence on the host is checked.
        let read_only = tools.path().join("escape");
        let escape = std::env::temp_dir().join(format!("adk-site-escape-{}", std::process::id()));
        let _ = std::fs::remove_file(&escape);
        let script = tools.path().join("zola");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/bash
if [ "$1" = "--version" ]; then echo "zola 0.0.1"; exit 0; fi
if {{ exec 3<>/dev/tcp/1.1.1.1/80; }} 2>/dev/null; then echo "network reachable" >&2; exit 10; fi
if {{ printf x > "{}"; }} 2>/dev/null; then echo "wrote outside its run directory" >&2; exit 11; fi
{{ printf x > "{}"; }} 2>/dev/null
echo "confined" >&2
exit 12
"#,
                read_only.display(),
                escape.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = tempfile::tempdir().unwrap();
        let error = ExternalGenerator::zola(&script)
            .build(&project().await, out.path())
            .await
            .unwrap_err()
            .to_string();
        if adk_sandbox::get_enforcer().is_ok() {
            assert!(error.contains("confined"), "{error}");
        } else {
            assert!(error.contains("no OS sandbox"), "{error}");
        }
        assert!(!read_only.exists(), "nothing was written into the read-only program directory");
        assert!(!escape.exists(), "nothing reached the host's temporary directory");
    }

    #[tokio::test]
    async fn assembly_keeps_generator_assets_drops_the_rest_and_needs_every_page() {
        let project = project().await;
        let generator = ExternalGenerator::zola("zola");
        let work = tempfile::tempdir().unwrap();
        let export = generator.export(&project, work.path()).await.unwrap();
        let generated = tempfile::tempdir().unwrap();
        copy_dir(&recorded("zola"), generated.path());
        for (path, bytes) in [
            ("theme/site.js", "console.log(1)"),
            ("assets/site.css", "body { color: red }"),
            ("rss.xml", "<rss/>"),
            ("sitemap.xml", "<urlset/>"),
            ("tags/index.html", "<p>tags</p>"),
            ("search_index.en.json", "[]"),
        ] {
            let target = generated.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("stale.html"), "old").unwrap();
        let report = generator.assemble(&export, generated.path(), out.path()).unwrap();
        let site = files(out.path());
        assert!(site.contains("theme/site.js"), "a generator asset is kept");
        assert_ne!(
            std::fs::read(out.path().join("assets/site.css")).unwrap(),
            b"body { color: red }",
            "the built-in stylesheet wins"
        );
        for dropped in ["rss.xml", "tags/index.html", "search_index.en.json", "stale.html"] {
            assert!(!site.contains(dropped), "{dropped} is not published");
        }
        assert!(report.removed.contains("stale.html"));
        assert_eq!(report.outputs, site);
        assert_ne!(
            std::fs::read(out.path().join("sitemap.xml")).unwrap(),
            b"<urlset/>",
            "the built-in sitemap wins"
        );

        std::fs::remove_file(generated.path().join("adk-pages/0003/index.html")).unwrap();
        let error = generator.assemble(&export, generated.path(), out.path()).unwrap_err();
        assert!(error.to_string().contains("wrote no page"), "{error}");
    }

    #[tokio::test]
    async fn your_own_project_keeps_its_templates_and_config() {
        let own = tempfile::tempdir().unwrap();
        for (path, bytes) in [
            ("config.toml", "base_url = \"https://example.com\"\ntitle = \"Mine\"\n"),
            ("templates/adk-page.html", "<p>{{ page.extra.ctx.page.title }}</p>"),
            ("static/fonts/brand.woff2", "font"),
            ("node_modules/left-pad/index.js", "module.exports = 1"),
            ("public/old.html", "old build"),
        ] {
            let target = own.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, bytes).unwrap();
        }
        let work = tempfile::tempdir().unwrap();
        let export = ExternalGenerator::zola("zola")
            .with_project(own.path())
            .export(&project().await, work.path())
            .await
            .unwrap();
        let read = |path: &str| std::fs::read_to_string(export.project.join(path)).unwrap();
        assert_eq!(read("templates/adk-page.html"), "<p>{{ page.extra.ctx.page.title }}</p>");
        assert!(read("config.toml").contains("Mine"));
        assert_eq!(read("static/fonts/brand.woff2"), "font");
        assert!(
            read("content/adk-0001.md").contains("adk-pages/0001"),
            "content is exported over the project"
        );
        assert!(
            export.project.join("content/_index.md").is_file(),
            "missing starter files are added"
        );
        assert!(
            !export.project.join("node_modules").exists()
                && !export.project.join("public").exists()
        );
    }
}
