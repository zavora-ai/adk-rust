# Portable team architectures with OpenAI

These five runnable binaries bind a serializable `TeamSpec` to ordinary ADK-Rust
agents. Each program prints its portable specification before compiling it to an
executable `CompiledTeam` and running it through `Runner`.

All examples use `OpenAIClient` and read `OPENAI_API_KEY` from the environment.
Set `TEAM_MODEL` to override the default `gpt-5-mini` model.

## Architectures

| Binary | Topology | Expected control flow |
|---|---|---|
| `team-supervisor-handoff` | Supervisor → billing or technical | The Runner changes the active agent. The supervisor does not resume, and each specialist has an empty handoff allowlist. |
| `team-supervisor-delegation` | Supervisor → researcher tool | The researcher completes a nested invocation and returns a result. The supervisor remains active and writes the final answer. |
| `team-parallel-swarm` | Facts + risks + reviewer in parallel | Researchers publish to invocation-scoped `SharedState`; the reviewer waits for both notes and reconciles them. |
| `team-hybrid` | Supervisor → sequential draft workflow → publisher | The workflow is delegated and returns a draft; the supervisor then hands control to the publisher. |
| `team-runtime-ui` | Supervisor → billing or technical, served by `adk-server` | The server's REST and SSE API describes the compiled topology, creates sessions, streams Runner events, and exposes team state and execution history. |

`Delegate` and `Handoff` are intentionally different relationships. A delegate
behaves like a function call. A handoff changes which agent owns the rest of the
turn. The target lists in each `TeamSpec` are exact; compilation does not widen an
edge to other peers.

The delegation binary also demonstrates an exact per-edge input schema,
last-eight-message history projection, transactional state/artifact write
policy, timeout, bounded retry, and ordered team lifecycle hooks. All examples
apply aggregate event, model/tool call, transfer/delegation, and wall-time
budgets through `TeamPolicy`. Standard Runner plugins and member callbacks still
apply because the compiled team is an ordinary executable `Agent` root.

## Run

```bash
cp examples/team_architectures/.env.example .env
# Edit .env and set OPENAI_API_KEY.

cargo run --manifest-path examples/team_architectures/Cargo.toml \
  --bin team-supervisor-handoff
cargo run --manifest-path examples/team_architectures/Cargo.toml \
  --bin team-supervisor-delegation
cargo run --manifest-path examples/team_architectures/Cargo.toml \
  --bin team-parallel-swarm
cargo run --manifest-path examples/team_architectures/Cargo.toml \
  --bin team-hybrid
cargo run --manifest-path examples/team_architectures/Cargo.toml \
  --bin team-runtime-ui
```

`team-runtime-ui` serves the team's REST and SSE API on `http://127.0.0.1:8088/api/`. Set
`ADK_UI_ADDRESS` to bind a different local address.

> **Note:** `adk-server` no longer bundles a runtime UI (spec 3.3 §10 S1). The web home of
> `adk-surface` replaces it, and these binaries serve the REST, SSE and A2A APIs, so `/ui/`
> answers `404` on them. The screenshots show the bundled UI as it was. See
> [The Web Home](../../docs/official_docs/surfaces/web-home.md).


`cargo check` compiles every binary without making a network request. OpenAI is
contacted only when a binary is run.
