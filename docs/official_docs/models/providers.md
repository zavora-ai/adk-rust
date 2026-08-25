# Model Providers (Cloud)

ADK-Rust supports multiple cloud LLM providers through the `adk-model` crate. All providers implement the `Llm` trait, making them interchangeable in your agents.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Cloud Model Providers                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   • Gemini (Google)    ⭐ Default    - Multimodal, large context    │
│   • OpenAI (GPT-5)    🔥 Popular    - Best ecosystem               │
│   • Anthropic (Claude) 🧠 Smart      - Best reasoning               │
│   • DeepSeek           💭 Thinking   - Chain-of-thought, cheap      │
│   • Groq               ⚡ Ultra-Fast  - Fastest inference           │
│                                                                     │
│   For local/offline models, see:                                    │
│   • Ollama     → ollama.md                                          │
│   • mistral.rs → mistralrs.md                                       │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

## Quick Comparison

| Provider | Best For | Speed | Cost | Key Feature |
|----------|----------|-------|------|-------------|
| **Gemini** | General use | ⚡⚡⚡ | 💰 | Multimodal, large context, thinking |
| **OpenAI** | Reliability | ⚡⚡ | 💰💰 | Best ecosystem |
| **Anthropic** | Complex reasoning | ⚡⚡ | 💰💰 | Safest, most thoughtful |
| **DeepSeek** | Chain-of-thought | ⚡⚡ | 💰 | Thinking mode, cheap |
| **Groq** | Speed-critical | ⚡⚡⚡⚡ | 💰 | Fastest inference |

---

## Step 1: Installation

Add the providers you need to your `Cargo.toml`:

```toml
[dependencies]
# Pick one or more providers:
adk-model = { version = "2.1.0", features = ["gemini"] }        # Google Gemini (default)
adk-model = { version = "2.1.0", features = ["openai"] }        # OpenAI GPT-5
adk-model = { version = "2.1.0", features = ["anthropic"] }     # Anthropic Claude
adk-model = { version = "2.1.0", features = ["deepseek"] }      # DeepSeek
adk-model = { version = "2.1.0", features = ["groq"] }          # Groq (ultra-fast)

# Or all cloud providers at once:
adk-model = { version = "2.1.0", features = ["all-providers"] }
```

## Step 2: Set Your API Key

```bash
export GOOGLE_API_KEY="your-key"      # Gemini
export OPENAI_API_KEY="your-key"      # OpenAI
export ANTHROPIC_API_KEY="your-key"   # Anthropic
export DEEPSEEK_API_KEY="your-key"    # DeepSeek
export GROQ_API_KEY="your-key"        # Groq
```

## Schema Normalization

Each provider automatically normalizes MCP tool schemas at request time. You don't need to do anything — it works transparently. But here's what happens under the hood:

| Provider | Schema Adapter | Behavior |
|----------|---------------|----------|
| Gemini | `GeminiSchemaAdapter` | Aggressive: resolves `$ref`, collapses combiners, strips unsupported keywords |
| OpenAI (strict) | `OpenAiStrictSchemaAdapter` | Preserves structure, adds `additionalProperties: false` |
| OpenAI | `OpenAiSchemaAdapter` | Minimal safe fixes |
| Anthropic | `AnthropicSchemaAdapter` | Near pass-through |
| DeepSeek | `GenericSchemaAdapter` | Conservative safe transforms |
| Ollama | `GenericSchemaAdapter` | Conservative safe transforms |

Access the adapter programmatically via the `Llm` trait:

```rust
use adk_core::{Llm, SchemaAdapter};

let adapter = model.schema_adapter();
let normalized = adapter.normalize_schema(raw_schema);
```

See [Schema Normalization](../tools/schema-normalization.md) for full documentation.

---

## Gemini (Google) ⭐ Default

