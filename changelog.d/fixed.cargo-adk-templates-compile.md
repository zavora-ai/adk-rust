- **Every `cargo adk new` template and addon compiles** (`cargo-adk`): the `sequential`,
  `parallel`, `loop`, `conditional` and `custom` templates, the `multi-agent` and `pipeline`
  patterns built on `sequential`, and the `browser` addon generated code that did not compile.
  The workflow templates share one `Arc<dyn Llm>` and use the prelude's agents; `parallel`
  adds an aggregator over the specialists' output keys, `loop` ends when a critic calls
  `exit_loop`, `conditional` routes with `LlmConditionalAgent`, and `custom` calls the model
  from its own `Agent` implementation. The `browser` addon builds a `BrowserToolset`.
- **Addons do what they say** (`cargo-adk`): `telemetry` installs ADK telemetry and exports
  spans over OTLP when `OTEL_EXPORTER_OTLP_ENDPOINT` is set, and is refused by `harness`,
  `autonomous` and `vision`, whose launcher installs its own exporter. `auth` no longer panics
  when `AUTH_API_KEY` is unset, no addon logs work it does not do, and the addon and pattern
  descriptions in `cargo adk addons`, `cargo adk templates` and the READMEs state which
  addons are starting points to wire in. Generated projects compile without warnings.
- **CI compiles every template, pattern and addon** (`scripts/check-cargo-adk-templates.sh`):
  the PR-tier `templates` job checks each one with `--all-targets` and fails on a warning in
  the generated code, and a `cargo-adk` test fails when a template or addon has no entry.
