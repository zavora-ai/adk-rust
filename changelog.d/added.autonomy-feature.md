- **`autonomy` feature** (`adk-rust`): turns on everything an agent needs to run unattended —
  the harness and its kernel, durable and background runs with cron wake-ups, signed webhook
  wake-ups, the approvals service, SQLite sessions and the harness's SQLite stores, and the
  signed audit log. It is in the `enterprise` and `full` tiers. `adk_rust::harness::Workspace`
  re-exports `adk_devtools::Workspace`, so a harness needs no direct `adk-devtools`
  dependency. [Build an autonomous agent](docs/official_docs/autonomy/index.md) chooses among
  the mechanisms for running an agent unattended and builds one end to end.
