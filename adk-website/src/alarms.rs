//! Alarms by urgency (spec 3.5 §7.21, §7.20 "Alarms by urgency and channel").
//!
//! | Urgency | Examples | Delivery |
//! |---|---|---|
//! | P1 | Incident, site down, payment anomaly, key compromise suspected | Immediately on the owner's P1 channels, including inside quiet hours; repeated until acknowledged |
//! | P2 | A held decision blocking a customer, a guardrail breach, a failed renewal | On the owner's chosen channel, outside quiet hours |
//! | P3 | Everything else | The weekly review |
//!
//! [`AlarmRouter`] delivers through the kernel's `ChannelSink`s to the owner's paired
//! threads, reads the charter's reporting routes and quiet hours, and is the product's
//! `CharterEventSink` (guardrail breaches, pauses, autonomy changes) and `WalletAlarmHook`
//! (payment anomalies, reconciliation mismatches).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};
use adk_kernel::QuietHours;
use adk_kernel::channels::ChannelSink;
use adk_kernel::charter::{CharterEvent, CharterEventSink};
use adk_payments::wallet::{WalletAlarm, WalletAlarmHook};
use async_trait::async_trait;
use awp_types::{Reporting, Urgency};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::audit;

/// How often an unacknowledged P1 alarm is sent again (spec 3.5 §7.21).
pub const P1_REPEAT: Duration = Duration::minutes(15);

/// An alarm to the owner (spec 3.5 §7.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alarm {
    /// The alarm's id, given when it is raised.
    #[serde(default)]
    pub id: String,
    /// The site.
    pub site: Uuid,
    /// How soon the owner hears of it.
    pub urgency: Urgency,
    /// What it is, such as `incident`, `payment_anomaly` or `guardrail_breach`.
    pub kind: String,
    /// One line for the owner: what happened, what was done, what the owner must decide.
    pub summary: String,
    /// When it was raised.
    pub raised_at: DateTime<Utc>,
}

impl Alarm {
    /// An alarm of `kind` at `urgency` for `site`.
    pub fn new(
        site: Uuid,
        urgency: Urgency,
        kind: impl Into<String>,
        summary: impl Into<String>,
        raised_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: String::new(),
            site,
            urgency,
            kind: kind.into(),
            summary: summary.into(),
            raised_at,
        }
    }

    /// The alarm as the owner reads it on a channel.
    pub fn text(&self) -> String {
        let label = match self.urgency {
            Urgency::P1 => "Urgent",
            Urgency::P2 => "Needs you",
            _ => "For the review",
        };
        format!("{label}: {} (alarm {})", self.summary, self.id)
    }
}

/// One message an alarm sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delivery {
    /// The alarm.
    pub alarm: String,
    /// The channel.
    pub channel: String,
    /// The owner's thread on it.
    pub thread: String,
    /// The attempt: 1 for the first send of a P1 alarm, more for its repeats.
    pub attempt: u32,
    /// When.
    pub at: DateTime<Utc>,
}

/// What [`AlarmRouter::raise`] did with an alarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "routed", rename_all = "snake_case")]
pub enum Routed {
    /// Sent now.
    Sent {
        /// The messages.
        deliveries: Vec<Delivery>,
    },
    /// A P2 alarm inside quiet hours: sent when they end.
    Deferred,
    /// Held for the weekly review.
    Review,
}

