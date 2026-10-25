//! Variants as separate static files (spec 3.5 §7.12, §7.8): a wrapper around the site's
//! generator that adds each running experiment's variant and script to the build, and takes
//! a stopped one's out again.

use std::path::Path;
use std::sync::Arc;

use adk_site::content::SiteProject;
use adk_site::generator::{BuildReport, ChangeSet, SiteGenerator};
use adk_site::{Result, SiteError};
use async_trait::async_trait;
use uuid::Uuid;

use super::{Arm, ExperimentPlan, ExperimentStore, SCRIPT, SCRIPT_PATH, VARIANT_DIR};

/// The site's generator with its experiments (spec 3.5 §7.12): after the inner generator
/// builds, each running experiment's variant is written as its own static file under
/// `/_exp/{id}/` — the control page with the plan's changes, marked `noindex`, its canonical
/// link still naming the control — and the control, the variant and the goal pages load
/// `/_awp/experiments.js` with the experiment's id, arm, allocation and the edge's beacon.
///
/// Every file it adds or changes is reported as written and every file it takes out as
/// removed, so incremental releases carry them. A stopped experiment's variant leaves the
/// build and its script tags leave the pages: the control is served alone. An experiment
/// whose control page is missing or no longer holds a change's text is left out of the
/// build, with a warning, until its page matches again.
///
/// # Example
///
/// ```rust,ignore
/// let generator = Arc::new(ExperimentGenerator::new(
///     Arc::new(BuiltinGenerator),
///     experiments.clone(),
///     site.id,
///     "https://edge.example/awp/v2/beacon",
/// ));
/// let runtime = SiteRuntime::builder(site, base, store, generator, host, log, work).build();
/// ```
pub struct ExperimentGenerator {
    inner: Arc<dyn SiteGenerator>,
    store: Arc<dyn ExperimentStore>,
    site: Uuid,
    beacon: String,
}

impl std::fmt::Debug for ExperimentGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExperimentGenerator")
            .field("site", &self.site)
            .field("beacon", &self.beacon)
            .finish_non_exhaustive()
    }
}

impl ExperimentGenerator {
    /// `inner` with `site`'s experiments from `store`, counted at `beacon`, the edge's
    /// `POST /awp/v2/beacon` URL.
    pub fn new(
        inner: Arc<dyn SiteGenerator>,
        store: Arc<dyn ExperimentStore>,
        site: Uuid,
        beacon: impl Into<String>,
    ) -> Self {
        Self { inner, store, site, beacon: beacon.into() }
    }

    /// Adds the running experiments to the build in `out` and takes the stopped ones out.
    async fn apply(&self, out: &Path, report: &mut BuildReport) -> Result<()> {
        let records = self.store.list(self.site).await.map_err(|error| {
            SiteError::Pipeline(format!("the experiment store could not be read: {error}"))
        })?;
        let mut running = false;
        for record in &records {
            let plan = &record.plan;
            // Only validated plans are stored; skip one that no longer validates.
            let (Ok(control), Ok(variant)) = (plan.control_file(), plan.variant_file()) else {
                continue;
            };
            let goals: Vec<String> = plan
                .goal_pages
                .iter()
                .filter_map(|page| super::page_file(&plan.id, page).ok())
                .collect();
            // Every page loses the experiment's tag first, so a build never carries two.
            let mut pages = Vec::new();
            for file in std::iter::once(&control).chain(&goals) {
                if let Some(html) = read_html(out, file).await? {
                    pages.push((file.clone(), strip_tag(&html, &plan.id)));
                }
            }
            let built = if record.status.is_running() {
                self.variant_of(plan, &control, &pages)
            } else {
                None
            };
            match built {
                Some(variant_html) => {
                    running = true;
                    for (file, html) in &mut pages {
                        let arm = if *file == control { Some(Arm::Control) } else { None };
                        *html =
                            insert_head(html, &self.tag(plan, arm)).unwrap_or_else(|| html.clone());
                    }
                    write_if_changed(out, &variant, &variant_html, report).await?;
                }
                None => remove_file(out, &variant, report).await?,
            }
            for (file, html) in &pages {
                write_if_changed(out, file, html, report).await?;
            }
        }
        let script = SCRIPT_PATH.trim_start_matches('/');
        if running {
            write_if_changed(out, script, SCRIPT, report).await?;
        } else {
            remove_file(out, script, report).await?;
        }
        Ok(())
    }

    /// The variant's page, or `None` when the control cannot carry the experiment.
    fn variant_of(
        &self,
        plan: &ExperimentPlan,
        control: &str,
        pages: &[(String, String)],
    ) -> Option<String> {
        let Some((_, html)) = pages.iter().find(|(file, _)| file == control) else {
            tracing::warn!(experiment = %plan.id, page = %plan.page, "experiment left out of the build: its control page was not built");
            return None;
        };
        let mut variant = html.clone();
        for change in &plan.variant {
            if !variant.contains(&change.find) {
                tracing::warn!(experiment = %plan.id, page = %plan.page, "experiment left out of the build: its control page no longer holds a change's text");
                return None;
            }
            variant = variant.replace(&change.find, &change.replace);
        }
        let head = format!(
            "<meta name=\"robots\" content=\"noindex\">\n{}",
            self.tag(plan, Some(Arm::Variant))
        );
        let variant = insert_head(&variant, &head);
        if variant.is_none() {
            tracing::warn!(experiment = %plan.id, page = %plan.page, "experiment left out of the build: its control page has no </head>");
        }
        variant
    }

