//! Partitions and crossings (spec 3.2 §7.8, §13 Q3, K-I15): the partition policy's decisions,
//! the crossing fingerprint, run grants, the partition audit records, the partition identity,
//! and memory confined to a partition's scope.

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActingContext, AgentPrincipal, Content, Memory, MemoryEntry, PolicyCall, PolicyDecision,
    ToolConfirmationRequest, ToolOrigin, ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::audit::{PARTITION_CHANGED, PARTITION_CREATED, PARTITION_CROSSING};
use adk_kernel::{
    CrossPartition, Crossing, CrossingAccess, CrossingFilter, CrossingOutcome, CrossingResource,
    CrossingRoute, CrossingRun, Partition, PartitionId, PartitionMode, PartitionPolicy, Partitions,
};
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
use proptest::prelude::*;
use serde_json::{Value, json};

const SEARCH: &str = "partition_memory_search";
const REMEMBER: &str = "partition_memory_remember";

struct Setup {
    partitions: Partitions,
    recorder: InMemoryAuditRecorder,
}

/// `home` and `work`, both open to the coordinator, and `vault`, closed to crossings.
async fn setup() -> Setup {
    let recorder = InMemoryAuditRecorder::new();
    let partitions =
        Partitions::in_memory().with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    let by = AuditActor::default();
    for (id, cross) in [
        ("home", CrossPartition::CoordinatorWithApproval),
        ("work", CrossPartition::CoordinatorWithApproval),
        ("vault", CrossPartition::Deny),
    ] {
        let partition = Partition::new(id, format!("{id}-agents")).with_cross_partition(cross);
        partitions.put(partition, by.clone()).await.unwrap();
    }
    Setup { partitions, recorder }
}

fn policy(partitions: &Partitions, home: &str, coordinator: bool) -> PartitionPolicy {
    let policy = partitions
        .policy(home)
        .with_crossing_tool(
            SEARCH,
            CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Read, "memory"),
        )
        .with_crossing_tool(
            REMEMBER,
            CrossingRoute::argument(CrossingResource::Memory, CrossingAccess::Write, "memory"),
        );
    if coordinator { policy.with_coordinator("harness") } else { policy }
}

fn request(tool: &str, args: Value, invocation: &str) -> ToolPolicyRequest {
    let mut request = ToolPolicyRequest::new(tool, args);
    request.agent_name = "harness".into();
    request.invocation_id = invocation.into();
    request.acting = Some(ActingContext::new(AgentPrincipal::derived("app", "harness")));
    request
}

fn read_work(invocation: &str) -> ToolPolicyRequest {
    request(SEARCH, json!({ "partition": "work", "query": "standup" }), invocation)
}

fn origin(tool: &str) -> ToolOrigin {
    ToolOrigin::new(adk_core::STATIC_TOOL_ORIGIN, &json!({ "name": tool }), None)
}

async fn decide(
    policy: &PartitionPolicy,
    request: &ToolPolicyRequest,
    call_id: &str,
) -> PolicyDecision {
    decide_in(policy, request, call_id, None).await
}

async fn decide_in(
    policy: &PartitionPolicy,
    request: &ToolPolicyRequest,
    call_id: &str,
    durable_run: Option<&str>,
) -> PolicyDecision {
    let origin = origin(&request.tool_name);
    let mut call = PolicyCall::new().with_origin(&origin).with_function_call_id(call_id);
    if let Some(run) = durable_run {
        call = call.with_durable_run_id(run);
    }
    policy.evaluate_call(request, &call).await
}

/// The crossing a call to `tool` with `args` by the harness makes, and its fingerprint.
fn crossing_of(policy: &PartitionPolicy, request: &ToolPolicyRequest) -> (Crossing, String) {
    let route = policy.route(&request.tool_name).unwrap();
    let crossing = route.crossing(policy.home(), &request.args).unwrap();
    let confirmation = ToolConfirmationRequest::new(&request.tool_name, request.args.clone())
        .with_acting_context(request.acting.clone().unwrap())
        .with_origin(origin(&request.tool_name));
    let fingerprint = crossing.fingerprint(&confirmation);
    (crossing, fingerprint)
}

fn crossing_records(recorder: &InMemoryAuditRecorder) -> Vec<Value> {
    recorder
        .records()
        .into_iter()
        .filter(|draft| draft.action == PARTITION_CROSSING)
        .map(|draft| Value::Object(draft.ext.into_iter().collect()))
        .collect()
}

