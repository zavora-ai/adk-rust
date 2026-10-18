# Server Exposure

What an ADK-Rust server exposes, and what its A2A clients accept. Every scenario serves a real
`LlmAgent` through `ServerBuilder`, `create_app_with_a2a`, the `Launcher`, or
`adk_cli::serve::run_serve`, and talks to it over HTTP with `reqwest`, `A2aClient`,
`A2aV1Client`, `RemoteA2aAgent`, and `RemoteA2aV1Agent`.

| Kind | Meaning |
|------|---------|
| `live` | Calls Gemini or OpenAI |
| `scripted` | Points the real `OpenAIClient` at a local Chat Completions upstream, or a real A2A client at a local peer, for faults a live API never produces |
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
| `F1 hostile-card-refused` | scripted | A card naming another server as its endpoint is refused and that server receives nothing |
| `F2 hostile-card-refused-v1` | scripted | The same for a v1 card's JSON-RPC interface |

A1 and A2 report `SKIP` when the host has no non-loopback address; live scenarios report `SKIP`
without their key.

## Running

```bash
cp examples/server_exposure/.env.example examples/server_exposure/.env   # optional, for live scenarios
cargo run --manifest-path examples/server_exposure/Cargo.toml
```

`ADK_ENV_FILE` points at another env file. The run prints one table row per scenario and exits
non-zero when any scenario fails.
