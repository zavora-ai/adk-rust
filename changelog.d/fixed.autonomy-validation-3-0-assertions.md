- **`examples/autonomy_validation` passes on 3.0** (`examples`): `runner_plugin_denies_tool_err`
  asserts the 3.0 answer to a failing before-tool callback (`tool '<name>' was not run: a
  before-tool callback failed with …`, without the callback's message) and
  `path_guardrail_fail_closed` asserts `PathAllowList`'s `holds no path at any depth` denial;
  both still require zero executions. The example's README and `examples/README.md` describe
  its Phase 0–2 scope and the original-call-id resume of `approval_across_runs`.