#[tokio::test]
async fn a_call_that_crosses_no_partition_is_the_other_layers_to_decide() {
    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "work", false);
    let other = request("read_file", json!({ "path": "notes.md" }), "inv-1");
    assert_eq!(decide(&policy, &other, "c1").await, PolicyDecision::Allow);
    let own = request(SEARCH, json!({ "partition": "work", "query": "x" }), "inv-1");
    assert_eq!(decide(&policy, &own, "c2").await, PolicyDecision::Allow);
    let nameless = request(SEARCH, json!({ "query": "x" }), "inv-1");
    assert!(matches!(decide(&policy, &nameless, "c3").await, PolicyDecision::Deny { .. }));
}

/// K-I15: an agent that is not the coordinator never crosses, and the refusal is audited.
#[tokio::test]
async fn a_work_agents_read_of_a_home_fact_is_denied_at_the_policy() {
    let Setup { partitions, recorder } = setup().await;
    let policy = policy(&partitions, "work", false);
    let read_home = request(SEARCH, json!({ "partition": "home", "query": "dentist" }), "inv-1");
    let PolicyDecision::Deny { reason } = decide(&policy, &read_home, "c1").await else {
        panic!("a crossing by a member of the work partition is denied");
    };
    assert!(reason.contains("only the coordinator"), "{reason}");
    let records = crossing_records(&recorder);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["partition"], "work");
    assert_eq!(records[0]["to"], "home");
    assert_eq!(records[0]["outcome"], "denied");
    assert_eq!(records[0]["resource"], "memory");
    // The audit record carries a digest, never the query.
    assert!(!records[0].to_string().contains("dentist"));
    let ledger = partitions.crossings(&CrossingFilter::default().from("work")).await.unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].outcome, CrossingOutcome::Denied);
}

#[tokio::test]
async fn the_coordinators_crossing_needs_approval_naming_the_crossing() {
    let Setup { partitions, recorder } = setup().await;
    let policy = policy(&partitions, "home", true);
    let PolicyDecision::RequireApproval { reason } =
        decide(&policy, &read_work("inv-1"), "c1").await
    else {
        panic!("the coordinator's crossing is held for approval");
    };
    assert!(reason.contains("work › memory"), "{reason}");
    assert!(reason.contains("allow this read of work › memory for this run"), "{reason}");
    assert!(reason.contains("deny and block crossings to work from this partition"), "{reason}");
    // The governed path and a prompt's re-check evaluate one call twice: one record.
    decide(&policy, &read_work("inv-1"), "c1").await;
    let records = crossing_records(&recorder);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["outcome"], "held");
}

#[tokio::test]
async fn a_partition_closed_to_crossings_or_a_block_refuses_even_the_coordinator() {
    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "home", true);
    let vault = request(SEARCH, json!({ "partition": "vault", "query": "x" }), "inv-1");
    let PolicyDecision::Deny { reason } = decide(&policy, &vault, "c1").await else {
        panic!("vault takes part in no crossings");
    };
    assert!(reason.contains("cross_partition: deny"), "{reason}");
    let unknown = request(SEARCH, json!({ "partition": "garage", "query": "x" }), "inv-1");
    assert!(matches!(decide(&policy, &unknown, "c2").await, PolicyDecision::Deny { .. }));

    partitions.block("home", "work", AuditActor::default()).await.unwrap();
    let PolicyDecision::Deny { reason } = decide(&policy, &read_work("inv-1"), "c3").await else {
        panic!("crossings from home to work are blocked");
    };
    assert!(reason.contains("blocked"), "{reason}");
    // A closed source partition refuses its own coordinator too.
    let closed = policy_from_vault(&partitions);
    assert!(matches!(
        decide(&closed, &read_work("inv-1"), "c4").await,
        PolicyDecision::Deny { .. }
    ));
}

fn policy_from_vault(partitions: &Partitions) -> PartitionPolicy {
    policy(partitions, "vault", true)
}

