//! S5 — tamper, detect, reverse (spec 3.5 §5 S5, §7.20, §7.21, §7.8 "Probes", A-I4, A-I9,
//! A-I11).
//!
//! A launched bakery runs its Guardian's probe against the host and the public origin, with
//! the agent edge in front. A file on the host is replaced, twice: first while the protect job
//! acts with approval (Ask), then after the owner pre-authorised the rollback with a standing
//! rule (Auto).
//!
//! Pass condition: within one probe interval, `site.drift.detected`, status `incident`, edge
//! `read_only`, rollback to the last good release (standing rule in Auto, held in Ask), one P1
//! alarm to the owner, linked audit records.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditRecorder;
use adk_core::principal::SubjectRef;
use adk_harness::HarnessMode;
use adk_harness::website::{job_levels, promote_job};
use adk_kernel::charter::OwnerDecision;
use adk_model::ScriptedResponse;
use adk_site::content::ContentStore;
use adk_site::host::SiteHost;
use adk_site::migrate::claim::{DomainClaim, RecordKind, StaticDnsResolver};
use adk_site::migrate::http::{EgressScope, HttpClient};
use adk_site::migrate::probe::ProbeConfig;
use adk_site::migrate::serve::StaticServer;
use adk_website::alarms::AlarmRouter;
use adk_website::incident::{GuardianProbe, IncidentPath, RollbackAuthority, StatusSink};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{AutonomyLevel, SiteKind, SiteState};
use axum::http::StatusCode;
use serde_json::{Value, json};
use url::Url;

use crate::checks::Checks;
use crate::fixture::edge::edge;
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";
const PAGE: &str = "index.html";

/// Replaces the served home page of release `seq` the way an intruder does: a new file renamed
/// over the served one, so other releases' stored objects stay as they were.
fn tamper(site: &Site, seq: u64) -> Result<()> {
    let served = site.host_dir().join(format!("releases/{seq}/{PAGE}"));
    let replacement = served.with_extension("tmp");
    std::fs::write(&replacement, b"<html><body>defaced</body></html>")?;
    std::fs::rename(&replacement, &served)?;
    Ok(())
}

async fn defaced(site: &Site) -> Result<bool> {
    let served = site.host.fetch(PAGE).await?.ok_or_else(|| anyhow!("no home page"))?;
    Ok(String::from_utf8_lossy(&served).contains("defaced"))
}

fn ext<'a>(records: &'a [adk_core::audit_log::AuditRecordDraft], action: &str) -> Vec<&'a Value> {
    records.iter().filter(|r| r.action == action).flat_map(|r| r.ext.values()).collect()
}

