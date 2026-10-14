# SQLx 0.8 upgrade fixtures

Databases written by the adk-* 2.3.0 crates from crates.io, the last release on SQLx
0.8. The `upgrade_from_sqlx_0_8` tests in `adk-session`, `adk-memory`, and `adk-graph`
load them and open them with the current crates.

| File | Crate | Written by |
|------|-------|------------|
| `adk-session/tests/fixtures/sqlx-0.8/session_sqlite.sql` | `adk-session` | `SqliteSessionService` |
| `adk-session/tests/fixtures/sqlx-0.8/session_postgres.sql` | `adk-session` | `PostgresSessionService` |
| `adk-memory/tests/fixtures/sqlx-0.8/memory_sqlite.sql` | `adk-memory` | `SqliteMemoryService` |
| `adk-memory/tests/fixtures/sqlx-0.8/memory_postgres.sql` | `adk-memory` | `PostgresMemoryService`, no embedding provider |
| `adk-graph/tests/fixtures/sqlx-0.8/checkpoints_sqlite.sql` | `adk-graph` | `SqliteCheckpointer` |

The fixtures are SQL text rather than database files so a change to them is reviewable.
adk-graph 2.3.0 had no PostgreSQL checkpointer, so there is no PostgreSQL graph fixture.

## Regenerating

1. Create the generator below as a standalone project outside the repository. It
   depends on the published 2.3.0 crates only.
2. Start a PostgreSQL 17 server with the pgvector extension and create an empty
   database:

   ```bash
   docker run -d --name adk-fixtures -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
   psql postgres://postgres:postgres@localhost:5432/postgres -c 'CREATE DATABASE adk_fixture_2x'
   ```

3. Run the generator, then dump each database as SQL text. `pg_dump` 17.6 and later
   write `\restrict` and `\unrestrict` lines, which are `psql` commands rather than
   SQL, so they are removed:

   ```bash
   mkdir out
   cargo run -- out postgres://postgres:postgres@localhost:5432/adk_fixture_2x

   repo=/path/to/adk-rust
   sqlite3 out/session.db .dump > "$repo/adk-session/tests/fixtures/sqlx-0.8/session_sqlite.sql"
   sqlite3 out/memory.db .dump > "$repo/adk-memory/tests/fixtures/sqlx-0.8/memory_sqlite.sql"
   sqlite3 out/graph.db .dump > "$repo/adk-graph/tests/fixtures/sqlx-0.8/checkpoints_sqlite.sql"

   dump() {
     PGTZ=UTC pg_dump --no-owner --no-privileges --inserts "$@" \
       postgres://postgres:postgres@localhost:5432/adk_fixture_2x \
       | grep -v '^\\\(un\)\{0,1\}restrict '
   }
   dump -t public._adk_session_migrations -t public.sessions -t public.events \
     -t public.app_states -t public.user_states \
     > "$repo/adk-session/tests/fixtures/sqlx-0.8/session_postgres.sql"
   dump -t public._adk_memory_migrations -t public.memory_entries \
     > "$repo/adk-memory/tests/fixtures/sqlx-0.8/memory_postgres.sql"
   ```

The table-only memory dump leaves out `CREATE EXTENSION vector`; the test creates the
extension before it loads the dump.

### Generator

`Cargo.toml`:

```toml
[package]
name = "adk-2x-fixtures"
version = "0.0.0"
edition = "2024"
publish = false

[workspace]

[dependencies]
adk-core = "=2.3.0"
adk-session = { version = "=2.3.0", features = ["sqlite", "postgres"] }
adk-memory = { version = "=2.3.0", features = ["sqlite-memory", "database-memory"] }
adk-graph = { version = "=2.3.0", features = ["sqlite"] }
chrono = "0.4"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

`src/main.rs`:

```rust
//! Writes the adk-* 2.3.0 (SQLx 0.8) databases the upgrade tests load.
//!
//! Usage: adk-2x-fixtures <output dir> [<postgres url>]

use std::collections::HashMap;
use std::path::Path;

use adk_core::{AdkIdentity, AppName, Content, Event, SessionId, UserId};
use adk_graph::{Checkpoint, Checkpointer, SqliteCheckpointer};
use adk_memory::{MemoryEntry, MemoryService, SqliteMemoryService};
use adk_session::{AppendEventRequest, CreateRequest, SessionService};
use serde_json::json;

const APP: &str = "upgrade_app";
const USER: &str = "upgrade_user";
const SESSION: &str = "upgrade_session";

async fn write_session(service: &dyn SessionService) {
    let state = HashMap::from([
        ("app:plan".to_string(), json!("team")),
        ("user:name".to_string(), json!("O'Brien")),
        ("topic".to_string(), json!("tides")),
    ]);
    service
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state,
        })
        .await
        .expect("create session");
    let identity = AdkIdentity::new(
        AppName::new(APP).unwrap(),
        UserId::new(USER).unwrap(),
        SessionId::new(SESSION).unwrap(),
    );
    for (index, (author, text)) in
        [("user", "When is high tide?"), ("assistant", "High tide is at 06:12.")]
            .into_iter()
            .enumerate()
    {
        let mut event = Event::with_id(format!("event-{index}"), "invocation-1");
        event.author = author.to_string();
        event.llm_response.content = Some(Content::new(author).with_text(text));
        event.actions.state_delta.insert("turns".to_string(), json!(index + 1));
        service
            .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
            .await
            .expect("append event");
    }
}

async fn write_memory(service: &dyn MemoryService) {
    service
        .add_session(
            APP,
            USER,
            SESSION,
            vec![MemoryEntry {
                content: Content::new("user").with_text("O'Brien prefers the harbour at high tide"),
                author: "user".to_string(),
                timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
                    .unwrap()
                    .into(),
            }],
        )
        .await
        .expect("add memory");
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("output directory");
    let out = Path::new(&out);
    let sqlite = |name: &str| format!("sqlite://{}?mode=rwc", out.join(name).display());

    let sessions = adk_session::SqliteSessionService::new(&sqlite("session.db")).await.unwrap();
    sessions.migrate().await.unwrap();
    write_session(&sessions).await;

    let memory = SqliteMemoryService::new(&sqlite("memory.db")).await.unwrap();
    memory.migrate().await.unwrap();
    write_memory(&memory).await;

    let checkpointer = SqliteCheckpointer::new(&sqlite("graph.db")).await.unwrap();
    let mut checkpoint = Checkpoint::new(
        "upgrade_thread",
        HashMap::from([("count".to_string(), json!(2)), ("note".to_string(), json!("O'Brien"))]),
        3,
        vec!["review".to_string()],
    );
    checkpoint.checkpoint_id = "upgrade_checkpoint".to_string();
    checkpoint.created_at = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .unwrap()
        .into();
    checkpointer.save(&checkpoint).await.unwrap();

    if let Some(url) = args.next() {
        let sessions = adk_session::PostgresSessionService::new(&url).await.unwrap();
        sessions.migrate().await.unwrap();
        write_session(&sessions).await;
        // No embedding provider: entries are stored without vectors and searched by keyword.
        let memory = adk_memory::PostgresMemoryService::new(&url, None).await.unwrap();
        memory.migrate().await.unwrap();
        write_memory(&memory).await;
    }
}
```
