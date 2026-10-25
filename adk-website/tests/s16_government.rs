//! S16 — a public body (spec 3.5 §5 S16, §7.16.1 Government, §7.6, A-I23): a county site in
//! two official languages with an editor and a publisher.
//!
//! Pass conditions covered here: the launch gate fails until every page exists in both
//! languages and passes; no third-party script ships; the permit form is a service
//! capability that needs a verified identity; a release the editor proposes publishes only
//! after the publisher approves, each a distinct person; no advertising, sales or wallet job
//! exists beyond hosting. The OIDC identity of the applicant is the edge's (spec 3.5 §7.9).

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_kernel::charter::RoleApproval;
use adk_site::content::SiteProject;
use adk_site::gate::{GateCheck, GateInput, GateMode, LaunchGate, Quarantine};
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::pipeline::{ApprovalDecision, Change, EntryPoint, PipelineOutcome};
use adk_website::kinds::{CharterInputs, KindRule, ThirdPartyScripts};
use adk_website::onboarding::build::{SitePlan, stage_release_one};
use adk_website::packs::capability;
use adk_website::team::AgentRole;
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, Markdown, Role, RoleHolder, SiteKind,
    TrustTier,
};
use common::*;

const CLERK: &str = OWNER;
const DIRECTOR: &str = "director";

