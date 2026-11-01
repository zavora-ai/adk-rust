//! Linux bubblewrap sandbox enforcer.
//!
//! Uses `bwrap` with user namespaces to create isolated filesystem and
//! process environments without requiring root privileges.
//!
//! ## How It Works
//!
//! 1. Bubblewrap arguments are generated from the [`SandboxPolicy`]
//! 2. The original command is wrapped: `bwrap <args> -- <program> <args...>`
//! 3. The kernel enforces namespace-based isolation on the child process
//!
//! ## Key bwrap Arguments
//!
//! - `--die-with-parent` — kill child when parent exits
//! - `--unshare-pid` — isolate process ID namespace
//! - `--unshare-net` — unless the network is `Open`: an empty network namespace, so no IP
//!   traffic and no abstract Unix sockets leave the sandbox
//! - `--ro-bind <src> <dest>` — read-only filesystem bind mount
//! - `--bind <src> <dest>` — read-write filesystem bind mount
//! - `--new-session` — always: a new session with no controlling terminal, so the child
//!   cannot inject input into the caller's terminal with `TIOCSTI`
//! - `--seccomp <fd>` — seccomp program that denies process creation
//!
//! ## Nested Paths
//!
//! bubblewrap shows the last mount made over a path, so the policy's paths are mounted
//! shallowest first and the most specific entry decides (spec 3.1 §10 S3): a read-only entry
//! inside a read-write entry is mounted after it and stays read-only. Each writable directory
//! above such an entry is also bound onto itself, which makes it a mount point the kernel
//! refuses to rename or remove, so the entry cannot be carried away from its path and the
//! path recreated writable.
//!
//! ## Network
//!
//! `Denied` network access is `--unshare-net`: an empty network namespace, so no IP traffic and
//! no abstract Unix sockets. A Unix-domain socket on the filesystem is reached through its
//! path, not the network namespace, and a read-only bind does not refuse `connect`, so under
//! `Denied` the enforcer also keeps the host's shared socket directories out:
//!
//! | Policy | Effect |
//! |--------|--------|
//! | A grant of `/run`, or of a path under it, that holds a socket or cannot be fully listed | Refused with [`SandboxError::Unenforceable`] |
//! | A grant of `/tmp`, `/var/tmp` or `/dev/shm` itself | Refused the same way; a directory below them can be granted |
//! | A grant above them, such as `/` | Each is covered with an empty, read-only `tmpfs` before deeper grants are mounted, hiding the sockets of Docker, D-Bus, systemd, tmux, X11 and SSH agents |
//!
//! A socket elsewhere under a granted path, such as in a home directory, stays reachable;
//! grant directories that hold none.
//!
//! `Proxied` (feature `egress-proxy`) keeps `--unshare-net` and places the egress proxy's
//! listener inside the call's network namespace: bubblewrap reports the sandbox's process id on
//! [`INFO_FD`] and holds the program on [`BLOCK_FD`]; a short-lived helper joins the sandbox's
//! user and network namespaces, brings up its loopback interface, opens the listening socket on
//! `127.0.0.1:<port>` and passes it back; the call's [`CallProxy`] serves it, and only then is
//! the program released. When any step fails the program is released with no listener, so it
//! reaches nothing. Without `egress-proxy`, `Proxied` is refused with
//! [`SandboxError::Unenforceable`].
//!
//! [`CallProxy`]: crate::egress_proxy::CallProxy
//!
//! ## Process Spawning
//!
//! When `allow_process_spawn` is `false`, the wrapped program runs under a seccomp filter
//! that fails `fork`, `vfork`, and non-thread `clone` with `EPERM` and `clone3` with
//! `ENOSYS`. Threads and `execve` keep working. The filter is generated for x86-64 and
//! AArch64; on any other architecture [`SandboxEnforcer::configure_command`] returns
//! `EnforcerFailed` rather than running without it.
//!
//! The filter reaches bubblewrap as file descriptor [`SECCOMP_FD`], which
//! [`SandboxEnforcer::configure_command`] installs on the command. Running the output of
//! [`LinuxEnforcer::generate_args`] without that call makes bubblewrap exit with an error.

use std::ffi::{OsStr, OsString};

use super::layering::Layering;
use super::seccomp;
use super::{
    AccessMode, AllowedPath, NetworkAccess, NetworkSupport, ProxyListener, SandboxEnforcer,
    SandboxPolicy, WrappedCommand,
};
use crate::error::SandboxError;

