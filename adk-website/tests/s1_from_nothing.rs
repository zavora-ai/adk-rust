//! S1 — from nothing to live (spec 3.5 §5 S1, §7.17, §7.18, A-I14, A-I18, A-I19, A-I20,
//! A-I21): a scripted owner pairs with the setup code, answers the interview while the live
//! preview rebuilds, signs charter version 1 drafted from the business template, picks a
//! design direction and approves the launch. Release 1 is signed on a `DirectoryHost` and
//! verifies from the published files alone.
//!
//! Pass condition: charter v1 signed with every KPI carrying a baseline source and no target;
//! every page maps to a charter goal; zero published claims from `Assumed` facts; launch gate
//! green; release 1 signed and verified. `awp verify` of the full contract runs with the edge.

mod common;

use std::sync::Arc;

use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_harness::website::CONNECTION_NOTICE;
use adk_harness::{FactFilter, FactStatus, FactStore};
use adk_kernel::charter::{CharterStore, RoleApproval};
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_model::ScriptedResponse;
use adk_site::content::ContentStore;
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::{
    LivePreview, SitePlan, known_facts, page_goal, stage_release_one,
};
use adk_website::onboarding::discovery::Discovery;
use adk_website::onboarding::{
    OnboardingProgress, RunPlan, SetupGate, Stage, StartingPoint, draft_charter, plan,
};
use adk_website::team::AgentRole;
use adk_website::themes::ThemeId;
use awp_types::{Channel, ContentState, Role, SiteKind};
use common::*;
use serde_json::json;

