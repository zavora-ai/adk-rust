//! S1 — from nothing to live (spec 3.5 §5 S1, §7.17, §7.18, A-I14, A-I18, A-I19, A-I20,
//! A-I21).
//!
//! `adk-rust website new` with no source: the owner pairs with the setup code, answers the
//! Steward's interview while the live preview rebuilds, signs charter version 1 drafted from
//! the business template, picks a design direction and approves the launch.
//!
//! Pass condition: charter v1 signed with every KPI carrying a baseline source; every page
//! maps to a charter goal; zero published claims from `Assumed` facts; launch gate green;
//! release 1 signed; `awp verify` passes on the local deployment, and against the preview of
//! the next release in the launch gate.

use std::sync::Arc;

use adk_awp::signing::anchor::FixedTxtResolver;
use adk_awp::signing::jws::DocumentSigner;
use adk_awp::verify::Verifier;
use adk_awp::verify::fetch::RouterFetcher;
use adk_awp::verify::pins::InMemoryPinStore;
use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_harness::website::CONNECTION_NOTICE;
use adk_harness::{FactFilter, FactStatus, FactStore, HarnessMode};
use adk_kernel::charter::{CharterStore, RoleApproval};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_model::ScriptedResponse;
use adk_site::content::ContentStore;
use adk_site::gate::{CheckStatus, GateCheck, PreviewVerification};
use adk_website::contract::{ContractInputs, add_to_project, site_documents};
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::{
    LivePreview, SitePlan, known_facts, page_goal, stage_release_one,
};
use adk_website::onboarding::discovery::Discovery;
use adk_website::onboarding::{
    OnboardingProgress, RunPlan, SetupGate, Stage, StartingPoint, draft_charter, plan,
    steward_instruction,
};
use adk_website::team::AgentRole;
use adk_website::themes::ThemeId;
use anyhow::{Result, anyhow};
use awp_types::{Channel, ContentState, Role, SiteKind};
use serde_json::json;

use crate::checks::Checks;
use crate::fixture::edge::{edge, site_keys};
use crate::fixture::*;
use crate::live::Mode;

/// What the owner says in the interview.
const OWNER_TURNS: [&str; 2] = [
    "We're Juniper Bakery in Nakuru and we bake rye and sourdough every morning.",
    "We open at seven every weekday.",
];

