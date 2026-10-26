//! Review quotes in the running site (spec 3.5 §5 S2, A-I18): the gate's check reads the
//! reviews the owner approves while the site runs, from the project's `data/reviews.json`.

use std::sync::Arc;

use adk_site::content::{SiteConfig, SiteProject};
use adk_site::gate::{GateInput, GateMode, LaunchGate, Quarantine, QuotableReview};
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_website::quotes::{QuotedReviews, ReviewQuotes};
use awp_types::{ContentId, ContentItem, ContentKind, ContentState, Markdown};
use serde_json::json;

const REVIEW: &str = "The sourdough is the best in Nakuru and it is still warm at seven.";

fn reviews_page(body: &str) -> ContentItem {
    ContentItem {
        id: ContentId("pages/reviews/en".into()),
        kind: ContentKind::Page,
        slug: "reviews".into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), json!("Reviews")),
            ("summary".to_string(), json!("What people say.")),
            ("goal".to_string(), json!("reputation")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown(body.into())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

async fn quote_rules(quotes: &Arc<ReviewQuotes>, body: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let config = SiteConfig::new("Juniper Bakery", "juniper.example", vec!["en".parse().unwrap()]);
    let project = SiteProject::new(config, vec![reviews_page(body)], chrono::Utc::now());
    let out = dir.path().join("out");
    let report = BuiltinGenerator.build(&project, &out).await.unwrap();
    let gate = LaunchGate::new().with_extension(Arc::new(QuotedReviews::new(quotes.clone())));
    let quarantine = Quarantine::default();
    let input = GateInput {
        project: &project,
        out: &out,
        report: &report,
        changed: None,
        quarantine: &quarantine,
    };
    let verdict = gate.run(GateMode::Fast, &input).await.unwrap();
    verdict
        .blocking()
        .map(|(_, finding)| finding.rule.clone())
        .filter(|rule| rule.starts_with("review-quote"))
        .collect()
}

#[tokio::test]
async fn a_page_quotes_a_found_review_only_as_the_owner_approved_and_the_approval_lasts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data/reviews.json");
    let quotes = Arc::new(ReviewQuotes::open(&path).unwrap());
    quotes
        .add(vec![
            QuotableReview::new(REVIEW).by("Wanjiru K."),
            QuotableReview::new("Good rye, but they sold out by noon again."),
        ])
        .await
        .unwrap();
    let quoted = format!("> {REVIEW}\n>\n> — Wanjiru K.");
    assert_eq!(quote_rules(&quotes, &quoted).await, ["review-quote-unapproved"]);
    let anonymous = "Customers say: good rye, but they sold out by noon again.";
    assert_eq!(quote_rules(&quotes, anonymous).await, ["review-quote-anonymous"]);

    quotes.approve(0, Some("dec-quote-1")).await.unwrap();
    assert!(quote_rules(&quotes, &quoted).await.is_empty(), "approved and attributed");
    let unattributed = format!("> {REVIEW}");
    assert_eq!(quote_rules(&quotes, &unattributed).await, ["review-quote-unattributed"]);

    let reopened = Arc::new(ReviewQuotes::open(&path).unwrap());
    assert!(reopened.list().await[0].usable(), "the approval survives a restart");
    assert!(quote_rules(&reopened, &quoted).await.is_empty());
}
