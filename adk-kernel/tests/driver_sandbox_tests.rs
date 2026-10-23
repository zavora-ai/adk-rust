//! A driver started with `sandbox = "os"` cannot write outside its allowed paths (spec 3.2
//! §7.7, K-I13): Seatbelt on macOS, bubblewrap on Linux. A driver whose program lives
//! outside the system directories — reached through a link, or a script whose interpreter
//! is elsewhere — starts under the sandbox and is confined the same way.
//!
//! Where the platform has no working enforcer the tests report it and pass, unless
//! `ADK_SANDBOX_REQUIRE_ENFORCER=1` (the `sandbox-linux` CI job) makes them fail.

#![cfg(all(feature = "drivers", unix))]

mod driver_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::ReadonlyContext;
use adk_kernel::PartitionId;
use adk_kernel::drivers::{DriverIsolation, DriverManifest, DriverSupervisor};
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
    let toolchain = tempfile::tempdir().unwrap();
    let interpreter = toolchain.path().join("sh");
    std::fs::copy("/bin/sh", &interpreter).unwrap();
    std::fs::set_permissions(&interpreter, std::fs::Permissions::from_mode(0o755)).unwrap();
    let scripts = tempfile::tempdir().unwrap();
    let script = scripts.path().join("notes-server");
    let exe = std::env::current_exe().unwrap();
    std::fs::write(
        &script,
        format!("#!{}\nexec '{}' \"$@\"\n", interpreter.display(), exe.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The binary the script runs is a file the server reads, so the manifest lists it, and
    // its loader: under a Nix toolchain that is the store.
    let mut read = vec![exe.parent().unwrap()];
    if Path::new("/nix/store").is_dir() {
        read.push(Path::new("/nix/store"));
    }
    let isolation =
        assert_confined(&work, work.manifest_with("notes", &script, &read, "sandbox = \"os\""))
            .await;
    let opened = program_paths(&isolation);
    assert!(
        opened.contains(&canonical(toolchain.path())),
        "the interpreter's directory is opened: {isolation:?}"
    );
    assert!(opened.contains(&canonical(scripts.path())), "and the script's: {isolation:?}");
}
