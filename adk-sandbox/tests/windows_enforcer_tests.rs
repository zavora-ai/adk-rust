//! The Windows restricted-token enforcer confines a shell (spec 3.2 §10 W5).
//!
//! Each test runs `cmd.exe /c` through `WindowsEnforcer::run_confined`, or through
//! `WindowsEnforcer::spawn_contained` with its standard streams on pipes, and checks the file
//! system afterwards. Scripts name paths relative to the workspace, so they need no quoting,
//! and every refusal is paired with a control that the same command form succeeds where the
//! policy allows it. They run on the PR tier's `windows` job.
#![cfg(all(windows, feature = "sandbox-windows"))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::io::{Read as _, Write as _};

use adk_sandbox::process_tree::ContainedChild;
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

/// The user is a restricting SID (MSYS2's objects name it), so the token reads what the user
/// may read; a path only `Administrators` and `SYSTEM` may read stays closed, since the token
/// holds `Administrators` deny-only.
#[test]
fn the_shell_cannot_read_a_path_only_administrators_may_read() {
    let (_root, base, workspace) = layout();
    let outside = base.join("administrators");
    std::fs::create_dir_all(&outside).expect("create the directory");
    std::fs::write(outside.join("note.txt"), "ungranted-content").expect("write the note");
    let status = std::process::Command::new("icacls")
        .arg(&outside)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-32-544:(OI)(CI)F",
            "*S-1-5-18:(OI)(CI)F",
            "/T",
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls runs");
    assert!(status.success(), "icacls set the directory's ACL");
    std::fs::write(workspace.join("granted.txt"), "granted-content").expect("write granted");

    run(
        &workspace,
        r"type granted.txt > control.txt & type ..\administrators\note.txt > read.txt 2>&1",
    );

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "granted-content", "the same command reads a granted file");
    let seen = std::fs::read_to_string(workspace.join("read.txt")).expect("read.txt");
    assert!(!seen.contains("ungranted-content"), "the ungranted file was read: {seen}");
}

/// Git for Windows' `bash` (the MSYS2 runtime) starts under the token and writes the workspace
/// but not a directory beside it (spec 3.2 §10 W5).
#[tokio::test]
async fn git_bash_starts_under_the_token_and_writes_only_the_workspace() {
    let bash = Path::new(r"C:\Program Files\Git\usr\bin\bash.exe");
    if !bash.is_file() {
        assert!(std::env::var_os("CI").is_none(), "Git for Windows is on every CI runner");
        eprintln!("skipped: Git for Windows is not installed");
        return;
    }
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "outside");
    let policy = SandboxPolicyBuilder::new().allow_read_write(&workspace).build();
    let escaped = outside.join("escaped.txt").display().to_string().replace('\\', "/");
    let script = format!("echo inside > inside.txt; echo out > '{escaped}'; echo ran");
    let mut child = WindowsEnforcer::new()
        .spawn_contained(
            &policy,
            bash.as_os_str(),
            &[OsString::from("-c"), OsString::from(script)],
            &workspace,
        )
        .expect("bash starts");
    drop(child.stdin.take());
    let (mut stdout, mut stderr) = (String::new(), String::new());
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");
    child.stderr.take().expect("piped").read_to_string(&mut stderr).expect("stderr");
    let code = child.process.wait().await.expect("waited");

    assert_eq!(stdout.trim(), "ran", "exit {code}, stderr {stderr}");
    let inside = std::fs::read_to_string(workspace.join("inside.txt")).expect("inside.txt");
    assert_eq!(inside.trim(), "inside");
    assert!(!outside.join("escaped.txt").exists(), "a write landed outside the workspace");
}

/// Git for Windows' installation on the CI runners.
const GIT: &str = r"C:\Program Files\Git";

/// Serialises the tests that start unconfined MSYS2 programs or read the user's MSYS2
/// namespace, so one cannot hold the namespace the other checks.
static USER_NAMESPACE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Whether Git for Windows is installed; on CI it must be.
fn git_installed() -> bool {
    let installed = Path::new(GIT).join(r"usr\bin\bash.exe").is_file();
    assert!(installed || std::env::var_os("CI").is_none(), "Git for Windows is on every CI runner");
    installed
}

/// An unconfined Git `bash` of the user, up and holding the user's MSYS2 namespace, ended
/// with its job when dropped.
struct UsersBash {
    _job: adk_sandbox::process_tree::JobObject,
    _child: std::process::Child,
}

