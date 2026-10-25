//! The incident path (spec 3.5 §7.20 "Run and protect", §7.21 P1, §7.11, A-I4, A-I11, S5).
//!
//! | Step | What happens |
//! |---|---|
//! | Probe | The Guardian's probe ([`GuardianProbe`]) re-proves the domain, reads the host and the origin against the signed releases, and records `site.drift.detected` |
//! | Incident | Drift opens one incident: the site's status becomes `incident` (`site.status.changed`) |
//! | Read-only | The edge answers every non-read capability with 503 — the edge's status control, added with the edge |
//! | Rollback | To the last good signed release: the Guardian runs `site_rollback` when its `protect` job's level lets the call run (a standing rule the owner created, A-I11); otherwise the rollback is a proposal the owner approves |
//! | Alarm | One P1 alarm, repeated until a person acknowledges it |
//! | Exit | Only a person's decision closes the incident and restores `operational` |
//!
//! An incident a job caused in its own area demotes that area's jobs one level (§7.11). Drift
//! is a change no release explains — tampering from outside — so it demotes no job: the
//! Guardian's reversal is the response, not the cause.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_auth::signed_audit::NoteVerifier;
use adk_core::audit_log::{AuditActor, AuditRecorder, AuditTrail};
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    ActingContext, AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock, ToolEffect,
};
use adk_harness::website::{LevelDecision, SITE_ROLLBACK, SiteRef, SiteRuntime, level_decision};
use adk_kernel::charter::AutonomyLadder;
use adk_site::host::SiteHost;
use adk_site::migrate::claim::{DnsResolver, DomainClaim};
use adk_site::migrate::http::HttpClient;
use adk_site::migrate::probe::{Drift, DriftLocation, ProbeConfig, ProbeReport, Prober};
use async_trait::async_trait;
use awp_types::{AutonomyLevel, SiteState, Urgency};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::alarms::{Alarm, AlarmRouter};
use crate::audit;

/// The job whose level decides whether the Guardian rolls back alone (spec 3.5 §7.20).
pub const PROTECT_JOB: &str = "protect";

/// The site's status as the product last set it (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusRecord {
    /// The state.
    pub state: SiteState,
    /// Since when.
    pub since: DateTime<Utc>,
    /// Why, as a snake_case code.
    pub reason: String,
    /// The release being served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_seq: Option<u64>,
}

/// Where the site's status goes: the edge's signed status document serves it (spec 3.5
/// §7.5, §7.9).
#[async_trait]
pub trait StatusSink: Send + Sync {
    /// Sets the site's status.
    ///
    /// # Errors
    ///
    /// Returns the sink's error.
    async fn set(&self, site: Uuid, status: StatusRecord) -> Result<()>;

    /// The site's status.
    ///
    /// # Errors
    ///
    /// Returns the sink's error.
    async fn get(&self, site: Uuid) -> Result<Option<StatusRecord>>;
}

/// A [`StatusSink`] in memory, until the edge's status store serves it.
#[derive(Debug, Default)]
pub struct InMemoryStatusBoard {
    statuses: Mutex<BTreeMap<Uuid, StatusRecord>>,
}

impl InMemoryStatusBoard {
    /// No statuses yet.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StatusSink for InMemoryStatusBoard {
    async fn set(&self, site: Uuid, status: StatusRecord) -> Result<()> {
        self.statuses.lock().await.insert(site, status);
        Ok(())
    }

    async fn get(&self, site: Uuid) -> Result<Option<StatusRecord>> {
        Ok(self.statuses.lock().await.get(&site).cloned())
    }
}

/// Who may roll back (spec 3.5 A-I11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackAuthority {
    /// The `protect` job's level lets `site_rollback` run: the Guardian rolls back.
    Standing,
    /// The owner approves the rollback.
    Proposal,
}

/// The rollback an incident plans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackPlan {
    /// The last good signed release.
    pub to_release: u64,
    /// Who rolls back.
    pub authority: RollbackAuthority,
    /// The release served after the rollback, once it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
}

/// How an incident ended: by a person's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentExit {
    /// Who decided.
    pub by: String,
    /// The decision's id.
    pub decision: String,
    /// When.
    pub at: DateTime<Utc>,
}

/// An incident (spec 3.5 §7.20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    /// Its id.
    pub id: String,
    /// The site.
    pub site: Uuid,
    /// When it opened.
    pub opened_at: DateTime<Utc>,
    /// The drift that opened it.
    pub drift: Vec<Drift>,
    /// The rollback it plans.
    pub rollback: RollbackPlan,
    /// The area whose jobs it demoted, when a job's work caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// The jobs it demoted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub demoted: Vec<String>,
    /// The P1 alarm.
    pub alarm: String,
    /// How it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<IncidentExit>,
}

