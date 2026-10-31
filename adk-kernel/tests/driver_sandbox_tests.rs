//! A driver started with `sandbox = "os"` cannot write outside its allowed paths (spec 3.2
//! §7.7, K-I13): Seatbelt on macOS, bubblewrap on Linux. A driver whose program lives
//! outside the system directories — reached through a link, or a script whose interpreter
//! is elsewhere — starts under the sandbox and is confined the same way. What a sandboxed
//! driver starts has exited when unmounting it returns (K-I5).
//!
//! Where the platform has no working enforcer the tests report it and pass, unless
//! `ADK_SANDBOX_REQUIRE_ENFORCER=1` (the `sandbox-linux` CI job) makes them fail.

#![cfg(all(feature = "drivers", unix))]

use crate::driver_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::ReadonlyContext;
use adk_kernel::PartitionId;
use adk_kernel::drivers::{DriverIsolation, DriverManifest, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use driver_support::{RunContext, Workdir};
use serde_json::json;

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

/// Mounts `manifest` under the OS sandbox, starts it through a tool call, and checks a
/// write inside the work directory lands while one outside is refused. Returns how the
/// driver was isolated.
async fn assert_confined(work: &Workdir, manifest: DriverManifest) -> DriverIsolation {
    let outside = tempfile::tempdir().unwrap();
    let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
        .home(work.path())
        .fail_if_sandbox_unavailable(true)
        .build();
    drivers.start().await.unwrap();
    drivers.mount(manifest, "alice").await.unwrap();

    let ctx = RunContext::new(None, "s1", "call-1");
    let tools = drivers
        .toolset_for(&PartitionId::default(), &["notes".to_string()])
        .tools(ctx.clone() as Arc<dyn ReadonlyContext>)
        .await
        .expect("the driver starts under the sandbox and lists its tools");
    let write = tools.iter().find(|tool| tool.name() == "write_file").unwrap();
    let isolation = drivers.record("notes").unwrap().isolation.unwrap();
    assert!(isolation.is_sandboxed(), "the driver runs under the OS sandbox: {isolation:?}");

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
    let refusal = answer.to_string().to_lowercase();
    assert!(
        ["not permitted", "denied", "read-only", "no such file"]
            .iter()
            .any(|word| refusal.contains(word)),
        "the write failed with the enforcer's refusal: {answer}"
    );
    drivers.shutdown().await;
    isolation
}

/// The directories the sandbox opened for the driver's program, as its isolation records
/// them.
fn program_paths(isolation: &DriverIsolation) -> Vec<PathBuf> {
    match isolation {
        DriverIsolation::OsSandbox { program_paths, .. } => program_paths.clone(),
        other => panic!("not sandboxed: {other:?}"),
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandboxed_driver_cannot_write_outside_its_allowed_paths() {
    if !enforcer_available() {
        return;
    }
    let work = Workdir::new();
    let isolation = assert_confined(&work, work.manifest("notes", "sandbox = \"os\"")).await;
    let exe = canonical(&std::env::current_exe().unwrap());
    assert!(
        program_paths(&isolation).contains(&exe.parent().unwrap().to_path_buf()),
        "the program's directory is recorded: {isolation:?}"
    );
}

/// A program installed as a link in a directory of links — `~/.cargo/bin`, `/opt/<app>/bin`,
/// Homebrew's `bin` — runs from where the link points.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_driver_reached_through_a_link_outside_the_system_directories_starts() {
    if !enforcer_available() {
        return;
    }
    let work = Workdir::new();
    let links = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let link = links.path().join("notes-server");
    std::os::unix::fs::symlink(&exe, &link).unwrap();
    let isolation =
        assert_confined(&work, work.manifest_with("notes", &link, &[], "sandbox = \"os\"")).await;
    assert!(
        program_paths(&isolation).contains(&canonical(&exe).parent().unwrap().to_path_buf()),
        "the link's target directory is opened: {isolation:?}"
    );
}

/// A script whose interpreter lives outside the system directories, as a server run by a
/// language toolchain in the user's home does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_script_driver_whose_interpreter_is_outside_the_system_directories_starts() {
    if !enforcer_available() {
        return;
    }
    use std::os::unix::fs::PermissionsExt as _;
    let work = Workdir::new();
    // The interpreter is a copy of this test binary, which serves the driver: the script's
    // path reaches it as one more test-name filter. A copy of `/bin/sh` cannot stand in,
    // since macOS launch constraints kill a system binary started from another directory.
    let toolchain = tempfile::tempdir().unwrap();
    let interpreter = toolchain.path().join("notes-interpreter");
    std::fs::copy(std::env::current_exe().unwrap(), &interpreter).unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scripts = tempfile::tempdir().unwrap();
    let script = scripts.path().join("notes-server");
    std::fs::write(&script, format!("#!{}\n", interpreter.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let isolation =
        assert_confined(&work, work.manifest_with("notes", &script, &[], "sandbox = \"os\"")).await;
    let opened = program_paths(&isolation);
    assert!(
        opened.contains(&canonical(toolchain.path())),
        "the interpreter's directory is opened: {isolation:?}"
    );
    assert!(opened.contains(&canonical(scripts.path())), "and the script's: {isolation:?}");
}

/// A sandboxed driver's descendants end with it (spec 3.2 §7.7, K-I5). The descendant
/// appends to a file every 20 ms. Under Seatbelt it stays in the driver's process group, so
/// it has exited when the unmount returns and the file stops growing then. Under bubblewrap it
/// runs in the sandbox's own session and PID namespace, which `--die-with-parent` kills as the
/// group ends, so it may finish exiting just after the unmount returns; the file stops
/// growing within 100 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sandboxed_drivers_descendants_have_exited_when_the_unmount_returns() {
    if !enforcer_available() {
        return;
    }
    let work = Workdir::new();
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
        .expect("the driver starts under the sandbox and lists its tools");
    let isolation = drivers.record("notes").unwrap().isolation.unwrap();
    assert!(isolation.is_sandboxed(), "the driver runs under the OS sandbox: {isolation:?}");

    let heartbeat = work.path().join("heartbeat.log");
    let beats = || std::fs::metadata(&heartbeat).map_or(0, |metadata| metadata.len());
    let spawn = tools.iter().find(|tool| tool.name() == "spawn_descendant").unwrap();
    let answer =
        spawn.execute(ctx, json!({ "heartbeat": heartbeat.display().to_string() })).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while beats() < 6 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(beats() >= 6, "the descendant writes under the sandbox: {answer}");

    assert!(drivers.unmount("notes", "alice").await.unwrap());

    let at_return = beats();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = beats();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(beats(), settled, "a descendant of the driver outlived the unmount");
    if matches!(&isolation, DriverIsolation::OsSandbox { enforcer, .. } if enforcer == "seatbelt") {
        assert_eq!(
            settled, at_return,
            "a descendant of the driver wrote after the unmount returned"
        );
    }
    drivers.shutdown().await;
}
