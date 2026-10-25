# Agent Surfaces: the Terminal

The terminal is the same home as the web, from the keyboard (spec 3.3 §5.2): a status line,
the roster and what is coming up on the left, the thread on the right, and each decision as a
box answered by key. It is a client of the [engine](index.md) like every other surface — it
shows what the engine projects, in the engine's words, and changes nothing except through
commands.

> **Note:** the terminal is Beta. It is the `tui` feature of `adk-surface` and of `adk-cli`;
> neither is on by default. (`adk-cli`'s default `surface` feature is the web home.)

## Modes

| Mode | Flag | What it is |
|---|---|---|
| Full screen | none (the default on a capable terminal), or `--ui full` | The alternate screen in raw mode: status line, roster, coming up, "While you were away", the thread, decision boxes, the message box and key hints |
| Inline | `--ui inline` (the default unless standard input and output are both a terminal of at least 60 by 16) | The same thread and decision boxes printed into the scrollback, the message box on the last line. With standard input piped, or `TERM=dumb`, plain lines only: each line read is a message, and decisions wait in the inbox |
| JSON | `--output json` | One engine event per line, then an `end` line, and an exit code — for CI |

```bash
cargo install adk-cli --features tui
adk-rust                                          # full screen
adk-rust --ui inline                              # the scrollback
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
REPL; `--voice` keeps the REPL. The REPL's other slash commands (`/hooks`, `/memory`,
`/rules`, `/sandbox`, `/todos`, `/plan`, `/cost`, `/team`, `/skills`, `/web`, `/mcp`,
`/effort`, `/approvals`) and tab completion are not in the terminal; `adk-rust memory`,
`adk-rust rules`, `adk-rust approvals` and `adk-rust skills` cover several from the command
line. `--server`, `--home` and `--token` go after a kernel subcommand (`adk-rust freeze
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

The keys answer only while the message box is empty, and only once the box has had the focus
for a moment, so a keystroke meant for a message, a double tap or a held key never answers a
decision that just appeared or just took the focus (§7.4 rules 1 and 2, S-I4). The keys answer
the oldest open decision of the shown thread; it is the one box drawn whole, the others are
listed under it, and the keys answer only while it is wholly on screen. A call the box cuts
short (more than ten arguments, or long values) is allowed or edited only after `?` shows it
in full. Edit is offered only where the card shows the call's own arguments, never a redacted
view of them. An edit or a reason is sent at the version the decision had when you chose it,
and one whose decision is answered somewhere else first is not sent: the terminal says it was
decided. A message typed while a decision is open is queued and shown above the message box as
"Queued", "Sent after this card is answered".

| Key | Does |
|---|---|
| `Tab` | The next agent's thread |
| `Ctrl-T` | Today: "While you were away" |
| `Ctrl-R` | Permissions: the mode (Just look · Suggest · Ask first · Go ahead), the daily spending limit, quiet time, Stop all |
| `Ctrl-K` | Stop all |
| `/` | Commands: `/mode`, `/stop`, `/resume`, `/session`, `/help`, `/quit` |
| `Ctrl-C` | Leave; a kernel's agents carry on, and its decisions stay in the inbox |

"Always allow" is offered in process, where the source can create the standing rule. Over a
kernel's control plane no route creates one, so a kernel-attached terminal offers Allow once,
Edit and Deny.

## JSON lines and exit codes

`--output json` sends `--message` (or standard input when piped), then writes the session's
engine events until the turn settles:

| Line `type` | What |
|---|---|
| `command` | The outcome of sending the message: `sent` or `queued` |
| `entry`, `decisions`, `reset` | A feed frame of the session, as the engine projects it |
| `decided` | A decision of the session that was answered, as `card` with `card.decided.text` such as "You said yes in Telegram at 07:31": without a message, each one answered before the terminal attached; then each one answered elsewhere while it runs |
| `end` | `outcome`, `exitCode`, `sessionId` and `text` |

| Exit code | `outcome` | Meaning |
|---|---|---|
| 0 | `done` | The turn finished |
| 1 | `error` | The turn failed, ended without a reply, or every agent is stopped |
| 3 | `waitingForYou` | A decision waits for a person; it stays in the inbox for any surface |
| 4 | `budgetReached` | Today's spend reached the daily spending limit, on a kernel whose limit the terminal knows: one on this machine, whose `[kernel.quotas] daily_spend_cap_usd` or personal answer it reads. The control plane does not report the limit, so a kernel on another machine ends such a turn with 1; the harness in process has no daily limit |

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
| The terminal is given back | Full screen and inline restore raw mode, bracketed paste, and for full screen the alternate screen and the cursor, on exit, on a panic in the terminal's own code, and on `SIGINT`, `SIGTERM` and `SIGHUP` (exit 130, 143, 129), registered before the terminal changes. A panic another task catches, such as a tool's, leaves the screen up |
| Pastes are pastes | Full screen and inline turn bracketed paste on, so pasted text goes into the message box and never answers a decision |
| Nothing draws over the screen | While the full screen is up, logs and standard error — what a tool server writes there too — go to `terminal.log` in the config directory's `adk-rust` (Unix) |

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

`TerminalSurface::notice` adds a line the terminal shows when it starts, and
`TerminalSurface::restore_on_signals(false)` leaves `SIGINT`, `SIGTERM` and `SIGHUP` to an
application that goes on after the terminal: the terminal's handlers are tokio's, which last as
long as the process. `TerminalSurface::run_json` writes the JSON lines to any writer, and
`TerminalSurface::screen` returns one frame of the full screen as a terminal receives it, for
tests and documentation.

## Related

- [The Engine](index.md) — feed, projection, commands and what each surface may decide
- [The Control Plane](../kernel/control-plane.md) — the routes a kernel-attached terminal uses
- [Harness Modes and Approvals](../harness/modes-and-approvals.md) — held calls, standing rules, modes
