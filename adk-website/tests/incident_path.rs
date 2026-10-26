//! The incident path up to the edge's read-only step (spec 3.5 §7.20, §7.21, §7.11, A-I4,
//! A-I11, S5): drift opens one incident, sets the status, raises one P1 alarm and plans a
//! rollback to the last good signed release. At Propose the rollback is the owner's
//! decision and the Guardian's `site_rollback` is refused; at "act with approval" it is
//! held for the owner; once the owner pre-authorises it ("act, reversible only", a standing
//! rule the owner's decision created) the Guardian rolls back alone. Only a person closes an
//! incident.

mod common;

use std::sync::Arc;

use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_harness::website::{job_levels, promote_job};
use adk_kernel::charter::OwnerDecision;
use adk_model::ScriptedResponse;
use adk_site::content::ContentStore;
use adk_site::host::SiteHost;
use adk_site::migrate::probe::{Drift, DriftKind, DriftLocation, ProbeReport};
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_site::release::{file_digest, manifest_path};
use adk_website::alarms::AlarmRouter;
use adk_website::incident::{InMemoryStatusBoard, IncidentPath, RollbackAuthority, StatusSink};
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::SitePlan;
use adk_website::team::AgentRole;
use awp_types::{AutonomyLevel, Channel, Charter, ReleaseManifest, Role, SiteKind, SiteState};
use common::*;
use serde_json::json;

/// The home page: release 2 changed it, so every release after 1 lists it.
const PAGE: &str = "index.html";

async fn launched() -> (Site, Charter) {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let items = SitePlan::new(&charter, "Juniper Bakery").items(&charter, &[], &["KE".to_string()]);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "release 1");
    // Release 2: the owner's own edit of the home page.
    let mut home =
        site.store.get(&awp_types::ContentId("pages/home/en".into())).await.unwrap().unwrap();
    home.fields.insert("summary".into(), json!("Rye and sourdough, baked every morning."));
    let edit = Change {
        items: vec![home],
        media: Vec::new(),
        entry: EntryPoint::WebEditor,
        proposer: AgentRole::Steward.principal(&site.site),
        job: "freshness".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(edit).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "release 2");
    (site, charter)
}

/// Replaces the served page and returns the probe's report of it.
async fn tamper(site: &Site, seq: u64) -> ProbeReport {
    let manifest: ReleaseManifest =
        serde_json::from_slice(&site.host.fetch(&manifest_path(seq)).await.unwrap().unwrap())
            .unwrap();
    let expected = manifest
        .files
        .iter()
        .find(|(path, _)| path.trim_start_matches('/') == PAGE)
        .unwrap()
        .1
        .sha256
        .clone();
    let defaced = b"<html><body>defaced</body></html>";
    let served = site.dirs[0].path().join(format!("releases/{seq}/{PAGE}"));
    assert!(served.exists(), "{}", served.display());
    // Replaced as an intruder replaces a file: a new file renamed over the served one, so the
    // host's stored objects of other releases stay as they were.
    let replacement = served.with_extension("tmp");
    std::fs::write(&replacement, defaced).unwrap();
    std::fs::rename(&replacement, &served).unwrap();
    ProbeReport {
        probed_at: chrono::Utc::now(),
        checkpoint: Some(seq),
        window: vec![seq],
        files_checked: 1,
        drift: vec![Drift {
            location: DriftLocation::Host,
            kind: DriftKind::Changed,
            path: format!("/{PAGE}"),
            expected: vec![expected],
            found: Some(file_digest(defaced)),
            window: vec![seq],
            detail: "the page matches no release in the window".into(),
        }],
        status: Vec::new(),
        certificate: None,
        dns: Vec::new(),
        missing_headers: Vec::new(),
        problems: Vec::new(),
    }
}

async fn page_matches_release(site: &Site) -> bool {
    let served = site.host.fetch(PAGE).await.unwrap().unwrap();
    !String::from_utf8_lossy(&served).contains("defaced")
}

