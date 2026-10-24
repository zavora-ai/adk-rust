//! Drivers on a real stdio MCP server (spec 3.2 §7.7, §11): K6 — mount, crash and restart
//! with backoff, a redeclared tool going stale and needing re-approval — secrets that reach
//! the server as files, never in its environment, and on Windows the Job Object that ends a
//! driver's whole process tree with it (§10 W1).

#![cfg(feature = "drivers")]

mod driver_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActingContext, CredentialBroker, InMemoryRunStore, PolicyDecision, ReadonlyContext,
    RevocationScope, ScopedToken, ToolOrigin, async_trait,
};
use adk_kernel::drivers::{
    DriverManifest, DriverPolicy, DriverProcessTree, DriverState, DriverSupervisor, driver_source,
};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{EventSubscriber, Kernel, KernelConfig, KernelEvent, PartitionId};
use driver_support::{RunContext, UNSANDBOXED, Workdir};
use serde_json::{Value, json};

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_driver() {
    driver_support::serve_stdio().await;
}

/// Waits for the next `Driver` event of `id` in `state`, for at most ten seconds, and
/// returns its reason.
async fn driver_event(
    events: &mut EventSubscriber,
    id: &str,
    state: DriverState,
) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let record = events.recv().await.expect("the bus stays open");
            if let KernelEvent::Driver { id: seen, state: seen_state, reason } = record.event
                && seen == id
                && seen_state == state
            {
                return reason;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {state:?} event for driver {id}"))
}

fn kernel(dir: &std::path::Path, drivers: &DriverSupervisor, trail: AuditTrail) -> Kernel {
    Kernel::builder()
        .config(
            KernelConfig::default()
                .with_app_name("adk-rust")
                .with_home(dir)
                .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket()),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .audit(trail)
        .drivers(drivers.clone())
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k6_a_driver_restarts_after_a_crash_and_a_redeclared_tool_needs_reapproval() {
    let work = Workdir::new();
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .health_check_interval(Duration::from_millis(100))
        .build();
    let kernel = kernel(work.path(), &drivers, AuditTrail::new(recorder.clone()));
    kernel.start().await.unwrap();
    let mut events = kernel.events();

    // Mount: nothing runs until a partition lists the driver's tools.
    drivers.mount(work.manifest("notes", ""), "alice").await.unwrap();
    assert_eq!(drivers.state("notes"), Some(DriverState::Stopped));
    let toolset = drivers.toolset_for(&PartitionId::default(), &["notes".to_string()]);
    let ctx = RunContext::new(None, "s1", "call-1");
    let tools = toolset.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["crash", "env_report", "read_note", "send_note", "spawn_descendant", "write_file"]
    );
    driver_event(&mut events, "notes", DriverState::Running).await;
    let pinned = drivers.record("notes").unwrap().manifest.declaration_digests;
    assert_eq!(pinned.len(), 6, "the first listing after approval pins every tool");
    for tool in &tools {
        assert_eq!(tool.origin(), driver_source("notes"));
    }

    // Another partition does not see the driver, and a partition list without it does not.
    let elsewhere = drivers.toolset_for(&PartitionId::new("home"), &["notes".to_string()]);
    assert!(elsewhere.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await.unwrap().is_empty());
    let unlisted = drivers.toolset_for(&PartitionId::default(), &[]);
    assert!(unlisted.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await.unwrap().is_empty());

    // Kill: the server exits; the supervisor restarts it with backoff.
    let crash = tools.iter().find(|tool| tool.name() == "crash").unwrap().clone();
    let _ = crash.execute(ctx.clone(), json!({})).await;
    driver_event(&mut events, "notes", DriverState::Crashed).await;
    let attempt = driver_event(&mut events, "notes", DriverState::Restarting).await;
    assert_eq!(attempt.as_deref(), Some("restart attempt 1"));
    driver_event(&mut events, "notes", DriverState::Running).await;

    // Redeclare: the changed tool goes stale and the driver needs re-approval.
    work.redeclare("now also deletes");
    drivers.restart("notes").await.unwrap();
    let reason = driver_event(&mut events, "notes", DriverState::NeedsReapproval).await;
    assert!(reason.unwrap().contains("read_note"));
    assert_eq!(drivers.state("notes"), Some(DriverState::NeedsReapproval));
    let record = drivers.record("notes").unwrap();
    assert_eq!(record.stale.keys().collect::<Vec<_>>(), ["read_note"]);

    let policy = DriverPolicy::new(drivers.clone());
    let read_now = record.stale["read_note"].clone();
    assert!(matches!(
        policy.decide("notes", "read_note", &read_now),
        PolicyDecision::RequireApproval { .. }
    ));
    assert_eq!(policy.decide("notes", "send_note", &pinned["send_note"]), PolicyDecision::Allow);
    assert!(matches!(
        policy.decide("calendar", "read_note", &read_now),
        PolicyDecision::Deny { .. }
    ));

    // Re-approve: the new declaration is pinned and the tool is allowed again.
    let record = drivers.reapprove("notes", "alice").await.unwrap();
    assert!(record.stale.is_empty());
    assert_eq!(record.manifest.declaration_digests["read_note"], read_now);
    let reason = driver_event(&mut events, "notes", DriverState::Running).await;
    assert_eq!(reason.as_deref(), Some("re-approved by alice"));
    assert_eq!(policy.decide("notes", "read_note", &read_now), PolicyDecision::Allow);
    assert_eq!(drivers.state("notes"), Some(DriverState::Running));

    let actions: Vec<String> = recorder.records().into_iter().map(|draft| draft.action).collect();
    for action in [
        "kernel.driver.mounted",
        "kernel.driver.started",
        "kernel.driver.crashed",
        "kernel.driver.restarted",
        "kernel.driver.stale",
        "kernel.driver.reapproved",
    ] {
        assert!(actions.iter().any(|seen| seen == action), "{action} missing from {actions:?}");
    }
    kernel.stop().await.unwrap();
    assert!(!work.path().join("drivers/run/notes").exists(), "the run directory is removed");
}

