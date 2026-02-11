# adk-guardrail

Guardrails framework for ADK agents - input/output validation, content filtering, PII redaction.

[![Crates.io](https://img.shields.io/crates/v/adk-guardrail.svg)](https://crates.io/crates/adk-guardrail)
[![Documentation](https://docs.rs/adk-guardrail/badge.svg)](https://docs.rs/adk-guardrail)
[![License](https://img.shields.io/crates/l/adk-guardrail.svg)](LICENSE)

## Overview

`adk-guardrail` provides safety and validation for AI agents in the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework:

- **PiiRedactor** - Detect and redact PII (Email, Phone, SSN, Credit Card, IP Address)
- **ContentFilter** - Block harmful content, off-topic responses, max length
- **SchemaValidator** - Validate JSON output against schemas
- **GuardrailSet** - Compose multiple guardrails with parallel execution

## Installation

```toml
[dependencies]
adk-guardrail = "0.3.0"

# With JSON schema validation (default)
adk-guardrail = { version = "0.3.0", features = ["schema"] }
```

## Quick Start

### PII Redaction

```rust
use adk_guardrail::{PiiRedactor, Guardrail};
use adk_core::Content;

let redactor = PiiRedactor::new();
let content = Content::new("user").with_text("Contact me at test@example.com");
let result = redactor.validate(&content).await;

// Result is Transform with "[EMAIL REDACTED]"
```

### Content Filtering

```rust
use adk_guardrail::ContentFilter;

// Block harmful content
let filter = ContentFilter::harmful_content();

// Keep responses on-topic
let filter = ContentFilter::on_topic("cooking", vec!["recipe".into(), "bake".into()]);

// Limit response length
let filter = ContentFilter::max_length(1000);

// Block specific keywords
let filter = ContentFilter::blocked_keywords(vec!["forbidden".into()]);
```

### Agent Integration

Requires `guardrails` feature on `adk-agent`:

```toml
adk-agent = { version = "0.3.0", features = ["guardrails"] }
```

```rust
use adk_agent::LlmAgentBuilder;
use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};

let input_guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(PiiRedactor::new());

let agent = LlmAgentBuilder::new("assistant")
    .input_guardrails(input_guardrails)
    .build()?;
```

## Guardrail Results

Each guardrail returns one of three results:

| Result | Description |
|--------|-------------|
| `Pass` | Content is valid, continue execution |
| `Fail` | Content is invalid, block with reason and severity |
| `Transform` | Content modified (e.g., PII redacted), continue with new content |

## Severity Levels

| Level | Description |
|-------|-------------|
| `Low` | Minor issue, may continue |
| `Medium` | Moderate issue, should review |
| `High` | Serious issue, should block |
| `Critical` | Dangerous content, must block |

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

let schema = json!({
    "type": "object",
    "properties": {
        "name": { "type": "string" },
        "age": { "type": "integer" }
    },
    "required": ["name"]
});

let validator = SchemaValidator::new("user_schema", schema)?;
```

## Features

- Parallel guardrail execution with early exit on failure
- Composable with `GuardrailSet` builder pattern
- Integration with `LlmAgentBuilder` via `input_guardrails()` / `output_guardrails()`
- Async validation with the `Guardrail` trait

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations with guardrail support
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
