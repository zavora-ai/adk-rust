//! The kernel's drivers in the harness (spec 3.2 §7.7, §10 HK1, K-I9, K-I14): every effect
//! class maps onto a trusted `External` class whatever the server's hints; a standing rule
//! pinned to a driver tool's declaration stops matching when the tool is redeclared, and
//! re-approving the driver carries the rule to the new declaration.
//!
//! The driver is this test binary run as a stdio MCP server.

// `adk-tool` (for `SimpleToolContext`) comes with `team`, `web` or `mcp`.
#![cfg(all(feature = "kernel", any(feature = "team", feature = "web", feature = "mcp")))]

use crate::common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::principal::SubjectKind;
use adk_core::{
    ActingContext, Llm, PolicyDecision, ReadonlyContext, SubjectRef, ToolEffect, ToolOrigin,
    ToolPolicyRequest,
};
use adk_harness::approvals::matcher::{RuleCall, RuleOutcome, evaluate_standing_rules};
use adk_harness::{
    HarnessAgent, HarnessMode, HarnessRulePins, InMemoryStandingRuleStore, OriginPin, RuleScope,
    StandingDecision, StandingRule, StandingRuleStore, TableRow, ToolKind, decision_cell,
    driver_tool_kind,
};
use adk_kernel::PartitionId;
use adk_kernel::drivers::{DriverManifest, DriverSupervisor, EffectClass, driver_source};
use adk_kernel::stores::InMemoryDriverStore;
use proptest::prelude::*;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

const SERVER_ENV: &str = "ADK_HARNESS_KERNEL_DRIVER_TEST_SERVER";
const DECLARATION_ENV: &str = "ADK_HARNESS_KERNEL_DRIVER_TEST_DECLARATION";

/// Notes: a read and a send, whose description the declaration file extends.
#[derive(Clone)]
struct Notes;

impl ServerHandler for Notes {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let variant = std::env::var(DECLARATION_ENV)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![
            // The server claims read-only; the manifest's class decides.
            Tool::new("send_note", format!("Sends a note.{variant}"), schema.clone())
                .with_annotations(ToolAnnotations::new().read_only(true)),
            Tool::new("read_note", "Reads a note.", schema),
        ]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
    }
}

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_driver() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = Notes.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

#[test]
fn every_effect_class_maps_to_a_trusted_external_class_by_the_manifest() {
    for class in EffectClass::ALL {
        let kind = driver_tool_kind(class);
        let effect = if class == EffectClass::Read {
            ToolEffect::ReadOnly
        } else {
            ToolEffect::NonIdempotent
        };
        assert_eq!(kind, ToolKind::External { effect, trusted_origin: true }, "{class}");
        let row = TableRow::of(&kind, false);
        let cells: Vec<_> = HarnessMode::ALL.iter().map(|mode| decision_cell(*mode, row)).collect();
        if class == EffectClass::Read {
            assert_eq!(row, TableRow::ExternalReadOnlyTrusted);
        } else {
            assert_eq!(row, TableRow::ExternalOther, "{class} is never a trusted read");
            assert!(kind.rule_eligible());
            assert!(
                cells.iter().all(|cell| *cell != adk_harness::ModeCell::Allow),
                "{class}: {cells:?}"
            );
        }
    }
}

