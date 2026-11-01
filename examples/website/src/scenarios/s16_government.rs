//! S16 — a public body (spec 3.5 §5 S16, §7.16.1 Government, §7.6, §7.9, A-I23).
//!
//! A county's WordPress site moves to an AWP site of kind `government`: two official
//! languages, a permit-application form, a clerk who edits and a director who publishes.
//!
//! Pass condition: the launch gate fails until every page passes WCAG 2.2 AA in both
//! languages; no third-party script ships; the form becomes a service capability whose
//! applicant identity comes from an OIDC provider fixture; a release proposed by the editor
//! publishes only after the publisher approves (A-I23); no advertising, sales or wallet jobs
//! exist beyond hosting.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_site::content::SiteProject;
use adk_site::gate::{CheckStatus, GateCheck, GateInput, GateMode, LaunchGate, Quarantine};
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::pipeline::{ApprovalDecision, Change, EntryPoint, PipelineOutcome};
use adk_website::kinds::{CharterInputs, KindRule, ThirdPartyScripts};
use adk_website::onboarding::build::{SitePlan, stage_release_one};
use adk_website::team::AgentRole;
use anyhow::{Result, anyhow};
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, Markdown, Role, RoleHolder, SiteKind,
};
use axum::http::StatusCode;
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::edge::{APPLICANT, EdgeSite, edge};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "nakuru.go.ke";
const CLERK: &str = OWNER;
const DIRECTOR: &str = "director";