#[derive(Debug, Clone)]
struct OpenP1 {
    alarm: Alarm,
    attempts: u32,
    last: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct RouterState {
    open: BTreeMap<String, OpenP1>,
    deferred: Vec<Alarm>,
    review: Vec<Alarm>,
    raised: Vec<Alarm>,
}

/// Routes alarms to the owner by urgency (spec 3.5 §7.21).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_website::alarms::{Alarm, AlarmRouter, Routed};
/// use awp_types::{Reporting, Urgency, Cadence};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let reporting = Reporting { review: Cadence::Weekly, routes: vec![], quiet_hours: None };
/// let router = AlarmRouter::new(uuid::Uuid::nil(), &reporting);
/// let routed = router.raise(Alarm::new(uuid::Uuid::nil(), Urgency::P3, "freshness", "Two pages are stale", chrono::Utc::now())).await;
/// assert_eq!(routed, Routed::Review);
/// assert_eq!(router.review_items().await.len(), 1);
/// # }
/// ```
pub struct AlarmRouter {
    site: Uuid,
    routes: BTreeMap<Urgency, Vec<String>>,
    quiet: Option<QuietHours>,
    sinks: BTreeMap<String, (Arc<dyn ChannelSink>, String)>,
    clock: Arc<dyn Clock>,
    repeat: Duration,
    audit: Option<AuditTrail>,
    state: Mutex<RouterState>,
    next: AtomicU64,
}

impl std::fmt::Debug for AlarmRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlarmRouter")
            .field("site", &self.site)
            .field("routes", &self.routes)
            .field("channels", &self.sinks.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The kernel's quiet hours for the charter's (spec 3.5 §7.21): in their IANA time zone,
/// or UTC when the zone is unknown.
pub fn quiet_hours(reporting: &Reporting) -> Option<QuietHours> {
    let quiet = reporting.quiet_hours.as_ref()?;
    let hours = QuietHours::new(quiet.start, quiet.end);
    Some(match quiet.timezone.parse::<chrono_tz::Tz>() {
        Ok(zone) => hours.with_time_zone(zone),
        Err(_) => {
            tracing::warn!(timezone = %quiet.timezone, "unknown quiet-hours time zone; reading the hours in UTC");
            hours.with_utc_offset_minutes(0)
        }
    })
}

impl AlarmRouter {
    /// A router for `site` over the charter's `reporting`: its routes per urgency and its
    /// quiet hours. Add the owner's threads with [`with_sink`](Self::with_sink).
    pub fn new(site: Uuid, reporting: &Reporting) -> Self {
        let routes = reporting
            .routes
            .iter()
            .map(|route| {
                (route.urgency, route.channels.iter().map(|c| c.as_str().to_string()).collect())
            })
            .collect();
        Self {
            site,
            routes,
            quiet: quiet_hours(reporting),
            sinks: BTreeMap::new(),
            clock: Arc::new(SystemClock),
            repeat: P1_REPEAT,
            audit: None,
            state: Mutex::new(RouterState::default()),
            next: AtomicU64::new(1),
        }
    }

    /// Delivers on `sink`'s channel to the owner's `thread` there.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn ChannelSink>, thread: impl Into<String>) -> Self {
        self.sinks.insert(sink.channel().to_string(), (sink, thread.into()));
        self
    }

    /// The clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// How often an unacknowledged P1 alarm repeats.
    #[must_use]
    pub fn with_repeat(mut self, every: Duration) -> Self {
        self.repeat = every;
        self
    }

    /// Where `website.alarm.*` records go.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    fn in_quiet_hours(&self, at: DateTime<Utc>) -> bool {
        self.quiet.as_ref().is_some_and(|quiet| quiet.contains(at))
    }

    /// The channels of `urgency`: its route's channels the router has a thread on; for P1
    /// with no route, every thread, since an incident must reach the owner.
    fn channels(&self, urgency: Urgency) -> Vec<String> {
        let routed: Vec<String> = self
            .routes
            .get(&urgency)
            .map(|channels| {
                channels.iter().filter(|c| self.sinks.contains_key(*c)).cloned().collect()
            })
            .unwrap_or_default();
        if routed.is_empty() && urgency == Urgency::P1 {
            return self.sinks.keys().cloned().collect();
        }
        routed
    }

