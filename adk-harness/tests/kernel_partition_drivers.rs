//! Drivers per partition (spec 3.2 §7.7, §7.8, K-I15): a partition's harness offers the
//! model the tools of a driver only when the driver's manifest lists the partition and the
//! partition's own driver list names the driver; any other driver is invisible there. The
//! coordinator reaches another partition's drivers only as crossings: each call is asked
//! for under a non-waivable approval, a read once per run, anything else each call.
//!
//! The drivers are this test binary run as a stdio MCP server publishing one tool, named
//! by its environment.

#![cfg(feature = "kernel")]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{RunConfig, ToolApproval, ToolConfirmationRequest};
use adk_harness::{Harness, HarnessAgent, HarnessMode, PromptAnswer};
use adk_kernel::drivers::{DriverManifest, DriverSupervisor, driver_source};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{
    CrossPartition, CrossingFilter, CrossingOutcome, CrossingResource, Partition, PartitionId,
    Partitions,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{InMemorySessionService, SessionService};
use common::{APP, Outcome, ScriptedPrompter, collect, no_user_file, outcome, responses, user};
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

const SERVER_ENV: &str = "ADK_HARNESS_PARTITION_DRIVER_TEST_SERVER";
const TOOL_ENV: &str = "ADK_HARNESS_PARTITION_DRIVER_TEST_TOOL";

/// One read tool, named by [`TOOL_ENV`].
#[derive(Clone)]
struct OneTool;

impl ServerHandler for OneTool {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let name = std::env::var(TOOL_ENV).unwrap_or_else(|_| "unnamed".into());
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![Tool::new(name, "Reads an entry.", schema)]))
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
async fn stdio_partition_driver() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = OneTool.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

/// The manifest of driver `id`, serving `partitions`, whose one tool is `tool`.
fn manifest(id: &str, partitions: &[&str], tool: &str) -> DriverManifest {
    classed(id, partitions, tool, "read")
}

/// [`manifest`] with `tool` of effect class `class`.
fn classed(id: &str, partitions: &[&str], tool: &str, class: &str) -> DriverManifest {
    DriverManifest::from_toml(&format!(
        "id = {id:?}\ncommand = {:?}\n\
         args = [\"kernel_partition_drivers::stdio_partition_driver\", \"--exact\", \"--ignored\", \"--quiet\", \"--test-threads=1\"]\n\
         partitions = {partitions:?}\nsandbox = \"unsandboxed\"\n\
         env = {{ {SERVER_ENV} = \"1\", {TOOL_ENV} = {tool:?} }}\n[tools]\n{tool} = {class:?}\n",
        std::env::current_exe().unwrap().display().to_string(),
    ))
    .unwrap()
}

/// The tools partition `partition`'s harness offers the model on its first turn.
async fn offered(
    drivers: &DriverSupervisor,
    partitions: &Partitions,
    partition: &str,
    dir: &std::path::Path,
) -> Vec<String> {
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")]));
    let record = partitions.get(&PartitionId::new(partition)).await.unwrap().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    // `partition_drivers` before `partition`: mounted once the partition is named.
    let harness = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(dir))
        .project_instructions(no_user_file())
        .without_default_members()
        .partition_drivers(drivers)
        .partition(record, partitions.clone())
        .build()
        .unwrap()
        .into_harness(APP, sessions)
        .unwrap();
    let alice = user("alice");
    let session = harness.open_session(&alice, None).await.unwrap();
    collect(harness.send(&alice, &session, "what can you reach?").await.unwrap()).await;
    let mut tools: Vec<String> =
        model.requests()[0].tools.keys().filter(|name| name.ends_with("_read")).cloned().collect();
    tools.sort();
    tools
}