/// Directories where host daemons and sessions keep Unix sockets. Under a restrictive network
/// a grant of one of them is refused, and a grant above one covers it with an empty `tmpfs`.
const SOCKET_DIRECTORIES: &[&str] = &["/run", "/tmp", "/var/tmp", "/dev/shm"];

/// File descriptor bubblewrap writes its JSON information, the sandbox's process id among it,
/// to under `Proxied` network access.
pub const INFO_FD: i32 = 65;

/// File descriptor bubblewrap waits on before it runs the program under `Proxied` network
/// access, until the proxy's listener is in place.
pub const BLOCK_FD: i32 = 66;

/// File descriptor bubblewrap reads the process-creation seccomp program from.
///
/// High enough to stay clear of stdio and the descriptors the standard library uses while
/// spawning, and below every default `RLIMIT_NOFILE`.
pub const SECCOMP_FD: i32 = 64;

/// Linux bubblewrap sandbox enforcer.
///
/// Wraps child processes with `bwrap` arguments to enforce namespace-based
/// filesystem, network, and process isolation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::sandbox::linux::LinuxEnforcer;
/// use adk_sandbox::sandbox::{SandboxEnforcer, SandboxPolicyBuilder};
/// use std::ffi::OsString;
///
/// let enforcer = LinuxEnforcer::new();
/// enforcer.probe()?;
///
/// let policy = SandboxPolicyBuilder::new()
///     .allow_read("/usr/lib")
///     .allow_read_write("/tmp/work")
///     .build();
///
/// let wrapped = enforcer.wrap_command(
///     "python3".as_ref(),
///     &[OsString::from("-c"), OsString::from("print('hello')")],
///     &policy,
/// )?;
/// // wrapped.program == "bwrap"
/// // wrapped.args == ["--die-with-parent", "--unshare-pid", "--unshare-net",
/// //                   "--new-session", "--seccomp", "64",
/// //                   "--ro-bind", "/usr/lib", "/usr/lib",
/// //                   "--bind", "/tmp/work", "/tmp/work",
/// //                   "--", "python3", "-c", "print('hello')"]
/// // `configure_command` must then be applied to the command built from `wrapped`.
/// ```
pub struct LinuxEnforcer;

impl LinuxEnforcer {
    /// Creates a new Linux bubblewrap enforcer.
    pub fn new() -> Self {
        Self
    }

    /// Generates bubblewrap arguments from the policy.
    ///
    /// Always starts with `--die-with-parent` and `--unshare-pid`, and always passes
    /// `--new-session`. Bind mounts follow, shallowest path first, with a read-write bind
    /// of each writable directory above a nested read-only entry (see
    /// [Nested Paths](self#nested-paths)).
    /// The returned arguments do NOT include the `--` separator or the
    /// original program/args — those are appended by `wrap_command`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_sandbox::sandbox::linux::LinuxEnforcer;
    /// use adk_sandbox::sandbox::SandboxPolicyBuilder;
    ///
    /// let policy = SandboxPolicyBuilder::new()
    ///     .allow_read("/usr/lib")
    ///     .build();
    ///
    /// let args = LinuxEnforcer::generate_args(&policy);
    /// assert_eq!(args[0], "--die-with-parent");
    /// assert_eq!(args[1], "--unshare-pid");
    /// assert!(args.contains(&"--unshare-net".to_string()));
    /// ```
    pub fn generate_args(policy: &SandboxPolicy) -> Vec<String> {
        Self::generate_args_from_paths(
            &policy.allowed_paths,
            &policy.network,
            policy.allow_process_spawn,
        )
    }