    async fn send(&self, alarm: &Alarm, channels: &[String], attempt: u32) -> Vec<Delivery> {
        let mut deliveries = Vec::new();
        let at = self.clock.now();
        for channel in channels {
            let Some((sink, thread)) = self.sinks.get(channel) else { continue };
            match sink.send_message(thread, &alarm.text()).await {
                Ok(()) => deliveries.push(Delivery {
                    alarm: alarm.id.clone(),
                    channel: channel.clone(),
                    thread: thread.clone(),
                    attempt,
                    at,
                }),
                Err(error) => {
                    tracing::warn!(website.alarm = %alarm.id, channel, error = %error, "alarm delivery failed")
                }
            }
        }
        deliveries
    }

    /// Raises `alarm` (spec 3.5 §7.21): P1 at once on its channels, quiet hours or not, and
    /// open until acknowledged; P2 on its channel outside quiet hours, deferred inside them,
    /// and to the weekly review when the charter routes it nowhere; P3 to the weekly review.
    pub async fn raise(&self, mut alarm: Alarm) -> Routed {
        if alarm.id.is_empty() {
            alarm.id = format!("alarm-{}", self.next.fetch_add(1, Ordering::Relaxed));
        }
        let now = self.clock.now();
        let routed = match alarm.urgency {
            Urgency::P1 => {
                let deliveries = self.send(&alarm, &self.channels(Urgency::P1), 1).await;
                self.state.lock().await.open.insert(
                    alarm.id.clone(),
                    OpenP1 { alarm: alarm.clone(), attempts: 1, last: now },
                );
                Routed::Sent { deliveries }
            }
            Urgency::P2 => {
                let channels = self.channels(Urgency::P2);
                if channels.is_empty() {
                    self.state.lock().await.review.push(alarm.clone());
                    Routed::Review
                } else if self.in_quiet_hours(now) {
                    self.state.lock().await.deferred.push(alarm.clone());
                    Routed::Deferred
                } else {
                    Routed::Sent { deliveries: self.send(&alarm, &channels, 1).await }
                }
            }
            _ => {
                self.state.lock().await.review.push(alarm.clone());
                Routed::Review
            }
        };
        self.state.lock().await.raised.push(alarm.clone());
        tracing::info!(website.alarm = %alarm.id, website.alarm.kind = %alarm.kind, urgency = ?alarm.urgency, "website.alarm.raised");
        audit::record(
            self.audit.as_ref(),
            audit::ALARM_RAISED,
            AuditActor::default(),
            json!({
                "site": self.site,
                "alarm": alarm.id,
                "kind": alarm.kind,
                "urgency": alarm.urgency,
                "routed": routed,
            }),
        )
        .await;
        routed
    }

