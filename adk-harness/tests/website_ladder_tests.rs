//! The website tool catalogue and the autonomy ladder compiled to harness policy (spec 3.5
//! §7.10, §7.11, D-7, A-I13): each tool's effect, class and holds; the §7.11 table as tests;
//! a higher rung never decides stricter than a lower one; promotion only by an owner's
//! decision, which creates the standing rules; demotion applied at the next call with its
//! reason, and the rules it no longer warrants revoked.

#![cfg(feature = "website")]

use crate::website_common;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{ActingContext, PolicyDecision, ToolEffect, ToolPolicyRequest};
use adk_harness::website::{
    CATALOG, Hold, LevelDecision, ToolSpec, WebsiteClasses, WebsitePartition, apply_demotion,
    job_levels, level_decision, mode_for_level, promote_job, spec,
};
use adk_harness::{HarnessMode, OriginClasses, StandingRuleStore, ToolKind};
use adk_kernel::charter::{AutonomyStore, OwnerDecision};
use adk_kernel::drivers::EffectClass;
use awp_types::AutonomyLevel;
use proptest::prelude::*;
use serde_json::json;
use website_common::{OWNER, World, owner_decision};

const LADDER: [AutonomyLevel; 5] = [
    AutonomyLevel::Observe,
    AutonomyLevel::Propose,
    AutonomyLevel::ActWithApproval,
    AutonomyLevel::ActReversibleOnly,
    AutonomyLevel::ActWithinBudget,
];

