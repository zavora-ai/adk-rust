- **Gemini declarations keep nullability, unions and map value types** (`adk-gemini`):
  the `OpenApiSubset` and `VertexOpenApiSubset` dialects turn a `null` branch, type or
  enum value into `nullable: true`, keep an `anyOf`/`oneOf` of several types (and a type
  array) as `anyOf` instead of keeping only the first non-null branch, replace a `true`
  subschema with `{}` and drop a `false` property, write non-string `enum` values as strings
  while keeping or adding their `integer`, `number` or `boolean` `type`, and record a map's
  value type in its description. Gemini rejected a declaration with a boolean subschema or a
  numeric enum with a 400.
