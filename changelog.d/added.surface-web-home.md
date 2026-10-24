- **The web home** (`adk-surface`, feature `web`; spec 3.3 §5.1, §7, §8, §9): Threads (the
  roster with status words, the selected agent's thread with decisions inline on their own
  background, Decisions with a filled count badge, Coming up), Today ("While you were away":
  spent, quiet time, what waits, one line per event with a status pill and its actions,
  routines today) and Memory & rules ("Agent Memory and Rules": what your agents remember, can
  do and did), in light and dark from the §9 tokens with a theme switch, a one-column phone
  layout under 640 px, keyboard access and visible focus. `adk_surface::web::home()` serves the
  embedded React app at `/ui/` under a strict content security policy for
  `ServerBuilder::add_root_routes`. Every answer goes through the engine's `decide` with the
  version the card showed, a conflict renders as "already decided", a card changed since it
  was shown is shown again, Enter and Esc never decide, and typing while a decision is open is
  queued and read back from `GET /api/surface/sessions/{session}/queued`. A thread follows one
  conversation, and a decision opens in its own session's thread. A server started with a
  token asks for it once. The client reads the engine feed and never the `adk_ui` profile
  (§10 S3). **Take over, then Done**: `adk_surface::web::take_over_router(engine, take_over)`
  serves `POST /api/surface/decisions/{id}/take-over`, which hands what the person typed to
  the harness's `TakeOver` for the one call that asked; a take-over card is headed by what to
  type and offers only **Deny** as a decide. The engine offers **Edit** only where a card
  shows the call's own arguments, never over a redacted view. Where you can say yes from lists, removes and pairs
  devices and chat accounts through the kernel's pairing routes. Controls whose kernel routes
  are not built yet (quiet time, the spending limit and the mode; memory and the record check)
  render read-only through typed seams. A developer panel at `/ui/developer` keeps the
  topology, protocols, telemetry and state views of the former runtime UI. The
  `scripted_home` example serves the home over a scripted kernel for the Playwright
  end-to-end test.
- **`adk-rust serve` and `adk-rust up` serve the web home** (`adk-cli`, feature `surface`, on
  by default): at `http://127.0.0.1:8080/ui/`, over the harness's sessions, approval inbox,
  modes and standing rules; `up` adds the kernel and records every command and how each
  answer was given on its signed audit log. A message typed in the home runs as the operator,
  with their mode and standing rules, and its **Stop all** freezes the server's runs, the
  background runs and the home's turns alike. `HarnessLauncher::with_auth_token` and
  `HarnessLauncher::with_governance` set the token and the shared kill switch. The umbrella
  `surface` feature turns on `adk-cli`'s.