/// §13 Q3: the coordinator's identical read is not asked again in the same run, a write is,
/// and the next run asks again.
#[tokio::test]
async fn an_approved_read_holds_for_the_rest_of_its_run_and_nothing_beyond() {
    let Setup { partitions, recorder } = setup().await;
    let policy = policy(&partitions, "home", true);
    let first = read_work("inv-1");
    assert!(matches!(decide(&policy, &first, "c1").await, PolicyDecision::RequireApproval { .. }));
    // The person approved it; the crossing tool ran and recorded the approval.
    let (crossing, fingerprint) = crossing_of(&policy, &first);
    let invocation = CrossingRun::Invocation("inv-1".into());
    partitions
        .executed(&crossing, &fingerprint, &invocation, "urn:adk:agent:app/harness", SEARCH)
        .await
        .unwrap();

    assert_eq!(decide(&policy, &read_work("inv-1"), "c2").await, PolicyDecision::Allow);
    let other_query = request(SEARCH, json!({ "partition": "work", "query": "salary" }), "inv-1");
    assert!(
        matches!(decide(&policy, &other_query, "c3").await, PolicyDecision::RequireApproval { .. }),
        "another read is another crossing"
    );
    let write =
        request(REMEMBER, json!({ "partition": "work", "text": "x", "kind": "decision" }), "inv-1");
    let (write_crossing, write_print) = crossing_of(&policy, &write);
    assert!(matches!(decide(&policy, &write, "c4").await, PolicyDecision::RequireApproval { .. }));
    partitions
        .executed(&write_crossing, &write_print, &invocation, "agent", REMEMBER)
        .await
        .unwrap();
    assert!(
        matches!(decide(&policy, &write, "c5").await, PolicyDecision::RequireApproval { .. }),
        "a write stays one-shot"
    );
    assert!(
        matches!(
            decide(&policy, &read_work("inv-2"), "c6").await,
            PolicyDecision::RequireApproval { .. }
        ),
        "the next run asks again"
    );
    let outcomes: Vec<String> = crossing_records(&recorder)
        .iter()
        .map(|record| record["outcome"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        outcomes,
        ["held", "approved", "runGrant", "held", "held", "approved", "held", "held"]
    );
}

/// A durable run's grant holds across the invocations that resume it.
#[tokio::test]
async fn a_durable_runs_grant_spans_its_invocations() {
    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "home", true);
    let first = read_work("inv-1");
    let (crossing, fingerprint) = crossing_of(&policy, &first);
    let run = CrossingRun::Durable("run-9".into());
    partitions.executed(&crossing, &fingerprint, &run, "agent", SEARCH).await.unwrap();
    let resumed = read_work("inv-2");
    assert_eq!(decide_in(&policy, &resumed, "c1", Some("run-9")).await, PolicyDecision::Allow);
    assert!(matches!(
        decide_in(&policy, &resumed, "c2", Some("run-10")).await,
        PolicyDecision::RequireApproval { .. }
    ));
}

/// A run with no durable run id keys its grant by its invocation; the grant is dropped when
/// the invocation ends, so a later run, even one that reused the id, asks again.
#[tokio::test]
async fn an_invocations_grant_is_dropped_when_it_ends() {
    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "home", true);
    let read = read_work("inv-7");
    let (crossing, fingerprint) = crossing_of(&policy, &read);
    let invocation = CrossingRun::of(None, "inv-7");
    assert_eq!(invocation, CrossingRun::Invocation("inv-7".into()));
    partitions.executed(&crossing, &fingerprint, &invocation, "agent", SEARCH).await.unwrap();
    assert_eq!(decide(&policy, &read, "c1").await, PolicyDecision::Allow);
    assert_eq!(partitions.grants(&invocation).await.unwrap().len(), 1);
    assert_eq!(partitions.release_run(&invocation).await.unwrap(), 1);
    assert!(partitions.grants(&invocation).await.unwrap().is_empty());
    assert!(
        matches!(decide(&policy, &read, "c2").await, PolicyDecision::RequireApproval { .. }),
        "a released invocation holds no grant"
    );
    // A durable run's grant is the store's, released the same way.
    let durable = CrossingRun::of(Some("run-3"), "inv-8");
    partitions.executed(&crossing, &fingerprint, &durable, "agent", SEARCH).await.unwrap();
    assert_eq!(partitions.store().grants("run-3").await.unwrap().len(), 1);
    partitions.release_run(&durable).await.unwrap();
    assert!(partitions.store().grants("run-3").await.unwrap().is_empty());
}

/// K-I9: a write, and a driver tool that does not declare itself read-only — a
/// `Financial` or `PublishPublic` call among them — is never kept as a grant, whatever
/// access the caller stated.
#[tokio::test]
async fn writes_and_acting_driver_calls_are_never_reusable_grants() {
    use adk_core::{Tool, ToolContext, ToolEffect};
    use adk_kernel::CrossingTool;

    struct Pay;
    #[async_trait::async_trait]
    impl Tool for Pay {
        fn name(&self) -> &str {
            "send_invoice"
        }
        fn description(&self) -> &str {
            "pays an invoice"
        }
        fn effect(&self) -> ToolEffect {
            ToolEffect::NonIdempotent
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(json!({}))
        }
    }

    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "home", true);
    // The caller says Read, the tool does not declare itself read-only: one-shot.
    let tool = CrossingTool::new(
        Arc::new(Pay),
        &policy,
        "work",
        CrossingResource::Driver,
        CrossingAccess::Read,
        "billing",
    );
    let route = policy.route(tool.name()).unwrap();
    let crossing = route.crossing(policy.home(), &json!({})).unwrap();
    assert_eq!(crossing.access, CrossingAccess::Write);
    let call = request(tool.name(), json!({ "invoice": 7 }), "inv-1");
    let (crossing, fingerprint) = crossing_of(&policy, &call);
    let invocation = CrossingRun::Invocation("inv-1".into());
    partitions.executed(&crossing, &fingerprint, &invocation, "agent", tool.name()).await.unwrap();
    assert!(partitions.grants(&invocation).await.unwrap().is_empty());
    assert!(matches!(decide(&policy, &call, "c2").await, PolicyDecision::RequireApproval { .. }));
}

