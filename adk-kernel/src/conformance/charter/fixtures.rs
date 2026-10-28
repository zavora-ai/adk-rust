//! Charters, decisions and signals the suites and the charter tests build (spec 3.5 §7.6,
//! §7.10).

use adk_core::principal::SubjectRef;
use awp_types::{
    ActionClass, ApprovalChain, AutonomyLevel, Bounded, Breach, Budget, Cadence, Charter,
    Direction, Governance, Guardrail, GuardrailAction, JobAuthority, Kpi, Measurement, MetricRef,
    Reporting, Role, RoleHolder, Signal, SignalKind, SiteKind, SourceRef, Target, Voice,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::charter::{CommitDecision, RoleApproval};

/// A valid business charter of `site` whose owners are `owners`, with no KPIs, guardrails
/// or jobs, and no approval chain (a commit then needs one owner).
pub fn charter(site: Uuid, owners: &[&str]) -> Charter {
    Charter {
        version: 0,
        site_id: site,
        kind: SiteKind::Business,
        governance: Governance {
            roles: owners
                .iter()
                .map(|owner| RoleHolder {
                    principal: (*owner).to_string(),
                    roles: vec![Role::Owner],
                })
                .collect(),
            approval_chains: Vec::new(),
        },
        mission: "Sell solar kits to homes in Nairobi".into(),
        strategy: Vec::new(),
        goals: Vec::new(),
        kpis: Vec::new(),
        guardrails: Vec::new(),
        authority: Vec::new(),
        budget: Budget {
            period: Cadence::Monthly,
            currency: "USD".into(),
            caps: Default::default(),
        },
        voice: Voice {
            tone: "warm and plain".into(),
            languages: Vec::new(),
            disclosure: "Written by the site's AI agent for its owner.".into(),
        },
        red_lines: Vec::new(),
        reporting: Reporting { review: Cadence::Weekly, routes: Vec::new(), quiet_hours: None },
        public: Default::default(),
        price_rule: Default::default(),
        handoff_threshold: None,
        publish_delay_hours: None,
        amount_thresholds: Default::default(),
        signed: None,
    }
}

/// `charter` with `principal` holding `role` as well.
pub fn with_role(mut charter: Charter, role: Role, principal: &str) -> Charter {
    match charter.governance.roles.iter_mut().find(|holder| holder.principal == principal) {
        Some(holder) => holder.roles.push(role),
        None => charter
            .governance
            .roles
            .push(RoleHolder { principal: principal.to_string(), roles: vec![role] }),
    }
    charter
}

/// `charter` with the approval chain of `action` requiring `roles`.
pub fn with_chain(mut charter: Charter, action: impl Into<ActionClass>, roles: &[Role]) -> Charter {
    let action = action.into();
    charter.governance.approval_chains.retain(|chain| chain.action != action);
    charter.governance.approval_chains.push(ApprovalChain { action, roles: roles.to_vec() });
    charter
}

/// A KPI `id` measuring `metric` from `source` daily, upwards, with no baseline or target.
pub fn kpi(id: &str, metric: &str, source: &str) -> Kpi {
    Kpi {
        id: id.to_string(),
        name: id.replace('_', " "),
        metric: MetricRef(metric.to_string()),
        source: SourceRef::from(source),
        direction: Direction::Up,
        baseline: None,
        target: None,
        cadence: Cadence::Daily,
    }
}

/// `kpi` with a baseline of `baseline` measured at `at` and a target of `target` 90 days on.
pub fn with_target(mut kpi: Kpi, baseline: f64, target: f64, at: DateTime<Utc>) -> Kpi {
    kpi.baseline = Some(Measurement {
        value: baseline,
        window: std::time::Duration::from_secs(14 * 24 * 60 * 60),
        measured_at: at,
    });
    kpi.target = Some(Target { value: target, horizon: at + chrono::Duration::days(90) });
    kpi
}

/// A guardrail `id` that pauses `jobs` when `metric`'s mean over `window_days` rises above
/// `threshold`.
pub fn guardrail(
    id: &str,
    metric: &str,
    threshold: f64,
    window_days: u64,
    jobs: &[&str],
) -> Guardrail {
    Guardrail {
        id: id.to_string(),
        metric: MetricRef(metric.to_string()),
        threshold,
        breach: Breach::Above,
        window: std::time::Duration::from_secs(window_days * 24 * 60 * 60),
        action: GuardrailAction::PauseJob,
        jobs: jobs.iter().map(|job| (*job).to_string()).collect(),
    }
}

/// The authority of `job` at `level`, with no limits.
pub fn authority(job: &str, level: AutonomyLevel) -> JobAuthority {
    JobAuthority { job: job.to_string(), level, limits: Default::default() }
}

/// A decision approved by each `(role, person)` at `at`.
pub fn decision(id: &str, at: DateTime<Utc>, approvals: &[(Role, &str)]) -> CommitDecision {
    approvals.iter().fold(CommitDecision::new(id, at), |decision, (role, who)| {
        decision.with_approval(RoleApproval::new(*role, SubjectRef::human(*who), at))
    })
}

/// `kind` emitted now by the concierge.
pub fn signal(kind: SignalKind) -> Signal {
    Signal { id: Uuid::new_v4(), kind, emitted_by: "concierge".into(), emitted_at: Utc::now() }
}

/// `signal` as emitted by `source`.
pub fn emitted_by(mut signal: Signal, source: &str) -> Signal {
    signal.emitted_by = source.to_string();
    signal
}

/// A content-gap signal on `topic` asked `questions` times, from the concierge.
pub fn content_gap(topic: &str, questions: u32) -> Signal {
    signal(SignalKind::ContentGap {
        topic: Bounded::new(topic, 120).expect("a short topic"),
        question_count: questions,
        samples: Vec::new(),
    })
}

/// A review signal from `platform` with `rating` stars and `excerpt`, from the concierge.
pub fn review(platform: &str, rating: u8, excerpt: &str) -> Signal {
    signal(SignalKind::ReviewSeen {
        platform: platform.to_string(),
        rating,
        excerpt: Bounded::new(excerpt, 280).expect("a short excerpt"),
    })
}
