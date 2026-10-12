//! Agent identity through a run: every policy decision, held approval, tool context, and
//! spend record carries the acting agent's id; a redeploy keeps it; delegation extends the
//! chain.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ActingContext, AdkError, AgentId, AgentPrincipal, ApprovalScope, ApprovalStore, Content,
    DeclarativePolicy, InMemoryApprovalStore, InMemorySpendLedger, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, PolicyDecision, PolicyRule, Result, RunConfig, SessionId, SpendKey,
    SpendLedger, SpendPeriod, StaticIdentitySource, SubjectRef, Tool, ToolContext, ToolPolicy,
    ToolPolicyRequest, UsageMetadata, UserId,
};
use adk_runner::{LlmSpendEstimate, Runner};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::AgentTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const APP: &str = "acme";
const AGENT_ID: &str = "spiffe://acme.test/agent/billing";

/// Replays scripted responses, each reporting a cost of one cent.
struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()) })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "gemini-3.7-flash"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let mut response =
            self.responses.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
                content: Some(Content::new("model").with_text("done")),
                turn_complete: true,
                ..Default::default()
            });
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 10,
            candidates_token_count: 5,
            total_token_count: 15,
            cost: Some(0.01),
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn call(tool: &str, args: Value, id: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: tool.to_string(),
                args,
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

/// Records the acting context of every execution.
struct LookupTool {
    seen: Arc<Mutex<Vec<ActingContext>>>,
}

#[async_trait]
impl Tool for LookupTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks up an invoice."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.seen.lock().unwrap().push(ctx.acting_context());
        Ok(json!({ "status": "paid" }))
    }
}

/// Wraps a declarative policy and records every request it decides.
#[derive(Debug)]
struct RecordingPolicy {
    inner: DeclarativePolicy,
    seen: Mutex<Vec<ToolPolicyRequest>>,
}

impl RecordingPolicy {
    fn new(inner: DeclarativePolicy) -> Arc<Self> {
        Arc::new(Self { inner, seen: Mutex::new(Vec::new()) })
    }

    fn acting(&self) -> Vec<ActingContext> {
        self.seen.lock().unwrap().iter().map(|request| request.acting.clone().unwrap()).collect()
    }

    fn invocations(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|request| request.invocation_id.clone()).collect()
    }
}

#[async_trait]
impl ToolPolicy for RecordingPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.seen.lock().unwrap().push(request.clone());
        self.inner.decide(request)
    }
}

fn principal(instance: &str) -> AgentPrincipal {
    AgentPrincipal::new(AgentId::new(AGENT_ID).unwrap())
        .with_tenant("acme-corp")
        .with_instance(instance)
}

struct Deployment {
    policy: Arc<dyn ToolPolicy>,
    ledger: Arc<dyn SpendLedger>,
    approvals: Option<Arc<dyn ApprovalStore>>,
    source: Option<StaticIdentitySource>,
}

/// Builds a runner for one deployment of the billing agent and runs one turn calling `lookup`.
async fn run_billing(
    deployment: Deployment,
    seen: Arc<Mutex<Vec<ActingContext>>>,
) -> Vec<Result<adk_core::Event>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let agent = LlmAgentBuilder::new("billing")
        .model(ScriptedModel::new(vec![call("lookup", json!({ "invoice": "INV-1" }), "c1")]))
        .tool(Arc::new(LookupTool { seen }))
        .build()
        .unwrap();
    let mut config =
        RunConfig::builder().tool_policy(deployment.policy).spend_ledger(deployment.ledger);
    if let Some(store) = deployment.approvals {
        let service = adk_core::StoreApprovalService::new(store.clone()).with_router(
            adk_core::ApprovalPolicy::new([adk_core::ApproverSelector::group("finance")]),
        );
        config = config.approval_store(store).approval_service(Arc::new(service));
    }
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config.build());
    if let Some(source) = deployment.source {
        builder = builder.identity_source(Arc::new(source));
    }
    let runner =
        builder.build().unwrap().with_llm_spend_estimate(LlmSpendEstimate::per_call(30_000));
    runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("is INV-1 paid?"),
        )
        .await
        .unwrap()
        .collect()
        .await
}

