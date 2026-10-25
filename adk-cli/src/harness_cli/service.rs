//! `adk-rust up --install`, `--uninstall` and `--install --print` (spec 3.2 §7.12, §10 W3):
//! `adk-rust up` kept running as the user by launchd, systemd or the Windows service control
//! manager, through `adk_kernel::service`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_cli::harness::KernelSettings;
use adk_kernel::control::ControlClient;
use adk_kernel::service::{self, ServiceDefinition, ServicePlatform};
use anyhow::{Context, Result, bail};

use super::up::{DEFAULT_BIND, default_home};
use super::user_config;
use crate::cli::UpArgs;

/// How long `--install` waits for the installed kernel to answer.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// The platform's service form, or why there is none.
fn platform() -> Result<ServicePlatform> {
    ServicePlatform::current().context(
        "this platform has no service form for the kernel (launchd on macOS, systemd on \
         Linux, the service control manager on Windows); run `adk-rust up` under your own \
         supervisor",
    )
}

/// The command line this process was started with, as UTF-8.
fn argv() -> Result<Vec<String>> {
    std::env::args_os()
        .map(|word| {
            word.into_string().map_err(|word| {
                anyhow::anyhow!(
                    "the argument {} is not UTF-8; a service definition needs UTF-8 arguments",
                    word.to_string_lossy()
                )
            })
        })
        .collect()
}

/// The absolute kernel home `up` resolves: `--home`, `[kernel] home`, or the config
/// directory's adk-rust, so every command that names the home names one service.
fn kernel_home(up: &UpArgs, settings: &KernelSettings) -> Result<PathBuf> {
    let home = match up.home.clone().or_else(|| settings.home.clone()) {
        Some(home) => home,
        None => default_home()?,
    };
    std::path::absolute(&home).with_context(|| format!("could not resolve {}", home.display()))
}

/// The workspace `up` works in: `--dir`, else the current directory, made absolute.
fn workspace(up: &UpArgs) -> Result<PathBuf> {
    let dir = match &up.harness.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().context("no current directory")?,
    };
    let dir = std::path::absolute(&dir)
        .with_context(|| format!("could not resolve {}", dir.display()))?;
    if !dir.is_dir() {
        bail!("--dir {} is not a directory", dir.display());
    }
    Ok(dir)
}

/// The service's arguments: this command line without the installing flags, with the home
/// and the workspace pinned as absolute paths, and, for a Windows service, the flag that
/// makes `up` answer the service control manager.
///
/// A secret never enters a definition, which others can read: `--api-key`, `--auth-token`
/// and a `--store` URL with a password are refused.
fn service_args(
    argv: &[String],
    home: &Path,
    workspace: &Path,
    windows_service: Option<&str>,
) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut words = argv.iter().skip(1);
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (word.as_str(), None),
        };
        match flag {
            "--install" | "--uninstall" | "--print" => {}
            "--home" | "--dir" | "-d" => {
                if inline.is_none() {
                    words.next();
                }
            }
            "--api-key" | "--auth-token" => bail!(
                "{flag} would be written into the service definition, which others can read: \
                 keep the provider key in the keychain (`adk-rust` saves it there on first \
                 run), and serve the installed kernel on loopback"
            ),
            "--store" => {
                let url = match inline {
                    Some(url) => url.to_string(),
                    None => words.next().cloned().unwrap_or_default(),
                };
                if adk_harness::redact_store_url(&url) != url {
                    bail!(
                        "--store {} carries a password, which would be written into the \
                         service definition: use a URL without one and the backend's own \
                         credentials (such as ~/.pgpass)",
                        adk_harness::redact_store_url(&url)
                    );
                }
                args.extend(["--store".to_string(), url]);
            }
            // `-dDIR`: the workspace in the short flag's attached form.
            _ if word.starts_with("-d") && !word.starts_with("--") => {}
            _ => args.push(word.clone()),
        }
    }
    args.extend([
        "--home".to_string(),
        home.to_string_lossy().into_owned(),
        "--dir".to_string(),
        workspace.to_string_lossy().into_owned(),
    ]);
    if let Some(name) = windows_service {
        args.extend(["--windows-service".to_string(), name.to_string()]);
    }
    Ok(args)
}

