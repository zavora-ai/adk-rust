# OpenAI Conversations API Example

Demonstrates the OpenAI Responses API **Conversations** lifecycle. The Conversations API provides server-managed conversation state, allowing you to send multiple messages without re-sending the full history on each request. The server automatically prepends previous items to each new request, enabling persistent multi-turn interactions.

## Prerequisites

- Rust 1.85.0+
- `OPENAI_API_KEY` environment variable set with a valid OpenAI API key

## Running

```bash
cargo run --manifest-path examples/openai_conversations/Cargo.toml
```

## What It Does

1. **Create** — Creates a new conversation via `ConversationsClient::create()`
2. **First message** — Sends an initial message with `conversation_id` in extensions, introducing context
3. **Follow-up** — Sends a second message referencing the first, demonstrating that the server retains history without the client re-sending it
4. **Metadata** — Retrieves conversation metadata via `ConversationsClient::get()`
5. **Delete** — Cleans up the conversation via `ConversationsClient::delete()`

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | Yes | — | OpenAI API key |

## Related

- [`adk-model/src/openai/conversations.rs`](../../adk-model/src/openai/conversations.rs) — Conversations API client implementation
- [`adk-model/src/openai/responses_client.rs`](../../adk-model/src/openai/responses_client.rs) — OpenAI Responses API client
- [OpenAI Conversations API](https://platform.openai.com/docs/api-reference/conversations) — Official documentation
