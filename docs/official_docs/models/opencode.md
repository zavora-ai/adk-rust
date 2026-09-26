# OpenCode Go and Zen

Enable `adk-model`'s `opencode` feature, or the same feature on `adk-rust`.

```rust
use adk_model::opencode::{OpenCodeClient, OpenCodeConfig, OpenCodeService};

let model = OpenCodeClient::new(
    OpenCodeConfig::new(OpenCodeService::Go, std::env::var("OPENCODE_API_KEY")?, "deepseek-v4.1-flash")
        .with_user_agent("my-coding-agent/1.0")
        .with_session_id("conversation-42"),
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`OpenCodeClient` implements `adk_core::Llm` and delegates to the existing protocol clients. Choose `OpenCodeService::Go` or `OpenCodeService::Zen` explicitly; the original OpenAI clients do not detect OpenCode endpoints.

| Service | Base URL | Routing reference |
| --- | --- | --- |
| Go | `https://opencode.ai/zen/go/v1` | [Go endpoint table](https://opencode.ai/docs/go/#endpoints) |
| Zen | `https://opencode.ai/zen/v1` | [Zen endpoint table](https://opencode.ai/docs/zen/#endpoints) |

Routes include Chat Completions, Responses, Anthropic Messages, and Gemini GenerateContent. The service matters: MiniMax M3 and Qwen3.8 Max use Messages on Go but Chat Completions on Zen. Jev's structured System One API is outside the `Llm` conversation interface and is not supported.

- Supply the application's own `User-Agent` and a stable `x-opencode-session` value. Reuse the ID for main and auxiliary requests in the same conversation.
- Unknown model IDs require `with_api(OpenCodeApi::...)`; the client does not guess a protocol or retry on another API.
- Chat Completions replays returned thinking through `reasoning_content` for tool continuations.
- Chat Completions and Responses accept `with_reasoning_effort`. Messages uses `with_anthropic_thinking` and `with_anthropic_effort`. GenerateContent uses `with_gemini_thinking`. Choose options supported by the selected model.
- `with_base_url` supports HTTPS proxies and loopback HTTP tests. Include the `/v1` suffix.
- Use `LlmRequest.config` for sampling and output limits. `with_retry_config` configures the selected model client's retry policy.

The standalone `examples/opencode` crate demonstrates streaming. Offline HTTP tests cover API routing, identity headers, usage conversion, tool continuations, and invalid configuration; they do not establish live account availability or quota.
