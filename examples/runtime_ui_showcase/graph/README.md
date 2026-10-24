# Graph workflow

![Graph workflow in the embedded runtime UI](../screenshots/graph.jpg)

This example exposes a real `GraphAgent` with deterministic control flow:

```text
migration_workflow → plan → review → END
```

Both workflow nodes are ordinary OpenAI-backed `LlmAgent`s. The graph controls
their order and state projection; the runtime UI reads the graph's portable
flow topology without depending on `adk-graph` types.

## Run

```bash
cargo run --manifest-path examples/runtime_ui_showcase/Cargo.toml \
  --bin runtime-ui-graph
```

`/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10 S1), and the
screenshot shows it as it was. Send the prompt through the REST API the binary serves under
`http://127.0.0.1:8088/api/` (`POST /api/run_sse` or `POST /api/run`; see
[Server API](../../../docs/official_docs/deployment/server.md)):

> Plan a zero-downtime migration of the checkout API from VMs to Kubernetes.

Expected behavior:

1. `plan` produces a bounded migration plan.
2. Graph state passes that plan into `review`.
3. `review` returns the final Markdown risk report and go/no-go verdict.
4. `GET /api/ui/agents/{name}` describes the exact workflow flow edges.
