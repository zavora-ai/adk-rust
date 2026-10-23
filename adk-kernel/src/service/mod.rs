//! Service installation (spec 3.2 §7.12, §10 W3): `adk-rust up` kept running by the
//! platform's service manager, as the user, logging to the kernel home and restarting on
//! failure.
//!
//! | Platform | Form | Definition | Starts |
//! |---|---|---|---|
//! | macOS | launchd agent | `~/Library/LaunchAgents/<name>.plist` | At login, in the user's session |
//! | Linux | systemd user unit | `<config dir>/systemd/user/<name>.service` | With the user's service manager: at login, or at boot with lingering on |
//! | Windows | Windows service under the user's account | The service control manager | At boot (delayed automatic start) |
//!
//! Each form runs as the user, so a store key held in the OS keychain stays reachable: an
//! agent rather than a daemon, a user unit, a service under the user's account — which the
//! service control manager needs that account's password to start. Each writes the
//! kernel's output to `<home>/kernel.log`, created owner-only, and restarts the kernel when
//! it exits with an error or dies: launchd's `KeepAlive` on an unsuccessful exit, systemd's
//! `Restart=on-failure`, and the service's failure actions (a restart after 5, 10 and 30
//! seconds; an exit with an error counts as a failure). Stopping the service drains the
//! kernel as `SIGTERM` does.
//!
//! [`ServiceDefinition::render`] shows what [`install`] registers, for any platform,
//! without installing anything; `adk-rust up --install --print` prints it.
//!
//! # Example
//!
//! ```rust
//! use adk_kernel::service::{ServiceDefinition, ServicePlatform};
//!
//! let definition = ServiceDefinition::new("/usr/local/bin/adk-rust", "/home/ana/.config/adk-rust")
//!     .with_working_directory("/home/ana/project");
//! let unit = definition.render(ServicePlatform::Systemd);
//! assert!(unit.contains("ExecStart=/usr/local/bin/adk-rust up --home /home/ana/.config/adk-rust\n"));
//! assert!(unit.contains("Restart=on-failure\n"));
//! assert!(unit.contains("StandardOutput=append:/home/ana/.config/adk-rust/kernel.log\n"));
//! ```

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio_util::sync::CancellationToken;

use crate::error::{KernelError, Result};

mod launchd;
mod systemd;
mod windows;

#[cfg(windows)]
pub use windows::run_as_service;

/// The file under the kernel home the service writes the kernel's output to.
pub const LOG_FILE: &str = "kernel.log";

/// A service manager the kernel installs into (spec 3.2 §7.12).
///
/// # Example
///
/// ```rust
/// use adk_kernel::service::ServicePlatform;
///
/// assert_eq!(ServicePlatform::Systemd.as_str(), "systemd");
/// if cfg!(target_os = "macos") {
///     assert_eq!(ServicePlatform::current(), Some(ServicePlatform::Launchd));
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServicePlatform {
    /// A launchd agent (macOS).
    Launchd,
    /// A systemd user unit (Linux).
    Systemd,
    /// A Windows service under the user's account.
    Windows,
}

impl ServicePlatform {
    /// The service manager of the platform this runs on, or `None` where the kernel has no
    /// service form.
    pub const fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Launchd)
        } else if cfg!(target_os = "linux") {
            Some(Self::Systemd)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else {
            None
        }
    }

    /// The platform's name: `launchd`, `systemd` or `windows`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
            Self::Windows => "windows",
        }
    }
}

impl std::fmt::Display for ServicePlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the service manager runs: `adk-rust up` with a kernel home (spec 3.2 §7.12).
///
/// Never put a secret in [`args`](Self::args) or [`environment`](Self::environment): a
/// definition is a file others may read, and a Windows service's command line and
/// environment are readable by every local user.
///
/// # Example
///
/// ```rust
/// use adk_kernel::service::{ServiceDefinition, ServicePlatform};
///
/// let definition = ServiceDefinition::new("/opt/adk/bin/adk-rust", "/srv/kernel")
///     .with_args(["up", "--home", "/srv/kernel", "--bind", "127.0.0.1:9090"])
///     .with_env("PATH", "/opt/adk/bin:/usr/bin:/bin");
/// assert_eq!(definition.name, ServiceDefinition::name_for(std::path::Path::new("/srv/kernel")));
/// assert_eq!(definition.log_path(), std::path::Path::new("/srv/kernel/kernel.log"));
/// let plist = definition.render(ServicePlatform::Launchd);
/// assert!(plist.contains("<string>127.0.0.1:9090</string>"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceDefinition {
    /// The launchd label, the systemd unit's stem and the Windows service name. Defaults to
    /// [`name_for`](Self::name_for) the home, so each kernel home has its own service.
    pub name: String,
    /// The `adk-rust` binary, as an absolute path.
    pub program: PathBuf,
    /// The arguments after the program. Defaults to `up --home <home>`.
    pub args: Vec<String>,
    /// The kernel home; the service writes the kernel's output to `<home>/kernel.log`.
    pub home: PathBuf,
    /// The directory the kernel starts in. Defaults to the home.
    pub working_directory: PathBuf,
    /// Variables the kernel starts with, such as the installing shell's `PATH`.
    pub environment: Vec<(String, String)>,
    /// The Windows account the service logs on as, `DOMAIN\user`; `None` is the user who
    /// installs it, the only account [`install`] accepts. launchd and systemd always run the
    /// definition as the user who installs it.
    pub account: Option<String>,
}