/// Runs S1.
pub async fn run(mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await?;
    let trail = AuditTrail::new(Arc::new(site.audit.clone()));
    let owner = user(OWNER);

    // A-I20: a fresh deployment takes no onboarding input until the setup code pairs a device.
    let gate = SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), OWNER);
    let unpaired = gate.admit(None).await.is_err() && gate.admit(Some("adkd_guess")).await.is_err();
    let issued = gate.issue().await?;
    let paired = gate.claim(&issued.code, "Amina's phone").await?;
    let second = gate.claim(&issued.code, "a stranger").await.is_err();
    checks.check(
        "A-I20: no input before the setup code pairs one device",
        unpaired && second,
        "unpaired and guessed tokens refused; the code paired one device only",
    );
    let token = paired.token.clone();
    let admit = || async { gate.admit(Some(&token)).await.map_err(|e| anyhow!("{e}")) };

    // 1 Start, 2 Discover, 3 Protect: from nothing there is nothing to read or protect.
    let start = StartingPoint::Nothing;
    let progress = OnboardingProgress::start(site.site.clone(), &start, Some(trail.clone())).await;
    let steps = plan(&start, Some(&site.template));
    checks.check(
        "the plan's connect step carries the connection notice",
        steps.iter().any(|s| s.id == "connect" && s.instruction.contains(CONNECTION_NOTICE)),
        format!("{} steps", steps.len()),
    );
    progress.complete(Stage::Start, None).await;
    let discovered = Discovery::new(start.clone(), "en".parse()?)?
        .run(&source_context("juniper.example")?)
        .await?;
    checks.check(
        "discovery from nothing reads nothing",
        discovered.inventory.is_none() && discovered.cards().is_empty(),
        "no inventory, no owner cards",
    );
    progress.complete(Stage::Discover, None).await;
    progress.complete(Stage::Protect, None).await;

    // 4 Interview, led by the Steward under its onboarding job; the preview rebuilds per answer.
    adk_website::jobs::register_onboarding(&site.ladder, site.site.id).await?;
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread for Nakuru every morning.",
        vec!["en".parse()?],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    inputs.contact_email = Some("hello@juniper.example".into());
    let draft = draft_charter(&site.template, &site.team, &inputs, Some(&trail)).await;
    let levels = adk_harness::website::job_levels(
        &site.compiler(&draft),
        site.autonomy.as_ref(),
        site.site.id,
    )
    .await?;
    let steward_mode = site.compiler(&draft).mode_for_agent("steward", &levels);
    checks.check(
        "onboarding acts with the owner's approval",
        steward_mode == HarnessMode::Ask,
        format!("{steward_mode:?}"),
    );
    let interview = format!(
        "{}\n\nThe owner has paired their phone. Discovery found nothing to read. You are in \
         stage 4, the interview: record every fact the owner states with interview_record, \
         quoting the owner's exact words, before you reply; then ask one short next question.",
        steward_instruction("Juniper Bakery", &start, Some(&site.template))
    );
    let prompter = ScriptedPrompter::new();
    let (steward, model) = site.back_office(
        &mode,
        "steward",
        &draft,
        vec![
            call("interview_record", json!({ "text": "Juniper Bakery bakes rye and sourdough every morning", "quote": "we bake rye and sourdough every morning" })),
            ScriptedResponse::text("Noted. When do you open?"),
            call("interview_record", json!({ "text": "Juniper Bakery opens at seven every weekday", "quote": "We open at seven every weekday" })),
            call("interview_record", json!({ "text": "Juniper Bakery was founded in 1990", "quote": "founded in 1990" })),
            ScriptedResponse::text("Thank you."),
        ],
        |b| b.instruction(interview).mode(steward_mode).prompter(prompter.clone()),
    )?;
    let session = steward.open_session(&owner, None).await?;
    let preview = LivePreview::new(
        site.runtime.clone(),
        site.facts.clone(),
        site.partitions.fact_scope(OWNER),
        "Juniper Bakery",
        AgentRole::Steward.principal(&site.site),
    );
    let mut digests = Vec::new();
    let mut previews_grow = true;
    for turn in OWNER_TURNS {
        admit().await?;
        let events = collect(steward.send(&owner, &session, turn).await?).await?;
        if mode.is_live() {
            trace(&events);
        }
        let snapshot = preview.refresh(&draft).await?;
        previews_grow &= snapshot.published && digests.last() != Some(&snapshot.digest);
        digests.push(snapshot.digest.clone());
    }
    let facts = site.facts.list(FactFilter::user(&site.site.name, OWNER)).await?;
    let known: Vec<_> = facts.iter().filter(|f| f.status == FactStatus::Known).collect();
    let said = OWNER_TURNS.join(" ").to_lowercase();
    let quoted = known.iter().all(|fact| {
        fact.provenance.quote.as_deref().is_some_and(|quote| {
            said.contains(&quote.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        })
    });
    checks.check(
        "A-I18/A-I19: facts are Known only in the owner's own words",
        !known.is_empty() && quoted,
        format!("{} facts, {} known, each quoting the owner", facts.len(), known.len()),
    );
    if !mode.is_live() {
        checks.check(
            "an answer the owner never gave stays Assumed",
            facts.iter().any(|f| f.status == FactStatus::Assumed && f.text.contains("1990")),
            "\"founded in 1990\" was not said",
        );
    }
    let last = preview.refresh(&draft).await?;
    checks.check(
        "the live preview rebuilds per answer and shows only Known facts",
        previews_grow && last.facts == known.len(),
        format!("{} previews, {} known facts shown", digests.len(), last.facts),
    );
    progress.complete(Stage::Interview, None).await;

    // 5 Charter: drafted from the template, requested as a card, committed when the owner
    // signs. The tool use is mechanical, so it is scripted in both modes. The request is
    // held as every card is; the owner lets it through to become a card.
    prompter.approve_once();
    let (drafter, _) = site.back_office(
        &Mode::Scripted,
        "steward",
        &draft,
        vec![
            call("charter_draft", json!({ "charter": draft })),
            call("charter_commit_request", json!({ "draft": "charter-1" })),
            ScriptedResponse::text("Please sign charter version 1."),
        ],
        |b| b.mode(steward_mode).prompter(prompter.clone()),
    )?;
    let drafting = drafter.open_session(&owner, None).await?;
    admit().await?;
    let drafted = collect(drafter.send(&owner, &drafting, "Draft the charter.").await?).await?;
    let request = response(&drafted, "charter_commit_request", 0)
        .ok_or_else(|| anyhow!("no commit request: {:?}", responses(&drafted)))?;
    checks.check(
        "nothing commits before the owner signs",
        request["request"].is_string()
            && request["committed"] == false
            && site.charters.current(site.site.id).await?.is_none(),
        format!("signing request {}", request["request"]),
    );
    let signed = site
        .runtime
        .sign_charter(
            request["request"].as_str().unwrap_or_default(),
            RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
        )
        .await?
        .ok_or_else(|| anyhow!("the owner is every required signer"))?;
    let charter = signed.charter.clone();
    adk_website::jobs::register(&site.ladder, &charter).await?;
    let no_targets = charter.kpis.iter().all(|kpi| kpi.target.is_none());
    let sources = charter.kpis.iter().all(|kpi| !kpi.source.to_string().is_empty());
    checks.check(
        "charter v1 signed; every KPI carries a baseline source and no target (A-I14)",
        signed.version() == 1 && no_targets && sources,
        format!("version {}, {} KPIs", signed.version(), charter.kpis.len()),
    );
    let _ = model;
    progress.complete(Stage::Charter, None).await;

    // 6 Build: the root signs the contract over the charter's public terms; the owner picks
    // the harbour direction; release 1 is staged from Known facts.
    let keys = site_keys("juniper.example")?;
    let contract_inputs = ContractInputs {
        id: site.site.id,
        name: "Juniper Bakery".into(),
        domain: "juniper.example".into(),
        languages: vec!["en".parse()?],
        edge: Some(site.site.origin.clone()),
        release_seq: 1,
        terms: charter.public.clone(),
    };
    let (documents, contract) = site_documents(&keys, &contract_inputs, chrono::Utc::now()).await?;
    checks.check(
        "A-I5: the root key signs the contract",
        contract.proof.as_ref().is_some_and(|p| p.kid == keys.root.kid()),
        keys.root.kid(),
    );
    add_to_project(&mut site.base, documents, charter.public.clone());
    site.runtime = site.assemble(Some(ThemeId::Harbour));
    let known = known_facts(site.facts.as_ref(), &site.partitions.fact_scope(OWNER)).await?;
    let site_plan = SitePlan::new(&charter, "Juniper Bakery");
    checks.check(
        "every planned page maps to a charter goal",
        site_plan.unmapped(&charter).is_empty(),
        format!("{} pages", site_plan.pages.len()),
    );
    let staged = stage_release_one(
        &site.runtime,
        site_plan.items(&charter, &known, &["KE".to_string()]),
        &AgentRole::Steward.principal(&site.site),
    )
    .await?;
    checks.require(
        "release 1 passes the per-change checks",
        staged.held.is_empty(),
        staged.gate.summary(),
    )?;
    progress.complete(Stage::Build, None).await;

    // 7 Launch gate, then 8 the launch: held for the owner even though onboarding acts with
    // approval (A-I21).
    let launch_prompter = ScriptedPrompter::new();
    launch_prompter.approve_once();
    let (launcher, launch_model) = site.back_office(
        &mode,
        "steward",
        &charter,
        vec![
            call("launch_gate_run", json!({})),
            call("content_edit", json!({ "id": "pages/home/en", "fields": { "summary": "Fresh bread for Nakuru every morning." }, "job": "onboarding" })),
            ScriptedResponse::text("Ready to launch."),
        ],
        |b| {
            b.instruction(
                "You are the Steward launching Juniper Bakery's site. When asked to run the \
                 launch gate, call launch_gate_run. Then call content_edit on id pages/home/en \
                 with fields {\"summary\": \"Fresh bread for Nakuru every morning.\"} and job \
                 onboarding. When asked to go live, call site_publish with that draft's id.",
            )
            .mode(HarnessMode::Ask)
            .prompter(launch_prompter.clone())
        },
    )?;
    let launch_session = launcher.open_session(&owner, None).await?;
    admit().await?;
    let gated = collect(
        launcher
            .send(&owner, &launch_session, "Run the launch gate, then prepare the home page.")
            .await?,
    )
    .await?;
    if mode.is_live() {
        trace(&gated);
    }
    let verdict = response(&gated, "launch_gate_run", 0).unwrap_or_default();
    let gate_green = site.runtime.run_launch_gate().await?;
    checks.check(
        "launch gate green",
        gate_green.passed() && (verdict.is_null() || verdict["passed"] == true),
        gate_green.summary(),
    );
    progress.complete(Stage::LaunchGate, None).await;
    let draft_id = response(&gated, "content_edit", 0)
        .and_then(|r| r["draft"].as_str().map(str::to_string))
        .unwrap_or_default();
    launch_model.push(call("site_publish", json!({ "draft": draft_id })));
    launch_model.push(ScriptedResponse::text("Live."));
    admit().await?;
    let launched = collect(
        launcher.send(&owner, &launch_session, &format!("Go live with draft {draft_id}.")).await?,
    )
    .await?;
    if mode.is_live() {
        trace(&launched);
    }
    let live = response(&launched, "site_publish", 0).unwrap_or_default();
    checks.check(
        "A-I21: the launch is the owner's decision",
        live["published"] == true && launch_prompter.held() == ["site_publish"],
        format!("held {:?}, published {}", launch_prompter.held(), live["published"]),
    );
    let release = verify_served(&site.host, "juniper.example", &site.release_key).await;
    checks.require(
        "release 1 signed and verified from the host's files",
        matches!(release, Ok(1)),
        format!("{release:?}"),
    )?;

    // `awp verify` of the whole contract against the local deployment: the edge with the
    // release behind it, the root anchored by its `_awp-root` TXT record (A-I9).
    let site_edge = edge(&site, "juniper.example").await?;
    let (record, value) = keys.root_record("juniper.example");
    let verifier = Verifier::new(
        Arc::new(RouterFetcher::new(site_edge.router.clone())),
        Arc::new(FixedTxtResolver::default().with(&record, &value)),
        Arc::new(InMemoryPinStore::default()),
    );
    let report = verifier.verify("juniper.example").await;
    checks.check(
        "awp verify passes on the local deployment",
        report.failure.is_none()
            && report.release_seq == Some(1)
            && report.posture.a2a
            && report.posture.security_txt,
        format!(
            "failure {:?}, release {:?}, {} pages sampled",
            report.failure,
            report.release_seq,
            report.sampled.len()
        ),
    );
    progress.complete(Stage::ConnectAndDeploy, Some(1)).await;

    // From now on the launch gate's full tier runs `awp verify` against the preview of the
    // next release: the working copy staged over the host's log of release 1, the Agent Card
    // read from the edge (spec 3.5 §7.8.1 "Machine views").
    site.gate = site.gate.clone().with_extension(Arc::new(
        PreviewVerification::new(
            "juniper.example",
            site.host.clone(),
            Arc::new(FixedTxtResolver::default().with(&record, &value)),
        )
        .with_fetcher(Arc::new(RouterFetcher::new(site_edge.router.clone()))),
    ));
    let preview = site.assemble(Some(ThemeId::Harbour)).run_launch_gate().await?;
    let verified = preview.check(GateCheck::AwpVerify);
    checks.check(
        "awp verify passes against the preview of release 2",
        verified.is_some_and(|check| {
            check.status == CheckStatus::Passed
                && check.note.as_deref().is_some_and(|note| note.contains("release 2"))
        }),
        format!("{verified:?}"),
    );

    // Every published page maps to a goal; no claim rests on an Assumed fact (A-I18).
    let assumed: Vec<String> = site
        .facts
        .list(FactFilter::user(&site.site.name, OWNER))
        .await?
        .into_iter()
        .filter(|fact| fact.status == FactStatus::Assumed)
        .map(|fact| fact.id)
        .collect();
    let goals: Vec<&str> = charter.goals.iter().map(|g| g.id.as_str()).collect();
    let items = site.store.list().await?;
    let mapped = items.iter().all(|item| {
        item.state == ContentState::Published
            && page_goal(item).is_some_and(|goal| goals.contains(&goal))
    });
    let claims_from_assumed =
        items.iter().flat_map(|i| i.sources.iter()).filter(|s| assumed.contains(&s.0)).count();
    checks.check(
        "every page maps to a charter goal",
        mapped && items.len() >= 7,
        format!("{} published pages", items.len()),
    );
    checks.check(
        "zero published claims from Assumed facts",
        claims_from_assumed == 0,
        format!("{} Assumed facts, none cited", assumed.len()),
    );

    // 9 Run: the 30/60/90-day plan; the first review on day 7.
    let launch = chrono::Utc::now();
    let run = RunPlan::new(&charter, launch);
    checks.check(
        "the first weekly review is on day 7",
        run.first_review - launch == chrono::Duration::days(7),
        format!("{} promotion offers planned", run.promotions.len()),
    );
    progress.complete(Stage::Run, Some(1)).await;
    let actions = site.actions();
    let audited = [
        "website.onboarding.started",
        "website.charter.drafted",
        "website.charter.committed",
        "website.onboarding.completed",
    ]
    .iter()
    .all(|a| actions.iter().any(|b| b == a));
    checks.check(
        "onboarding is audited",
        audited && progress.next().await.is_none(),
        format!("{} audit records", actions.len()),
    );
    Ok(checks)
}