#[tokio::test]
async fn drift_opens_one_incident_and_the_rollback_follows_the_protect_jobs_authority() {
    let (mut site, charter) = launched().await;
    let trail = AuditTrail::new(Arc::new(site.audit.clone()));
    let phone = Phone::new("sms");
    let alarms = Arc::new(
        AlarmRouter::new(site.site.id, &charter.reporting)
            .with_sink(phone.clone(), "owner-thread")
            .with_audit(trail.clone()),
    );
    let status = Arc::new(InMemoryStatusBoard::new());
    let path = IncidentPath::new(
        site.runtime.clone(),
        site.ladder.clone(),
        alarms.clone(),
        status.clone(),
    )
    .with_audit(trail);

    // At Propose: one incident, one P1 alarm, the rollback is the owner's. The host's copy of
    // release 2 was changed, so the last good release is 1.
    let report = tamper(&site, 2).await;
    let incident = path.on_probe(&report).await.unwrap().expect("drift opens an incident");
    assert_eq!(incident.rollback.to_release, 1);
    assert_eq!(incident.rollback.authority, RollbackAuthority::Proposal);
    assert!(incident.demoted.is_empty(), "tampering from outside demotes no job");
    assert_eq!(status.get(site.site.id).await.unwrap().unwrap().state, SiteState::Incident);
    let again = path.on_probe(&report).await.unwrap().unwrap();
    assert_eq!(again.id, incident.id, "the next probe joins the open incident");
    assert_eq!(phone.messages().len(), 1, "one P1 alarm per incident");
    assert!(phone.messages()[0].starts_with("Urgent:"), "{:?}", phone.messages());

    let (guardian, _) = site.back_office(
        "guardian",
        &charter,
        vec![
            ScriptedResponse::function_call("site_rollback", json!({ "toRelease": 1 })),
            ScriptedResponse::text("Proposed."),
        ],
        |b| b.mode(adk_harness::HarnessMode::Plan),
    );
    let owner = user(OWNER);
    let session = guardian.open_session(&owner, None).await.unwrap();
    let refused =
        collect(guardian.send(&owner, &session, &incident.guardian_turn()).await.unwrap()).await;
    assert!(
        response(&refused, "site_rollback", 0).to_string().contains("Propose"),
        "{:?}",
        responses(&refused)
    );
    assert!(!page_matches_release(&site).await, "nothing rolled back alone");

    let service = SubjectRef::service("urn:adk:agent:juniper/guardian");
    assert!(path.approve_rollback(&incident.id, &service).await.is_err(), "no agent approves");
    let rolled = path.approve_rollback(&incident.id, &SubjectRef::human(OWNER)).await.unwrap();
    assert_eq!(rolled.rollback.done, Some(3), "the rollback is a new release");
    assert!(page_matches_release(&site).await, "the owner's rollback restored the page");
    assert!(
        path.close(&incident.id, &service, "dec-close").await.is_err(),
        "exit is a person's decision"
    );
    path.close(&incident.id, &SubjectRef::human(OWNER), "dec-close").await.unwrap();
    assert_eq!(status.get(site.site.id).await.unwrap().unwrap().state, SiteState::Operational);
    assert!(alarms.open().await.is_empty(), "closing acknowledges the alarm");

    // The owner pre-authorises the rollback: two promotions by the owner's decisions.
    let compiler = site.compiler(&charter);
    for (n, level) in [(1, AutonomyLevel::ActWithApproval), (2, AutonomyLevel::ActReversibleOnly)] {
        let decision = OwnerDecision::new(
            format!("dec-promote-{n}"),
            SubjectRef::human(OWNER),
            chrono::Utc::now(),
        );
        let promotion =
            promote_job(&site.ladder, &compiler, site.rules.as_ref(), "protect", &decision, OWNER)
                .await
                .unwrap();
        assert_eq!(promotion.job.level, level);
    }
    assert_eq!(path.rollback_authority().await.unwrap(), RollbackAuthority::Standing);

    let report = tamper(&site, 3).await;
    let second = path.on_probe(&report).await.unwrap().unwrap();
    assert_eq!(second.rollback.to_release, 1, "release 2's copy was changed too");
    assert_ne!(second.id, incident.id);
    assert_eq!(second.rollback.authority, RollbackAuthority::Standing);
    let levels = job_levels(&compiler, site.autonomy.as_ref(), site.site.id).await.unwrap();
    let mode = compiler.mode_for_agent("guardian", &levels);
    assert_eq!(mode, adk_harness::HarnessMode::Auto);
    let (guardian, _) = site.back_office(
        "guardian",
        &charter,
        vec![
            ScriptedResponse::function_call("site_rollback", json!({ "toRelease": 1 })),
            ScriptedResponse::text("Rolled back."),
        ],
        |b| b.mode(mode),
    );
    let session = guardian.open_session(&owner, None).await.unwrap();
    let ran =
        collect(guardian.send(&owner, &session, &second.guardian_turn()).await.unwrap()).await;
    let published = response(&ran, "site_rollback", 0);
    assert_eq!(published["published"], true, "{published}");
    assert!(page_matches_release(&site).await, "the Guardian rolled back alone");
    let rolled =
        path.rolled_back(&second.id, published["release"].as_u64().unwrap()).await.unwrap();
    assert_eq!(rolled.rollback.done, Some(4));
    assert_eq!(
        status.get(site.site.id).await.unwrap().unwrap().state,
        SiteState::Incident,
        "still open until a person closes it"
    );
    assert_eq!(phone.messages().len(), 2);

    let actions = site.actions();
    for action in [
        "site.status.changed",
        "site.rollback",
        "website.alarm.raised",
        "website.alarm.acknowledged",
    ] {
        assert!(actions.iter().any(|a| a == action), "{action} in {actions:?}");
    }
}

#[tokio::test]
async fn at_act_with_approval_the_guardians_rollback_is_held_for_the_owner() {
    let (mut site, charter) = launched().await;
    let compiler = site.compiler(&charter);
    let decision = OwnerDecision::new("dec-promote", SubjectRef::human(OWNER), chrono::Utc::now());
    promote_job(&site.ladder, &compiler, site.rules.as_ref(), "protect", &decision, OWNER)
        .await
        .unwrap();
    let alarms = Arc::new(AlarmRouter::new(site.site.id, &charter.reporting));
    let path = IncidentPath::new(
        site.runtime.clone(),
        site.ladder.clone(),
        alarms,
        Arc::new(InMemoryStatusBoard::new()),
    );
    assert_eq!(path.rollback_authority().await.unwrap(), RollbackAuthority::Proposal);
    let incident = path.on_probe(&tamper(&site, 2).await).await.unwrap().unwrap();

    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let (guardian, _) = site.back_office(
        "guardian",
        &charter,
        vec![
            ScriptedResponse::function_call("site_rollback", json!({ "toRelease": 1 })),
            ScriptedResponse::text("Done."),
        ],
        |b| b.mode(adk_harness::HarnessMode::Ask).prompter(prompter.clone()),
    );
    let owner = user(OWNER);
    let session = guardian.open_session(&owner, None).await.unwrap();
    let events =
        collect(guardian.send(&owner, &session, &incident.guardian_turn()).await.unwrap()).await;
    assert_eq!(prompter.held(), ["site_rollback"], "held for the owner");
    assert_eq!(response(&events, "site_rollback", 0)["published"], true);
    assert!(page_matches_release(&site).await);
}