fn notice(slug: &str, language: &str, title: &str) -> ContentItem {
    ContentItem {
        id: ContentId(format!("notices/{slug}/{language}")),
        kind: ContentKind::Notice,
        slug: slug.into(),
        language: language.parse().unwrap(),
        fields: [
            ("title".to_string(), serde_json::json!(title)),
            ("summary".to_string(), serde_json::json!(title)),
            ("goal".to_string(), serde_json::json!("accuracy")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown(format!("{title}.\n"))),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    }
}

#[tokio::test]
async fn s16_a_public_body_needs_both_languages_no_trackers_and_two_people_to_publish() {
    let site =
        Site::new(SiteKind::Government, "Kaunti ya Nakuru", "nakuru.go.ke", &["en", "sw"]).await;
    let template = &site.template;
    for rule in [
        KindRule::TwoPersonPublication,
        KindRule::AllLanguagesRequired,
        KindRule::NoThirdPartyScripts,
        KindRule::AiDisclosureOnEveryReply,
    ] {
        assert!(template.has(&rule), "{rule:?}");
    }
    assert!(
        site.team.public_instruction().contains("AI agent"),
        "every reply discloses AI assistance"
    );

    // No advertising, sales or wallet jobs beyond hosting; no Revenue or Finance agent.
    let jobs: BTreeSet<&str> = template.jobs().iter().map(|job| job.id).collect();
    for absent in ["advertising", "sell", "nurture", "handoff", "running-costs", "cost-and-return"]
    {
        assert!(!jobs.contains(absent), "a public body runs no {absent} job");
    }
    assert!(jobs.contains("hosting"));
    assert!(!site.team.runs(AgentRole::Revenue) && !site.team.runs(AgentRole::Finance));

    // The permit form is a service capability for verified applicants.
    assert!(template.capabilities.contains(&"submit_service_form"));
    assert_eq!(capability("submit_service_form").unwrap().min_trust, TrustTier::Verified);

    // Charter v1: the clerk owns and edits, the director publishes.
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Kaunti ya Nakuru",
        CLERK,
        "Serve the county's residents.",
        vec!["en".parse().unwrap(), "sw".parse().unwrap()],
        "KES",
    );
    inputs.people.push(RoleHolder { principal: DIRECTOR.into(), roles: vec![Role::Publisher] });
    let charter = template.draft_charter(&site.team, &inputs);
    charter.validate().unwrap();
    site.commit(&charter, &[(Role::Owner, CLERK)]).await;

    // The launch gate fails while a page exists in one official language only.
    let steward = AgentRole::Steward.principal(&site.site);
    let plan = SitePlan::new(&charter, "Kaunti ya Nakuru");
    let all = plan.items(&charter, &[], &["KE".to_string()]);
    let english: Vec<ContentItem> =
        all.iter().filter(|i| i.language.as_str() == "en").cloned().collect();
    let english_only = stage_release_one(&site.runtime, english, &steward).await.unwrap();
    assert!(english_only.written.is_empty(), "every page waits for its Swahili version");
    let languages = english_only.gate.check(GateCheck::RequiredLanguages).unwrap();
    assert!(!languages.findings.is_empty(), "{}", english_only.gate.summary());
    assert!(!site.runtime.run_launch_gate().await.unwrap().passed(), "nothing to launch yet");
    let both = stage_release_one(&site.runtime, all.clone(), &steward).await.unwrap();
    assert!(both.held.is_empty(), "{}", both.gate.summary());
    let report = site.runtime.run_launch_gate().await.unwrap();
    assert!(report.passed(), "{}", report.summary());

    // No third-party script ships: a theme that loads a tracker fails the kind's check.
    let mut project = SiteProject::new(
        site.base.config.clone(),
        all.iter()
            .cloned()
            .map(|mut i| {
                i.state = ContentState::Published;
                i
            })
            .collect(),
        chrono::Utc::now(),
    );
    let base = project.theme.templates["base.html"].clone();
    project.theme.templates.insert(
        "base.html".into(),
        base.replacen(
            "</head>",
            "<script src=\"https://tracker.example/t.js\"></script></head>",
            1,
        ),
    );
    let out = tempfile::tempdir().unwrap();
    let built = BuiltinGenerator.build(&project, out.path()).await.unwrap();
    let gate =
        LaunchGate::new().with_extension(Arc::new(ThirdPartyScripts::new("https://nakuru.go.ke")));
    let verdict = gate
        .run(
            GateMode::Full,
            &GateInput {
                project: &project,
                out: out.path(),
                report: &built,
                changed: None,
                quarantine: &Quarantine::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        verdict.checks.iter().any(|c| c.findings.iter().any(|f| f.rule == "third-party-resource")),
        "{}",
        verdict.summary()
    );
    assert!(!verdict.passed());

    // A notice the editor proposes publishes only after the publisher approves (A-I23).
    let pipeline = site.runtime.pipeline();
    let change = Change {
        items: vec![
            notice("water-rationing", "en", "Water rationing from Monday"),
            notice("water-rationing", "sw", "Mgao wa maji kuanzia Jumatatu"),
        ],
        media: Vec::new(),
        entry: EntryPoint::PairedDevice { device: "clerk-phone".into() },
        proposer: steward.clone(),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let draft = pipeline.draft(change).await.unwrap();
    let preview = pipeline.preview(&draft).await.unwrap();
    assert!(preview.gate.passed(), "{}", preview.gate.summary());
    let ticket = preview.staged.digest.clone();
    assert!(
        matches!(
            pipeline.approve(&draft, &preview).await.unwrap(),
            ApprovalDecision::Pending { .. }
        ),
        "nobody approved yet"
    );
    let approvals = site.runtime.approvals();
    approvals
        .approve(
            &ticket,
            RoleApproval::new(Role::Editor, SubjectRef::human(CLERK), chrono::Utc::now()),
        )
        .await;
    assert!(
        matches!(
            pipeline.approve(&draft, &preview).await.unwrap(),
            ApprovalDecision::Pending { .. }
        ),
        "the editor alone is not the chain"
    );
    approvals
        .approve(
            &ticket,
            RoleApproval::new(Role::Publisher, SubjectRef::human(DIRECTOR), chrono::Utc::now()),
        )
        .await;
    let ApprovalDecision::Approved { approvers, decision } =
        pipeline.approve(&draft, &preview).await.unwrap()
    else {
        panic!("the editor and the publisher, two people, satisfy the chain");
    };
    let people: BTreeSet<&str> = approvers.iter().map(String::as_str).collect();
    assert!(people.contains(CLERK) && people.contains(DIRECTOR), "{approvers:?}");
    let outcome = pipeline.complete(&draft, decision).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)));
    assert_eq!(verify_served(&site.host, "nakuru.go.ke", &site.release_key).await, 1);
}