    /// Records that a person acknowledged the P1 alarm `id`: it stops repeating.
    ///
    /// # Errors
    ///
    /// `Forbidden` when `by` is not a person; `NotFound` for an alarm that is not open.
    pub async fn acknowledge(&self, id: &str, by: &SubjectRef) -> Result<()> {
        if by.kind != SubjectKind::Human || adk_kernel::charter::is_agent_principal(&by.id) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Forbidden,
                "website.alarm.agent_ack",
                format!("{} is not a person; only a person acknowledges an alarm", by.id),
            ));
        }
        if self.state.lock().await.open.remove(id).is_none() {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::NotFound,
                "website.alarm.not_open",
                format!("alarm {id} is not open"),
            ));
        }
        audit::record(
            self.audit.as_ref(),
            audit::ALARM_ACKNOWLEDGED,
            AuditActor::subject(by.clone()),
            json!({ "site": self.site, "alarm": id }),
        )
        .await;
        Ok(())
    }

    /// Sends what is due (spec 3.5 §7.21): every open P1 alarm whose last send is older than
    /// the repeat interval, and the deferred P2 alarms once quiet hours end. Run it from the
    /// alarms job's schedule.
    pub async fn tick(&self) -> Vec<Delivery> {
        let now = self.clock.now();
        let due: Vec<(Alarm, u32)> = {
            let mut state = self.state.lock().await;
            state
                .open
                .values_mut()
                .filter(|open| now - open.last >= self.repeat)
                .map(|open| {
                    open.attempts += 1;
                    open.last = now;
                    (open.alarm.clone(), open.attempts)
                })
                .collect()
        };
        let mut deliveries = Vec::new();
        for (alarm, attempt) in due {
            deliveries.extend(self.send(&alarm, &self.channels(Urgency::P1), attempt).await);
        }
        if !self.in_quiet_hours(now) {
            let deferred = std::mem::take(&mut self.state.lock().await.deferred);
            let channels = self.channels(Urgency::P2);
            for alarm in deferred {
                deliveries.extend(self.send(&alarm, &channels, 1).await);
            }
        }
        deliveries
    }

    /// The P1 alarms still open.
    pub async fn open(&self) -> Vec<Alarm> {
        self.state.lock().await.open.values().map(|open| open.alarm.clone()).collect()
    }

    /// The alarms waiting for the weekly review.
    pub async fn review_items(&self) -> Vec<Alarm> {
        self.state.lock().await.review.clone()
    }

    /// Takes the alarms waiting for the weekly review, which the review then carries.
    pub async fn take_review_items(&self) -> Vec<Alarm> {
        std::mem::take(&mut self.state.lock().await.review)
    }

    /// Clears the alarms `ids` from those waiting for the weekly review: a review that
    /// carried them reached the owner (spec 3.5 §7.21).
    pub async fn clear_review_items(&self, ids: &[String]) {
        self.state.lock().await.review.retain(|alarm| !ids.contains(&alarm.id));
    }

    /// Every alarm raised since `since`, for the weekly review's incidents.
    pub async fn raised_since(&self, since: DateTime<Utc>) -> Vec<Alarm> {
        self.state.lock().await.raised.iter().filter(|a| a.raised_at >= since).cloned().collect()
    }
}

#[async_trait]
impl CharterEventSink for AlarmRouter {
    async fn emit(&self, event: &CharterEvent) {
        let at = self.clock.now();
        let (site, kind, summary) = match event {
            CharterEvent::GuardrailBreached {
                site,
                guardrail,
                metric,
                value,
                threshold,
                jobs,
                ..
            } => (
                *site,
                "guardrail_breach",
                format!(
                    "guardrail {guardrail} breached: {metric} at {value:.3} against {threshold}; \
                     paused {}",
                    jobs.join(", ")
                ),
            ),
            CharterEvent::JobPaused { site, job, reason, .. } => {
                (*site, "job_paused", format!("job {job} paused: {reason}"))
            }
            CharterEvent::AutonomyChanged { site, job, from, to, reason, .. } => (
                *site,
                "autonomy_changed",
                format!("job {job} moved from {from:?} to {to:?}: {reason}"),
            ),
            CharterEvent::PromotionOffered { site, job, to, streak, .. } => (
                *site,
                "promotion_offered",
                format!("job {job} earned {to:?} after {streak} unchanged approvals"),
            ),
            CharterEvent::BaselineLocked { site, kpi, value, .. } => (
                *site,
                "baseline_locked",
                format!("KPI {kpi} has a baseline of {value:.2}; you may set a target"),
            ),
            CharterEvent::Committed { site, version, .. } => {
                (*site, "charter_committed", format!("charter version {version} took effect"))
            }
            other => (self.site, "charter_event", format!("{other:?}")),
        };
        self.raise(Alarm::new(site, event.urgency(), kind, summary, at)).await;
    }
}

#[async_trait]
impl WalletAlarmHook for AlarmRouter {
    async fn raise(&self, alarm: &WalletAlarm) -> Result<()> {
        let kind = match alarm.urgency() {
            Urgency::P1 => "payment_anomaly",
            _ => "reconciliation_mismatch",
        };
        AlarmRouter::raise(
            self,
            Alarm::new(self.site, alarm.urgency(), kind, alarm.summary(), alarm.raised_at),
        )
        .await;
        Ok(())
    }
}

/// The most characters of a handoff card's summary.
const HANDOFF_SUMMARY_MAX: usize = 600;

