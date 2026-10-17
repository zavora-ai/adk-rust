# Session Integrity

`Runner` and the `adk-server` REST and A2A endpoints over every session backend that runs
locally: in-memory, encrypted (over in-memory), SQLite, PostgreSQL, and Redis. Each scenario runs
once per backend and asserts on what the session service stored and what the endpoints
answered, never on model prose.

The deterministic scenarios point the real `OpenAIClient` at a scripted OpenAI-compatible Chat
Completions server on `127.0.0.1`. Faults a backend cannot produce on demand come from a
`SessionService` wrapper around the real backend: a `get` that fails once with a transient
`Unavailable` error, and `create` calls held until two first requests both reach it.
`live_recovery` runs against OpenAI when `OPENAI_API_KEY` is set.

## Scenarios

| Scenario | Passes when |
|----------|-------------|
| `rewind_scoped_to_user` | Alice and Bob each own a session with the same ID; `rewind_steps` on Bob's identity truncates only Bob's session, and then on Alice's only Alice's. Backends without rewind print `SKIP` |
| `rewind_keeps_initial_state` | A session created with `tier = gold` keeps it after rewinding its last turn and after rewinding every event, both in the returned session and on re-read; state a tool wrote is dropped with its turn |
| `duplicate_create_conflicts` | A second `create` for an existing identity fails with `session.already_exists` (`Conflict`), the events, state, and app tier are unchanged, and `POST /api/apps/{app}/users/{user}/sessions/{existing}` returns 409 |
| `transient_get_keeps_session` | With one failing `get`, `POST /api/run` and `POST /api/run_sse` return 503 and A2A `message/send` returns a JSON-RPC error; the session keeps its 3 events and `plan = gold`, and the next request continues it |
| `concurrent_first_requests` | Two `POST /api/run` for a new session, whose creates race, both return 200 and both user turns are stored |
| `unknown_session_not_found` | Both append paths report a missing session as `session.not_found`; `POST /api/run/{app}/{user}/{missing}` and `GET /api/apps/{app}/users/{user}/sessions/{missing}` return 404 |
| `same_millisecond_order` | 40 call/response pairs 300 µs apart inside one millisecond read back call first, and 12 events with one timestamp read back in append order |
| `nul_round_trip` | A turn whose tool returns text containing U+0000 and writes a state value containing it completes, and both read back unchanged, as does a message containing U+0000 |
| `list_order_and_pages` | 12 sessions updated in an order unlike their creation order list newest first, pages of 5 tile the list, and `GET /api/apps/{app}/users/{user}/sessions` returns the same order |
| `live_recovery` | On the in-memory backend with live OpenAI: turn 1 states a fact, a request whose session read fails returns 503, and turn 3 answers from turn 1 |

## Running

Start the servers the PostgreSQL and Redis backends use (any Redis 7 or later, any PostgreSQL 13
or later):

```bash
redis-server --port 6380 --daemonize yes --save ""
createdb sessions
```

Then run every scenario on every backend, or a comma-separated selection of either:

```bash
POSTGRES_URL=postgres://postgres@127.0.0.1:5432/sessions \
REDIS_URL=redis://127.0.0.1:6380 \
  cargo run --manifest-path examples/session_integrity/Cargo.toml
cargo run --manifest-path examples/session_integrity/Cargo.toml -- \
  --scenario rewind_scoped_to_user,transient_get_keeps_session --backend memory,sqlite
```

A backend whose URL is not set prints `SKIP`. Each run prints `PASS`, `FAIL`, or `SKIP` with its
evidence, and the process exits non-zero on any `FAIL`. `OPENAI_API_KEY` and the server variables
are read from the environment, then from the dotenv file named by `ADK_ENV_FILE` (default
`./.env`), which overrides them; `.env.example` lists them.
