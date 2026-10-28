//! Leads: their score, the handoff threshold, consent for nurture, and nurture sends inside
//! the owner's quiet hours (spec 3.5 §5 S8, §7.20 "Qualify and nurture with consent", "Warm
//! handoff with context", §7.21, A-I8, A-I17).
//!
//! | Part | Does |
//! |---|---|
//! | [`NURTURE_SCOPE`] | The consent scope a nurture sequence needs; the charter's public terms and the edge ask for it beside `contact` when the kind runs the nurture job |
//! | [`LeadScorer`] | Scores a lead from 0 to 100 by its consent, its qualifying answers and the nurture it received; the kind's threshold says when it is ready for the owner |
//! | [`LeadRecords`], [`SqliteLeadRecords`] | The site's contact book, kept durable: who wrote, with which receipts, how much nurture they received, and which leads were handed off |
//! | [`Leads`] | Rescores leads, answers the outbox's `ConsentCheck` from the edge's consent receipts, and keeps one handoff per lead |
//! | [`nurture_tools`] | The email outbox's sequence and newsletter sends, refused inside the owner's quiet hours, each send rescoring the recipient's leads |
//!
//! The lead itself holds no address (A-I3): the edge's form keeps it in the contact book, and
//! a lead carries only the contact's reference.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_awp::v2::store::{ConsentReceiptStore, Lead, LeadStore};
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock, Tool};
use adk_kernel::QuietHours;
use async_trait::async_trait;
use awp_types::{ConsentScope, ContactRef, LeadRef};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row as _;
use uuid::Uuid;

use crate::edge::handlers::{Contact, ContactBook};

/// The consent scope a nurture sequence needs (spec 3.5 §5 S8, A-I8): a visitor who asks to be
/// contacted about their enquiry (`contact`) has not asked for a sequence of follow-ups.
pub const NURTURE_SCOPE: &str = "nurture";

/// The score at which a lead is handed to the owner when the kind sets none (spec 3.5 §7.20).
pub const DEFAULT_HANDOFF_THRESHOLD: f64 = 70.0;

/// The qualifying questions a lead's answers are read for: when, how many, how much.
const QUALIFIERS: [(&str, &[&str]); 3] = [
    ("when", &["date", "when", "day", "deadline", "event date"]),
    ("size", &["guests", "people", "quantity", "size", "how many", "number", "headcount"]),
    ("budget", &["budget", "price range", "spend"]),
];

/// A lead's score and why (spec 3.5 §5 S8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeadScore {
    /// The lead.
    pub lead: LeadRef,
    /// From 0 to 100.
    pub value: f64,
    /// The threshold it is compared with.
    pub threshold: f64,
    /// Whether it reached the threshold, so the lead is ready for the owner.
    pub ready: bool,
    /// What scored, one line each.
    pub reasons: Vec<String>,
}

/// Scores leads (spec 3.5 §7.20 "Qualify and nurture with consent").
///
/// | Signal | Points |
/// |---|---|
/// | A live `contact` consent receipt | 20 |
/// | A live `nurture` consent receipt | 10 |
/// | Each qualifying answer — when, how many, budget — in the lead's answers or as `Key: value` in its message | 15 |
/// | Each nurture step sent, up to three | 5 |
///
/// # Example
///
/// ```rust
/// use adk_website::leads::LeadScorer;
///
/// let scorer = LeadScorer::new(70.0);
/// assert_eq!(scorer.threshold(), 70.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeadScorer {
    threshold: f64,
}

impl Default for LeadScorer {
    fn default() -> Self {
        Self::new(DEFAULT_HANDOFF_THRESHOLD)
    }
}

