- **Gemini function declarations keep `$ref` descriptions and schema bounds** (`adk-core`,
  `adk-gemini`): `schema_utils::resolve_refs` keeps the keywords beside a `$ref` when it
  inlines the definition, and they take precedence over the definition's own, so a field's
  description reaches Gemini instead of its type's. The `OpenApiSubset` and
  `VertexOpenApiSubset` dialects of `GeminiSchemaAdapter` no longer strip `minimum`,
  `maximum`, `minLength`, `maxLength`, `pattern`, `minItems` and `maxItems`, which the Gemini
  `Schema` proto defines, so `GeminiModel` sends them; `generateContent` and the Live API
  accept them. `exclusiveMinimum`, `exclusiveMaximum` and `multipleOf` are still stripped.
