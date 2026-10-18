# Provider request correctness

Drives `LlmAgent` and `Runner` with the real `adk-model` clients and checks the requests they
send and how the run ends. Live scenarios go to the provider APIs, through a local proxy that
records each request where the check needs the request body; scripted scenarios point the
client at a local upstream that replays provider responses, for faults a live API does not
produce on demand (a stalled stream, a DeepSeek-V3 server without a tool-call parser).

## Scenarios

| Finding | Scenario | Mode | Checks |
|---------|----------|------|--------|
| ML5 | `ml5-openai-chat`, `ml5-groq`, `ml5-deepseek` | live | A run budget refuses two parallel calls, so one content answers both; the next two turns on the session succeed |
| ML5 | `ml5-compatible`, `ml5-azure-openai`, `ml5-groq-scripted`, `ml5-deepseek-scripted`, `ml5-azure-ai` | scripted | The follow-up request answers every tool call with its own `tool` message |
| ML6 | `ml6-responses`, `ml6-openai-routed` | live | An `output_schema` agent with a tool sends `text.format` `json_schema` on every Responses request and answers with JSON matching it |
| ML12 | `ml12-openai-chat`, `ml12-anthropic`, `ml12-groq`, `ml12-deepseek` | live | Stop sequences, seed, and penalties reach the request and the stop sequence cuts the answer; an unsupported field fails naming it |
| ML12 | `ml12-responses` | live | `stop_sequences` and `seed` fail naming the field instead of being dropped |
| ML12 | `ml12-azure-ai` | scripted | The same fields reach an Azure AI Inference request; `top_logprobs` fails naming it |
| ML13 | `ml13-anthropic-whitespace`, `ml13-anthropic-empty-text` | live | After a turn that answered with only whitespace (or empty text), the next live `claude-sonnet-4-6` turn succeeds and no blank text block is sent |
| ML14 | `ml14-responses-reasoning` | live | Reasoning summaries are not replayed as assistant messages over two tool turns |
| ML15 | `ml15-anthropic` | live | `claude-sonnet-4-6` describes an image a tool returned |
| ML15 | `ml15-groq`, `ml15-deepseek`, `ml15-azure-ai`, `ml15-ollama` | scripted | The follow-up request carries the tool's image |
| ML18 | `ml18-openai-chat`, `ml18-openai-responses`, `ml18-groq`, `ml18-deepseek`, `ml18-azure-ai`, `ml18-ollama` | scripted | A stream that stalls after its first bytes fails after the 2-second idle timeout instead of hanging |
| ML18 | `ml18-live-happy-path` | live | OpenAI Chat, OpenAI Responses, and Groq clients with timeouts set complete a turn |
| ML25 | `ml25-anthropic-display` | live | `claude-haiku-5-5` adaptive thinking with `ThinkingDisplay::Summarized` returns thinking text |
| ML29 | `ml29-compatible`, `ml29-ollama` | scripted | DeepSeek-V3 native tool-call tokens in the text run the tool |
| ML29 | `ml29-compatible-live` | live | The OpenAI-compatible client runs a native tool call against Groq |

## Running

```bash
cp .env.example .env    # or point ADK_ENV_FILE at an existing .env
cargo run               # every scenario
cargo run -- ML5 ML13   # only these findings
```

Keys load from `ADK_ENV_FILE`, else the repository root `.env`, overriding the shell
environment. A live scenario without its key is skipped; one whose account cannot pay (DeepSeek
HTTP 402) is reported as skipped with the reason.

`cargo run --no-default-features` builds without the `fixed` feature, which leaves out the two
options the fixes added (`with_timeouts` and `with_thinking_display`), so the same scenarios
run against a revision that predates them.