    /// Internal: generates args from pre-canonicalized paths.
    fn generate_args_from_paths(
        paths: &[AllowedPath],
        network: &NetworkAccess,
        allow_process_spawn: bool,
    ) -> Vec<String> {
        let mut args = Vec::with_capacity(16);

        // Always: terminate child when parent exits
        args.push("--die-with-parent".to_string());

        // Always: isolate PID namespace
        args.push("--unshare-pid".to_string());

        // Network isolation
        if network.restricts() {
            args.push("--unshare-net".to_string());
        }
        if let NetworkAccess::Proxied { .. } = network {
            args.extend(
                ["--info-fd", &INFO_FD.to_string(), "--block-fd", &BLOCK_FD.to_string()]
                    .map(str::to_string),
            );
        }

        // Always: a new session with no controlling terminal, so the child cannot inject
        // input into the caller's terminal with `TIOCSTI` (spec 3.1 §10 S2).
        args.push("--new-session".to_string());

        // Process spawn restriction: the seccomp program is what denies fork and clone.
        if !allow_process_spawn {
            args.push("--seccomp".to_string());
            args.push(SECCOMP_FD.to_string());
        }

        // Filesystem bind mounts, shallowest first: a later mount covers an earlier one, so the
        // most specific entry decides. Pinned directories become mount points, which the
        // kernel refuses to rename or remove. Under a restrictive network a socket directory
        // below a mount is covered right after it, before any deeper grant is mounted.
        let mut covered: Vec<&str> = Vec::new();
        for (path, mode) in Layering::new(paths).mounts() {
            let path_str = path.to_string_lossy().to_string();
            match mode {
                AccessMode::ReadOnly => {
                    args.push("--ro-bind".to_string());
                    args.push(path_str.clone());
                    args.push(path_str);
                }
                AccessMode::ReadWrite => {
                    args.push("--bind".to_string());
                    args.push(path_str.clone());
                    args.push(path_str);
                }
            }
            if !network.restricts() {
                continue;
            }
            for directory in SOCKET_DIRECTORIES {
                let target = std::path::Path::new(directory);
                let below = target != path && target.starts_with(path);
                if below
                    && !covered.contains(directory)
                    && std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir())
                {
                    args.push("--tmpfs".to_string());
                    args.push((*directory).to_string());
                    covered.push(directory);
                }
            }
        }
        for directory in covered {
            args.push("--remount-ro".to_string());
            args.push(directory.to_string());
        }

        args
    }
}

impl Default for LinuxEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxEnforcer for LinuxEnforcer {
    fn name(&self) -> &str {
        "bubblewrap"
    }

    fn network_support(&self) -> NetworkSupport {
        NetworkSupport { denied: true, proxied: cfg!(feature = "egress-proxy") }
    }

    fn proxy_listener(&self) -> ProxyListener {
        ProxyListener::SandboxLoopback
    }

    fn probe(&self) -> Result<(), SandboxError> {
        // Check that bwrap binary exists
        let result = std::process::Command::new("bwrap")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match result {
            Ok(status) if status.success() => {
                // Also check user namespaces are available
                // The root bind is not optional. bwrap gives the new namespace an empty
                // root, so without it `/bin/true` does not exist inside and execvp fails —
                // which this probe then reported as "user namespaces are not available",
                // pointing operators at a sysctl that was never the problem. The check
                // failed on every host, so the enforcer was never selectable on Linux.
                let ns_check = std::process::Command::new("bwrap")
                    .args(["--ro-bind", "/", "/", "--unshare-user", "--", "/bin/true"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();

                match ns_check {
                    Ok(s) if s.success() => Ok(()),
                    _ => Err(SandboxError::EnforcerUnavailable {
                        enforcer: "bubblewrap".to_string(),
                        message: "user namespaces are not available. Check that \
                                  `kernel.unprivileged_userns_clone` sysctl is set to 1."
                            .to_string(),
                    }),
                }
            }
            Ok(_) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "bubblewrap".to_string(),
                message: "bwrap binary found but returned an error. \
                          Verify installation is complete."
                    .to_string(),
            }),
            Err(e) => Err(SandboxError::EnforcerUnavailable {
                enforcer: "bubblewrap".to_string(),
                message: format!(
                    "bwrap binary not found: {e}. Install bubblewrap: \
                     `apt install bubblewrap` (Debian/Ubuntu) or \
                     `dnf install bubblewrap` (Fedora/RHEL)."
                ),
            }),
        }
    }

