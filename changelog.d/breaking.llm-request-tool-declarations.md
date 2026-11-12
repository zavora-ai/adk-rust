- **Tools reach the model in declaration order** (`adk-core`, `adk-model`, `adk-agent`):
  `LlmRequest.tools` is now `adk_core::ToolDeclarations`, a map that keeps the order the
  agent declares its tools in (static tools, then each toolset's), instead of a `HashMap`
  with a different order on every run. Every provider sends tools in that order, as
  adk-python and adk-go do. Anthropic, Bedrock and the OpenAI clients no longer sort tools
  by name; the order is still the same on every request, so the prompt-cache prefix stays
  stable, but a cached prefix from before the upgrade misses once.
  `CacheCapable::create_cache` and `BuiltinToolGovernor::govern` take `&ToolDeclarations`.

  Migration: build tools with `ToolDeclarations::new()` and `insert(name, declaration)`, or
  `collect()` an iterator of `(name, declaration)` pairs; `tools: Default::default()` still
  works in an `LlmRequest` literal. `get`, `contains_key`, `remove`, `iter`, `keys`,
  `values`, `len` and `is_empty` work as on a `HashMap`; `keys()` and `values()` return
  `ToolDeclarationsKeys` and `ToolDeclarationsValues`, which print with `{:?}` as a
  `HashMap`'s keys and values do.