fn allow_billing_agents() -> DeclarativePolicy {
    DeclarativePolicy::builder()
        .rule(
            PolicyRule::allow("lookup")
                .for_agent("spiffe://acme.test/agent/*")
                .for_tenant("acme-corp"),
        )
        .build()
}

async fn spent(ledger: &InMemorySpendLedger, agent: &str) -> u64 {
    ledger.spent(&SpendKey::org(APP).with_agent(agent), SpendPeriod::Day).await.unwrap()
}

#[tokio::test]
async fn policy_decisions_tool_context_and_spend_carry_the_configured_agent_id() {
    let policy = RecordingPolicy::new(allow_billing_agents());
    let ledger = Arc::new(InMemorySpendLedger::default());
    let seen = Arc::new(Mutex::new(Vec::new()));

    let events = run_billing(
        Deployment {
            policy: policy.clone(),
            ledger: ledger.clone(),
            approvals: None,
            source: Some(StaticIdentitySource::new(principal("replica-a"))),
        },
        seen.clone(),
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let decided = policy.acting();
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].agent, principal("replica-a"));
    assert_eq!(decided[0].on_behalf_of, Some(SubjectRef::human("alice")));
    assert!(policy.invocations()[0].starts_with("inv-"), "{:?}", policy.invocations());
    assert_eq!(*seen.lock().unwrap(), decided, "the tool sees the context the policy decided on");
    // Two model calls at one cent each.
    assert_eq!(spent(&ledger, AGENT_ID).await, 20_000);
}

#[tokio::test]
async fn a_redeploy_keeps_the_agent_id() {
    let policy = RecordingPolicy::new(allow_billing_agents());
    let ledger = Arc::new(InMemorySpendLedger::default());
    for instance in ["replica-a", "replica-b"] {
        run_billing(
            Deployment {
                policy: policy.clone(),
                ledger: ledger.clone(),
                approvals: None,
                source: Some(StaticIdentitySource::new(principal(instance))),
            },
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;
    }

    let decided = policy.acting();
    assert_eq!(
        decided.iter().map(|acting| acting.agent.id.as_str()).collect::<Vec<_>>(),
        [AGENT_ID, AGENT_ID]
    );
    assert_eq!(
        decided.iter().map(|acting| acting.agent.instance.as_deref()).collect::<Vec<_>>(),
        [Some("replica-a"), Some("replica-b")]
    );
    assert_eq!(spent(&ledger, AGENT_ID).await, 40_000, "both deployments spend under one key");
}

#[tokio::test]
async fn without_configuration_the_derived_id_is_stable_across_runners() {
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("lookup").build());
    let ledger = Arc::new(InMemorySpendLedger::default());
    for _ in 0..2 {
        run_billing(
            Deployment {
                policy: policy.clone(),
                ledger: ledger.clone(),
                approvals: None,
                source: None,
            },
            Arc::new(Mutex::new(Vec::new())),
        )
        .await;
    }

    let expected = AgentPrincipal::derived(APP, "billing");
    assert_eq!(expected.id.as_str(), "urn:adk:agent:acme/billing");
    assert!(policy.acting().iter().all(|acting| acting.agent == expected));
    assert_eq!(spent(&ledger, "urn:adk:agent:acme/billing").await, 40_000);
}

