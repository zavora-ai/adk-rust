//! Review quotes in the running site (spec 3.5 §5 S2, §7.18 "no invented … testimonials",
//! A-I18): the reviews discovery found, kept with the owner's approvals, and the launch-gate
//! check that polices every page quoting one.
//!
//! [`ReviewQuotes`] holds the reviews — durable in the project's `data/reviews.json` — and
//! [`QuotedReviews`] is a gate extension over them, so a review the owner approves while the
//! site runs counts at the next release without rebuilding the gate. A page may quote a
//! review only with the reviewer's public name on the page and the owner's approval.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{AdkError, Result};
use adk_site::gate::{
    CheckResult, CheckStatus, GateCheck, GateExtension, GateInput, GateMode, QuotableReview,
    review_findings,
};
use adk_site::migrate::ReviewQuote;
use async_trait::async_trait;
use tokio::sync::RwLock;

/// The reviews a site may quote and the owner's approvals (spec 3.5 S2).
///
/// # Example
///
/// ```rust
/// use adk_site::gate::QuotableReview;
/// use adk_website::quotes::ReviewQuotes;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let quotes = ReviewQuotes::in_memory();
/// quotes.add(vec![QuotableReview::new("The best sourdough in town.").by("Wanjiru K.")]).await.unwrap();
/// assert!(!quotes.list().await[0].usable());
/// quotes.approve(0, Some("dec-7")).await.unwrap();
/// assert!(quotes.list().await[0].usable());
/// # }
/// ```
#[derive(Debug)]
pub struct ReviewQuotes {
    path: Option<PathBuf>,
    reviews: RwLock<Vec<QuotableReview>>,
}

fn file_error(path: &Path, error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the site's review quotes at {}: {error}", path.display()))
}

impl ReviewQuotes {
    /// Quotes kept in memory only.
    pub fn in_memory() -> Self {
        Self { path: None, reviews: RwLock::new(Vec::new()) }
    }

    /// Quotes kept at `path`, read now when the file exists.
    ///
    /// # Errors
    ///
    /// A file that exists and does not read.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let reviews = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| file_error(&path, e))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(file_error(&path, error)),
        };
        Ok(Self { path: Some(path), reviews: RwLock::new(reviews) })
    }

    async fn save(&self, reviews: &[QuotableReview]) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let bytes = serde_json::to_vec_pretty(reviews).map_err(|e| file_error(path, e))?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| file_error(path, e))?;
        }
        let temporary = path.with_extension("json.tmp");
        tokio::fs::write(&temporary, bytes).await.map_err(|e| file_error(path, e))?;
        tokio::fs::rename(&temporary, path).await.map_err(|e| file_error(path, e))
    }

    /// Adds `found`, skipping a review already held with the same text.
    ///
    /// # Errors
    ///
    /// The file's error.
    pub async fn add(&self, found: Vec<QuotableReview>) -> Result<()> {
        let mut reviews = self.reviews.write().await;
        for review in found {
            if !reviews.iter().any(|held| held.text == review.text) {
                reviews.push(review);
            }
        }
        self.save(&reviews).await
    }

    /// Adds the reviews discovery found (spec 3.5 §7.18 stage 2), none approved.
    ///
    /// # Errors
    ///
    /// The file's error.
    pub async fn add_discovered(&self, found: &[ReviewQuote]) -> Result<()> {
        let found = found
            .iter()
            .map(|review| QuotableReview { approved_by: None, ..QuotableReview::from(review) })
            .collect();
        self.add(found).await
    }

    /// Records the owner's decision on review `index`: approved by `decision`, or `None` to
    /// withdraw an approval.
    ///
    /// # Errors
    ///
    /// `NotFound` for an index the list does not hold; the file's error.
    pub async fn approve(&self, index: usize, decision: Option<&str>) -> Result<()> {
        let mut reviews = self.reviews.write().await;
        let review = reviews.get_mut(index).ok_or_else(|| {
            AdkError::new(
                adk_core::ErrorComponent::Server,
                adk_core::ErrorCategory::NotFound,
                "website.quotes.not_found",
                format!("no review {index}"),
            )
        })?;
        review.approved_by = decision.map(str::to_string);
        self.save(&reviews).await
    }

    /// Every review held, in the order found.
    pub async fn list(&self) -> Vec<QuotableReview> {
        self.reviews.read().await.clone()
    }
}

/// The launch gate's review-quote check over [`ReviewQuotes`] (spec 3.5 S2): every item a
/// release shows that quotes a held review must name its reviewer and carry the owner's
/// approval. Reported as `GateCheck::Claims`, in both tiers.
#[derive(Debug, Clone)]
pub struct QuotedReviews {
    quotes: Arc<ReviewQuotes>,
}

impl QuotedReviews {
    /// The check over `quotes`.
    pub fn new(quotes: Arc<ReviewQuotes>) -> Self {
        Self { quotes }
    }
}

#[async_trait]
impl GateExtension for QuotedReviews {
    fn check(&self) -> GateCheck {
        GateCheck::Claims
    }

    fn runs_in(&self, _mode: GateMode) -> bool {
        true
    }

    async fn run(&self, input: &GateInput<'_>) -> adk_site::Result<CheckResult> {
        let reviews = self.quotes.list().await;
        let shown: std::collections::BTreeSet<_> =
            input.report.pages.iter().filter_map(|page| page.item.as_ref()).collect();
        let findings: Vec<_> = input
            .project
            .items
            .iter()
            .filter(|item| shown.contains(&item.id))
            .filter(|item| input.changed.is_none_or(|changed| changed.contains(&item.id)))
            .flat_map(|item| review_findings(item, &reviews))
            .collect();
        let status = if findings.iter().any(|f| f.blocking) {
            CheckStatus::Failed
        } else {
            CheckStatus::Passed
        };
        Ok(CheckResult {
            check: GateCheck::Claims,
            status,
            findings,
            note: Some(format!("{} review(s) the site may quote with approval", reviews.len())),
        })
    }
}
