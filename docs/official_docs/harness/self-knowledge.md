# What the harness knows about itself

A model judges a harness by what it is told about it. A `HarnessAgent` tells it three
things, generated from the harness that is actually running rather than written by hand
(spec 3.1 §7.25):

| Part | Where | Content |
|---|---|---|
| The `<harness>` block | The system instruction, after `<environment>` | What each class of call gets in each mode, the commands the person types, members, packs, web search, memory, and what each safety mechanism covers |
| The standing-rules digest | The reminder at the end of each request | The standing rules that apply to this user and session, deny before allow |
| `harness_help` | A tool, class `Read`, declared in every mode | The detail on demand: every command, a subcommand's long help, a documentation page, or the whole manifest as JSON |

With them the model can predict which of its calls will prompt, answer "can I resume this
later?" with the right command, and never promise an undo the build does not have.

## The `<harness>` block

The block is built once, when the harness is built, from the tool-class registry and the
mode table resolved under the build's switches. Its class lines say what happens in this
harness: with `allow_host_shell_in_auto`, the host shell line reads `allow` in `auto`.

```text
<harness version="3.0.0" surface="adk-rust 3.0.0">
Classes (read-only | plan | ask | auto):
  read, todos and members (glob grep harness_help read_file update_todo write_todos): allow | allow | allow | allow
  plan exit (exit_plan_mode): deny | ask every time | deny | deny
  edit (apply_patch edit_file write_file): deny | deny | ask | allow
  edit, protected path (AGENTS.md, CLAUDE.md, MEMORY.md, .envrc, lefthook.yml and anything under .git, .adk, .skills or .claude): deny | deny | ask every time | ask every time
  shell (host) (bash): deny | deny | ask | ask
  other (MCP servers, drivers, skills, extras): deny | deny | ask | ask
Standing rules can turn "ask" into allow or deny; the reminder lists the ones that apply. "ask every time" is never covered by a rule.
Commands the person types (you cannot run them; when input looks like a mistyped command, name the closest one and say what it does, and never claim a command ran):
  /approvals  Calls in this session waiting for a decision
  ...
Packs: browser (off: --pack browser), payments (not compiled), ...
Web search: anthropic's built-in search, every domain
Memory: working notes, long-term facts (embedded by onnx/BAAI/bge-small-en-v1.5)
Edits: edit_file takes several replacements in one file as edits; apply_patch changes several files, all or none; prefer edit_file across files. An edit of a file changed since you read it is refused as stale: read it again.
Safety (promise only what these cover):
  action ledger: covers ...; does not cover ...
  rewind: covers the plan graph's position only; does not cover files: no file is restored; ...
harness_help explains the harness on demand; topics: cli <subcommand>, commands, manifest, or a page name.
</harness>
```

With snapshots on — adk-cli's default — the `rewind` line says instead that the whole
workspace is snapshotted before each user turn and plan step, shell writes included, that
`/rewind` restores files, transcript and plan, what is skipped, and that a snapshot reaching
the file cap is partial ([Snapshots and rewind](snapshots-and-rewind.md)).

| Property | Rule |
|---|---|
| Size | At most 6 KiB (`HARNESS_BLOCK_MAX_BYTES`); a longer block ends with the sections it cut and points at `harness_help` |
| Stability | Sorted, with no timestamp and no session data: two builds with the same configuration render the same bytes, and the block stays in the cached prefix of every request |
| Wording | Slash commands are typed by the person; the model names the closest command when input looks mistyped and never claims one ran |
| Edit tools | Every model is offered `write_file`, `edit_file` and `apply_patch`, so the tool surface is one per process; the `Edits:` line names the multi-file tool the model's family works best with, from `PREFERRED_MULTI_FILE_EDIT` (`apply_patch` for OpenAI models, `edit_file` otherwise), data the eval suite can revise (spec 3.1 §13 Q12) |

## The standing-rules digest

Rules change during a session, so they are listed in the per-request reminder, rebuilt for
every request and never saved, rather than in the cached prefix:

```text
Standing rules for you here; a call one matches runs or is refused without asking:
- [5] deny edit_file: within migrations/ (every session): "schema changes go through review"
- [3] allow bash: commands starting "cargo test" (every session)
(+4 more; /rules lists them)
```

| Rule | Mechanism |
|---|---|
| What is listed | Rules of this app, user and session that a declared tool's origin admits; deny before allow, then by number |
| Numbering | As `/rules` numbers them: every rule of the user, oldest first |
| Cap | 20 rules or 1 KiB of rule lines, then `(+N more; /rules lists them)` |
| Secrets | Labels and reasons pass the secret redaction; a deny reason is quoted and cut at 80 characters |

## `harness_help`

| `topic` | Returns |
|---|---|
| none | The topic list and the block |
| `commands` | Every command the surface lists, with its summary |
| `cli <subcommand>` | That subcommand's long help |
| `manifest` | The manifest as JSON |
| a page name | A page the surface registered with `help_page` |

Answers are plain text inside `<harness-help topic="…" version="…">`, at most 32 KiB, and
read nothing from the workspace or the network.

## Describing your surface

`adk-rust` describes itself: every slash command of the REPL, every subcommand with its
`about` and long help, and the flags of the REPL and of `rules add`, `memory`, `sessions`,
`up` and `wake`, generated from its command table and its clap tree, and whether the session
runs under `adk-rust up`. The flags are the first section cut when the block runs past
6 KiB; `harness_help commands` returns them.

A harness built as a library reports `SurfaceManifest::library()`. An application that puts
its own commands in front of the harness describes them, so the model can name them:

```rust
use std::sync::Arc;

use adk_harness::{CommandEntry, HarnessAgent, InstructionsConfig, SurfaceManifest};
use adk_model::{ScriptedLlm, ScriptedResponse};

fn main() -> adk_core::Result<()> {
    let dir = tempfile::tempdir().expect("a temporary workspace");
    let surface = SurfaceManifest::new("my-app", "1.0.0")
        .command(CommandEntry::slash("approvals", "Calls waiting for a decision"))
        .command(CommandEntry::subcommand(
            "resume",
            "Continue the most recent session",
            Some("Usage: my-app resume [SESSION]".into()),
        ))
        .pack_switch("browser", "--with-browser");
    let harness = HarnessAgent::builder("assistant")
        .model(Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")])))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
        .surface(surface)
        .help_page("getting-started", "Type a task; the harness asks before it edits.")
        .build()?;
    assert!(harness.manifest().render().contains("my-app resume"));
    Ok(())
}
```

`HarnessAgent::manifest()` returns the manifest the block was rendered from.
