//! Freeze (spec 3.5 §7.8 "Freeze", §7.18 stage 3, S4): the legacy site is crawled from a
//! private vantage point, the snapshot is published as a signed release, and its forms post to
//! edge capabilities. Nothing contacts the old server when a page is requested: the host
//! serves files.
//!
//! | Step | What happens |
//! |------|--------------|
//! | Prove | The domain must be proven (A-I9) |
//! | Crawl | A crawl with assets, through a client that reaches only the vantage origin, the public origin mapped onto it |
//! | Screen | Server code is left out (A-I1); detector findings hold their files out until an owner decides |
//! | Map | Contact, search, booking, shop and newsletter forms post to their edge capability; other forms become a notice |
//! | Release | A snapshot release in the site log (`ReleaseSource::Migration`), legacy redirects kept |
//! | Publish | To the host's production target; the old server can then leave the public path |

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use awp_types::{CapabilityV2, GeneratorRef, Redirect, ReleaseSource};
use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use super::SourceContext;
use super::audit::record;
use super::convert::Converted;
use super::detect::{Detectors, Finding};
use super::error::{MigrateError, Result};
use super::html::analyse;
use super::http::{ConnectionRecord, CrawlSigner, EgressScope, HttpClient, origin_of};
use super::inventory::{FeatureKind, SiteInventory, SourceKind};
use super::mapping::{FeatureMap, map_features};
use super::quarantine::QuarantineBook;
use super::sources::Crawl;
use crate::host::{HostTarget, PublishReport, SiteHost};
use crate::log::SiteLog;
use crate::release::{PreviousRelease, ReleaseBuilder, ReleaseOptions, StagedRelease};
use crate::scan::scan_file;

/// What to freeze.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreezeConfig {
    /// The legacy site's public origin, such as `https://example.com`.
    pub public_origin: Url,
    /// Where the crawler reaches the legacy server privately, such as `http://10.0.0.5:8080`.
    pub vantage: Url,
    /// Where the edge serves capability bindings, such as `https://agent.example.com`; forms
    /// post to `<edge>/awp/v2/capabilities/<name>`.
    pub edge_origin: Url,
}

/// One form of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenForm {
    /// The page.
    pub page: String,
    /// The feature it served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature: Option<FeatureKind>,
    /// The capability it now posts to; `None` when it became a notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

/// What a freeze did.
#[derive(Debug, Clone)]
pub struct FreezeReport {
    /// The release published.
    pub release: StagedRelease,
    /// The host's report.
    pub publish: PublishReport,
    /// Files in the snapshot.
    pub files: usize,
    /// Redirects carried over.
    pub redirects: Vec<Redirect>,
    /// Paths left out, and why.
    pub excluded: Vec<(String, String)>,
    /// The forms and what they became.
    pub forms: Vec<FrozenForm>,
    /// The detectors' findings, as a quarantine.
    pub quarantine: QuarantineBook,
    /// The capabilities the forms post to, for the edge.
    pub capabilities: Vec<CapabilityV2>,
    /// Every request the crawl made: all to the vantage origin.
    pub connections: Vec<ConnectionRecord>,
    /// What the crawl read.
    pub inventory: SiteInventory,
}

/// Freezes a legacy site (S4).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::migrate::SourceContext;
/// use adk_site::migrate::freeze::{Freeze, FreezeConfig};
/// # async fn demo(ctx: &SourceContext, log: &adk_site::log::SiteLog, host: &dyn adk_site::host::SiteHost) -> adk_site::migrate::Result<()> {
/// let freeze = Freeze::new(FreezeConfig {
///     public_origin: "https://example.com".parse().unwrap(),
///     vantage: "http://10.0.0.5:8080".parse().unwrap(),
///     edge_origin: "https://agent.example.com".parse().unwrap(),
/// });
/// let out = tempfile::tempdir().unwrap();
/// let report = freeze.run(ctx, out.path(), None, log, host).await?;
/// assert!(report.connections.iter().all(|c| c.origin == "http://10.0.0.5:8080"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Freeze {
    config: FreezeConfig,
    signer: Option<Arc<dyn CrawlSigner>>,
    min_interval: Duration,
    detectors: Detectors,
}

