# Release tool fixes

Checks for the second set of 3.0 docs-audit fixes. Every scenario drives the real stack —
`LlmAgentBuilder` and `Runner` over the in-memory session service, the real `OpenAIClient`
and `GeminiModel` clients, `RagTool`, `McpToolset` over a stdio server, `AgentTool`,
`ActionNodeExecutor`, `ProcessBackend` under Seatbelt, `McpHttpClientBuilder`, `AcpSession`
against an `AcpServer`, and `adk_runner::Launcher` — and prints `PASS`, `FAIL`, or `SKIP` with
its evidence.

The stdio MCP server, the ACP server, and the console launcher are this binary, started as
child processes with `--mcp-server`, `--acp-server <shutdown timeout ms>`, and
`--launcher <gemini | base URL>`. Scripted model upstreams are local axum servers speaking the
OpenAI Chat Completions API.

## Scenarios

| Scenario | Item | Mode | Passes when |
|----------|------|------|-------------|
| `rag-result-has-no-vectors` | 1 | Local | A `rag_search` result has text, metadata, and score, and no `embedding` field |
| `rag-top-k-is-reranked` | 1 | Local | A `top_k` override runs the pipeline's reranker once and returns that many results |
| `rag-live-model-input` | 1 | Live | With Gemini embeddings, the `rag_search` response in the model's request has no `embedding` and stays under 4 KB |
| `mcp-is-error-text-reaches-model` | 2 | Scripted | The tool message after an `isError` result carries the server's guidance |
| `mcp-is-error-live-self-correction` | 2 | Live | Gemini calls `lookup_order` again with the ID the error text names and answers with the tracking code |
| `agent-tool-error-is-redacted` | 3 | Scripted | A delegate whose provider echoes a connection string fails, and the coordinator's model never receives the credential |
| `notification-host-allowlist` | 4 | Local | A webhook outside `allow_hosts` fails the node and the internal service gets no request |
| `notification-scheme-policy` | 4 | Local | An `http` webhook fails under `allow_schemes(["https"])` |
| `rss-host-allowlist` | 4 | Local | A feed outside `allow_hosts` fails the node and gets no request |
| `rss-redirect-checked` | 4 | Local | A redirect to an unlisted host is not followed |
| `seatbelt-usr-bin-python3` | 5 | Local | On macOS, Python found through `PATH=/usr/bin:/usr/local/bin` runs under Seatbelt |
| `progress-marker-survives-full-queue` | 6 | Scripted | A client that stalls while a tool's worker floods progress receives one truncation marker |
| `mcp-oauth-connect-error-code` | 7 | Scripted | A rejected client credential fails `connect()` with `mcp.oauth.token_fetch` and `Unauthorized` |
| `acp-shutdown-lets-prompt-finish` | 7 | Local | A prompt running when the server shuts down is answered within the 5 s `shutdown_timeout` |
| `acp-shutdown-timeout-bounds-wait` | 7 | Local | A prompt longer than a 200 ms `shutdown_timeout` is cut off at about the timeout |
| `agent-tool-output-key-answer` | 10 | Scripted | A delegate with an `output_key` answers through the tool instead of `No response from agent` |
| `agent-tool-output-key-live` | 10 | Live | The same on Gemini |
| `launcher-tool-call-once-scripted` | 11 | Scripted | The console `Launcher` prints the tool call once |
| `launcher-tool-call-once-live` | 11 | Live | The same on Gemini, which streams the call in a partial event and again in the complete one |

## Running

```bash
cargo run --manifest-path examples/release_tool_fixes/Cargo.toml
cargo run --manifest-path examples/release_tool_fixes/Cargo.toml -- 1 4
```

Arguments select audit items; without them every scenario runs. The process exits non-zero on
any `FAIL`. Keys are loaded from the dotenv file named by `ADK_ENV_FILE`, else from the
repository's `.env`; either overrides the process environment. The live scenarios run on
`gemini-3.5-flash-lite` when `GOOGLE_API_KEY` (or `GEMINI_API_KEY`) is set and make about ten
model calls; without a key they are skipped. The Seatbelt scenario is skipped off macOS.
