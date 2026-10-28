# Guardrails

Input/output validation and content safety using `adk-guardrail`.

## Overview

Guardrails validate and transform agent inputs and outputs to ensure safety, compliance, and quality. Input guardrails screen a user message before it is saved or reaches a model, output guardrails screen everything a model turn shows the client and the session — the final response and any text written beside a tool call — and tool guardrails screen a tool call's arguments and its result. They can:

- Block harmful or off-topic content
- Redact PII (emails, phones, SSNs, card numbers, and optionally IP addresses)
- Enforce JSON schema on outputs
- Limit content length
- Redact or withhold a tool's result before the model sees it

## Installation

```toml
[dependencies]
adk-guardrail = "3.0.0"
```

The `schema` feature, on by default, provides `SchemaValidator`; build with
`default-features = false` to leave out the `jsonschema` dependency.

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
    Low,      // Passes; recorded as a warning
    Medium,   // Blocks once every guardrail in the set has run
    High,     // Blocks once every guardrail in the set has run
    Critical, // Blocks and stops the set at once (when `fail_fast` is true)
}
```

A `Low` failure never blocks. The content passes (transformed, if another guardrail
transformed it), the executor logs the failure at `warn` with the guardrail's name and severity,
and an agent records the failure on the output event it screened, under the event metadata key
`adk.guardrail.warnings` (`GUARDRAIL_WARNINGS_KEY`), as a JSON array of
`{"guardrail", "reason", "severity"}` objects. The client sees it on the event and the session
keeps it. A low-severity failure of an input guardrail is logged only.

## PII Redaction

Automatically detect and redact personally identifiable information:

```rust
use adk_guardrail::{PiiRedactor, PiiType};

// Default: emails, phones, SSNs, card numbers — not IP addresses
let redactor = PiiRedactor::new();

// Or select specific types; name `PiiType::IpAddress` to redact IP addresses
let redactor = PiiRedactor::with_types(&[
    PiiType::Email,
    PiiType::Phone,
    PiiType::Ssn,
    PiiType::CreditCard,
    PiiType::IpAddress,
]);

// Direct redaction
let (redacted, found_types) = redactor.redact("Email: test@example.com");
// redacted = "Email: [EMAIL REDACTED]"
// found_types = [PiiType::Email]
```

**Supported PII Types:**

| Type | Matches | Redaction | In `new()` |
|------|---------|-----------|------------|
| `Email` | `user@domain.com`; the top-level domain is letters only, so `user@domain.com\|ref` keeps `\|ref` | `[EMAIL REDACTED]` | Yes |
| `Phone` | `555-123-4567` | `[PHONE REDACTED]` | Yes |
| `Ssn` | `123-45-6789` | `[SSN REDACTED]` | Yes |
| `CreditCard` | A 13–19 digit card number that passes the Luhn check, as one run of digits or grouped 4-4-4-4 (`4111-1111-1111-1111`), 4-6-5 (`3782 822463 10005`), or 4-4-4-4-3 (`6205 5000 0000 0000 004`) | `[CREDIT CARD REDACTED]` | Yes |
| `IpAddress` | `192.168.1.1` | `[IP REDACTED]` | No |

A number that fails the Luhn check, such as a 16-digit order number, is not a card number and
passes unchanged.

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

| Constructor | Checks | Severity of a failure |
|-------------|--------|-----------------------|
| `harmful_content()` | Blocks `kill`, `murder`, `bomb`, `terrorist`, `malware`, `ransomware` | `Critical` |
| `harmful_content_strict()` | The same list plus `hack` and `exploit` | `Critical` |
| `blocked_keywords(keywords)` | Blocks each keyword, case-insensitively | `High` |
| `on_topic(topic, keywords)` | Requires at least one keyword | `Medium` |
| `max_length(max)` | Fails text longer than `max` bytes | `Medium` |

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
use adk_guardrail::{SchemaValidator, Severity};
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

Content with no JSON fails with the validator's severity (`High` unless set).

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

A `Critical` failure from a guardrail whose `fail_fast()` is `true` makes `run` return
`Err(GuardrailError::ValidationFailed { .. })` instead of a result.

`GuardrailExecutor::enforce(&guardrails, &content, phase, preamble)` turns the result into a
decision: a `Screening` with the content to continue with and the low-severity warnings, or a
`GuardrailError::Blocked` error. Agents screen through it.

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

`validate_preamble` screens text a model writes beside a tool call, or in a turn the provider
paused, and defaults to `validate`. Override it when a check describes the complete answer
rather than any text the user sees — see [Text beside a tool call](#text-beside-a-tool-call).

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
use adk_guardrail::{ContentFilter, GuardrailSet, PiiRedactor, SchemaValidator};
use serde_json::json;

let input_guardrails = GuardrailSet::new()
    .with(ContentFilter::harmful_content())
    .with(PiiRedactor::new());

let output_schema = json!({
    "type": "object",
    "properties": { "answer": { "type": "string" } },
    "required": ["answer"]
});
let output_guardrails = GuardrailSet::new()
    .with(SchemaValidator::new(&output_schema)?);

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .instruction("You are a helpful assistant.")
    .input_guardrails(input_guardrails)
    .output_guardrails(output_guardrails)
    .build()?;
```

