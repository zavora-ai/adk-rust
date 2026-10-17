# Tool Schemas

Whether an `LlmAgent` enforces a `#[tool]` tool's argument schema, what Gemini receives of that
schema, what happens to two tools with one name, and the scripted `Llm` test double. Every
scenario drives the real stack — `LlmAgentBuilder`, `Runner`, `InMemorySessionService`, and the
provider clients `OpenAIClient` and `GeminiModel` — and asserts on what reached the model, what
reached the wire, and how often the tool ran, never on model prose.

The tool is `book_slots`, a `#[tool]` function whose argument type bounds `count` to 1–10, gives
`room` a length and a pattern, caps `attendees` at three items, and nests a documented
`confirm_to: Address` that schemars emits as a `$ref` with the field's description beside it.

Deterministic bad-argument turns come from a scripted OpenAI-compatible Chat Completions server
on `127.0.0.1`; the Gemini wire schema is captured by a scripted Gemini `generateContent`
server. The real clients are pointed at both through their base URLs. The `live_*` scenarios
run against OpenAI (`gpt-5.6-terra`) and Gemini (`gemini-3.5-flash-lite`) when their keys are
set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `unknown_argument` | Scripted OpenAI-compatible | Calls carrying an undeclared top-level key (`dry_run`) and an undeclared nested key (`confirm_to.zip`) are answered with an invalid-arguments error naming the key, `book_slots` does not run for them, and the model's corrected call runs once |
| `out_of_range_argument` | Scripted OpenAI-compatible | A call with `count: 50` and `room: "lobby"` is answered with an error naming `/count`, the maximum and `/room`; only the corrected call runs |
| `gemini_wire_schema` | Scripted Gemini | The function declaration `GeminiModel` sends keeps the `confirm_to` field's own description, inlines `Address`, and keeps `minimum`/`maximum`, `minLength`/`maxLength`, `pattern` and `maxItems` |
| `duplicate_tool_names` | None | `LlmAgentBuilder::build` fails when two tools are named `book_slots` |
| `scripted_llm_records_requests` | None | An `LlmAgent` on `adk_model::ScriptedLlm` runs a two-turn tool loop; the double recorded both requests, the second carrying the tool result |
| `live_openai_schema` | OpenAI | The schema with its bounds is accepted and a booking runs with arguments inside them |
| `live_gemini_schema` | Gemini | The declaration with the `$ref` description and bounds is accepted and a booking runs with arguments inside the bounds |

## Running

```bash
cargo run --manifest-path examples/tool_schemas/Cargo.toml
cargo run --manifest-path examples/tool_schemas/Cargo.toml -- --scenario unknown_argument,gemini_wire_schema
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Keys are read from the repository's `.env` (`OPENAI_API_KEY`, `GOOGLE_API_KEY` or
`GEMINI_API_KEY`), overriding variables the shell already exports; set `ADK_ENV_FILE` to an
absolute path to read another file.