/// The request the governed path makes for `tool` in session `s1`, by alice's agent.
fn request(tool: &str) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, json!({}))
        .with_agent_name("work")
        .with_identity("adk-rust", "alice", "s1")
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived(
            "adk-rust",
            "work",
            "alice",
            SubjectKind::Human,
        ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redeclared_driver_tool_stops_matching_its_rule_until_reapproved() {
    let dir = tempfile::tempdir().unwrap();
    let declaration = dir.path().join("declaration.txt");
    let rules: Arc<dyn StandingRuleStore> = Arc::new(InMemoryStandingRuleStore::new());
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .home(dir.path())
        .health_check_interval(Duration::from_millis(100))
        .rule_pins(Arc::new(HarnessRulePins::new("adk-rust", rules.clone())))
        .build();
    drivers.start().await.unwrap();
    let manifest = DriverManifest::from_toml(&format!(
        "id = \"notes\"\ncommand = {:?}\nargs = [\"kernel_driver_tests::stdio_driver\", \"--exact\", \"--ignored\", \"--quiet\", \"--test-threads=1\"]\n\
         sandbox = \"unsandboxed\"\nenv = {{ {SERVER_ENV} = \"1\", {DECLARATION_ENV} = {:?} }}\n\
         [tools]\nsend_note = \"send_external\"\nread_note = \"read\"\n",
        std::env::current_exe().unwrap().display().to_string(),
        declaration.display().to_string(),
    ))
    .unwrap();
    drivers.mount(manifest, "alice").await.unwrap();

    let model: Arc<dyn Llm> = Arc::new(adk_model::ScriptedLlm::new(
        "scripted",
        Vec::<adk_model::ScriptedResponse>::new(),
    ));
    let harness = HarnessAgent::builder("work")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(common::no_user_file())
        .mode(HarnessMode::Ask)
        .standing_rules(rules.clone())
        .kernel_drivers(&drivers, &PartitionId::default(), &["notes".to_string()])
        .build()
        .unwrap();
    let policy = harness.runtime().policy().clone();

    let ctx: Arc<dyn ReadonlyContext> = Arc::new(adk_tool::SimpleToolContext::new("work"));
    let toolset = drivers.toolset_for(&PartitionId::default(), &["notes".to_string()]);
    let origins = |tools: Vec<Arc<dyn adk_core::Tool>>| -> BTreeMap<String, ToolOrigin> {
        tools.iter().map(|tool| (tool.name().to_string(), ToolOrigin::of(tool.as_ref()))).collect()
    };
    let before = origins(toolset.tools(ctx.clone()).await.unwrap());
    let send = &before["send_note"];
    assert_eq!(send.source, driver_source("notes"));

    // The manifest classes the tools, not the server's `readOnlyHint`.
    let send_request = request("send_note").with_effect(ToolEffect::NonIdempotent);
    assert_eq!(
        policy.classify(&send_request, Some(send)),
        ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: true }
    );
    let read_request = request("read_note").with_effect(ToolEffect::ReadOnly);
    assert_eq!(
        policy.classify(&read_request, Some(&before["read_note"])),
        ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true }
    );
    assert_eq!(
        policy.decide(&read_request, Some(&before["read_note"]), Some("c0")).await,
        PolicyDecision::Allow
    );
    assert!(matches!(
        policy.decide(&send_request, Some(send), Some("c1")).await,
        PolicyDecision::RequireApproval { .. }
    ));

    // A standing rule pinned to the approved declaration allows the send.
    rules
        .grant(StandingRule {
            id: "send-notes".into(),
            scope: RuleScope::User {
                app_name: "adk-rust".into(),
                user_id: "alice".into(),
                subject_kind: SubjectKind::Human,
            },
            tool: "send_note".into(),
            origin: OriginPin::exact(send),
            agent: None,
            predicates: vec![],
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human("alice"),
            created_at: chrono::Utc::now(),
            expires_at: None,
            label: "send notes".into(),
            source_fingerprint: None,
        })
        .await
        .unwrap();
    assert_eq!(policy.decide(&send_request, Some(send), Some("c2")).await, PolicyDecision::Allow);
    let kernel_policy = drivers.policy();
    let call = |origin| adk_core::PolicyCall::new().with_origin(origin);
    assert_eq!(
        kernel_policy.evaluate_call(&send_request, &call(send)).await,
        PolicyDecision::Allow
    );

    // Redeclared: the rule no longer matches, and the kernel asks before every call.
    std::fs::write(&declaration, " It now also deletes the note.").unwrap();
    drivers.restart("notes").await.unwrap();
    let after = origins(toolset.tools(ctx.clone()).await.unwrap());
    let changed = &after["send_note"];
    assert_ne!(changed.declaration_digest, send.declaration_digest);
    assert!(matches!(
        policy.decide(&send_request, Some(changed), Some("c3")).await,
        PolicyDecision::RequireApproval { .. }
    ));
    assert!(matches!(
        kernel_policy.evaluate_call(&send_request, &call(changed)).await,
        PolicyDecision::RequireApproval { .. }
    ));
    assert_eq!(drivers.state("notes"), Some(adk_kernel::drivers::DriverState::NeedsReapproval));

    // Re-approved by alice: her rule follows the new declaration.
    drivers.reapprove("notes", "alice").await.unwrap();
    assert_eq!(
        policy.decide(&send_request, Some(changed), Some("c4")).await,
        PolicyDecision::Allow
    );
    assert_eq!(
        kernel_policy.evaluate_call(&send_request, &call(changed)).await,
        PolicyDecision::Allow
    );
    let rule = rules.list("adk-rust", "alice").await.unwrap().remove(0);
    assert_eq!(rule.origin, OriginPin::exact(changed));
    drivers.shutdown().await;
}

