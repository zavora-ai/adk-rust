//! MSYS2 programs under the restricted token (spec 3.2 §10 W5).
//!
//! The MSYS2 runtime under Git for Windows' `sh` keeps its shared memory, the mount table
//! among it, in an object namespace named after a hash of the path of `msys-2.0.dll`, and a
//! process it starts inherits that name. A Low-integrity token cannot write a namespace the
//! user's unconfined MSYS2 programs hold, so the shell would exit at start; and a namespace a
//! sandboxed command created first would be joined by the MSYS2 programs the user starts
//! next, which would then read a mount table the sandbox wrote. A call that starts an MSYS2
//! program therefore:
//!
//! 1. runs it from a private copy of the runtime — `msys-2.0.dll` and the interpreters
//!    scripts name, copied for the call into an owner-only, Medium-integrity directory and
//!    compared with the installation's files before use — whose `etc\fstab` roots it at the
//!    installation, so the call's MSYS2 processes share a namespace with no other process;
//! 2. holds the user's namespace of every MSYS2 installation it can reach — the program's and
//!    those on `PATH` — with a keeper, an unconfined `cat` of that installation started before
//!    the call, so the Low token can neither create nor write it.

#![cfg_attr(not(all(target_os = "windows", feature = "sandbox-windows")), allow(dead_code))]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The MSYS2 runtime, beside every MSYS2 program in an installation's `usr\bin`.
const RUNTIME_DLL: &str = "msys-2.0.dll";

/// Interpreters copied beside the private runtime and first on the call's `PATH`, so a
/// program outside MSYS2 that looks one up by name, as `git` does for a hook's `#!/bin/sh`,
/// starts it in the call's namespace.
const INTERPRETERS: [&str; 3] = ["sh.exe", "bash.exe", "env.exe"];

/// Git for Windows' environment directories and the `MSYSTEM` its launchers set for each.
const ENVIRONMENTS: [(&str, &str); 3] =
    [("mingw64", "MINGW64"), ("clangarm64", "CLANGARM64"), ("mingw32", "MINGW32")];

/// The variable that switches the runtime to a testing namespace; only the first process of a
/// tree reads it, so it would split the call's processes across two namespaces.
const TESTING_VARIABLE: &str = "CYGWIN_TESTING";

/// An MSYS2 installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Installation {
    /// The installation root, which holds `usr` and `etc`.
    pub(super) root: PathBuf,
    /// Its `usr\bin`, which holds the runtime and the programs.
    pub(super) bin: PathBuf,
}

impl Installation {
    /// The installation whose `usr\bin` is `dir`, or whose launchers `dir` holds, as Git for
    /// Windows' `bin` and `cmd` do.
    pub(super) fn of_dir(dir: &Path) -> Option<Self> {
        if !dir.is_absolute() {
            return None;
        }
        let bin = [dir.to_path_buf(), dir.join("..").join("usr").join("bin")]
            .into_iter()
            .find(|bin| bin.join(RUNTIME_DLL).is_file())?;
        let bin = super::strip_unc_prefix(&std::fs::canonicalize(bin).ok()?);
        let root = bin.parent()?.parent()?.to_path_buf();
        Some(Self { root, bin })
    }
}

/// An MSYS2 program a call starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Program {
    /// Its installation.
    pub(super) installation: Installation,
    /// Its file name in the installation's `usr\bin`.
    pub(super) name: OsString,
    /// Set when the caller named a Git for Windows launcher (`bin\sh.exe`), which starts the
    /// program of the same name in `usr\bin` after setting `MSYSTEM` and putting its
    /// environment's `bin` and `usr\bin` first on `PATH`; the call does the same.
    pub(super) launcher: Option<Launcher>,
}

/// What a Git for Windows launcher sets before it starts its program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Launcher {
    /// `MSYSTEM`, when the installation has a known environment directory.
    pub(super) msystem: Option<&'static str>,
    /// The directories it puts first on `PATH`, in order.
    pub(super) path: Vec<PathBuf>,
}