#[tokio::test]
async fn s1_from_nothing_to_a_signed_verified_release() {
    let mut site =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let trail = AuditTrail::new(Arc::new(site.audit.clone()));
    let owner = user(OWNER);

    // A-I20: a fresh deployment takes no onboarding input until the setup code pairs a device.
    let gate = SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), OWNER);
    assert!(gate.admit(None).await.is_err());
    assert!(gate.admit(Some("adkd_guess_secret")).await.is_err());
    let issued = gate.issue().await.unwrap();
    let paired = gate.claim(&issued.code, "Amina's phone").await.unwrap();
    assert!(gate.claim(&issued.code, "a stranger").await.is_err(), "a code pairs one device");
    let token = paired.token.clone();
    let admit = || async { gate.admit(Some(&token)).await.expect("the paired device is admitted") };

    // 1 Start: from nothing.
    let start = StartingPoint::Nothing;
    let progress = OnboardingProgress::start(site.site.clone(), &start, Some(trail.clone())).await;
    let steps = plan(&start, Some(&site.template));
    assert!(steps.iter().any(|s| s.id == "connect" && s.instruction.contains(CONNECTION_NOTICE)));
    progress.complete(Stage::Start, None).await;

    // 2 Discover: nothing to read; the owner names the kind. 3 Protect: nothing to protect.
    let discovered = Discovery::new(start.clone(), "en".parse().unwrap())
        .unwrap()
        .run(&source_context("juniper.example"))
        .await
        .unwrap();
    assert!(discovered.inventory.is_none() && discovered.cards().is_empty());
    progress.complete(Stage::Discover, None).await;
    progress.complete(Stage::Protect, None).await;

    // 4 Interview, led by the Steward under its onboarding job; the preview rebuilds per answer.
    adk_website::jobs::register_onboarding(&site.ladder, site.site.id).await.unwrap();
    let mut inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        OWNER,
        "Fresh bread for Nakuru every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    inputs.p2_channels = vec![Channel::Whatsapp];
    inputs.contact_email = Some("hello@juniper.example".into());
    let draft = draft_charter(&site.template, &site.team, &inputs, Some(&trail)).await;
    let prompter = ScriptedPrompter::new();
    prompter.approve_once();
    let levels = adk_harness::website::job_levels(
        &site.compiler(&draft),
        site.autonomy.as_ref(),
        site.site.id,
    )
    .await
    .unwrap();
    let mode = site.compiler(&draft).mode_for_agent("steward", &levels);
    assert_eq!(mode, adk_harness::HarnessMode::Ask, "onboarding acts with the owner's approval");
    let (steward, model) = site.back_office(
        "steward",
        &draft,
        vec![
            call("interview_record", json!({ "text": "Juniper Bakery bakes rye and sourdough every morning", "quote": "we bake rye and sourdough every morning" })),
            ScriptedResponse::text("Noted. When do you open?"),
        ],
        |b| b.mode(mode).prompter(prompter.clone()),
    );
    let session = steward.open_session(&owner, None).await.unwrap();
    let preview = LivePreview::new(
        site.runtime.clone(),
        site.facts.clone(),
        site.partitions.fact_scope(OWNER),
        "Juniper Bakery",
        AgentRole::Steward.principal(&site.site),
    );
    admit().await;
    let first = collect(
        steward
            .send(&owner, &session, "We're in Nakuru and we bake rye and sourdough every morning.")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(response(&first, "interview_record", 0)["status"], "known");
    let after_one = preview.refresh(&draft).await.unwrap();
    assert_eq!(after_one.facts, 1);
    assert!(after_one.published, "the preview is on the host");
    assert!(after_one.about.as_deref().unwrap_or_default().contains("rye and sourdough"));

    model.push(call("interview_record", json!({ "text": "Juniper Bakery opens at seven every weekday", "quote": "We open at seven every weekday" })));
    model.push(call(
        "interview_record",
        json!({ "text": "Juniper Bakery was founded in 1990", "quote": "founded in 1990" }),
    ));
    model.push(ScriptedResponse::text("Thank you."));
    admit().await;
    let second =
        collect(steward.send(&owner, &session, "We open at seven every weekday.").await.unwrap())
            .await;
    assert_eq!(response(&second, "interview_record", 0)["status"], "known");
    assert_eq!(response(&second, "interview_record", 1)["status"], "assumed", "never said");
    let after_two = preview.refresh(&draft).await.unwrap();
    assert_eq!(after_two.facts, 2, "the preview shows only Known facts");
    assert_ne!(after_two.digest, after_one.digest, "each answer rebuilds the preview");
    progress.complete(Stage::Interview, None).await;

    // 5 Charter: drafted from the template, requested as a card, committed when the owner signs.
    model.push(call("charter_draft", json!({ "charter": draft })));
    model.push(call("charter_commit_request", json!({ "draft": "charter-1" })));
    model.push(ScriptedResponse::text("Please sign charter version 1."));
    admit().await;
    let drafted =
        collect(steward.send(&owner, &session, "Draft the charter.").await.unwrap()).await;
    let request = response(&drafted, "charter_commit_request", 0);
    assert_eq!(request["committed"], false, "{request}");
    assert!(
        site.charters.current(site.site.id).await.unwrap().is_none(),
        "nothing commits before the owner signs"
    );
    let signed = site
        .runtime
        .sign_charter(
            request["request"].as_str().unwrap(),
            RoleApproval::new(Role::Owner, SubjectRef::human(OWNER), chrono::Utc::now()),
            "dec-charter-1",
        )
        .await
        .unwrap()
        .expect("the owner is every required signer");
    let charter = signed.charter.clone();
    assert_eq!(signed.version(), 1);
    adk_website::jobs::register(&site.ladder, &charter).await.unwrap();
    for kpi in &charter.kpis {
        assert!(kpi.target.is_none(), "A-I14: {} has no target before a baseline", kpi.id);
        assert!(!kpi.source.to_string().is_empty(), "{} names its baseline source", kpi.id);
    }
    progress.complete(Stage::Charter, None).await;

    // 6 Build: the owner picks the harbour direction; release 1 is staged from Known facts.
    site.runtime = site.assemble(Some(ThemeId::Harbour));
    let known = known_facts(site.facts.as_ref(), &site.partitions.fact_scope(OWNER)).await.unwrap();
    assert_eq!(known.len(), 2);
    let site_plan = SitePlan::new(&charter, "Juniper Bakery");
    assert!(site_plan.unmapped(&charter).is_empty());
    let staged = stage_release_one(
        &site.runtime,
        site_plan.items(&charter, &known, &["KE".to_string()]),
        &AgentRole::Steward.principal(&site.site),
    )
    .await
    .unwrap();
    assert!(staged.held.is_empty(), "{}", staged.gate.summary());
    progress.complete(Stage::Build, None).await;

    // 7 Launch gate, then 8 the launch: held for the owner even though onboarding acts
    // with approval (A-I21).
    let launch_prompter = ScriptedPrompter::new();
    launch_prompter.approve_once();
    let (launcher, launch_model) = site.back_office(
        "steward",
        &charter,
        vec![
            call("launch_gate_run", json!({})),
            call("content_edit", json!({ "id": "pages/home/en", "fields": { "summary": "Fresh bread for Nakuru every morning." }, "job": "onboarding" })),
            ScriptedResponse::text("Ready to launch."),
        ],
        |b| b.mode(adk_harness::HarnessMode::Ask).prompter(launch_prompter.clone()),
    );
    let launch_session = launcher.open_session(&owner, None).await.unwrap();
    admit().await;
    let gated =
        collect(launcher.send(&owner, &launch_session, "Run the launch gate.").await.unwrap())
            .await;
    let verdict = response(&gated, "launch_gate_run", 0);
    assert_eq!(verdict["passed"], true, "{}", verdict["summary"]);
    progress.complete(Stage::LaunchGate, None).await;
    let draft_id = response(&gated, "content_edit", 0)["draft"].as_str().unwrap().to_string();
    launch_model.push(call("site_publish", json!({ "draft": draft_id })));
    launch_model.push(ScriptedResponse::text("Live."));
    admit().await;
    let launched = collect(launcher.send(&owner, &launch_session, "Go live.").await.unwrap()).await;
    let live = response(&launched, "site_publish", 0);
    assert_eq!(live["published"], true, "{live}");
    assert_eq!(launch_prompter.held(), ["site_publish"], "the launch is the owner's decision");
    let release = verify_served(&site.host, "juniper.example", &site.release_key).await;
    assert_eq!(release, 1, "release 1 is signed and verifies");
    progress.complete(Stage::ConnectAndDeploy, Some(release)).await;

    // Every page maps to a charter goal; no claim rests on an Assumed fact (A-I18).
    let assumed: Vec<String> = site
        .facts
        .list(FactFilter::user(&site.site.name, OWNER))
        .await
        .unwrap()
        .into_iter()
        .filter(|fact| fact.status == FactStatus::Assumed)
        .map(|fact| fact.id)
        .collect();
    assert_eq!(assumed.len(), 1);
    let goals: Vec<&str> = charter.goals.iter().map(|g| g.id.as_str()).collect();
    let items = site.store.list().await.unwrap();
    assert!(items.len() >= 7, "home, about, the goal pages and the legal pages");
    for item in &items {
        assert_eq!(item.state, ContentState::Published);
        let goal = page_goal(item).unwrap_or_else(|| panic!("{} names no goal", item.id));
        assert!(goals.contains(&goal), "{} serves {goal}, which the charter lacks", item.id);
        for cited in &item.sources {
            assert!(!assumed.contains(&cited.0), "{} claims an Assumed fact", item.id);
        }
    }

    // 9 Run: the 30/60/90-day plan; the first review on day 7; promotions are offers only.
    let launch = chrono::Utc::now();
    let run = RunPlan::new(&charter, launch);
    assert_eq!(run.first_review - launch, chrono::Duration::days(7));
    assert!(run.promotions.iter().any(|(job, _)| job == "protect"), "{:?}", run.promotions);
    for (job, _) in &run.promotions {
        let level = site.ladder.level(site.site.id, job).await.unwrap();
        assert!(
            level.is_some_and(|l| adk_kernel::charter::ladder_rank(l)
                <= adk_kernel::charter::ladder_rank(awp_types::AutonomyLevel::Propose)),
            "{job} works at Propose for the first week"
        );
    }
    progress.complete(Stage::Run, Some(release)).await;
    assert_eq!(progress.next().await, None);

    let actions = site.actions();
    for action in [
        "website.onboarding.started",
        "website.charter.drafted",
        "website.charter.committed",
        "website.onboarding.stage",
        "website.onboarding.completed",
    ] {
        assert!(actions.iter().any(|a| a == action), "{action} in {actions:?}");
    }
}