impl ServiceDefinition {
    /// A definition running `program up --home <home>` from the home.
    pub fn new(program: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            name: Self::name_for(&home),
            program: program.into(),
            args: vec!["up".into(), "--home".into(), home.to_string_lossy().into_owned()],
            working_directory: home.clone(),
            home,
            environment: Vec::new(),
            account: None,
        }
    }

    /// The service name of the kernel home `home`: `adk-rust-kernel-<tag>`, the tag derived
    /// from the home's absolute path, as the named pipe's is.
    pub fn name_for(home: &Path) -> String {
        format!("adk-rust-kernel-{}", crate::path_tag(home))
    }

    /// Sets the service's name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Replaces the arguments after the program.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the directory the kernel starts in.
    #[must_use]
    pub fn with_working_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.working_directory = directory.into();
        self
    }

    /// Adds a variable the kernel starts with.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Sets the Windows account the service logs on as.
    #[must_use]
    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    /// `<home>/kernel.log`, where the service writes the kernel's output.
    pub fn log_path(&self) -> PathBuf {
        self.home.join(LOG_FILE)
    }

    /// [`log_path`](Self::log_path) with `platform`'s separator, whichever platform renders it.
    fn log_for(&self, platform: ServicePlatform) -> String {
        let home = self.home.to_string_lossy();
        match platform {
            ServicePlatform::Windows => {
                format!("{}\\{LOG_FILE}", home.trim_end_matches(['\\', '/']))
            }
            ServicePlatform::Launchd | ServicePlatform::Systemd => {
                format!("{}/{LOG_FILE}", home.trim_end_matches('/'))
            }
        }
    }

    /// Where [`install`] writes the definition on `platform`: the plist under
    /// `~/Library/LaunchAgents`, the unit under `<config dir>/systemd/user`, or `None` for a
    /// Windows service, which the service control manager keeps.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Service`] when the user's home or config directory is unknown.
    pub fn definition_path(&self, platform: ServicePlatform) -> Result<Option<PathBuf>> {
        Ok(match platform {
            ServicePlatform::Launchd => Some(launchd::plist_path(&self.name)?),
            ServicePlatform::Systemd => Some(systemd::unit_path(&self.name)?),
            ServicePlatform::Windows => None,
        })
    }

    /// The definition as `platform`'s service manager takes it: the launchd plist, the
    /// systemd unit, or, for Windows, a description of the service [`install`] registers.
    pub fn render(&self, platform: ServicePlatform) -> String {
        match platform {
            ServicePlatform::Launchd => launchd::render(self),
            ServicePlatform::Systemd => systemd::render(self),
            ServicePlatform::Windows => windows::render(self),
        }
    }
}

/// What [`install`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Installed {
    /// The service manager it installed into.
    pub platform: ServicePlatform,
    /// The service's name.
    pub name: String,
    /// The definition file it wrote; `None` for a Windows service.
    pub definition: Option<PathBuf>,
    /// Where the kernel's output goes.
    pub log: PathBuf,
    /// What the user should know, such as when the service starts.
    pub notes: Vec<String>,
}

/// What [`uninstall`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Uninstalled {
    /// The service manager it removed the service from.
    pub platform: ServicePlatform,
    /// The service's name.
    pub name: String,
    /// The definition file it removed; `None` for a Windows service.
    pub definition: Option<PathBuf>,
}