#[tokio::test]
async fn a_rule_for_another_agent_does_not_authorize_this_one() {
    let policy = RecordingPolicy::new(
        DeclarativePolicy::builder()
            .rule(PolicyRule::allow("lookup").for_agent("spiffe://acme.test/agent/support"))
            .build(),
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    run_billing(
        Deployment {
            policy: policy.clone(),
            ledger: Arc::new(InMemorySpendLedger::default()),
            approvals: None,
            source: Some(StaticIdentitySource::new(principal("replica-a"))),
        },
        seen.clone(),
    )
    .await;

    assert_eq!(policy.acting().len(), 1);
    assert!(seen.lock().unwrap().is_empty(), "the call is denied by default");
}

#[tokio::test]
async fn held_approvals_record_the_acting_context() {
    let store = Arc::new(InMemoryApprovalStore::new());
    let policy = RecordingPolicy::new(
        DeclarativePolicy::builder().require_approval("lookup", "invoices are sensitive").build(),
    );
    run_billing(
        Deployment {
            policy: policy.clone(),
            ledger: Arc::new(InMemorySpendLedger::default()),
            approvals: Some(store.clone()),
            source: Some(StaticIdentitySource::new(principal("replica-a"))),
        },
        Arc::new(Mutex::new(Vec::new())),
    )
    .await;

    let pending = store.pending(&ApprovalScope::new(APP, "alice", "s1")).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.acting.as_deref(), policy.acting().first());
    assert_eq!(pending[0].request.acting.as_ref().unwrap().agent.id.as_str(), AGENT_ID);

    // The approval service's request records the same acting context for the approver.
    let requests = store.list_requests(&adk_core::ApprovalRequestQuery::default()).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].acting.as_ref(), policy.acting().first());
    assert_eq!(requests[0].requester, Some(SubjectRef::human("alice")));
}

/// An identity source that cannot answer.
#[derive(Debug)]
struct BrokenSource;

#[async_trait]
impl adk_core::IdentitySource for BrokenSource {
    async fn current(&self) -> Result<AgentPrincipal> {
        Err(AdkError::new(
            adk_core::ErrorComponent::Auth,
            adk_core::ErrorCategory::Unavailable,
            "identity.source.unavailable",
            "workload API socket is not reachable",
        ))
    }
}

#[tokio::test]
async fn a_failing_identity_source_stops_the_run_before_the_agent() {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = LlmAgentBuilder::new("billing")
        .model(ScriptedModel::new(vec![call("lookup", json!({}), "c1")]))
        .tool(Arc::new(LookupTool { seen: seen.clone() }))
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .identity_source(Arc::new(BrokenSource))
        .build()
        .unwrap();
    let events: Vec<_> = runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("hi"),
        )
        .await
        .unwrap()
        .collect()
        .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].as_ref().unwrap_err().code, "identity.source.unavailable");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn agent_tool_delegation_extends_the_chain() {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let researcher = LlmAgentBuilder::new("researcher")
        .description("Looks things up.")
        .model(ScriptedModel::new(vec![call("lookup", json!({}), "c2")]))
        .tool(Arc::new(LookupTool { seen: seen.clone() }))
        .build()
        .unwrap();
    let planner = LlmAgentBuilder::new("planner")
        .model(ScriptedModel::new(vec![call("researcher", json!({ "request": "INV-1" }), "c1")]))
        .tool(Arc::new(AgentTool::new(Arc::new(researcher))))
        .build()
        .unwrap();
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(planner))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(RunConfig::builder().tool_policy(policy.clone()).build())
        .agent_principal(principal("replica-a"))
        .build()
        .unwrap();
    let events: Vec<_> = runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap()
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let decided = policy.acting();
    assert_eq!(decided.len(), 2, "{decided:?}");
    let (planner_call, researcher_call) = (&decided[0], &decided[1]);
    assert_eq!(planner_call.agent.id.as_str(), AGENT_ID);
    assert!(planner_call.delegation_chain.is_empty());
    assert_eq!(researcher_call.agent.id.as_str(), "urn:adk:agent:acme/researcher");
    assert_eq!(
        researcher_call.agent.tenant.as_deref(),
        Some("acme-corp"),
        "the tenant carries over"
    );
    assert_eq!(researcher_call.delegation_chain, vec![SubjectRef::agent(&planner_call.agent.id)]);
    assert_eq!(researcher_call.on_behalf_of, Some(SubjectRef::human("alice")));
    let invocations = policy.invocations();
    assert_ne!(invocations[1], invocations[0]);
    assert!(!invocations[1].is_empty());
    assert_eq!(seen.lock().unwrap().as_slice(), &decided[1..]);
}

