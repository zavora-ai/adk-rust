- **The terminal's decision box fits the screen and its keys never answer or type by accident**
  (`adk-surface`, `adk-cli`, spec 3.3 §5.2, §7.1, §7.4): the box keeps its keys on screen and
  shows a long call a window at a time, allowed only once read; a decision key never becomes a
  message while the box is not lit, off screen or just closed elsewhere; Esc arriving with the
  next key, emptying the message box, a view closing and a paste on Windows never answer; the
  Permissions keys act when pressed twice; `Tab` never carries an edit to another thread; `↑`
  takes back a queued message (`SurfaceEngine::unqueue_last`). A decided call shows once in the
  thread at its place; notices pass; the Decisions badge shows only above zero. The full screen
  starts without the cursor-position query, signals are registered before the background query,
  and inline sends standard error to `terminal.log`. Piped lines run one turn at a time, JSON
  messages start a session of their own, a turn that fails after a tool call exits 1, exit 4
  uses the limit the kernel reports, and leaving waits for a Stop all on its way and says what
  it did not send. `/session` binds sessions to the workspace in process.
