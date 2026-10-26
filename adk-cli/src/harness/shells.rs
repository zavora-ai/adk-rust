//! `/shells`: the session's background shells (spec 3.1 §7.29 "People").

use std::fmt::Write as _;

use adk_core::Result;
use adk_harness::ShellStatus;

use super::launcher::Shared;

/// The command's row in `/help` and the manifest.
pub(super) const DESCRIPTION: &str = "Background shells; /shells kill N stops one";

/// `/shells` lists each shell with its command, age and state; `/shells kill N` stops one.
pub(super) async fn run(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let mut words = args.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => {
            let shells = harness.shells(&shared.user, &shared.session).await;
            if shells.is_empty() {
                return Ok("No background shells in this session.".into());
            }
            let mut out = String::from("Background shells (id, state, age, command):");
            for shell in &shells {
                let _ = write!(out, "\n  {}", shell.line());
            }
            Ok(out)
        }
        (Some("kill"), Some(id)) => {
            let running = harness
                .shells(&shared.user, &shared.session)
                .await
                .iter()
                .any(|shell| shell.shell_id == id && shell.status == ShellStatus::Running);
            Ok(match harness.kill_shell(&shared.user, &shared.session, id).await {
                Some(shell) if running => format!("stopped: {}", shell.line()),
                Some(shell) => {
                    format!("already ended, removed from the reminder: {}", shell.line())
                }
                None => format!("no background shell {id} in this session"),
            })
        }
        _ => Ok("usage: /shells, or /shells kill N".into()),
    }
}
