# Session Backends

`LlmAgent` and `Runner` over the session backends: turns on a Redis Cluster, sessions a
standalone Redis server already holds, identifier validation on every backend that runs
locally, and history that survives repeated context compaction. Every scenario drives the real
stack — `LlmAgentBuilder`, `Runner`, a session service, and `OpenAIClient` — and asserts on what
the session service stored and what reached the model, never on model prose.

The deterministic scenarios point the real client at a scripted OpenAI-compatible Chat
Completions server on `127.0.0.1`, which records every request, so a scenario can read exactly
what the agent and the compaction summarizer sent. `cluster_multi_turn_live` runs against
OpenAI when `OPENAI_API_KEY` is set.

## Scenarios

| Scenario | Backend | Passes when |
|----------|---------|-------------|
| `cluster_lifecycle` | Redis Cluster | Create, two runner turns (one with a tool call writing app, user, and session state), both append paths, `list`, `delete`, and `delete_all_sessions` succeed for identifiers containing `:`, `{`, and `}`, and every state tier reads back |
| `cluster_multi_turn_live` | Redis Cluster, OpenAI | Three live turns: the third recalls a fact from the first and a tool result from the second, and all three turns are stored |
| `standalone_existing_data` | Standalone Redis | A session seeded earlier (by the build named in `SESSION_SEED_LABEL`) reads back with its state tiers and events, is listed, and accepts a new turn; with `EXPECT_SEED_FROM` set, the session must have been seeded by that build before the run |
| `invalid_identifiers` | In-memory, encrypted, SQLite, PostgreSQL, Redis, Redis Cluster | Empty, oversized, and NUL-containing identifiers are rejected as `InvalidInput` by `create`, `delete`, `append_event`, `delete_all_sessions`, and `list` |
| `compaction_keeps_history` | Standalone Redis (in-memory without `REDIS_URL`) | With compaction after every turn and a 4-event history window, the second summary's input holds the first summary's facts and the turn-1 tool result, and the turn-4 model request carries both although turn 1 is outside the window |

## Running

Start the servers the scenarios use (any Redis 7 or later; `redis-cli --cluster create` needs at
least three nodes):

```bash
redis-server --port 6380 --daemonize yes --save ""
for port in 7001 7002 7003; do
  mkdir -p /tmp/adk-cluster/$port
  redis-server --port $port --cluster-enabled yes --dir /tmp/adk-cluster/$port \
    --daemonize yes --save ""
done
redis-cli --cluster create 127.0.0.1:7001 127.0.0.1:7002 127.0.0.1:7003 \
  --cluster-replicas 0 --cluster-yes
```

Then run every scenario, or a comma-separated selection:

```bash
REDIS_URL=redis://127.0.0.1:6380 \
REDIS_CLUSTER_NODES=127.0.0.1:7001,127.0.0.1:7002,127.0.0.1:7003 \
POSTGRES_URL=postgres://postgres@127.0.0.1:5432/adk_sessions \
  cargo run --manifest-path examples/session_backends/Cargo.toml
cargo run --manifest-path examples/session_backends/Cargo.toml -- --scenario invalid_identifiers
```

A scenario whose server is not configured prints `SKIP`. Each scenario prints `PASS`, `FAIL`, or
`SKIP` with its evidence; the process exits non-zero on any `FAIL`. `OPENAI_API_KEY` and the
server variables are read from the environment, then from the dotenv file named by
`ADK_ENV_FILE` (default `./.env`), which overrides them; `.env.example` lists them.

To check that standalone sessions written by an earlier build stay readable, run
`standalone_existing_data` once with that build and `SESSION_SEED_LABEL=<name>`, then with the
current build and `EXPECT_SEED_FROM=<name>`.
