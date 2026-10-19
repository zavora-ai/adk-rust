- **Integral floats are read as integers** (`adk-tool`, `adk-rust-macros`): `3.0` for an
  integer field of a `#[tool]` tool, or of a `FunctionTool` or `StatefulTool` with a
  parameters schema, reaches the function as `3` instead of failing serde's integer types,
  also with `#[tool(lenient_args)]` or `with_lenient_args(true)`; some models write every
  number with a decimal point. `adk_tool::coerce_integral_floats` applies the same rule for
  hand-written tools.
