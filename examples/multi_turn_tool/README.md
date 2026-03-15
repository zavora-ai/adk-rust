# Multi-Turn Tool Conversation Example

Demonstrates correct tool response preservation across multiple conversation turns.

## The Problem This Showcases

In a multi-turn session with tool calls:

1. **Turn 1**: User asks a question → LLM calls a tool → tool returns a response with `"function"` role
2. **Turn 2**: User asks a follow-up → session history is loaded, including Turn 1's tool response

The session must preserve the `"function"` role on tool responses. If it gets mapped to `"model"`, the LLM provider receives malformed history and either errors out or produces incorrect results.

This was fixed in PR #139 (`MutableSession::conversation_history()`).

## Running

```bash
# Gemini (default)
export GOOGLE_API_KEY=...
cargo run --example multi_turn_tool

# OpenAI
export OPENAI_API_KEY=...
cargo run --example multi_turn_tool --features openai
```

## Suggested Conversation

```
Turn 1: How many widgets are in stock?
Turn 2: And how about gadgets?
Turn 3: Order 10 of whichever has more stock.
```

Each turn exercises the tool pipeline, and Turn 2+ validates that prior tool responses are correctly preserved in session history.
