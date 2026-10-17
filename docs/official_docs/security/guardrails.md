# Guardrails

Input/output validation and content safety using `adk-guardrail`.

## Overview

Guardrails validate and transform agent inputs and outputs to ensure safety, compliance, and quality. Input guardrails screen a user message before it is saved or reaches a model, output guardrails screen a response before it is returned, and both can:

- Block harmful or off-topic content
- Redact PII (emails, phones, SSNs, credit cards)
- Enforce JSON schema on outputs
- Limit content length

## Installation

```toml
[dependencies]
adk-guardrail = "3.0.0"

# For JSON schema validation
adk-guardrail = { version = "3.0.0", features = ["schema"] }
```

## Core Concepts

### GuardrailResult

Every guardrail returns one of three results:

```rust
pub enum GuardrailResult {
    Pass,                                    // Content is valid
    Fail { reason: String, severity: Severity },  // Content rejected
    Transform { new_content: Content, reason: String },  // Content modified
}
```

### Severity Levels

```rust
pub enum Severity {
    Low,      // Warning only, doesn't block
    Medium,   // Blocks but continues other checks
    High,     // Blocks immediately
    Critical, // Blocks and fails fast
}
```

## PII Redaction

Automatically detect and redact personally identifiable information:

```rust
use adk_guardrail::{PiiRedactor, PiiType};

// Default: emails, phones, SSNs, credit cards
let redactor = PiiRedactor::new();

// Or select specific types
let redactor = PiiRedactor::with_types(&[
    PiiType::Email,
    PiiType::Phone,
]);

// Direct redaction
let (redacted, found_types) = redactor.redact("Email: test@example.com");
// redacted = "Email: [EMAIL REDACTED]"
// found_types = [PiiType::Email]
```

**Supported PII Types:**

| Type | Pattern | Redaction |
|------|---------|-----------|
| `Email` | `user@domain.com` | `[EMAIL REDACTED]` |
| `Phone` | `555-123-4567` | `[PHONE REDACTED]` |
| `Ssn` | `123-45-6789` | `[SSN REDACTED]` |
| `CreditCard` | `4111-1111-1111-1111` | `[CREDIT CARD REDACTED]` |
| `IpAddress` | `192.168.1.1` | `[IP REDACTED]` |

## Content Filtering

Block harmful content or enforce topic constraints:

```rust
use adk_guardrail::ContentFilter;

// Block harmful content patterns (developer-friendly: excludes "hack"/"exploit")
let filter = ContentFilter::harmful_content();

// Strict variant that also blocks "hack" and "exploit"
let filter = ContentFilter::harmful_content_strict();

// Block specific keywords. Returns an error when the list cannot be compiled
// (for example, past the regex size limit) rather than a filter that blocks nothing.
let filter = ContentFilter::blocked_keywords(vec![
    "forbidden".into(),
    "banned".into(),
])?;

// Enforce topic relevance
let filter = ContentFilter::on_topic("cooking", vec![
    "recipe".into(),
    "cook".into(),
    "bake".into(),
]);

// Limit content length
let filter = ContentFilter::max_length(1000);
```

### Custom Content Filter

```rust
use adk_guardrail::{ContentFilter, ContentFilterConfig, Severity};

let config = ContentFilterConfig {
    blocked_keywords: vec!["spam".into()],
    required_topics: vec!["rust".into(), "programming".into()],
    max_length: Some(5000),
    min_length: Some(10),
    severity: Severity::High,
};

let filter = ContentFilter::new("custom_filter", config)?;
```

## Schema Validation

Enforce JSON schema on agent outputs (requires `schema` feature):

```rust
use adk_guardrail::SchemaValidator;
use serde_json::json;

let schema = json!({
    "type": "object",
    "properties": {
        "name": { "type": "string" },
        "age": { "type": "integer", "minimum": 0 }
    },
    "required": ["name"]
});

let validator = SchemaValidator::new(&schema)?
    .with_name("user_schema")
    .with_severity(Severity::High);
```

The validator extracts JSON from:
- Raw JSON text
- Markdown code blocks (` ```json ... ``` `)

## GuardrailSet

Combine multiple guardrails:

```rust
use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};

let guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(ContentFilter::max_length(5000))
    .with(PiiRedactor::new());
```

## GuardrailExecutor

Run guardrails and get detailed results:

```rust
use adk_guardrail::{GuardrailExecutor, GuardrailSet, PiiRedactor};
use adk_core::Content;

let guardrails = GuardrailSet::new()
    .with(PiiRedactor::new());

let content = Content::new("user")
    .with_text("Contact: test@example.com");

let result = GuardrailExecutor::run(&guardrails, &content).await?;

if result.passed {
    // Use transformed content if available
    let final_content = result.transformed_content.unwrap_or(content);
    println!("Content passed validation");
} else {
    for (name, reason, severity) in &result.failures {
        println!("Guardrail '{}' failed: {} ({:?})", name, reason, severity);
    }
}
```

### ExecutionResult

```rust
pub struct ExecutionResult {
    pub passed: bool,                              // Overall pass/fail
    pub transformed_content: Option<Content>,      // Modified content (if any)
    pub failures: Vec<(String, String, Severity)>, // (name, reason, severity)
}
```