### Text beside a tool call

A model turn that calls tools can also carry text — "Let me look that up", or a sentence that
repeats the user's details. The client receives that text and the session saves it, so output
guardrails screen it before either does, with the same outcomes as the final response:

| Outcome | Effect |
|---------|--------|
| Pass | The turn is delivered and its calls run |
| Transform | The text is rewritten in place; the calls are unchanged and run |
| Block | The run ends with a structured block error; nothing of the turn is sent or saved and its calls do not run |

- **Only the text is shown.** Guardrails screen the turn's text parts through
  `Guardrail::validate_preamble`, so a function call never reaches a guardrail and always
  survives a transform. The same applies to a turn the provider paused (Anthropic `pause_turn`).
- **Answer-shape checks do not apply.** `SchemaValidator` passes a preamble, since it is not the
  structured answer. `ContentFilter` applies its blocked keywords and maximum length, and skips
  required topics and the minimum length, which describe a complete answer.
- **Provider-native messages.** The OpenAI Responses API returns preamble text as a
  `commentary` message, which the adapter keeps as a native message part so it replays with its
  phase. Its text is screened as a text part and written back into the message, in the final
  response too.
- **Buffered delivery.** Output guardrails need the whole turn, so with output guardrails set an
  `SSE` or `Bidi` run delivers each model turn as one event instead of streaming partial text.
  The event's debug copy of the provider response (`gcp.vertex.agent.llm_response`) carries the
  screened content.

### When a guardrail blocks

A block ends the run with an `AdkError` a caller can tell apart from a failure:

| Field | Input block | Output block |
|-------|-------------|--------------|
| `component` | `Guardrail` | `Guardrail` |
| `category` | `InvalidInput` (HTTP 400, not retryable) | `InvalidInput` (HTTP 400, not retryable) |
| `code` | `guardrail.input_blocked` (`INPUT_BLOCKED_CODE`) | `guardrail.output_blocked` (`OUTPUT_BLOCKED_CODE`) |
| `details.metadata["guardrail"]` | Name of the blocking guardrail | Name of the blocking guardrail |
| `details.metadata["severity"]` | Its severity, such as `"High"` | Its severity |
| `details.metadata["phase"]` | `"input"` | `"output"` |
| `details.metadata["failures"]` | Every failure, `Low` ones included | Every failure |

The blocking guardrail is the first whose failure is above `Low`. The message keeps the form
`output guardrails blocked content: <name> (<severity>): <reason>`. The runner's save-time input
screening raises the same error.

```rust
use adk_guardrail::OUTPUT_BLOCKED_CODE;
use futures::StreamExt;

match stream.next().await {
    Some(Err(error)) if error.code == OUTPUT_BLOCKED_CODE => {
        let guardrail = error.details.metadata.get("guardrail");
        // Show a refusal instead of a server error.
    }
    _ => {}
}
```

## Tool Guardrails

A `ToolGuardrail` screens a call's arguments before the tool runs (`validate_call`) and its
result before the model and the session see it (`validate_result`). Both default to allowing,
so a guardrail implements only the side it checks. `applies_to(tool_name)` scopes a guardrail to
some tools; the built-in guardrails set it with `on_tools(...)`. A `ToolGuardrailSet` runs its
guardrails in order: revisions compose, and the first denial or withholding stops the set.

### Screening calls

