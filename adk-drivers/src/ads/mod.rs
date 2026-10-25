//! Advertising drivers (spec 3.5 §7.13): spend and performance read, campaigns launched
//! only by a person's decision, bids and pauses within budget.
//!
//! | Tool | Effect class | Side | Default cap |
//! |---|---|---|---|
//! | `ads_performance` | `read` | back office | Read |
//! | `ads_launch_campaign` | `financial` (also `publish_public`) | back office | Always held |
//! | `ads_set_bid` | `financial` | back office | Within budget |
//! | `ads_pause_campaign` | `financial` | back office | Within budget (it only lowers spend) |
//!
//! | Adapter | Feature |
//! |---|---|
//! | [`InMemoryAds`] | — |
//! | [`RecordedAds`]: an advertising API's recorded responses, replayed, so no test spends; [`RecordingAds`] records them from a provider's sandbox | — |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::Tool;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::class::{AutonomyCap, Side, ToolSpec};
use crate::email::time_arg;
use crate::error::{DriverError, Result};
use crate::metrics::{KpiSource, unknown_metric};
use crate::outbound::str_arg;
use crate::tools::DriverTool;

mod recorded;

pub use recorded::{AdsInteraction, RecordedAds, RecordingAds};

/// A campaign to launch (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CampaignDraft {
    /// Its name.
    pub name: String,
    /// The ad's text.
    pub ad_text: String,
    /// Where the ad leads: an `https` address.
    pub landing_page: String,
    /// The most it spends a day, in minor units.
    pub daily_budget_minor: u64,
    /// The bid per click, in minor units.
    pub bid_minor: u64,
    /// The currency, as ISO 4217.
    pub currency: String,
}

impl CampaignDraft {
    /// Checks the draft (spec 3.5 §7.13): a name, ad text, an `https` landing page, a
    /// daily budget, and a bid within it.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] naming what is wrong.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.ad_text.trim().is_empty() {
            return Err(DriverError::invalid("a campaign needs a name and ad text"));
        }
        if !self.landing_page.starts_with("https://") {
            return Err(DriverError::invalid("a campaign's landing page is an https address"));
        }
        if self.daily_budget_minor == 0 {
            return Err(DriverError::invalid("a campaign needs a daily budget"));
        }
        check_bid(self.bid_minor, self.daily_budget_minor)?;
        if self.currency.len() != 3 {
            return Err(DriverError::invalid("the currency is an ISO 4217 code such as USD"));
        }
        Ok(())
    }
}

/// Refuses a zero bid or one above the daily budget.
pub(crate) fn check_bid(bid_minor: u64, daily_budget_minor: u64) -> Result<()> {
    if bid_minor == 0 || bid_minor > daily_budget_minor {
        return Err(DriverError::invalid(format!(
            "a bid is above zero and at most the campaign's daily budget ({daily_budget_minor})"
        )));
    }
    Ok(())
}

/// Whether a campaign runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum CampaignState {
    /// It runs and spends.
    Active,
    /// It is paused and spends nothing.
    Paused,
}

/// A campaign (spec 3.5 §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Campaign {
    /// Its id on the service.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Whether it runs.
    pub state: CampaignState,
    /// The most it spends a day, in minor units.
    pub daily_budget_minor: u64,
    /// The bid per click, in minor units.
    pub bid_minor: u64,
    /// The currency.
    pub currency: String,
}

/// A campaign's spend and results over a window (spec 3.5 §7.12, §7.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CampaignPerformance {
    /// The campaign.
    pub campaign: String,
    /// What it spent, in minor units.
    pub spend_minor: u64,
    /// The currency.
    pub currency: String,
    /// Times an ad was shown.
    pub impressions: u64,
    /// Clicks.
    pub clicks: u64,
    /// Conversions the service attributes to it.
    pub conversions: u64,
}

/// An advertising account (spec 3.5 §7.13).
///
/// A launch is `Financial` and `PublishPublic`, held for the owner in every mode; a bid
/// change and a pause are `Financial` within the owner's budget. The governed path holds
/// before the driver is called; the driver refuses a draft or bid that breaks the
/// contract.
#[async_trait]
pub trait AdsDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// Every campaign's performance over `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a reversed window, and the service's errors.
    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CampaignPerformance>>;

    /// Creates `draft` and starts it.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a draft [`CampaignDraft::validate`] refuses, and
    /// the service's errors.
    async fn launch_campaign(&self, draft: &CampaignDraft) -> Result<Campaign>;

    /// Sets `campaign`'s bid to `bid_minor` of `currency`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::NotFound`] for an unknown campaign, [`DriverError::Invalid`]
    /// for another currency or a bid above the daily budget, and the service's errors.
    async fn set_bid(&self, campaign: &str, bid_minor: u64, currency: &str) -> Result<Campaign>;

    /// Pauses `campaign`; pausing a paused campaign changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::NotFound`] for an unknown campaign, and the service's errors.
    async fn pause_campaign(&self, campaign: &str) -> Result<Campaign>;
}