// --- Identity invariants: session subject, per-agent principals, one invocation id ---

async fn alice_session() -> Arc<InMemorySessionService> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    sessions
}

fn lookup_agent(name: &str, seen: Arc<Mutex<Vec<ActingContext>>>) -> Arc<dyn adk_core::Agent> {
    Arc::new(
        LlmAgentBuilder::new(name)
            .model(ScriptedModel::new(vec![call("lookup", json!({}), &format!("c-{name}"))]))
            .tool(Arc::new(LookupTool { seen }))
            .build()
            .unwrap(),
    )
}

async fn run_alice(runner: &Runner, config: Option<RunConfig>) -> Vec<Result<adk_core::Event>> {
    runner
        .run_with_config(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("go"),
            config,
        )
        .await
        .unwrap()
        .collect()
        .await
}

fn audit_agents(recorder: &adk_core::audit_log::InMemoryAuditRecorder) -> Vec<String> {
    recorder
        .records()
        .iter()
        .filter_map(|record| record.actor.agent.as_ref().map(ToString::to_string))
        .collect()
}

#[tokio::test]
async fn a_run_acting_for_another_subject_is_refused_and_audited() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = adk_core::audit_log::InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(lookup_agent("billing", seen.clone()))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(Arc::new(DeclarativePolicy::builder().allow("*").build()))
        .audit_log(Arc::new(recorder.clone()))
        .build()
        .unwrap();

    // alice's session, configured to act for bob.
    let config = RunConfig::builder().on_behalf_of(SubjectRef::human("bob")).build();
    let events = run_alice(&runner, Some(config)).await;

    assert_eq!(events.len(), 1, "{events:?}");
    let error = events[0].as_ref().unwrap_err();
    assert_eq!(error.code, adk_core::SUBJECT_MISMATCH_CODE);
    assert_eq!(error.category, adk_core::ErrorCategory::Forbidden);
    assert!(seen.lock().unwrap().is_empty(), "no tool ran");
    let records = recorder.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].kind, adk_core::audit_log::AuditRecordKind::Access);
    assert_eq!(records[0].decision, Some(adk_core::audit_log::AuditDecision::Denied));
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::human("bob")));
    assert_eq!(records[0].ext["sessionSubject"], json!({ "kind": "human", "id": "alice" }));

    // The same id as a different kind is a different subject.
    let config = RunConfig::builder().on_behalf_of(SubjectRef::service("alice")).build();
    let events = run_alice(&runner, Some(config)).await;
    assert_eq!(events[0].as_ref().unwrap_err().code, adk_core::SUBJECT_MISMATCH_CODE);
}

#[tokio::test]
async fn a_subject_delegation_policy_allows_acting_for_another_subject() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(lookup_agent("billing", seen.clone()))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .subject_delegation(Arc::new(
            adk_core::StaticSubjectDelegation::new()
                .allow(SubjectRef::human("alice"), SubjectRef::human("bob")),
        ))
        .build()
        .unwrap();

    let config = RunConfig::builder().on_behalf_of(SubjectRef::human("bob")).build();
    let events = run_alice(&runner, Some(config)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let decided = policy.acting();
    assert_eq!(
        decided,
        vec![
            ActingContext::new(AgentPrincipal::derived(APP, "billing"))
                .on_behalf_of(SubjectRef::human("bob"))
                .delegated_by(SubjectRef::human("alice"))
        ]
    );
    assert_eq!(*seen.lock().unwrap(), decided);
}

#[tokio::test]
async fn sequential_sub_agents_act_as_themselves_in_policy_and_audit() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pipeline = adk_agent::SequentialAgent::new(
        "pipeline",
        vec![lookup_agent("drafter", seen.clone()), lookup_agent("reviewer", seen.clone())],
    );
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let recorder = adk_core::audit_log::InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(pipeline))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .audit_log(Arc::new(recorder.clone()))
        .build()
        .unwrap();

    let events = run_alice(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let ids = ["urn:adk:agent:acme/drafter", "urn:adk:agent:acme/reviewer"];
    let decided: Vec<_> =
        policy.acting().iter().map(|acting| acting.agent.id.to_string()).collect();
    assert_eq!(decided, ids);
    assert_eq!(audit_agents(&recorder), ids);
    assert!(
        policy
            .acting()
            .iter()
            .all(|acting| acting.on_behalf_of == Some(SubjectRef::human("alice"))),
        "every sub-agent acts for the session's user"
    );
}

