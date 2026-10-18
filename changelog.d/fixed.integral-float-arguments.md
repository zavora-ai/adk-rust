- **Integral floats are read as integers** (`adk-tool`, `adk-rust-macros`): `3.0` for
  an integer field of a `#[tool]`, `FunctionTool` or `StatefulTool` reaches the function
  as `3` (also with `lenient_args`) instead of failing serde's integer types; some models
  write every number with a decimal point. `adk_tool::coerce_integral_floats` applies the
  same rule for hand-written tools.
