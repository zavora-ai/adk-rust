//! Drivers per partition (spec 3.2 §7.7, §7.8, K-I15): a partition's harness offers the
//! model the tools of a driver only when the driver's manifest lists the partition and the
//! partition's own driver list names the driver; any other driver is invisible there.
//!
//! The drivers are this test binary run as a stdio MCP server publishing one tool, named
//! by its environment.

#![cfg(feature = "kernel")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_harness::{HarnessAgent, HarnessMode};
use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{Partition, PartitionId, Partitions};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{InMemorySessionService, SessionService};
use common::{APP, collect, no_user_file, user};
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
    DriverManifest::from_toml(&format!(
        "id = {id:?}\ncommand = {:?}\n\
         args = [\"stdio_partition_driver\", \"--exact\", \"--ignored\", \"--quiet\", \"--test-threads=1\"]\n\
         partitions = {partitions:?}\nsandbox = \"unsandboxed\"\n\
         env = {{ {SERVER_ENV} = \"1\", {TOOL_ENV} = {tool:?} }}\n[tools]\n{tool} = \"read\"\n",
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
