- **The desktop tray** (`adk-surface/desktop/`, Experimental; spec 3.3 §5.5): a Tauri 2 shell
  whose windows load the kernel's web home at `/ui/` and the new tray panel at `/ui/tray`, with
  a tray icon that carries the decisions badge (`badge.count` in its log). The panel shows
  whether your agents are running, spending against the daily limit, decisions — **Allow
  once** on everyday calls, **Deny** wherever the engine offers it, and the home at
  `/ui/decisions/<id>` for approving the rest — what is working now and next, an "Ask Finance"
  box, **Open home** and **Stop all**, never **Resume all**. The shell is an unpublished app in
  its own workspace, reaches a kernel on the same computer only, gives no page IPC, and shows
  "Can't reach your agents" until the kernel answers. `GET /api/surface/agents` gains
  `primary`, the agent a message goes to when the person names none
  (`Source::primary_agent`, `SurfaceEngine::primary_agent`), and `adk-surface` gains the
  `scripted_tray` example.