/// The maintenance process's report of recurring crossings: each request counts once, and
/// the remedy is partition membership.
#[tokio::test]
async fn recurring_crossings_are_counted_once_per_request() {
    let Setup { partitions, .. } = setup().await;
    let policy = policy(&partitions, "home", true);
    let before = chrono::Utc::now() - chrono::Duration::seconds(1);
    for (call, invocation) in [("c1", "inv-1"), ("c2", "inv-2"), ("c3", "inv-3")] {
        let read = read_work(invocation);
        decide(&policy, &read, call).await;
        let (crossing, fingerprint) = crossing_of(&policy, &read);
        let run = CrossingRun::Invocation(invocation.into());
        partitions.executed(&crossing, &fingerprint, &run, "agent", SEARCH).await.unwrap();
        // The identical read again in the same run.
        decide(&policy, &read, &format!("{call}-again")).await;
    }
    let work_agent = work_policy(&partitions);
    decide(
        &work_agent,
        &request(SEARCH, json!({ "partition": "home", "query": "x" }), "inv-9"),
        "c9",
    )
    .await;

    let recurring = partitions.recurring_crossings(before, 2).await.unwrap();
    assert_eq!(recurring.len(), 1, "{recurring:?}");
    assert_eq!(
        (recurring[0].from.as_str(), recurring[0].to.as_str(), recurring[0].count),
        ("home", "work", 6),
        "three held requests and three let through by their run's approval"
    );
    assert!(recurring[0].remedy().contains("never waived"));
    assert_eq!(partitions.recurring_crossings(before, 1).await.unwrap().len(), 2);
    assert!(
        partitions
            .recurring_crossings(chrono::Utc::now() + chrono::Duration::hours(1), 1)
            .await
            .unwrap()
            .is_empty()
    );
}

fn work_policy(partitions: &Partitions) -> PartitionPolicy {
    policy(partitions, "work", false)
}

/// The crossing fingerprint extends the call's fingerprint with the target partition and
/// the resource class.
#[test]
fn the_crossing_fingerprint_differs_by_target_partition_and_resource_class() {
    let acting = ActingContext::new(AgentPrincipal::derived("app", "harness"));
    let call = ToolConfirmationRequest::new(SEARCH, json!({ "partition": "work", "query": "x" }))
        .with_acting_context(acting.clone())
        .with_origin(origin(SEARCH));
    let crossing =
        |to: &str, resource| Crossing::new("home", to, resource, "memory", CrossingAccess::Read);
    let base = crossing("work", CrossingResource::Memory).fingerprint(&call);
    assert!(base.starts_with(&call.fingerprint()));
    assert_eq!(base, crossing("work", CrossingResource::Memory).fingerprint(&call.clone()));
    let mut seen = std::collections::BTreeSet::new();
    for to in ["work", "team", "home"] {
        for resource in
            [CrossingResource::Memory, CrossingResource::Driver, CrossingResource::Session]
        {
            assert!(seen.insert(crossing(to, resource).fingerprint(&call)), "{to} {resource}");
        }
    }
    // Another agent's identical call is another crossing.
    let other = ToolConfirmationRequest::new(SEARCH, json!({ "partition": "work", "query": "x" }))
        .with_acting_context(ActingContext::new(AgentPrincipal::derived("app", "member")))
        .with_origin(origin(SEARCH));
    assert_ne!(base, crossing("work", CrossingResource::Memory).fingerprint(&other));
    // A control character in a partition id cannot forge a separator.
    let forged = crossing("work\u{1f}memory", CrossingResource::Memory).fingerprint(&call);
    assert_eq!(forged.matches('\u{1f}').count(), base.matches('\u{1f}').count());
}