impl Incident {
    /// The Guardian's turn for the incident: the plan in plain words, run under the
    /// `incident` playbook.
    pub fn guardian_turn(&self) -> String {
        let paths: Vec<&str> = self.drift.iter().map(|d| d.path.as_str()).collect();
        let act = match self.rollback.authority {
            RollbackAuthority::Standing => format!(
                "Roll the site back to signed release {} with site_rollback now.",
                self.rollback.to_release
            ),
            RollbackAuthority::Proposal => format!(
                "Your authority does not let you roll back alone: call site_rollback to \
                 release {} so the owner decides it, and say so in one sentence.",
                self.rollback.to_release
            ),
        };
        format!(
            "Incident {}: {} served file(s) match no signed release ({}). {act} Then report \
             what you did. The incident stays open until a person closes it.",
            self.id,
            paths.len(),
            paths.join(", ")
        )
    }
}

/// Opens, tracks and closes incidents (spec 3.5 §7.20, A-I11).
pub struct IncidentPath {
    site: SiteRef,
    runtime: Arc<SiteRuntime>,
    ladder: AutonomyLadder,
    alarms: Arc<AlarmRouter>,
    status: Arc<dyn StatusSink>,
    clock: Arc<dyn Clock>,
    audit: Option<AuditTrail>,
    incidents: Mutex<BTreeMap<String, Incident>>,
    tainted: Mutex<std::collections::BTreeSet<u64>>,
    next: AtomicU64,
}

impl std::fmt::Debug for IncidentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncidentPath").field("site", &self.site).finish_non_exhaustive()
    }
}

fn not_a_person(by: &SubjectRef) -> bool {
    by.kind != SubjectKind::Human || adk_kernel::charter::is_agent_principal(&by.id)
}

impl IncidentPath {
    /// The incident path of `runtime`'s site over the kernel's `ladder`, raising alarms on
    /// `alarms` and setting the status on `status`.
    pub fn new(
        runtime: Arc<SiteRuntime>,
        ladder: AutonomyLadder,
        alarms: Arc<AlarmRouter>,
        status: Arc<dyn StatusSink>,
    ) -> Self {
        Self {
            site: runtime.site().clone(),
            runtime,
            ladder,
            alarms,
            status,
            clock: Arc::new(SystemClock),
            audit: None,
            incidents: Mutex::new(BTreeMap::new()),
            tainted: Mutex::new(std::collections::BTreeSet::new()),
            next: AtomicU64::new(1),
        }
    }

    /// The clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Where `site.status.changed` and `site.rollback` records go.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Who may roll back now: the Guardian when its `protect` job's level lets
    /// `site_rollback` run, otherwise the owner (spec 3.5 §7.11, A-I11).
    ///
    /// # Errors
    ///
    /// Returns the autonomy store's error.
    pub async fn rollback_authority(&self) -> Result<RollbackAuthority> {
        let level = self
            .ladder
            .level(self.site.id, PROTECT_JOB)
            .await
            .map_err(|e| AdkError::config(e.to_string()))?
            .unwrap_or(AutonomyLevel::Propose);
        let spec = adk_harness::website::spec(SITE_ROLLBACK);
        Ok(match level_decision(level, spec, ToolEffect::NonIdempotent) {
            LevelDecision::Allow => RollbackAuthority::Standing,
            _ => RollbackAuthority::Proposal,
        })
    }

    async fn set_status(&self, state: SiteState, reason: &str, release: Option<u64>) -> Result<()> {
        let record = StatusRecord {
            state,
            since: self.clock.now(),
            reason: reason.to_string(),
            release_seq: release,
        };
        self.status.set(self.site.id, record).await?;
        tracing::info!(site.state = ?state, reason, "site.status.changed");
        audit::record(
            self.audit.as_ref(),
            audit::STATUS_CHANGED,
            AuditActor::default(),
            json!({ "site": self.site.id, "state": state, "reason": reason, "releaseSeq": release }),
        )
        .await;
        Ok(())
    }

