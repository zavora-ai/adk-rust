- **`#[tool]` checks arguments against its schema before the function runs**
  (`adk-rust-macros`, `adk-tool`): the generated tool validates each call against its
  parameters schema, with `additionalProperties: false` set on every object that declares all
  of its fields; `parameters_schema()` itself is unchanged. A key the schema does not name, a
  value outside a `range`, `length`, or `pattern` bound, or a missing required field fails the
  call with a `tool.invalid_arguments` error that lists each violation by JSON pointer; an
  `LlmAgent` shows that message to the model so it can correct the call, and the function does
  not run. serde dropped unknown keys and the bounds were never enforced. `null` stays accepted
  for an optional field. Opt a tool out with `#[tool(lenient_args)]`, or keep one struct's keys
  open with `#[schemars(extend("additionalProperties" = true))]`, where a `#[serde(alias)]` or a
  custom deserializer accepts input the schema does not describe.