/// Work lists `calendar` and home lists `diary` and `mail`. `calendar` serves both
/// partitions, `diary` home only, `mail` work only: work sees `calendar` alone — `mail` is
/// not on its list — and home sees `diary` alone — `calendar` is not on its list and
/// `mail`'s manifest does not serve home.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_partitions_harness_sees_only_its_own_partitions_drivers() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .home(dir.path())
        .health_check_interval(Duration::from_millis(200))
        .build();
    drivers.start().await.unwrap();
    for manifest in [
        manifest("calendar", &["work", "home"], "calendar_read"),
        manifest("diary", &["home"], "diary_read"),
        manifest("mail", &["work"], "mail_read"),
    ] {
        drivers.mount(manifest, "alice").await.unwrap();
    }
    let partitions = Partitions::in_memory();
    for partition in [
        Partition::new("work", "work").with_driver("calendar"),
        Partition::new("home", "home").with_driver("diary").with_driver("mail"),
    ] {
        partitions.put(partition, AuditActor::default()).await.unwrap();
    }
    assert_eq!(offered(&drivers, &partitions, "work", dir.path()).await, ["calendar_read"]);
    assert_eq!(offered(&drivers, &partitions, "home", dir.path()).await, ["diary_read"]);
    drivers.shutdown().await;
}

/// `partition_drivers` configures a partition's harness: without a partition the build
/// fails rather than mounting every driver.
#[tokio::test]
async fn partition_drivers_without_a_partition_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
    let model = Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let Err(error) = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .mode(HarnessMode::Ask)
        .partition_drivers(&drivers)
        .build()
    else {
        panic!("a harness without a partition mounted the drivers");
    };
    assert!(error.to_string().contains("partition_drivers"), "{error}");
}

/// Mounted drivers and a home and work partition both open to the coordinator: `calendar`
/// (a read) and `mail` (sends) serve work, `diary` serves home.
async fn world(dir: &std::path::Path) -> (DriverSupervisor, Partitions) {
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .home(dir)
        .health_check_interval(Duration::from_millis(200))
        .build();
    drivers.start().await.unwrap();
    for manifest in [
        manifest("calendar", &["work"], "calendar_read"),
        classed("mail", &["work"], "mail_send", "send_external"),
        manifest("diary", &["home"], "diary_read"),
    ] {
        drivers.mount(manifest, "alice").await.unwrap();
    }
    let partitions = Partitions::in_memory();
    for partition in [
        Partition::new("work", "work")
            .with_driver("calendar")
            .with_driver("mail")
            .with_cross_partition(CrossPartition::CoordinatorWithApproval),
        Partition::new("home", "home")
            .with_driver("diary")
            .with_cross_partition(CrossPartition::CoordinatorWithApproval),
    ] {
        partitions.put(partition, AuditActor::default()).await.unwrap();
    }
    (drivers, partitions)
}

/// Home's coordinator with its own drivers and work's as crossings, over `script`.
async fn coordinator(
    drivers: &DriverSupervisor,
    partitions: &Partitions,
    dir: &std::path::Path,
    script: Vec<ScriptedResponse>,
    prompter: Arc<ScriptedPrompter>,
    mode: HarnessMode,
) -> (Harness, Arc<ScriptedLlm>) {
    let model = Arc::new(ScriptedLlm::new("scripted", script));
    let home = partitions.get(&PartitionId::new("home")).await.unwrap().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let harness = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(dir))
        .project_instructions(no_user_file())
        .without_default_members()
        .partition(home, partitions.clone())
        .mode(mode)
        .partition_coordinator()
        .partition_drivers(drivers)
        .partition_driver_crossings(drivers, ["work"])
        .prompter(prompter)
        .build()
        .unwrap()
        .into_harness(APP, sessions)
        .unwrap();
    (harness, model)
}

async fn send(harness: &Harness, message: &str) -> Vec<adk_core::Event> {
    let alice = user("alice");
    let session = harness.open_session(&alice, None).await.unwrap();
    collect(harness.send(&alice, &session, message).await.unwrap()).await
}