/// What `--install` registers for this command line.
fn definition(up: &UpArgs, platform: ServicePlatform) -> Result<ServiceDefinition> {
    // The personal preset's workspace is a folder under the home, so the current directory's
    // `.adk/harness.toml` plays no part (spec 3.3 §10 H1).
    let personal_home =
        up.harness.preset == Some(adk_harness::Preset::Personal) && up.harness.dir.is_none();
    let settings = if personal_home {
        KernelSettings::resolve_user(user_config().as_deref())?
    } else {
        KernelSettings::resolve(user_config().as_deref(), &workspace(up)?)?
    };
    let home = kernel_home(up, &settings)?;
    let workspace = if personal_home {
        adk_harness::personal::PersonalWorkspace::create(&home)?.root().to_path_buf()
    } else {
        workspace(up)?
    };
    let bind = match up.bind.or(settings.bind) {
        Some(bind) => bind,
        None => DEFAULT_BIND.parse()?,
    };
    if !bind.ip().is_loopback() {
        bail!(
            "an installed kernel serves on loopback: beyond it, it needs a bearer token, which \
             the service definition would expose. Bind 127.0.0.1 and put an authenticating \
             proxy in front, or run `adk-rust up --bind {bind} --auth-token …` yourself"
        );
    }
    let name = ServiceDefinition::name_for(&home);
    let windows_service = (platform == ServicePlatform::Windows).then_some(name.as_str());
    let args = service_args(&argv()?, &home, &workspace, windows_service)?;
    let program = std::env::current_exe().context("could not find this adk-rust binary")?;
    let mut definition =
        ServiceDefinition::new(program, &home).with_args(args).with_working_directory(&workspace);
    // Tools the agent runs resolve as they do in this shell; a service manager's own PATH is
    // minimal.
    if let Some(path) = std::env::var_os("PATH") {
        definition = definition.with_env("PATH", path.to_string_lossy());
    }
    Ok(definition)
}

/// `adk-rust up --install --print`: the definition `--install` would register, on stdout.
pub fn print_service(up: &UpArgs) -> Result<()> {
    let platform = platform()?;
    print!("{}", definition(up, platform)?.render(platform));
    Ok(())
}

/// `adk-rust up --install` (spec 3.2 §7.12): registers and starts the service, then waits
/// for the kernel to answer on its socket or pipe.
pub async fn install_service(up: &UpArgs) -> Result<()> {
    let platform = platform()?;
    let definition = definition(up, platform)?;
    // A service has no terminal, so the personal preset's first-run questions are asked here
    // (spec 3.3 §13 Q7).
    if up.harness.preset == Some(adk_harness::Preset::Personal) {
        super::personal_settings(&definition.home)?;
    }
    #[cfg(windows)]
    let password = Some(windows_password()?);
    #[cfg(not(windows))]
    let password: Option<String> = None;
    let installed = tokio::task::spawn_blocking({
        let definition = definition.clone();
        move || service::install(&definition, password.as_deref())
    })
    .await
    .context("the installer stopped")??;
    let form = match platform {
        ServicePlatform::Launchd => "a launchd agent",
        ServicePlatform::Systemd => "a systemd user unit",
        _ => "a Windows service",
    };
    println!("Installed the kernel as {form}: {}", installed.name);
    if let Some(file) = &installed.definition {
        println!("  definition {}", file.display());
    }
    let command: Vec<String> = std::iter::once(definition.program.to_string_lossy().into_owned())
        .chain(definition.args.iter().cloned())
        .map(|word| if word.contains(' ') { format!("\"{word}\"") } else { word })
        .collect();
    println!("  runs {}", command.join(" "));
    println!("  home {}", definition.home.display());
    println!("  log  {}", installed.log.display());
    for note in &installed.notes {
        println!("  note: {note}");
    }
    let deadline = tokio::time::Instant::now() + ANSWER_WAIT;
    loop {
        match ControlClient::connect_local(&definition.home).await {
            Ok(Some(client)) => {
                println!("The kernel answers on {}.", client.endpoint());
                break;
            }
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Ok(None) => {
                println!("The kernel does not answer yet; its log is {}.", installed.log.display());
                break;
            }
            Err(error) => {
                println!("The kernel's local transport refused this shell: {error}");
                break;
            }
        }
    }
    let home = if up.home.is_some() {
        format!(" --home {}", definition.home.display())
    } else {
        String::new()
    };
    println!("Reach it with `adk-rust ps{home}`; remove it with `adk-rust up --uninstall{home}`.");
    Ok(())
}

/// `adk-rust up --uninstall`: stops and removes the service of this kernel home; the home,
/// its stores and its log stay.
pub fn uninstall_service(up: &UpArgs) -> Result<()> {
    platform()?;
    let workspace = std::env::current_dir().context("no current directory")?;
    let settings = KernelSettings::resolve(user_config().as_deref(), &workspace)?;
    let home = kernel_home(up, &settings)?;
    let removed = service::uninstall(&ServiceDefinition::name_for(&home))?;
    println!("Removed the kernel service {} for {}.", removed.name, home.display());
    if let Some(file) = &removed.definition {
        println!("  deleted {}", file.display());
    }
    println!("  kept the kernel home, its stores and its log");
    Ok(())
}

