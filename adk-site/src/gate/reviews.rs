//! Review quotes (spec 3.5 §5 S2, §7.18 "no invented … testimonials", A-I18): a page may
//! quote a review only with the reviewer's public attribution and the owner's approval. The
//! launch gate finds quotes by their words, whatever the page's markup, and reports them under
//! [`GateCheck::Claims`](super::GateCheck::Claims).

use awp_types::ContentItem;
use serde::{Deserialize, Serialize};

use super::{Finding, collect_text};

/// Consecutive words of a review that make a quote of it.
const QUOTE_WORDS: usize = 8;

/// A review a page might quote, and what makes quoting it publishable.
///
/// # Example
///
/// ```rust
/// use adk_site::gate::QuotableReview;
///
/// let review = QuotableReview::new("The sourdough is the best in town.")
///     .by("Wanjiru K.")
///     .approved("dec-7");
/// assert!(review.usable());
/// assert!(!QuotableReview::new("Good rye.").approved("dec-8").usable());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotableReview {
    /// The review's text.
    pub text: String,
    /// The reviewer's public display name, when the platform shows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The owner's decision that approved quoting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// Where the review was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl QuotableReview {
    /// A review with `text`, anonymous and not approved.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), author: None, approved_by: None, source: None }
    }

    /// Sets the reviewer's public name.
    #[must_use]
    pub fn by(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Records the owner's approving decision.
    #[must_use]
    pub fn approved(mut self, decision: impl Into<String>) -> Self {
        self.approved_by = Some(decision.into());
        self
    }

    /// Whether a page may quote it: a public attribution and the owner's approval.
    pub fn usable(&self) -> bool {
        self.author.as_deref().is_some_and(|a| !a.trim().is_empty()) && self.approved_by.is_some()
    }
}

/// Lower-case words, letters and digits only.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether `haystack` (words joined by single spaces, padded with spaces) holds a quote of
/// `review`: all of a short review, or any run of [`QUOTE_WORDS`] of a longer one.
fn quotes(haystack: &str, review: &[String]) -> bool {
    if review.is_empty() {
        return false;
    }
    let window = review.len().min(QUOTE_WORDS);
    review.windows(window).any(|run| haystack.contains(&format!(" {} ", run.join(" "))))
}

/// The blocking findings for `item`'s quotes of `reviews`.
pub(super) fn review_findings(item: &ContentItem, reviews: &[QuotableReview]) -> Vec<Finding> {
    if reviews.is_empty() {
        return Vec::new();
    }
    let mut text = item.body.as_ref().map(|b| b.0.clone()).unwrap_or_default();
    for value in item.fields.values() {
        collect_text(value, &mut text);
    }
    let haystack = format!(" {} ", words(&text).join(" "));
    let mut findings = Vec::new();
    for review in reviews {
        let review_words = words(&review.text);
        // Three words or fewer say too little to tell a quote from ordinary prose.
        if review_words.len() < 4 || !quotes(&haystack, &review_words) {
            continue;
        }
        let excerpt: String = review.text.chars().take(60).collect();
        let author = review.author.as_deref().map(str::trim).filter(|a| !a.is_empty());
        let problem = match (author, &review.approved_by) {
            (None, _) => Some((
                "review-quote-anonymous",
                "it has no public attribution, and a review is quoted only with the reviewer's public name".to_string(),
            )),
            (Some(_), None) => Some((
                "review-quote-unapproved",
                "the owner has not approved quoting it".to_string(),
            )),
            (Some(author), Some(_)) if !quotes(&haystack, &words(author)) => Some((
                "review-quote-unattributed",
                format!("the page does not name its reviewer, {author}"),
            )),
            (Some(_), Some(_)) => None,
        };
        if let Some((rule, why)) = problem {
            findings.push(Finding::blocking(
                rule,
                None,
                Some(item.id.clone()),
                format!("the item quotes the review \"{excerpt}…\": {why} (spec 3.5 S2)"),
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{ContentId, ContentKind, ContentState, Markdown};

    fn page(body: &str) -> ContentItem {
        ContentItem {
            id: ContentId("pages/reviews/en".into()),
            kind: ContentKind::Page,
            slug: "reviews".into(),
            language: "en".parse().unwrap(),
            fields: Default::default(),
            volatile: Vec::new(),
            body: Some(Markdown(body.into())),
            media: Vec::new(),
            sources: Vec::new(),
            schedule: None,
            state: ContentState::Published,
        }
    }

    #[test]
    fn quotes_are_found_by_their_words_and_policed() {
        let wanjiru = QuotableReview::new(
            "The sourdough is the best in Nakuru and it is still warm at seven.",
        )
        .by("Wanjiru K.");
        let anonymous = QuotableReview::new("Good rye, but they sold out by noon.").approved("d-2");
        let reviews = vec![wanjiru.clone(), anonymous];
        let rules = |body: &str, reviews: &[QuotableReview]| -> Vec<String> {
            review_findings(&page(body), reviews).into_iter().map(|f| f.rule).collect()
        };
        let quoted =
            "> *The sourdough is the best in Nakuru* and it is still warm…\n>\n> — Wanjiru K.";
        assert_eq!(rules(quoted, &reviews), ["review-quote-unapproved"]);
        let approved = vec![wanjiru.clone().approved("d-1")];
        assert!(rules(quoted, &approved).is_empty());
        assert_eq!(
            rules("> the sourdough is the best in Nakuru and it is still warm", &approved),
            ["review-quote-unattributed"]
        );
        assert_eq!(
            rules("Customers say: \"Good rye, but they sold out by noon!\"", &reviews),
            ["review-quote-anonymous"]
        );
        assert!(rules("We bake sourdough and rye every morning.", &reviews).is_empty());
    }
}
