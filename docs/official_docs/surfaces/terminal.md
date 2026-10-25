# Agent Surfaces: the Terminal

The terminal is the same home as the web, from the keyboard (spec 3.3 §5.2): a status line,
the roster and what is coming up on the left, the thread on the right, and each decision as a
box answered by key. It is a client of the [engine](index.md) like every other surface — it
shows what the engine projects, in the engine's words, and changes nothing except through
commands.

> **Note:** the terminal is Beta. It is the `tui` feature of `adk-surface` and of `adk-cli`.
> `adk-cli` builds it by default, so a plain `adk-rust` opens the full screen; `adk-surface`
> leaves it off by default.

## Modes

| Mode | Flag | What it is |
|---|---|---|
| Full screen | none (the default on a capable terminal), or `--ui full` | The alternate screen in raw mode: status line, roster, coming up, "While you were away", the thread, decision boxes, the message box and key hints |
| Inline | `--ui inline` (the default unless standard input and output are both a terminal of at least 60 by 16) | The same thread and decision boxes printed into the scrollback, the message box on the last line. With standard input piped, or `TERM=dumb`, plain lines only: each line read is a message, sent once the turn before it has finished, and decisions wait in the inbox |
| JSON | `--output json` | One engine event per line, then an `end` line, and an exit code — for CI |
| REPL | `--ui repl`, or `--voice` | The line-by-line harness REPL the full screen replaced as the default |

```bash
cargo install adk-cli
adk-rust                                          # full screen
adk-rust --ui inline                              # the scrollback
adk-rust --ui repl                                # the line-by-line REPL
adk-rust --output json --message "Summarise the open issues"
adk-rust --theme light                            # auto (the default), light or dark
```

`--theme auto` asks the terminal for its background (OSC 11), then reads `COLORFGBG`, and
falls back to dark. Colours are the §9 tokens in 24-bit colour where the terminal advertises
it (`COLORTERM=truecolor`), else the nearest of the 256 indexed colours.

## Where the agents run

