//! The kernel as a Windows service under the user's account (spec 3.2 §10 W3): registered
//! with the service control manager, started at boot, restarted on failure, and run through
//! the service dispatcher by [`run_as_service`].

use std::borrow::Cow;

use super::ServiceDefinition;

/// The display name every kernel service carries; the description names its home.
const DISPLAY_NAME: &str = "ADK-Rust agent kernel";

/// The delays of the restarts after the first, second and later failures.
const RESTART_DELAYS: [u64; 3] = [5, 10, 30];

/// A description of the service [`install`] registers: the service control manager keeps the
/// definition, so this is what `sc.exe qc` and the failure actions would show.
pub(super) fn render(definition: &ServiceDefinition) -> String {
    let account = definition.account.as_deref().unwrap_or("the user who installs it");
    let restarts = RESTART_DELAYS.map(|delay| format!("{delay} s")).join(", ");
    let mut text = format!(
        "service: {}\n\
         display name: {DISPLAY_NAME}\n\
         description: {}\n\
         command line: {}\n\
         account: {account}\n\
         start: automatic (delayed), at boot\n\
         on failure: restart after {restarts}; the count resets after a day; an exit with an \
         error counts as a failure\n\
         working directory: {}\n",
        definition.name,
        description(definition),
        command_line(definition),
        definition.working_directory.display(),
    );
    for (key, value) in &definition.environment {
        text.push_str(&format!("environment: {key}={value}\n"));
    }
    text.push_str(&format!("log: {}\n", definition.log_for(super::ServicePlatform::Windows)));
    text
}

fn description(definition: &ServiceDefinition) -> String {
    format!("Runs `adk-rust up` for the kernel home {}", definition.home.display())
}