fn users_bash() -> UsersBash {
    use std::os::windows::process::CommandExt as _;
    let mut child = std::process::Command::new(Path::new(GIT).join(r"usr\bin\bash.exe"))
        .args(["-c", "echo up; exec sleep 120"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(adk_sandbox::process_tree::CREATE_SUSPENDED)
        .spawn()
        .expect("the user's bash starts");
    let job = adk_sandbox::process_tree::JobObject::contain_suspended(child.id()).expect("job");
    let mut up = [0u8; 2];
    child.stdout.take().expect("piped").read_exact(&mut up).expect("the user's bash is up");
    assert_eq!(&up, b"up");
    UsersBash { _job: job, _child: child }
}

/// Runs `program` with `args` contained, under `policy`, and returns its exit code, standard
/// output and standard error.
fn contained(
    policy: &adk_sandbox::sandbox::SandboxPolicy,
    program: &Path,
    args: &[&str],
    cwd: &Path,
) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut child = WindowsEnforcer::new()
        .spawn_contained(policy, program.as_os_str(), &args, cwd)
        .expect("the contained program starts");
    drop(child.stdin.take());
    let (mut stdout, mut stderr) = (String::new(), String::new());
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");
    child.stderr.take().expect("piped").read_to_string(&mut stderr).expect("stderr");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let code = runtime.block_on(child.process.wait()).expect("waited");
    (code, stdout, stderr)
}

/// A Git `bash` of the user's own holds the user's MSYS2 namespace, which the Low token
/// cannot write: the confined `bash` runs from a private copy of the runtime, in a namespace
/// of its own, and the programs it starts run there too.
#[test]
fn git_bash_starts_beside_the_users_own_git_bash() {
    if !git_installed() {
        return;
    }
    let _serial = USER_NAMESPACE.lock().unwrap_or_else(|e| e.into_inner());
    let _users = users_bash();
    let (_root, _base, workspace) = layout();
    let policy = SandboxPolicyBuilder::new().allow_read_write(&workspace).build();

    for shell in [r"usr\bin\bash.exe", r"bin\sh.exe"] {
        let (code, stdout, stderr) = contained(
            &policy,
            &Path::new(GIT).join(shell),
            &["-c", "sleep 1; ls -d / >/dev/null; echo child > child.txt; echo ran"],
            &workspace,
        );
        assert_eq!(stdout.trim(), "ran", "{shell}: exit {code:#x}, stderr {stderr}");
        let written = std::fs::read_to_string(workspace.join("child.txt")).expect("child.txt");
        assert_eq!(written.trim(), "child", "{shell}");
    }
}

/// The MSYS2 runtime's name for the user's namespace of the installation whose runtime is
/// `dll`: its `hash_path_name` of the `\??\` path, as `init_installation_root` computes it.
fn users_namespace(dll: &Path) -> String {
    let mut hash: u64 = 0;
    for unit in format!(r"\??\{}", dll.display()).encode_utf16() {
        let upper = char::from_u32(u32::from(unit))
            .map(char::to_uppercase)
            .and_then(|mut upper| match (upper.next(), upper.next()) {
                (Some(single), None) => Some(u64::from(u32::from(single))),
                _ => None,
            })
            .unwrap_or(u64::from(unit));
        hash = upper.wrapping_add(hash << 6).wrapping_add(hash << 16).wrapping_sub(hash);
    }
    format!("msys-2.0S5-{hash:x}")
}

#[test]
fn the_users_namespace_name_is_the_one_the_runtime_reports() {
    // `NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0S5-1888ae32e00d56aa)` in the runner's
    // error before this fix.
    assert_eq!(
        users_namespace(&Path::new(GIT).join(r"usr\bin\msys-2.0.dll")),
        "msys-2.0S5-1888ae32e00d56aa"
    );
}

/// The mandatory label of the NT object directory `name`: `Ok(None)` for none (Medium), the
/// level's RID otherwise, and the `NTSTATUS` when it cannot be opened.
fn directory_label(name: &str) -> Result<Option<u32>, i32> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        ACE_HEADER, ACL, GetAce, GetSidSubAuthority, GetSidSubAuthorityCount,
        LABEL_SECURITY_INFORMATION, PSID, SYSTEM_MANDATORY_LABEL_ACE,
    };

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: HANDLE,
        object_name: *const UnicodeString,
        attributes: u32,
        security_descriptor: *const std::ffi::c_void,
        security_quality_of_service: *const std::ffi::c_void,
    }
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        fn NtOpenDirectoryObject(
            handle: *mut HANDLE,
            access: u32,
            attributes: *const ObjectAttributes,
        ) -> i32;
    }
    const READ_CONTROL: u32 = 0x0002_0000;
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;

    let wide: Vec<u16> = name.encode_utf16().collect();
    let unicode = UnicodeString {
        length: (wide.len() * 2) as u16,
        maximum_length: (wide.len() * 2) as u16,
        buffer: wide.as_ptr(),
    };
    let attributes = ObjectAttributes {
        length: size_of::<ObjectAttributes>() as u32,
        root_directory: std::ptr::null_mut(),
        object_name: &unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null(),
        security_quality_of_service: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: the attributes and the name they point to outlive the call; `handle` is writable.
    let status = unsafe { NtOpenDirectoryObject(&mut handle, READ_CONTROL, &attributes) };
    if status < 0 {
        return Err(status);
    }
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `handle` is open with READ_CONTROL; the out-pointers are writable.
    let error = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sacl,
            &mut descriptor,
        )
    };
    // SAFETY: `handle` is open and owned here.
    unsafe { CloseHandle(handle) };
    assert_eq!(error, 0, "GetSecurityInfo({name})");
    let mut label = None;
    if !sacl.is_null() {
        // SAFETY: `sacl` points into `descriptor`, which is freed below.
        for index in 0..u32::from(unsafe { (*sacl).AceCount }) {
            let mut ace: *mut std::ffi::c_void = std::ptr::null_mut();
            // SAFETY: `index` is below the ACL's entry count.
            if unsafe { GetAce(sacl, index, &mut ace) } == 0 {
                continue;
            }
            // SAFETY: every entry starts with an ACE header.
            if unsafe { (*ace.cast::<ACE_HEADER>()).AceType } != SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                continue;
            }
            // SAFETY: a mandatory label entry's SID starts at its `SidStart`.
            let sid: PSID =
                unsafe { (&raw mut (*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>()).SidStart).cast() };
            // SAFETY: `sid` is a valid SID with at least one sub-authority.
            let count = unsafe { *GetSidSubAuthorityCount(sid) };
            // SAFETY: the last sub-authority is within the SID.
            label = Some(unsafe { *GetSidSubAuthority(sid, u32::from(count) - 1) });
        }
    }
    // SAFETY: `descriptor` was allocated by `GetSecurityInfo`.
    unsafe { LocalFree(descriptor) };
    Ok(label)
}