/// The `[kernel] drivers` path of `adk-rust up` (spec 3.2 §7.7, §8.2): manifests approved
/// before the kernel starts are mounted by its start, on its audit trail and bus; at the next
/// start the same manifests are already mounted as approved and keep their pins, and a
/// changed manifest needs a new approval.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drivers_approved_before_start_mount_with_the_kernel_and_keep_their_pins() {
    let work = Workdir::new();
    let store = Arc::new(InMemoryDriverStore::new());
    let notes = work.manifest("notes", "");
    let calendar = DriverManifest::from_toml(
        "id = \"calendar\"\ncommand = \"mcp-calendar\"\npartitions = [\"work\"]\n",
    )
    .unwrap();

    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let drivers = DriverSupervisor::builder(store.clone()).build();
    drivers.mount_at_start(notes.clone(), "alice");
    drivers.mount_at_start(calendar.clone(), "alice");
    let first = kernel(work.path(), &drivers, AuditTrail::new(recorder.clone()));
    let mut events = first.events();
    assert!(drivers.mounted().is_empty(), "nothing mounts before the kernel starts");
    first.start().await.unwrap();
    assert_eq!(drivers.mounted(), ["calendar", "notes"]);
    // Mounted in the order they were approved.
    for id in ["notes", "calendar"] {
        let reason = driver_event(&mut events, id, DriverState::Stopped).await;
        assert_eq!(reason.as_deref(), Some("mounted by alice"));
    }
    let mounted: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.action == "kernel.driver.mounted")
        .collect();
    assert_eq!(mounted.len(), 2, "one approval record per driver");
    for record in &mounted {
        assert_eq!(record.ext["by"], "alice");
        assert!(record.actor.instance.as_deref().is_some_and(|id| id.starts_with("kernel-")));
    }

    // The first listing pins each tool's declaration.
    let toolset = drivers.toolset_for(&PartitionId::default(), &["notes".to_string()]);
    let ctx = RunContext::new(None, "s1", "call-1");
    toolset.tools(ctx as Arc<dyn ReadonlyContext>).await.unwrap();
    let pinned = drivers.record("notes").unwrap().manifest.declaration_digests;
    assert_eq!(pinned.len(), 6);
    first.stop().await.unwrap();

    // The next start finds both mounted as approved: nothing is mounted again, and the pins
    // and the approval stay.
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let drivers = DriverSupervisor::builder(store.clone()).build();
    for manifest in [&notes, &calendar] {
        assert!(drivers.is_mounted_as(manifest).await.unwrap(), "{}", manifest.id);
        drivers.mount_at_start(manifest.clone(), "bob");
    }
    let second = kernel(work.path(), &drivers, AuditTrail::new(recorder.clone()));
    second.start().await.unwrap();
    let record = drivers.record("notes").unwrap();
    assert_eq!(
        (record.approved_by.as_str(), &record.manifest.declaration_digests),
        ("alice", &pinned)
    );
    assert!(
        recorder.records().iter().all(|record| record.action != "kernel.driver.mounted"),
        "no new approval was recorded"
    );

    // A manifest that changed since its approval is not mounted as approved.
    let narrowed = notes.clone().with_sampling(true);
    assert!(!drivers.is_mounted_as(&narrowed).await.unwrap());
    second.stop().await.unwrap();
}

/// Mints a fixed secret per resource, for the driver's agent only.
#[derive(Debug)]
struct Vault;

