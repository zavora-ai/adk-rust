# adk-gemini

**ADK-Rust fork of the [gemini-rust](https://github.com/flachesis/gemini-rust) library**

A comprehensive Rust client library for Google's Gemini 2.5 API, maintained as part of the ADK-Rust project.

[![ADK-Rust](https://img.shields.io/badge/ADK--Rust-adk--gemini-blue)](https://github.com/zavora-ai/adk-rust)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## 🙏 Attribution

**This crate is a fork of the excellent [gemini-rust](https://github.com/flachesis/gemini-rust) library** by [@flachesis](https://github.com/flachesis). We are deeply grateful for their work in creating and maintaining this high-quality Gemini API client.

### Upstream Project

- **Repository**: [github.com/flachesis/gemini-rust](https://github.com/flachesis/gemini-rust)
- **Crates.io**: [crates.io/crates/gemini-rust](https://crates.io/crates/gemini-rust)
- **Original Author**: [@flachesis](https://github.com/flachesis)

### Our Commitment

We are committed to:

1. **Staying aligned** with the upstream gemini-rust project as much as possible
2. **Contributing back** any general improvements that would benefit the broader community
3. **Maintaining attribution** and respecting the original MIT license
4. **Minimizing divergence** - only adding ADK-specific extensions when necessary

### Why a Fork?

The ADK-Rust project requires certain extensions for deep integration with the Agent Development Kit:

- Exporting additional types (e.g., `GroundingMetadata`, `GroundingChunk`) for grounding support
- Future ADK-specific extensions for agent workflows
- Workspace-level version management

We will regularly sync with upstream to incorporate improvements and fixes.

---

## ✨ Features

- **🚀 Complete Gemini 2.5 API Implementation** - Full support for all Gemini API endpoints
- **🛠️ Function Calling & Tools** - Custom functions and Google Search integration with OpenAPI schema support
- **📦 Batch Processing** - Efficient batch content generation and embedding
- **💾 Content Caching** - Cache system instructions and conversation history for cost optimization
- **🔄 Streaming Responses** - Real-time streaming of generated content
- **🧠 Thinking Mode** - Support for Gemini 2.5 thinking capabilities
- **🎨 Image Generation** - Text-to-image generation and image editing capabilities
- **🎤 Speech Generation** - Text-to-speech with single and multi-speaker support
- **🖼️ Multimodal Support** - Images and binary data processing
- **📊 Text Embeddings** - Advanced embedding generation with multiple task types
- **⚙️ Highly Configurable** - Custom models, endpoints, and generation parameters
- **🔒 Type Safe** - Comprehensive type definitions with full `serde` support
- **⚡ Async/Await** - Built on `tokio` for high-performance async operations
- **🌐 Grounding Support** - Full access to `GroundingMetadata` for Google Search results

## 📦 Installation

This crate is part of the ADK-Rust workspace. Add it to your `Cargo.toml`:

```toml
[dependencies]
adk-gemini = "0.2.0"
```

Or use it through `adk-model`:

```toml
[dependencies]
adk-model = { version = "0.2.1", features = ["gemini"] }
```

## 🚀 Quick Start

```rust
use adk_gemini::Gemini;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let client = Gemini::new(api_key)?;
    
    let response = client
        .generate_content()
        .with_user_message("Hello, Gemini!")
        .execute()
        .await?;
    
    println!("{}", response.text());
    Ok(())
}
```

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

### Grounding Metadata

Access Google Search grounding results:

```rust
use adk_gemini::{Gemini, GroundingMetadata, GroundingChunk, WebGroundingChunk};

// Access grounding metadata from responses
if let Some(grounding) = response.candidates.first()
    .and_then(|c| c.grounding_metadata.as_ref()) 
{
    if let Some(queries) = &grounding.web_search_queries {
        println!("Searched: {:?}", queries);
    }
    if let Some(chunks) = &grounding.grounding_chunks {
        for chunk in chunks {
            if let Some(web) = &chunk.web {
                println!("Source: {} - {}", web.title, web.uri);
            }
        }
    }
}
```

## 📚 Examples

See the `examples/` directory for comprehensive usage examples covering:

- Basic content generation
- Streaming responses
- Function calling & tools
- Google Search grounding
- Thinking mode (Gemini 2.5)
- Image and speech generation
- Batch processing
- Content caching

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

Original work Copyright (c) 2024 [@flachesis](https://github.com/flachesis)  
Modifications Copyright (c) 2024 Zavora AI

## 🙏 Acknowledgments

- **[@flachesis](https://github.com/flachesis)** - Creator and maintainer of the original [gemini-rust](https://github.com/flachesis/gemini-rust) library
- **[@npatsakula](https://github.com/npatsakula)** - Major contributions to the upstream project
- Google for providing the Gemini API
- The Rust community for excellent async and HTTP libraries
- All contributors to both gemini-rust and adk-rust projects
