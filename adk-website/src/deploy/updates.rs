//! The running site's constant updates (spec 3.5 §7.8.3, A-I23, A-I24, A-I27): the
//! `UpdateEngine` driven on its own schedule, its journal for `adk-rust site batches` and
//! `site feeds`, held deliveries and items as the owner's decision cards, and its alarms on
//! the owner's channels.
//!
//! | Part | Does |
//! |---|---|
//! | [`UpdateDriver`] | Ticks the engine when [`UpdateEngine::next_due`] says, journals each closed batch and the feed counters, and keeps the cards in step |
//! | [`UpdateJournal`] | `data/updates/batches.jsonl` (one `BatchRecord` per line) and `data/updates/feeds.json` |
//! | [`UpdateCards`] | Each held delivery and held item becomes an approval request in the kernel's inbox, which the owner's channels show as a decision card; the owner's answer releases or discards it, and the change's proposer cannot approve it (A-I23) |
//! | [`SiteAlarms`] | The engine's alarms on the product's P1/P2/P3 routing: a delivery held by its bounds is P2 |

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    AdkError, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApprovalState, ApproverSelector, Result, ToolConfirmationRequest,
};
use adk_site::updates::{
    AlarmSink, BatchRecord, Decision, FeedStatus, HoldReason, SiteAlarm, UpdateEngine,
};
use async_trait::async_trait;
use awp_types::ContentId;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::alarms::{Alarm, AlarmRouter};

/// The tool a held delivery's card names (spec 3.5 A-I27).
pub const RELEASE_DELIVERY: &str = "site_update_release_delivery";
/// The tool a held item's card names (spec 3.5 §7.8.3 "Approvals on exceptions only").
pub const RELEASE_ITEM: &str = "site_update_release_item";

fn site_error(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// The engine's journal beside the content repository, read by `adk-rust site batches` and
/// `site feeds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateJournal {
    dir: PathBuf,
}

impl UpdateJournal {
    /// The journal in `dir`, `data/updates/` of the project.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Appends `records` to `batches.jsonl`.
    ///
    /// # Errors
    ///
    /// The file system's error.
    pub fn append(&self, records: &[BatchRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(site_error)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("batches.jsonl"))
            .map_err(site_error)?;
        for record in records {
            let line = serde_json::to_string(record).map_err(site_error)?;
            writeln!(file, "{line}").map_err(site_error)?;
        }
        Ok(())
    }

    /// Replaces `feeds.json` with `feeds`.
    ///
    /// # Errors
    ///
    /// The file system's error.
    pub fn write_feeds(&self, feeds: &[FeedStatus]) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(site_error)?;
        let text = serde_json::to_vec_pretty(feeds).map_err(site_error)?;
        let temporary = self.dir.join("feeds.json.tmp");
        std::fs::write(&temporary, text).map_err(site_error)?;
        std::fs::rename(&temporary, self.dir.join("feeds.json")).map_err(site_error)
    }

    /// The batches journaled so far.
    ///
    /// # Errors
    ///
    /// A line that is not a batch record.
    pub fn batches(&self) -> Result<Vec<BatchRecord>> {
        match std::fs::read_to_string(self.dir.join("batches.jsonl")) {
            Ok(text) => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).map_err(site_error))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(site_error(error)),
        }
    }
}

/// Something the engine holds for the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    Delivery(u64),
    Item(ContentId),
}

/// What the owner decided on a card, once applied.
#[derive(Debug, Clone, PartialEq)]
pub struct CardOutcome {
    /// The card's approval request.
    pub request: String,
    /// Released or discarded.
    pub decision: Decision,
    /// Who decided.
    pub by: String,
    /// Batches the release closed.
    pub batches: Vec<BatchRecord>,
}

