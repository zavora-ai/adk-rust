- **No host file tool runs beside an unapproved sandboxed command** (`adk-harness`, spec 3.1
  §9.2 I26): while `auto_allow_sandboxed` is in effect, `read_file`, `glob`, `grep`,
  `write_file` and `edit_file` wait for a running sandboxed or container command, and a
  command waits for running file tools, across every session and team member of the harness.
  A command could otherwise rearrange the workspace while a host file tool resolved a path in
  it.
- **A container command leaves no process behind** (`adk-harness`, spec 3.1 §9.2 I23): when a
  command in a session's container exits, every process it started is killed, though the
  container stays up for the session's next command.