/// A program outside MSYS2 that starts Git's `bash` by its installation's path, as `git.exe`
/// starts a hook's shell, loads the installation's own runtime. The call holds the user's
/// namespace of that installation at the user's integrity level, so the `bash` cannot create
/// it at Low integrity for the user's next Git Bash to join, and does not start.
#[test]
fn a_contained_call_never_creates_the_users_msys2_namespace_at_low() {
    if !git_installed() {
        return;
    }
    let _serial = USER_NAMESPACE.lock().unwrap_or_else(|e| e.into_inner());
    let (_root, _base, workspace) = layout();
    std::fs::write(
        workspace.join("fresh.cmd"),
        "@\"C:\\Program Files\\Git\\usr\\bin\\bash.exe\" -c \"echo fresh; exec sleep 8\"\r\n\
         @echo after\r\n\
         @ping -n 7 127.0.0.1 >nul\r\n",
    )
    .unwrap();
    let system = std::env::var("SystemRoot").expect("SystemRoot");
    let policy = SandboxPolicyBuilder::new()
        .allow_read_write(&workspace)
        .env("PATH", format!(r"{system}\System32;{GIT}\cmd"))
        .build();
    // The runtime opens this directory first and stops at once when it cannot write it, so
    // it is where a Low-integrity namespace would begin.
    let directories = vec![format!(
        r"\BaseNamedObjects\{}",
        users_namespace(&Path::new(GIT).join(r"usr\bin\msys-2.0.dll"))
    )];

    let checker = {
        let directories = directories.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            directories.iter().map(|dir| (dir.clone(), directory_label(dir))).collect::<Vec<_>>()
        })
    };
    let cmd = Path::new(&system).join(r"System32\cmd.exe");
    let (code, stdout, stderr) = contained(&policy, &cmd, &["/d", "/c", "fresh.cmd"], &workspace);
    let labels = checker.join().expect("the check ran");

    for (dir, label) in labels {
        match label {
            Ok(Some(level)) => assert!(level >= 0x2000, "{dir} is labelled {level:#x}"),
            Ok(None) => {}
            Err(status) => panic!("{dir} is held while the call runs, but: {status:#x}"),
        }
    }
    assert!(!stdout.contains("fresh"), "bash started in the user's namespace: {stdout}");
    assert!(stdout.contains("after"), "exit {code:#x}, stdout {stdout}, stderr {stderr}");
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