    fn wrap_command(
        &self,
        program: &OsStr,
        args: &[OsString],
        policy: &SandboxPolicy,
    ) -> Result<WrappedCommand, SandboxError> {
        self.network_support().check(self.name(), &policy.network)?;

        // 1. Canonicalize all paths in the policy
        let canonicalized_paths = canonicalize_paths(&policy.allowed_paths)?;
        if policy.network.restricts() {
            refuse_socket_directories(&canonicalized_paths, &policy.network)?;
        }

        // 2. Generate bwrap args from canonicalized paths
        let bwrap_args = Self::generate_args_from_paths(
            &canonicalized_paths,
            &policy.network,
            policy.allow_process_spawn,
        );

        // 3. Build the wrapped command: bwrap <args> -- <program> <original_args...>
        let mut wrapped_args: Vec<OsString> = bwrap_args.into_iter().map(OsString::from).collect();
        wrapped_args.push(OsString::from("--"));
        wrapped_args.push(program.to_owned());
        wrapped_args.extend_from_slice(args);

        Ok(WrappedCommand { program: OsString::from("bwrap"), args: wrapped_args })
    }

    /// Installs the process-creation seccomp program as file descriptor [`SECCOMP_FD`].
    ///
    /// # Errors
    ///
    /// Returns `EnforcerFailed` when the policy denies process spawning on an architecture
    /// without a generated filter, or when the descriptor cannot be prepared.
    fn configure_command(
        &self,
        cmd: &mut tokio::process::Command,
        policy: &SandboxPolicy,
    ) -> Result<(), SandboxError> {
        use std::io::Write;
        use std::os::fd::{AsRawFd, OwnedFd};

        if let NetworkAccess::Proxied { endpoint } = policy.network {
            let failed = |error: std::io::Error| SandboxError::EnforcerFailed {
                enforcer: "bubblewrap".to_string(),
                message: format!("failed to prepare the egress proxy's descriptors: {error}"),
            };
            let (info_reader, info_writer) = std::io::pipe().map_err(failed)?;
            let (block_reader, block_writer) = std::io::pipe().map_err(failed)?;
            let (info_fd, block_fd) = (OwnedFd::from(info_writer), OwnedFd::from(block_reader));
            // SAFETY: the closure runs in the forked child before exec and calls only dup2(2)
            // and fcntl(2), which are async-signal-safe; it owns both descriptors.
            unsafe {
                cmd.pre_exec(move || {
                    install_fd(info_fd.as_raw_fd(), INFO_FD)?;
                    install_fd(block_fd.as_raw_fd(), BLOCK_FD)
                });
            }
            std::thread::Builder::new()
                .name("adk-sandbox-egress-namespace".to_string())
                .spawn(move || namespace::attach_listener(info_reader, block_writer, endpoint.port))
                .map_err(failed)?;
        }

        if policy.allow_process_spawn {
            return Ok(());
        }

        let abi = seccomp::Abi::native().ok_or_else(|| SandboxError::EnforcerFailed {
            enforcer: "bubblewrap".to_string(),
            message: format!(
                "no process-creation seccomp filter exists for {}; allow process spawning in \
                 the policy or run on x86_64 or aarch64",
                std::env::consts::ARCH
            ),
        })?;
        let program = seccomp::encode(&seccomp::deny_process_creation(abi));

        let failed = |error: std::io::Error| SandboxError::EnforcerFailed {
            enforcer: "bubblewrap".to_string(),
            message: format!("failed to prepare the seccomp program descriptor: {error}"),
        };
        // The program is a few hundred bytes, well inside a pipe buffer, so the write cannot
        // block, and closing the writer gives bubblewrap its end-of-file.
        let (reader, mut writer) = std::io::pipe().map_err(failed)?;
        writer.write_all(&program).map_err(failed)?;
        drop(writer);
        let program_fd = OwnedFd::from(reader);

        // SAFETY: the closure runs in the forked child before exec and calls only dup2(2) and
        // fcntl(2), which are async-signal-safe. `program_fd` is owned by the closure, so the
        // descriptor stays open until the command is dropped.
        unsafe {
            cmd.pre_exec(move || install_fd(program_fd.as_raw_fd(), SECCOMP_FD));
        }
        Ok(())
    }
}