    /// Acts on a probe (spec 3.5 §7.20, S5): no drift, nothing; drift with an incident open,
    /// the drift joins it; otherwise a new incident — status `incident`, a rollback plan to
    /// the last good signed release, one P1 alarm. Drift at the origin leaves the host's copy
    /// of the current release intact, so it is the last good one; drift on the host changed
    /// that copy, which cannot restore itself, so the last good release is the latest one
    /// before it whose copy no probe found changed.
    ///
    /// # Errors
    ///
    /// Returns the status sink's or the autonomy store's error.
    pub async fn on_probe(&self, report: &ProbeReport) -> Result<Option<Incident>> {
        if !report.drift_detected() {
            return Ok(None);
        }
        if let Some(open) =
            self.incidents.lock().await.values_mut().find(|incident| incident.closed.is_none())
        {
            open.drift.extend(report.drift.iter().cloned());
            return Ok(Some(open.clone()));
        }
        let previous = self.runtime.pipeline().previous().await.map(|p| p.manifest.seq);
        let Some(current) = report.checkpoint.or(previous) else {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.incident.no_release",
                "drift was found but no signed release is known to roll back to",
            ));
        };
        let on_host = report.drift.iter().any(|drift| drift.location == DriftLocation::Host);
        let to_release = {
            let mut tainted = self.tainted.lock().await;
            if on_host {
                tainted.insert(current);
            }
            (1..=current).rev().find(|seq| !tainted.contains(seq)).unwrap_or(current)
        };
        self.open(report.drift.clone(), to_release, None).await.map(Some)
    }

    /// Opens an incident for `drift` with a rollback to `to_release`; an incident a job
    /// caused names its `area`, whose jobs it demotes one level (spec 3.5 §7.11).
    ///
    /// # Errors
    ///
    /// Returns the status sink's or the autonomy store's error.
    #[tracing::instrument(skip_all, name = "website.job.protect", fields(website.incident = tracing::field::Empty, site.id = %self.site.id))]
    pub async fn open(
        &self,
        drift: Vec<Drift>,
        to_release: u64,
        area: Option<&str>,
    ) -> Result<Incident> {
        let id = format!("incident-{}", self.next.fetch_add(1, Ordering::Relaxed));
        tracing::Span::current().record("website.incident", id.as_str());
        self.set_status(SiteState::Incident, "drift_detected", Some(to_release)).await?;
        let demoted = match area {
            Some(area) => self
                .ladder
                .incident(self.site.id, area, &id)
                .await
                .map_err(|e| AdkError::config(e.to_string()))?
                .into_iter()
                .map(|job| job.job)
                .collect(),
            None => Vec::new(),
        };
        let authority = self.rollback_authority().await?;
        let act = match authority {
            RollbackAuthority::Standing => format!("rolling back to signed release {to_release}"),
            RollbackAuthority::Proposal => {
                format!("approve rolling back to signed release {to_release}")
            }
        };
        let now = self.clock.now();
        let mut alarm = Alarm::new(
            self.site.id,
            Urgency::P1,
            "incident",
            format!(
                "Incident {id}: {} file(s) on {} match no signed release; {act}.",
                drift.len(),
                self.site.origin
            ),
            now,
        );
        alarm.id = format!("{id}-alarm");
        self.alarms.raise(alarm.clone()).await;
        let incident = Incident {
            id: id.clone(),
            site: self.site.id,
            opened_at: now,
            drift,
            rollback: RollbackPlan { to_release, authority, done: None },
            area: area.map(str::to_string),
            demoted,
            alarm: alarm.id,
            closed: None,
        };
        self.incidents.lock().await.insert(id, incident.clone());
        Ok(incident)
    }

    /// Records that the rollback of incident `id` ran, as the Guardian's `site_rollback` or
    /// the owner's approval, with the release now served.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown incident.
    pub async fn rolled_back(&self, id: &str, served: u64) -> Result<Incident> {
        let mut incidents = self.incidents.lock().await;
        let incident = incidents.get_mut(id).ok_or_else(|| unknown(id))?;
        incident.rollback.done = Some(served);
        audit::record(
            self.audit.as_ref(),
            audit::ROLLBACK,
            AuditActor::default(),
            json!({ "site": self.site.id, "incident": id, "toRelease": incident.rollback.to_release, "releaseSeq": served }),
        )
        .await;
        Ok(incident.clone())
    }

    /// The owner approves the proposed rollback of incident `id`: it runs as the owner's act
    /// (spec 3.5 A-I11). When the host cannot restore the planned release exactly, the
    /// releases before it are tried in turn: the last good one is the latest that restores.
    ///
    /// # Errors
    ///
    /// `Forbidden` when `by` is not a person; `NotFound` for an unknown incident; the
    /// pipeline's error.
    pub async fn approve_rollback(&self, id: &str, by: &SubjectRef) -> Result<Incident> {
        if not_a_person(by) {
            return Err(forbidden(by, "approves a rollback"));
        }
        let planned = {
            let incidents = self.incidents.lock().await;
            incidents.get(id).ok_or_else(|| unknown(id))?.rollback.to_release
        };
        let mut failure = None;
        for to in (1..=planned).rev() {
            match self.runtime.pipeline().rollback(to).await {
                Ok(published) => return self.rolled_back(id, published.staged.manifest.seq).await,
                Err(error) => {
                    tracing::warn!(website.incident = id, to_release = to, error = %error, "release does not restore; trying the one before");
                    failure = Some(error);
                }
            }
        }
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Internal,
            "website.incident.rollback",
            format!(
                "no signed release up to {planned} restores exactly: {}",
                failure.map(|e| e.to_string()).unwrap_or_default()
            ),
        ))
    }

    /// Closes incident `id` by a person's decision `decision`: the status returns to
    /// `operational` (spec 3.5 §7.20: "exit is a person's decision").
    ///
    /// # Errors
    ///
    /// `Forbidden` when `by` is not a person; `NotFound` for an unknown incident; the
    /// status sink's error.
    pub async fn close(&self, id: &str, by: &SubjectRef, decision: &str) -> Result<Incident> {
        if not_a_person(by) {
            return Err(forbidden(by, "closes an incident"));
        }
        let (incident, served) = {
            let mut incidents = self.incidents.lock().await;
            let incident = incidents.get_mut(id).ok_or_else(|| unknown(id))?;
            incident.closed = Some(IncidentExit {
                by: by.id.clone(),
                decision: decision.to_string(),
                at: self.clock.now(),
            });
            let served = incident.rollback.done.unwrap_or(incident.rollback.to_release);
            (incident.clone(), served)
        };
        if let Err(error) = self.alarms.acknowledge(&incident.alarm, by).await {
            tracing::debug!(error = %error, "the incident's alarm was already acknowledged");
        }
        self.set_status(SiteState::Operational, "incident_closed", Some(served)).await?;
        Ok(incident)
    }

    /// Every incident, open and closed.
    pub async fn incidents(&self) -> Vec<Incident> {
        self.incidents.lock().await.values().cloned().collect()
    }

    /// The open incident, if any.
    pub async fn open_incident(&self) -> Option<Incident> {
        self.incidents.lock().await.values().find(|i| i.closed.is_none()).cloned()
    }
}