/// An advertising account in process memory (spec 3.5 §7.13): campaigns and recorded
/// daily results, for tests and as the hermetic reference of the [`AdsDriver`] conformance
/// suite. Nothing is spent.
#[derive(Debug, Default)]
pub struct InMemoryAds {
    campaigns: Mutex<BTreeMap<String, Campaign>>,
    days: Mutex<Vec<(String, NaiveDate, CampaignPerformance)>>,
}

impl InMemoryAds {
    /// No campaigns.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a day of results for `campaign`.
    pub fn record_day(
        &self,
        campaign: &str,
        day: NaiveDate,
        spend_minor: u64,
        impressions: u64,
        clicks: u64,
        conversions: u64,
    ) {
        let currency = self
            .campaigns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(campaign)
            .map(|campaign| campaign.currency.clone())
            .unwrap_or_else(|| "USD".into());
        self.days.lock().unwrap_or_else(PoisonError::into_inner).push((
            campaign.to_string(),
            day,
            CampaignPerformance {
                campaign: campaign.to_string(),
                spend_minor,
                currency,
                impressions,
                clicks,
                conversions,
            },
        ));
    }

    /// The campaigns.
    pub fn campaigns(&self) -> Vec<Campaign> {
        self.campaigns.lock().unwrap_or_else(PoisonError::into_inner).values().cloned().collect()
    }
}

#[async_trait]
impl AdsDriver for InMemoryAds {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CampaignPerformance>> {
        if to <= from {
            return Err(DriverError::invalid("a window ends after it starts"));
        }
        let (first, last) = crate::analytics::day_range(from, to);
        let days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        let mut totals: BTreeMap<String, CampaignPerformance> = BTreeMap::new();
        for (campaign, _, result) in days.iter().filter(|(_, day, _)| *day >= first && *day <= last)
        {
            let total = totals.entry(campaign.clone()).or_insert_with(|| CampaignPerformance {
                campaign: campaign.clone(),
                spend_minor: 0,
                currency: result.currency.clone(),
                impressions: 0,
                clicks: 0,
                conversions: 0,
            });
            total.spend_minor += result.spend_minor;
            total.impressions += result.impressions;
            total.clicks += result.clicks;
            total.conversions += result.conversions;
        }
        Ok(totals.into_values().collect())
    }

    async fn launch_campaign(&self, draft: &CampaignDraft) -> Result<Campaign> {
        draft.validate()?;
        let campaign = Campaign {
            id: format!("cmp-{}", Uuid::new_v4().simple()),
            name: draft.name.clone(),
            state: CampaignState::Active,
            daily_budget_minor: draft.daily_budget_minor,
            bid_minor: draft.bid_minor,
            currency: draft.currency.to_ascii_uppercase(),
        };
        self.campaigns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(campaign.id.clone(), campaign.clone());
        Ok(campaign)
    }

    async fn set_bid(&self, campaign: &str, bid_minor: u64, currency: &str) -> Result<Campaign> {
        let mut campaigns = self.campaigns.lock().unwrap_or_else(PoisonError::into_inner);
        let found = campaigns
            .get_mut(campaign)
            .ok_or_else(|| DriverError::not_found(format!("campaign `{campaign}`")))?;
        if !found.currency.eq_ignore_ascii_case(currency) {
            return Err(DriverError::invalid(format!(
                "campaign `{campaign}` bids in {}",
                found.currency
            )));
        }
        check_bid(bid_minor, found.daily_budget_minor)?;
        found.bid_minor = bid_minor;
        Ok(found.clone())
    }

    async fn pause_campaign(&self, campaign: &str) -> Result<Campaign> {
        let mut campaigns = self.campaigns.lock().unwrap_or_else(PoisonError::into_inner);
        let found = campaigns
            .get_mut(campaign)
            .ok_or_else(|| DriverError::not_found(format!("campaign `{campaign}`")))?;
        found.state = CampaignState::Paused;
        Ok(found.clone())
    }
}

/// The spend and results of an [`AdsDriver`] as a [`KpiSource`] named `ads`:
/// `ads.spend`, `ads.clicks`, `ads.conversions` and `ads.cost_per_conversion`, over every
/// campaign.
pub struct AdsMetrics {
    driver: Arc<dyn AdsDriver>,
}

impl AdsMetrics {
    /// The source over `driver`.
    pub fn new(driver: Arc<dyn AdsDriver>) -> Self {
        Self { driver }
    }
}

impl AdsMetrics {
    /// The metrics it measures, as KPIs name them.
    pub fn metrics(&self) -> Vec<String> {
        ["ads.spend", "ads.clicks", "ads.conversions", "ads.cost_per_conversion"]
            .map(String::from)
            .to_vec()
    }

