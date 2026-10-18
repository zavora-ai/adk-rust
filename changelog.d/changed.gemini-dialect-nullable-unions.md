- **Gemini declarations keep nullability, unions and map value types** (`adk-gemini`):
  the `OpenApiSubset` and `VertexOpenApiSubset` dialects turn a `null` branch, type or
  enum value into `nullable: true`, keep an `anyOf`/`oneOf` of several types as `anyOf`
  (and a type array as one) instead of the first branch, replace a `true` subschema with
  `{}` and drop a `false` property, write non-string `enum` values as strings while
  keeping the numeric `type`, and record a map's value type in its description. Gemini
  rejected a declaration with a boolean subschema or a numeric enum with a 400.
