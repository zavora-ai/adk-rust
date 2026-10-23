//! The Windows restricted-token enforcer confines a shell (spec 3.2 §10 W5).
//!
//! Each test runs `cmd.exe /c` through `WindowsEnforcer::run_confined` and checks the file
//! system afterwards. Scripts name paths relative to the workspace, so they need no quoting,
//! and every refusal is paired with a control that the same command form succeeds where the
//! policy allows it. They run on the PR tier's `windows` job.
#![cfg(all(windows, feature = "sandbox-windows"))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_sandbox::sandbox::SandboxPolicyBuilder;
use adk_sandbox::sandbox::windows::{ConfinedOutcome, WindowsEnforcer};

fn run_for(workspace: &Path, script: &str, timeout: Duration) -> ConfinedOutcome {
    let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    WindowsEnforcer::new()
        .run_confined(
            &policy,
            "cmd.exe".as_ref(),
            &[OsString::from("/c"), OsString::from(script)],
            workspace,
            Some(timeout),
        )
        .expect("the confined command runs")
}

fn run(workspace: &Path, script: &str) -> ConfinedOutcome {
    run_for(workspace, script, Duration::from_secs(60))
}

/// A temporary base directory holding the workspace `ws` and its siblings.
fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(root.path()).expect("canonicalize");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    (root, base, workspace)
}

/// A sibling directory only the current user can open, so the restricted token is not
/// granted it; the user (outside the sandbox) can still write it.
fn private_dir(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).expect("create the directory");
    let user = std::env::var("USERNAME").expect("USERNAME");
    let status = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/inheritance:r", "/grant:r"])
        .arg(format!("{user}:(OI)(CI)F"))
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    std::fs::write(dir.join("probe.txt"), "host").expect("the user can write the directory");
    dir
}

/// The workspace's files and their contents, for a failure message.
fn listing(workspace: &Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(workspace).into_iter().flatten().flatten() {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let content: String = content.chars().take(400).collect();
        out.push_str(&format!("{}: {content:?}\n", entry.file_name().to_string_lossy()));
    }
    out
}

#[test]
fn the_shell_writes_inside_the_workspace() {
    let (_root, _base, workspace) = layout();
    let outcome = run(&workspace, "echo ok> inside.txt");
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    let written = std::fs::read_to_string(workspace.join("inside.txt")).expect("inside.txt");
    assert_eq!(written.trim(), "ok");
}

#[test]
fn the_shell_cannot_write_outside_the_workspace() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "outside");

    run(&workspace, r"echo yes> control.txt & echo no> ..\outside\outside.txt");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "yes", "the script ran");
    assert!(!outside.join("outside.txt").exists(), "a file was written outside the workspace");
}

#[test]
fn the_shell_cannot_read_a_path_the_policy_does_not_grant() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "ungranted");
    std::fs::write(outside.join("note.txt"), "ungranted-content").expect("write the note");
    std::fs::write(workspace.join("granted.txt"), "granted-content").expect("write granted");

    run(&workspace, r"type granted.txt > control.txt & type ..\ungranted\note.txt > read.txt 2>&1");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "granted-content", "the same command reads a granted file");
    let seen = std::fs::read_to_string(workspace.join("read.txt")).expect("read.txt");
    assert!(!seen.contains("ungranted-content"), "the ungranted file was read: {seen}");
}

/// A sandboxed shell starts child processes, which inherit the restricted token: the token's
/// default DACL must let the shell open the processes it creates.
#[test]
fn the_shell_starts_a_child_process() {
    let (_root, _base, workspace) = layout();
    let outcome = run(&workspace, "cmd /c echo child^> child.txt 2> spawn-err.txt");
    let child = std::fs::read_to_string(workspace.join("child.txt")).unwrap_or_default();
    assert_eq!(child.trim(), "child", "{outcome:?}\n{}", listing(&workspace));
}

#[test]
fn no_descendant_outlives_the_call() {
    let (_root, _base, workspace) = layout();
    // A background shell appends to `tick.txt` in a long loop; the outer shell spins long
    // enough for it to start, then exits, and the call's job is terminated.
    let outcome = run(
        &workspace,
        "start /b cmd /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt 2> start-err.txt \
         & for /l %j in (1,1,1000000) do @rem",
    );
    assert!(!outcome.timed_out, "{outcome:?}");
    let tick = workspace.join("tick.txt");
    let size = std::fs::metadata(&tick)
        .unwrap_or_else(|e| panic!("the background shell ran: {e}\n{}", listing(&workspace)))
        .len();
    std::thread::sleep(Duration::from_secs(3));
    let later = std::fs::metadata(&tick).expect("tick.txt").len();
    assert_eq!(size, later, "a descendant kept running after the call");
}

