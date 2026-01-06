# Multi-Agent Test

Test examples for the [Multi-Agent Systems](../../docs/official_docs/agents/multi-agent.md) documentation.

## Prerequisites

Set your API key:
```bash
echo 'GOOGLE_API_KEY=your-key' > .env
```

## Examples

### Customer Service (Coordinator Pattern)

A coordinator agent routes customer queries to specialists:

```
coordinator
├── billing_agent (payments, invoices, subscriptions)
└── support_agent (errors, bugs, troubleshooting)
```

```bash
cargo run --bin customer_service
```

**Example prompts:**
- "I have a question about my last invoice" → Routes to `billing_agent`
- "The app keeps crashing" → Routes to `support_agent`
- "How do I upgrade my plan?" → Routes to `billing_agent`

### Hierarchical (Multi-Level Tree)

A 3-level agent hierarchy for content creation:

```
project_manager
└── content_creator
    ├── researcher
    └── writer
```

```bash
cargo run --bin hierarchical
```

**Example prompts:**
- "Create a blog post about AI in healthcare" → PM → Content Creator → Writer
- "Research electric vehicles" → PM → Content Creator → Researcher

## How It Works

1. **Parent agent** receives user message
2. LLM analyzes request and sub-agent descriptions
3. LLM calls `transfer_to_agent(agent_name="target")`
4. **Runner** detects transfer and invokes target agent
5. **Target agent** responds with the same user message

The transfer is seamless - the user sees a continuous conversation.

## Build All

```bash
cargo build
```
