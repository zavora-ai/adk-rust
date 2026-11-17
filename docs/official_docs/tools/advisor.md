# Advisor Tool

`AdvisorTool` lets an agent consult a stronger or different model for advice in the middle of
a task. The agent runs on a fast model and calls the tool with a question when it reaches a hard
decision; the advisor reads the question, the agent's context and the recent conversation, and
returns advice as the tool's result. The agent acts on it.

It is the provider-neutral counterpart of Google ADK's `ModelConsultTool` and of Claude's
advisor tool, and any `Llm` can be the advisor.

## Installation

`AdvisorTool` is in `adk-tool` and needs no feature of its own:

```toml
[dependencies]
adk-agent = "3.0.0"
adk-model = { version = "3.0.0", features = ["anthropic"] }
adk-tool = "3.0.0"
```

## Usage

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_tool::AdvisorTool;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = std::env::var("ANTHROPIC_API_KEY")?;
    let worker = AnthropicClient::new(AnthropicConfig::new(key.clone(), "claude-haiku-5-5"))?;
    let advisor = AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-5-5"))?;

    let consult = AdvisorTool::builder(Arc::new(advisor))
        .max_consultations_per_run(2)
        .max_output_tokens(2_048)
        .build();

    let _agent = LlmAgentBuilder::new("support")
        .instruction("Resolve the customer's issue with your tools.")
        .model(Arc::new(worker))
        .tool(Arc::new(consult))
        .build()?;
    Ok(())
}
```

The agent's model sees a tool named `consult_advisor` with two arguments:

| Argument | Required | Meaning |
|----------|----------|---------|
| `question` | yes | What the agent needs advice on: the decision, the problem, or the plan to check |
| `context` | no | Facts the advisor needs that the conversation may not show |

The tool's description tells the agent to consult before a hard or irreversible decision, when
it is stuck, and before it reports a complex task as done. Replace it with
`AdvisorToolBuilder::description` to change when the agent consults.

## What the advisor sees

The advisor's request has two contents and no tools, so the advisor can only answer:

1. Its system instruction (`AdvisorToolBuilder::instruction` replaces the default, which asks
   for a recommended decision, ordered next steps and the risks to check).
2. The handover: the consulting agent's name, the question, the context, and the agent's
   conversation rendered as a transcript — text, tool calls and tool results, each call or result
   cut at 2,000 characters, thinking left out — of at most 24,000 characters, the most recent
   kept.

`history_chars(0)` sends only the question and context.

## Result

The tool returns a JSON object:

| `status` | Fields | When | Counted |
|----------|--------|------|---------|
| `ok` | `advice`, `advisorModel`, `consultations`, `usage` | The advisor answered; advice cut off at `max_output_tokens` ends with a note saying so | Yes |
| `limit_reached` | `message`, `consultations` | The run has used its consultations; the advisor is not called | — |
| `error` | `error`, `message`, `advisorModel`, `consultations` | The advisor failed (`error` is the provider's code), refused (`tool.advisor.refused`), or returned no text (`tool.advisor.empty`) | Only when the advisor responded |
| `invalid_request` | `message` | `question` is missing or empty | No |

```json
{
  "status": "ok",
  "advice": "Do not release ORD-42 yet: the fraud hold needs a manual review first...",
  "advisorModel": "claude-sonnet-5-5",
  "consultations": { "used": 1, "max": 2, "remaining": 1 },
  "usage": { "inputTokens": 812, "outputTokens": 164, "thinkingTokens": null, "totalTokens": 976, "costUsd": 0.003264 }
}
```

The call and its result are a function call and a function response in the run's event stream,
like any tool.

## Governance

| Control | Behaviour |
|---------|-----------|
| Governed tool path | The call is authorized like any tool call: policy, approvals and the signed audit record apply before it runs. The tool is read-only |
| Run budget | The advisor's call is one model call, and its tokens and cost count, against the run's `RunBudget` |
| Spend ledger | The call reserves and commits under the advisor's provider, attributed to `advisor:{agent}`; org and vendor caps cover it. `spend_agent` sets another attribution |
| Per-run cap | `max_consultations_per_run` (3 by default) per root invocation; `0` turns consultation off |
| Kill switch | A frozen run fails the call with `governance.frozen` before the advisor is called, and a freeze during the consultation ends it |
| Refusals by the run | A budget or spend-ledger refusal fails the call with its own error; the consultation is not counted |

A cap keyed to the consulting agent's own id (`urn:adk:agent:{app}/{agent}`) does not cover the
advisor's spend; key a cap to `advisor:{agent}`, or to the org or vendor, to bound it.

Consultation counts live in the tool, so a run resumed in another process starts its count
again; the run budget still bounds it.

## Configuration

| Builder method | Default | Effect |
|----------------|---------|--------|
| `name` | `consult_advisor` | The tool's name |
| `description` | When to consult | What the agent's model reads |
| `instruction` | Concise, actionable guidance | The advisor's system instruction |
| `max_consultations_per_run` | 3 | Consultations per run |
| `max_output_tokens` | The advisor client's | Caps each answer |
| `effort` | The advisor client's | The advisor's reasoning effort |
| `history_chars` | 24,000 | Conversation shown to the advisor |
| `spend_agent` | `advisor:{agent}` | Spend-ledger attribution |

## Choosing a pairing

The advisor pays off when it is clearly stronger than the agent and the agent actually consults.
Pair a fast agent model with a frontier advisor — `claude-haiku-5-5` consulting
`claude-sonnet-5-5`, or a Gemini Flash model consulting a Pro one — and compare against the
stronger model alone at a lower effort before you adopt it.