/// Canonicalizes all paths in the policy, logging warnings for changed paths.
/// Makes `source` available to the program as `target`, inheritable across exec.
///
/// Runs in the forked child; dup2(2) and fcntl(2) are async-signal-safe.
fn install_fd(source: i32, target: i32) -> std::io::Result<()> {
    // SAFETY: plain descriptor syscalls on descriptors the caller owns.
    let result = unsafe {
        if source == target {
            // dup2 onto itself leaves FD_CLOEXEC set, so clear the flag directly.
            libc::fcntl(source, libc::F_SETFD, 0)
        } else {
            libc::dup2(source, target)
        }
    };
    if result == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

/// The egress proxy's listener inside a sandbox's network namespace.
#[cfg(feature = "egress-proxy")]
mod namespace {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    /// Waits for bubblewrap's process id, opens the listener in its namespace, hands it to the
    /// call's proxy, and releases the program, which runs with no listener if a step failed.
    pub(super) fn attach_listener(
        info: std::io::PipeReader,
        mut block: std::io::PipeWriter,
        port: u16,
    ) {
        let attached =
            child_pid(info).and_then(|pid| listen_in_namespace(pid, port)).and_then(|listener| {
                if crate::egress_proxy::deliver_namespace_listener(port, listener) {
                    Ok(())
                } else {
                    Err(std::io::Error::other("no egress proxy waits for this call"))
                }
            });
        if let Err(error) = attached {
            tracing::warn!(
                error = %error,
                sandbox.proxy_port = port,
                "the egress proxy listener could not be placed in the sandbox; the program \
                 reaches no network"
            );
        }
        let _ = block.write_all(b"1");
    }

    /// Reads bubblewrap's JSON information up to its closing brace and returns `child-pid`.
    fn child_pid(mut info: std::io::PipeReader) -> std::io::Result<u32> {
        let mut text = Vec::new();
        let mut chunk = [0u8; 512];
        while !text.contains(&b'}') {
            match info.read(&mut chunk)? {
                0 => break,
                n => text.extend_from_slice(&chunk[..n]),
            }
            if text.len() > 64 * 1024 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&text);
        text.split("\"child-pid\"")
            .nth(1)
            .and_then(|rest| {
                let digits: String = rest
                    .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse().ok()
            })
            .ok_or_else(|| {
                std::io::Error::other(format!("bubblewrap reported no child-pid: {text}"))
            })
    }

    /// Opens a listener on `127.0.0.1:<port>` inside the namespaces of process `pid`, through a
    /// helper that joins them, since only a single-threaded process may join a user namespace.
    fn listen_in_namespace(pid: u32, port: u16) -> std::io::Result<std::net::TcpListener> {
        let user = File::open(format!("/proc/{pid}/ns/user"))?;
        let net = File::open(format!("/proc/{pid}/ns/net"))?;
        let (parent, child) = UnixStream::pair()?;
        let (user_fd, net_fd, child_fd) = (user.as_raw_fd(), net.as_raw_fd(), child.as_raw_fd());
        let mut helper = std::process::Command::new("true");
        helper
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: the closure runs in the forked, single-threaded child before exec and makes
        // only async-signal-safe system calls on stack data.
        unsafe {
            helper.pre_exec(move || {
                if libc::setns(user_fd, libc::CLONE_NEWUSER) == -1
                    || libc::setns(net_fd, libc::CLONE_NEWNET) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                loopback_up()?;
                let listener = bind_loopback(port)?;
                send_fd(child_fd, listener)
            });
        }
        let status = helper.status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("the namespace helper ended with {status}")));
        }
        drop((user, net, child));
        let fd = receive_fd(&parent)?;
        Ok(std::net::TcpListener::from(fd))
    }

    /// Sets `lo` up in the current network namespace.
    unsafe fn loopback_up() -> std::io::Result<()> {
        // SAFETY: a zeroed `ifreq` is a valid value; the name is NUL-terminated.
        unsafe {
            let socket = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
            if socket == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let mut request: libc::ifreq = std::mem::zeroed();
            for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo\0") {
                *slot = *byte as libc::c_char;
            }
            let mut result = libc::ioctl(socket, libc::SIOCGIFFLAGS as _, &mut request);
            if result != -1 {
                request.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
                result = libc::ioctl(socket, libc::SIOCSIFFLAGS as _, &request);
            }
            let error = std::io::Error::last_os_error();
            libc::close(socket);
            if result == -1 { Err(error) } else { Ok(()) }
        }
    }

    /// A listening TCP socket on `127.0.0.1:<port>`.
    unsafe fn bind_loopback(port: u16) -> std::io::Result<libc::c_int> {
        // SAFETY: `address` is a fully initialised `sockaddr_in` of the length passed.
        unsafe {
            let socket = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if socket == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let address = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: port.to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(std::net::Ipv4Addr::LOCALHOST).to_be(),
                },
                sin_zero: [0; 8],
            };
            let length = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            if libc::bind(socket, (&raw const address).cast(), length) == -1
                || libc::listen(socket, 128) == -1
            {
                let error = std::io::Error::last_os_error();
                libc::close(socket);
                return Err(error);
            }
            Ok(socket)
        }
    }

    /// Sends `fd` over the Unix socket `socket` (SCM_RIGHTS).
    unsafe fn send_fd(socket: libc::c_int, fd: libc::c_int) -> std::io::Result<()> {
        // SAFETY: the control buffer is aligned for `cmsghdr` and large enough for one
        // descriptor; every pointer refers to stack data that outlives the call.
        unsafe {
            let mut byte = [0u8; 1];
            let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
            let mut control = [0u64; 8];
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen =
                libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32) as _;
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as u32) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>(), fd);
            if libc::sendmsg(socket, &message, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// Receives one descriptor sent with SCM_RIGHTS.
    fn receive_fd(socket: &UnixStream) -> std::io::Result<OwnedFd> {
        // SAFETY: as in `send_fd`; the received descriptor is owned by the returned value.
        unsafe {
            let mut byte = [0u8; 1];
            let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
            let mut control = [0u64; 8];
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = std::mem::size_of_val(&control) as _;
            if libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let header = libc::CMSG_FIRSTHDR(&message);
            if header.is_null()
                || (*header).cmsg_level != libc::SOL_SOCKET
                || (*header).cmsg_type != libc::SCM_RIGHTS
            {
                return Err(std::io::Error::other("the namespace helper sent no listener"));
            }
            let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<libc::c_int>());
            Ok(OwnedFd::from_raw_fd(fd))
        }
    }
}

