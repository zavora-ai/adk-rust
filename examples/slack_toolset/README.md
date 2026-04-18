# Slack Toolset Example

Demonstrates the native Slack toolset from ADK-Rust v0.7.0 — an LLM agent that
reads channels, sends messages, adds reactions, and lists threads via the Slack API.

## What This Shows

- Creating a `SlackToolset` with a Slack Bot Token
- Building an `LlmAgent` with the Slack toolset attached
- Four Slack tools: `slack_send_message`, `slack_read_channel`, `slack_add_reaction`, `slack_list_threads`
- Dry-run mode when no token is configured (prints what would happen)
- Live mode with real Slack API calls
- Handling Slack API errors with descriptive messages

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` for the Gemini LLM provider
- (Optional) A Slack Bot Token for live mode

### Setting Up a Slack Bot Token

1. Go to [https://api.slack.com/apps](https://api.slack.com/apps) and create a new app
2. Under **OAuth & Permissions**, add these bot token scopes:
   - `chat:write` — send messages
   - `channels:history` — read channel messages
   - `reactions:write` — add emoji reactions
   - `channels:read` — list channels (for thread listing)
3. Install the app to your workspace
4. Copy the **Bot User OAuth Token** (`xoxb-...`)

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key for the LLM agent |
| `SLACK_BOT_TOKEN` | No | Slack Bot Token — enables live mode |
| `SLACK_CHANNEL` | No | Target channel (default: `#general`) |
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
# Dry-run mode (no Slack token needed)
cargo run --manifest-path examples/slack_toolset/Cargo.toml

# Live mode
export SLACK_BOT_TOKEN=xoxb-your-bot-token
export SLACK_CHANNEL=#your-channel
cargo run --manifest-path examples/slack_toolset/Cargo.toml
```

## Expected Output

### Dry-run mode

```
╔══════════════════════════════════════════╗
║  Slack Toolset — ADK-Rust v0.7.0         ║
╚══════════════════════════════════════════╝

⚠️  Running in dry-run mode (no SLACK_BOT_TOKEN set)
   Set SLACK_BOT_TOKEN to run against the real Slack API.

--- Available Slack Tools ---

  1. slack_send_message
     Posts a message to a Slack channel.
     ...

--- Simulated Agent Interaction ---

  Agent prompt: "Read the last 5 messages from #general, ..."
  ...

✅ Slack Toolset example completed successfully.
```

### Live mode

```
╔══════════════════════════════════════════╗
║  Slack Toolset — ADK-Rust v0.7.0         ║
╚══════════════════════════════════════════╝

🔑 Running in live mode with SLACK_BOT_TOKEN
   Target channel: #your-channel

--- Running Slack Agent ---

  🔧 Tool call: slack_read_channel({"channel":"#your-channel","limit":5})
  ← Response from slack_read_channel: { ... }
  💬 Agent: Here's a summary of the recent messages...
  🔧 Tool call: slack_send_message({"channel":"#your-channel","text":"..."})
  🔧 Tool call: slack_add_reaction({"channel":"#your-channel","timestamp":"...","name":"thumbsup"})

✅ Slack Toolset example completed successfully.
```