fn notice(slug: &str, language: &str, title: &str) -> ContentItem {
    ContentItem {
        id: ContentId(format!("notices/{slug}/{language}")),
        kind: ContentKind::Notice,
        slug: slug.into(),
        language: language.parse().expect("a language"),
        fields: [
            ("title".to_string(), json!(title)),
            ("summary".to_string(), json!(title)),
            ("goal".to_string(), json!("accuracy")),
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

/// Runs S16.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let site = Site::new(SiteKind::Government, "Kaunti ya Nakuru", DOMAIN, &["en", "sw"]).await?;
    let template = site.template.clone();
    checks.check(
        "the kind requires two people to publish, every official language, no third-party scripts and AI disclosure",
        [KindRule::TwoPersonPublication, KindRule::AllLanguagesRequired, KindRule::NoThirdPartyScripts, KindRule::AiDisclosureOnEveryReply]
            .iter()
            .all(|rule| template.has(rule))
            && site.team.public_instruction().contains("AI agent"),
        "government template",
    );
    let jobs: BTreeSet<&str> = template.jobs().iter().map(|job| job.id).collect();
    let absent = ["advertising", "sell", "nurture", "handoff", "running-costs", "cost-and-return"];
    checks.check(
        "no advertising, sales or wallet jobs exist beyond hosting",
        absent.iter().all(|j| !jobs.contains(j))
            && jobs.contains("hosting")
            && !site.team.runs(AgentRole::Revenue)
            && !site.team.runs(AgentRole::Finance),
        format!("jobs {jobs:?}"),
    );

    // Charter v1: the clerk owns and edits, the director publishes.
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Kaunti ya Nakuru",
        CLERK,
        "Serve the county's residents.",
        vec!["en".parse()?, "sw".parse()?],
        "KES",
    );
    inputs.people.push(RoleHolder { principal: DIRECTOR.into(), roles: vec![Role::Publisher] });
    let charter = template.draft_charter(&site.team, &inputs);
    charter.validate().map_err(|e| anyhow!("{e:?}"))?;
    site.commit(&charter, &[(Role::Owner, CLERK)]).await?;

    // The launch gate fails while a page exists in one official language only, then passes
    // with WCAG 2.2 AA in both.
    let steward = AgentRole::Steward.principal(&site.site);
    let all = SitePlan::new(&charter, "Kaunti ya Nakuru").items(&charter, &[], &["KE".to_string()]);
    let english: Vec<ContentItem> =
        all.iter().filter(|i| i.language.as_str() == "en").cloned().collect();
    let english_only = stage_release_one(&site.runtime, english, &steward).await?;
    let first = site.runtime.run_launch_gate().await?;
    let languages = english_only
        .gate
        .check(GateCheck::RequiredLanguages)
        .map(|c| c.findings.len())
        .unwrap_or(0);
    checks.check(
        "the launch gate fails while any page lacks its Swahili version",
        english_only.written.is_empty() && languages > 0 && !first.passed(),
        format!("{languages} language findings; {}", first.summary()),
    );
    let both = stage_release_one(&site.runtime, all.clone(), &steward).await?;
    let report = site.runtime.run_launch_gate().await?;
    checks.check(
        "every page passes WCAG 2.2 AA in both languages and the gate is green",
        both.held.is_empty()
            && report.passed()
            && report
                .check(GateCheck::Accessibility)
                .is_some_and(|c| c.status == CheckStatus::Passed)
            && report
                .check(GateCheck::RequiredLanguages)
                .is_some_and(|c| c.status == CheckStatus::Passed),
        format!("{} items in en and sw; {}", both.written.len(), report.summary()),
    );

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
    let base = project.theme.templates.get("base.html").cloned().unwrap_or_default();
    project.theme.templates.insert(
        "base.html".into(),
        base.replacen(
            "</head>",
            "<script src=\"https://tracker.example/t.js\"></script></head>",
            1,
        ),
    );
    let out = tempfile::tempdir()?;
    let built = BuiltinGenerator.build(&project, out.path()).await?;
    let verdict = LaunchGate::new()
        .with_extension(Arc::new(ThirdPartyScripts::new(&format!("https://{DOMAIN}"))))
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
        .await?;
    checks.check(
        "no third-party script ships",
        !verdict.passed()
            && verdict
                .checks
                .iter()
                .any(|c| c.findings.iter().any(|f| f.rule == "third-party-resource")),
        "a theme loading tracker.example fails the gate",
    );

    // A notice the clerk (editor) proposes publishes only after the director (publisher)
    // approves, each a distinct person (A-I23).
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
    let draft = pipeline.draft(change).await?;
    let preview = pipeline.preview(&draft).await?;
    let ticket = match pipeline.approve(&draft, &preview).await? {
        ApprovalDecision::Pending { ticket } => ticket,
        other => return Err(anyhow!("the release did not wait for its chain: {other:?}")),
    };
    // The chain is one request in the site's approval service; the charter says who holds
    // each role.
    let approvals = site.runtime.approvals();
    let request =
        approvals.get(&ticket).await?.ok_or_else(|| anyhow!("the release has no request"))?;
    let clerk = approvals.approve(&ticket, request.version, &SubjectRef::human(CLERK)).await?;
    let editor_only =
        matches!(pipeline.approve(&draft, &preview).await?, ApprovalDecision::Pending { .. });
    approvals.approve(&ticket, clerk.version, &SubjectRef::human(DIRECTOR)).await?;
    let decided = pipeline.approve(&draft, &preview).await?;
    let ApprovalDecision::Approved { approvers, decision } = decided else {
        return Err(anyhow!("the chain is not satisfied: {decided:?}"));
    };
    let outcome = pipeline.complete(&draft, decision).await?;
    let seq = match outcome {
        PipelineOutcome::Published(p) => p.staged.manifest.seq,
        other => return Err(anyhow!("not published: {other:?}")),
    };
    checks.check(
        "a release the editor proposes publishes only after the publisher approves (A-I23)",
        editor_only
            && approvers.iter().any(|a| a == CLERK)
            && approvers.iter().any(|a| a == DIRECTOR),
        format!(
            "pending, pending with the editor alone, then release {seq} approved by {approvers:?}"
        ),
    );
    verify_served(&site.host, DOMAIN, &site.release_key).await?;

    // The permit form is a service capability: a verified applicant from the national
    // identity provider can apply; nobody else sees it.
    let county = edge(&site, DOMAIN).await?;
    let (_, _, public) = county.send(county.get("/awp/v2/manifest")).await;
    let (_, _, verified) = county.send(county.applicant(county.get("/awp/v2/manifest"))).await;
    checks.check(
        "the form becomes a service capability for verified applicants only",
        !EdgeSite::names(&public).contains(&"submit_service_form".to_string())
            && verified["tier"] == "verified"
            && EdgeSite::names(&verified).contains(&"submit_service_form".to_string()),
        format!(
            "public sees {:?}; the applicant ({}) sees {:?}",
            EdgeSite::names(&public),
            verified["tier"],
            EdgeSite::names(&verified)
        ),
    );
    let grant = json!({ "scope": "service", "policyVersion": "1", "jurisdiction": "KE" });
    let (granted, _, receipt) =
        county.send(county.applicant(county.post("/awp/v2/consent", &grant))).await;
    let receipt = receipt["id"].as_str().unwrap_or_default().to_string();
    let application =
        json!({ "service": "building-permit", "fields": { "plot": "LR 4471/12", "storeys": 2 } });
    let (status, _, applied) = county
        .send(county.applicant(county.call(
            "submit_service_form",
            "permit-1",
            Some(&receipt),
            &application,
        )))
        .await;
    let (anonymous, _, _) = county
        .send(county.call("submit_service_form", "permit-2", Some(&receipt), &application))
        .await;
    let requester =
        format!("{:?}", county.audit.records().iter().map(|r| &r.ext).collect::<Vec<_>>());
    checks.check(
        "the applicant's identity comes from the OIDC provider fixture",
        granted == StatusCode::CREATED
            && status == StatusCode::OK
            && anonymous == StatusCode::NOT_FOUND
            && requester.contains(APPLICANT.issuer),
        format!(
            "applied {status} ({}); without the provider's token {anonymous}",
            applied["reference"]
        ),
    );
    Ok(checks)
}
