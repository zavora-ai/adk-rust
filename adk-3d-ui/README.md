# adk-3d-ui

`adk-3d-ui` is a minimal 3D UI runtime for ADK-Rust with a simple transport model:

- Server -> client: SSE (`ui_ops`, `toast`, `done`, `error`, `ping`)
- Client -> server: HTTP POST events (`select`, `command`, `approve_action`)

This currently includes:

- Phase 0: protocol and server contract skeleton
- Phase 1: embedded Three.js runtime with incremental `create/patch/remove` op application
- Phase 2: prompt-intent planning with session context (last prompt/command/selection)
- Phase 3 (vertical slice): DevOps-style workbench panel and live status patch loop
- Phase 4: approval-gated action handling with audit trail and execute/reject outcomes

## Run

```bash
cargo run -p adk-3d-ui
```

Open `http://127.0.0.1:8099`.

Optional environment variables:

- `ADK_3D_UI_HOST` (default `127.0.0.1`)
- `ADK_3D_UI_PORT` (default `8099`)

## API

- `POST /api/3d/session` -> create a new session
- `GET /api/3d/stream/{session_id}` -> SSE stream
- `POST /api/3d/event/{session_id}` -> send UI events
- `POST /api/3d/run/{session_id}` -> compile prompt into `ui_ops`

## Notes

- The frontend is currently an embedded static page (`ui/index.html`).
- `planner.rs` and `executor.rs` provide the initial prompt->ops pipeline.
- `policy.rs` adds risk-tier tagging for action proposals.
- `server.rs` applies command/select events back into scene patches and short live status updates.
- `session.rs` stores pending actions and per-session action audit entries.
- Frontend component kinds implemented in v1 runtime:
  - `group`
  - `text3d`
  - `orb`
  - `panel3d`
  - `trail`
  - `command_bar`
