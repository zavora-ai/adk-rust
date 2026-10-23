//! A driver started with `sandbox = "os"` cannot write outside its allowed paths (spec 3.2
//! §7.7, K-I13): Seatbelt on macOS, bubblewrap on Linux.
//!
//! Where the platform has no working enforcer the test reports it and passes, unless
//! `ADK_SANDBOX_REQUIRE_ENFORCER=1` (the `sandbox-linux` CI job) makes it fail.

#![cfg(feature = "drivers")]

mod driver_support;

use std::sync::Arc;

use adk_core::ReadonlyContext;
use adk_kernel::PartitionId;
use adk_kernel::drivers::{DriverIsolation, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use driver_support::{RunContext, Workdir};
use serde_json::json;

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_driver() {
    driver_support::serve_stdio().await;
}

/// Whether the platform's enforcer runs here; panics when one is required and missing.
fn enforcer_available() -> bool {
    match adk_sandbox::sandbox::get_enforcer() {
        Ok(_) => true,
        Err(error) if std::env::var("ADK_SANDBOX_REQUIRE_ENFORCER").as_deref() == Ok("1") => {
            panic!("ADK_SANDBOX_REQUIRE_ENFORCER=1 and no OS sandbox enforcer runs: {error}")
        }
        Err(error) => {
            eprintln!("skipped: no OS sandbox enforcer runs here ({error})");
            false
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandboxed_driver_cannot_write_outside_its_allowed_paths() {
    if !enforcer_available() {
        return;
    }
    let work = Workdir::new();
    let outside = tempfile::tempdir().unwrap();
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .home(work.path())
        .fail_if_sandbox_unavailable(true)
        .build();
    drivers.start().await.unwrap();
    drivers.mount(work.manifest("notes", "sandbox = \"os\""), "alice").await.unwrap();

    let ctx = RunContext::new(None, "s1", "call-1");
    let tools = drivers
        .toolset_for(&PartitionId::default(), &["notes".to_string()])
        .tools(ctx.clone() as Arc<dyn ReadonlyContext>)
        .await
        .unwrap();
    let write = tools.iter().find(|tool| tool.name() == "write_file").unwrap();
    let isolation = drivers.record("notes").unwrap().isolation;
    assert!(
        matches!(isolation, Some(DriverIsolation::OsSandbox { .. })),
        "the driver runs under the OS sandbox: {isolation:?}"
    );

    let inside = work.path().join("inside.txt");
    let answer = write
        .execute(ctx.clone(), json!({ "path": inside.display().to_string(), "text": "ok" }))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&inside).unwrap(), "ok", "{answer}");

    let escape = outside.path().join("escape.txt");
    let answer = write
        .execute(ctx, json!({ "path": escape.display().to_string(), "text": "escaped" }))
        .await
        .unwrap();
    assert!(!escape.exists(), "the sandboxed driver wrote outside its allowed paths: {answer}");
    assert!(
        answer.to_string().to_lowercase().contains("not permitted")
            || answer.to_string().to_lowercase().contains("denied")
            || answer.to_string().to_lowercase().contains("read-only")
            || answer.to_string().to_lowercase().contains("no such file"),
        "the write failed with the enforcer's refusal: {answer}"
    );
    drivers.shutdown().await;
}