/// The command line the service control manager starts, each word quoted as the Microsoft C
/// runtime parses it, as `windows-service` quotes it.
fn command_line(definition: &ServiceDefinition) -> String {
    let program = definition.program.to_string_lossy();
    std::iter::once(program.as_ref())
        .chain(definition.args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `word` quoted for the Microsoft C runtime: as is when it holds no space, tab, newline or
/// quote, otherwise in quotes with each quote and the backslashes before it escaped.
pub(super) fn quote(word: &str) -> Cow<'_, str> {
    if !word.is_empty() && !word.contains([' ', '\t', '\n', '\x0b', '"']) {
        return Cow::Borrowed(word);
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for c in word.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            c => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    Cow::Owned(quoted)
}

#[cfg(windows)]
pub(super) use manager::{install, uninstall};
#[cfg(windows)]
pub use runtime::run_as_service;

#[cfg(windows)]
mod manager {
    use std::ffi::OsString;
    use std::time::{Duration, Instant};

    use windows_service::service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
        ServiceState, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE,
    };
    use windows_sys::Win32::Security::Authentication::Identity::{
        LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose,
        LsaNtStatusToWinError, LsaOpenPolicy, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
    };
    use windows_sys::Win32::Security::{
        LOGON32_LOGON_SERVICE, LOGON32_PROVIDER_DEFAULT, LogonUserW,
    };
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, REG_MULTI_SZ, RegSetKeyValueW};

    use super::super::{Installed, ServiceDefinition, ServicePlatform, Uninstalled};
    use super::{DISPLAY_NAME, RESTART_DELAYS, description};
    use crate::error::{KernelError, Result};
    use crate::win::{UserSid, account_name, wide};

    /// How long a stopping service may take: the kernel's 30-second drain and its stop.
    const STOP_WAIT: Duration = Duration::from_secs(75);

    fn failed(what: &str, error: impl std::fmt::Display) -> KernelError {
        KernelError::Service { reason: format!("{what}: {error}") }
    }

    /// The service control manager, with `access`; refused outside an elevated shell.
    fn manager(access: ServiceManagerAccess) -> Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access).map_err(|error| {
            failed(
                "could not open the service control manager; registering or removing a \
                 service needs an elevated shell (Run as administrator), and the service runs \
                 as you regardless",
                error,
            )
        })
    }

    /// Whether `error` says the service does not exist.
    fn missing(error: &windows_service::Error) -> bool {
        matches!(error, windows_service::Error::Winapi(error)
            if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32))
    }

    /// Registers the service under the current user's account and starts it.
    pub(in super::super) fn install(
        definition: &ServiceDefinition,
        password: Option<&str>,
    ) -> Result<Installed> {
        let account = account_name()?;
        if let Some(requested) = &definition.account
            && !requested.eq_ignore_ascii_case(&account)
        {
            return Err(KernelError::Service {
                reason: format!(
                    "the kernel's service runs as the user who installs it ({account}), not \
                     {requested}: install it from {requested}'s session"
                ),
            });
        }
        let password = password.ok_or_else(|| KernelError::Service {
            reason: format!(
                "the service control manager needs {account}'s password to run the kernel as \
                 {account}: set ADK_SERVICE_PASSWORD or answer the prompt"
            ),
        })?;
        let user = UserSid::current()?;
        grant_service_logon(&user)?;
        check_logon(&account, password)?;

        let info = ServiceInfo {
            name: OsString::from(&definition.name),
            display_name: OsString::from(DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: definition.program.clone(),
            launch_arguments: definition.args.iter().map(OsString::from).collect(),
            dependencies: Vec::new(),
            account_name: Some(OsString::from(&account)),
            account_password: Some(OsString::from(password)),
        };
        let access = ServiceAccess::QUERY_STATUS
            | ServiceAccess::START
            | ServiceAccess::STOP
            | ServiceAccess::CHANGE_CONFIG;
        let scm = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let service = match scm.open_service(&definition.name, access) {
            Ok(service) => {
                stop(&service)?;
                service
                    .change_config(&info)
                    .map_err(|error| failed("could not update the service", error))?;
                service
            }
            Err(error) if missing(&error) => scm
                .create_service(&info, access)
                .map_err(|error| failed("could not register the service", error))?,
            Err(error) => return Err(failed("could not open the service", error)),
        };
        service
            .set_description(description(definition))
            .map_err(|error| failed("could not describe the service", error))?;
        service
            .set_delayed_auto_start(true)
            .map_err(|error| failed("could not delay the service's start", error))?;
        let actions = RESTART_DELAYS
            .iter()
            .map(|&delay| ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(delay),
            })
            .collect();
        service
            .update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
                reboot_msg: None,
                command: None,
                actions: Some(actions),
            })
            .map_err(|error| failed("could not set the service's restarts", error))?;
        service
            .set_failure_actions_on_non_crash_failures(true)
            .map_err(|error| failed("could not set the service's restarts", error))?;
        set_environment(&definition.name, &definition.environment)?;
        service
            .start::<&str>(&[])
            .map_err(|error| failed("the service was registered but did not start", error))?;
        Ok(Installed {
            platform: ServicePlatform::Windows,
            name: definition.name.clone(),
            definition: None,
            log: definition.log_path(),
            notes: vec![
                format!(
                    "it starts at boot as {account}; `Get-Service {}` shows it",
                    definition.name
                ),
                format!(
                    "{account} holds the \"Log on as a service\" right, which the service needs; \
                     after a password change, run `adk-rust up --install` again"
                ),
            ],
        })
    }

    /// Stops the service if it runs and waits until it has stopped.
    fn stop(service: &Service) -> Result<()> {
        let status =
            service.query_status().map_err(|error| failed("could not query the service", error))?;
        if status.current_state == ServiceState::Stopped {
            return Ok(());
        }
        if status.current_state != ServiceState::StopPending
            && let Err(error) = service.stop()
            && !matches!(&error, windows_service::Error::Winapi(error)
                if error.raw_os_error() == Some(ERROR_SERVICE_NOT_ACTIVE as i32))
        {
            return Err(failed("could not stop the service", error));
        }
        let deadline = Instant::now() + STOP_WAIT;
        while Instant::now() < deadline {
            let status = service
                .query_status()
                .map_err(|error| failed("could not query the service", error))?;
            if status.current_state == ServiceState::Stopped {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(KernelError::Service {
            reason: format!("the service did not stop within {} s", STOP_WAIT.as_secs()),
        })
    }

    /// Grants `user` the "Log on as a service" right, which a service under a user's account
    /// needs and which the API that registers it does not grant.
    fn grant_service_logon(user: &UserSid) -> Result<()> {
        // SAFETY: an all-zero LSA_OBJECT_ATTRIBUTES is the documented empty value.
        let mut attributes: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        attributes.Length = size_of::<LSA_OBJECT_ATTRIBUTES>() as u32;
        let mut policy = 0;
        // SAFETY: a null system name is this machine; `attributes` and `policy` are valid.
        let status = unsafe {
            LsaOpenPolicy(
                std::ptr::null(),
                &raw const attributes,
                (POLICY_CREATE_ACCOUNT | POLICY_LOOKUP_NAMES) as u32,
                &raw mut policy,
            )
        };
        if status != 0 {
            return Err(lsa_failed("could not open the local security policy", status));
        }
        let right: Vec<u16> = "SeServiceLogonRight".encode_utf16().collect();
        let right = LSA_UNICODE_STRING {
            Length: (right.len() * 2) as u16,
            MaximumLength: (right.len() * 2) as u16,
            Buffer: right.as_ptr().cast_mut(),
        };
        // SAFETY: `policy` is open, the SID is valid, and `right` points at live UTF-16.
        let status = unsafe { LsaAddAccountRights(policy, user.sid(), &raw const right, 1) };
        // SAFETY: `policy` was opened above and is closed once.
        unsafe { LsaClose(policy) };
        if status != 0 {
            return Err(lsa_failed("could not grant the \"Log on as a service\" right", status));
        }
        Ok(())
    }

    fn lsa_failed(what: &str, status: i32) -> KernelError {
        // SAFETY: converts a status code; no pointers.
        let code = unsafe { LsaNtStatusToWinError(status) };
        let error = std::io::Error::from_raw_os_error(code as i32);
        let hint = if code == ERROR_ACCESS_DENIED {
            "; this needs an elevated shell (Run as administrator)"
        } else {
            ""
        };
        failed(what, format!("{error}{hint}"))
    }

    /// Logs `account` on as a service with `password`, so a wrong password fails here and
    /// not at the service's first start.
    fn check_logon(account: &str, password: &str) -> Result<()> {
        let (domain, user) = account.split_once('\\').unwrap_or((".", account));
        let (domain, user, password) = (wide(domain), wide(user), wide(password));
        let mut token = std::ptr::null_mut();
        // SAFETY: the strings are NUL-terminated; `token` is writable and closed below.
        let logged_on = unsafe {
            LogonUserW(
                user.as_ptr(),
                domain.as_ptr(),
                password.as_ptr(),
                LOGON32_LOGON_SERVICE,
                LOGON32_PROVIDER_DEFAULT,
                &raw mut token,
            )
        };
        if logged_on == 0 {
            return Err(failed(
                &format!("{account} could not log on as a service with that password"),
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: `token` was just returned by `LogonUserW`.
        unsafe { CloseHandle(token) };
        Ok(())
    }

    /// Sets the variables the service starts with, which the service control manager reads
    /// from the service's `Environment` value.
    fn set_environment(name: &str, environment: &[(String, String)]) -> Result<()> {
        let mut block: Vec<u16> = Vec::new();
        for (key, value) in environment {
            block.extend(format!("{key}={value}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        let key = wide(format!("SYSTEM\\CurrentControlSet\\Services\\{name}"));
        let value = wide("Environment");
        // SAFETY: the strings are NUL-terminated and `block` holds the given byte count.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                REG_MULTI_SZ,
                block.as_ptr().cast(),
                (block.len() * 2) as u32,
            )
        };
        if status != 0 {
            return Err(failed(
                "could not set the service's environment",
                std::io::Error::from_raw_os_error(status as i32),
            ));
        }
        Ok(())
    }

    /// Stops the service and removes it from the service control manager.
    pub(in super::super) fn uninstall(name: &str) -> Result<Uninstalled> {
        let scm = manager(ServiceManagerAccess::CONNECT)?;
        let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
        let service = scm.open_service(name, access).map_err(|error| {
            if missing(&error) {
                KernelError::Service { reason: format!("no Windows service {name} is installed") }
            } else {
                failed("could not open the service", error)
            }
        })?;
        stop(&service)?;
        service.delete().map_err(|error| failed("could not remove the service", error))?;
        Ok(Uninstalled {
            platform: ServicePlatform::Windows,
            name: name.to_string(),
            definition: None,
        })
    }
}

#[cfg(windows)]
mod runtime {
    use std::ffi::OsString;
    use std::os::windows::io::IntoRawHandle as _;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::{define_windows_service, service_dispatcher};
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    use crate::error::{KernelError, Result};

    /// The work the service runs, handed from [`run_as_service`] to the service's thread.
    type Work = Box<dyn FnOnce() -> std::result::Result<(), String> + Send>;

    /// The service's name and work, until the dispatcher starts the service.
    static PENDING: Mutex<Option<(String, Work)>> = Mutex::new(None);

    /// The work's outcome, for [`run_as_service`] to return.
    static OUTCOME: Mutex<Option<std::result::Result<(), String>>> = Mutex::new(None);

    define_windows_service!(ffi_service_main, service_main);

    /// Runs `work` as the Windows service `name`, which the service control manager started
    /// (spec 3.2 §10 W3), with the process's standard output and error going to `log`.
    ///
    /// Blocks until the service stops. The service reports running before `work` starts and
    /// stopped after it returns, with an error exit code when it failed, which the failure
    /// actions [`install`](super::super::install) sets turn into a restart. A stop or a
    /// system shutdown resolves [`stop_requested`](super::super::stop_requested), which
    /// [`Kernel::shutdown_on_signal`](crate::Kernel::shutdown_on_signal) waits for; `work`
    /// should drain and return then.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Service`] when the process was not started by the service
    /// control manager, or with `work`'s error.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn example(log: std::fs::File) -> adk_kernel::Result<()> {
    /// adk_kernel::service::run_as_service("adk-rust-kernel-0123456789abcdef", log, || {
    ///     // Start the kernel here and return once `stop_requested` resolved and it drained.
    ///     Ok(())
    /// })
    /// # }
    /// ```
    pub fn run_as_service<F>(name: &str, log: std::fs::File, work: F) -> Result<()>
    where
        F: FnOnce() -> std::result::Result<(), String> + Send + 'static,
    {
        // The process keeps the log open for its life, as its standard handles.
        let log = log.into_raw_handle();
        // SAFETY: `log` is an open file handle this process owns from here on.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, log);
            SetStdHandle(STD_ERROR_HANDLE, log);
        }
        *PENDING.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((name.to_string(), Box::new(work)));
        service_dispatcher::start(name, ffi_service_main).map_err(|error| {
            KernelError::Service {
                reason: format!(
                    "the service control manager did not start this process as {name} ({error}); \
                 `--windows-service` is for the installed service: run `adk-rust up --install`"
                ),
            }
        })?;
        match OUTCOME.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(Err(reason)) => Err(KernelError::Service { reason }),
            _ => Ok(()),
        }
    }

    fn status(state: ServiceState, exit_code: ServiceExitCode, wait: Duration) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: match state {
                ServiceState::Running => {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                }
                _ => ServiceControlAccept::empty(),
            },
            exit_code,
            checkpoint: 0,
            wait_hint: wait,
            process_id: None,
        }
    }

    /// The service's thread, which the dispatcher starts.
    fn service_main(_arguments: Vec<OsString>) {
        let Some((name, work)) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let handle: Arc<OnceLock<ServiceStatusHandle>> = Arc::new(OnceLock::new());
        let reporter = handle.clone();
        let registered = service_control_handler::register(&name, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(handle) = reporter.get() {
                    // The kernel drains for up to 30 seconds before it stops.
                    let _ = handle.set_service_status(status(
                        ServiceState::StopPending,
                        ServiceExitCode::NO_ERROR,
                        Duration::from_secs(60),
                    ));
                }
                super::super::stop_token().cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        });
        let registered = match registered {
            Ok(registered) => registered,
            Err(error) => {
                *OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Err(format!("could not register the service's control handler: {error}")));
                return;
            }
        };
        let _ = handle.set(registered);
        let _ = registered.set_service_status(status(
            ServiceState::Running,
            ServiceExitCode::NO_ERROR,
            Duration::ZERO,
        ));
        let outcome = work();
        let exit_code = match &outcome {
            Ok(()) => ServiceExitCode::NO_ERROR,
            Err(_) => ServiceExitCode::ServiceSpecific(1),
        };
        *OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        let _ =
            registered.set_service_status(status(ServiceState::Stopped, exit_code, Duration::ZERO));
    }
}