`LlmAgentBuilder::tool_guardrails` runs `validate_call` on the governed path, after the
`ToolPolicy` and before confirmation (see
[Tool Authorization](tool-authorization.md#combining-mechanisms)):

| `validate_call` returns | Effect |
|-------------------------|--------|
| `ToolGuardrailResult::Allow` | The call proceeds |
| `ToolGuardrailResult::ReviseArgs { args, .. }` | The call proceeds with `args`, which the `ToolPolicy` checks again |
| `ToolGuardrailResult::Deny { reason, severity }` | The tool does not run; the model receives `Tool '<name>' blocked by guardrail '<guardrail>' (<severity>): <reason>` |

| Built-in | Denies |
|----------|--------|
| `DeniedArgumentPattern::new(name, regex, severity)?` | A call whose serialized arguments match `regex` |
| `PathAllowList::new(name, arg_names, allowed_roots)` | A call whose path argument is not absolute, contains `..`, or resolves (symlinks included) outside every allowed root; also a call that omits one of `arg_names`, unless `allow_missing()` is set. Severity `Critical` unless `with_severity` changes it |

Without `on_tools`, a `PathAllowList` applies to every tool, so a tool that takes no path
argument is denied too; a tool not named in `on_tools` is not checked at all.

```rust
use adk_guardrail::{DeniedArgumentPattern, PathAllowList, Severity, ToolGuardrailSet};

let tool_guardrails = ToolGuardrailSet::new()
    .with(
        PathAllowList::new("workspace", ["path", "source", "destination"], ["/srv/workspace"])
            .on_tools(["read_file", "move_file"])
            .allow_missing(),
    )
    .with(DeniedArgumentPattern::new("no-force-push", r"--force\b", Severity::High)?.on_tools(["run_command"]));
```

### Screening results

| `validate_result` returns | What the model and the session see |
|---------------------------|------------------------------------|
| `ToolResultGuardrailResult::Allow` | The result |
| `ToolResultGuardrailResult::Revise { result, .. }` | `result`, such as the result with fields redacted |
| `ToolResultGuardrailResult::Withhold { reason, .. }` | `{"status": "result_withheld", "error": "tool '<name>' ran, but its result was withheld: tool guardrail '<guardrail>' (<severity>): <reason>; do not call it again for this request."}` |

`ResultTextGuardrail` applies any content guardrail to every text value of a result, at any
depth, so the result keeps its JSON shape; a failure above `Low` withholds the result.

```rust
use adk_agent::LlmAgentBuilder;
use adk_guardrail::{ContentFilter, PiiRedactor, ResultTextGuardrail, ToolGuardrailSet};

// lookup_order: Arc<dyn Tool>

let tool_guardrails = ToolGuardrailSet::new()
    .with(ResultTextGuardrail::new(PiiRedactor::new()))
    .with(
        ResultTextGuardrail::new(ContentFilter::blocked_keywords(vec!["internal only".into()])?)
            .on_tools(["search_wiki"]),
    );

let agent = LlmAgentBuilder::new("support")
    .model(model)
    .tool(lookup_order)
    .tool_guardrails(tool_guardrails)
    .build()?;
```

`LlmAgent` runs the set on every result it is about to send the model — after the after-tool
callbacks and plugins, and on a tool's error response too — before the tool event reaches the
client or the session. A held call's pending response is not screened. A result that ends the
turn is then screened by the output guardrails as well.

A `ToolGuardrailSet` is also an `adk_core::ToolResultScreen`, which is how the realtime runtimes
take it (`tool_result_screen`); see [Tools in Realtime Sessions](../realtime/tools.md#screening-tool-results).

## Input Guardrails and the Session

The `Runner` screens each user message with the input guardrails of its agent tree before it
saves the message, so the session, and every later model request built from it, holds only
what the guardrails let through.

| Guardrail outcome | What the runner does |
|-------------------|----------------------|
| Pass | Saves the message unchanged, with `actions.input_screened` set |
| Transform, such as PII redaction | Saves the transformed message, with the marker set; this run and every later run see only that form |
| Block | Fails the run with the guardrail's error before anything is saved; no agent runs and no model sees the message |

A tree with no input guardrails saves the message unchanged and without the marker.

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
│  Model Turn         │
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  Output Guardrails  │ ← final text, and text beside a tool call (validate_preamble)
│  (parallel)         │
└─────────────────────┘
    │
    ▼ (rewritten, or blocked: nothing is sent or saved)
┌─────────────────────┐
│  Tool Call          │ ← tool guardrails screen the arguments (validate_call)
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  Tool Result        │ ← tool guardrails screen the result (validate_result)
└─────────────────────┘
    │
    ▼
Next model turn, or the final response
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
