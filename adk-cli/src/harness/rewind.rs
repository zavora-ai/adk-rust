//! `/rewind`, `/rewind N` and `/redo` at the console, and `adk-rust sessions rewind`
//! (spec 3.1 §7.27): the plan is shown and confirmed once, then applied.

use std::fmt::Write as _;

use adk_core::{Result, SessionId, UserId};
use adk_harness::snapshot::{RewindPlan, RewindTarget};
use adk_harness::{Harness, escape_control};

use super::launcher::Shared;
use crate::console::{ConsoleInput, ConsoleIo};

/// `/rewind`: the session's turns with their changed-file counts; `/rewind N`: the plan
/// for going back to before turn N, asked once.
pub(crate) async fn rewind(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    if harness.snapshots().is_none() {
        return Ok(off());
    }
    let args = args.trim();
    if args.is_empty() {
        return list(&harness, &shared.user, &shared.session).await;
    }
    let Ok(turn) = args.parse::<u32>() else {
        return Ok(format!(
            "/rewind takes a turn number, such as /rewind 2; /rewind lists them. Got '{}'.",
            escape_control(args)
        ));
    };
    let plan = harness.rewind_plan(&shared.user, &shared.session, RewindTarget::Turn(turn)).await?;
    confirm_and_apply(&harness, &shared.user, &shared.session, plan, &shared.console, false).await
}

/// `/redo`: restores the snapshot taken before the last rewind, asked once.
pub(crate) async fn redo(shared: &Shared) -> Result<String> {
    let harness = shared.harness();
    if harness.snapshots().is_none() {
        return Ok(off());
    }
    let plan = harness.redo_plan(&shared.user, &shared.session).await?;
    confirm_and_apply(&harness, &shared.user, &shared.session, plan, &shared.console, false).await
}

/// `adk-rust sessions rewind ID [N]` (spec 3.1 §7.27): without `turn` the session's turns;
/// with it, or with `redo`, the plan, asked once at the terminal unless `yes`, then applied.
/// `yes` keeps the files a command or the person changed since the latest snapshot.
///
/// # Example
///
/// ```rust,ignore
/// let text = adk_cli::harness::rewind_session(&harness, &user, &session, Some(2), false, false).await?;
/// println!("{text}");
/// ```
///
/// # Errors
///
/// Returns the harness's rewind error, or the terminal's when it cannot be opened.
pub async fn rewind_session(
    harness: &Harness,
    user: &UserId,
    session: &SessionId,
    turn: Option<u32>,
    redo: bool,
    yes: bool,
) -> Result<String> {
    let plan = match (turn, redo) {
        (_, true) => harness.redo_plan(user, session).await?,
        (Some(turn), false) => harness.rewind_plan(user, session, RewindTarget::Turn(turn)).await?,
        (None, false) => return list(harness, user, session).await,
    };
    let console = if yes { ConsoleIo::scripted(Vec::new(), false) } else { ConsoleIo::terminal()? };
    confirm_and_apply(harness, user, session, plan, &console, yes).await
}

fn off() -> String {
    "Snapshots are off (--no-snapshots or [snapshots] enabled = false), so there is nothing to \
     rewind to."
        .into()
}

/// The session's turns, as `/rewind` lists them.
pub(crate) async fn list(harness: &Harness, user: &UserId, session: &SessionId) -> Result<String> {
    let turns = harness.rewind_turns(user, session).await?;
    if turns.is_empty() {
        return Ok("No turns yet: nothing to rewind.".into());
    }
    let mut out = String::from("Turns (/rewind N goes back to before turn N):\n");
    for turn in &turns {
        let changed = match (&turn.snapshot_id, turn.changed_files) {
            (None, _) => "no snapshot".to_string(),
            (Some(_), Some(1)) => "1 file changed".to_string(),
            (Some(_), Some(n)) => format!("{n} files changed"),
            (Some(_), None) => "snapshot".to_string(),
        };
        let partial = if turn.partial { ", partial snapshot" } else { "" };
        let _ = writeln!(
            out,
            "  {:>3}  {}  ({changed}{partial})",
            turn.number,
            escape_control(&turn.preview)
        );
    }
    let _ = write!(out, "/redo undoes the last rewind.");
    Ok(out)
}

/// Shows `plan`, asks once (or not, with `yes`), and applies it.
///
/// With conflicts the question offers to keep them or restore them too; the one answer is
/// also the confirmation of every protected path the plan lists (I29).
pub(crate) async fn confirm_and_apply(
    harness: &Harness,
    user: &UserId,
    session: &SessionId,
    mut plan: RewindPlan,
    console: &ConsoleIo,
    yes: bool,
) -> Result<String> {
    let shown = plan.render();
    if plan.is_empty() {
        return Ok(format!(
            "{shown}Nothing to rewind: the workspace, transcript and plan match the snapshot."
        ));
    }
    let conflicts = plan.conflicts().count();
    let choice = if yes {
        Choice::KeepConflicts
    } else {
        console.print(&shown);
        let question = if conflicts > 0 {
            "Rewind? [y] yes, keep the conflicting files  [a] yes, restore them too  [N] no > "
        } else {
            "Rewind? [y/N] > "
        };
        match console.read_line(question).await {
            ConsoleInput::Line(answer) => match answer.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => Choice::KeepConflicts,
                "a" | "all" if conflicts > 0 => Choice::RestoreConflicts,
                _ => Choice::No,
            },
            _ => Choice::No,
        }
    };
    match choice {
        Choice::No => {
            return Ok(if yes { shown } else { "Not rewound: nothing changed.".into() });
        }
        Choice::KeepConflicts => plan.choose_conflicts(false),
        Choice::RestoreConflicts => plan.choose_conflicts(true),
    }
    plan.confirm_protected();
    let report = harness.rewind(user, session, &plan).await?;
    let mut out = String::new();
    if yes {
        out.push_str(&shown);
    }
    out.push_str(&report.render());
    Ok(out)
}

enum Choice {
    No,
    KeepConflicts,
    RestoreConflicts,
}