#[test]
fn every_tool_has_its_effect_class_and_holds() {
    use EffectClass::{Financial, PublishPublic, Read, SendExternal, WriteLocal};
    use ToolEffect::{Idempotent, NonIdempotent, ReadOnly};
    use WebsitePartition::{BackOffice, Public};
    // (name, partition, effect, class, draft, reversible, hold)
    let expected: Vec<(&str, WebsitePartition, ToolEffect, EffectClass, bool, bool, Hold)> = vec![
        ("content_draft", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("content_edit", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("translation_draft", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("media_intake", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("charter_draft", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("release_preview", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("site_discover", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("interview_record", BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
        ("content_schedule", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
        ("content_correct", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
        ("content_withdraw", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
        ("content_revert", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
        ("site_publish", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::AtLaunch),
        ("site_rollback", BackOffice, NonIdempotent, PublishPublic, false, true, Hold::Ladder),
        ("host_fetch", BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
        ("launch_gate_run", BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
        ("awp_verify", BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
        (
            "charter_commit_request",
            BackOffice,
            NonIdempotent,
            WriteLocal,
            false,
            false,
            Hold::Always,
        ),
        (
            "job_promotion_request",
            BackOffice,
            NonIdempotent,
            WriteLocal,
            false,
            false,
            Hold::Always,
        ),
        ("wallet_pay", BackOffice, NonIdempotent, Financial, false, false, Hold::Ladder),
        ("edge_read_only", BackOffice, Idempotent, PublishPublic, false, true, Hold::Ladder),
        ("experiment_start", BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
        ("experiment_report", BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
        ("lead_handoff", BackOffice, NonIdempotent, WriteLocal, false, false, Hold::Ladder),
        ("search_site", Public, ReadOnly, Read, false, false, Hold::Ladder),
        ("reply_in_thread", Public, NonIdempotent, SendExternal, false, false, Hold::Ladder),
        ("emit_signal", Public, NonIdempotent, WriteLocal, false, false, Hold::Ladder),
    ];
    let actual: Vec<_> = CATALOG
        .iter()
        .map(|s| (s.name, s.partition, s.effect, s.class, s.draft, s.reversible, s.hold))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn the_harness_classes_website_tools_by_the_catalogue() {
    let classes = WebsiteClasses;
    let class = |source: &str, tool: &str| classes.class_of(source, tool);
    let trusted = |effect| Some(ToolKind::External { effect, trusted_origin: true });
    assert_eq!(class("website:back-office", "host_fetch"), trusted(ToolEffect::ReadOnly));
    assert_eq!(class("website:back-office", "content_draft"), Some(ToolKind::Todo));
    assert_eq!(class("website:back-office", "site_publish"), trusted(ToolEffect::NonIdempotent));
    assert_eq!(
        class("website:back-office", "charter_commit_request"),
        Some(ToolKind::Todo),
        "a decision card"
    );
    assert_eq!(class("website:public", "search_site"), trusted(ToolEffect::ReadOnly));
    assert_eq!(class("website:public", "emit_signal"), Some(ToolKind::Todo));
    assert_eq!(class("adk-drivers:reply", "reply_in_thread"), Some(ToolKind::Todo));
    assert_eq!(class("website:public", "get_quote"), Some(ToolKind::Todo), "a declared capability");
    // A back-office tool published as public, or a lookalike from another origin, is not.
    assert_eq!(class("website:public", "site_publish"), None);
    assert_eq!(class("mcp:evil", "site_publish"), None);
    assert_eq!(class("adk-drivers:reply", "site_publish"), None);
}

/// The §7.11 table, row by row: read-only, draft, reversible and other tools at each level.
#[test]
fn the_autonomy_ladder_table() {
    let read = spec("host_fetch");
    let draft = spec("content_draft");
    let reversible = spec("site_rollback");
    let other = spec("site_publish");
    let short = |decision: LevelDecision| match decision {
        LevelDecision::Allow => "allow",
        LevelDecision::Deny(_) => "deny",
        LevelDecision::Hold(_) => "hold",
        LevelDecision::WithinBudget => "budget",
    };
    let row = |level| {
        [read, draft, reversible, other]
            .map(|tool| short(level_decision(level, tool, ToolEffect::NonIdempotent)))
    };
    assert_eq!(row(AutonomyLevel::Observe), ["allow", "deny", "deny", "deny"]);
    assert_eq!(row(AutonomyLevel::Propose), ["allow", "allow", "deny", "deny"]);
    assert_eq!(row(AutonomyLevel::ActWithApproval), ["allow", "allow", "hold", "hold"]);
    assert_eq!(row(AutonomyLevel::ActReversibleOnly), ["allow", "allow", "allow", "hold"]);
    assert_eq!(row(AutonomyLevel::ActWithinBudget), ["allow", "allow", "allow", "budget"]);
    assert_eq!(
        LADDER.map(mode_for_level),
        [
            HarnessMode::ReadOnly,
            HarnessMode::Plan,
            HarnessMode::Ask,
            HarnessMode::Auto,
            HarnessMode::Auto
        ]
    );
    // A tool outside the catalogue is decided by its declared effect.
    assert_eq!(
        level_decision(AutonomyLevel::Observe, None, ToolEffect::ReadOnly),
        LevelDecision::Allow
    );
    assert_eq!(
        short(level_decision(AutonomyLevel::Propose, None, ToolEffect::NonIdempotent)),
        "deny"
    );
}

fn rank(decision: &LevelDecision) -> u8 {
    match decision {
        LevelDecision::Allow => 0,
        LevelDecision::WithinBudget => 1,
        LevelDecision::Hold(_) => 2,
        LevelDecision::Deny(_) => 3,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A higher rung never decides a call more strictly than a lower one (spec 3.5 §7.11).
    #[test]
    fn a_higher_rung_is_never_stricter(
        tool in 0usize..CATALOG.len(),
        low in 0usize..5,
        high in 0usize..5,
    ) {
        prop_assume!(low <= high);
        let spec: &ToolSpec = &CATALOG[tool];
        let lower = level_decision(LADDER[low], Some(spec), spec.effect);
        let higher = level_decision(LADDER[high], Some(spec), spec.effect);
        prop_assert!(rank(&higher) <= rank(&lower), "{} at {:?} is {:?}, at {:?} is {:?}", spec.name, LADDER[high], higher, LADDER[low], lower);
    }
}

fn call(world: &World, agent: &str, tool: &str, args: serde_json::Value) -> ToolPolicyRequest {
    let effect = spec(tool).map_or(ToolEffect::NonIdempotent, |s| s.effect);
    ToolPolicyRequest::new(tool, args)
        .with_effect(effect)
        .with_agent_name(agent)
        .with_acting_context(ActingContext::derived(
            &world.site.name,
            agent,
            OWNER,
            adk_core::principal::SubjectKind::Human,
        ))
}

#[tokio::test]
async fn the_ladder_policy_reads_each_jobs_level_at_every_call() {
    let world = World::new(true, false).await;
    let pauses = Arc::new(world.compiler.job_pauses());
    let policy = world.compiler.policy(world.autonomy.clone(), pauses.clone());
    // freshness (growth) is at Propose: drafts run, publication is refused.
    assert_eq!(
        policy.evaluate(&call(&world, "growth", "content_draft", json!({}))).await,
        PolicyDecision::Allow
    );
    let publish = policy.evaluate(&call(&world, "growth", "site_publish", json!({}))).await;
    assert!(
        matches!(&publish, PolicyDecision::Deny { reason } if reason.contains("Propose")),
        "{publish:?}"
    );
    // launch (steward) acts with approval: publication is held.
    let held = policy.evaluate(&call(&world, "steward", "site_publish", json!({}))).await;
    assert!(matches!(held, PolicyDecision::RequireApproval { .. }), "{held:?}");
    // running-costs (finance) acts within budget: the charter's spend cap bounds a payment.
    let small =
        policy.evaluate(&call(&world, "finance", "wallet_pay", json!({ "amount": 4_000 }))).await;
    assert_eq!(small, PolicyDecision::Allow);
    let large =
        policy.evaluate(&call(&world, "finance", "wallet_pay", json!({ "amount": 9_000 }))).await;
    assert!(matches!(large, PolicyDecision::RequireApproval { .. }), "{large:?}");
    // A website tool no job of the agent covers is refused; other tools pass to the harness.
    let uncovered = policy.evaluate(&call(&world, "finance", "site_publish", json!({}))).await;
    assert!(matches!(uncovered, PolicyDecision::Deny { .. }), "{uncovered:?}");
    assert_eq!(
        policy.evaluate(&call(&world, "finance", "read_file", json!({}))).await,
        PolicyDecision::Allow
    );
    // A guardrail breach pauses the job before its next action (A-I15).
    use adk_kernel::charter::JobControl;
    pauses.pause(world.site.id, "running-costs", "spend anomaly").await.unwrap();
    let paused =
        policy.evaluate(&call(&world, "finance", "wallet_pay", json!({ "amount": 10 }))).await;
    assert!(
        matches!(&paused, PolicyDecision::Deny { reason } if reason.contains("paused")),
        "{paused:?}"
    );

    // A demotion applies to the next call, with its reason.
    world.ladder.guardrail_breach(world.site.id, "launch", "complaints").await.unwrap();
    let demoted = policy.evaluate(&call(&world, "steward", "site_publish", json!({}))).await;
    assert!(
        matches!(&demoted, PolicyDecision::Deny { reason } if reason.contains("Propose") && reason.contains("complaints guardrail")),
        "{demoted:?}"
    );
}

#[tokio::test]
async fn compiled_modes_follow_each_agents_loosest_job() {
    let world = World::new(true, false).await;
    let levels = job_levels(&world.compiler, world.autonomy.as_ref(), world.site.id).await.unwrap();
    assert_eq!(world.compiler.mode_for_agent("growth", &levels), HarnessMode::Plan);
    assert_eq!(world.compiler.mode_for_agent("steward", &levels), HarnessMode::Ask);
    assert_eq!(world.compiler.mode_for_agent("finance", &levels), HarnessMode::Auto);
    assert_eq!(world.compiler.mode_for_agent("nobody", &BTreeMap::new()), HarnessMode::Plan);
}

/// A-I13: only an owner's decision raises a job; the decision creates the standing rules.
#[tokio::test]
async fn promotion_without_an_owners_decision_is_refused() {
    let world = World::new(true, false).await;
    let (site, rules) = (world.site.id, world.rules.as_ref());
    let start = world.autonomy.get(site, "freshness").await.unwrap().unwrap().level;
    assert_eq!(start, AutonomyLevel::Propose);

    let agent = OwnerDecision::new(
        "d-agent",
        SubjectRef::human("urn:adk:agent:bakery/steward"),
        chrono::Utc::now(),
    );
    let service =
        OwnerDecision::new("d-svc", SubjectRef::service("kernel:review"), chrono::Utc::now());
    let stranger = OwnerDecision::new("d-x", SubjectRef::human("mallory"), chrono::Utc::now());
    let publisher = OwnerDecision::new(
        "d-pub",
        SubjectRef::human(website_common::PUBLISHER),
        chrono::Utc::now(),
    );
    for decision in [&agent, &service, &stranger, &publisher] {
        let refused =
            promote_job(&world.ladder, &world.compiler, rules, "freshness", decision, OWNER).await;
        assert!(refused.is_err(), "{decision:?} promoted freshness");
    }
    assert_eq!(
        world.autonomy.get(site, "freshness").await.unwrap().unwrap().level,
        AutonomyLevel::Propose
    );
    assert!(
        rules.list(&world.site.name, OWNER).await.unwrap().is_empty(),
        "no rule without an owner"
    );

    // By the owner: act with approval grants the rules of the job's acting tools — the ladder
    // still holds each of their calls — and act reversible only keeps only reversible acts.
    let first = promote_job(
        &world.ladder,
        &world.compiler,
        rules,
        "freshness",
        &owner_decision("d-1"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(first.job.level, AutonomyLevel::ActWithApproval);
    let granted: Vec<&str> = first.rules.iter().map(|rule| rule.tool.as_str()).collect();
    assert_eq!(granted, ["content_correct", "content_correct", "site_publish", "site_publish"]);
    assert!(first.rules.iter().all(|rule| rule.created_by == SubjectRef::human(OWNER)));
    let held = world
        .compiler
        .policy(world.autonomy.clone(), Arc::new(world.compiler.job_pauses()))
        .evaluate(&call(&world, "growth", "site_publish", json!({})))
        .await;
    assert!(matches!(held, PolicyDecision::RequireApproval { .. }), "{held:?}");
    let second = promote_job(
        &world.ladder,
        &world.compiler,
        rules,
        "freshness",
        &owner_decision("d-2"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(second.job.level, AutonomyLevel::ActReversibleOnly);
    assert!(second.rules.is_empty(), "freshness has no reversible tool that needs a rule");
    assert!(
        rules.list(&world.site.name, OWNER).await.unwrap().is_empty(),
        "the act-with-approval rules go: this level allows only reversible acts"
    );
    let third = promote_job(
        &world.ladder,
        &world.compiler,
        rules,
        "freshness",
        &owner_decision("d-3"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(third.job.level, AutonomyLevel::ActWithinBudget);
    let tools: Vec<&str> = third.rules.iter().map(|rule| rule.tool.as_str()).collect();
    assert_eq!(tools, ["content_correct", "content_correct", "site_publish", "site_publish"]);
    for rule in &third.rules {
        assert_eq!(rule.created_by, SubjectRef::human(OWNER), "the owner creates the rule");
        assert_eq!(rule.agent.as_deref(), Some("urn:adk:agent:bakery/growth"));
        assert!(rule.label.contains("d-3"));
    }
    assert_eq!(rules.list(&world.site.name, OWNER).await.unwrap().len(), 4);

    // The site lowers it; the rules the new level does not warrant are revoked.
    let lowered = world.ladder.lower(site, "freshness", "two complaints").await.unwrap().unwrap();
    let revoked = apply_demotion(&world.compiler, rules, "freshness", lowered.level).await.unwrap();
    assert_eq!(revoked.len(), 4);
    assert!(rules.list(&world.site.name, OWNER).await.unwrap().is_empty());
}

/// A-I11: at "act, reversible only" the owner's decision pre-authorises the two incident
/// acts — the rollback and the edge's read-only switch — and nothing else.
#[tokio::test]
async fn act_reversible_only_pre_authorises_the_incident_acts() {
    let world = World::new(true, false).await;
    let compiler = world.compiler.clone().job(
        adk_harness::website::JobBinding::new(
            "protect",
            "guardian",
            ["site_rollback", "edge_read_only", "host_fetch", "site_publish"],
        )
        .in_area("run-and-protect"),
    );
    world
        .ladder
        .register(world.site.id, "protect", AutonomyLevel::ActWithApproval, Some("run-and-protect"))
        .await
        .unwrap();
    let promoted = promote_job(
        &world.ladder,
        &compiler,
        world.rules.as_ref(),
        "protect",
        &owner_decision("d-protect"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(promoted.job.level, AutonomyLevel::ActReversibleOnly);
    let mut tools: Vec<&str> = promoted.rules.iter().map(|rule| rule.tool.as_str()).collect();
    tools.dedup();
    assert_eq!(tools, ["edge_read_only", "site_rollback"]);
    for rule in &promoted.rules {
        assert_eq!(
            rule.origin,
            adk_harness::OriginPin::AnyDeclaration {
                source: adk_harness::website::BACK_OFFICE_ORIGIN.into()
            }
        );
    }
    let policy = compiler.policy(world.autonomy.clone(), Arc::new(compiler.job_pauses()));
    let switch = |tool: &str| call(&world, "guardian", tool, json!({ "reason": "drift" }));
    assert_eq!(policy.evaluate(&switch("edge_read_only")).await, PolicyDecision::Allow);
    let publish = policy.evaluate(&switch("site_publish")).await;
    assert!(matches!(publish, PolicyDecision::RequireApproval { .. }), "{publish:?}");
}

/// The standing rules of a tool outside the catalogue pin the origin the agent is given.
#[tokio::test]
async fn a_drivers_standing_rule_pins_its_origin() {
    let world = World::new(true, false).await;
    struct Driver;
    #[async_trait::async_trait]
    impl adk_core::Tool for Driver {
        fn name(&self) -> &str {
            "email_send_sequence_step"
        }
        fn description(&self) -> &str {
            "sends"
        }
        fn origin(&self) -> &str {
            "adk-drivers:email/in-memory"
        }
        async fn execute(
            &self,
            _: Arc<dyn adk_core::ToolContext>,
            _: serde_json::Value,
        ) -> adk_core::Result<serde_json::Value> {
            Ok(json!({}))
        }
    }
    let tools: Vec<Arc<dyn adk_core::Tool>> = vec![Arc::new(Driver)];
    let compiler = world
        .compiler
        .clone()
        .job(adk_harness::website::JobBinding::new(
            "nurture",
            "revenue",
            ["email_send_sequence_step"],
        ))
        .with_tool_origins(&tools);
    world.ladder.register(world.site.id, "nurture", AutonomyLevel::Propose, None).await.unwrap();
    let promoted = promote_job(
        &world.ladder,
        &compiler,
        world.rules.as_ref(),
        "nurture",
        &owner_decision("d-nurture"),
        OWNER,
    )
    .await
    .unwrap();
    assert_eq!(promoted.rules.len(), 2, "one per subject kind");
    for rule in &promoted.rules {
        assert_eq!(
            rule.origin,
            adk_harness::OriginPin::AnyDeclaration { source: "adk-drivers:email/in-memory".into() }
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// The autonomy ladder never raises a level without an owner's decision (spec 3.5 §11):
    /// any sequence of breaches, rejections, incidents, lowerings and promotion attempts by
    /// agents and services leaves every job at or below where it started.
    #[test]
    fn the_ladder_never_rises_without_an_owner(ops in proptest::collection::vec(0u8..6, 1..12)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let world = World::new(true, false).await;
            let site = world.site.id;
            let start = world.autonomy.get(site, "launch").await.unwrap().unwrap().level;
            for (n, op) in ops.iter().enumerate() {
                let _ = match op {
                    0 => world.ladder.guardrail_breach(site, "launch", "g").await.map(|_| ()),
                    1 => world.ladder.lower(site, "launch", "lowered").await.map(|_| ()),
                    2 => world.ladder.incident(site, "account", "i").await.map(|_| ()),
                    3 => {
                        let agent = OwnerDecision::new(format!("a{n}"), SubjectRef::human("urn:adk:agent:bakery/steward"), chrono::Utc::now());
                        promote_job(&world.ladder, &world.compiler, world.rules.as_ref(), "launch", &agent, OWNER).await.map(|_| ()).map_err(|e| adk_kernel::KernelError::Config { reason: e.to_string() })
                    }
                    4 => {
                        let service = OwnerDecision::new(format!("s{n}"), SubjectRef::service("kernel"), chrono::Utc::now());
                        world.ladder.promote(site, "launch", &service).await.map(|_| ())
                    }
                    _ => {
                        let decision = adk_kernel::charter::JobDecision::new(site, "launch", format!("r{n}"), adk_kernel::charter::DecisionOutcome::Rejected, SubjectRef::human(OWNER), chrono::Utc::now());
                        world.ladder.record_decision(decision).await.map(|_| ())
                    }
                };
                let now = world.autonomy.get(site, "launch").await.unwrap().unwrap().level;
                assert!(adk_kernel::charter::ladder_rank(now) <= adk_kernel::charter::ladder_rank(start), "{now:?} above {start:?} after {ops:?}");
            }
            assert!(world.rules.list(&world.site.name, OWNER).await.unwrap().is_empty());
        });
    }
}