#[tokio::test]
async fn an_identity_source_principal_applies_to_the_root_unless_it_maps_sub_agents() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let pipeline = adk_agent::SequentialAgent::new(
        "pipeline",
        vec![lookup_agent("drafter", seen.clone()), lookup_agent("reviewer", seen.clone())],
    );
    let reviewer = AgentPrincipal::new(AgentId::new("spiffe://acme.test/agent/reviewer").unwrap());
    let source =
        StaticIdentitySource::new(principal("replica-a")).with_agent("reviewer", reviewer.clone());
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let recorder = adk_core::audit_log::InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(pipeline))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .audit_log(Arc::new(recorder.clone()))
        .identity_source(Arc::new(source))
        .build()
        .unwrap();

    let events = run_alice(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    // The source's own principal is the root's (`pipeline`), not the drafter's.
    let ids = ["urn:adk:agent:acme/drafter", "spiffe://acme.test/agent/reviewer"];
    let decided: Vec<_> =
        policy.acting().iter().map(|acting| acting.agent.id.to_string()).collect();
    assert_eq!(decided, ids);
    assert_eq!(audit_agents(&recorder), ids);
    assert_eq!(policy.acting()[1].agent, reviewer);
}

#[tokio::test]
async fn a_transfer_target_acts_as_its_own_principal() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let router = LlmAgentBuilder::new("router")
        .model(ScriptedModel::new(vec![call(
            "transfer_to_agent",
            json!({ "agent_name": "billing" }),
            "c-transfer",
        )]))
        .sub_agent(lookup_agent("billing", seen.clone()))
        .build()
        .unwrap();
    let billing = AgentPrincipal::new(AgentId::new(AGENT_ID).unwrap());
    let source =
        StaticIdentitySource::new(principal("replica-a")).with_agent("billing", billing.clone());
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(router))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .identity_source(Arc::new(source))
        .build()
        .unwrap();

    let events = run_alice(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        seen.lock().unwrap().iter().map(|acting| acting.agent.clone()).collect::<Vec<_>>(),
        [billing]
    );
}

#[tokio::test]
async fn the_invocation_id_has_one_source() {
    /// Records the invocation id the tool context reports.
    struct InvocationTool {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for InvocationTool {
        fn name(&self) -> &str {
            "lookup"
        }

        fn description(&self) -> &str {
            "Reports its invocation."
        }

        async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            self.seen.lock().unwrap().push(ctx.invocation_id().to_string());
            Ok(json!({}))
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = LlmAgentBuilder::new("billing")
        .model(ScriptedModel::new(vec![call("lookup", json!({}), "c1")]))
        .tool(Arc::new(InvocationTool { seen: seen.clone() }))
        .build()
        .unwrap();
    let policy = RecordingPolicy::new(DeclarativePolicy::builder().allow("*").build());
    let recorder = adk_core::audit_log::InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(alice_session().await as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .audit_log(Arc::new(recorder.clone()))
        .build()
        .unwrap();
    let events = run_alice(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    // The execution identity's id is the one the policy, the tool, and the audit see.
    let invocation = policy.invocations()[0].clone();
    assert!(invocation.starts_with("inv-"), "{invocation}");
    assert_eq!(*seen.lock().unwrap(), vec![invocation.clone()]);
    let records = recorder.records();
    assert_eq!(records[0].actor.invocation_id.as_deref(), Some(invocation.as_str()));
    // The acting context itself carries no invocation id to drift from it.
    let acting = serde_json::to_value(&policy.acting()[0]).unwrap();
    assert!(acting.get("invocationId").is_none(), "{acting}");
}