/// The snapshot file of a path, or `None` for pages a static host cannot serve (queries). An
/// asset's query is a cache buster that static hosts ignore, so it is dropped.
fn snapshot_file(path: &str, html: bool) -> Option<(String, Option<Redirect>)> {
    let path = if html { path } else { path.split('?').next().unwrap_or(path) };
    if path.contains('?') || !path.starts_with('/') || path.split('/').any(|s| s == "..") {
        return None;
    }
    let relative = path.trim_start_matches('/');
    if relative.is_empty() || relative.ends_with('/') {
        return Some((format!("{relative}index.html"), None));
    }
    let last = relative.rsplit('/').next().unwrap_or_default();
    if html && !last.contains('.') {
        return Some((
            format!("{relative}/index.html"),
            Some(Redirect { from: path.to_string(), to: format!("{path}/"), status: 301 }),
        ));
    }
    Some((relative.to_string(), None))
}

/// Rewrites the forms of a page: mapped ones post to their capability, the others become a
/// notice. Forms are matched to `kinds` in document order; text inside scripts and comments
/// is left alone.
fn rewrite_forms(
    page: &str,
    html: &str,
    kinds: &[Option<FeatureKind>],
    map: &FeatureMap,
    edge: &Url,
) -> (String, Vec<FrozenForm>) {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static SKIP: OnceLock<Regex> = OnceLock::new();
    static ACTION: OnceLock<Regex> = OnceLock::new();
    let action = ACTION.get_or_init(|| {
        Regex::new(r#"(?is)\s(action|method)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#)
            .expect("static regex")
    });
    let open = OPEN.get_or_init(|| Regex::new(r"(?is)<form\b[^>]*>").expect("static regex"));
    let skip = SKIP.get_or_init(|| {
        Regex::new(r"(?is)<script\b.*?</script>|<!--.*?-->").expect("static regex")
    });
    let skipped: Vec<(usize, usize)> = skip.find_iter(html).map(|m| (m.start(), m.end())).collect();
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    let mut index = 0;
    let mut forms = Vec::new();
    for found in open.find_iter(html) {
        if found.start() < at
            || skipped.iter().any(|(s, e)| found.start() >= *s && found.start() < *e)
        {
            continue;
        }
        let kind = kinds.get(index).copied().flatten();
        index += 1;
        out.push_str(&html[at..found.start()]);
        let capability = kind.and_then(|k| map.capability_for(k)).map(|c| c.name.clone());
        match &capability {
            Some(name) => {
                let target = edge
                    .join(&CapabilityV2::invocation_path(name))
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                let stripped = action.replace_all(found.as_str(), "");
                let tag = stripped.trim_end_matches('>').trim_end_matches('/');
                out.push_str(&format!(
                    "{tag} action=\"{target}\" method=\"post\" data-awp-capability=\"{name}\">"
                ));
                at = found.end();
            }
            None => {
                let close = html[found.end()..]
                    .to_ascii_lowercase()
                    .find("</form>")
                    .map(|i| found.end() + i + "</form>".len());
                out.push_str("<p class=\"awp-frozen-notice\">This form is not available while the site moves.</p>");
                at = close.unwrap_or(found.end());
            }
        }
        forms.push(FrozenForm { page: page.to_string(), feature: kind, capability });
    }
    out.push_str(&html[at..]);
    (out, forms)
}

impl Freeze {
    /// A freeze of `config`.
    pub fn new(config: FreezeConfig) -> Self {
        Self {
            config,
            signer: None,
            min_interval: Duration::from_millis(250),
            detectors: Detectors::new(),
        }
    }

    /// Signs the crawl.
    #[must_use]
    pub fn with_signer(mut self, signer: Arc<dyn CrawlSigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Spaces the crawl's requests (default 250 ms).
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// Crawls, writes the snapshot to `out`, releases it into `log` after `previous`, and
    /// publishes it to `host`.
    ///
    /// # Errors
    ///
    /// [`MigrateError::NotProven`] without a proven domain (A-I9); crawl, release and host
    /// errors.
    pub async fn run(
        &self,
        ctx: &SourceContext,
        out: &Path,
        previous: Option<&PreviousRelease>,
        log: &SiteLog,
        host: &dyn SiteHost,
    ) -> Result<FreezeReport> {
        let public = &self.config.public_origin;
        ctx.require_proven(public.host_str().unwrap_or_default(), "a freeze")?;
        let mut builder = HttpClient::builder()
            .egress(EgressScope::Only(BTreeSet::from([origin_of(&self.config.vantage)])))
            .map_origin(public, self.config.vantage.clone())
            .min_interval(self.min_interval);
        if let Some(signer) = &self.signer {
            builder = builder.signer(Arc::clone(signer));
        }
        let client = Arc::new(builder.build()?);
        let crawl_ctx = SourceContext { http: Arc::clone(&client), ..ctx.clone() };
        let mut inventory = SiteInventory::new(SourceKind::Crawl, ctx.clock.now());
        let start = public.join("/").map_err(|e| MigrateError::parse("public origin", e))?;
        Crawl::new(start).with_assets().crawl_into(&crawl_ctx, &mut inventory).await?;
        let findings: Vec<Finding> = self.detectors.scan_inventory(&inventory).await;
        let mut converted = Converted::default();
        for item in &inventory.items {
            converted.paths.insert(item.key.clone(), item.path.clone());
        }
        let quarantine = QuarantineBook::new(findings, &converted);
        let held = quarantine.excluded();
        let held_paths: BTreeSet<String> = held
            .iter()
            .map(|subject| converted.paths.get(subject).cloned().unwrap_or_else(|| subject.clone()))
            .collect();
        let map = map_features(&inventory);
        let mut excluded = Vec::new();
        let mut redirects = Vec::new();
        let mut forms = Vec::new();
        let mut files = 0usize;
        tokio::fs::create_dir_all(out).await.map_err(|e| crate::error::SiteError::io(out, e))?;
        for url in &inventory.urls {
            if let Some(target) =
                url.redirect_to.as_ref().filter(|_| matches!(url.status, 301 | 302 | 307 | 308))
            {
                if !url.path.contains('?') && target.starts_with('/') {
                    redirects.push(Redirect {
                        from: url.path.clone(),
                        to: target.clone(),
                        status: if url.status == 302 || url.status == 307 { 302 } else { 301 },
                    });
                }
                continue;
            }
            if url.status != 200 {
                continue;
            }
            let Some(body) = &url.body else { continue };
            let html = url.content_type.as_deref().is_some_and(|t| t.contains("html"));
            let Some((file, redirect)) = snapshot_file(&url.path, html) else {
                excluded.push((
                    url.path.clone(),
                    "a static host cannot serve a URL with a query".into(),
                ));
                continue;
            };
            if held_paths.contains(&url.path) || url.item.as_ref().is_some_and(|k| held.contains(k))
            {
                excluded.push((
                    url.path.clone(),
                    "quarantined: a detector finding waits for a decision".into(),
                ));
                continue;
            }
            let mut bytes = body.to_vec();
            let mut page_forms = Vec::new();
            if html {
                let text = String::from_utf8_lossy(body).into_owned();
                let page_url = public.join(&url.path).unwrap_or_else(|_| public.clone());
                let kinds: Vec<Option<FeatureKind>> =
                    analyse(&text, &page_url).forms.iter().map(|f| f.kind).collect();
                let (rewritten, rewritten_forms) =
                    rewrite_forms(&url.path, &text, &kinds, &map, &self.config.edge_origin);
                page_forms = rewritten_forms;
                bytes = rewritten.into_bytes();
            }
            if let Some(finding) = scan_file(&file, &bytes).into_iter().next() {
                excluded
                    .push((url.path.clone(), format!("server code (A-I1): {:?}", finding.kind)));
                continue;
            }
            forms.extend(page_forms);
            let target = out.join(&file);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| crate::error::SiteError::io(parent, e))?;
            }
            tokio::fs::write(&target, &bytes)
                .await
                .map_err(|e| crate::error::SiteError::io(&target, e))?;
            files += 1;
            redirects.extend(redirect);
        }
        let mut seen = BTreeSet::new();
        redirects.retain(|r| seen.insert(r.from.clone()));
        let capabilities: Vec<CapabilityV2> = {
            let used: BTreeSet<&String> =
                forms.iter().filter_map(|f| f.capability.as_ref()).collect();
            map.capabilities
                .iter()
                .map(|d| d.capability.clone())
                .filter(|c| used.contains(&c.name))
                .collect()
        };
        for (file, bytes) in host.redirects_format().render(&redirects) {
            let target = out.join(&file);
            tokio::fs::write(&target, bytes)
                .await
                .map_err(|e| crate::error::SiteError::io(&target, e))?;
        }
        let options = ReleaseOptions {
            source: ReleaseSource::Migration,
            generator: GeneratorRef {
                name: "adk-site-freeze".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            charter_version: 0,
            redirects: redirects.clone(),
            written: None,
            force_snapshot: true,
        };
        let release = ReleaseBuilder::new().release(out, previous, options, log).await?;
        let publish = host.publish(&release, HostTarget::Production).await?;
        tracing::info!(seq = release.manifest.seq, files, excluded = excluded.len(), "site frozen");
        if let Some(recorder) = &ctx.audit {
            let value = serde_json::json!({
                "origin": origin_of(public),
                "seq": release.manifest.seq,
                "manifest": release.digest,
                "files": files,
                "excluded": excluded.len(),
                "capabilities": capabilities.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            });
            record(recorder, &ctx.acting, "site.freeze", None, value).await?;
        }
        let excluded_paths: BTreeMap<String, String> = excluded.iter().cloned().collect();
        Ok(FreezeReport {
            release,
            publish,
            files,
            redirects,
            excluded: excluded_paths.into_iter().collect(),
            forms,
            quarantine,
            capabilities,
            connections: client.connections(),
            inventory,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::inventory::{DetectedFeature, SiteInventory};

    #[test]
    fn snapshot_paths() {
        assert_eq!(snapshot_file("/", true).unwrap().0, "index.html");
        assert_eq!(snapshot_file("/about/", true).unwrap().0, "about/index.html");
        let (file, redirect) = snapshot_file("/about", true).unwrap();
        assert_eq!(file, "about/index.html");
        assert_eq!(redirect.unwrap().to, "/about/");
        assert_eq!(snapshot_file("/a/b.css", false).unwrap().0, "a/b.css");
        assert!(snapshot_file("/?p=1", true).is_none());
        assert_eq!(snapshot_file("/a/style.css?ver=6.8", false).unwrap().0, "a/style.css");
    }

    #[test]
    fn forms_post_to_capabilities_or_become_notices() {
        let mut inventory = SiteInventory::new(SourceKind::Crawl, chrono::Utc::now());
        inventory.add_feature(DetectedFeature {
            kind: FeatureKind::ContactForm,
            evidence: vec![],
            fields: vec![],
        });
        let map = map_features(&inventory);
        let edge = Url::parse("https://agent.example.com").unwrap();
        let html = r#"<script>var t = "<form>";</script><form class="wpcf7" action="/contact/" method="post"><input name="a"></form>
<form id="loginform" action="/wp-login.php"><input type="password" name="pwd"></form><p>after</p>"#;
        let (out, forms) = rewrite_forms(
            "/contact/",
            html,
            &[Some(FeatureKind::ContactForm), Some(FeatureKind::Login)],
            &map,
            &edge,
        );
        assert!(out.contains(r#"<form class="wpcf7" action="https://agent.example.com/awp/v2/capabilities/submit_inquiry" method="post" data-awp-capability="submit_inquiry">"#), "{out}");
        assert!(!out.contains("wp-login.php"));
        assert!(out.contains("awp-frozen-notice"));
        assert!(out.contains(r#"var t = "<form>";"#));
        assert!(out.ends_with("<p>after</p>"));
        assert_eq!(forms.len(), 2);
        assert_eq!(forms[0].capability.as_deref(), Some("submit_inquiry"));
        assert_eq!(forms[1].capability, None);
        assert_eq!(forms[1].feature, Some(FeatureKind::Login));
    }
}
