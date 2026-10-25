//! Answer-engine optimisation tracking as the `aeo` KPI source (spec 3.5 §7.12
//! "Discoverability metrics", §7.20 AEO "tracking at Observe").

use std::sync::Arc;

use adk_core::{Clock, SystemClock};
use adk_harness::web::SearchProvider;
use adk_kernel::charter::KpiSource;
use adk_server::awp::BeaconStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Tally, check_site, total};

/// Hits read per question.
const HITS: usize = 10;

/// What one provider answered for one question (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AeoObservation {
    /// The `web_search` provider.
    pub provider: String,
    /// The question asked.
    pub question: String,
    /// Whether a result cites the site.
    pub cited: bool,
    /// The first citing result's position, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    /// The cited URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One run of an [`AeoTracker`] (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AeoReport {
    /// When it ran.
    pub at: DateTime<Utc>,
    /// Each provider's answer to each question.
    pub observations: Vec<AeoObservation>,
    /// The queries that failed, as `provider: error`.
    pub failures: Vec<String>,
}

/// Periodic questions to answer engines through `web_search` providers, recording whether
/// and how they cite the site (spec 3.5 §7.12, §7.20 "AEO", tracking at Observe).
///
/// Each run asks every provider every question and counts, for the day: `aeo.queries`, and
/// when a result's host is the site's domain or a subdomain of it, `aeo.citations`,
/// `aeo.citations:{provider}` and, within the first three results, `aeo.top3`. It only
/// reads: it fetches no page, and no result text reaches a model — only result URLs are
/// compared.
///
/// # Example
///
/// ```rust,ignore
/// let tracker = AeoTracker::new(site, "juniper.example", counts.clone())
///     .question("where can I buy rye bread in Nakuru")
///     .provider(Arc::new(SearxngSearch::new(searx_url)));
/// let report = tracker.run().await?;
/// ```
pub struct AeoTracker {
    site: Uuid,
    domain: String,
    questions: Vec<String>,
    providers: Vec<Arc<dyn SearchProvider>>,
    counts: Arc<dyn BeaconStore>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AeoTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AeoTracker")
            .field("site", &self.site)
            .field("domain", &self.domain)
            .field("questions", &self.questions.len())
            .field("providers", &self.providers.len())
            .finish_non_exhaustive()
    }
}

impl AeoTracker {
    /// A tracker of `site` at `domain`, counting in `counts`, with no questions or providers.
    pub fn new(site: Uuid, domain: impl Into<String>, counts: Arc<dyn BeaconStore>) -> Self {
        Self {
            site,
            domain: domain.into().trim_start_matches("www.").to_ascii_lowercase(),
            questions: Vec::new(),
            providers: Vec::new(),
            counts,
            clock: Arc::new(SystemClock),
        }
    }

    /// Asks `question` on every run.
    #[must_use]
    pub fn question(mut self, question: impl Into<String>) -> Self {
        self.questions.push(question.into());
        self
    }

    /// Asks through `provider` on every run.
    #[must_use]
    pub fn provider(mut self, provider: Arc<dyn SearchProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Dates counts with `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Whether `url` is on the site's domain.
    fn cites(&self, url: &str) -> bool {
        url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|host| host == self.domain || host.ends_with(&format!(".{}", self.domain)))
    }

    /// Asks every provider every question once and counts the citations (spec 3.5 §7.12).
    ///
    /// # Errors
    ///
    /// Returns the count store's error; a provider's failure is reported in the run's
    /// `failures` and counts nothing.
    pub async fn run(&self) -> adk_kernel::Result<AeoReport> {
        let at = self.clock.now();
        let day = at.date_naive();
        let mut tally = Tally::default();
        let mut report = AeoReport { at, observations: Vec::new(), failures: Vec::new() };
        for provider in &self.providers {
            for question in &self.questions {
                match provider.search(question, HITS, None).await {
                    Ok(hits) => {
                        let found = hits
                            .iter()
                            .take(HITS)
                            .enumerate()
                            .find(|(_, hit)| self.cites(&hit.url));
                        tally.add(day, "aeo.queries", 1);
                        if let Some((index, _)) = found {
                            tally.add(day, "aeo.citations", 1);
                            tally.add(day, format!("aeo.citations:{}", provider.name()), 1);
                            if index < 3 {
                                tally.add(day, "aeo.top3", 1);
                            }
                        }
                        report.observations.push(AeoObservation {
                            provider: provider.name().to_string(),
                            question: question.clone(),
                            cited: found.is_some(),
                            rank: found.map(|(index, _)| index + 1),
                            url: found.map(|(_, hit)| hit.url.clone()),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(provider = provider.name(), error = %error, "an AEO query failed");
                        report.failures.push(format!("{}: {error}", provider.name()));
                    }
                }
            }
        }
        tally.flush("aeo", self.counts.as_ref()).await?;
        Ok(report)
    }
}

/// [`AeoTracker`]'s counts as the `aeo` KPI source (spec 3.5 §7.12):
///
/// | Metric | Value over the window |
/// |---|---|
/// | `aeo.queries` | Questions asked, per provider |
/// | `aeo.citations`, `pages.answer_citations` | Answers that cite the site |
/// | `aeo.citations:{provider}` | The same, for one provider |
/// | `aeo.top3` | Answers that cite the site within the first three results |
/// | `aeo.citation_rate` | Citations per question asked; none without questions |
#[derive(Clone)]
pub struct AeoKpi {
    site: Uuid,
    counts: Arc<dyn BeaconStore>,
}

impl std::fmt::Debug for AeoKpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AeoKpi").field("site", &self.site).finish_non_exhaustive()
    }
}

impl AeoKpi {
    /// `site`'s AEO counts in `counts`.
    pub fn new(site: Uuid, counts: Arc<dyn BeaconStore>) -> Self {
        Self { site, counts }
    }
}

#[async_trait]
impl KpiSource for AeoKpi {
    fn name(&self) -> &str {
        "aeo"
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("aeo", self.site, site)?;
        match metric {
            "pages.answer_citations" => total("aeo", &self.counts, "aeo.citations", from, to).await,
            "aeo.citation_rate" => {
                let queries = total("aeo", &self.counts, "aeo.queries", from, to).await?;
                let citations = total("aeo", &self.counts, "aeo.citations", from, to).await?;
                Ok(queries
                    .zip(citations)
                    .filter(|(queries, _)| *queries > 0.0)
                    .map(|(queries, citations)| citations / queries))
            }
            _ => total("aeo", &self.counts, metric, from, to).await,
        }
    }
}