> **Best for**: General purpose, multimodal tasks, large documents
> 
> **Key highlights**:
> - 🖼️ Native multimodal (images, video, audio, PDF)
> - 📚 Up to 2M token context window
> - 🧠 Thinking mode: level-based (Gemini 3) and budget-based (Gemini 2.5) with thought signatures
> - 💰 Competitive pricing
> - ⚡ Fast inference

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

    let agent = LlmAgentBuilder::new("gemini_assistant")
        .description("Gemini-powered assistant")
        .instruction("You are a helpful assistant powered by Google Gemini. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `gemini-3.7-flash` | Default balanced agent model | 1M tokens |
| `gemini-3.6-flash` | Previous balanced generation | 1M tokens |
| `gemini-3.5-flash-lite` | Cost-efficient routing and high-volume tasks | 1M tokens |
| `gemini-3.1-pro-preview` | Advanced preview reasoning | 2M tokens |

### Thinking Mode

Gemini 3 models support level-based thinking, while Gemini 2.5 uses budget-based thinking. When using thinking mode with function calling, Gemini 2.5+ and 3.x models return `thoughtSignature` values that must be echoed back in subsequent turns to preserve reasoning context. ADK-Rust handles this automatically — signatures are serialized when present and omitted when `None`.

```rust
use adk_gemini::{Gemini, ThinkingLevel};

// Gemini 3: level-based thinking
let response = client.generate_content()
    .with_user_message("Solve this step by step")
    .with_thinking_level(ThinkingLevel::High)
    .with_thoughts_included(true)
    .execute().await?;

// Gemini 2.5: budget-based thinking
let response = client.generate_content()
    .with_user_message("Solve this step by step")
    .with_thinking_budget(2048)
    .with_thoughts_included(true)
    .execute().await?;
```

### Example Output

```
👤 User: What's in this image? [uploads photo of a cat]

🤖 Gemini: I can see a fluffy orange tabby cat sitting on a windowsill. 
The cat appears to be looking outside, with sunlight illuminating its fur. 
It has green eyes and distinctive striped markings typical of tabby cats.
```

---

## OpenAI (GPT-5) 🔥 Popular

> **Best for**: Production apps, reliable performance, broad capabilities
> 
> **Key highlights**:
> - 🏆 Industry standard
> - 🔧 Excellent tool/function calling
> - 📖 Best documentation & ecosystem
> - 🎯 Consistent, predictable outputs
> - 📋 **Structured output** with JSON schema enforcement
> - 🧠 **Reasoning effort** control for GPT-5.6 reasoning models
> - 🆕 **[Responses API](./openai-responses.md)** — dedicated client for `/v1/responses` with reasoning summaries, built-in tools, and server-side state

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5.6-terra"))?;

    let agent = LlmAgentBuilder::new("openai_assistant")
        .description("OpenAI-powered assistant")
        .instruction("You are a helpful assistant powered by OpenAI GPT-5. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Structured Output (JSON Schema)

OpenAI supports guaranteed JSON output via `output_schema`. ADK-Rust automatically wires this to OpenAI's `response_format` API:

```rust
use adk_rust::prelude::*;
use serde_json::json;
use std::sync::Arc;

let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5.6-terra"))?;

let agent = LlmAgentBuilder::new("data_extractor")
    .model(Arc::new(model))
    .instruction("Extract person information from the text.")
    .output_schema(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "number" },
            "email": { "type": "string" }
        },
        "required": ["name", "age"]
    }))
    .build()?;

// Response is guaranteed to be valid JSON matching the schema
```

For strict mode with nested objects, include `additionalProperties: false` at each level:

```rust
.output_schema(json!({
    "type": "object",
    "properties": {
        "title": { "type": "string" },
        "metadata": {
            "type": "object",
            "properties": {
                "author": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["author"],
            "additionalProperties": false  // Required for nested objects
        }
    },
    "required": ["title", "metadata"],
    "additionalProperties": false  // Auto-injected at root level
}))
```

### Reasoning Effort

For OpenAI reasoning models, control how much reasoning effort the model applies:

```rust
use adk_model::openai::{OpenAIClient, OpenAIConfig, OpenAIReasoningEffort};

let config = OpenAIConfig::new(&api_key, "gpt-5.6-terra");
let model = OpenAIClient::new_with_reasoning_effort(
    config,
    OpenAIReasoningEffort::XHigh,
)?;
```

The complete vocabulary is `None`, `Minimal`, `Low`, `Medium`, `High`, `XHigh`,
and `Max`; availability depends on the model and API. GPT-5.6 Chat Completions
supports up to `XHigh`; use `OpenAIResponsesClient` for `Max`. The original
three-value `ReasoningEffort` API remains available for backward compatibility.

### OpenAI-Compatible Local APIs

Use `OpenAIConfig::compatible()` to connect to local servers (Ollama, vLLM, LM Studio):

```rust
// Ollama exposes OpenAI-compatible API at /v1
let config = OpenAIConfig::compatible(
    "not-needed",                      // API key (ignored by Ollama)
    "http://localhost:11434/v1",       // Base URL
    "llama3.2"                         // Model name
);
let model = OpenAIClient::new(config)?;
```

> **Note**: Structured output (`output_schema`) requires backend support. Native OpenAI fully supports it; local servers may have limited support.

### Gemini via the OpenAI-Compatible Endpoint

Gemini models are reachable through the OpenAI Chat Completions wire format at
`https://generativelanguage.googleapis.com/v1beta/openai`. Use the
`OpenAICompatibleConfig::gemini(...)` preset (under the `openai` feature) with a
`GEMINI_API_KEY` to run Gemini through the same OpenAI-compatible client you use
for every other provider:

```rust
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};

let api_key = std::env::var("GEMINI_API_KEY")?;
let model = OpenAICompatible::new(
    OpenAICompatibleConfig::gemini(api_key, "gemini-3.5-flash"),
)?;
```

This path supports chat, streaming, function calling, structured output, and
reasoning effort (OpenAI's `reasoning_effort` maps to Gemini thinking
levels/budgets). Gemini-specific options — e.g. `thinking_config` with
`include_thoughts`, or `cached_content` — are passed through the request's
`extensions["openai"]["extra_body"]["google"]` map, which the client merges
verbatim into the request body.

> **When to use this vs `GeminiModel`**: For native Gemini features
> (server-side tools, the Interactions API, native `ThinkingConfig`,
> multimodal-first ergonomics), prefer
> [`GeminiModel`](#gemini-google--default). Use the OpenAI-compatible preset when
> you want a single uniform client across providers.

**Examples** (require `GEMINI_API_KEY` or `GOOGLE_API_KEY`):

```bash
# Direct client: chat, reasoning effort, extra_body thinking, streaming,
# function calling, structured output.
cargo run -p adk-model --features openai --example gemini_openai_compat

# The same compat client driving a normal LlmAgent in a Runner.
# (Lives in adk-agent: it exercises the agent layer, which sits above adk-model.)
cargo run -p adk-agent --example gemini_openai_compat_agent
```

### Legacy Reasoning-Effort API

The original three-level `ReasoningEffort` API remains available for compatibility:

```rust
use adk_model::openai::{OpenAIClient, OpenAIConfig, ReasoningEffort};

let config = OpenAIConfig::new(&api_key, "gpt-5")
    .with_reasoning_effort(ReasoningEffort::High);
let model = OpenAIClient::new(config)?;
```

Available levels: `Low` (fastest), `Medium` (balanced), `High` (most thorough).

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `gpt-5.6-terra` | Balanced default for production agents | 256K tokens |
| `gpt-5.6-sol` | Flagship reasoning and coding | 256K tokens |
| `gpt-5.6-luna` | Cost-efficient, high-volume workloads | 128K tokens |
| `gpt-5.6` | Flagship alias | 256K tokens |

### Example Output

```
👤 User: Write a haiku about Rust programming

🤖 GPT-5: Memory so safe,
Ownership guards every byte—
Compiler, my friend.
```

---

## Anthropic (Claude) 🧠 Smart

> **Best for**: Complex reasoning, safety-critical apps, long documents
> 
> **Key highlights**:
> - 🧠 Exceptional reasoning ability
> - 🛡️ Most safety-focused
> - 📚 200K token context
> - ✍️ Excellent writing quality

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-5"))?;

    let agent = LlmAgentBuilder::new("anthropic_assistant")
        .description("Anthropic-powered assistant")
        .instruction("You are a helpful assistant powered by Anthropic Claude. Be concise and thoughtful.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Context |
|-------|-------------|---------|
| `claude-sonnet-5` | Balanced intelligence and cost (default) | 1M tokens |
| `claude-opus-5` | Flagship capability | 1M tokens |
| `claude-fable-5` | Premium creative and long-form work | 1M tokens |
| `claude-haiku-4-5` | Cost-efficient previous generation | 200K tokens |

### Example Output

```
👤 User: Explain quantum entanglement to a 10-year-old

🤖 Claude: Imagine you have two magic coins. When you flip them, they always 
land the same way - both heads or both tails - even if one coin is on Earth 
and the other is on the Moon! Scientists call this "entanglement." The coins 
are connected in a special way that we can't see, like invisible best friends 
who always make the same choice at the exact same time.
```

---

## DeepSeek 💭 Thinking

> **Best for**: Complex problem-solving, math, coding, reasoning tasks
> 
> **Key highlights**:
> - 💭 **Thinking mode** - shows chain-of-thought reasoning
> - 💰 Very cost-effective (10x cheaper than GPT-4)
> - 🔄 Context caching for repeated prefixes
> - 🧮 Strong at math and coding

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("DEEPSEEK_API_KEY")?;
    
    // Standard chat model
    let model = DeepSeekClient::chat(&api_key)?;
    
    // OR: Reasoning model with thinking mode
    // let model = DeepSeekClient::reasoner(&api_key)?;

    let agent = LlmAgentBuilder::new("deepseek_assistant")
        .description("DeepSeek-powered assistant")
        .instruction("You are a helpful assistant powered by DeepSeek. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Description | Special Feature |
|-------|-------------|-----------------|
| `deepseek-v4-flash` | Current default | Fast general-purpose agents |
| `deepseek-v4-pro` | Advanced reasoning | Complex agentic work |

### Example Output (Reasoner with Thinking Mode)

```
👤 User: What's 17 × 23?

🤖 DeepSeek: <thinking>
Let me break this down:
17 × 23 = 17 × (20 + 3)
       = 17 × 20 + 17 × 3
       = 340 + 51
       = 391
</thinking>

The answer is 391.
```

---

## Groq ⚡ Ultra-Fast

> **Best for**: Real-time applications, chatbots, speed-critical tasks
> 
> **Key highlights**:
> - ⚡ **Fastest inference** - 10x faster than competitors
> - 🔧 LPU (Language Processing Unit) technology
> - 💰 Competitive pricing
> - 🦙 Runs LLaMA, Mixtral, Gemma models

### Complete Working Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::new(GroqConfig::gpt_oss_120b(&api_key))?;

    let agent = LlmAgentBuilder::new("groq_assistant")
        .description("Groq-powered assistant")
        .instruction("You are a helpful assistant powered by Groq. Be concise and fast.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

### Available Models

| Model | Method | Description |
|-------|--------|-------------|
| `openai/gpt-oss-120b` | `GroqClient::new(GroqConfig::gpt_oss_120b(key))` | Current production default |
| `openai/gpt-oss-20b` | `GroqClient::new(GroqConfig::new(key, "openai/gpt-oss-20b"))` | Lower-cost GPT-OSS model |
| Any model | `GroqClient::new(GroqConfig::new(key, "model"))` | Custom model |

### Example Output

```
👤 User: Quick! Name 5 programming languages

🤖 Groq (in 0.2 seconds): 
1. Rust
2. Python
3. JavaScript
4. Go
5. TypeScript
```

---

## Switching Providers

All providers implement the same `Llm` trait, so switching is easy:

```rust
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

// Just change the model - everything else stays the same!
let model: Arc<dyn adk_core::Llm> = Arc::new(
    // Pick one:
    // GeminiModel::new(&api_key, "gemini-3.7-flash")?
    // OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-5.6-terra"))?
    // AnthropicClient::new(AnthropicConfig::new(&api_key, "claude-sonnet-5"))?
    // DeepSeekClient::chat(&api_key)?
    // GroqClient::new(GroqConfig::gpt_oss_120b(&api_key))?
);

let agent = LlmAgentBuilder::new("assistant")
    .instruction("You are a helpful assistant.")
    .model(model)
    .build()?;
```

---

## Examples

Use cargo-adk to generate provider-specific projects with validated 0.8 dependencies:

```bash
cargo adk new gemini_agent --provider gemini
cargo adk new openai_agent --template openai
cargo adk new anthropic_agent --provider anthropic
```

The generated projects are compiled in CI by `scripts/check-cargo-adk-templates.sh`. The full example gallery is maintained in the [adk-playground](https://github.com/zavora-ai/adk-playground) repo.

---

## Related

- [Ollama (Local)](./ollama.md) - Run models locally with Ollama
- [Local Models (mistral.rs)](./mistralrs.md) - Native Rust inference
- [LlmAgent](../agents/llm-agent.md) - Using models with agents
- [Function Tools](../tools/function-tools.md) - Adding tools to agents

---

**Previous**: [← Realtime Agents](../agents/realtime-agents.md) | **Next**: [Ollama (Local) →](./ollama.md)

## What happens to content a provider cannot carry

`Content` can express more than any single provider transport accepts, so each adapter has
to decide what to do with the remainder. Those decisions are now recorded rather than
applied invisibly. Every part is classified:

| Disposition | Meaning |
|-------------|---------|
| `Converted` | Carried to the provider in an equivalent native form |
| `Downgraded` | Carried in a lossier form — a file reference rendered as descriptive text the model can read but not fetch |
| `Omitted` | Not carried at all |

Downgrades and omissions emit a `tracing` warning as they are recorded, naming the part
kind, MIME type, and reason, so neither is silent.

To see the outcome before dispatching a request:

```rust
use adk_core::{Content, Part};
use adk_model::bedrock::convert::report_for_contents;

let content = Content {
    role: "user".to_string(),
    parts: vec![Part::inline_data("audio/wav", vec![0u8; 16])],
};
let report = report_for_contents(std::slice::from_ref(&content));

for omission in report.omitted_parts() {
    println!("{} was dropped: {}", omission.kind, omission.detail);
}
```

To refuse a request that would reach the model incomplete rather than receive an answer
about material the model never saw:

```rust
use adk_core::{Content, Part};
use adk_model::bedrock::convert::report_for_contents;

let content = Content {
    role: "user".to_string(),
    parts: vec![Part::inline_data("video/mp4", vec![0u8; 16])],
};

if let Some(error) = report_for_contents(std::slice::from_ref(&content)).into_error() {
    return Err(error);
}
```

`into_error` covers omissions only. A downgrade still reaches the model, and rejecting it
would refuse the documented textual fallback.

> **Note:** the ledger is complete by construction. Any part that leaves an adapter without
> a recorded fate — including one added by a future change — is recorded as an omission
> with an explicit "no recorded reason", and `adk-model/tests/part_conversion_matrix_tests.rs`
> fails on it.

### Bedrock Converse coverage

| Part | Disposition |
|------|-------------|
| Text, FunctionCall, FunctionResponse, Thinking | `Converted` |
| `InlineData` with JPEG, PNG, GIF, WebP | `Converted` as an image block |
| `InlineData` with a supported document type (PDF and similar) | `Converted` as a document block |
| `InlineData` with audio, video, or arbitrary binary | `Omitted` |
| `FileData` for an image or supported document | `Downgraded` to text — Converse takes S3 URIs, not arbitrary URLs |
| `FileData` for any other type | `Omitted` |
| `ServerToolCall`, `ServerToolResponse` | `Omitted` — Gemini-specific |
| `EmbeddedResource` text, or a blob of a supported type | `Converted` |
| `EmbeddedResource` blob of an unsupported type | `Omitted` |