/// A declaration and annotations a driver tool could publish.
fn declaration() -> impl Strategy<Value = (Value, Option<Value>)> {
    ("[a-z_]{1,10}", ".{0,30}", proptest::option::of(any::<bool>())).prop_map(
        |(name, description, read_only)| {
            (
                json!({ "name": name, "description": description, "parameters": { "type": "object" } }),
                read_only.map(|flag| json!({ "readOnlyHint": flag })),
            )
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Any change to what a pinned driver tool declares stops its standing rules matching.
    #[test]
    fn any_declaration_mutation_stops_a_pinned_rule_matching(
        (declaration, annotations) in declaration(),
        which in 0u8..4,
        salt in "[a-z0-9]{1,6}",
    ) {
        let source = driver_source("notes");
        let tool = declaration["name"].as_str().unwrap().to_string();
        let pinned = ToolOrigin::new(&source, &declaration, annotations.as_ref());
        let (mut changed, mut changed_annotations) = (declaration.clone(), annotations.clone());
        match which {
            0 => changed["description"] = json!(format!("{} {salt}", declaration["description"].as_str().unwrap())),
            1 => changed["parameters"]["required"] = json!([salt.clone()]),
            2 => changed_annotations = Some(json!({ "readOnlyHint": annotations.as_ref().and_then(|a| a["readOnlyHint"].as_bool()).is_none_or(|flag| !flag) })),
            _ => changed[format!("x_{salt}")] = json!(true),
        }
        let observed = ToolOrigin::new(&source, &changed, changed_annotations.as_ref());
        let rule = StandingRule {
            id: "r".into(),
            scope: RuleScope::User { app_name: "adk-rust".into(), user_id: "alice".into(), subject_kind: SubjectKind::Human },
            tool: tool.clone(),
            origin: OriginPin::exact(&pinned),
            agent: None,
            predicates: vec![],
            decision: StandingDecision::Allow,
            created_by: SubjectRef::human("alice"),
            created_at: chrono::Utc::now(),
            expires_at: None,
            label: "r".into(),
            source_fingerprint: None,
        };
        let acting = ActingContext::derived("adk-rust", "work", "alice", SubjectKind::Human);
        let args = json!({});
        let workspace = std::path::Path::new("/");
        let evaluate = |origin: &ToolOrigin| {
            evaluate_standing_rules(
                std::slice::from_ref(&rule),
                &RuleCall {
                    app_name: "adk-rust",
                    user_id: "alice",
                    session_id: "s1",
                    acting: Some(&acting),
                    tool_name: &tool,
                    args: &args,
                    origin: Some(origin),
                    kind: driver_tool_kind(EffectClass::SendExternal),
                    workspace_root: workspace,
                },
                chrono::Utc::now(),
            )
        };
        let allowed = matches!(evaluate(&pinned).outcome, RuleOutcome::Allow { .. });
        prop_assert!(allowed, "the rule matches its pinned declaration");
        let stale = evaluate(&observed);
        let still_allowed = matches!(stale.outcome, RuleOutcome::Allow { .. });
        prop_assert!(!still_allowed, "the rule matched a changed declaration");
        prop_assert_eq!(stale.stale.len(), 1);
    }
}