/// Installs `definition` into this platform's service manager and starts it (spec 3.2
/// §7.12), replacing an installed service of the same name.
///
/// `password` is the Windows account's password, which the service control manager needs to
/// run a service as that account; launchd and systemd ignore it. On Windows this needs an
/// elevated shell, grants the account the "Log on as a service" right, and checks the
/// password before registering the service. The log file is created owner-only where the
/// service manager would create it.
///
/// # Errors
///
/// Returns [`KernelError::Service`] when the platform has no service form, the service
/// manager refuses (its output is in the message), or, on Windows, without a password, an
/// elevated shell, or for an account other than the current user.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::service::{ServiceDefinition, install};
///
/// # fn example() -> adk_kernel::Result<()> {
/// let definition = ServiceDefinition::new("/usr/local/bin/adk-rust", "/srv/kernel");
/// let installed = install(&definition, std::env::var("ADK_SERVICE_PASSWORD").ok().as_deref())?;
/// println!("{} logs to {}", installed.name, installed.log.display());
/// # Ok(())
/// # }
/// ```
pub fn install(definition: &ServiceDefinition, password: Option<&str>) -> Result<Installed> {
    match ServicePlatform::current() {
        #[cfg(target_os = "macos")]
        Some(ServicePlatform::Launchd) => {
            let _ = password;
            launchd::install(definition)
        }
        #[cfg(target_os = "linux")]
        Some(ServicePlatform::Systemd) => {
            let _ = password;
            systemd::install(definition)
        }
        #[cfg(windows)]
        Some(ServicePlatform::Windows) => windows::install(definition, password),
        _ => {
            let _ = (definition, password);
            Err(unsupported())
        }
    }
}

/// Stops and removes the service `name` (spec 3.2 §7.12): the reverse of [`install`]. The
/// kernel home, its stores and its log stay.
///
/// # Errors
///
/// Returns [`KernelError::Service`] when no service of that name is installed, the service
/// manager refuses, or, on Windows, outside an elevated shell.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::service::{ServiceDefinition, uninstall};
///
/// # fn example() -> adk_kernel::Result<()> {
/// let removed = uninstall(&ServiceDefinition::name_for(std::path::Path::new("/srv/kernel")))?;
/// println!("removed {}", removed.name);
/// # Ok(())
/// # }
/// ```
pub fn uninstall(name: &str) -> Result<Uninstalled> {
    match ServicePlatform::current() {
        #[cfg(target_os = "macos")]
        Some(ServicePlatform::Launchd) => launchd::uninstall(name),
        #[cfg(target_os = "linux")]
        Some(ServicePlatform::Systemd) => systemd::uninstall(name),
        #[cfg(windows)]
        Some(ServicePlatform::Windows) => windows::uninstall(name),
        _ => {
            let _ = name;
            Err(unsupported())
        }
    }
}

fn unsupported() -> KernelError {
    KernelError::Service {
        reason: "this platform has no service form for the kernel (launchd on macOS, systemd \
                 on Linux, the service control manager on Windows); run `adk-rust up` under \
                 your own supervisor"
            .into(),
    }
}

/// The token a Windows service's stop request cancels.
fn stop_token() -> &'static CancellationToken {
    static STOP: OnceLock<CancellationToken> = OnceLock::new();
    STOP.get_or_init(CancellationToken::new)
}

/// Resolves when the Windows service control manager asks this process, running as the
/// kernel's service, to stop or the system is shutting down (spec 3.2 §10 W3); never
/// otherwise, and never on other platforms, whose service managers send `SIGTERM`.
/// [`Kernel::shutdown_on_signal`](crate::Kernel::shutdown_on_signal) waits for it.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() {
/// tokio::select! {
///     _ = tokio::signal::ctrl_c() => {}
///     () = adk_kernel::service::stop_requested() => {}
/// }
/// # }
/// ```
pub async fn stop_requested() {
    stop_token().cancelled().await;
}

/// Creates `<home>/kernel.log` owner-only (`0600`) when it does not exist, so the service
/// manager appends to a file only the user can read.
#[cfg(unix)]
fn prepare_log(definition: &ServiceDefinition) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::create_dir_all(&definition.home).map_err(|error| KernelError::Service {
        reason: format!("could not create the kernel home {}: {error}", definition.home.display()),
    })?;
    let log = definition.log_path();
    std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&log).map_err(
        |error| KernelError::Service {
            reason: format!("could not create the kernel log {}: {error}", log.display()),
        },
    )?;
    Ok(log)
}

/// Writes `contents` to the definition file `path`, creating its directory.
#[cfg(unix)]
fn write_definition(path: &Path, contents: &str) -> Result<()> {
    let failed = |error: std::io::Error| KernelError::Service {
        reason: format!("could not write {}: {error}", path.display()),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::write(path, contents).map_err(failed)
}

/// Runs a service manager's command, answering its output on failure.
#[cfg(unix)]
fn run(program: &str, args: &[&str]) -> std::result::Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!("`{program} {}` failed ({}): {}", args.join(" "), output.status, text.trim()))
    }
}

#[cfg(test)]
mod tests;
