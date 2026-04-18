# Spanner Toolset Example

Demonstrates the native Spanner toolset from ADK-Rust v0.7.0 — an LLM agent that
lists tables, inspects table schemas, and executes SQL queries via the Cloud Spanner API.

## What This Shows

- Creating a `SpannerToolset` with a project ID, instance ID, and database ID
- Building an `LlmAgent` with the Spanner toolset attached
- Three Spanner tools: `spanner_list_tables`, `spanner_get_table_schema`, `spanner_execute_sql`
- Dry-run mode when no project is configured (prints what would happen)
- Live mode with real Spanner API calls
- Full discovery workflow: tables → schema → query → results

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` for the Gemini LLM provider
- (Optional) A Google Cloud project with Cloud Spanner enabled for live mode
- (Optional) Application Default Credentials configured via `gcloud auth application-default login`

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key for the LLM agent |
| `SPANNER_PROJECT_ID` | No | GCP project ID — enables live mode |
| `SPANNER_INSTANCE_ID` | No | Spanner instance ID (required with project ID) |
| `SPANNER_DATABASE_ID` | No | Spanner database ID (required with project ID) |
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
# Dry-run mode (no GCP project needed)
cargo run --manifest-path examples/spanner_toolset/Cargo.toml

# Live mode
export SPANNER_PROJECT_ID=your-gcp-project-id
export SPANNER_INSTANCE_ID=your-spanner-instance-id
export SPANNER_DATABASE_ID=your-spanner-database-id
gcloud auth application-default login
cargo run --manifest-path examples/spanner_toolset/Cargo.toml
```

## Expected Output

### Dry-run mode

```
╔══════════════════════════════════════════╗
║  Spanner Toolset — ADK-Rust v0.7.0       ║
╚══════════════════════════════════════════╝

⚠️  Running in dry-run mode (no SPANNER_PROJECT_ID set)
   Set SPANNER_PROJECT_ID, SPANNER_INSTANCE_ID, and SPANNER_DATABASE_ID
   to run against the real Spanner API.

--- Available Spanner Tools ---

  1. spanner_list_tables
     Lists all tables in the Spanner database.
     ...

--- Simulated Agent Interaction ---

  Agent prompt: "List tables, pick one, inspect its schema, ..."
  ...

✅ Spanner Toolset example completed successfully.
```

### Live mode

```
╔══════════════════════════════════════════╗
║  Spanner Toolset — ADK-Rust v0.7.0       ║
╚══════════════════════════════════════════╝

🔑 Running in live mode with project: your-project, instance: your-instance, database: your-db

--- Running Spanner Agent ---

  🔧 Tool call: spanner_list_tables({})
  ← Response from spanner_list_tables: { ... }
  🔧 Tool call: spanner_get_table_schema({"table_name":"Users"})
  ← Response from spanner_get_table_schema: { ... }
  🔧 Tool call: spanner_execute_sql({"query":"SELECT * FROM Users LIMIT 5"})
  ← Response from spanner_execute_sql: { ... }
  💬 Agent: Here are the first 5 rows from the Users table...

✅ Spanner Toolset example completed successfully.
```
