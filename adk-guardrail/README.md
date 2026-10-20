# adk-guardrail

Guardrails framework for ADK agents - input/output validation, content filtering, PII redaction.

[![Crates.io](https://img.shields.io/crates/v/adk-guardrail.svg)](https://crates.io/crates/adk-guardrail)
[![Documentation](https://docs.rs/adk-guardrail/badge.svg)](https://docs.rs/adk-guardrail)
[![License](https://img.shields.io/crates/l/adk-guardrail.svg)](../LICENSE)

## Overview

`adk-guardrail` provides safety and validation for AI agents in the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework:

- **PiiRedactor** - Detect and redact PII (Email, Phone, SSN, Credit Card, IP Address)
- **ContentFilter** - Block harmful content, off-topic responses, max length
- **SchemaValidator** - Validate JSON output against schemas
- **GuardrailSet** / **GuardrailExecutor** - Compose content guardrails and enforce them
- **ToolGuardrail** - Allow, deny, or revise a tool call's arguments, and deliver, revise, or
  withhold its result (`validate_result`)
- **ResultTextGuardrail** - Apply any content guardrail, such as `PiiRedactor`, to every text
  value of a tool result
- **DeniedArgumentPattern** / **PathAllowList** - Refuse tool calls whose arguments match a
  pattern, or confine path-valued arguments to a set of roots

Output guardrails screen the final response and any text a model writes beside a tool call
(`Guardrail::validate_preamble`). A block is an `AdkError` with category `InvalidInput` and code
`guardrail.input_blocked` or `guardrail.output_blocked`, naming the guardrail and its severity
in `details.metadata`. A `Low` failure passes and is recorded on the output event under
`adk.guardrail.warnings`.

## Installation

```toml
[dependencies]
adk-guardrail = "3.0.0"
```

| Feature | Default | Enables |
|---------|---------|---------|
| `schema` | yes | `SchemaValidator` through `jsonschema` |

## Quick Start

### PII Redaction

```rust
use adk_core::Content;
use adk_guardrail::{Guardrail, GuardrailResult, PiiRedactor};

#[tokio::main]
async fn main() {
    let redactor = PiiRedactor::new();
    let content = Content::new("user").with_text("Contact me at test@example.com");

    match redactor.validate(&content).await {
        GuardrailResult::Transform { new_content, .. } => {
            assert_eq!(new_content.parts[0].text(), Some("Contact me at [EMAIL REDACTED]"));
        }
        other => panic!("expected a redaction, got {other:?}"),
    }
}
```

### Content Filtering

```rust
use adk_guardrail::ContentFilter;

fn filters() -> adk_guardrail::Result<Vec<ContentFilter>> {
    Ok(vec![
        // Block harmful content
        ContentFilter::harmful_content(),
        // Keep responses on-topic
        ContentFilter::on_topic("cooking", vec!["recipe".into(), "bake".into()]),
        // Limit response length
        ContentFilter::max_length(1000),
        // Block specific keywords (an error, not an open filter, if the list cannot compile)
        ContentFilter::blocked_keywords(vec!["forbidden".into()])?,
    ])
}
```

### Agent Integration

Requires `guardrails` feature on `adk-agent`:

```toml
adk-agent = { version = "3.0.0", features = ["guardrails"] }
```

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};
use adk_guardrail::{ContentFilter, GuardrailSet, PiiRedactor};

fn guarded(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    let input_guardrails = GuardrailSet::new()
        .with(ContentFilter::harmful_content())
        .with(PiiRedactor::new());

    LlmAgentBuilder::new("assistant").model(model).input_guardrails(input_guardrails).build()
}
```

### Tool Result Guardrails

```rust
use adk_guardrail::{
    DeniedArgumentPattern, PathAllowList, PiiRedactor, ResultTextGuardrail, Severity,
    ToolGuardrailSet,
};

fn tool_guardrails() -> Result<ToolGuardrailSet, Box<dyn std::error::Error>> {
    Ok(ToolGuardrailSet::new()
        // Confine the `path` argument of `plist_write` to one directory
        .with(
            PathAllowList::new("agents-only", ["path"], ["/Users/me/Library/LaunchAgents"])
                .on_tools(["plist_write"]),
        )
        // Refuse shell calls that pipe to a shell
        .with(
            DeniedArgumentPattern::new("no-pipe-to-sh", r"\|\s*sh\b", Severity::High)?
                .on_tools(["run_command"]),
        )
        // Redact PII in every tool result before the model and the session see it
        .with(ResultTextGuardrail::new(PiiRedactor::new())))
}
```

Pass the set to `LlmAgentBuilder::tool_guardrails`, or to a realtime runtime as its tool-result screen. Tool guardrails run in order and revisions compose; the first denial stops evaluation and is reported to the model as the tool's result. `PathAllowList` denies a call that omits a configured path argument (`allow_missing()` permits it), and applies to every tool unless `on_tools` names them.

## Guardrail Results

Each guardrail returns one of three results:

| Result | Description |
|--------|-------------|
| `Pass` | Content is valid, continue execution |
| `Fail` | Content is invalid, block with reason and severity |
| `Transform` | Content modified (e.g., PII redacted), continue with new content |

## Severity Levels

| Level | Effect |
|-------|--------|
| `Low` | The content passes; the failure is logged at `warn` and recorded under `adk.guardrail.warnings` (`GUARDRAIL_WARNINGS_KEY`) |
| `Medium`, `High` | The content is blocked after every guardrail has run |
| `Critical` | The content is blocked, and evaluation stops at once when the guardrail's `fail_fast()` is `true` (the default) |

`GuardrailExecutor::enforce` returns the screened content and its warnings, or `GuardrailError::Blocked { phase, failures }`.

## Built-in Guardrails

### PiiRedactor

Detects and redacts personally identifiable information:

- `Email` → `[EMAIL REDACTED]`
- `Phone` → `[PHONE REDACTED]`
- `Ssn` → `[SSN REDACTED]`
- `CreditCard` → `[CREDIT CARD REDACTED]`
- `IpAddress` → `[IP REDACTED]`

### ContentFilter

Validates content against rules:

- `harmful_content()` - Blocks common harmful patterns
- `on_topic(topic, keywords)` - Ensures topic relevance
- `max_length(n)` - Limits content length
- `blocked_keywords(list)` - Blocks specific words

### SchemaValidator

Validates JSON output against a schema (requires `schema` feature):

```rust
use adk_guardrail::SchemaValidator;
use serde_json::json;

fn validator() -> adk_guardrail::Result<SchemaValidator> {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" }
        },
        "required": ["name"]
    });

    Ok(SchemaValidator::new(&schema)?.with_name("user_schema"))
}
```

## Execution

- Guardrails whose `run_parallel()` is `true` (the default) run concurrently on the same input; when more than one of them transforms it, only the last transform in set order is kept. The rest run in order afterwards, each on the previous guardrail's output (`PiiRedactor` runs in this group).
- `LlmAgentBuilder::input_guardrails`, `output_guardrails`, and `tool_guardrails` attach sets to an agent (`adk-agent`'s `guardrails` feature).
- Guardrail reasons reach the caller and the model; logs carry them only when `RunConfig::record_payloads` is set.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| A block is an `AdkError` with `ErrorComponent::Guardrail`, `ErrorCategory::InvalidInput`, and code `guardrail.input_blocked` / `guardrail.output_blocked` | Match the codes instead of `agent.legacy`; `GuardrailError` gains `Blocked { phase, failures }` |
| `PathAllowList` denies a call that omits a configured path argument | `allow_missing()` restores the old behaviour |
| `ContentFilter::new` and `ContentFilter::blocked_keywords` return `Result` | Handle `GuardrailError::Regex` |
| `ToolGuardrail::validate_result`, `ResultTextGuardrail`, and `validate_preamble` are new; `validate_call` defaults to allowing | — |
| `Low` findings pass and are recorded as warnings | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations with guardrail support
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