#[tokio::test]
async fn partition_changes_are_audited_with_the_fields_that_changed() {
    let Setup { partitions, recorder } = setup().await;
    let before = recorder.records().len();
    partitions.set_mode("work", PartitionMode::Auto, AuditActor::default()).await.unwrap();
    // Writing the same record again changes nothing and records nothing.
    let work = partitions.get(&PartitionId::new("work")).await.unwrap().unwrap();
    partitions.put(work, AuditActor::default()).await.unwrap();
    let records = recorder.records();
    let created = records.iter().filter(|draft| draft.action == PARTITION_CREATED).count();
    assert_eq!(created, 3);
    let changed: Vec<_> =
        records[before..].iter().filter(|draft| draft.action == PARTITION_CHANGED).collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].ext["fields"], json!(["defaultMode"]));
    assert_eq!(changed[0].ext["from"]["defaultMode"], "ask");
    assert_eq!(changed[0].ext["to"]["defaultMode"], "auto");
    let error = partitions
        .set_mode("garage", PartitionMode::Auto, AuditActor::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), adk_kernel::error::NOT_FOUND_CODE);
    let invalid = Partition::new("bad", "");
    assert!(partitions.put(invalid, AuditActor::default()).await.is_err());
}

/// Every agent of a partition's runner carries its class and tenant (spec 3.1 §7.14).
#[tokio::test]
async fn the_partition_identity_gives_every_agent_its_class_and_tenant() {
    let partition = Partition::new("work", "work-agents").with_tenant("acme");
    let source = partition.identity_source("adk-rust", "harness");
    let root = source.current().await.unwrap();
    assert_eq!(root.id.as_str(), "urn:adk:agent:adk-rust/harness");
    let member = source.principal_for("reviewer").await.unwrap().unwrap();
    for principal in [root, member] {
        assert_eq!(principal.class.as_deref(), Some("work-agents"));
        assert_eq!(principal.tenant.as_deref(), Some("acme"));
        assert_eq!(principal.attributes[Partition::PRINCIPAL_ATTRIBUTE], "work");
    }
}

fn memory_service() -> Arc<dyn Memory> {
    Arc::new(MemoryServiceAdapter::new(Arc::new(InMemoryMemoryService::new()), "app", "alice"))
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry { content: Content::new("user").with_text(text), author: "test".into() }
}

fn texts(entries: &[MemoryEntry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| {
            entry.content.parts.iter().filter_map(|part| part.text().map(str::to_string))
        })
        .collect()
}

#[tokio::test]
async fn scoped_memory_refuses_another_partitions_project_and_an_unscoped_memory() {
    let shared = memory_service();
    let work = Partition::new("work", "work").scope_memory(shared.clone()).unwrap();
    let home = Partition::new("home", "home").scope_memory(shared).unwrap();
    work.add(entry("standup at nine")).await.unwrap();
    assert_eq!(texts(&work.search("standup").await.unwrap()), ["standup at nine"]);
    assert!(home.search("standup").await.unwrap().is_empty());
    assert!(home.search_in_project("standup", "work").await.is_err());
    assert!(home.add_to_project(entry("x"), "work").await.is_err());
    assert!(home.delete("standup").await.is_err());

    #[derive(Debug)]
    struct Unscoped;
    #[async_trait::async_trait]
    impl Memory for Unscoped {
        async fn search(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
    }
    assert!(Partition::new("work", "work").scope_memory(Arc::new(Unscoped)).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// No memory search crosses partitions: whatever is written through one partition's
    /// scoped memory is never found through another's (K-I15).
    #[test]
    fn no_memory_search_crosses_partitions(
        writes in proptest::collection::vec((0usize..3, "[a-z]{3,8}( [a-z]{3,8}){0,3}"), 1..12),
        queries in proptest::collection::vec("[a-z]{3,8}", 1..6),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let shared = memory_service();
            let ids = ["home", "work", "team"];
            let scoped: Vec<Arc<dyn Memory>> = ids
                .iter()
                .map(|id| Partition::new(*id, *id).scope_memory(shared.clone()).unwrap())
                .collect();
            let mut written: Vec<Vec<String>> = vec![Vec::new(); ids.len()];
            for (partition, text) in &writes {
                let tagged = format!("{text} {}", ids[*partition]);
                scoped[*partition].add(entry(&tagged)).await.unwrap();
                written[*partition].push(tagged);
            }
            for (index, memory) in scoped.iter().enumerate() {
                for query in queries.iter().map(String::as_str).chain(ids) {
                    for found in texts(&memory.search(query).await.unwrap()) {
                        prop_assert!(
                            written[index].contains(&found),
                            "partition {} found {found:?}, written by another partition",
                            ids[index]
                        );
                    }
                }
            }
            Ok(())
        })?;
    }
}
