# Schema Normalization

ADK-Rust normalizes every tool's parameters schema — from an MCP server, a `#[tool]`
function, a `FunctionTool`, or a hand-written `Tool` — for each LLM provider at request time.
A tool declares one schema, and each provider receives the form its function-calling API
accepts.

## How It Works

```
MCP Server → raw JSON Schema → McpToolset (stores verbatim)
                                      ↓
                          Model.generate_content()
                                      ↓
                          SchemaAdapter.normalize_schema()
                                      ↓
                          Provider API (Gemini/OpenAI/Anthropic)
```

1. **McpToolset** discovers tools and stores their raw `inputSchema` without modification; a
   schema over its `McpSchemaLimits` size or node limit is skipped at discovery (see
   [MCP client](../mcp/client.md))
2. When the model builds a request, it calls `schema_adapter().normalize_schema()` on each tool's schema
3. Each provider has its own adapter that applies only the transforms its API requires
4. Results are cached by content hash to avoid redundant normalization. The cache lock is
   not held while a schema is normalized, so one slow schema does not delay other requests

## Provider Behavior

The Gemini column is the default `GeminiSchemaAdapter::new()` that `GeminiModel` uses; see
[GeminiSchemaAdapter](#geminischemaadapter) for its other dialects.

| Feature | Gemini | OpenAI Strict | OpenAI | Anthropic | Generic |
|---------|--------|---------------|--------|-----------|---------|
| `$ref` resolution | ✅ Inlines, keeping the keywords beside the `$ref` | ❌ Preserves | ❌ Preserves | ❌ Preserves | ❌ Preserves |
| `anyOf`/`oneOf` | Null branch → `nullable`; one branch merged, several kept as `anyOf` | Preserves | Preserves | Preserves | Preserves |
| `allOf` | Merges | Preserves | Preserves | Preserves | Preserves |
| `additionalProperties` | Removes | Sets `false` | Preserves | Preserves | Preserves |
| Type arrays | `null` → `nullable`; several types → `anyOf` | Preserves | Preserves | Preserves | Preserves |
| Boolean subschemas | `true` → `{}`; a `false` property is dropped | Preserves | Preserves | Preserves | Preserves |
| Non-string `enum` values | → strings, `type` kept (`1` → `"1"`) | Preserves | Preserves | Preserves | Preserves |
| `$schema` | Strips | Strips | Strips | Strips | Strips |
| `if`/`then`/`else` | Strips | Strips | Strips | Strips | Strips |
| `const` | → `enum` | → `enum` | → `enum` | Preserves | → `enum` |
| Unsupported `format` | Strips | Strips | Strips | Preserves | Strips |
| Nesting depth limit | 5 levels | None | None | None | None |
| `minimum`/`maximum`, `minLength`/`maxLength`, `pattern`, `minItems`/`maxItems` | Preserves | Preserves | Preserves | Preserves | Preserves |
| `exclusiveMin/Max`, `multipleOf` | Removes | Preserves | Preserves | Preserves | Preserves |

## The SchemaAdapter Trait

All adapters implement `adk_core::SchemaAdapter` (`Send + Sync + Debug`). Only
`normalize_schema` is required:

| Method | Returns | Default |
|--------|---------|---------|
| `normalize_schema(&self, schema: Value)` | The schema in the form the provider accepts | Required |
| `normalize_tool_name(&self, name: &str)` | `Cow<str>` | The name truncated to 64 bytes at a UTF-8 character boundary |
| `empty_schema(&self)` | The schema for a tool with no `parameters_schema` | `{"type": "object", "properties": {}}` |
| `parameters_field(&self)` | The function-declaration field the normalized schema is posted under | `"parameters"` |

Each `Llm` implementation returns its adapter via `schema_adapter()`, which defaults to
`GenericSchemaAdapter`:

```rust
use adk_core::Llm;
use adk_model::GeminiModel;

let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;
let adapter = model.schema_adapter(); // &dyn SchemaAdapter: a GeminiSchemaAdapter
```

| Client | Adapter |
|--------|---------|
| `GeminiModel` | `GeminiSchemaAdapter::new()` |
| `OpenAIClient`, Azure OpenAI | `OpenAiSchemaAdapter` |
| `AnthropicClient` | `AnthropicSchemaAdapter` |
| Ollama, DeepSeek, and clients that do not override `schema_adapter()` | `GenericSchemaAdapter` |

## Available Adapters

### GeminiSchemaAdapter

The most aggressive adapter. Applies all destructive transforms required by Gemini's function-calling API:

```rust
use adk_gemini::schema_adapter::GeminiSchemaAdapter;
use adk_core::SchemaAdapter;

// Standard Gemini API
let adapter = GeminiSchemaAdapter::new();

// Vertex AI (sets additionalProperties: false instead of removing)
let adapter = GeminiSchemaAdapter::vertex_ai();

// Standard JSON Schema, posted under `parametersJsonSchema`
let adapter = GeminiSchemaAdapter::json_schema();
```

The constructor picks a `GeminiSchemaDialect`, which decides both the reduction and the
function-declaration field (`parameters_field()`):

| Constructor | Dialect | Field | Reduction |
|-------------|---------|-------|-----------|
| `new()` (default) | `OpenApiSubset` | `parameters` | All steps below; removes `additionalProperties` |
| `vertex_ai()` | `VertexOpenApiSubset` | `parameters` | All steps below; sets `additionalProperties: false` on objects instead of removing it |
| `json_schema()` | `JsonSchema` | `parametersJsonSchema` | Steps 1, 2, 13, 14 and 15 only, so `allOf`, `anyOf`, `if`/`then`, `additionalProperties` and every bound stay |

`GeminiModel` always uses `new()`. A Gemini Live session takes the JSON Schema dialect with
`GeminiRealtimeModel::with_schema_dialect(GeminiSchemaDialect::JsonSchema)` in `adk-realtime`.

Transform pipeline (`OpenApiSubset` and `VertexOpenApiSubset`):
1. Resolve `$ref` (inline from definitions). Keywords beside a `$ref`, such as a field's own
   `description`, stay on the inlined schema and take precedence over the definition's. A
   reference becomes `{"type": "object"}` when it cannot be resolved, when it names a
   definition already being inlined on its path (so a self-referencing definition is expanded once), when the chain of references
   is longer than 10, or when inlining it would copy more than
   `schema_utils::MAX_INLINED_REF_NODES` values in total.
2. Strip `$schema`
3. Replace boolean subschemas: `true` → `{}`; a property whose schema is `false` is dropped
4. Reduce `anyOf`/`oneOf`: null branches → `nullable: true`; one remaining branch is merged,
   several stay as `anyOf`
5. Merge `allOf` sub-schemas
6. Reduce type arrays: `["string", "null"]` → `"string"` + `nullable: true`;
   `["string", "integer"]` → `anyOf`
7. Strip `if`/`then`/`else`
8. Convert `const` → single-element `enum`
9. Reduce `enum` to strings: `null` → `nullable: true`; `[1, 2]` → `["1", "2"]` with
   `type: "integer"` kept, so Gemini still answers with a number
10. Add implicit `type: "object"`
11. Describe a map's value type (`additionalProperties` schema) in its `description`
12. Remove keywords the Gemini `Schema` proto does not define. The bounds it does define —
    `minimum`, `maximum`, `minLength`, `maxLength`, `pattern`, `minItems`, `maxItems` — are kept,
    so the model is shown the limits a `#[tool]` enforces.
13. Strip unsupported `format` values
14. Enforce nesting depth (5 levels)
15. Remove `definitions`/`$defs`

### OpenAiStrictSchemaAdapter

Preserves schema structure while adding `additionalProperties: false` to every object, and
returns `{"type": "object", "properties": {}, "additionalProperties": false}` as its empty
schema, as OpenAI strict mode requires. No client selects it by default:

```rust
use adk_model::openai::OpenAiStrictSchemaAdapter;
use adk_core::SchemaAdapter;

let adapter = OpenAiStrictSchemaAdapter;
```

### OpenAiSchemaAdapter

Minimal safe fixes for non-strict mode:

```rust
use adk_model::openai::OpenAiSchemaAdapter;

let adapter = OpenAiSchemaAdapter;
```

### AnthropicSchemaAdapter

Near pass-through — Anthropic supports most JSON Schema features:

```rust
use adk_model::anthropic::AnthropicSchemaAdapter;

let adapter = AnthropicSchemaAdapter;
```

### GenericSchemaAdapter

Default for unknown providers (Ollama, DeepSeek, etc.):

```rust
use adk_core::GenericSchemaAdapter;

let adapter = GenericSchemaAdapter;
```

## Schema Caching

Normalized schemas are cached by a hash of their content, independent of object key order:

```rust
use adk_core::{GenericSchemaAdapter, SchemaCache};
use serde_json::json;
use std::sync::Arc;

let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
let schema = json!({"type": "object", "properties": {"name": {"type": "string"}}});

// First call normalizes and caches
let result = cache.normalize(&schema);

// Subsequent calls return cached result (no re-normalization)
let cached = cache.normalize(&schema);

// Invalidate when tools change
cache.clear();
```

Each cache owns one adapter instance, so entries normalized for different
providers or adapter configurations cannot collide. The Gemini, OpenAI, Anthropic, and
OpenAI-compatible clients use these caches automatically during `generate_content()`. The
cache lock is held only for the lookup and the insert, never while a schema is normalized.

## Tool Name Truncation

All built-in adapters truncate tool names exceeding 64 bytes at valid UTF-8 character boundaries:

```rust
use adk_core::SchemaAdapter;
use adk_gemini::schema_adapter::GeminiSchemaAdapter;

let adapter = GeminiSchemaAdapter::new();

// Short names pass through unchanged
let name = adapter.normalize_tool_name("get_weather");
assert_eq!(name, "get_weather");

// Long names are truncated to 64 bytes
let long = "mcp_server_github_com_organization_repository_pull_request_review_comments";
let truncated = adapter.normalize_tool_name(long);
assert!(truncated.len() <= 64);

// Multi-byte characters are never split
let emoji_name = "🔧_tool_名前が長い";
let result = adapter.normalize_tool_name(emoji_name);
assert!(std::str::from_utf8(result.as_bytes()).is_ok());
```

## Shared Utilities

The `adk_core::schema_utils` module provides composable transform functions that adapters use internally:

```rust
use adk_core::schema_utils;
use serde_json::json;

let mut schema = json!({
    "$schema": "http://json-schema.org/draft-07/schema#",
    "type": "object",
    "properties": {
        "status": { "const": "active" },
        "email": { "type": "string", "format": "hostname" }
    },
    "if": { "properties": { "x": { "type": "number" } } },
    "then": { "required": ["x"] }
});

// Apply individual transforms
schema_utils::strip_schema_keyword(&mut schema);
schema_utils::strip_conditional_keywords(&mut schema);
schema_utils::convert_const_to_enum(&mut schema);
schema_utils::strip_unsupported_formats(&mut schema, &["date-time", "email", "uri"]);
```

Available utilities:
- `strip_schema_keyword` — removes `$schema`
- `strip_conditional_keywords` — removes `if`/`then`/`else`
- `add_implicit_object_type` — adds `type: "object"` when `properties` exists
- `convert_const_to_enum` — converts `const` to single-element `enum`
- `strip_unsupported_formats` — removes format values not in allowlist
- `strip_null_from_enum` — removes null from enum arrays
- `truncate_tool_name` — truncates at UTF-8 boundary
- `resolve_refs` — inlines `$ref` references from definitions, keeping the keywords beside each `$ref`
- `collapse_combiners` — collapses `anyOf`/`oneOf` to first non-null (not used by the Gemini adapter)
- `merge_all_of` — merges `allOf` sub-schemas
- `collapse_type_arrays` — collapses `["string", "null"]` to `"string"` (not used by the Gemini adapter)
- `enforce_nesting_depth` — replaces deep schemas with `{"type": "object"}`

## Custom Adapters

Implement `SchemaAdapter` for custom providers:

```rust
use adk_core::{SchemaAdapter, schema_utils};
use serde_json::Value;
use std::borrow::Cow;

#[derive(Debug)]
struct MyProviderAdapter;

impl SchemaAdapter for MyProviderAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        // Apply only the transforms your provider needs
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);
        // Keep everything else as-is
        schema
    }
}
```

## Example

Run the schema normalization demo to see all adapters in action:

```bash
cargo run --manifest-path examples/schema_normalization/Cargo.toml
```

No API keys needed — demonstrates normalization logic locally.

---

**Previous**: [← MCP Tools](mcp-tools.md) | **Next**: [Sessions →](../sessions/sessions.md)
