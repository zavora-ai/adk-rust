- **The terminal surface** (`adk-surface` feature `tui`, `adk-cli` feature `surface`; spec
  3.3 §5.2, Beta): `adk-rust` with no subcommand opens a full-screen terminal on a capable
  terminal — status line, roster, coming up, "While you were away", the thread, and each
  decision as a box answered by key (`y` allow once, `a` always allow, `e` edit, `n` deny, `?`
  details). Esc never decides, the keys answer only while the message box is empty and the box
  has been on screen a moment, and a message typed while a decision is open is queued and
  shown as queued. `--ui inline` prints the same thread into the scrollback; `--output json`
  writes one engine event per line and exits 0 (done), 1 (error), 3 (waiting for a decision)
  or 4 (daily spending limit reached). Light and dark come from the §9 tokens, detected from
  the terminal's background unless `--theme` sets them. The terminal attaches to the running
  kernel over its socket, to a kernel on another machine with `--server` and a device token
  (`--pair CODE` claims one and keeps it owner-only), or runs the harness in process. Model,
  tool and file text passes through `adk_surface::tui::terminal_safe`, and the terminal is
  restored on exit, on a panic in its own code and on `SIGINT`, `SIGTERM` and `SIGHUP`, in full
  screen and inline (`TerminalSurface::restore_on_signals` leaves the signals to an embedding
  application). The keys answer only the decision box drawn whole on screen, once it has had
  the focus a moment; a call the box cuts short is allowed only after `?`; Edit is offered only
  over the call's own arguments (`adk_core::ApprovalRequest::shows_call_args`). In process the
  terminal keeps long-term memory, `--yes`, `--trust`, `--continue` and the session's workspace.