/// `lead_handoff` over `router` (spec 3.5 §7.20 "Warm handoff with context", §7.21): the
/// Revenue agent hands a qualified lead to the owner as one P2 card with the conversation's
/// summary, on the owner's chosen channel outside quiet hours. It implements `adk-harness`'
/// catalogue entry, so the ladder decides it as the `handoff` job's.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_tool::SimpleToolContext;
/// use adk_website::alarms::{AlarmRouter, handoff_tool};
/// use awp_types::{Cadence, Reporting};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let reporting = Reporting { review: Cadence::Weekly, routes: vec![], quiet_hours: None };
/// let router = Arc::new(AlarmRouter::new(uuid::Uuid::nil(), &reporting));
/// let tool = handoff_tool(router.clone());
/// let ctx = Arc::new(SimpleToolContext::new("revenue"));
/// let card = tool.execute(ctx, serde_json::json!({ "leadRef": "lead-1", "summary": "A wedding cake for 120 guests on 14 February" })).await.unwrap();
/// assert_eq!(card["handedOff"], true);
/// assert_eq!(router.raised_since(chrono::DateTime::UNIX_EPOCH).await[0].kind, "lead_handoff");
/// # }
/// ```
pub fn handoff_tool(router: Arc<AlarmRouter>) -> Arc<dyn adk_core::Tool> {
    Arc::new(HandoffTool { router, leads: None })
}

/// `lead_handoff` over `router` that hands a lead off only once its score has reached the
/// kind's threshold, and only once (spec 3.5 §5 S8, §7.20): the card carries the score the
/// site computed, not one the agent states.
pub fn handoff_tool_for(
    router: Arc<AlarmRouter>,
    leads: Arc<crate::leads::Leads>,
) -> Arc<dyn adk_core::Tool> {
    Arc::new(HandoffTool { router, leads: Some(leads) })
}

/// `lead_handoff`: one card to the owner per qualified lead.
struct HandoffTool {
    router: Arc<AlarmRouter>,
    leads: Option<Arc<crate::leads::Leads>>,
}

#[async_trait]
impl adk_core::Tool for HandoffTool {
    fn name(&self) -> &str {
        adk_harness::website::LEAD_HANDOFF
    }

    fn description(&self) -> &str {
        "Hands a qualified lead to the owner: one card on their chosen channel with the lead's \
         reference and a short summary of the conversation in your own words."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "leadRef": { "type": "string" },
                "summary": { "type": "string", "maxLength": HANDOFF_SUMMARY_MAX },
                "score": { "type": "number" }
            },
            "required": ["leadRef", "summary"],
            "additionalProperties": false
        }))
    }

    fn origin(&self) -> &str {
        adk_harness::website::BACK_OFFICE_ORIGIN
    }

    fn effect(&self) -> adk_core::ToolEffect {
        adk_core::ToolEffect::NonIdempotent
    }

    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let invalid = |message: &str| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "website.handoff.invalid",
                format!("invalid arguments for lead_handoff: {message}"),
            )
        };
        let lead = args["leadRef"].as_str().filter(|r| !r.trim().is_empty());
        let lead = lead.ok_or_else(|| invalid("a leadRef is required"))?;
        let summary: String = args["summary"]
            .as_str()
            .ok_or_else(|| invalid("a summary is required"))?
            .chars()
            .take(HANDOFF_SUMMARY_MAX)
            .collect();
        let score = match &self.leads {
            Some(leads) => {
                let scored = leads.hand_off(&awp_types::LeadRef(lead.to_string())).await?;
                format!(" (score {:.0} of {:.0})", scored.value, scored.threshold)
            }
            None => args["score"]
                .as_f64()
                .map(|score| format!(" (score {score:.0})"))
                .unwrap_or_default(),
        };
        let routed = self
            .router
            .raise(Alarm::new(
                self.router.site,
                Urgency::P2,
                "lead_handoff",
                format!("A warm lead is ready for you{score}: {summary} [lead {lead}]"),
                self.router.clock.now(),
            ))
            .await;
        Ok(json!({ "handedOff": true, "lead": lead, "routed": routed }))
    }
}
