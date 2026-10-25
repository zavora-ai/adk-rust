- **A terminal on a kernel records its answers, sets the mode and cleans up while stopped**
  (`adk-kernel`, `adk-surface`, `adk-cli`, spec 3.3 §5.2, §7.2, §11, S-I10): with the kernel's
  socket or operator token, every answer, Stop all, Resume all, mode change and message the
  terminal sends is on the kernel's signed record with the surface and how it was given
  (`POST /api/kernel/surface/records`, `ControlClient::record_surface`,
  `ControlSource::audit_trail`). `/mode` and the Permissions keys set the mode through the
  kernel's `/mode`, also on a thread with no message yet, and the status line shows the mode
  the kernel reports (`GET /api/kernel/sessions/{session}/mode`,
  `CommandHost::session_mode`, `Source::opens_sessions`). While everything is stopped, a
  command that makes the agents do less — `/rules rm`, `/memory forget`, `/sandbox on`, a
  stricter `/mode` — runs (`CommandHost::tightens`). The kernel's "Always allow" routes never
  make a rule of a high-risk call and grant one only after the caller's own "Allow once"; an
  "Always allow" lookup that failed for a moment is asked again.
- **The terminal's keys and boxes** (`adk-surface`, spec 3.3 §5.2, §7.1, §8): `Tab` never
  moves to another thread while a command with arguments is typed; `PgDn` in Help, Today and a
  command's output moves by the rows a page shows; inline, a decision key answers the box
  printed after a command's output instead of becoming a message; a decision that ended without
  an answer is never called answered; the box shows the summary of a decision the kernel asks,
  such as who suggests a follow-up and when it runs. The paired terminal's start-up notice
  reads as one sentence.