/// Without `egress-proxy` there is no proxy to serve the listener; `Proxied` is refused before
/// a command is built, so this only releases the program.
#[cfg(not(feature = "egress-proxy"))]
mod namespace {
    use std::io::Write;

    pub(super) fn attach_listener(
        _info: std::io::PipeReader,
        mut block: std::io::PipeWriter,
        _port: u16,
    ) {
        let _ = block.write_all(b"1");
    }
}

/// Refuses a grant of a shared socket directory, or of a path under `/run` that holds a Unix
/// socket: a bind mount keeps its sockets reachable whatever the network namespace.
fn refuse_socket_directories(
    paths: &[AllowedPath],
    network: &NetworkAccess,
) -> Result<(), SandboxError> {
    let run = std::path::Path::new("/run");
    for entry in paths {
        let shared = SOCKET_DIRECTORIES
            .iter()
            .any(|directory| entry.path == std::path::Path::new(directory));
        // A walk of at most this many entries; a larger tree is refused unread.
        let mut budget = 4096;
        if shared || (entry.path.starts_with(run) && holds_socket(&entry.path, &mut budget)) {
            return Err(SandboxError::Unenforceable {
                enforcer: "bubblewrap".to_string(),
                requirement: format!(
                    "network access {network} while granting '{}': Unix sockets there (Docker, \
                     D-Bus, systemd, SSH agents) stay reachable through a bind mount; grant a \
                     directory below it that holds none",
                    entry.path.display()
                ),
            });
        }
    }
    Ok(())
}

/// Whether `path` is, or holds within `budget` entries, a Unix socket; a directory that cannot
/// be listed or a tree past the budget counts as holding one.
fn holds_socket(path: &std::path::Path, budget: &mut usize) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return true };
    if metadata.file_type().is_socket() {
        return true;
    }
    if !metadata.is_dir() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return true };
    for entry in entries {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let Ok(entry) = entry else { return true };
        let Ok(kind) = entry.file_type() else { return true };
        if kind.is_socket() || (kind.is_dir() && holds_socket(&entry.path(), budget)) {
            return true;
        }
    }
    false
}