impl LeadScorer {
    /// A scorer that hands a lead off at `threshold`.
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }

    /// The handoff threshold.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Scores `lead`, whose person holds live receipts for `scopes` and received `nurtured`
    /// nurture steps.
    pub fn score(&self, lead: &Lead, scopes: &BTreeSet<String>, nurtured: u32) -> LeadScore {
        let mut value = 0.0;
        let mut reasons = Vec::new();
        if scopes.contains("contact") {
            value += 20.0;
            reasons.push("consented to be contacted (+20)".to_string());
        }
        if scopes.contains(NURTURE_SCOPE) {
            value += 10.0;
            reasons.push("consented to follow-ups (+10)".to_string());
        }
        for qualifier in qualifying_answers(lead) {
            value += 15.0;
            reasons.push(format!("answered {qualifier} (+15)"));
        }
        let steps = nurtured.min(3);
        if steps > 0 {
            value += 5.0 * f64::from(steps);
            reasons.push(format!("received {steps} nurture step(s) (+{})", 5 * steps));
        }
        let value = value.min(100.0);
        LeadScore {
            lead: lead.id.clone(),
            value,
            threshold: self.threshold,
            ready: value >= self.threshold,
            reasons,
        }
    }
}

/// The qualifying questions `lead` answered: an answer whose key names one, or a
/// `Key: value` sentence of its message that does.
fn qualifying_answers(lead: &Lead) -> Vec<&'static str> {
    let mut keys: Vec<String> = lead
        .answers
        .iter()
        .filter(|(key, value)| {
            !matches!(key.as_str(), "capability" | "message") && !value.as_str().trim().is_empty()
        })
        .map(|(key, _)| key.to_lowercase())
        .collect();
    if let Some(message) = lead.answers.get("message") {
        for sentence in message.as_str().split(['.', '\n', ';']) {
            if let Some((key, value)) = sentence.split_once(':')
                && !value.trim().is_empty()
            {
                keys.push(key.trim().to_lowercase());
            }
        }
    }
    QUALIFIERS
        .iter()
        .filter(|(_, names)| keys.iter().any(|key| names.iter().any(|name| key == name)))
        .map(|(qualifier, _)| *qualifier)
        .collect()
}

/// The site's records of the people behind its leads (spec 3.5 §7.10, A-I3, A-I8): the
/// contact book the edge's forms write, read by address for nurture, with each contact's
/// nurture count and the leads handed to the owner.
#[async_trait]
pub trait LeadRecords: ContactBook {
    /// The contacts whose address is `email`, in any case.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn by_email(&self, email: &str) -> Result<Vec<Contact>>;

    /// Counts one nurture step sent to `contact`; returns the count.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn note_nurture(&self, contact: &ContactRef) -> Result<u32>;

    /// The nurture steps sent to `contact`.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn nurtured(&self, contact: &ContactRef) -> Result<u32>;

    /// Records that `lead` was handed to the owner at `at`; returns `false` when it was
    /// already, so the owner gets one card per lead.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn hand_off(&self, lead: &LeadRef, at: DateTime<Utc>) -> Result<bool>;
}

fn store_error(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the site's lead records: {error}"))
}

/// [`LeadRecords`] in SQLite, such as the site's `data/edge.db`, so the people behind the
/// leads are still reachable after a restart.
///
/// # Example
///
/// ```rust
/// use adk_website::leads::{LeadRecords, SqliteLeadRecords};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let records = SqliteLeadRecords::in_memory().await.unwrap();
/// assert!(records.by_email("ana@example.com").await.unwrap().is_empty());
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteLeadRecords {
    pool: sqlx::SqlitePool,
}

impl SqliteLeadRecords {
    /// Records in `pool`; call [`migrate`](Self::migrate) once.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Records in a private in-memory database, migrated.
    ///
    /// # Errors
    ///
    /// The database's error.
    pub async fn in_memory() -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(store_error)?;
        let records = Self::new(pool);
        records.migrate().await?;
        Ok(records)
    }

    /// Creates the tables.
    ///
    /// # Errors
    ///
    /// The database's error.
    pub async fn migrate(&self) -> Result<()> {
        for statement in [
            "CREATE TABLE IF NOT EXISTS website_contacts (
                contact_ref TEXT PRIMARY KEY,
                email TEXT,
                name TEXT,
                consent TEXT NOT NULL,
                at TEXT NOT NULL,
                nurtured INTEGER NOT NULL DEFAULT 0
            )",
            "CREATE INDEX IF NOT EXISTS website_contacts_email ON website_contacts (email)",
            "CREATE TABLE IF NOT EXISTS website_handoffs (
                lead_ref TEXT PRIMARY KEY,
                at TEXT NOT NULL
            )",
        ] {
            sqlx::query(statement).execute(&self.pool).await.map_err(store_error)?;
        }
        Ok(())
    }

    async fn all(&self) -> Result<Vec<Contact>> {
        let rows =
            sqlx::query("SELECT contact_ref, email, name, consent, at FROM website_contacts")
                .fetch_all(&self.pool)
                .await
                .map_err(store_error)?;
        rows.iter().map(contact_of).collect()
    }
}

