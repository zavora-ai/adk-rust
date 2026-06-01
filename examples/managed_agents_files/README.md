# Managed Agents: File Upload and Processing

Demonstrates uploading a file via the Anthropic Files API, mounting it in a managed agent session, and asking the agent to analyze the file contents.

## Prerequisites

- Rust 1.85.0+
- An `ANTHROPIC_API_KEY` with Managed Agents and Files API beta access

## Setup

```bash
cp ../../.env.example .env
# Add your ANTHROPIC_API_KEY to .env
```

## Running

```bash
cargo run -p managed-agents-files
```

## What it demonstrates

1. Create a `FilesClient` and upload a CSV file (generated in-memory)
2. Create a managed agent session
3. Mount the uploaded file in the session at `/workspace/sales_data.csv`
4. Ask the agent to analyze the file
5. Stream the agent's analysis response
6. Clean up: delete the file, archive session, delete agent and environment
