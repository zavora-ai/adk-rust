//! The kernel as a systemd user unit (spec 3.2 §7.12): the user's own service manager runs
//! it, so it reaches what the user's session reaches.

use std::path::PathBuf;

use super::ServiceDefinition;
use crate::error::{KernelError, Result};

/// `<config dir>/systemd/user/<name>.service`.
pub(super) fn unit_path(name: &str) -> Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| KernelError::Service {
        reason: "could not determine the user's config directory; set HOME or XDG_CONFIG_HOME"
            .into(),
    })?;
    Ok(config.join("systemd/user").join(format!("{name}.service")))
}

/// The unit file.
pub(super) fn render(definition: &ServiceDefinition) -> String {
    let home = definition.home.to_string_lossy();
    let mut command = vec![word(&definition.program.to_string_lossy())];
    command.extend(definition.args.iter().map(String::as_str).map(word));
    let mut unit = String::from("[Unit]\n");
    unit.push_str(&format!("Description=ADK-Rust agent kernel ({})\n\n", specifiers(&home)));
    unit.push_str("[Service]\nType=simple\n");
    unit.push_str(&format!("ExecStart={}\n", command.join(" ")));
    unit.push_str(&format!(
        "WorkingDirectory={}\n",
        specifiers(&definition.working_directory.to_string_lossy())
    ));
    for (key, value) in &definition.environment {
        unit.push_str(&format!("Environment={}\n", quoted(&specifiers(&format!("{key}={value}")))));
    }
    // 0077: whatever the kernel creates is the user's alone.
    unit.push_str("UMask=0077\n");
    // Restarted when it exits with an error or dies; a drained stop exits 0 and stays down.
    unit.push_str("Restart=on-failure\nRestartSec=5\n");
    // Longer than the kernel's 30-second drain on SIGTERM.
    unit.push_str("TimeoutStopSec=60\n");
    unit.push_str(&format!(
        "StandardOutput=append:{}\n",
        specifiers(&definition.log_for(super::ServicePlatform::Systemd))
    ));
    unit.push_str("StandardError=inherit\n\n[Install]\nWantedBy=default.target\n");
    unit
}

/// `text` with systemd's `%` specifiers escaped.
fn specifiers(text: &str) -> String {
    text.replace('%', "%%")
}

/// One word of a command line: specifiers and variables escaped, quoted when it holds
/// whitespace, quotes, backslashes or is empty.
fn word(text: &str) -> String {
    let escaped = specifiers(text).replace('$', "$$");
    let plain = !escaped.is_empty()
        && !escaped.chars().any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';'));
    if plain { escaped } else { quoted(&escaped) }
}

/// `text` in double quotes, with C-style escapes for what the quotes cannot hold.
fn quoted(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(target_os = "linux")]
pub(super) use installer::{install, uninstall};

#[cfg(target_os = "linux")]
mod installer {
    use super::super::{
        Installed, ServiceDefinition, ServicePlatform, Uninstalled, prepare_log, run,
        write_definition,
    };
    use super::unit_path;
    use crate::error::{KernelError, Result};

    /// Why `systemctl --user` failed, with the usual remedy for a missing user manager.
    fn refused(error: String) -> KernelError {
        let remedy = if error.contains("bus") || error.contains("XDG_RUNTIME_DIR") {
            "; the user's systemd instance is not reachable from this shell: run the command \
             from a login session, or enable lingering with `loginctl enable-linger` and set \
             XDG_RUNTIME_DIR=/run/user/$(id -u)"
        } else {
            ""
        };
        KernelError::Service { reason: format!("{error}{remedy}") }
    }

    /// Writes the unit, enables it for the user's service manager and (re)starts it.
    pub(in super::super) fn install(definition: &ServiceDefinition) -> Result<Installed> {
        let unit = unit_path(&definition.name)?;
        let log = prepare_log(definition)?;
        write_definition(&unit, &definition.render(ServicePlatform::Systemd))?;
        let service = format!("{}.service", definition.name);
        run("systemctl", &["--user", "daemon-reload"]).map_err(refused)?;
        run("systemctl", &["--user", "enable", &service]).map_err(refused)?;
        run("systemctl", &["--user", "restart", &service]).map_err(refused)?;
        let mut notes = vec![format!(
            "`systemctl --user status {service}` shows it; `journalctl` holds nothing, the \
             kernel writes to its log"
        )];
        let user = std::env::var("USER").unwrap_or_default();
        let lingering = run("loginctl", &["show-user", &user, "--property=Linger", "--value"])
            .is_ok_and(|linger| linger.trim() == "yes");
        if !lingering {
            notes.push(
                "it starts with your first login; to start it at boot without a login, run \
                 `loginctl enable-linger`"
                    .to_string(),
            );
        }
        Ok(Installed {
            platform: ServicePlatform::Systemd,
            name: definition.name.clone(),
            definition: Some(unit),
            log,
            notes,
        })
    }

    /// Stops and disables the unit and removes its file.
    pub(in super::super) fn uninstall(name: &str) -> Result<Uninstalled> {
        let unit = unit_path(name)?;
        let service = format!("{name}.service");
        if !unit.exists() {
            return Err(KernelError::Service {
                reason: format!(
                    "no systemd user unit {service} is installed (no {})",
                    unit.display()
                ),
            });
        }
        run("systemctl", &["--user", "disable", "--now", &service]).map_err(refused)?;
        std::fs::remove_file(&unit)?;
        run("systemctl", &["--user", "daemon-reload"]).map_err(refused)?;
        Ok(Uninstalled {
            platform: ServicePlatform::Systemd,
            name: name.to_string(),
            definition: Some(unit),
        })
    }
}
