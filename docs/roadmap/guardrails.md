# Guardrails Framework

*Priority: 🔴 P0 | Target: Q1 2025 | Effort: 4 weeks*

## Overview

Implement a guardrails system for validating agent inputs and outputs, matching OpenAI Agents SDK capabilities.

## Problem Statement

Currently, ADK-Rust lacks built-in validation for:
- Harmful/off-topic content filtering
- Output schema enforcement
- Cost/token limits
- PII detection and redaction

OpenAI Agents SDK provides first-class guardrails that run in parallel with agents and can break early on failures.

## Proposed Solution

### New Crate: `adk-guardrail`

```rust
use adk_guardrail::{Guardrail, GuardrailResult, ContentFilter, SchemaValidator};

// Define guardrails
let input_guardrails = vec![
    ContentFilter::harmful_content(),
    ContentFilter::off_topic("customer support"),
];

let output_guardrails = vec![
    SchemaValidator::json_schema(schema),
    ContentFilter::pii_redaction(),
];

// Attach to agent
let agent = LlmAgentBuilder::new("assistant")
    .input_guardrails(input_guardrails)
    .output_guardrails(output_guardrails)
    .build()?;
```

## Core Components

### 1. Guardrail Trait

```rust
#[async_trait]
pub trait Guardrail: Send + Sync {
    fn name(&self) -> &str;
    
    async fn validate(&self, content: &Content) -> GuardrailResult;
    
    /// Run in parallel with agent (early exit on failure)
    fn run_parallel(&self) -> bool { true }
}

pub enum GuardrailResult {
    Pass,
    Fail { reason: String, severity: Severity },
    Transform { new_content: Content }, // For redaction
}
```

### 2. Built-in Guardrails

| Guardrail | Description |
|-----------|-------------|
| `ContentFilter::harmful_content()` | Block violence, hate speech, etc. |
| `ContentFilter::off_topic(topic)` | Ensure relevance to topic |
| `ContentFilter::pii_redaction()` | Redact SSN, emails, phones |
| `SchemaValidator::json_schema(schema)` | Enforce JSON output format |
| `CostGuardrail::max_tokens(limit)` | Token limits per request |
| `CostGuardrail::max_cost(dollars)` | Cost limits per request |
| `LengthGuardrail::max_chars(limit)` | Character limits |
| `LanguageGuardrail::only(languages)` | Language restrictions |

### 3. Custom Guardrails

```rust
struct MyGuardrail;

#[async_trait]
impl Guardrail for MyGuardrail {
    fn name(&self) -> &str { "my-guardrail" }
    
    async fn validate(&self, content: &Content) -> GuardrailResult {
        // Custom validation logic
        if content.text().contains("forbidden") {
            GuardrailResult::Fail {
                reason: "Contains forbidden word".into(),
                severity: Severity::High,
            }
        } else {
            GuardrailResult::Pass
        }
    }
}
```

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                       Request                            │
└─────────────────────┬───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│              Input Guardrails (Parallel)                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │ Content  │  │   PII    │  │  Schema  │   ...        │
│  │  Filter  │  │ Redactor │  │Validator │              │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘              │
│       │             │             │                     │
│       └─────────────┼─────────────┘                     │
│                     │ (early exit on fail)              │
└─────────────────────┼───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│                    LLM Agent                             │
└─────────────────────┬───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│             Output Guardrails (Parallel)                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │  Schema  │  │   Cost   │  │ Language │   ...        │
│  │Validator │  │  Guard   │  │  Guard   │              │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘              │
│       │             │             │                     │
│       └─────────────┼─────────────┘                     │
│                     │ (early exit on fail)              │
└─────────────────────┼───────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────┐
│                      Response                            │
└─────────────────────────────────────────────────────────┘
```

## Implementation Plan

### Week 1: Core Framework
- [ ] Create `adk-guardrail` crate
- [ ] Define `Guardrail` trait
- [ ] Implement parallel execution engine
- [ ] Add to `LlmAgentBuilder`

### Week 2: Content Filters
- [ ] `ContentFilter::harmful_content()` (LLM-based)
- [ ] `ContentFilter::off_topic()`
- [ ] `ContentFilter::pii_redaction()` (regex + LLM)
- [ ] Unit tests

### Week 3: Schema & Cost
- [ ] `SchemaValidator` (JSON Schema)
- [ ] `CostGuardrail` (token counting)
- [ ] `LengthGuardrail`
- [ ] Integration tests

### Week 4: Documentation & Examples
- [ ] API documentation
- [ ] `guardrail_basic` example
- [ ] `guardrail_custom` example
- [ ] Update README

## Success Metrics

- [ ] All guardrails run in <50ms overhead
- [ ] 95% accuracy on harmful content detection
- [ ] PII redaction catches SSN, email, phone, credit cards
- [ ] Schema validation matches JSON Schema spec

## Dependencies

- `jsonschema` for JSON Schema validation
- `regex` for PII patterns
- Gemini/OpenAI for LLM-based filtering

## Related

- [OpenAI Agents SDK Guardrails](https://openai.github.io/openai-agents-python/guardrails/)
- [adk-ui validation.rs](../adk-ui/src/validation.rs) - Existing validation pattern