/// Held deliveries and items as the owner's decision cards (spec 3.5 §7.8.3, A-I23, A-I27):
/// approval requests in the kernel's inbox, which the owner's channels show as cards.
pub struct UpdateCards {
    engine: Arc<UpdateEngine>,
    approvals: Arc<dyn ApprovalService>,
    site: Uuid,
    app: String,
    owner: String,
    carded: Mutex<BTreeMap<String, Held>>,
}

impl UpdateCards {
    /// Cards for `engine`'s holds in `approvals`, for site `site` of app `app`, decided by
    /// `owner`.
    pub fn new(
        engine: Arc<UpdateEngine>,
        approvals: Arc<dyn ApprovalService>,
        site: Uuid,
        app: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            engine,
            approvals,
            site,
            app: app.into(),
            owner: owner.into(),
            carded: Mutex::new(BTreeMap::new()),
        }
    }

    fn request(
        &self,
        id: String,
        tool: &str,
        args: serde_json::Value,
        summary: String,
    ) -> ApprovalRequest {
        let call = ToolConfirmationRequest::new(tool, args);
        let scope = ApprovalScope::new(self.app.clone(), self.owner.clone(), "site-updates");
        let policy = ApprovalPolicy::new([ApproverSelector::human(self.owner.clone())]);
        ApprovalRequest::new(id, scope, &call, policy)
            .with_summary(summary)
            .with_risk(ApprovalRisk::Medium)
    }

    /// Submits a card for every hold that has none; returns the new cards.
    ///
    /// # Errors
    ///
    /// The approval service's error.
    pub async fn sync(&self) -> Result<Vec<ApprovalRequest>> {
        let mut carded = self.carded.lock().await;
        let mut submitted = Vec::new();
        for held in self.engine.held_deliveries().await {
            let id = format!("site-update-{}-delivery-{}", self.site, held.delivery);
            if carded.contains_key(&id) {
                continue;
            }
            let summary = format!(
                "Feed {} sent a delivery its bounds held: release it or discard it",
                held.feed
            );
            let args =
                json!({ "delivery": held.delivery, "feed": held.feed, "report": held.report });
            let request = self
                .approvals
                .submit(self.request(id.clone(), RELEASE_DELIVERY, args, summary))
                .await?;
            carded.insert(id, Held::Delivery(held.delivery));
            submitted.push(request);
        }
        for held in self.engine.held_items().await {
            let item = held.update.item.id.clone();
            let id = format!(
                "site-update-{}-item-{}-{}",
                self.site,
                item.0.replace('/', "~"),
                held.batch
            );
            if carded.contains_key(&id) {
                continue;
            }
            let why = match &held.reason {
                HoldReason::PriceRule { allowed_pct } => {
                    format!("its price moves more than {allowed_pct}%")
                }
                HoldReason::NewItem => "it is new".to_string(),
                HoldReason::FastTier { .. } => "it failed the fast checks".to_string(),
                _ => "it is an exception".to_string(),
            };
            let summary = format!("{} waits because {why}: publish it or discard it", item.0);
            let args =
                json!({ "item": item, "reason": held.reason, "proposer": held.update.proposer });
            let request = self
                .approvals
                .submit(self.request(id.clone(), RELEASE_ITEM, args, summary))
                .await?;
            carded.insert(id, Held::Item(item));
            submitted.push(request);
        }
        Ok(submitted)
    }

    /// Applies every decided card: an approval releases, a rejection discards; a release by
    /// the change's own proposer is refused and the card stays decided (A-I23).
    ///
    /// # Errors
    ///
    /// The approval service's error; a refused decision is logged and skipped.
    pub async fn apply(&self) -> Result<Vec<CardOutcome>> {
        let mut carded = self.carded.lock().await;
        let mut outcomes = Vec::new();
        let ids: Vec<String> = carded.keys().cloned().collect();
        for id in ids {
            let Some(request) = self.approvals.get(&id).await? else { continue };
            let decision = match request.state {
                ApprovalState::Approved => Decision::Release,
                ApprovalState::Rejected | ApprovalState::Expired | ApprovalState::Cancelled => {
                    Decision::Discard
                }
                ApprovalState::Pending | ApprovalState::Responded => continue,
            };
            let by = request
                .decisions
                .last()
                .map_or_else(|| self.owner.clone(), |decision| decision.approver.id.clone());
            let Some(held) = carded.remove(&id) else { continue };
            let batches = match &held {
                Held::Delivery(delivery) => {
                    self.engine.decide_delivery(*delivery, decision, &by).await.map(|o| o.batches)
                }
                Held::Item(item) => self.engine.decide_item(item, decision, &by).await,
            };
            match batches {
                Ok(batches) => outcomes.push(CardOutcome { request: id, decision, by, batches }),
                Err(error) => {
                    tracing::warn!(card = %id, error = %error, "the owner's decision was not applied");
                }
            }
        }
        Ok(outcomes)
    }
}