| Reach | How | May approve high risk and resume |
|---|---|---|
| The running kernel, on its machine | `adk-rust` finds the kernel at `--home` (default: the config directory's `adk-rust`) and attaches over its socket | Yes: a paired local terminal |
| A kernel on another machine | `--server URL`, with a device token (`--pair CODE` once, then kept), `--token`, or `$ADK_SERVE_TOKEN` | No: high-risk decisions wait for the web home or a local terminal |
| No kernel | The harness runs in this process (also with `--ephemeral` or `--continue`) | Yes |

In process, the terminal keeps what the REPL keeps: long-term memory, `--yes` (held calls are
allowed once, an unsandboxed retry only with `--allow-unsandboxed`), `--trust` (the
workspace's instruction files and hooks load; without it an untrusted workspace says so), and
the session's workspace — `--continue` opens the latest session of the workspace, a new
session records the workspace, and a session that works in another workspace is refused.

With the `tui` feature, `adk-rust` without a subcommand is the terminal in place of the
REPL; `--ui repl` and `--voice` keep the REPL. `--server`, `--home` and `--token` go after a kernel subcommand (`adk-rust freeze
--server URL`); before one they are refused.

To pair a terminal on another machine, run `adk-rust pair` on the kernel's machine, then
start the terminal with the code it shows:

```bash
# On the kernel's machine
adk-rust pair --name "Laptop"

# On the laptop
adk-rust --server http://kernel.lan:8080 --pair ABCD-2345
```

The claim returns a device token once. The terminal keeps it in the config directory under
`adk-rust/devices/`, readable by its owner only, and sends it on every later start against
that server. The token reaches only the routes its scopes open (read, decide, stop); resuming
after a stop is never one of them (S-I10). `adk-rust pair revoke` ends it.

## Deciding by key

| Key | Answer |
|---|---|
| `y` | Allow once |
| `a` | Always allow (the scope the decision names), where the surface may offer it |
| `e` | Edit the call's arguments as JSON, then allow; policy checks the edited call again |
| `n` | Deny, with an optional reason |
| `?` | Details: the whole call and how it got here |
| `Esc` | Never decides: closes the details or a view, and switches between answering by key and typing |

The keys answer the oldest open decision of the shown thread. It is the one box drawn; the
others are listed under it, one line each.

| Rule | How the terminal keeps it (§7.1, §7.4 rules 1 and 2, S-I4) |
|---|---|
| The box fits the screen | The box, its keys included, fits the thread pane. A call too long for it shows a window at a time, with a line saying how much more there is; `PgDn` and `PgUp` move the window, and the keys stay on screen |
| The exact call is read before it is allowed | `y`, `a` and `e` answer only once every row of the call has been on screen; a call the box cuts short (more than ten arguments, or long values) only once `?` shows it in full. `n` and `?` always work |
| A key meant for a message never answers | The keys answer only while the message box is empty, once the box has been on screen for a moment, and not within a moment of the last keystroke into the message box (also after Ctrl-C, Ctrl-U, Delete or Esc empties it). A box that takes the focus, changes, or comes back into view — scrolled back, or after Today, Permissions or Help — arms again |
| A key meant for the box never becomes a message | While the message box is empty and you are not typing, `y`, `a`, `e`, `n` and `?` are the box's keys: before the box is lit, or while it is off screen, they do nothing and the terminal says why. Just after a box closes because it was answered somewhere else, they do nothing either and the terminal says where it was answered ("Already answered in Telegram."), and a reason typed after `n` is not sent |
| Esc never decides | Esc, also arriving with the next key in one read (tmux, screen, a slow link), switches to typing; that key is typed |
| A paste is never an answer | Bracketed paste puts a paste in the message box. On a console without it (Windows), a decision key with more keys right behind it is typing |

Edit is offered only where the card shows the call's own arguments, never a redacted view of
them. An edit or a reason is sent at the version the decision had when you chose it, and one
whose decision is answered somewhere else first is not sent: the terminal says it was decided.
`Tab` does nothing while an edit or a reason is open, so neither goes to another agent's
thread. A message typed while a decision is open is queued and shown above the message box as
"Queued", "Sent after this card is answered"; `↑` in the empty message box takes the newest one
back before it is sent.

A decision no longer open shows once in the thread, at the place it was asked, with how it was
answered: "✓ Decided · Write file invoice.txt · You said yes in the terminal at 06:45", also
when it was answered in the home or on the phone. Inline prints it once. Other notices pass: one
that says what happened goes after a few seconds and with the next message sent; a problem, such
as a refused key, stays until the next key.

| Key | Does |
|---|---|
| `Tab` | The next agent's thread |
| `Ctrl-T` | Today: "While you were away" |
| `Ctrl-R` | Permissions: the mode (Just look · Suggest · Ask first · Go ahead), the daily spending limit, quiet time, Stop all. Its keys (`1`–`4`, and `r` for Resume all after a stop) act when pressed twice, so a message typed there changes nothing |
| `Ctrl-G` | Your agents: add, change, pause and remove the agents you add, over `/agent`; see [Your Agents](agents.md#in-the-terminal) |
| `Ctrl-K` | Stop all |
| `PgUp` `PgDn` | Scroll the thread, or the call in a decision box |
| `↑` | Take back the newest queued message, before it is sent |
| `/` | Commands: the terminal's own and the REPL's, completed as you type; see [Commands](#commands) |
| `Ctrl-C` | Leave; a kernel's agents carry on, and its decisions stay in the inbox. A command still on its way, such as a Stop all, is waited for (up to 5 seconds), and the terminal says what it did not send |

`/session ID` and `/session new` bind the session to the workspace as the start does: in process,
a session that works in another workspace is refused, and a new one records the workspace.

"Always allow" is offered wherever the engine offers it: where the harness can make a standing
rule of the call, in process and on the kernel's socket or with its operator token alike. On a
kernel the rule is made while the decision waits and granted once the yes is recorded, so an
answer that loses to one given elsewhere grants nothing. A paired device's token reaches no
route that makes a rule, so a remote terminal paired by code offers Allow once, Edit and Deny,
and `a` says the home app offers "Always allow" there.

## Commands

`/` in the message box starts a command. The commands it may be are listed above the message
box, the terminal's own first; `↑` and `↓` choose one, `Tab` completes the name (or the start
the names share), Enter runs the one typed or chosen, and Esc clears it. While a command is
typed, `Tab` never moves to another agent's thread.

| Command | Does |
|---|---|
| `/help` | Keys and every command, a page at a time (`PgDn`) |
| `/today`, `/permissions` | Today and Permissions, as `Ctrl-T` and `Ctrl-R` |
| `/mode` | The session's mode; `/mode ask first` or `/mode ask` (also `read-only`, `plan`, `auto`) sets it through the engine, as Permissions does |
| `/session` | The session: id, user, workspace, mode; `/session new` or `/session ID` opens another |
| `/stop`, `/resume` | Stop all, Resume all |
| `/quit`, `/exit` | Leave |
| `/todos`, `/plan`, `/approvals`, `/cost`, `/team` | The REPL's: the shown thread's todo list, its last approved plan, its calls waiting for a decision; your spend; the team |
| `/rules`, `/web` | Your standing rules, `/rules rm N` revokes one; the web allowlist, `/web allow HOST` remembers one host |
| `/memory` | Working notes and long-term facts; `/memory forget ID`, `/memory accept ID` |
| `/hooks`, `/sandbox`, `/effort`, `/mcp`, `/skills` | Hooks and whether each runs; the shell; the reasoning effort; MCP servers and `/mcp resources SERVER`; the skills the agent can load |

The REPL's commands run the REPL's own code, in the thread the terminal shows, and their output
is a pane of its own — Esc goes back to the thread. While it shows, a decision key answers
nothing; the box arms again once the thread is back. Switching the shell (`/sandbox on`) or the
effort (`/effort high`) takes a restart here, as the commands say.

| Where the agents run | Where the REPL's commands run |
|---|---|
| In process | Over the harness in this process |
| The kernel, on its socket or with the operator's token | On the kernel, over the harness `adk-rust up` runs (`GET /api/kernel/commands`, `POST /api/kernel/commands/run`); one that changes something waits while everything is stopped, and each run is on the kernel's record. `/session` says how the terminal reached the kernel |
| A kernel on another machine, paired by code | None: they are the kernel operator's, and the terminal says so when it starts. `adk-rust rules`, `adk-rust memory` and `adk-rust approvals` on the kernel's machine cover several |

## JSON lines and exit codes

`--output json` sends `--message` (or standard input when piped), then writes the session's
engine events until the turn settles. Without `--session`, the message starts a session of its
own, never one where a decision this run did not cause is waiting:

| Line `type` | What |
|---|---|
| `command` | The outcome of sending the message: `sent` or `queued` |
| `entry`, `decisions`, `reset` | A feed frame of the session, as the engine projects it |
| `decided` | A decision of the session that was answered, as `card` with `card.decided.text` such as "You said yes in Telegram at 07:31": without a message, each one answered before the terminal attached; then each one answered elsewhere while it runs |
| `end` | `outcome`, `exitCode`, `sessionId` and `text` |

| Exit code | `outcome` | Meaning |
|---|---|---|
| 0 | `done` | The turn finished |
| 1 | `error` | The turn failed, ended without a reply (also after a tool call), or every agent is stopped |
| 3 | `waitingForYou` | A decision waits for a person; it stays in the inbox for any surface. A message queued behind it is not sent, and the `end` line says so |
| 4 | `budgetReached` | Today's spend reached the daily spending limit in force, as the kernel reports it (`GET /kernel/settings`: the default area's, or the only area's); the harness in process has no daily limit |

```bash
adk-rust --output json --message "Write the release notes" > events.jsonl
case $? in
  0) echo "done" ;;
  3) echo "waiting for a decision in the inbox" ;;
  4) echo "daily spending limit reached" ;;
  *) echo "failed" ;;
esac
```

## Safety

| Rule | How the terminal keeps it |
|---|---|
| Model, tool and file text is inert (S-I9) | Every such string passes through `terminal_safe`: control characters, the `ESC` of ANSI and OSC sequences, C1 controls, bidi and zero-width characters are written as `\u{..}` escapes. JSON lines escape `DEL`, C1 and bidi characters as `\uXXXX` |
| The terminal is given back | Full screen and inline restore raw mode, bracketed paste, and for full screen the alternate screen and the cursor, on exit, on a panic in the terminal's own code, and on `SIGINT`, `SIGTERM` and `SIGHUP` (exit 130, 143, 129), registered before the terminal changes — before `--theme auto` asks for the background too. A panic another task catches, such as a tool's, leaves the screen up |
| Pastes are pastes | Full screen and inline turn bracketed paste on, so pasted text goes into the message box and never answers a decision |
| Nothing draws over the screen | While the full screen or the inline UI is up on a terminal, logs and standard error — what a tool server writes there too — go to `terminal.log` in the config directory's `adk-rust` (Unix) |
| Any terminal that draws | The full screen never asks the terminal where its cursor is, so a terminal that does not answer that query (a hidden panel, some IDE consoles) still gets it |

## Embedding

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["tui"] }
```

```rust,no_run
use std::sync::Arc;

use adk_surface::tui::{TerminalSurface, ThemeChoice, UiMode};
use adk_surface::{Caller, LocalSource, Origin, SurfaceEngine, SurfaceKind};

async fn run(
    sessions: Arc<dyn adk_session::SessionService>,
    approvals: Arc<dyn adk_core::ApprovalService>,
) -> std::io::Result<i32> {
    let source = LocalSource::builder("my-app")
        .sessions(sessions)
        .approvals(approvals)
        .build()
        .map_err(std::io::Error::other)?;
    let engine = SurfaceEngine::builder(Arc::new(source)).build();
    let me = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
    TerminalSurface::new(engine, me).theme(ThemeChoice::Auto).run(UiMode::FullScreen).await
}
```

`TerminalSurface::command` adds a slash command: a `TerminalCommand` gets its arguments and a
`CommandContext` naming the shown session, and what it returns is shown escaped in the output
pane; the terminal's own commands win over one of the same name, except that an added `mode`
or `session` answers the bare `/mode` and `/session`.
`TerminalSurface::notice` adds a line the terminal shows when it starts, until the first key;
`TerminalSurface::open_sessions_with` sets how `/session` opens a session, for an application
whose sessions are bound to something, such as a workspace; and
`TerminalSurface::restore_on_signals(false)` leaves `SIGINT`, `SIGTERM` and `SIGHUP` to an
application that goes on after the terminal: the terminal's handlers are tokio's, which last as
long as the process. `TerminalSurface::run_json` writes the JSON lines to any writer, and
`TerminalSurface::screen` returns one frame of the full screen as a terminal receives it, for
tests and documentation.

## Related

- [The Engine](index.md) — feed, projection, commands and what each surface may decide
- [The Control Plane](../kernel/control-plane.md) — the routes a kernel-attached terminal uses
- [Harness Modes and Approvals](../harness/modes-and-approvals.md) — held calls, standing rules, modes