#[async_trait]
impl CredentialBroker for Vault {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &url::Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        assert_eq!(acting.agent.id.as_str(), "urn:adk:agent:adk-rust/driver.notes");
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            "s3cr3t-token-value",
            resource.clone(),
            scopes.to_vec(),
        ))
    }

    async fn revoke(&self, _scope: &RevocationScope) -> adk_core::Result<u64> {
        Ok(0)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_secret_reaches_the_driver_as_a_file_and_never_in_its_environment() {
    let work = Workdir::new();
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .credential_broker(Arc::new(Vault))
        .build();
    let kernel =
        kernel(work.path(), &drivers, AuditTrail::new(Arc::new(InMemoryAuditRecorder::new())));
    kernel.start().await.unwrap();
    let mut manifest = work.manifest("notes", "");
    manifest.server.env.insert("NOTES_TOKEN".into(), "secret://notes/token".into());
    drivers.mount(manifest, "alice").await.unwrap();

    let ctx = RunContext::new(None, "s1", "call-1");
    let tools = drivers
        .toolset_for(&PartitionId::default(), &["notes".to_string()])
        .tools(ctx.clone() as Arc<dyn ReadonlyContext>)
        .await
        .unwrap();
    let report = tools.iter().find(|tool| tool.name() == "env_report").unwrap();
    let answer = report.execute(ctx, json!({})).await.unwrap();
    let text = answer["output"]
        .as_str()
        .or_else(|| answer.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| answer.to_string());
    let report: Value = serde_json::from_str(&text).unwrap_or(answer.clone());
    let env = report["env"].as_object().expect("the server reports its environment");

    assert!(!env.contains_key("NOTES_TOKEN"), "{env:?}");
    assert!(
        env.values().all(|value| !value.as_str().unwrap_or_default().contains("s3cr3t")),
        "the secret reached the environment: {env:?}"
    );
    assert!(!env.contains_key("CARGO_MANIFEST_DIR"), "the kernel's environment stays with it");
    assert_eq!(report["files"]["NOTES_TOKEN_FILE"], "s3cr3t-token-value");
    let origin = ToolOrigin::of(report_tool(&tools).as_ref());
    assert_eq!(origin.source, "driver:notes");
    kernel.stop().await.unwrap();
}

fn report_tool(tools: &[Arc<dyn adk_core::Tool>]) -> Arc<dyn adk_core::Tool> {
    tools.iter().find(|tool| tool.name() == "env_report").unwrap().clone()
}

/// The text of a driver tool's answer.
fn answer_text(answer: &Value) -> String {
    answer["output"]
        .as_str()
        .or_else(|| answer.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| answer.to_string())
}

/// What holds a driver's process tree is recorded when it starts. On Windows it is the
/// driver's Job Object, and unmounting the driver ends a process the driver started, which
/// otherwise runs on after it (spec 3.2 §10 W1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drivers_process_tree_is_recorded_and_ends_with_it() {
    let work = Workdir::new();
    let drivers =
        DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).home(work.path()).build();
    drivers.start().await.unwrap();
    drivers.mount(work.manifest("notes", UNSANDBOXED), "alice").await.unwrap();
    let ctx = RunContext::new(None, "s1", "call-1");
    let tools = drivers
        .toolset_for(&PartitionId::default(), &["notes".to_string()])
        .tools(ctx.clone() as Arc<dyn ReadonlyContext>)
        .await
        .unwrap();

    let expected = if cfg!(windows) { Some(DriverProcessTree::JobObject) } else { None };
    assert_eq!(drivers.record("notes").unwrap().process_tree, expected);

    #[cfg(windows)]
    {
        let spawn = tools.iter().find(|tool| tool.name() == "spawn_descendant").unwrap();
        let answer = spawn.execute(ctx, json!({})).await.unwrap();
        let pid: u32 = answer_text(&answer).parse().unwrap();
        assert!(!process::ends_within(pid, Duration::from_millis(500)), "the descendant started");

        assert!(drivers.unmount("notes", "alice").await.unwrap());

        let ended = process::ends_within(pid, Duration::from_secs(10));
        process::terminate(pid);
        assert!(ended, "a process the driver started outlived the driver");
    }
    #[cfg(not(windows))]
    let _ = (tools, ctx, answer_text);
    drivers.shutdown().await;
}

/// Waiting for and ending a process by its ID.
#[cfg(windows)]
mod process {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    /// Whether the process `pid` has ended, or ends within `wait`.
    pub fn ends_within(pid: u32, wait: Duration) -> bool {
        // SAFETY: plain arguments; a null handle means no such process is left.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return true;
        }
        let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open with `PROCESS_SYNCHRONIZE` and closed once.
        unsafe {
            let waited = WaitForSingleObject(process, millis);
            CloseHandle(process);
            waited == WAIT_OBJECT_0
        }
    }

    /// Ends the process `pid`, should a test leave it running.
    pub fn terminate(pid: u32) {
        // SAFETY: plain arguments; the handle is checked and closed once.
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }
}