/// The coordinator sees its own partition's driver directly and work's only as the crossing
/// `work__calendar_read`. The read is asked once, and its identical read later in the run
/// is let through on the run's grant; the ledger records each step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_coordinator_reaches_another_partitions_driver_only_as_a_crossing() {
    let dir = tempfile::tempdir().unwrap();
    let (drivers, partitions) = world(dir.path()).await;
    let prompter = ScriptedPrompter::new();
    prompter.then(|_| PromptAnswer::ApproveOnce);
    let script = vec![
        ScriptedResponse::function_call("work__calendar_read", serde_json::json!({})),
        ScriptedResponse::function_call("work__calendar_read", serde_json::json!({})),
        ScriptedResponse::text("Standup is at nine."),
    ];
    let (home, model) =
        coordinator(&drivers, &partitions, dir.path(), script, prompter.clone(), HarnessMode::Ask)
            .await;
    let events = send(&home, "what is on the work calendar?").await;

    let mut offered: Vec<String> = model.requests()[0]
        .tools
        .keys()
        .filter(|name| name.ends_with("_read") || name.ends_with("_send"))
        .cloned()
        .collect();
    offered.sort();
    assert_eq!(offered, ["diary_read", "work__calendar_read", "work__mail_send"]);
    let reads: Vec<_> = responses(&events)
        .into_iter()
        .filter(|(name, _)| name == "work__calendar_read")
        .map(|(_, response)| response)
        .collect();
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|read| outcome(read) == Outcome::Executed), "{reads:?}");
    assert_eq!(prompter.count(), 1, "the read once; its identical read on the run's grant");
    let prompt = prompter.prompts.lock().unwrap()[0].clone();
    assert!(
        prompt.rendered.title.starts_with("Partition crossing into work › calendar"),
        "{}",
        prompt.rendered.title
    );
    assert!(prompt.suggestions.is_empty(), "no rule answers a crossing");
    let ledger = partitions.crossings(&CrossingFilter::default()).await.unwrap();
    let steps: Vec<CrossingOutcome> = ledger.iter().map(|record| record.outcome).collect();
    assert_eq!(
        steps,
        [CrossingOutcome::Held, CrossingOutcome::Approved, CrossingOutcome::RunGrant]
    );
    assert!(ledger.iter().all(|record| record.resource == CrossingResource::Driver));
    assert!(ledger.iter().all(|record| record.to == PartitionId::new("work")));

    // Work's own harness sees its drivers directly, and no crossing.
    assert_eq!(
        offered_all(&drivers, &partitions, "work", dir.path()).await,
        ["calendar_read", "mail_send"]
    );
    drivers.shutdown().await;
}

/// The tools of partition `partition`'s harness offered on its first turn that a driver
/// serves, with any crossing.
async fn offered_all(
    drivers: &DriverSupervisor,
    partitions: &Partitions,
    partition: &str,
    dir: &std::path::Path,
) -> Vec<String> {
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")]));
    let record = partitions.get(&PartitionId::new(partition)).await.unwrap().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let harness = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(dir))
        .project_instructions(no_user_file())
        .without_default_members()
        .partition(record, partitions.clone())
        .partition_drivers(drivers)
        .build()
        .unwrap()
        .into_harness(APP, sessions)
        .unwrap();
    send(&harness, "what can you reach?").await;
    let mut tools: Vec<String> = model.requests()[0]
        .tools
        .keys()
        .filter(|name| name.ends_with("_read") || name.ends_with("_send"))
        .cloned()
        .collect();
    tools.sort();
    tools
}

