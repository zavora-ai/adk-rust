//! The kernel as a launchd agent (spec 3.2 §7.12): an agent, not a daemon, so it runs in the
//! user's session, where the login keychain is.

use std::path::PathBuf;

use super::ServiceDefinition;
use crate::error::{KernelError, Result};

/// `~/Library/LaunchAgents/<name>.plist`.
pub(super) fn plist_path(name: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| KernelError::Service {
        reason: "could not determine the user's home directory; set HOME".into(),
    })?;
    Ok(home.join("Library/LaunchAgents").join(format!("{name}.plist")))
}

/// The agent's property list.
pub(super) fn render(definition: &ServiceDefinition) -> String {
    let string = |key: &str, value: &str| {
        format!("    <key>{key}</key>\n    <string>{}</string>\n", escape(value))
    };
    let log = definition.log_for(super::ServicePlatform::Launchd);
    let mut plist = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");
    plist.push_str(&string("Label", &definition.name));
    plist.push_str("    <key>ProgramArguments</key>\n    <array>\n");
    let program = definition.program.to_string_lossy();
    for argument in
        std::iter::once(program.as_ref()).chain(definition.args.iter().map(String::as_str))
    {
        plist.push_str(&format!("        <string>{}</string>\n", escape(argument)));
    }
    plist.push_str("    </array>\n");
    plist.push_str(&string("WorkingDirectory", &definition.working_directory.to_string_lossy()));
    if !definition.environment.is_empty() {
        plist.push_str("    <key>EnvironmentVariables</key>\n    <dict>\n");
        for (key, value) in &definition.environment {
            plist.push_str(&format!(
                "        <key>{}</key>\n        <string>{}</string>\n",
                escape(key),
                escape(value)
            ));
        }
        plist.push_str("    </dict>\n");
    }
    plist.push_str("    <key>RunAtLoad</key>\n    <true/>\n");
    // Restarted when it exits with an error or dies; a drained stop exits 0 and stays down.
    plist.push_str("    <key>KeepAlive</key>\n    <dict>\n");
    plist.push_str("        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n");
    plist.push_str("    <key>ThrottleInterval</key>\n    <integer>10</integer>\n");
    // Longer than the kernel's 30-second drain on SIGTERM.
    plist.push_str("    <key>ExitTimeOut</key>\n    <integer>60</integer>\n");
    // 0o077: whatever the kernel creates is the user's alone.
    plist.push_str("    <key>Umask</key>\n    <integer>63</integer>\n");
    plist.push_str(&string("StandardOutPath", &log));
    plist.push_str(&string("StandardErrorPath", &log));
    plist.push_str("</dict>\n</plist>\n");
    plist
}

/// `text` with the characters XML reserves escaped.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(target_os = "macos")]
pub(super) use installer::{install, uninstall};

#[cfg(target_os = "macos")]
mod installer {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Duration;

    use super::super::{
        Installed, ServiceDefinition, ServicePlatform, Uninstalled, prepare_log, run,
        write_definition,
    };
    use super::plist_path;
    use crate::error::{KernelError, Result};

    /// Writes the plist and bootstraps it into the user's GUI session, or into the user
    /// domain where there is none, as over SSH.
    pub(in super::super) fn install(definition: &ServiceDefinition) -> Result<Installed> {
        let plist = plist_path(&definition.name)?;
        let log = prepare_log(definition)?;
        write_definition(&plist, &definition.render(ServicePlatform::Launchd))?;
        // This user just wrote the plist, so its owner is the uid to load it for.
        let uid = std::fs::metadata(&plist)?.uid();
        let plist_text = plist.to_string_lossy().into_owned();
        let mut errors = Vec::new();
        for domain in [format!("gui/{uid}"), format!("user/{uid}")] {
            // Replaces a loaded agent of the same name; nothing to boot out is fine.
            let _ = run("launchctl", &["bootout", &format!("{domain}/{}", definition.name)]);
            match bootstrap(&domain, &plist_text) {
                Ok(()) => {
                    let mut notes = Vec::new();
                    if domain.starts_with("user/") {
                        notes.push(
                            "no GUI session: the agent runs in the user domain, where the login \
                             keychain may be locked"
                                .to_string(),
                        );
                    }
                    notes.push(format!(
                        "it starts again at each login; `launchctl print {domain}/{}` shows it",
                        definition.name
                    ));
                    return Ok(Installed {
                        platform: ServicePlatform::Launchd,
                        name: definition.name.clone(),
                        definition: Some(plist),
                        log,
                        notes,
                    });
                }
                Err(error) => errors.push(error),
            }
        }
        Err(KernelError::Service {
            reason: format!(
                "launchd did not load {}: {}; the plist stays, so `launchctl bootstrap gui/{uid} \
                 {plist_text}` retries it",
                definition.name,
                errors.join("; ")
            ),
        })
    }

    /// `launchctl bootstrap`, retried while a booted-out agent of the same name is still
    /// stopping.
    fn bootstrap(domain: &str, plist: &str) -> std::result::Result<(), String> {
        let mut attempt = 0;
        loop {
            match run("launchctl", &["bootstrap", domain, plist]) {
                Ok(_) => return Ok(()),
                Err(error) if attempt < 10 && error.contains("Input/output error") => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Boots the agent out of whichever domain holds it and removes its plist.
    pub(in super::super) fn uninstall(name: &str) -> Result<Uninstalled> {
        let plist = plist_path(name)?;
        let home = dirs::home_dir().ok_or_else(|| KernelError::Service {
            reason: "could not determine the user's home directory; set HOME".into(),
        })?;
        let uid = std::fs::metadata(&home)?.uid();
        let booted = [format!("gui/{uid}/{name}"), format!("user/{uid}/{name}")]
            .iter()
            .filter(|target| run("launchctl", &["bootout", target]).is_ok())
            .count();
        let existed = plist.exists();
        if existed {
            std::fs::remove_file(&plist)?;
        }
        if booted == 0 && !existed {
            return Err(KernelError::Service {
                reason: format!("no launchd agent {name} is installed (no {})", plist.display()),
            });
        }
        Ok(Uninstalled {
            platform: ServicePlatform::Launchd,
            name: name.to_string(),
            definition: existed.then_some(plist),
        })
    }
}
