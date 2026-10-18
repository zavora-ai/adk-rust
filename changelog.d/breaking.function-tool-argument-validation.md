- **`FunctionTool` and `StatefulTool` check arguments against their schema**
  (`adk-tool`): a tool given a schema with `with_parameters_schema::<T>()` validates each
  call as `#[tool]` does, with undeclared keys rejected, and fails a call that breaks it
  with a `tool.invalid_arguments` error the model can correct; the handler does not run.
  `with_lenient_args(true)` turns the check off.