fn unknown(id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::NotFound,
        "website.incident.unknown",
        format!("no incident {id}"),
    )
}

fn forbidden(by: &SubjectRef, act: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        "website.incident.not_a_person",
        format!("{} is not a person; only a person {act} (spec 3.5 §7.20)", by.id),
    )
}

/// The Guardian's probe as a kernel process (spec 3.5 §7.20, §7.8 "Probes", A-I9): each run
/// re-proves the domain claim — a `ProvenDomain` is never stored — probes the host and the
/// origin, and hands the report to the incident path.
pub struct GuardianProbe {
    /// The owner's claim of the domain.
    pub claim: DomainClaim,
    /// The resolver that proves it.
    pub resolver: Arc<dyn DnsResolver>,
    /// The host.
    pub host: Arc<dyn SiteHost>,
    /// The signed, rate-limited client that reads the origin.
    pub http: Arc<HttpClient>,
    /// The release keys the checkpoint must verify against.
    pub trusted: Vec<NoteVerifier>,
    /// What to probe.
    pub config: ProbeConfig,
    /// Where `site.drift.detected` goes, as the Guardian.
    pub audit: Option<(Arc<dyn AuditRecorder>, ActingContext)>,
    /// The incident path.
    pub incidents: Arc<IncidentPath>,
}

/// One probe's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeOutcome {
    /// The report.
    pub report: ProbeReport,
    /// The incident it opened or joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident: Option<Incident>,
}

impl GuardianProbe {
    /// Runs one probe (spec 3.5 §7.20).
    ///
    /// # Errors
    ///
    /// Returns the claim's refusal when the domain is no longer proven (A-I9), the prober's
    /// error, or the incident path's.
    #[tracing::instrument(skip_all, name = "site.probe", fields(site.domain = %self.config.domain))]
    pub async fn run(&self) -> Result<ProbeOutcome> {
        let migrate = |e: adk_site::migrate::MigrateError| AdkError::config(e.to_string());
        let proven =
            self.claim.verify_dns(self.resolver.as_ref(), Utc::now()).await.map_err(migrate)?;
        let mut prober = Prober::new(
            self.host.clone(),
            self.http.clone(),
            self.resolver.clone(),
            self.trusted.clone(),
            &proven,
            self.config.clone(),
        )
        .map_err(migrate)?;
        if let Some((recorder, acting)) = &self.audit {
            prober = prober.with_audit(recorder.clone(), acting.clone());
        }
        let report = prober.probe().await.map_err(migrate)?;
        let incident = self.incidents.on_probe(&report).await?;
        Ok(ProbeOutcome { report, incident })
    }
}