## Custom Guardrails

Implement the `Guardrail` trait:

```rust
use adk_guardrail::{Guardrail, GuardrailResult, Severity};
use adk_core::Content;
use async_trait::async_trait;

pub struct ProfanityFilter {
    words: Vec<String>,
}

#[async_trait]
impl Guardrail for ProfanityFilter {
    fn name(&self) -> &str {
        "profanity_filter"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts
            .iter()
            .filter_map(|p| p.text())
            .collect();

        for word in &self.words {
            if text.to_lowercase().contains(word) {
                return GuardrailResult::Fail {
                    reason: format!("Contains profanity: {}", word),
                    severity: Severity::High,
                };
            }
        }

        GuardrailResult::Pass
    }

    // Run in parallel with other guardrails (default: true)
    fn run_parallel(&self) -> bool {
        true
    }

    // Fail fast on this guardrail's failure (default: true)
    fn fail_fast(&self) -> bool {
        true
    }
}
```

## Integration with Agents

Guardrails integrate with `LlmAgentBuilder`:

Configured input guardrails run before model execution, and configured output guardrails run on generated responses before they are returned to the caller.

A tool result that ends the turn, because the tool set `skip_summarization` or `escalate`, is
the final response, so output guardrails screen it too, before the client or the session sees
it. Each function response is screened as its JSON text: a block ends the run with an error and
nothing of the result is sent or saved, and a transformed text replaces the response, parsed
back to JSON when it still parses and wrapped as `{"result": "<text>"}` when it does not.

> **Note:** `LlmAgentBuilder` accepts these sets only when `adk-agent` is built with its `guardrails` feature. The `adk-rust` `guardrail` feature enables it.

```rust
use adk_agent::LlmAgentBuilder;
use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};

let input_guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(PiiRedactor::new());

let output_guardrails = GuardrailSet::new()
    .with(SchemaValidator::new(&output_schema)?);

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .instruction("You are a helpful assistant.")
    .input_guardrails(input_guardrails)
    .output_guardrails(output_guardrails)
    .build()?;
```

## Input Guardrails and the Session

The `Runner` screens each user message with the input guardrails of its agent tree before it
saves the message, so the session, and every later model request built from it, holds only
what the guardrails let through.

| Guardrail outcome | What the runner does |
|-------------------|----------------------|
| Pass | Saves the message unchanged, with `actions.input_screened` set |
| Transform, such as PII redaction | Saves the transformed message, with the marker set; this run and every later run see only that form |
| Block | Fails the run with the guardrail's error before anything is saved; no agent runs and no model sees the message |

- **Agent tree.** Every agent in the tree screens the message: an `LlmAgent` or `CodeActAgent`
  applies its own input guardrails, then its sub-agents' (`Agent::screen_user_input`). A
  guardrail on a nested agent, such as a `SequentialAgent` step or a transfer target, therefore
  applies before the first agent sees the message, because every agent in the session can see a
  saved message in a later run. An agent that wraps another agent without listing it in
  `sub_agents()` delegates `screen_user_input` to it.
- **Unscreened saved messages.** Before each run, the runner screens every saved user message
  without the `input_screened` marker — saved by an earlier release, or appended to the session
  directly — in the run's view of the session; the store is not rewritten. A transformed message
  is replaced in that view and a blocked one is withheld from it. A run that brings no message
  of its own (an empty resume turn, or a durable worker or managed runtime recovering a run)
  continues the latest saved message, so a block on that message fails the run without a model
  call.
- **Empty turns.** A blank user turn, which only resumes held tool calls, and user-role
  function responses carry no input and are not screened.
- **Agents still screen.** `LlmAgent` and `CodeActAgent` keep applying their input guardrails
  to the message they run with, so an agent run outside a `Runner` is screened too. A guardrail
  that returns `Transform` therefore sees its own output again and must be idempotent, as
  `PiiRedactor` is.
- **Every backend.** The marker is a field of `EventActions`, which every session backend
  persists.

## Execution Flow

```
User Input
    │
    ▼
┌─────────────────────┐
│  Input Guardrails   │ ← every agent in the tree: PII redaction, content filtering
│  (parallel)         │
└─────────────────────┘
    │
    ▼ (transformed, or blocked: nothing is saved)
┌─────────────────────┐
│  Session            │ ← saves only the screened message
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  Agent Execution    │
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  Output Guardrails  │ ← Schema validation, safety checks
│  (parallel)         │
└─────────────────────┘
    │
    ▼
Final Response
```

## Examples

```bash
cargo check -p adk-guardrail
cargo check -p adk-rust --no-default-features --features guardrail
```

## Best Practices

| Practice | Description |
|----------|-------------|
| **Layer guardrails** | Use input guardrails for safety, output for quality |
| **PII on input** | Redact PII before it reaches the model |
| **Schema on output** | Validate structured outputs with JSON schema |
| **Appropriate severity** | Use Critical sparingly, Low for warnings |
| **Test thoroughly** | Guardrails are security-critical code |

---

**Previous**: [← Access Control](access-control.md) | **Next**: [Memory →](memory.md)
