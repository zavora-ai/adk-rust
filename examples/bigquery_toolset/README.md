# BigQuery Toolset Example

Demonstrates the native BigQuery toolset from ADK-Rust v0.7.0 — an LLM agent that
lists datasets, inspects table schemas, and executes SQL queries via the BigQuery API.

## What This Shows

- Creating a `BigQueryToolset` with a Google Cloud project ID
- Building an `LlmAgent` with the BigQuery toolset attached
- Four BigQuery tools: `bigquery_list_datasets`, `bigquery_list_tables`, `bigquery_get_table_schema`, `bigquery_execute_sql`
- Dry-run mode when no project is configured (prints what would happen)
- Live mode with real BigQuery API calls
- Full discovery workflow: datasets → tables → schema → query → results

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` for the Gemini LLM provider
- (Optional) A Google Cloud project with BigQuery enabled for live mode
- (Optional) Application Default Credentials configured via `gcloud auth application-default login`

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key for the LLM agent |
| `GOOGLE_CLOUD_PROJECT` | No | GCP project ID — enables live mode |
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
# Dry-run mode (no GCP project needed)
cargo run --manifest-path examples/bigquery_toolset/Cargo.toml

# Live mode
export GOOGLE_CLOUD_PROJECT=your-gcp-project-id
gcloud auth application-default login
cargo run --manifest-path examples/bigquery_toolset/Cargo.toml
```

## Expected Output

### Dry-run mode

```
╔══════════════════════════════════════════╗
║  BigQuery Toolset — ADK-Rust v0.7.0      ║
╚══════════════════════════════════════════╝

⚠️  Running in dry-run mode (no GOOGLE_CLOUD_PROJECT set)
   Set GOOGLE_CLOUD_PROJECT to run against the real BigQuery API.

--- Available BigQuery Tools ---

  1. bigquery_list_datasets
     Lists all datasets in the Google Cloud project.
     ...

--- Simulated Agent Interaction ---

  Agent prompt: "Discover datasets, pick a table, inspect its schema, ..."
  ...

✅ BigQuery Toolset example completed successfully.
```

### Live mode

```
╔══════════════════════════════════════════╗
║  BigQuery Toolset — ADK-Rust v0.7.0      ║
╚══════════════════════════════════════════╝

🔑 Running in live mode with project: your-gcp-project-id

--- Running BigQuery Agent ---

  🔧 Tool call: bigquery_list_datasets({"project_id":"your-gcp-project-id"})
  ← Response from bigquery_list_datasets: { ... }
  🔧 Tool call: bigquery_list_tables({"dataset_id":"my_dataset"})
  ← Response from bigquery_list_tables: { ... }
  🔧 Tool call: bigquery_get_table_schema({"dataset_id":"my_dataset","table_id":"my_table"})
  ← Response from bigquery_get_table_schema: { ... }
  🔧 Tool call: bigquery_execute_sql({"query":"SELECT * FROM `my_dataset.my_table` LIMIT 5"})
  ← Response from bigquery_execute_sql: { ... }
  💬 Agent: Here are the first 5 rows from my_dataset.my_table...

✅ BigQuery Toolset example completed successfully.
```
