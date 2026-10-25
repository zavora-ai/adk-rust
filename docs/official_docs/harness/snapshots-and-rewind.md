# Snapshots and Rewind

The harness snapshots the whole workspace before each user turn, so a rewind restores what a
`bash` command wrote as well as what `write_file` and `edit_file` wrote (spec 3.1 §7.27). A
rewind puts back the files, the conversation and the plan from one snapshot, after showing
what it will do and what it cannot undo; `/redo` puts back what the rewind replaced.

| Part | Behaviour |
|---|---|
| When | Before each user turn (the first model call of a run that carries a user message), before each plan-graph step, before a rewind, and whenever `Snapshots::take` or `Harness::snapshot_now` is called — for example before a `Destructive` git call |
| What | Every file under the workspace root: `.gitignore` honoured, `.git` never opened, `target`, `node_modules`, `.venv`, `venv`, `dist`, `build`, `__pycache__` and `.next` skipped; files matching `Workspace::exclude_globs` (`.env`, keys, credentials) and files over 8 MiB are listed as not covered; the walk stops at 50,000 files and the snapshot is partial |
| Where | Objects addressed by their SHA-256 under `<config dir>/adk-rust/snapshots/<sha256 of the workspace root>/`, beside `harness.db` (the kernel home under `adk-rust up --home`); one record per snapshot in `harness.db`. Never in the workspace, never in its `.git` |
| Sealed | With a store key configured, every object and record is sealed with the store's envelope; `adk-rust store rotate-key` re-seals them |
| Cost | A stat cache (size, modification time, inode) means an unchanged file is not read again; an unchanged file is stored once however many snapshots hold it |
| Retention | The last 50 turn snapshots per session and 2 GiB of objects per workspace, oldest first; objects no snapshot references are collected; deleting a session deletes its snapshots |

## At the terminal

```text
You > /rewind
Turns (/rewind N goes back to before turn N):
    1  write the notes and run the script  (3 files changed)
/redo undoes the last rewind.
You > /rewind 1
Rewind before turn 1 — snapshot snap-6f2c… (turn, 2026-10-25 14:02:11 UTC)
  delete    1 file created since, removed: notes.txt
  conflicts 2 files changed since the latest snapshot by a command or by you, kept unless you choose them: kept.txt (restore), shell.txt (delete)
  not undone (effects outside the workspace stay): bash: echo 'from bash' > shell.txt && …
  transcript forks before turn 1: 7 events removed
Rewind? [y] yes, keep the conflicting files  [a] yes, restore them too  [N] no > a
Rewound: 1 restored, 0 recreated, 2 deleted; transcript: 7 events removed, 0 events put back.
/redo restores what was there before this rewind.
```

| Command | Effect |
|---|---|
| `/rewind` | The session's turns with the files each changed |
| `/rewind N` | Shows the plan for going back to before turn N and asks once |
| `/redo` | Restores the snapshot taken before the last rewind: files, transcript and plan |
| `adk-rust sessions rewind ID [N]` | The same outside the REPL; `--redo` undoes the last rewind, `--yes` applies without asking and keeps conflicting files and protected paths |
| `--no-snapshots`, `[snapshots] enabled = false` | Takes no snapshots; `/rewind` then says there is nothing to rewind to |

The `[snapshots]` table of the user config sets the limits; `.adk/harness.toml` cannot turn
snapshots off or change them (spec 3.1 §9.2 I3):

```toml
[snapshots]
enabled = true
keep_turns = 50
max_bytes = 2147483648
max_files = 50000
max_file_bytes = 8388608
skip_dirs = ["target", "node_modules", ".venv", "venv", "dist", "build", "__pycache__", ".next"]
```

## What the plan shows