/// The MSYS2 program at `path`, or the one a Git for Windows launcher at `path` starts.
pub(super) fn program(path: &Path) -> Option<Program> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_os_string();
    let installation = Installation::of_dir(dir)?;
    let dir = super::strip_unc_prefix(&std::fs::canonicalize(dir).ok()?);
    if same_path(&dir, &installation.bin) {
        return Some(Program { installation, name, launcher: None });
    }
    if !installation.bin.join(&name).is_file() {
        return None;
    }
    let environment = ENVIRONMENTS.iter().find_map(|(dir, msystem)| {
        let bin = installation.root.join(dir).join("bin");
        bin.is_dir().then_some((*msystem, bin))
    });
    let mut path: Vec<PathBuf> = environment.iter().map(|(_, bin)| bin.clone()).collect();
    path.push(installation.bin.clone());
    let launcher = Launcher { msystem: environment.map(|(msystem, _)| msystem), path };
    Some(Program { installation, name, launcher: Some(launcher) })
}

/// The files of the private runtime for `program`: the runtime, the program and the
/// interpreters the installation has, without repeats.
pub(super) fn private_files(program: &Program) -> Vec<OsString> {
    let mut files: Vec<OsString> = vec![RUNTIME_DLL.into(), program.name.clone()];
    for interpreter in INTERPRETERS {
        if program.installation.bin.join(interpreter).is_file()
            && !files.iter().any(|file| file.eq_ignore_ascii_case(interpreter))
        {
            files.push(interpreter.into());
        }
    }
    files
}

/// The private runtime's `etc\fstab`: the installation's root mounted over `/`, which the
/// runtime otherwise derives from its own location, then the installation's own entries.
pub(super) fn fstab(root: &Path, installed: &str) -> String {
    let root = root.display().to_string().replace('\\', "/").replace(' ', "\\040");
    format!(
        "# The private MSYS2 runtime of a sandboxed call, rooted at the installation.\n\
         {root} / ntfs override,binary,noacl,posix=0 0 0\n{installed}"
    )
}

/// Adjusts the call's environment `pairs` to start `program` from the private runtime in
/// `private_bin`: the private runtime first on `PATH`, then a launcher's directories and its
/// `MSYSTEM`, and no `CYGWIN_TESTING`.
pub(super) fn environment(
    pairs: &mut Vec<(OsString, OsString)>,
    private_bin: &Path,
    program: &Program,
) {
    pairs.retain(|(k, _)| !k.eq_ignore_ascii_case(TESTING_VARIABLE));
    let mut first = vec![private_bin.to_path_buf()];
    if let Some(launcher) = &program.launcher {
        first.extend(launcher.path.iter().cloned());
        if let Some(msystem) = launcher.msystem {
            set(pairs, "MSYSTEM", msystem.into());
        }
    }
    let mut path = OsString::new();
    for dir in &first {
        path.push(dir.as_os_str());
        path.push(";");
    }
    if let Some((_, old)) = pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")) {
        path.push(old);
    }
    set(pairs, "PATH", path);
}

/// The installations of the directories on `PATH` in `pairs`, without repeats.
pub(super) fn installations_on_path(pairs: &[(OsString, OsString)]) -> Vec<Installation> {
    let mut found: Vec<Installation> = Vec::new();
    let Some((_, path)) = pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")) else {
        return found;
    };
    for dir in path.to_string_lossy().split(';').filter(|dir| !dir.is_empty()) {
        if let Some(installation) = Installation::of_dir(Path::new(dir))
            && !found.iter().any(|known| same_path(&known.bin, &installation.bin))
        {
            found.push(installation);
        }
    }
    found
}

/// Replaces the value of `name`, matched without regard to case, or adds it.
fn set(pairs: &mut Vec<(OsString, OsString)>, name: &str, value: OsString) {
    match pairs.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
        Some((_, old)) => *old = value,
        None => pairs.push((name.into(), value)),
    }
}

/// Whether two Windows paths name the same location, without regard to case.
pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

/// Whether `inner` is `outer` or lies beneath it, without regard to case.
pub(super) fn within(inner: &Path, outer: &Path) -> bool {
    let (inner, outer) = (lower(inner), lower(outer));
    inner == outer || inner.starts_with(&format!("{outer}\\"))
}

/// `path` in lower case, without a trailing separator.
fn lower(path: &Path) -> String {
    path.as_os_str().to_string_lossy().to_lowercase().trim_end_matches('\\').to_string()
}