#[test]
fn a_timeout_terminates_the_job() {
    let (_root, _base, workspace) = layout();
    let outcome =
        run_for(&workspace, "for /l %i in (1,1,2000000000) do @rem", Duration::from_secs(2));
    assert_eq!(outcome, ConfinedOutcome { exit_code: None, timed_out: true });
}

#[test]
fn public_reads_do_not_grant_writes() {
    for (sid, readable) in [("S-1-5-32-545", true), ("S-1-1-0", true), ("S-1-15-2-2", false)] {
        let (_root, base, workspace) = layout();
        let outside = private_dir(&base, "outside");
        let status = std::process::Command::new("icacls")
            .arg(&outside)
            .args(["/grant", &format!("*{sid}:(OI)(CI)M")])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("icacls runs");
        assert!(status.success(), "the external directory has the requested public ACL");

        run(
            &workspace,
            r"echo control> control.txt & type ..\outside\probe.txt > seen.txt 2> read-error.txt & echo changed> ..\outside\written.txt 2> write-error.txt",
        );

        assert_eq!(
            std::fs::read_to_string(workspace.join("control.txt")).unwrap().trim(),
            "control"
        );
        let seen = std::fs::read_to_string(workspace.join("seen.txt")).unwrap();
        assert_eq!(seen.contains("host"), readable, "read access for {sid}: {seen:?}");
        assert!(!outside.join("written.txt").exists(), "public {sid} access permitted a write");
        assert_eq!(std::fs::read_to_string(outside.join("probe.txt")).unwrap(), "host");
    }
}

#[test]
fn readonly_paths_reject_writes() {
    let (_root, base, workspace) = layout();
    let readonly = private_dir(&base, "readonly");
    let policy =
        SandboxPolicyBuilder::new().allow_read_write(&workspace).allow_read(&readonly).build();
    let outcome = WindowsEnforcer::new()
        .run_confined(
            &policy,
            "cmd.exe".as_ref(),
            &[
                OsString::from("/c"),
                OsString::from(
                    r"type ..\readonly\probe.txt > seen.txt & echo changed> ..\readonly\probe.txt 2> write-error.txt & echo new> ..\readonly\new.txt 2> create-error.txt",
                ),
            ],
            &workspace,
            Some(Duration::from_secs(30)),
        )
        .expect("the confined command runs");
    assert!(!outcome.timed_out, "{outcome:?}");
    assert_eq!(std::fs::read_to_string(workspace.join("seen.txt")).unwrap().trim(), "host");
    assert_eq!(std::fs::read_to_string(readonly.join("probe.txt")).unwrap(), "host");
    assert!(!readonly.join("new.txt").exists(), "a read-only path admitted file creation");
}

#[test]
fn private_owner_cannot_rewrite_acl() {
    const PRIVATE_FILE: &str = "ADK_WINDOWS_PRIVATE_FILE";
    if let Some(path) = std::env::var_os(PRIVATE_FILE) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, GetLastError, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            WRITE_DAC,
        };

        let mut wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
        // SAFETY: the path is NUL-terminated and the other pointer parameters are optional.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                WRITE_DAC,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: GetLastError reads this thread's last Win32 error.
        let error = unsafe { GetLastError() };
        if handle != INVALID_HANDLE_VALUE {
            // SAFETY: an unexpectedly successful open still returns an owned handle.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        }
        assert_eq!(handle, INVALID_HANDLE_VALUE, "ownership allowed WRITE_DAC");
        assert_eq!(error, ERROR_ACCESS_DENIED);

        // SAFETY: the private fixture is owned by this test. A null DACL would remove its
        // protection, so this direct attempt must fail even though the caller owns the file.
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, ERROR_ACCESS_DENIED, "ownership allowed replacing the private DACL");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::write(&path, "changed").unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        std::fs::write("permissions-checked.txt", "checked")
            .expect("the writable control succeeds");
        return;
    }

    let (root, base, workspace) = layout();
    private_dir(&base, "private");
    let private = root.path().join("private").join("probe.txt");
    let executable = std::env::current_exe().expect("the test executable exists");
    let mut policy =
        SandboxPolicyBuilder::new().allow_read_write(&workspace).allow_read(&executable).build();
    policy.env.insert(PRIVATE_FILE.into(), private.display().to_string());
    let outcome = WindowsEnforcer::new()
        .run_confined(
            &policy,
            executable.as_os_str(),
            &[
                OsString::from("--exact"),
                OsString::from("private_owner_cannot_rewrite_acl"),
                OsString::from("--nocapture"),
            ],
            &workspace,
            Some(Duration::from_secs(30)),
        )
        .expect("the confined test actor runs");
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("permissions-checked.txt")).unwrap(),
        "checked"
    );
    assert_eq!(std::fs::read_to_string(private).unwrap(), "host");
}
