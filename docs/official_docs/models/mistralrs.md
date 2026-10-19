# mistral.rs Integration

Run LLMs locally with native Rust inference - no external servers, no API keys.

---

## What is mistral.rs?

[mistral.rs](https://github.com/EricLBuehler/mistral.rs) is a Rust inference engine that runs LLMs on your own hardware. ADK-Rust integrates it through the `adk-mistralrs` crate, which depends on `mistralrs` 0.8.

| Capability | Types |
|------------|-------|
| Text models as an `Llm` | `MistralRsModel` |
| In-situ quantization (ISQ) and PagedAttention | `MistralRsConfig::builder().isq(...)`, `.paged_attention(true)` |
| LoRA and X-LoRA adapters, swapped at runtime | `MistralRsAdapterModel`, `AdapterConfig` |
| Vision models | `MistralRsVisionModel` |
| Several models behind one `Llm` | `MistralRsMultiModel` |
| Speech generation, image generation, embeddings | `MistralRsSpeechModel`, `MistralRsDiffusionModel`, `MistralRsEmbeddingModel` |
| CPU, CUDA, and Metal | CPU by default; `cuda` and `metal` features |

---

## Step 1: Add Dependencies

`adk-mistralrs` is published to crates.io:

```toml
[package]
name = "my-local-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
adk-mistralrs = "3.0.0"
adk-agent = "3.0.0"
adk-rust = "3.0.0"
tokio = { version = "1", features = ["full"] }
anyhow = "1.0"
image = "0.25" # only for the vision example
```

For hardware acceleration, add feature flags:

```toml
# macOS with Apple Silicon
adk-mistralrs = { version = "3.0.0", features = ["metal"] }

# NVIDIA GPU (requires CUDA toolkit)
adk-mistralrs = { version = "3.0.0", features = ["cuda"] }
```

---

## Step 2: Basic Example

Load a model from HuggingFace and run it locally:

```rust
use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{Llm, MistralRsConfig, MistralRsModel, ModelSource};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load model from HuggingFace (downloads on first run)
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .build();

    println!("Loading model (this may take a while on first run)...");
    let model = MistralRsModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    // Create agent
    let agent = LlmAgentBuilder::new("local_assistant")
        .description("Local AI assistant powered by mistral.rs")
        .instruction("You are a helpful assistant running locally. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Run interactive chat
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

The first run downloads the model from the Hugging Face Hub into the local Hugging Face cache;
later runs load it from the cache.

---

## Step 3: Reduce Memory with Quantization

Large models need lots of RAM. Use ISQ (In-Situ Quantization) to reduce memory:

```rust
use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{Llm, MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load model with 4-bit quantization for reduced memory
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .isq(QuantizationLevel::Q4_0) // 4-bit quantization
        .paged_attention(true) // Memory-efficient attention
        .build();

    println!("Loading quantized model...");
    let model = MistralRsModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    let agent = LlmAgentBuilder::new("quantized_assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

`QuantizationLevel` has `Q4_0`, `Q4_1`, `Q5_0`, `Q5_1`, `Q8_0`, `Q8_1`, and the K-quants
`Q2K`, `Q3K`, `Q4K`, `Q5K`, and `Q6K`; fewer bits use less memory at some cost in quality.

---

## Step 4: LoRA Adapters (Fine-Tuned Models)

Load models with LoRA adapters for specialized tasks:

```rust
use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{AdapterConfig, Llm, MistralRsAdapterModel, MistralRsConfig, ModelSource};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load base model with LoRA adapter
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("meta-llama/Llama-3.2-3B-Instruct"))
        .adapter(AdapterConfig::lora("username/my-lora-adapter"))
        .build();

    println!("Loading model with LoRA adapter...");
    let model = MistralRsAdapterModel::new(config).await?;
    println!("Model loaded: {}", model.name());
    println!("Available adapters: {:?}", model.available_adapters());

    let agent = LlmAgentBuilder::new("lora_assistant")
        .instruction("You are a helpful assistant with specialized knowledge.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

Swap the active adapter at runtime without reloading the base model:

```rust
use adk_mistralrs::MistralRsAdapterModel;

async fn swap(model: &MistralRsAdapterModel) -> anyhow::Result<()> {
    model.swap_adapter("another-adapter").await?;
    Ok(())
}
```

---

## Step 5: Vision Models (Image Understanding)

Process images with vision-language models:

```rust
use adk_mistralrs::{Llm, MistralRsConfig, MistralRsVisionModel, ModelSource};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-vision-instruct"))
        .build();

    println!("Loading vision model...");
    let model = MistralRsVisionModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    // Analyze an image
    let image = image::open("photo.jpg")?;
    let description = model.generate_with_image("Describe this image.", vec![image]).await?;
    println!("{description}");

    Ok(())
}
```

---

## Step 6: Multi-Model Serving

Serve multiple models from a single instance:

```rust
use adk_mistralrs::{MistralRsConfig, MistralRsMultiModel, ModelSource};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let multi = MistralRsMultiModel::new();

    // Add models
    let phi_config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .build();
    multi.add_model("phi", phi_config).await?;

    let gemma_config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("google/gemma-2-2b-it"))
        .build();
    multi.add_model("gemma", gemma_config).await?;

    // Set default and route requests
    multi.set_default("phi").await?;
    println!("Available models: {:?}", multi.model_names().await);

    // `multi.generate_with_model(Some("gemma"), request, false)` routes one request;
    // as an `Llm`, the multi-model serves the default.

    Ok(())
}
```

---

## Model Sources

```rust
use adk_mistralrs::ModelSource;