/// `name` as a string, for messages.
fn shown(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
pub(super) use os::{Keeper, PrivateRuntime};

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
mod os {
    use std::ffi::OsString;
    use std::io::{Read as _, Write as _};
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    use super::{Installation, Program, fstab, private_files, shown, within};
    use crate::error::SandboxError;
    use crate::process_tree::JobObject;
    use crate::sandbox::windows::ENFORCER_NAME;
    use crate::sandbox::windows::imp::{
        ChildStdio, command_line, create_suspended, environment_block, pipe, protect_owner_only,
    };

    /// `CREATE_NO_WINDOW`: the keeper gets no console window.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// How long a keeper may take to start.
    const KEEPER_START: Duration = Duration::from_secs(30);

    /// A private runtime is removed by a later call once its marker is free and it is older
    /// than this, which covers the moment between its creation and its marker's.
    const STALE_AFTER: Duration = Duration::from_secs(60);

    /// The marker a call holds open, without delete sharing, while its private runtime is in use.
    const IN_USE: &str = "in-use";

    /// `FILE_SHARE_READ`: the marker can be read, not deleted, while it is held.
    const FILE_SHARE_READ: u32 = 0x1;

    fn failed(message: String) -> SandboxError {
        SandboxError::EnforcerFailed { enforcer: ENFORCER_NAME.to_string(), message }
    }

    /// The per-user directory that holds the calls' private runtimes.
    fn base() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("adk-sandbox")
            .join("msys2")
    }

    /// A call's private copy of an MSYS2 runtime, removed when the call's processes end.
    #[derive(Debug)]
    pub(crate) struct PrivateRuntime {
        dir: PathBuf,
        /// The copy's `usr\bin`.
        pub(crate) bin: PathBuf,
        /// The held marker, so no other call removes the runtime while it is in use.
        in_use: Option<std::fs::File>,
    }

    impl PrivateRuntime {
        /// Copies `program`'s runtime into a new directory under the per-user base, which is
        /// owner-only, Medium integrity and outside every read-write path in `granted`, and
        /// compares each copy with the installation's file before it is used.
        pub(crate) fn create(program: &Program, granted: &[PathBuf]) -> Result<Self, SandboxError> {
            let base = base();
            std::fs::create_dir_all(&base)
                .map_err(|error| failed(format!("cannot create {}: {error}", base.display())))?;
            let base =
                crate::sandbox::windows::strip_unc_prefix(&std::fs::canonicalize(&base).map_err(
                    |error| failed(format!("cannot resolve {}: {error}", base.display())),
                )?);
            if let Some(grant) =
                granted.iter().find(|grant| within(&base, grant) || within(grant, &base))
            {
                return Err(failed(format!(
                    "the policy grants {}, which holds or lies inside the private MSYS2 runtimes \
                     in {}; the sandbox cannot keep them from the sandboxed command, so grant a \
                     narrower path",
                    grant.display(),
                    base.display()
                )));
            }
            protect_owner_only(&base)?;
            sweep(&base);

            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = base.join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let bin = dir.join("usr").join("bin");
            std::fs::create_dir_all(&bin)
                .and_then(|()| std::fs::create_dir_all(dir.join("etc")))
                .map_err(|error| failed(format!("cannot create {}: {error}", dir.display())))?;
            let in_use = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .open(dir.join(IN_USE));
            let runtime = Self { dir, bin, in_use: in_use.ok() };
            if runtime.in_use.is_none() {
                return Err(failed(format!("cannot mark {} in use", runtime.dir.display())));
            }
            let installation = &program.installation;
            for file in private_files(program) {
                copy_verified(&installation.bin.join(&file), &runtime.bin.join(&file))?;
            }
            let installed = std::fs::read_to_string(installation.root.join("etc").join("fstab"))
                .unwrap_or_default();
            std::fs::write(
                runtime.dir.join("etc").join("fstab"),
                fstab(&installation.root, &installed),
            )
            .map_err(|error| failed(format!("cannot write the private fstab: {error}")))?;
            Ok(runtime)
        }
    }

    impl Drop for PrivateRuntime {
        fn drop(&mut self) {
            // The call's processes are ending with its job; their images unmap shortly after.
            drop(self.in_use.take());
            let dir = self.dir.clone();
            if std::fs::remove_dir_all(&dir).is_err() {
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        std::thread::sleep(Duration::from_millis(100));
                        if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
                            return;
                        }
                    }
                    tracing::debug!(dir = %dir.display(), "private MSYS2 runtime left for a later call");
                });
            }
        }
    }

    /// Removes the private runtimes of calls that ended without removing theirs: those whose
    /// marker no call holds.
    fn sweep(base: &Path) {
        let Ok(entries) = std::fs::read_dir(base) else { return };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_AFTER);
            let marker = entry.path().join(IN_USE);
            if old && (!marker.exists() || std::fs::remove_file(&marker).is_ok()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Copies `from` to `to`, then reads the copy back and refuses it unless it is
    /// byte-for-byte the installation's file.
    fn copy_verified(from: &Path, to: &Path) -> Result<(), SandboxError> {
        let bytes = std::fs::read(from)
            .map_err(|error| failed(format!("cannot read {}: {error}", from.display())))?;
        std::fs::write(to, &bytes)
            .map_err(|error| failed(format!("cannot write {}: {error}", to.display())))?;
        let copied = std::fs::read(to)
            .map_err(|error| failed(format!("cannot read {}: {error}", to.display())))?;
        if copied != bytes {
            return Err(failed(format!(
                "the private copy {} differs from {}; it is not run",
                to.display(),
                from.display()
            )));
        }
        Ok(())
    }

    /// An unconfined `cat` of an MSYS2 installation, which holds the user's namespace of that
    /// installation at the user's integrity level while a call runs.
    #[derive(Debug)]
    pub(crate) struct Keeper {
        /// Holds `cat`, which ends with it.
        job: JobObject,
        stdin: Option<std::fs::File>,
    }

    impl Keeper {
        /// Starts the keeper of `installation` and waits until its runtime is up, which
        /// creates or opens the namespace.
        ///
        /// Like the call's own process, `cat` inherits exactly its standard streams, which are
        /// never inheritable in this process (spec 3.2 §10 W5): it lives as long as the call,
        /// so a pipe end of another call it took would hold that call's output open.
        pub(crate) fn start(installation: &Installation) -> Result<Self, SandboxError> {
            let cat = installation.bin.join("cat.exe");
            let fail = |what: String| {
                failed(format!(
                    "cannot hold the MSYS2 namespace of {} ({what}); a sandboxed MSYS2 program \
                     of that installation could otherwise create it for the user's own",
                    installation.root.display()
                ))
            };
            let (stdin_read, stdin_write) = pipe()?;
            let (stdout_read, stdout_write) = pipe()?;
            let null = std::fs::OpenOptions::new()
                .write(true)
                .open("NUL")
                .map_err(|error| fail(format!("NUL did not open: {error}")))?;
            let stdio = ChildStdio { input: stdin_read, output: stdout_write, error: null.into() };
            let job = JobObject::new().map_err(|error| fail(format!("its job: {error}")))?;
            let environment: Vec<(OsString, OsString)> = std::env::vars_os()
                .filter(|(name, _)| !name.eq_ignore_ascii_case(super::TESTING_VARIABLE))
                .collect();
            let started = create_suspended(
                &job,
                None,
                &mut command_line(cat.as_os_str(), &[]),
                &environment_block(environment),
                None,
                CREATE_NO_WINDOW,
                Some(&stdio),
            )
            .and_then(|started| started.resume())
            .map_err(|error| fail(format!("{} did not start: {error}", shown(cat.as_os_str()))));
            // `cat` holds its own copies; on failure the job ends it.
            drop(stdio);
            started?;
            let mut keeper = Self { job, stdin: Some(stdin_write.into()) };
            let written = keeper.stdin.as_mut().map(|stdin| stdin.write_all(b"held\n"));
            if !matches!(written, Some(Ok(()))) {
                return Err(fail("its input closed".to_string()));
            }
            let mut stdout = std::fs::File::from(stdout_read);
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut echoed = [0u8; 5];
                let read = stdout.read_exact(&mut echoed);
                let _ = sender.send(read.is_ok() && &echoed == b"held\n");
            });
            match receiver.recv_timeout(KEEPER_START) {
                Ok(true) => Ok(keeper),
                Ok(false) => Err(fail("it exited at start".to_string())),
                Err(_) => Err(fail(format!("it was not up after {KEEPER_START:?}"))),
            }
        }
    }

    impl Drop for Keeper {
        fn drop(&mut self) {
            drop(self.stdin.take());
            // Ends `cat` and waits for it to exit.
            if let Err(error) = self.job.terminate_and_wait(JobObject::TERMINATION_WAIT) {
                tracing::debug!(error = %error, "could not wait for an MSYS2 keeper to exit");
            }
        }
    }

    /// A call's private runtime and keepers, kept only to be dropped with its job.
    #[derive(Debug)]
    pub(crate) struct Held {
        pub(crate) _runtime: Option<PrivateRuntime>,
        pub(crate) _keepers: Vec<Keeper>,
    }
}

