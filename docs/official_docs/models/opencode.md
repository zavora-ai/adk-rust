# OpenCode Go and Zen

`OpenCodeClient` routes OpenCode Go and OpenCode Zen models through the existing Chat
Completions, Responses, Anthropic Messages, and Gemini `generateContent` clients, and sends the
application's identity headers with every request.

## Installation

Enable the `opencode` feature on `adk-model`, or the same feature on `adk-rust`:

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["opencode"] }
```

## Usage

```rust
use adk_model::opencode::{OpenCodeClient, OpenCodeConfig, OpenCodeService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = OpenCodeClient::new(
        OpenCodeConfig::new(
            OpenCodeService::Go,
            std::env::var("OPENCODE_API_KEY")?,
            "deepseek-v4.1-flash",
        )
        .with_user_agent("my-coding-agent/1.0")
        .with_session_id("conversation-42"),
    )?;
    println!("{:?}", model.api());
    Ok(())
}
```

`OpenCodeClient` implements `adk_core::Llm` and delegates to the existing protocol clients. Choose
`OpenCodeService::Go` or `OpenCodeService::Zen` explicitly; the OpenAI clients do not detect
OpenCode endpoints.

| Service | Base URL | Routing reference |
| --- | --- | --- |
| Go | `https://opencode.ai/zen/go/v1` | [Go endpoint table](https://opencode.ai/docs/go/#endpoints) |
| Zen | `https://opencode.ai/zen/v1` | [Zen endpoint table](https://opencode.ai/docs/zen/#endpoints) |

The service determines the route: MiniMax M3 and Qwen3.8 Max use Messages on Go but Chat
Completions on Zen.

## Configuration

| Method | Applies to | Effect |
| --- | --- | --- |
| `with_user_agent` | All APIs | Sets the application's `User-Agent` (required) |
| `with_session_id` | All APIs | Sets `x-opencode-session` (required); reuse it for main and auxiliary requests in one conversation |
| `with_api` | All APIs | Selects the API for a model missing from the routing table |
| `with_reasoning_effort` | Chat Completions, Responses | Reasoning effort |
| `with_anthropic_thinking`, `with_anthropic_effort` | Messages | Thinking mode and output effort |
| `with_gemini_thinking` | GenerateContent | Thinking configuration |
| `with_base_url` | All APIs | HTTPS proxy, or loopback HTTP for tests; must end in `/v1` |
| `with_retry_config` | All APIs | Retry policy of the selected protocol client |

- Unknown model IDs require `with_api(OpenCodeApi::...)`; the client does not guess a protocol or
  retry on another API.
- `OpenCodeClient::new` rejects an empty API key, missing identity headers, reasoning options for a
  different API, and base URLs with whitespace, credentials, a query, or a fragment.
- Chat Completions replays returned thinking through `reasoning_content` for tool continuations.
- Redirects are not followed, so credentials are only sent to the configured host.
- Use `LlmRequest.config` for sampling and output limits.

## Example

The standalone [`examples/opencode`](../../../examples/opencode) crate streams one reply. Offline
HTTP tests in `adk-model/tests/opencode/` cover API routing, identity headers, usage conversion,
tool continuations, redirects, and invalid configuration; they do not establish live account
availability or quota.
