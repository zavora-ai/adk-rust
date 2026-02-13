# adk-gemini

Rust client library for Google's Gemini API — content generation, streaming, function calling, embeddings, image/speech generation, batch processing, caching, and Vertex AI.

[![Crates.io](https://img.shields.io/crates/v/adk-gemini.svg)](https://crates.io/crates/adk-gemini)
[![Documentation](https://docs.rs/adk-gemini/badge.svg)](https://docs.rs/adk-gemini)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`adk-gemini` is a comprehensive Rust client for the Google Gemini API, maintained as part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) project. It provides full coverage of the Gemini API surface:

- Content generation (text, images, audio)
- Real-time streaming responses
- Function calling and tool integration (including Google Search and URL Context)
- Thinking mode (Gemini 2.5 / Gemini 3)
- Text embeddings
- Image generation and editing
- Text-to-speech (single and multi-speaker)
- Batch processing
- Content caching
- File upload and management
- Structured JSON output
- Grounding with Google Search
- Vertex AI (Google Cloud) support with ADC, service accounts, and WIF
- Multimodal input (images, video, PDF, audio)

## Installation

```toml
[dependencies]
adk-gemini = "0.3.0"
```

Or through `adk-model`:

```toml
[dependencies]
adk-model = { version = "0.3.0", features = ["gemini"] }
```

## Quick Start

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;

    let response = client
        .generate_content()
        .with_user_message("Hello, Gemini!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

## Client Constructors

| Constructor | Description |
|-------------|-------------|
| `Gemini::new(api_key)` | Default model (gemini-2.5-flash) via v1beta |
| `Gemini::pro(api_key)` | Gemini 2.5 Pro | Gemini 3 Pro |
| `Gemini::with_model(api_key, model)` | Specific model |
| `Gemini::with_v1(api_key)` | Stable v1 API |
| `Gemini::with_model_v1(api_key, model)` | Specific model on v1 |
| `Gemini::with_base_url(api_key, url)` | Custom endpoint |
| `Gemini::with_google_cloud(api_key, project, location)` | Vertex AI |
| `Gemini::with_google_cloud_adc(project, location)` | Vertex AI with ADC |
| `Gemini::with_service_account_json(json)` | Service account (auto-detects project) |
| `Gemini::with_google_cloud_wif_json(json, project, location, model)` | Workload Identity Federation |

## ✅ Using the stable v1 API

By default the SDK uses the **v1beta** endpoint. Use `with_v1` to target the stable v1 API.

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let client = Gemini::with_v1(api_key)?;

    let response = client
        .generate_content()
        .with_user_message("Hello, Gemini!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

## ☁️ Vertex AI (Google Cloud) via the Google Cloud SDK

When you configure Vertex AI, the client uses the official Google Cloud Rust SDK
(`google-cloud-aiplatform`) plus `google-cloud-auth` for credentials. This supports
`generateContent` and `embedContent` requests through Vertex. Other Gemini REST-only
operations (batch jobs, streaming, files, cache) remain available on the Gemini API
and are not supported via the Vertex SDK.

### Vertex AI API Keys

For Vertex AI endpoints, you can configure the client with your project and location.
API key auth is still supported for model inference.

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let client = Gemini::with_google_cloud(api_key, "my-project", "us-central1")?;

    let response = client
        .generate_content()
        .with_user_message("Hello from Vertex AI!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

### Application Default Credentials (ADC)

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Gemini::with_google_cloud_adc("my-project", "us-central1")?;

    let response = client
        .generate_content()
        .with_user_message("Hello from Vertex AI!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

### Service Accounts

Service accounts are supported for Vertex AI. Provide the service account JSON key
and choose the project/location.

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service_account_json = std::fs::read_to_string("service-account.json")?;
    let client = Gemini::with_google_cloud_service_account_json(
        &service_account_json,
        "my-project",
        "us-central1",
        "gemini-2.5-flash",
    )?;

    let response = client
        .generate_content()
        .with_user_message("Hello from Vertex AI!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

### Workload Identity Federation (WIF)

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let wif_json = std::fs::read_to_string("wif-credentials.json")?;
    let client = Gemini::with_google_cloud_wif_json(
        &wif_json,
        "my-project",
        "us-central1",
        "gemini-2.5-flash",
    )?;

    let response = client
        .generate_content()
        .with_user_message("Hello from Vertex AI!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

### Google Cloud secrets via environment

When deploying, you can keep credentials in secret managers or CI/CD systems and
expose them as environment variables. The Google Cloud SDK credentials loader
supports `GOOGLE_APPLICATION_CREDENTIALS` (service account or WIF JSON), while
project/location can be provided as `GOOGLE_CLOUD_PROJECT` and
`GOOGLE_CLOUD_LOCATION`.

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let location = std::env::var("GOOGLE_CLOUD_LOCATION")?;

    // ADC will read GOOGLE_APPLICATION_CREDENTIALS when set.
    let client = Gemini::with_google_cloud_adc(&project, &location)?;

    let response = client
        .generate_content()
        .with_user_message("Hello from secrets-backed Vertex AI!")
        .execute()
        .await?;

    println!("{}", response.text());
    Ok(())
}
```

## 🔧 ADK-Specific Extensions

## Examples

### Streaming

```rust
use adk_gemini::Gemini;
use futures_util::TryStreamExt;

let client = Gemini::new(api_key)?;

let mut stream = client
    .generate_content()
    .with_system_prompt("You are a helpful assistant.")
    .with_user_message("Write a short story about a robot.")
    .execute_stream()
    .await?;

while let Some(chunk) = stream.try_next().await? {
    print!("{}", chunk.text());
}
```

### Function Calling

```rust
use adk_gemini::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WeatherRequest {
    location: String,
    unit: Option<String>,
}

let get_weather = FunctionDeclaration::new(
    "get_weather",
    "Get the current weather for a location",
    None,
).with_parameters::<WeatherRequest>();

let response = client
    .generate_content()
    .with_user_message("What's the weather in Tokyo?")
    .with_function(get_weather)
    .with_function_calling_mode(FunctionCallingMode::Any)
    .execute()
    .await?;

if let Some(call) = response.function_calls().first() {
    println!("Call: {} with args: {}", call.name, call.args);
}
```

### Google Search Grounding

```rust
use adk_gemini::{Gemini, Tool};

let response = client
    .generate_content()
    .with_user_message("What is the current Google stock price?")
    .with_tool(Tool::google_search())
    .execute()
    .await?;

println!("{}", response.text());

// Access grounding metadata
if let Some(grounding) = response.candidates.first()
    .and_then(|c| c.grounding_metadata.as_ref())
{
    if let Some(chunks) = &grounding.grounding_chunks {
        for chunk in chunks {
            if let Some(web) = &chunk.web {
                println!("Source: {} - {}", web.title, web.uri);
            }
        }
    }
}
```

### URL Context

```rust
use adk_gemini::{Gemini, Tool};

let response = client
    .generate_content()
    .with_user_message("Summarize this page: https://docs.rs/tokio/latest/tokio/")
    .with_tool(Tool::url_context())
    .execute()
    .await?;
```

### Thinking Mode (Gemini 2.5 / Gemini 3 Pro)

```rust
let client = Gemini::pro(api_key)?;

let response = client
    .generate_content()
    .with_user_message("Solve: what is the integral of x^2 * e^x dx?")
    .with_thinking_budget(2048)
    .with_thoughts_included(true)
    .execute()
    .await?;

// Access the model's reasoning
for thought in response.thoughts() {
    println!("Thought: {}", thought);
}
println!("Answer: {}", response.text());
```

### Structured JSON Output

```rust
use serde_json::json;

let schema = json!({
    "type": "object",
    "properties": {
        "name": { "type": "string" },
        "year_created": { "type": "integer" },
        "key_features": { "type": "array", "items": { "type": "string" } }
    },
    "required": ["name", "year_created", "key_features"]
});

let response = client
    .generate_content()
    .with_user_message("Tell me about the Rust programming language.")
    .with_response_mime_type("application/json")
    .with_response_schema(schema)
    .execute()
    .await?;

let parsed: serde_json::Value = serde_json::from_str(&response.text())?;
```

### Text Embeddings

```rust
use adk_gemini::{Gemini, Model, TaskType};

let client = Gemini::with_model(api_key, Model::TextEmbedding004)?;

let response = client
    .embed_content()
    .with_text("Hello, world!")
    .with_task_type(TaskType::RetrievalDocument)
    .execute()
    .await?;

println!("Embedding dimensions: {}", response.embedding.values.len());
```

### Image Generation

```rust
let client = Gemini::with_model(
    api_key,
    "models/gemini-2.5-flash-image-preview".to_string(),
)?;

let response = client
    .generate_content()
    .with_user_message("A photorealistic image of a mountain lake at sunset")
    .execute()
    .await?;

// Response contains inline image data (base64-encoded)
for candidate in &response.candidates {
    if let Some(parts) = &candidate.content.parts {
        for part in parts {
            if let adk_gemini::Part::InlineData { inline_data } = part {
                // inline_data.data contains base64-encoded image bytes
                // inline_data.mime_type contains the MIME type
            }
        }
    }
}
```

### Text-to-Speech

```rust
use adk_gemini::*;

let client = Gemini::with_model(
    api_key,
    "models/gemini-2.5-flash-preview-tts".to_string(),
)?;

let response = client
    .generate_content()
    .with_user_message("Hello! This is AI-generated speech.")
    .with_generation_config(GenerationConfig {
        response_modalities: Some(vec!["AUDIO".to_string()]),
        speech_config: Some(SpeechConfig {
            voice_config: Some(VoiceConfig {
                prebuilt_voice_config: Some(PrebuiltVoiceConfig {
                    voice_name: "Puck".to_string(),
                }),
            }),
            multi_speaker_voice_config: None,
        }),
        ..Default::default()
    })
    .execute()
    .await?;
```

### Content Caching

```rust
use std::time::Duration;

let cache = client
    .create_cache()
    .with_display_name("My Analysis Cache")?
    .with_system_instruction("You are a literary analyst.")
    .with_user_message(long_document_text)
    .with_ttl(Duration::from_secs(3600))
    .execute()
    .await?;

// Reuse the cache across multiple queries
let response = client
    .generate_content()
    .with_cached_content(&cache)
    .with_user_message("What is the central theme?")
    .execute()
    .await?;
```

### Batch Processing

```rust
let request1 = client
    .generate_content()
    .with_user_message("What is the meaning of life?")
    .build();

let request2 = client
    .generate_content()
    .with_user_message("What is the best programming language?")
    .build();

let batch = client
    .batch_generate_content()
    .with_request(request1)
    .with_request(request2)
    .execute()
    .await?;
```

### Multi-Turn Conversation

```rust
let response1 = client
    .generate_content()
    .with_system_prompt("You are a travel assistant.")
    .with_user_message("I'm planning a trip to Japan.")
    .execute()
    .await?;

let response2 = client
    .generate_content()
    .with_system_prompt("You are a travel assistant.")
    .with_user_message("I'm planning a trip to Japan.")
    .with_model_message(response1.text())
    .with_user_message("What about cherry blossom season?")
    .execute()
    .await?;
```

### Vertex AI (Google Cloud)

```rust
// API key auth
let client = Gemini::with_google_cloud(api_key, "my-project", "us-central1")?;

// Application Default Credentials
let client = Gemini::with_google_cloud_adc("my-project", "us-central1")?;

// Service account
let sa_json = std::fs::read_to_string("service-account.json")?;
let client = Gemini::with_google_cloud_service_account_json(
    &sa_json, "my-project", "us-central1", "gemini-2.5-flash",
)?;

// Workload Identity Federation
let wif_json = std::fs::read_to_string("wif-credentials.json")?;
let client = Gemini::with_google_cloud_wif_json(
    &wif_json, "my-project", "us-central1", "gemini-2.5-flash",
)?;
```

### Generation Config

```rust
use adk_gemini::GenerationConfig;

let response = client
    .generate_content()
    .with_user_message("Tell me a joke.")
    .with_generation_config(GenerationConfig {
        temperature: Some(0.9),
        top_p: Some(0.95),
        top_k: Some(40),
        max_output_tokens: Some(1024),
        ..Default::default()
    })
    .execute()
    .await?;
```

## API Modules

| Module | Description |
|--------|-------------|
| `generation` | Content generation (text, images, audio) |
| `embedding` | Text embedding generation |
| `batch` | Batch processing for multiple requests |
| `files` | File upload and management |
| `cache` | Content caching for reusable contexts |
| `safety` | Content moderation and safety settings |
| `tools` | Function calling and tool integration |
| `models` | Core primitive types (Content, Part, Role, Blob) |
| `prelude` | Convenient re-exports of commonly used types |

## Environment Variables

```bash
# Gemini API
GEMINI_API_KEY=your-api-key
# or
GOOGLE_API_KEY=your-api-key

# Vertex AI (Google Cloud)
GOOGLE_CLOUD_PROJECT=my-project
GOOGLE_CLOUD_LOCATION=us-central1
GOOGLE_APPLICATION_CREDENTIALS=/path/to/service-account.json
```

## Running Examples

```bash
export GEMINI_API_KEY=your-api-key

cargo run -p adk-gemini --example simple
cargo run -p adk-gemini --example streaming
cargo run -p adk-gemini --example tools
cargo run -p adk-gemini --example google_search
cargo run -p adk-gemini --example thinking_basic
cargo run -p adk-gemini --example embedding
cargo run -p adk-gemini --example image_generation
cargo run -p adk-gemini --example structured_response
cargo run -p adk-gemini --example cache_basic
cargo run -p adk-gemini --example batch_generate
cargo run -p adk-gemini --example url_context
cargo run -p adk-gemini --example simple_speech_generation
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-model](https://crates.io/crates/adk-model) - Multi-provider LLM integrations (uses adk-gemini internally)
- [adk-core](https://crates.io/crates/adk-core) - Core `Llm` trait
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations

## License

MIT

Original work Copyright (c) 2024 [@flachesis](https://github.com/flachesis)
Modifications Copyright (c) 2024 Zavora AI

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.

---

## Attribution

This crate is a fork of the excellent [gemini-rust](https://github.com/flachesis/gemini-rust) library by [@flachesis](https://github.com/flachesis). We are deeply grateful for their work in creating and maintaining this high-quality Gemini API client.

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
