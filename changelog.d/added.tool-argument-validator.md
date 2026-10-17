- **Tool argument validation helpers** (`adk-tool`): `ToolArgumentValidator` checks a call's
  arguments against a JSON Schema and returns a `tool.invalid_arguments` error the model can
  act on; `close_object_schemas` sets `additionalProperties: false` on objects that declare
  all of their fields; `model_facing_schema` is the reduction `#[tool]` applies to the schema
  it declares.