fn canonicalize_paths(paths: &[AllowedPath]) -> Result<Vec<AllowedPath>, SandboxError> {
    let mut result = Vec::with_capacity(paths.len());

    for entry in paths {
        let canonical = std::fs::canonicalize(&entry.path).map_err(|e| {
            SandboxError::PolicyViolation(format!(
                "failed to canonicalize allowed path '{}': {e}",
                entry.path.display()
            ))
        })?;

        if canonical != entry.path {
            tracing::warn!(
                original = %entry.path.display(),
                resolved = %canonical.display(),
                "allowed path resolved to a different location (possible symlink)"
            );
        }

        result.push(AllowedPath { path: canonical, mode: entry.mode });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicyBuilder;

    #[test]
    fn test_generate_args_deny_all() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert_eq!(args[0], "--die-with-parent");
        assert_eq!(args[1], "--unshare-pid");
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.contains(&"--new-session".to_string()));
        assert!(
            args.windows(2).any(|pair| pair[0] == "--seccomp" && pair[1] == SECCOMP_FD.to_string())
        );
        // No bind mounts
        assert!(!args.contains(&"--ro-bind".to_string()));
        assert!(!args.contains(&"--bind".to_string()));
    }

    #[test]
    fn test_generate_args_read_only_path() {
        let policy = SandboxPolicyBuilder::new().allow_read("/usr/lib").build();
        let args = LinuxEnforcer::generate_args(&policy);

        let ro_idx = args.iter().position(|a| a == "--ro-bind").unwrap();
        assert_eq!(args[ro_idx + 1], "/usr/lib");
        assert_eq!(args[ro_idx + 2], "/usr/lib");
        assert!(!args.contains(&"--bind".to_string()));
    }

    #[test]
    fn test_generate_args_read_write_path() {
        let policy = SandboxPolicyBuilder::new().allow_read_write("/tmp/work").build();
        let args = LinuxEnforcer::generate_args(&policy);

        let bind_idx = args.iter().position(|a| a == "--bind").unwrap();
        assert_eq!(args[bind_idx + 1], "/tmp/work");
        assert_eq!(args[bind_idx + 2], "/tmp/work");
        assert!(!args.contains(&"--ro-bind".to_string()));
    }

    #[test]
    fn test_generate_args_network_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_network().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn test_generate_args_network_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn test_generate_args_process_spawn_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(!args.contains(&"--seccomp".to_string()));
    }

    /// A shell is the case that allows process spawning, and it is the one that can reach a
    /// controlling terminal, so the new session cannot depend on the seccomp filter.
    #[test]
    fn every_policy_starts_a_new_session() {
        for allow_process_spawn in [false, true] {
            for network in [NetworkAccess::Denied, NetworkAccess::Open] {
                let args =
                    LinuxEnforcer::generate_args_from_paths(&[], &network, allow_process_spawn);
                assert_eq!(
                    args.iter().filter(|arg| *arg == "--new-session").count(),
                    1,
                    "spawn={allow_process_spawn} network={network}: {args:?}"
                );
            }
        }
    }

    #[test]
    fn test_generate_args_process_spawn_denied() {
        let policy = SandboxPolicyBuilder::new().build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert!(args.contains(&"--new-session".to_string()));
        let seccomp = args.iter().position(|arg| arg == "--seccomp").expect("--seccomp present");
        assert_eq!(args[seccomp + 1], SECCOMP_FD.to_string());
    }

    #[test]
    fn configure_command_is_a_no_op_when_spawning_is_allowed() {
        let policy = SandboxPolicyBuilder::new().allow_process_spawn().build();
        let mut cmd = tokio::process::Command::new("true");

        LinuxEnforcer::new().configure_command(&mut cmd, &policy).expect("nothing to configure");
    }

    #[test]
    fn configure_command_installs_the_filter_on_supported_architectures() {
        let policy = SandboxPolicyBuilder::new().build();
        let mut cmd = tokio::process::Command::new("true");

        let result = LinuxEnforcer::new().configure_command(&mut cmd, &policy);
        if seccomp::Abi::native().is_some() {
            result.expect("the filter is prepared on a supported architecture");
        } else {
            assert!(matches!(result, Err(SandboxError::EnforcerFailed { .. })), "{result:?}");
        }
    }

    #[test]
    fn test_generate_args_starts_with_die_with_parent() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/tmp")
            .allow_network()
            .allow_process_spawn()
            .build();
        let args = LinuxEnforcer::generate_args(&policy);

        assert_eq!(args[0], "--die-with-parent");
    }

    #[test]
    fn test_generate_args_no_empty_strings() {
        let policy = SandboxPolicyBuilder::new()
            .allow_read("/usr/lib")
            .allow_read_write("/tmp")
            .allow_network()
            .build();
        let args = LinuxEnforcer::generate_args(&policy);

        for arg in &args {
            assert!(!arg.is_empty(), "found empty string in bwrap args");
        }
    }

    #[test]
    fn test_name() {
        let enforcer = LinuxEnforcer::new();
        assert_eq!(enforcer.name(), "bubblewrap");
    }

    /// `Proxied` keeps the empty network namespace and asks bubblewrap for its process id and
    /// to wait for the listener; without `egress-proxy` there is no proxy, and it is refused.
    #[test]
    fn proxied_network_access_needs_the_egress_proxy() {
        let endpoint = super::super::ProxyEndpoint { port: 3128 };
        let policy = SandboxPolicyBuilder::new().proxied(endpoint).build();

        let result = LinuxEnforcer::new().wrap_command(OsStr::new("true"), &[], &policy);

        if cfg!(feature = "egress-proxy") {
            let args: Vec<String> = result
                .expect("Proxied is enforced with the egress proxy")
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            for flag in [
                vec!["--unshare-net".to_string()],
                vec!["--info-fd".to_string(), INFO_FD.to_string()],
                vec!["--block-fd".to_string(), BLOCK_FD.to_string()],
            ] {
                assert!(
                    args.windows(flag.len()).any(|w| w == flag.as_slice()),
                    "{flag:?} in {args:?}"
                );
            }
        } else {
            assert!(matches!(result, Err(SandboxError::Unenforceable { .. })), "{result:?}");
        }
    }

    /// A read-only bind does not refuse `connect` on a Unix socket, so the shared socket
    /// directories are refused under a restrictive network; an open network grants them.
    #[test]
    fn socket_directories_are_refused_under_denied_network() {
        let enforcer = LinuxEnforcer::new();
        for path in ["/run", "/tmp"] {
            let denied = SandboxPolicyBuilder::new().allow_read(path).build();
            let result = enforcer.wrap_command(OsStr::new("true"), &[], &denied);
            assert!(
                matches!(result, Err(SandboxError::Unenforceable { .. })),
                "{path}: {result:?}"
            );

            let open = SandboxPolicyBuilder::new().allow_read(path).allow_network().build();
            assert!(enforcer.wrap_command(OsStr::new("true"), &[], &open).is_ok(), "{path}");
        }
        let narrow = SandboxPolicyBuilder::new().allow_read("/usr").build();
        assert!(enforcer.wrap_command(OsStr::new("true"), &[], &narrow).is_ok());
    }

    /// A grant of `/` covers the socket directories with empty read-only `tmpfs` mounts before
    /// a deeper grant, so the workspace below `/tmp` stays reachable and the rest does not.
    #[test]
    fn a_granted_root_covers_the_socket_directories_under_denied_network() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let workspace = std::fs::canonicalize(workspace.path()).expect("canonical");
        let paths = [
            AllowedPath { path: std::path::PathBuf::from("/"), mode: AccessMode::ReadOnly },
            AllowedPath { path: workspace.clone(), mode: AccessMode::ReadWrite },
        ];
        let args = LinuxEnforcer::generate_args_from_paths(&paths, &NetworkAccess::Denied, true);
        let position = |needle: &[&str]| args.windows(needle.len()).position(|w| w == needle);
        let root = position(&["--ro-bind", "/", "/"]).expect("the root is bound");
        let run = position(&["--tmpfs", "/run"]).expect("/run is covered");
        let tmp = position(&["--tmpfs", "/tmp"]).expect("/tmp is covered");
        let workspace = workspace.to_string_lossy().into_owned();
        let bound = position(&["--bind", &workspace, &workspace]).expect("the workspace is bound");
        assert!(root < run && run < bound && tmp < bound, "{args:?}");
        assert!(position(&["--remount-ro", "/run"]).is_some_and(|ro| ro > bound), "{args:?}");

        let open = LinuxEnforcer::generate_args_from_paths(&paths, &NetworkAccess::Open, true);
        assert!(!open.iter().any(|arg| arg == "--tmpfs"), "{open:?}");
    }

    #[test]
    fn test_wrap_command_nonexistent_path_fails() {
        let enforcer = LinuxEnforcer::new();
        let policy =
            SandboxPolicyBuilder::new().allow_read("/nonexistent/path/that/does/not/exist").build();

        let result = enforcer.wrap_command(OsStr::new("echo"), &[], &policy);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SandboxError::PolicyViolation(_)),
            "expected PolicyViolation, got: {err:?}"
        );
    }
}