#[cfg(all(target_os = "windows", feature = "sandbox-windows"))]
pub(super) use os::Held;

#[cfg(test)]
mod tests {
    use super::*;

    /// An installation laid out as Git for Windows lays it out, in a temporary directory.
    #[cfg(windows)]
    fn git() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Git");
        for sub in ["usr/bin", "bin", "cmd", "mingw64/bin", "etc"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        for file in ["usr/bin/msys-2.0.dll", "usr/bin/sh.exe", "usr/bin/bash.exe", "bin/sh.exe"] {
            std::fs::write(root.join(file), b"").unwrap();
        }
        (dir, root)
    }

    #[test]
    fn fstab_roots_the_private_runtime_at_the_installation() {
        let installed = "none /tmp usertemp binary,posix=0,noacl 0 0\n";
        assert_eq!(
            fstab(Path::new(r"C:\Program Files\Git"), installed),
            "# The private MSYS2 runtime of a sandboxed call, rooted at the installation.\n\
             C:/Program\\040Files/Git / ntfs override,binary,noacl,posix=0 0 0\n\
             none /tmp usertemp binary,posix=0,noacl 0 0\n"
        );
    }

    #[test]
    fn the_environment_puts_the_private_runtime_first_and_drops_the_testing_switch() {
        let installation =
            Installation { root: PathBuf::from(r"C:\Git"), bin: PathBuf::from(r"C:\Git\usr\bin") };
        let launched = Program {
            installation: installation.clone(),
            name: "sh.exe".into(),
            launcher: Some(Launcher {
                msystem: Some("MINGW64"),
                path: vec![PathBuf::from(r"C:\Git\mingw64\bin"), PathBuf::from(r"C:\Git\usr\bin")],
            }),
        };
        let mut pairs: Vec<(OsString, OsString)> =
            vec![("Path".into(), r"C:\Windows".into()), ("cygwin_testing".into(), "1".into())];
        environment(&mut pairs, Path::new(r"C:\private\usr\bin"), &launched);
        assert_eq!(
            pairs,
            vec![
                (
                    OsString::from("Path"),
                    OsString::from(
                        r"C:\private\usr\bin;C:\Git\mingw64\bin;C:\Git\usr\bin;C:\Windows"
                    )
                ),
                (OsString::from("MSYSTEM"), OsString::from("MINGW64")),
            ]
        );

        let direct = Program { installation, name: "sh.exe".into(), launcher: None };
        let mut pairs: Vec<(OsString, OsString)> = Vec::new();
        environment(&mut pairs, Path::new(r"C:\private\usr\bin"), &direct);
        assert_eq!(pairs, vec![(OsString::from("PATH"), OsString::from(r"C:\private\usr\bin;"))]);
    }