/// The password of the account the service runs as: `ADK_SERVICE_PASSWORD`, or asked on the
/// console without echo.
#[cfg(windows)]
fn windows_password() -> Result<String> {
    use std::io::{IsTerminal as _, Write as _};

    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };

    if let Ok(password) = std::env::var("ADK_SERVICE_PASSWORD")
        && !password.is_empty()
    {
        return Ok(password);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "the Windows service runs as you, so the service control manager needs your \
             password: set ADK_SERVICE_PASSWORD, or run --install from a console"
        );
    }
    print!("Your Windows password (the service runs as you; it is not stored by adk-rust): ");
    std::io::stdout().flush()?;
    // SAFETY: the standard input handle needs no closing; `mode` is writable.
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut mode = 0;
    // SAFETY: as above.
    let console = unsafe { GetConsoleMode(input, &raw mut mode) } != 0;
    if console {
        // SAFETY: turns echo off on the console this process reads; restored below.
        unsafe { SetConsoleMode(input, mode & !ENABLE_ECHO_INPUT) };
    }
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    if console {
        // SAFETY: restores the mode read above.
        unsafe { SetConsoleMode(input, mode) };
    }
    println!();
    read.context("could not read the password")?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// `up` as the Windows service the service control manager started (spec 3.2 §10 W3): the
/// process's output goes to the owner-only `<home>/kernel.log`, the kernel runs until the
/// service is stopped, and an error stops the service with a failure exit code, which the
/// installed failure actions turn into a restart.
#[cfg(windows)]
pub async fn run_windows_service(
    name: String,
    options: crate::cli::AgentOptions,
    mut up: UpArgs,
) -> Result<()> {
    // Services start in the system directory; relative flags resolve against the workspace.
    if let Some(dir) = &up.harness.dir {
        std::env::set_current_dir(dir)
            .with_context(|| format!("could not enter {}", dir.display()))?;
    }
    let home = up.home.clone().context("the kernel's service runs with --home")?;
    std::fs::create_dir_all(&home)
        .with_context(|| format!("failed to create {}", home.display()))?;
    let log_path = home.join(service::LOG_FILE);
    let log = crate::owner_only::open_append(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    up.auth_token = std::env::var("ADK_SERVE_TOKEN").ok();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        service::run_as_service(&name, log, move || {
            // The dispatcher's service thread has the executable's 1 MiB stack, which a debug
            // build of `up` outgrows as it would the main thread's.
            crate::on_command_stack("service", move || {
                runtime.block_on(async move {
                    let ran = async {
                        let model = crate::resolve_model(&options).await?;
                        super::up(model, options.instruction.clone(), up).await
                    };
                    ran.await.map_err(|error| {
                        eprintln!("adk-rust up: {error:#}");
                        format!("{error:#}")
                    })
                })
            })
            .map_err(|error| format!("could not start the service's thread: {error}"))?
        })
    })
    .await
    .context("the service stopped abnormally")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn the_service_runs_this_command_line_without_the_installing_flags() {
        let home = Path::new("/srv/kernel");
        let workspace = Path::new("/work/project");
        let args = service_args(
            &words(
                "adk-rust --provider anthropic up --install --mode ask --home ./k --dir . \
                 --bind 127.0.0.1:9000 --print",
            ),
            home,
            workspace,
            None,
        )
        .unwrap();
        assert_eq!(
            args,
            words(
                "--provider anthropic up --mode ask --bind 127.0.0.1:9000 --home /srv/kernel \
                 --dir /work/project"
            )
        );

        let args = service_args(
            &words("adk-rust up --install --home=./k -d . --store redis://cache:6379/0"),
            home,
            workspace,
            Some("adk-rust-kernel-0123456789abcdef"),
        )
        .unwrap();
        assert_eq!(
            args,
            words(
                "up --store redis://cache:6379/0 --home /srv/kernel --dir /work/project \
                 --windows-service adk-rust-kernel-0123456789abcdef"
            )
        );
        let args = service_args(&words("adk-rust up --install -d."), home, workspace, None);
        assert_eq!(args.unwrap(), words("up --home /srv/kernel --dir /work/project"));
    }

    #[test]
    fn secrets_never_enter_the_definition() {
        let home = Path::new("/srv/kernel");
        for line in [
            "adk-rust --api-key sk-1 up --install",
            "adk-rust up --install --auth-token=t0k3n",
            "adk-rust up --install --store postgres://harness:hunter2@db/harness",
            "adk-rust up --install --store=redis://:hunter2@cache:6379",
        ] {
            let error = service_args(&words(line), home, home, None).unwrap_err().to_string();
            assert!(!error.contains("hunter2") && !error.contains("t0k3n"), "{error}");
            assert!(error.contains("definition"), "{line}: {error}");
        }
    }
}