| Entry | Content |
|---|---|
| Restore, recreate, delete | Files modified, deleted or created since the target |
| Conflicts | Files changed since the session's latest snapshot that no harness file tool wrote — a command or you changed them. Each is shown and restored only when chosen (`[a]`) |
| Protected | Protected paths among them (`AGENTS.md`, `.git/**`, `.adk/**`, …): changed only under the rewind's confirmation, which lists them |
| Not covered | Files the snapshot left out — too large, excluded, or past a partial walk — listed and never touched; a partial snapshot deletes nothing it did not reach |
| Not undone | Every call after the target that a file restore cannot take back: commands, payments, messages, pushes, with their action-ledger keys |
| Transcript | The session forks at the target turn: the events after it are removed and kept with the snapshot taken before the rewind, so `/redo` puts them back |
| Plan | The plan graph rewinds with the files: to just after the last step completed at the target, or back to its first step |

A path whose content changed between the plan and the rewind is left as it is and reported.
A rewind writes only inside the workspace root and never through a symlink: a file is written
to a temporary sibling and renamed into place, and a path under a symlinked directory is
refused.

## In the library

Snapshots are off until the builder sets them. Records and objects stay in memory unless a
store and an object directory are given:

```rust,no_run
use std::sync::Arc;

use adk_core::{Llm, SessionId, UserId};
use adk_harness::snapshot::{FileSnapshotObjects, RewindTarget, SnapshotConfig};
use adk_harness::HarnessAgent;
use adk_session::SessionService;

async fn rewind_last_turn(
    model: Arc<dyn Llm>,
    sessions: Arc<dyn SessionService>,
    user: &UserId,
    session: &SessionId,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new("./my-repo").canonicalize()?;
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(&root).allow_bash(true))
        .snapshots(SnapshotConfig::default())
        .snapshot_objects(Arc::new(FileSnapshotObjects::for_workspace(
            std::path::Path::new("/var/lib/my-app/snapshots"),
            &root,
        )))
        .build()?
        .into_harness("my-app", sessions)?;

    let turns = harness.rewind_turns(user, session).await?;
    let Some(last) = turns.last() else { return Ok(()) };
    let mut plan = harness.rewind_plan(user, session, RewindTarget::Turn(last.number)).await?;
    println!("{}", plan.render());
    plan.choose_conflicts(false);
    plan.confirm_protected();
    let report = harness.rewind(user, session, &plan).await?;
    println!("{}", report.render());
    Ok(())
}
```

| Item | Role |
|---|---|
| `HarnessAgentBuilder::snapshots`, `snapshot_store`, `snapshot_objects` | Turns snapshots on; the record store (`SqliteSnapshotStore` in `harness.db`, `InMemorySnapshotStore`) and the object store (`FileSnapshotObjects`, `InMemorySnapshotObjects`) |
| `Harness::rewind_turns`, `rewind_plan`, `redo_plan`, `rewind` | List, plan, and apply; `RewindTarget::Turn(n)`, `PlanStep(id)` or `Snapshot(id)` |
| `Harness::snapshot_now`, `HarnessRuntime::snapshots().take(..)` | Take a snapshot now, outside or inside a run |
| `adk_harness::snapshot::walk` | The breadth-first walker the snapshots and `adk-rust ultracode` share |

Errors carry codes: `harness.snapshot.off` (snapshots are off), `harness.rewind.no_turn`,
`harness.rewind.no_snapshot`, `harness.rewind.nothing_to_redo`, `harness.rewind.run_active`
(the session is running a turn), `harness.rewind.transcript_changed` and
`harness.snapshot.inside_workspace` (an object directory inside the workspace is refused).

## Records

| Record | Fields |
|---|---|
| `harness.snapshot.taken` | `snapshotId`, `kind` (`turn`, `step`, `destructive`, `rewind`), `files`, `bytesNew`, `partial`, `notCovered` |
| `harness.rewind` | `target`, `snapshotId`, `beforeRewind`, `restored`, `recreated`, `deleted`, `conflictsChosen`, `protectedConfirmed`, `effectsNotUndone` (ledger keys), `transcriptForkedAt`, `eventsRemoved`, `eventsRestored`, `planThread`; written before anything changes |

The `<harness>` block states what a rewind covers in this build, and that a snapshot which
reaches the file cap is partial, so the model never promises an undo that does not happen
([What the harness knows about itself](self-knowledge.md)).

## Related

- [The harness in your terminal](cli.md)
- [Plans and goals](plan-and-goals.md#checkpoints-and-rewind)
- [Durability](durability.md)
