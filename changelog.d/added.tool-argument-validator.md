- **Tool argument validation helpers** (`adk-tool`): `ToolArgumentValidator` checks a call's
  arguments against a JSON Schema and returns a `tool.invalid_arguments` error
  (`TOOL_INVALID_ARGUMENTS_CODE`) the model can act on; a schema that does not compile fails
  with `TOOL_INVALID_PARAMETERS_SCHEMA_CODE`. `close_object_schemas` sets
  `additionalProperties: false` on objects that declare all of their fields, and
  `model_facing_schema` is the reduction `#[tool]` applies to the schema it declares.