/// Starts `script` under `cmd.exe /d /c` with `spawn_contained`, the workspace read-write.
fn spawn(workspace: &Path, script: &str) -> ContainedChild {
    let policy = SandboxPolicyBuilder::new().allow_read_write(workspace).build();
    let system = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    WindowsEnforcer::new()
        .spawn_contained(
            &policy,
            system.join("System32").join("cmd.exe").as_os_str(),
            &[OsString::from("/d"), OsString::from("/c"), OsString::from(script)],
            workspace,
        )
        .expect("the contained command starts")
}

#[test]
fn the_probe_runs_a_command_under_the_restricted_token() {
    WindowsEnforcer::new().probe_confined().expect("the restricted-token path is available");
}

#[tokio::test]
async fn a_contained_command_streams_its_output_and_reports_its_exit_code() {
    let (_root, _base, workspace) = layout();
    let mut child = spawn(&workspace, "echo out& echo err 1>&2& exit /b 7");
    drop(child.stdin.take());
    let (mut stdout, mut stderr) = (String::new(), String::new());
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");
    child.stderr.take().expect("piped").read_to_string(&mut stderr).expect("stderr");

    assert_eq!(child.process.wait().await.expect("waited"), 7);
    assert_eq!((stdout.trim(), stderr.trim()), ("out", "err"));
}

#[tokio::test]
async fn a_contained_command_reads_its_standard_input() {
    let (_root, _base, workspace) = layout();
    // `sort` echoes its input and takes no quoted argument: `cmd /c` rereads the quoted
    // script, so an inner `"` would reach the program escaped as `\"`.
    let mut child = spawn(&workspace, "sort");
    let mut stdin = child.stdin.take().expect("piped");
    stdin.write_all(b"hello\r\n").expect("write stdin");
    drop(stdin);
    let mut stdout = String::new();
    child.stdout.take().expect("piped").read_to_string(&mut stdout).expect("stdout");

    assert_eq!(child.process.wait().await.expect("waited"), 0);
    assert_eq!(stdout.trim(), "hello");
}

#[tokio::test]
async fn a_contained_command_is_confined_to_its_grants() {
    let (_root, base, workspace) = layout();
    let outside = private_dir(&base, "outside");
    let mut child = spawn(&workspace, r"echo yes> control.txt & echo no> ..\outside\outside.txt");
    drop(child.stdin.take());

    child.process.wait().await.expect("waited");

    let control = std::fs::read_to_string(workspace.join("control.txt")).expect("control.txt");
    assert_eq!(control.trim(), "yes", "the script ran");
    assert!(!outside.join("outside.txt").exists(), "a file was written outside the workspace");
}

#[tokio::test]
async fn dropping_a_contained_child_ends_every_process_it_started() {
    let (_root, _base, workspace) = layout();
    let mut child = spawn(
        &workspace,
        "start /b cmd /d /c for /l %i in (1,1,2000000000) do @echo x^>^> tick.txt \
         & ping -n 300 127.0.0.1 >nul",
    );
    drop(child.stdin.take());
    let tick = workspace.join("tick.txt");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !tick.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(child.process.try_wait().expect("queried"), None, "the shell still runs");

    let process = child.process.clone();
    drop(child);

    tokio::time::timeout(Duration::from_secs(10), process.wait())
        .await
        .expect("the shell ended with its job")
        .expect("waited");
    let size = std::fs::metadata(&tick).expect("the background loop ran").len();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        std::fs::metadata(&tick).expect("tick.txt").len(),
        size,
        "a descendant kept running"
    );
}