    /// The script tag of `plan` on a page of `arm`; `None` is a goal page.
    fn tag(&self, plan: &ExperimentPlan, arm: Option<Arm>) -> String {
        let mut tag = format!(
            "<script src=\"{SCRIPT_PATH}\" data-awp-experiment=\"{}\" data-arm=\"{}\"",
            plan.id,
            arm.map_or("", Arm::letter)
        );
        if let Some(arm) = arm {
            tag.push_str(&format!(" data-allocation=\"{}\"", plan.allocation));
            if arm == Arm::Control {
                tag.push_str(&format!(" data-variant=\"{}\"", escape(&plan.variant_url())));
            }
        }
        tag.push_str(&format!(" data-beacon=\"{}\"></script>", escape(&self.beacon)));
        tag
    }
}

#[async_trait]
impl SiteGenerator for ExperimentGenerator {
    async fn build(&self, project: &SiteProject, out: &Path) -> Result<BuildReport> {
        let mut report = self.inner.build(project, out).await?;
        self.apply(out, &mut report).await?;
        Ok(report)
    }

    async fn build_incremental(
        &self,
        project: &SiteProject,
        out: &Path,
        changes: &ChangeSet,
    ) -> Result<BuildReport> {
        let mut report = self.inner.build_incremental(project, out, changes).await?;
        self.apply(out, &mut report).await?;
        Ok(report)
    }
}

/// Escapes an attribute value.
fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `html` with `head` on its own line before `</head>`; `None` without one.
fn insert_head(html: &str, head: &str) -> Option<String> {
    let at = html.to_ascii_lowercase().find("</head>")?;
    Some(format!("{}{head}\n{}", &html[..at], &html[at..]))
}

/// `html` without the script tag of experiment `id`.
fn strip_tag(html: &str, id: &str) -> String {
    let start_marker = format!("<script src=\"{SCRIPT_PATH}\" data-awp-experiment=\"{id}\"");
    let mut html = html.to_string();
    while let Some(start) = html.find(&start_marker) {
        let Some(close) = html[start..].find("</script>") else { break };
        let mut end = start + close + "</script>".len();
        if html[end..].starts_with('\n') {
            end += 1;
        }
        html.replace_range(start..end, "");
    }
    html
}

async fn read_html(out: &Path, file: &str) -> Result<Option<String>> {
    let path = out.join(file);
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SiteError::Io { path, source }),
    }
}

/// Writes `file` when its content differs, and reports it written.
async fn write_if_changed(
    out: &Path,
    file: &str,
    content: &str,
    report: &mut BuildReport,
) -> Result<()> {
    let path = out.join(file);
    let same = matches!(tokio::fs::read(&path).await, Ok(bytes) if bytes == content.as_bytes());
    if !same {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| SiteError::Io { path: parent.to_path_buf(), source })?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|source| SiteError::Io { path: path.clone(), source })?;
        report.written.insert(file.to_string());
    }
    report.outputs.insert(file.to_string());
    report.removed.remove(file);
    Ok(())
}

/// Removes `file` when it exists, and reports it removed.
async fn remove_file(out: &Path, file: &str, report: &mut BuildReport) -> Result<()> {
    let path = out.join(file);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            report.removed.insert(file.to_string());
            if file.starts_with(VARIANT_DIR) {
                // Empty variant directories go with their last file.
                let mut dir = path.parent();
                while let Some(current) = dir.filter(|d| d.starts_with(out.join(VARIANT_DIR))) {
                    if tokio::fs::remove_dir(current).await.is_err() {
                        break;
                    }
                    dir = current.parent();
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(SiteError::Io { path, source }),
    }
    report.outputs.remove(file);
    report.written.remove(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_strip_cleanly() {
        let html = "<html><head><title>x</title>\n</head><body></body></html>";
        let tag = format!(
            "<script src=\"{SCRIPT_PATH}\" data-awp-experiment=\"h\" data-arm=\"a\"></script>"
        );
        let with = insert_head(html, &tag).unwrap();
        assert!(with.contains(&tag));
        assert_eq!(strip_tag(&with, "h"), html);
        assert_eq!(strip_tag(&with, "other"), with, "another experiment's tag stays");
        assert_eq!(insert_head("<p>no head</p>", &tag), None);
    }

    #[test]
    fn attribute_values_are_escaped() {
        assert_eq!(
            escape("https://e.example/b?a=1&b=\"2\""),
            "https://e.example/b?a=1&amp;b=&quot;2&quot;"
        );
    }
}
