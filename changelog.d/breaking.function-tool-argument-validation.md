- **`FunctionTool` and `StatefulTool` check arguments against their schema**
  (`adk-tool`): a tool given a schema with `with_parameters_schema::<T>()` validates each
  call as `#[tool]` does — undeclared keys, values outside a `range`, `length`, or `pattern`
  bound, and missing required fields are rejected, and `null` stays accepted for an optional
  field — and fails a call that breaks it with a `tool.invalid_arguments` error the model can
  correct; the handler does not run. `with_lenient_args(true)` turns the check off. A tool
  without `with_parameters_schema` is not checked.
