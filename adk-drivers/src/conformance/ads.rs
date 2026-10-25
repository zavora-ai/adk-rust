//! The advertising suite (spec 3.5 §7.13, §11).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::ads::{AdsDriver, CampaignDraft, CampaignState};
use crate::error::DriverError;

/// Makes a fresh advertising adapter for [`ads_driver_contract`].
#[async_trait]
pub trait AdsHarness: Send + Sync {
    /// A fresh adapter with no campaigns.
    async fn driver(&self) -> Arc<dyn AdsDriver>;

    /// The draft the suite launches.
    fn draft(&self) -> CampaignDraft;

    /// The window the suite reads performance over.
    fn window(&self) -> (DateTime<Utc>, DateTime<Utc>);
}

/// Runs every advertising case on one fresh adapter, in this order, so a recorded-response
/// adapter replays one recording (spec 3.5 §7.13):
///
/// 1. Launching the harness's draft starts a campaign with its budget, bid and currency.
/// 2. A bid within the daily budget is set; one above it, or in another currency, is invalid.
/// 3. A pause stops the campaign; an unknown campaign is not found.
/// 4. A draft with a plain `http` landing page is invalid before any call.
/// 5. Performance over the window reads; a reversed window is invalid.
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn ads_driver_contract(harness: &dyn AdsHarness) {
    let ads = harness.driver().await;
    let draft = harness.draft();
    let campaign = ads.launch_campaign(&draft).await.expect("launch: started");
    assert_eq!(campaign.state, CampaignState::Active, "launch: active");
    assert_eq!(
        (campaign.daily_budget_minor, campaign.bid_minor, campaign.currency.as_str()),
        (draft.daily_budget_minor, draft.bid_minor, draft.currency.as_str()),
        "launch: the draft's budget, bid and currency"
    );

    let bid = draft.bid_minor + 1;
    let updated =
        ads.set_bid(&campaign.id, bid, &draft.currency).await.expect("bid: within the budget");
    assert_eq!(updated.bid_minor, bid, "bid: set");
    let error = ads
        .set_bid(&campaign.id, draft.daily_budget_minor + 1, &draft.currency)
        .await
        .expect_err("bid: above the daily budget");
    assert!(
        matches!(error, DriverError::Invalid { .. }),
        "bid: above the budget is invalid: {error:?}"
    );
    let other = if draft.currency == "EUR" { "USD" } else { "EUR" };
    let error = ads.set_bid(&campaign.id, bid, other).await.expect_err("bid: another currency");
    assert!(
        matches!(error, DriverError::Invalid { .. }),
        "bid: another currency is invalid: {error:?}"
    );

    let paused = ads.pause_campaign(&campaign.id).await.expect("pause: paused");
    assert_eq!(paused.state, CampaignState::Paused, "pause: paused");
    let error =
        ads.pause_campaign("cmp-no-such-campaign").await.expect_err("pause: unknown campaign");
    assert!(matches!(error, DriverError::NotFound { .. }), "pause: not found: {error:?}");

    let mut plain = draft.clone();
    plain.landing_page = "http://bakery.example/".into();
    let error = ads.launch_campaign(&plain).await.expect_err("draft: http landing page");
    assert!(matches!(error, DriverError::Invalid { .. }), "draft: invalid: {error:?}");

    let (from, to) = harness.window();
    ads.performance(from, to).await.expect("performance: reads");
    let error = ads.performance(to, from).await.expect_err("performance: reversed window");
    assert!(matches!(error, DriverError::Invalid { .. }), "performance: invalid: {error:?}");
}
