# Gemini Native Tools Example Matrix

This crate exercises every Gemini native tool wrapper currently exposed by `adk-tool`, while preserving the multi-turn mixed-tool fix that originally motivated the example.

## Run

```bash
export GOOGLE_API_KEY=your-key-here
cargo run --manifest-path examples/gemini3_builtin_tools/Cargo.toml
```

Optional:

```bash
export GEMINI_MODEL=gemini-3.1-flash-lite-preview
export GEMINI_FILE_SEARCH_STORE=stores/your-store
```

## Coverage

| Scenario | Wrapper(s) | Notes |
| --- | --- | --- |
| Google Search + function tool | `GoogleSearchTool` | Grounded search plus a local function tool |
| URL context + function tool | `UrlContextTool` | URL grounding in an agent flow |
| Google Maps + function tool | `GoogleMapsTool` | Location-aware grounding with retrieval config |
| Code execution + function tool | `GeminiCodeExecutionTool` | Preserves executable-code and execution-result parts |
| File search + function tool | `GeminiFileSearchTool` | Requires `GEMINI_FILE_SEARCH_STORE` or `GEMINI_FILE_SEARCH_STORES` |
| Computer use invocation | `GeminiComputerUseTool` | Surfaces native computer-use protocol calls |
| Multi-turn native tool then function tool | `GoogleSearchTool` | Verifies server-side tool history and thought signatures survive into the next function-tool turn |

## Notes

- Scenarios that require Gemini File Search stores are skipped automatically when the relevant env vars are missing.
- The example prints grounding metadata when Gemini returns it.
- Code execution and computer use are demonstrated via the native server-tool parts surfaced by the framework.