fn contact_of(row: &sqlx::sqlite::SqliteRow) -> Result<Contact> {
    let consent: String = row.try_get("consent").map_err(store_error)?;
    let at: String = row.try_get("at").map_err(store_error)?;
    Ok(Contact {
        contact_ref: ContactRef(row.try_get("contact_ref").map_err(store_error)?),
        email: row.try_get("email").map_err(store_error)?,
        name: row.try_get("name").map_err(store_error)?,
        consent: serde_json::from_str(&consent).map_err(store_error)?,
        at: DateTime::parse_from_rfc3339(&at).map_err(store_error)?.with_timezone(&Utc),
    })
}

fn address(email: &str) -> String {
    email.trim().to_lowercase()
}

#[async_trait]
impl ContactBook for SqliteLeadRecords {
    async fn put(&self, contact: Contact) -> Result<()> {
        // A second enquiry from the same person keeps the receipts of the first.
        let earlier: Vec<Uuid> =
            sqlx::query("SELECT consent FROM website_contacts WHERE contact_ref = ?")
                .bind(&contact.contact_ref.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(store_error)?
                .and_then(|row| row.try_get::<String, _>("consent").ok())
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        let consent: BTreeSet<Uuid> = earlier.into_iter().chain(contact.consent).collect();
        let consent =
            serde_json::to_string(&consent.into_iter().collect::<Vec<_>>()).map_err(store_error)?;
        sqlx::query(
            "INSERT INTO website_contacts (contact_ref, email, name, consent, at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (contact_ref) DO UPDATE SET
                email = excluded.email, name = excluded.name,
                consent = excluded.consent, at = excluded.at",
        )
        .bind(&contact.contact_ref.0)
        .bind(contact.email.as_deref().map(address))
        .bind(contact.name)
        .bind(consent)
        .bind(contact.at.to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }

    async fn by_receipts(&self, receipts: &[Uuid]) -> Result<Vec<Contact>> {
        Ok(self
            .all()
            .await?
            .into_iter()
            .filter(|contact| contact.consent.iter().any(|r| receipts.contains(r)))
            .collect())
    }

    async fn erase(&self, receipts: &[Uuid]) -> Result<usize> {
        let erased: Vec<Contact> = self.by_receipts(receipts).await?;
        for contact in &erased {
            sqlx::query("DELETE FROM website_contacts WHERE contact_ref = ?")
                .bind(&contact.contact_ref.0)
                .execute(&self.pool)
                .await
                .map_err(store_error)?;
        }
        Ok(erased.len())
    }
}

#[async_trait]
impl LeadRecords for SqliteLeadRecords {
    async fn by_email(&self, email: &str) -> Result<Vec<Contact>> {
        let rows = sqlx::query(
            "SELECT contact_ref, email, name, consent, at FROM website_contacts WHERE email = ?",
        )
        .bind(address(email))
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        rows.iter().map(contact_of).collect()
    }

    async fn note_nurture(&self, contact: &ContactRef) -> Result<u32> {
        sqlx::query("UPDATE website_contacts SET nurtured = nurtured + 1 WHERE contact_ref = ?")
            .bind(&contact.0)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        self.nurtured(contact).await
    }

    async fn nurtured(&self, contact: &ContactRef) -> Result<u32> {
        let count: Option<i64> =
            sqlx::query_scalar("SELECT nurtured FROM website_contacts WHERE contact_ref = ?")
                .bind(&contact.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(store_error)?;
        Ok(u32::try_from(count.unwrap_or(0)).unwrap_or(u32::MAX))
    }

    async fn hand_off(&self, lead: &LeadRef, at: DateTime<Utc>) -> Result<bool> {
        let done =
            sqlx::query("INSERT OR IGNORE INTO website_handoffs (lead_ref, at) VALUES (?, ?)")
                .bind(&lead.0)
                .bind(at.to_rfc3339())
                .execute(&self.pool)
                .await
                .map_err(store_error)?;
        Ok(done.rows_affected() == 1)
    }
}

/// The site's leads as the back office works them (spec 3.5 §7.20): scores, consent for
/// nurture, and one handoff per lead.
pub struct Leads {
    leads: Arc<dyn LeadStore>,
    records: Arc<dyn LeadRecords>,
    receipts: Arc<dyn ConsentReceiptStore>,
    scorer: LeadScorer,
    charters: Option<(Arc<dyn adk_kernel::charter::CharterStore>, Uuid)>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Leads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Leads").field("scorer", &self.scorer).finish_non_exhaustive()
    }
}

impl Leads {
    /// The leads in the edge's `leads` store, the people behind them in `records`, their
    /// consent in the edge's `receipts`, scored by `scorer`.
    pub fn new(
        leads: Arc<dyn LeadStore>,
        records: Arc<dyn LeadRecords>,
        receipts: Arc<dyn ConsentReceiptStore>,
        scorer: LeadScorer,
    ) -> Self {
        Self { leads, records, receipts, scorer, charters: None, clock: Arc::new(SystemClock) }
    }

    /// Reads the handoff threshold from `site`'s charter in force in `charters` (spec 3.5
    /// §5 S8, §7.6): its `handoffThreshold`, else the scorer's.
    #[must_use]
    pub fn with_charters(
        mut self,
        charters: Arc<dyn adk_kernel::charter::CharterStore>,
        site: Uuid,
    ) -> Self {
        self.charters = Some((charters, site));
        self
    }

    /// The clock consent and handoffs are read at.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The scorer used when the charter in force sets no handoff threshold.
    pub fn scorer(&self) -> LeadScorer {
        self.scorer
    }

    /// The scorer in force now: the charter's `handoffThreshold` when the charter in force
    /// sets one (spec 3.5 §5 S8), else [`scorer`](Self::scorer).
    pub async fn current_scorer(&self) -> LeadScorer {
        let Some((charters, site)) = &self.charters else { return self.scorer };
        match charters.current(*site).await {
            Ok(current) => current
                .and_then(|version| version.charter.handoff_threshold)
                .map_or(self.scorer, |threshold| LeadScorer::new(f64::from(threshold))),
            Err(error) => {
                tracing::warn!(error = %error, "the charter's handoff threshold could not be read; the default applies");
                self.scorer
            }
        }
    }

    /// The contact records.
    pub fn records(&self) -> &Arc<dyn LeadRecords> {
        &self.records
    }

    /// The scopes the receipts `ids` hold live now.
    async fn live_scopes(&self, ids: &[Uuid]) -> Result<BTreeSet<String>> {
        let now = self.clock.now();
        let mut scopes = BTreeSet::new();
        for id in ids {
            let record = self.receipts.get(*id).await.map_err(store_error)?;
            if let Some(record) = record
                && record.revoked_at.is_none_or(|revoked| now < revoked)
                && record.receipt.expires_at.is_none_or(|expires| now < expires)
            {
                scopes.insert(record.receipt.scope.as_str().to_string());
            }
        }
        Ok(scopes)
    }

    /// Scores lead `id` now and keeps the score on the lead (spec 3.5 §7.20).
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown lead; the stores' errors.
    pub async fn rescore(&self, id: &LeadRef) -> Result<LeadScore> {
        let lead = self.leads.get(id).await.map_err(store_error)?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.lead.not_found",
                format!("no lead {}", id.0),
            )
        })?;
        let receipts: Vec<Uuid> = lead.consent.iter().map(|r| r.0).collect();
        let scopes = self.live_scopes(&receipts).await?;
        let nurtured = self.records.nurtured(&lead.contact_ref).await?;
        let score = self.current_scorer().await.score(&lead, &scopes, nurtured);
        self.leads.set_score(id, score.value).await.map_err(store_error)?;
        Ok(score)
    }

    /// The leads of the person at `email`.
    async fn leads_of(&self, email: &str) -> Result<Vec<Lead>> {
        let contacts: BTreeSet<String> =
            self.records.by_email(email).await?.into_iter().map(|c| c.contact_ref.0).collect();
        if contacts.is_empty() {
            return Ok(Vec::new());
        }
        let all = self.leads.since(DateTime::UNIX_EPOCH).await.map_err(store_error)?;
        Ok(all.into_iter().filter(|lead| contacts.contains(&lead.contact_ref.0)).collect())
    }

    /// Counts one nurture step sent to `email` and rescores each of their leads.
    ///
    /// # Errors
    ///
    /// The stores' errors.
    pub async fn nurtured(&self, email: &str) -> Result<Vec<LeadScore>> {
        let leads = self.leads_of(email).await?;
        let contacts: BTreeSet<&ContactRef> = leads.iter().map(|lead| &lead.contact_ref).collect();
        for contact in contacts {
            self.records.note_nurture(contact).await?;
        }
        let mut scores = Vec::new();
        for lead in &leads {
            scores.push(self.rescore(&lead.id).await?);
        }
        Ok(scores)
    }

    /// Hands lead `id` to the owner once, when its score has reached the threshold (spec 3.5
    /// §7.20 "Warm handoff with context"): returns its score.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a lead below the threshold or one handed off already; the stores'
    /// errors.
    pub async fn hand_off(&self, id: &LeadRef) -> Result<LeadScore> {
        let score = self.rescore(id).await?;
        let refuse = |code: &'static str, message: String| {
            AdkError::new(ErrorComponent::Tool, ErrorCategory::InvalidInput, code, message)
        };
        if !score.ready {
            return Err(refuse(
                "website.lead.not_ready",
                format!(
                    "lead {} scores {:.0} of the {:.0} the site hands off at ({}); keep \
                     nurturing it within its consent",
                    id.0,
                    score.value,
                    score.threshold,
                    score.reasons.join("; ")
                ),
            ));
        }
        if !self.records.hand_off(id, self.clock.now()).await? {
            return Err(refuse(
                "website.lead.handed_off",
                format!("lead {} was handed to the owner already; one card per lead", id.0),
            ));
        }
        Ok(score)
    }
}