    #[test]
    fn paths_compare_without_regard_to_case() {
        assert!(within(Path::new(r"C:\Users\A\AppData\Local\x"), Path::new(r"c:\users\a\appdata")));
        assert!(within(Path::new(r"C:\a"), Path::new(r"C:\A\")));
        assert!(!within(Path::new(r"C:\ab"), Path::new(r"C:\a")));
        assert!(same_path(Path::new(r"C:\Git\usr\bin"), Path::new(r"c:\git\USR\bin")));
    }

    #[cfg(windows)]
    #[test]
    fn a_program_resolves_to_its_installation_and_a_launcher_to_its_program() {
        let (_dir, root) = git();
        let root =
            crate::sandbox::windows::strip_unc_prefix(&std::fs::canonicalize(&root).unwrap());
        let bin = root.join("usr").join("bin");

        let direct = program(&bin.join("sh.exe")).expect("an MSYS2 program");
        assert_eq!(direct.installation, Installation { root: root.clone(), bin: bin.clone() });
        assert_eq!(direct.launcher, None);

        let launched = program(&root.join("bin").join("sh.exe")).expect("a launcher");
        assert_eq!(
            launched.launcher,
            Some(Launcher {
                msystem: Some("MINGW64"),
                path: vec![root.join("mingw64").join("bin"), bin.clone()],
            })
        );
        assert_eq!(
            private_files(&launched),
            vec![OsString::from("msys-2.0.dll"), "sh.exe".into(), "bash.exe".into()]
        );

        assert_eq!(program(&root.join("cmd").join("git.exe")), None, "not an MSYS2 program");
        let pairs = vec![(
            OsString::from("PATH"),
            OsString::from(format!("{};{};C:\\Windows", root.join("cmd").display(), bin.display())),
        )];
        assert_eq!(installations_on_path(&pairs), vec![Installation { root, bin }]);
    }
}