fn sources() -> [ModelSource; 4] {
    [
        // Hugging Face Hub (downloaded on first use)
        ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"),
        // A local model directory
        ModelSource::local("/path/to/model"),
        // A pre-quantized GGUF file
        ModelSource::gguf("/path/to/model.Q4_K_M.gguf"),
        // A UQFF file
        ModelSource::uqff("/path/to/model.uqff"),
    ]
}
```

---

## Models

Any model ID the mistral.rs 0.8 engine supports can be loaded. The examples on this page use:

| Model | Use |
|-------|-----|
| `microsoft/Phi-3.5-mini-instruct` | Text |
| `microsoft/Phi-3.5-vision-instruct` | Vision and text |
| `google/gemma-2-2b-it` | Text |
| `meta-llama/Llama-3.2-3B-Instruct` | Text, base for LoRA adapters |

---

## Hardware Acceleration

### macOS (Apple Silicon)

```toml
adk-mistralrs = { version = "3.0.0", features = ["metal"] }
```

`metal` builds mistral.rs with Metal support for Apple Silicon.

### NVIDIA GPU

```toml
adk-mistralrs = { version = "3.0.0", features = ["cuda"] }
```

`cuda` needs the CUDA toolkit installed. `flash-attn`, `cudnn`, `nccl`, `mkl`, and
`accelerate` are also available.

### CPU Only

No features needed - CPU is the default.

---

## Validate the Crate

```bash
cargo build -p adk-mistralrs
cargo build -p adk-mistralrs --features metal
```

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| Out of memory | Quantize with `.isq(QuantizationLevel::Q4_0)` and enable `.paged_attention(true)` on the config builder |
| Slow first load | The first run downloads the model; later runs load it from the cache |
| Model not found | Check the Hugging Face model ID, and the network connection for the first download |

---

## Related

- [Model Providers](providers.md) - Cloud LLM providers
- [Ollama](ollama.md) - Alternative local model server
- [LlmAgent](../agents/llm-agent.md) - Using models with agents

---

**Previous**: [← Ollama (Local)](ollama.md) | **Next**: [Function Tools →](../tools/function-tools.md)
