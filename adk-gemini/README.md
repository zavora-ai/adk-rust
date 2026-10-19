# adk-gemini

Rust client for the Google Gemini API: content generation, streaming, function calling,
built-in tools, thinking, embeddings, image and speech generation, batch processing, caching,
files, the Interactions API (Beta), and Vertex AI.

[![Crates.io](https://img.shields.io/crates/v/adk-gemini.svg)](https://crates.io/crates/adk-gemini)
[![Documentation](https://docs.rs/adk-gemini/badge.svg)](https://docs.rs/adk-gemini)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`adk-gemini` is the Gemini client behind `adk_model::GeminiModel` and is maintained as part of
[ADK-Rust](https://github.com/zavora-ai/adk-rust). It covers:

- Content generation (text, images, audio) and streaming
- Function calling and built-in tools: Google Search, URL context, code execution, Google Maps,
  File Search, and computer use
- Thinking levels and thought signatures for multi-turn function calling
- Structured JSON output
- Embeddings (`gemini-embedding-2`, `gemini-embedding-001`)
- Image generation and editing, single- and multi-speaker text-to-speech
- Batch processing, context caching, and the Files API
- Model discovery (`list_models`, `get_model`)
- Vertex AI with API keys, Application Default Credentials, service accounts, and Workload
  Identity Federation (`vertex` feature)
- The Interactions API (Beta): server-side history, a typed step timeline, streaming step events,
  managed agents, and environments (`interactions` feature)

## Installation

```toml
[dependencies]
adk-gemini = "3.0.0"
```

| Feature | Enables |
|---------|---------|
| `studio` (default) | Marker only; the AI Studio backend is always compiled |
| `vertex` | Vertex AI constructors and `VertexBackend` |
| `interactions` | The Interactions API module and client methods |
| `backtrace` | `snafu`'s `backtraces-impl-backtrace-crate` feature |

`adk-model` uses this crate for `GeminiModel`:

```toml
[dependencies]
adk-model = { version = "3.0.0", features = ["gemini"] }
```

## Quick Start

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let response = client
        .generate_content()
        .with_user_message("Name one planet in a short sentence.")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

`Gemini::new` uses the default model, `gemini-3.7-flash`, on the `v1beta` API.

## Client constructors

| Constructor | Backend and model |
|-------------|-------------------|
| `Gemini::new(api_key)` | AI Studio, `gemini-3.7-flash`, `v1beta` |
| `Gemini::pro(api_key)` | AI Studio, `gemini-3.1-pro-preview` |
| `Gemini::with_model(api_key, model)` | AI Studio, any model |
| `Gemini::with_v1(api_key)`, `Gemini::with_model_v1(api_key, model)` | AI Studio on the stable `v1` API |
| `Gemini::with_base_url(api_key, url)`, `Gemini::with_model_and_base_url(api_key, model, url)` | A custom endpoint |
| `Gemini::with_google_cloud(api_key, project, location)` | Vertex AI with an API key (`vertex`) |
| `Gemini::with_google_cloud_adc(project, location)` | Vertex AI with Application Default Credentials (`vertex`) |
| `Gemini::with_google_cloud_service_account_json(json, project, location, model)` | Vertex AI with a service account key (`vertex`) |
| `Gemini::with_service_account_json(json)` | Vertex AI, project read from the service account key (`vertex`) |
| `Gemini::with_google_cloud_wif_json(json, project, location, model)` | Vertex AI with Workload Identity Federation (`vertex`) |
| `GeminiBuilder::new(api_key)` | Builder: model, HTTP client, base URL, and Google Cloud options |

`model` accepts a `Model` variant, a factory such as `Model::gemini_3_5_flash_lite()`, or a model
ID `String`; a bare ID gets the `models/` prefix.

## Examples

### Streaming

```rust
use adk_gemini::Gemini;
use futures::TryStreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let mut stream = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("Write a two-sentence story about a robot.")
        .execute_stream()
        .await?;

    while let Some(chunk) = stream.try_next().await? {
        print!("{}", chunk.text());
    }
    println!();
    Ok(())
}
```

### Function calling

The parameter type derives `schemars::JsonSchema`, so the example needs `schemars` and `serde`
as dependencies.

```rust
use adk_gemini::{FunctionCallingMode, FunctionDeclaration, Gemini};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherRequest {
    /// City name.
    location: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let get_weather =
        FunctionDeclaration::new("get_weather", "Get the current weather for a location", None)
            .with_parameters::<WeatherRequest>();

    let response = client
        .generate_content()
        .with_user_message("What's the weather in Tokyo?")
        .with_function(get_weather)
        .with_function_calling_mode(FunctionCallingMode::Any)
        .execute()
        .await?;

    if let Some(call) = response.function_calls().first() {
        println!("call: {} with args: {}", call.name, call.args);
    }
    Ok(())
}
```

Answer the call with `with_function_response(name, value)` on the next request.

### Google Search grounding

```rust
use adk_gemini::{Gemini, Tool};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let response = client
        .generate_content()
        .with_user_message("Who won the most recent Formula 1 race?")
        .with_tool(Tool::google_search())
        .execute()
        .await?;

    println!("{}", response.text());

    let grounding = response.candidates.first().and_then(|c| c.grounding_metadata.as_ref());
    if let Some(grounding) = grounding {
        for chunk in grounding.grounding_chunks.iter().flatten() {
            if let Some(web) = &chunk.web {
                println!("source: {:?} {:?}", web.title, web.uri);
            }
        }
        if let Some(entry) = &grounding.search_entry_point {
            println!("search suggestions HTML: {} bytes", entry.rendered_content.as_deref().map_or(0, str::len));
        }
    }
    Ok(())
}
```

`GroundingMetadata::search_entry_point` carries the search suggestions to display with grounded
answers, and `GroundingSegment::part_index` the part a segment belongs to (`None` for the first
part). `Tool::url_context()`, `Tool::code_execution()`, `Tool::google_maps(config)`,
`Tool::file_search(config)`, and `Tool::computer_use(config)` declare the other built-in tools.

### Thinking

```rust
use adk_gemini::{Gemini, ThinkingLevel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let response = client
        .generate_content()
        .with_user_message("What is 17 * 23? Answer with the number.")
        .with_thinking_level(ThinkingLevel::Low)
        .with_thoughts_included(true)
        .execute()
        .await?;

    for thought in response.thoughts() {
        println!("thought: {thought}");
    }
    println!("answer: {}", response.text());
    Ok(())
}
```

`ThinkingLevel` has `Minimal`, `Low`, `Medium`, and `High`; Gemini 3.1 Pro does not support
`Minimal`. `with_thinking_budget(tokens)` is the budget-based control of Gemini 2.5 models, and
a config that sets both a budget and a level is rejected. For `gemini-3.6-flash` and
`gemini-3.7-flash`, a request that sets `temperature`, `top_p`, `top_k`, `candidate_count`, or a
thinking budget fails with `ClientError::InvalidGenerationConfig` before it is sent.

### Thought signatures

With thinking and function calling, Gemini returns a `thoughtSignature` with each function call.
Send it back with the call in the next request to keep the model's reasoning context.

```rust
use adk_gemini::{Content, FunctionDeclaration, Gemini, Part, Role, ThinkingConfig, ThinkingLevel};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;
    let get_weather = FunctionDeclaration::new("get_weather", "Get the weather for a city", None);

    let response = client
        .generate_content()
        .with_user_message("What's the weather in Tokyo?")
        .with_function(get_weather)
        .with_thinking_config(
            ThinkingConfig::new().with_thinking_level(ThinkingLevel::Low).with_thoughts_included(true),
        )
        .execute()
        .await?;

    for (call, signature) in response.function_calls_with_thoughts() {
        // The model turn to replay in the next request's history.
        let model_turn = Content {
            parts: Some(vec![Part::FunctionCall {
                function_call: call.clone(),
                thought_signature: signature.cloned(),
            }]),
            role: Some(Role::Model),
        };
        println!("{} signed: {}", call.name, signature.is_some());
        let _ = (model_turn, json!({ "temperature_c": 21 }));
    }
    Ok(())
}
```

`thoughtSignature` is serialized both on the part and inside the `FunctionCall` object, because
Gemini 3 models require it in both places for multi-turn function calling. It is omitted when
`None`.

### Structured JSON output

```rust
use adk_gemini::Gemini;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "year_created": { "type": "integer" }
        },
        "required": ["name", "year_created"]
    });

    let response = client
        .generate_content()
        .with_user_message("Tell me about the Rust programming language.")
        .with_response_mime_type("application/json")
        .with_response_schema(schema)
        .execute()
        .await?;

    let parsed: serde_json::Value = serde_json::from_str(&response.text())?;
    println!("{parsed}");
    Ok(())
}
```

### Embeddings

```rust
use adk_gemini::{Gemini, Model, TaskType};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::with_model(std::env::var("GEMINI_API_KEY")?, Model::GeminiEmbedding2)?;

    let response = client
        .embed_content()
        .with_text("Hello, world!")
        .with_task_type(TaskType::RetrievalDocument)
        .execute()
        .await?;

    println!("dimensions: {}", response.embedding.values.len());
    Ok(())
}
```

### Image generation

```rust
use adk_gemini::{Gemini, Model, Part};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::with_model(std::env::var("GEMINI_API_KEY")?, Model::Gemini31FlashImage)?;

    let response = client
        .generate_content()
        .with_user_message("A photorealistic image of a mountain lake at sunset")
        .execute()
        .await?;

    for candidate in &response.candidates {
        for part in candidate.content.parts.iter().flatten() {
            if let Part::InlineData { inline_data, .. } = part {
                // `data` is base64-encoded.
                println!("{}: {} base64 bytes", inline_data.mime_type, inline_data.data.len());
            }
        }
    }
    Ok(())
}
```

### Text-to-speech

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::with_model(
        std::env::var("GEMINI_API_KEY")?,
        "gemini-3.1-flash-tts-preview".to_string(),
    )?;

    let response = client
        .generate_content()
        .with_user_message("Hello! This is generated speech.")
        .with_audio_output()
        .with_voice("Puck")
        .execute()
        .await?;

    println!("{} candidates", response.candidates.len());
    Ok(())
}
```

`with_multi_speaker_config(vec![SpeakerVoiceConfig::new(speaker, voice), ...])` assigns a voice
to each speaker.

### Context caching

```rust
use adk_gemini::Gemini;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;
    let long_document = std::fs::read_to_string("novel.txt")?;

    let cache = client
        .create_cache()
        .with_display_name("Novel analysis")?
        .with_system_instruction("You are a literary analyst.")
        .with_user_message(long_document)
        .with_ttl(Duration::from_secs(3600))
        .execute()
        .await?;

    let response = client
        .generate_content()
        .with_cached_content(&cache)
        .with_user_message("What is the central theme?")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

### Batch processing

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let first = client.generate_content().with_user_message("What is the meaning of life?").build();
    let second = client.generate_content().with_user_message("Name a prime number.").build();

    let batch = client.batch_generate_content().with_request(first).with_request(second).execute().await?;
    println!("batch: {}", batch.name());
    Ok(())
}
```

### Multi-turn conversation

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let first = client
        .generate_content()
        .with_system_prompt("You are a travel assistant.")
        .with_user_message("I'm planning a trip to Japan.")
        .execute()
        .await?;

    let second = client
        .generate_content()
        .with_system_prompt("You are a travel assistant.")
        .with_user_message("I'm planning a trip to Japan.")
        .with_model_message(first.text())
        .with_user_message("When is cherry blossom season?")
        .execute()
        .await?;

    println!("{}", second.text());
    Ok(())
}
```

### Interactions API (Beta)

Needs the `interactions` feature. The server keeps the history; continue a conversation with
`previous_interaction_id`.

```rust
use adk_gemini::{Gemini, Model, ThinkingLevel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let interaction = client
        .create_interaction()
        .model(Model::Gemini35Flash)
        .input_text("What is the capital of France?")
        .thinking_level(ThinkingLevel::Low)
        .send()
        .await?;

    println!("{:?}: {}", interaction.status, interaction.output_text().unwrap_or_default());
    Ok(())
}
```

`stream()` returns the step events, and `get_interaction`, `delete_interaction`, and
`cancel_interaction` manage stored interactions. The stream fails when it ends before
`interaction.completed` or reports a `failed` or `budget_exceeded` status. The
[`interactions_basic`](examples/interactions_basic.rs) example covers streaming and multi-turn
continuation, and [`examples/gemini_managed_agents/`](../examples/gemini_managed_agents/) covers
managed agents and environments.

### Vertex AI

Needs the `vertex` feature.

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Application Default Credentials on a regional endpoint.
    let regional = Gemini::with_google_cloud_adc("my-project", "us-central1")?;

    // The global endpoint, https://aiplatform.googleapis.com.
    let global = Gemini::with_google_cloud_adc("my-project", "global")?;

    // A service account key.
    let key = std::fs::read_to_string("service-account.json")?;
    let service_account =
        Gemini::with_google_cloud_service_account_json(&key, "my-project", "us-central1", "gemini-3.7-flash".to_string())?;

    // Workload Identity Federation.
    let wif = std::fs::read_to_string("wif-credentials.json")?;
    let federated =
        Gemini::with_google_cloud_wif_json(&wif, "my-project", "us-central1", "gemini-3.7-flash".to_string())?;

    let _ = (regional, global, service_account, federated);
    Ok(())
}
```

A `location` of `"global"` resolves to `https://aiplatform.googleapis.com`; any other location
uses `https://{location}-aiplatform.googleapis.com`.

## Backends

| Backend | Transport | Auth | Used by |
|---------|-----------|------|---------|
| `StudioBackend` | REST and SSE | API key | `Gemini::new`, `Gemini::with_model`, and the other AI Studio constructors |
| `VertexBackend` | gRPC for non-streaming requests, REST SSE for streaming | ADC, service account, WIF, or API key | The `with_google_cloud*` and service-account constructors |

The `GeminiBackend` trait defines `generate_content`, `generate_content_stream`, and the optional
operations. `VertexBackend` supports generation, streaming, embeddings, and cached content
(create, get, update, list, delete). The Files API, batch operations, and the Interactions API
are AI Studio only and return `ClientError::GoogleCloudUnsupported` on Vertex AI. A failed model
request is never replayed through another transport.

## Errors

`ClientError` reports an `error` object inside a successful `generateContent` body or
`streamGenerateContent` event as `ErrorInResponse`, with its `code`, `status`, and `message`,
instead of decoding it as an empty response. Through `adk_model::GeminiModel`, a stream that ends
before any chunk carries `finishReason` fails with `model.gemini.unfinished_stream`, and a blocked
prompt (`promptFeedback.blockReason`) ends the turn with `FinishReason::Safety`.

## API modules

| Module | Contents |
|--------|----------|
| `generation` | Content generation requests and responses |
| `embedding` | Embedding requests |
| `batch` | Batch processing |
| `files` | File upload and management |
| `cache` | Context caching |
| `safety` | Safety settings and ratings |
| `tools` | Function declarations and built-in tools |
| `backend` | `GeminiBackend`, `StudioBackend`, `VertexBackend` |
| `model_info` | Model discovery |
| `pricing` | Token cost estimation |
| `schema_adapter` | Function-declaration schema normalization (`GeminiSchemaAdapter`) |
| `interactions` | Interactions API (Beta), `interactions` feature |
| `prelude` | Re-exports of commonly used types |

## Environment variables

The examples read `GEMINI_API_KEY`. For Vertex AI, Application Default Credentials follow the
Google Cloud conventions, such as `GOOGLE_APPLICATION_CREDENTIALS`.

## Running examples

```bash
export GEMINI_API_KEY=your-api-key

cargo run -p adk-gemini --example simple
cargo run -p adk-gemini --example streaming
cargo run -p adk-gemini --example tools
cargo run -p adk-gemini --example google_search
cargo run -p adk-gemini --example thinking_basic
cargo run -p adk-gemini --example simple_thought_signature
cargo run -p adk-gemini --example embedding
cargo run -p adk-gemini --example image_generation
cargo run -p adk-gemini --example structured_response
cargo run -p adk-gemini --example cache_basic
cargo run -p adk-gemini --example batch_generate
cargo run -p adk-gemini --example url_context
cargo run -p adk-gemini --example simple_speech_generation
cargo run -p adk-gemini --features interactions --example interactions_basic
```

## Related crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-model](https://crates.io/crates/adk-model) — multi-provider `Llm` clients, including `GeminiModel` over this crate
- [adk-core](https://crates.io/crates/adk-core) — the `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) — agent implementations

## License

MIT

Original work Copyright (c) 2024 [@flachesis](https://github.com/flachesis)
Modifications Copyright (c) 2024 Zavora AI

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.

---

## Attribution

This crate is a fork of the [gemini-rust](https://github.com/flachesis/gemini-rust) library by [@flachesis](https://github.com/flachesis). We are grateful for their work in creating and maintaining this Gemini API client.

**Upstream Project**
- Repository: [github.com/flachesis/gemini-rust](https://github.com/flachesis/gemini-rust)
- Crates.io: [crates.io/crates/gemini-rust](https://crates.io/crates/gemini-rust)
- Original Author: [@flachesis](https://github.com/flachesis)

**Why a Fork?**

The ADK-Rust project requires certain extensions for deep integration with the Agent Development Kit — exporting additional types (e.g., `GroundingMetadata`, `GroundingChunk`) for grounding support, future ADK-specific extensions for agent workflows, and workspace-level version management. We regularly sync with upstream to incorporate improvements and fixes.

**Our Commitment**
1. Staying aligned with the upstream gemini-rust project as much as possible
2. Contributing back any general improvements that benefit the broader community
3. Maintaining attribution and respecting the original MIT license
4. Minimizing divergence — only adding ADK-specific extensions when necessary

**Acknowledgments**
- [@flachesis](https://github.com/flachesis) — Creator and maintainer of the original gemini-rust library
- [@npatsakula](https://github.com/npatsakula) — Major contributions to the upstream project
