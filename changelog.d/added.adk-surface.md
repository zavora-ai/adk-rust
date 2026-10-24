- **`adk-surface`: one engine under every agent surface** (spec 3.3, Beta): a new opt-in
  crate (`default = []`; umbrella feature `surface`, in no tier). `SurfaceEngine` reads a
  `LocalSource` (an in-process kernel, harness or runner with its session store and approval
  service) or a `ControlSource` (a kernel's control plane through `ControlClient`, with the
  approvals inbox read in full over the same HTTP or Unix-socket endpoint, one session at a
  time, with the client's bearer token and a 30-second bound on every call) and gives
  every surface the same feed (session events numbered by store position, a snapshot then a
  live tail, resumable after the last `seq` without a gap or a duplicate), the same projection
  (decision cards with every part of §7.1, thread items, agent rows, the status line and the
  "While you were away" briefing, in the plain words of §8 from `adk_surface::words`) and the
  same commands (decide at the version seen, where a conflict becomes "Decided" and is never
  retried; send; queue; stop all; resume all from the web home or a paired local terminal
  only; set the mode), each with the caller's subject and input origin on a `surface.*` audit
  record. What each surface may decide (§7.2) and the rules of §7.4 are enforced in
  `adk_surface::rules`. The `web` feature serves the engine under `/api/surface` for
  `ServerBuilder::add_api_routes`, with server-sent events resumable by `Last-Event-ID`, the
  session-owner check on every route, an OpenAPI 3.1 document generated from the Rust types,
  and TypeScript types generated from it (`scripts/check-surface-api.sh` fails CI on drift).