/// Runs S5.
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Business, "Juniper Bakery", DOMAIN, &["en"]).await?;
    let charter = site.launch("Juniper Bakery", "KES", Vec::new()).await?;
    // Release 2: the owner's own edit of the home page.
    let mut home = site
        .store
        .get(&awp_types::ContentId("pages/home/en".into()))
        .await?
        .ok_or_else(|| anyhow!("home"))?;
    home.fields.insert("summary".into(), json!("Rye and sourdough, baked every morning."));
    let mut edit = site.steward_change(vec![home], "freshness");
    edit.entry = adk_site::pipeline::EntryPoint::WebEditor;
    site.runtime.pipeline().run(edit).await?;

    // The agent edge in front, the owner's phone for P1, the incident path over the edge's
    // status, and the Guardian's probe: the domain re-proven by DNS each run, the host and the
    // public origin read through a signed client.
    let site_edge = edge(&site, DOMAIN).await?;
    let sms = Phone::new("sms");
    let audit: Arc<dyn AuditRecorder> = Arc::new(site.audit.clone());
    let trail = adk_core::audit_log::AuditTrail::new(audit.clone());
    let alarms = Arc::new(
        AlarmRouter::new(site.site.id, &charter.reporting)
            .with_sink(sms.clone(), "owner-sms")
            .with_audit(trail.clone()),
    );
    let status: Arc<dyn StatusSink> = Arc::new(site_edge.edge.status());
    let path = Arc::new(
        IncidentPath::new(
            site.runtime.clone(),
            site.ladder.clone(),
            alarms.clone(),
            status.clone(),
        )
        .with_audit(trail),
    );
    let origin = StaticServer::serve(&site.host_dir().join("current")).await?;
    let public = Url::parse(&site.site.origin)?;
    let claim = DomainClaim::new(DOMAIN, chrono::Utc::now())?;
    let resolver = Arc::new(StaticDnsResolver::new());
    let (name, value) = claim.dns_record();
    resolver.set(&name, RecordKind::Txt, [value]).await;
    let http = HttpClient::builder()
        .egress(EgressScope::Only(BTreeSet::from([origin.origin()])))
        .map_origin(&public, Url::parse(&origin.origin())?)
        .min_interval(Duration::ZERO)
        .build()?;
    let mut config = ProbeConfig::new(DOMAIN, public);
    config.sample = 100;
    config.seed = Some(5);
    let guardian_acting = adk_core::ActingContext::derived(
        &site.site.name,
        AgentRole::Guardian.as_str(),
        OWNER,
        adk_core::SubjectKind::Service,
    );
    let probe = GuardianProbe {
        claim,
        resolver,
        host: site.host.clone(),
        http: Arc::new(http),
        trusted: vec![site.release_key.clone()],
        config,
        audit: Some((audit.clone(), guardian_acting)),
        incidents: path.clone(),
    };
    let healthy = probe.run().await?;
    checks.require(
        "a healthy site shows no drift",
        !healthy.report.drift_detected() && healthy.incident.is_none(),
        format!("{} files checked", healthy.report.files_checked),
    )?;

    // Tamper 1, while the protect job acts with approval: the next probe finds it.
    let compiler = site.compiler(&charter);
    let decision = |n: u32| {
        OwnerDecision::new(format!("dec-promote-{n}"), SubjectRef::human(OWNER), chrono::Utc::now())
    };
    promote_job(&site.ladder, &compiler, site.rules.as_ref(), "protect", &decision(1), OWNER)
        .await?;
    tamper(&site, 2)?;
    let found = probe.run().await?;
    let incident = found.incident.clone().ok_or_else(|| anyhow!("drift opens an incident"))?;
    let drifted: BTreeSet<String> =
        found.report.drift.iter().map(|d| format!("{:?} {}", d.location, d.path)).collect();
    checks.check(
        "within one probe interval: site.drift.detected",
        found.report.drift_detected() && site.actions().iter().any(|a| a == "site.drift.detected"),
        format!("the first probe after the change: {drifted:?}"),
    );
    let state = status.get(site.site.id).await?.map(|s| s.state);
    let (code, _, document) = site_edge.send(site_edge.get("/awp/v2/status")).await;
    checks.check(
        "status incident, edge read_only",
        state == Some(SiteState::Incident)
            && code == StatusCode::OK
            && document["state"] == "read_only",
        format!("site {state:?}, edge {} ({})", document["state"], document["reasonCode"]),
    );
    let receipt = site_edge.consent("contact").await?;
    let enquiry = json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "Open today?" });
    let (refused, _, _) =
        site_edge.send(site_edge.call("submit_inquiry", "s5-1", Some(&receipt), &enquiry)).await;
    let (page, _, _) = site_edge.send(site_edge.get("/about/")).await;
    checks.check(
        "writes answer 503 read_only while pages keep serving (A-I11)",
        refused == StatusCode::SERVICE_UNAVAILABLE && page == StatusCode::OK,
        format!("enquiry {refused}, page {page}"),
    );
    checks.check(
        "one P1 alarm to the owner",
        sms.messages().len() == 1 && sms.messages()[0].starts_with("Urgent:"),
        sms.messages().first().cloned().unwrap_or_default(),
    );
    let again = probe.run().await?;
    checks.check(
        "the next probe joins the open incident: still one alarm",
        again.incident.as_ref().is_some_and(|i| i.id == incident.id) && sms.messages().len() == 1,
        format!("incident {}", incident.id),
    );

    // In Ask, the Guardian's rollback is held for the owner, who approves it.
    checks.check(
        "the rollback goes to the last good release",
        incident.rollback.to_release == 1
            && incident.rollback.authority == RollbackAuthority::Proposal,
        format!("to release {}, {:?}", incident.rollback.to_release, incident.rollback.authority),
    );
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let to = incident.rollback.to_release;
    let (guardian, _) = site.back_office(
        &mode,
        "guardian",
        &charter,
        vec![
            call("site_rollback", json!({ "toRelease": to })),
            ScriptedResponse::text("Rolled back."),
        ],
        |b| {
            b.instruction(AgentRole::Guardian.prompt())
                .mode(HarnessMode::Ask)
                .prompter(prompter.clone())
        },
    )?;
    let owner = user(OWNER);
    let session = guardian.open_session(&owner, None).await?;
    let events = collect(guardian.send(&owner, &session, &incident.guardian_turn()).await?).await?;
    if mode.is_live() {
        trace(&events);
    }
    let rolled = response(&events, "site_rollback", 0).unwrap_or_default();
    checks.check(
        "in Ask the rollback is held for the owner, then restores the page",
        prompter.held() == ["site_rollback"]
            && rolled["published"] == true
            && !defaced(&site).await?,
        format!("held {:?}, release {}", prompter.held(), rolled["release"]),
    );
    if let Some(seq) = rolled["release"].as_u64() {
        path.rolled_back(&incident.id, seq).await?;
    }
    let guardian_subject = SubjectRef::service(AgentRole::Guardian.principal(&site.site));
    let agent_close = path.close(&incident.id, &guardian_subject, "dec-close").await.is_err();
    path.close(&incident.id, &SubjectRef::human(OWNER), "dec-close-1").await?;
    let (_, _, resumed) = site_edge.send(site_edge.get("/awp/v2/status")).await;
    checks.check(
        "only a person closes the incident; the edge resumes",
        agent_close && resumed["state"] == "operational",
        format!("edge {}", resumed["state"]),
    );

    // The owner pre-authorises the rollback: "act, reversible only", a standing rule the
    // owner's decision created. Tamper 2: the Guardian rolls back alone, in Auto.
    let promotion =
        promote_job(&site.ladder, &compiler, site.rules.as_ref(), "protect", &decision(2), OWNER)
            .await?;
    let authority = path.rollback_authority().await?;
    checks.check(
        "a standing rule the owner created pre-authorises the rollback",
        promotion.job.level == AutonomyLevel::ActReversibleOnly
            && authority == RollbackAuthority::Standing,
        format!("{:?}, {authority:?}", promotion.job.level),
    );
    let current = rolled["release"].as_u64().unwrap_or(3);
    tamper(&site, current)?;
    let second = probe.run().await?.incident.ok_or_else(|| anyhow!("a second incident"))?;
    let levels = job_levels(&compiler, site.autonomy.as_ref(), site.site.id).await?;
    let guardian_mode = compiler.mode_for_agent("guardian", &levels);
    let auto_prompter = ScriptedPrompter::new();
    let to = second.rollback.to_release;
    let (guardian, _) = site.back_office(
        &mode,
        "guardian",
        &charter,
        vec![
            call("site_rollback", json!({ "toRelease": to })),
            ScriptedResponse::text("Rolled back."),
        ],
        |b| {
            b.instruction(AgentRole::Guardian.prompt())
                .mode(guardian_mode)
                .prompter(auto_prompter.clone())
        },
    )?;
    let session = guardian.open_session(&owner, None).await?;
    let events = collect(guardian.send(&owner, &session, &second.guardian_turn()).await?).await?;
    if mode.is_live() {
        trace(&events);
    }
    let alone = response(&events, "site_rollback", 0).unwrap_or_default();
    checks.check(
        "in Auto the Guardian rolls back alone under the standing rule",
        guardian_mode == HarnessMode::Auto
            && auto_prompter.held().is_empty()
            && alone["published"] == true
            && !defaced(&site).await?,
        format!("{guardian_mode:?}, release {}", alone["release"]),
    );
    if let Some(seq) = alone["release"].as_u64() {
        path.rolled_back(&second.id, seq).await?;
    }
    checks.check(
        "one P1 alarm per incident",
        sms.messages().len() == 2,
        format!("{} alarms for 2 incidents", sms.messages().len()),
    );

    // The records link: the drift, the status, the alarm and the rollback of each incident.
    let records = site.audit.records();
    let alarms_raised = ext(&records, "website.alarm.raised");
    let rollbacks = ext(&records, "site.rollback");
    let linked = [&incident, &second].iter().all(|i| {
        alarms_raised.iter().any(|v| v.as_str() == Some(i.alarm.as_str()))
            && rollbacks.iter().any(|v| v.as_str() == Some(i.id.as_str()))
    });
    let states: Vec<&Value> = ext(&records, "site.status.changed");
    checks.check(
        "linked audit records",
        linked
            && records.iter().filter(|r| r.action == "site.drift.detected").count() >= 2
            && states.iter().any(|v| v.as_str() == Some("incident")),
        format!(
            "{} drift, {} status, {} alarm, {} rollback records; each alarm and rollback names its incident",
            records.iter().filter(|r| r.action == "site.drift.detected").count(),
            records.iter().filter(|r| r.action == "site.status.changed").count(),
            records.iter().filter(|r| r.action == "website.alarm.raised").count(),
            records.iter().filter(|r| r.action == "site.rollback").count(),
        ),
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;
    Ok(checks)
}