    /// The value of `metric` over `[from, to)`; `None` when there is no data for the
    /// window.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a metric it does
    /// not measure, and the service's errors.
    pub async fn measure(
        &self,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        if !self.metrics().iter().any(|known| known == metric) {
            return Err(unknown_metric("ads", self.metrics(), metric));
        }
        let results = self.driver.performance(from, to).await?;
        let spend: u64 = results.iter().map(|result| result.spend_minor).sum();
        let clicks: u64 = results.iter().map(|result| result.clicks).sum();
        let conversions: u64 = results.iter().map(|result| result.conversions).sum();
        Ok(match metric {
            "ads.spend" => Some(spend as f64),
            "ads.clicks" => Some(clicks as f64),
            "ads.conversions" => Some(conversions as f64),
            _ => (conversions > 0).then(|| spend as f64 / conversions as f64),
        })
    }
}

#[async_trait]
impl KpiSource for AdsMetrics {
    fn name(&self) -> &str {
        "ads"
    }

    async fn read(
        &self,
        _site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(self.measure(metric, from, to).await?)
    }
}

fn performance_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "from": {"type": "string", "format": "date-time"},
            "to": {"type": "string", "format": "date-time"}
        },
        "required": ["from", "to"]
    })
}

fn launch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "ad_text": {"type": "string"},
            "landing_page": {"type": "string", "description": "An https address on the site"},
            "daily_budget_minor": {"type": "integer", "minimum": 1},
            "bid_minor": {"type": "integer", "minimum": 1},
            "currency": {"type": "string"}
        },
        "required": ["name", "ad_text", "landing_page", "daily_budget_minor", "bid_minor", "currency"]
    })
}

fn bid_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "campaign": {"type": "string"},
            "bid_minor": {"type": "integer", "minimum": 1},
            "currency": {"type": "string"}
        },
        "required": ["campaign", "bid_minor", "currency"]
    })
}

fn pause_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "campaign": {"type": "string"} },
        "required": ["campaign"]
    })
}

/// The advertising class's tools (spec 3.5 §7.13).
pub const ADS_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "ads_performance",
        description: "Read every campaign's spend, impressions, clicks and conversions over a window.",
        class: EffectClass::Read,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: performance_schema,
    },
    ToolSpec {
        name: "ads_launch_campaign",
        description: "Create a campaign and start it. Spends money and publishes ads; always held for the owner.",
        class: EffectClass::Financial,
        also: Some(EffectClass::PublishPublic),
        side: Side::BackOffice,
        cap: AutonomyCap::AlwaysHeld,
        budget_arg: None,
        schema: launch_schema,
    },
    ToolSpec {
        name: "ads_set_bid",
        description: "Change a campaign's bid per click, within the owner's budget.",
        class: EffectClass::Financial,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::WithinBudget,
        budget_arg: Some("/bid_minor"),
        schema: bid_schema,
    },
    ToolSpec {
        name: "ads_pause_campaign",
        description: "Pause a campaign so it stops spending.",
        class: EffectClass::Financial,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::WithinBudget,
        budget_arg: None,
        schema: pause_schema,
    },
];

fn u64_arg(args: &Value, name: &str) -> Result<u64> {
    args.get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| DriverError::invalid(format!("`{name}` is a whole number")))
}

/// The advertising tools over `driver` (spec 3.5 §7.13).
pub fn ads_tools(driver: Arc<dyn AdsDriver>) -> Vec<Arc<dyn Tool>> {
    let name = format!("ads/{}", driver.name());
    let mut tools = Vec::new();
    for tool in ADS_TOOLS {
        let driver = Arc::clone(&driver);
        let tool: Arc<dyn Tool> = match tool.name {
            "ads_performance" => DriverTool::shared(tool, &name, move |args| {
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let results = driver
                        .performance(time_arg(&args, "from")?, time_arg(&args, "to")?)
                        .await?;
                    Ok(json!({ "campaigns": results }))
                })
            }),
            "ads_launch_campaign" => DriverTool::shared(tool, &name, move |args| {
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let draft = CampaignDraft {
                        name: str_arg(&args, "name")?,
                        ad_text: str_arg(&args, "ad_text")?,
                        landing_page: str_arg(&args, "landing_page")?,
                        daily_budget_minor: u64_arg(&args, "daily_budget_minor")?,
                        bid_minor: u64_arg(&args, "bid_minor")?,
                        currency: str_arg(&args, "currency")?,
                    };
                    Ok(serde_json::to_value(driver.launch_campaign(&draft).await?)
                        .unwrap_or_default())
                })
            }),
            "ads_set_bid" => DriverTool::shared(tool, &name, move |args| {
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let campaign = driver
                        .set_bid(
                            &str_arg(&args, "campaign")?,
                            u64_arg(&args, "bid_minor")?,
                            &str_arg(&args, "currency")?,
                        )
                        .await?;
                    Ok(serde_json::to_value(campaign).unwrap_or_default())
                })
            }),
            _ => DriverTool::shared(tool, &name, move |args| {
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let campaign = driver.pause_campaign(&str_arg(&args, "campaign")?).await?;
                    Ok(serde_json::to_value(campaign).unwrap_or_default())
                })
            }),
        };
        tools.push(tool);
    }
    tools
}