#[async_trait]
impl adk_drivers::ConsentCheck for Leads {
    /// Whether `recipient` holds a live receipt for `scope`, from the receipts their enquiries
    /// carried to the edge (spec 3.5 A-I8).
    async fn is_live(&self, recipient: &str, scope: &str) -> adk_drivers::Result<bool> {
        let unavailable = |e: AdkError| adk_drivers::DriverError::unavailable("consent", e);
        let receipts: Vec<Uuid> = self
            .records
            .by_email(recipient)
            .await
            .map_err(unavailable)?
            .into_iter()
            .flat_map(|contact| contact.consent)
            .collect();
        let scopes = self.live_scopes(&receipts).await.map_err(unavailable)?;
        Ok(scopes.contains(scope))
    }
}

/// The consent scopes the kind asks visitors for (spec 3.5 §5 S8, §7.5, A-I8): those its
/// capabilities name, and [`NURTURE_SCOPE`] when the kind runs the nurture job.
pub fn consent_scopes(template: &crate::kinds::KindTemplate) -> Vec<ConsentScope> {
    let mut scopes: Vec<ConsentScope> = template
        .capabilities()
        .into_iter()
        .filter_map(|capability| capability.consent.map(|consent| consent.scope))
        .collect();
    if template.jobs().iter().any(|job| job.id == "nurture") {
        scopes.push(ConsentScope::from(NURTURE_SCOPE));
    }
    scopes.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    scopes.dedup();
    scopes
}