/// K-I9, K-I15: a crossing into a driver tool that sends is asked each call, even in Auto,
/// and a run-wide approval a caller passes for one is removed before the run starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_driver_crossing_that_sends_is_asked_every_call_and_never_waived() {
    let dir = tempfile::tempdir().unwrap();
    let (drivers, partitions) = world(dir.path()).await;
    let prompter = ScriptedPrompter::new();
    prompter.then(|_| PromptAnswer::ApproveOnce);
    prompter.then(|_| PromptAnswer::ApproveOnce);
    let script = vec![
        ScriptedResponse::function_call("work__mail_send", serde_json::json!({})),
        ScriptedResponse::function_call("work__mail_send", serde_json::json!({})),
        ScriptedResponse::text("Sent twice."),
    ];
    let (home, _) =
        coordinator(&drivers, &partitions, dir.path(), script, prompter.clone(), HarnessMode::Auto)
            .await;
    let events = send(&home, "send the note twice").await;
    let sends: Vec<_> = responses(&events)
        .into_iter()
        .filter(|(name, _)| name == "work__mail_send")
        .map(|(_, response)| outcome(&response))
        .collect();
    assert_eq!(sends, [Outcome::Executed, Outcome::Executed]);
    assert_eq!(prompter.count(), 2, "a send is approved one call at a time, in Auto too");

    let mut config = RunConfig::default();
    let crossing = ToolConfirmationRequest::new("work__mail_send", serde_json::json!({}))
        .with_origin(adk_core::ToolOrigin::new(
            driver_source("mail"),
            &serde_json::json!({ "name": "mail_send" }),
            None,
        ));
    config.tool_approvals.insert(crossing.fingerprint(), ToolApproval::approve());
    home.agent().runtime().layer().apply(&mut config);
    assert!(config.tool_approvals.is_empty(), "no run-wide approval answers a crossing");
    drivers.shutdown().await;
}

/// A partition that takes no crossing, or one the coordinator's partition blocks, offers the
/// coordinator nothing; and only the coordinator may be offered crossings.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_or_blocked_partition_offers_no_driver_crossing() {
    let dir = tempfile::tempdir().unwrap();
    let (drivers, partitions) = world(dir.path()).await;
    let crossings = |model: &ScriptedLlm| -> Vec<String> {
        model.requests()[0]
            .tools
            .keys()
            .filter(|name| name.starts_with("work__"))
            .cloned()
            .collect()
    };
    let script = || vec![ScriptedResponse::text("ok")];
    let (open, model) = coordinator(
        &drivers,
        &partitions,
        dir.path(),
        script(),
        ScriptedPrompter::new(),
        HarnessMode::Ask,
    )
    .await;
    send(&open, "hi").await;
    assert_eq!(crossings(&model).len(), 2);

    let mut work = partitions.get(&PartitionId::new("work")).await.unwrap().unwrap();
    work.cross_partition = CrossPartition::Deny;
    partitions.put(work.clone(), AuditActor::default()).await.unwrap();
    let (closed, model) = coordinator(
        &drivers,
        &partitions,
        dir.path(),
        script(),
        ScriptedPrompter::new(),
        HarnessMode::Ask,
    )
    .await;
    send(&closed, "hi").await;
    assert!(crossings(&model).is_empty(), "work takes no crossing");

    work.cross_partition = CrossPartition::CoordinatorWithApproval;
    partitions.put(work, AuditActor::default()).await.unwrap();
    let home = partitions.get(&PartitionId::new("home")).await.unwrap().unwrap();
    partitions
        .put(home.with_blocked_crossing(PartitionId::new("work")), AuditActor::default())
        .await
        .unwrap();
    let (blocked, model) = coordinator(
        &drivers,
        &partitions,
        dir.path(),
        script(),
        ScriptedPrompter::new(),
        HarnessMode::Ask,
    )
    .await;
    send(&blocked, "hi").await;
    assert!(crossings(&model).is_empty(), "home blocks crossings to work");

    let record = partitions.get(&PartitionId::new("work")).await.unwrap().unwrap();
    let Err(error) = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .partition(record, partitions.clone())
        .partition_driver_crossings(&drivers, ["home"])
        .build()
    else {
        panic!("a harness other than the coordinator was offered crossings");
    };
    assert!(error.to_string().contains("partition_coordinator"), "{error}");
    drivers.shutdown().await;
}