/// The engine's alarms on the product's routing (spec 3.5 §7.21): a delivery its bounds held
/// whole is P2 with its summary.
pub struct SiteAlarms {
    router: Arc<AlarmRouter>,
    site: Uuid,
}

impl SiteAlarms {
    /// Alarms of `site` through `router`.
    pub fn new(router: Arc<AlarmRouter>, site: Uuid) -> Self {
        Self { router, site }
    }
}

#[async_trait]
impl AlarmSink for SiteAlarms {
    async fn raise(&self, alarm: &SiteAlarm) -> adk_site::Result<()> {
        let summary = match alarm.detail.get("summary").and_then(serde_json::Value::as_str) {
            Some(summary) => format!("{}: {summary}", alarm.title),
            None => alarm.title.clone(),
        };
        self.router
            .raise(Alarm::new(self.site, alarm.urgency, alarm.code.clone(), summary, alarm.at))
            .await;
        Ok(())
    }
}

/// Drives the engine (spec 3.5 §7.8.3): ticks it when it is due, journals closed batches and
/// the feed counters, and keeps the cards in step.
pub struct UpdateDriver {
    engine: Arc<UpdateEngine>,
    journal: UpdateJournal,
    cards: Option<Arc<UpdateCards>>,
    journaled: Mutex<u64>,
}

impl UpdateDriver {
    /// A driver of `engine` journaling to `journal`, with `cards` for the holds.
    pub fn new(
        engine: Arc<UpdateEngine>,
        journal: UpdateJournal,
        cards: Option<Arc<UpdateCards>>,
    ) -> Self {
        Self { engine, journal, cards, journaled: Mutex::new(0) }
    }

    /// One step: applies decided cards, ticks the engine, journals every batch closed since
    /// the last step and the feed counters, and cards new holds. Returns the batches
    /// journaled.
    ///
    /// # Errors
    ///
    /// The engine's, the journal's or the approval service's error.
    pub async fn step(&self) -> Result<Vec<BatchRecord>> {
        if let Some(cards) = &self.cards {
            cards.apply().await?;
        }
        self.engine.tick().await.map_err(site_error)?;
        let mut journaled = self.journaled.lock().await;
        let fresh: Vec<BatchRecord> =
            self.engine.batches().await.into_iter().filter(|r| r.batch > *journaled).collect();
        self.journal.append(&fresh)?;
        if let Some(last) = fresh.iter().map(|r| r.batch).max() {
            *journaled = last;
        }
        self.journal.write_feeds(&self.engine.feeds().await)?;
        if let Some(cards) = &self.cards {
            cards.sync().await?;
        }
        Ok(fresh)
    }

    /// Steps until aborted: at the engine's next due time, and at least every `idle`.
    pub fn spawn(self: Arc<Self>, idle: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(error) = self.step().await {
                    tracing::warn!(error = %error, "the update engine's step failed");
                }
                let wait = match self.engine.next_due().await {
                    Some(due) => (due - chrono::Utc::now()).to_std().unwrap_or_default().min(idle),
                    None => idle,
                };
                tokio::time::sleep(wait.max(Duration::from_millis(50))).await;
            }
        })
    }
}
