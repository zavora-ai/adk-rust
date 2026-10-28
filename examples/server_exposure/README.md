# Server Exposure

What an ADK-Rust server exposes, and what its A2A clients accept. Every scenario serves a real
`LlmAgent` through `ServerBuilder`, `create_app_with_a2a`, the `Launcher`, or
`adk_cli::serve::run_serve`, and talks to it over HTTP with `reqwest`, `A2aClient`,
`A2aV1Client`, `RemoteA2aAgent`, and `RemoteA2aV1Agent`.

| Kind | Meaning |
|------|---------|
| `live` | Calls Gemini, OpenAI, or Anthropic |
| `scripted` | Points the real `OpenAIClient` at a local Chat Completions upstream, a real A2A client at a local peer, or the server at a governance store whose database is down, for faults a live API never produces |
| `local` | Exercises the server alone; no model call |

## Scenarios

| Scenario | Kind | Passes when |
|----------|------|-------------|
| `A1 launcher-default-bind` | local | `Launcher::run_serve_directly` does not answer on the host's LAN address |
| `A2 run-serve-default-bind` | local | `adk_cli::serve::run_serve` does not answer on the host's LAN address |
| `B1 loopback-control` | live | A request from loopback runs the agent |
| `B2 rebind-run-refused` | live | `POST /api/run` with `Host` and `Origin` on a rebinding domain is refused and the agent does not run |
| `B3 rebind-session-read-refused` | local | Reading another user's sessions with a rebinding `Host` is refused |
| `B4 cors-preflight-refused` | local | A preflight from an unrelated origin gets no `Access-Control-Allow-Origin` |
| `B5 launcher-app-rebind-refused` | local | `Launcher::build_app` refuses a rebinding `Host` |
| `C1 held-call-input-required` | scripted | A call held for confirmation leaves the pre-v1 task `input-required` |
| `C2 held-call-input-required-stream` | scripted | The pre-v1 stream ends on `input-required` with no stream error |
| `C3 long-running-input-required` | scripted | A long-running call leaves the task `input-required` |
| `C4 held-call-input-required-v1` | scripted | The v1 `SendMessage` task is `InputRequired` |
| `C5 held-call-input-required-live` | live | `gpt-5.6-terra` calls the held tool and the task is `input-required` |
| `D1 thinking-private` | scripted | `reasoning_content` from the model reaches neither the sent nor the streamed A2A response; the answer does |
| `D2 thinking-private-live` | live | No `gemini-3.7-flash` thought summary reaches the A2A response |
| `E1` / `E3 split-char-intact` | scripted | A `ü` split across two reads arrives intact (pre-v1 / v1) |
| `E2` / `E4 truncated-stream-error` | scripted | A stream that stops before a final status ends in an error (pre-v1 / v1) |
| `E5 a2a-stream-live` | live | `gemini-3.5-flash-lite` behind a real A2A server streams multi-byte text to `RemoteA2aAgent` intact and without error |
| `E6 member-freeze-refused` | live | `POST /api/admin/freeze` from an authenticated caller without the `admin` scope answers `403`, and `claude-sonnet-5-5` still answers that caller's next run |
| `E7 admin-freeze-stops-next-run` | live | An administrator's freeze answers `200`, and the agent's next run is refused with `governance.frozen` |
| `E8 member-unfreeze-refused` | live | `POST /api/admin/unfreeze` from the same non-admin caller answers `403`, and the freeze stays in force |
| `E9 admin-unfreeze-restores-run` | live | The administrator's unfreeze answers `200`, and `claude-sonnet-5-5` answers the next run |
| `E10 anonymous-freeze-refused` | local | A freeze without credentials answers `401`, from a server with a `RequestContextExtractor` and from one without |
| `F1 hostile-card-refused` | scripted | A card naming another server as its endpoint is refused and that server receives nothing |
| `F2 hostile-card-refused-v1` | scripted | The same for a v1 card's JSON-RPC interface |
| `G1 provider-text-withheld-run` | live | A `claude-sonnet-5-5` agent with an invalid API key meets a real Anthropic `401`; `POST /api/run` answers `401` with `Authentication failed`, code `model.anthropic.api_error`, category `unauthorized`, and none of Anthropic's text, and the server logs the full error under the response's `x-request-id` |
| `G2 provider-text-withheld-sse` | live | The `/api/run_sse` error event reads `model.unauthorized: Authentication failed`, and the full error is logged under its `x-request-id` |
| `G3 provider-text-withheld-ag-ui` | live | The protocol-native AG-UI `RUN_ERROR` reads `model.unauthorized: Authentication failed` and carries the code |
| `G4 provider-text-exposed` | live | With `expose_error_details`, all three carry Anthropic's message |
| `G5 store-text-withheld-503` | scripted | Over a governance store whose database is down, the freeze and unfreeze `503` `error` reads `session.unavailable: A service the server depends on is unavailable; retry later`; with `expose_error_details` it carries the driver text |
| `G6 request-messages-kept` | local | A missing session, an unknown app, and a frozen kill switch keep the messages the server wrote |
| `G7 happy-path-unaffected` | live | A `claude-sonnet-5-5` turn with the real key answers `200` with text and no error |

A1 and A2 report `SKIP` when the host has no non-loopback address; live scenarios report `SKIP`
without their key. G1–G4 send a deliberately invalid Anthropic key and need no account.

## Running

```bash
cp examples/server_exposure/.env.example examples/server_exposure/.env   # optional, for live scenarios
cargo run --manifest-path examples/server_exposure/Cargo.toml
```

`ADK_ENV_FILE` points at another env file. The run prints one table row per scenario and exits
non-zero when any scenario fails.