/// The email outbox's sends wrapped for the nurture job (spec 3.5 §5 S8, §7.21): a sequence
/// step or newsletter is refused inside the owner's `quiet` hours, saying when they end, and
/// each sequence step sent counts towards its recipient's leads, whose new scores come back
/// with the send. Every other tool is returned unchanged. The wrapped tools keep their
/// names, origins and declarations, so an owner's standing rules still pin them.
pub fn nurture_tools(
    tools: Vec<Arc<dyn Tool>>,
    leads: Arc<Leads>,
    quiet: Option<QuietHours>,
    clock: Arc<dyn Clock>,
) -> Vec<Arc<dyn Tool>> {
    tools
        .into_iter()
        .map(|tool| match tool.name() {
            "email_send_sequence_step" | "email_send_newsletter" => Arc::new(NurtureSend {
                inner: tool,
                leads: leads.clone(),
                quiet,
                clock: clock.clone(),
            }) as Arc<dyn Tool>,
            _ => tool,
        })
        .collect()
}

/// A nurture send inside quiet hours and lead scoring.
struct NurtureSend {
    inner: Arc<dyn Tool>,
    leads: Arc<Leads>,
    quiet: Option<QuietHours>,
    clock: Arc<dyn Clock>,
}

#[async_trait]
impl Tool for NurtureSend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> Value {
        self.inner.declaration()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }

    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }

    fn origin(&self) -> &str {
        self.inner.origin()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn effect(&self) -> adk_core::ToolEffect {
        self.inner.effect()
    }

    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    async fn execute(&self, ctx: Arc<dyn adk_core::ToolContext>, args: Value) -> Result<Value> {
        let now = self.clock.now();
        if let Some(quiet) = &self.quiet
            && quiet.contains(now)
        {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unavailable,
                "website.nurture.quiet_hours",
                format!(
                    "{} is not sent inside the owner's quiet hours; send it after {} UTC",
                    self.inner.name(),
                    quiet.release_at(now).format("%Y-%m-%d %H:%M")
                ),
            ));
        }
        let recipient = args.get("to").and_then(Value::as_str).map(str::to_string);
        let mut out = self.inner.execute(ctx, args).await?;
        if let Some(recipient) = recipient {
            match self.leads.nurtured(&recipient).await {
                Ok(scores) if !scores.is_empty() => {
                    let leads: BTreeMap<String, Value> = scores
                        .iter()
                        .map(|score| {
                            (
                                score.lead.0.clone(),
                                json!({
                                    "score": score.value,
                                    "threshold": score.threshold,
                                    "readyForHandoff": score.ready,
                                }),
                            )
                        })
                        .collect();
                    if let Some(object) = out.as_object_mut() {
                        object.insert("leads".into(), json!(leads));
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(error = %error, "the nurture send was not counted towards its leads");
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awp_types::{Bounded, Channel, ConsentReceiptRef};

    fn lead(message: &str, receipts: usize) -> Lead {
        Lead {
            id: LeadRef("lead-1".into()),
            captured_at: Utc::now(),
            source: Channel::Http,
            contact_ref: ContactRef("contact:1".into()),
            consent: (0..receipts).map(|_| ConsentReceiptRef(Uuid::new_v4())).collect(),
            answers: [
                ("capability".to_string(), Bounded::truncated("submit_inquiry", 200)),
                ("message".to_string(), Bounded::truncated(message, 2000)),
            ]
            .into_iter()
            .collect(),
            thread_ref: None,
            score: None,
        }
    }

    #[test]
    fn a_lead_scores_its_consent_answers_and_nurture() {
        let scorer = LeadScorer::default();
        let wedding = lead("A wedding cake. Date: 14 February 2027. Guests: 120.", 2);
        let both: BTreeSet<String> = ["contact".to_string(), NURTURE_SCOPE.to_string()].into();
        let fresh = scorer.score(&wedding, &both, 0);
        assert_eq!((fresh.value, fresh.ready), (60.0, false), "{:?}", fresh.reasons);
        let warmed = scorer.score(&wedding, &both, 2);
        assert_eq!((warmed.value, warmed.ready), (70.0, true), "{:?}", warmed.reasons);
        let contact_only: BTreeSet<String> = ["contact".to_string()].into();
        let vague = scorer.score(&lead("Do you bake cakes?", 1), &contact_only, 9);
        assert_eq!(vague.value, 35.0, "nurture counts three steps at most");
        assert!(!vague.ready);
    }
}
