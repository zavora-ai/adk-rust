# mistral.rs Integration

Run LLMs locally with native Rust inference - no external servers, no API keys.

---

## What is mistral.rs?

[mistral.rs](https://github.com/EricLBuehler/mistral.rs) is a high-performance Rust inference engine that runs LLMs directly on your hardware. ADK-Rust integrates it through the `adk-mistralrs` crate, pinned to **v0.8.0** with **Gemma 4** support.

> **Key highlights**:
> - 🦀 **Native Rust** - No Python, no external servers
> - 🔒 **Fully offline** - No API keys or internet required
> - ⚡ **Hardware acceleration** - CUDA, Metal (3.1 bfloat16), CPU optimizations
> - 📦 **Quantization** - ISQ, MXFP4, GGUF, UQFF for running large models on limited hardware
> - 🔧 **LoRA adapters** - Fine-tuned model support with hot-swapping
> - 👁️ **Multimodal** - Vision, audio, and video understanding (Gemma 4, Qwen 3 VL)
> - 🎤 **Speech** - Voxtral real-time speech recognition
> - 🎯 **Multi-model** - Serve multiple models from one instance

---

## Step 1: Add Dependencies

`adk-mistralrs` is published to crates.io as a workspace member. Add it as a standard dependency:

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

**What happens**:
1. First run downloads the model from HuggingFace (~2-8GB depending on model)
2. Model is cached locally in `~/.cache/huggingface/`
3. Subsequent runs load from cache instantly

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

**Quantization levels**:

| Level | Memory Reduction | Quality | Best For |
|-------|-----------------|---------|----------|
| `Q4_0` | ~75% | Good | Limited RAM (8GB) |
| `Q4_1` | ~70% | Better | Balanced |
| `Q8_0` | ~50% | High | Quality-focused |
| `Q8_1` | ~50% | Highest | Best quality |

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

**Hot-swap adapters at runtime**:
```rust
model.swap_adapter("another-adapter").await?;
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
    let response = model.generate_with_image("Describe this image.", vec![image]).await?;

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

    // Route to specific model
    // multi.generate_with_model(Some("gemma"), request, false).await?;

    Ok(())
}
```

---

## Model Sources

### HuggingFace Hub (Default)

```rust
ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct")
```

### Local Directory

```rust
ModelSource::local("/path/to/model")
```

### Pre-Quantized GGUF

```rust
ModelSource::gguf("/path/to/model.Q4_K_M.gguf")
```

---

## Recommended Models

| Model | Size | RAM Needed | Best For |
|-------|------|------------|----------|
| **`google/gemma-4-4b-it`** | **4B** | **8GB** | **Gemma 4 — multimodal (text, image, audio, video), Apache 2.0** |
| **`google/gemma-4-12b-it`** | **12B** | **24GB** | **Gemma 4 — high-quality multimodal reasoning** |
| `microsoft/Phi-3.5-mini-instruct` | 3.8B | 8GB | Fast, general purpose |
| `microsoft/Phi-3.5-vision-instruct` | 4.2B | 10GB | Vision + text |
| `Qwen/Qwen3-4B` | 4B | 8GB | Multilingual, coding, reasoning |
| `Qwen/Qwen3.5-7B-Instruct` | 7B | 16GB | Latest Qwen with strong reasoning |
| `google/gemma-2-2b-it` | 2B | 4GB | Lightweight |
| `mistralai/Mistral-7B-Instruct-v0.3` | 7B | 16GB | High quality |

---

## Hardware Acceleration

### macOS (Apple Silicon)

```toml
adk-mistralrs = { version = "3.0.0", features = ["metal"] }
```

Metal acceleration is automatic on M1/M2/M3 Macs.

### NVIDIA GPU

```toml
adk-mistralrs = { version = "3.0.0", features = ["cuda"] }
```

Requires CUDA toolkit 11.8+.

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

**Out of Memory**
```rust
// Enable quantization
.isq(QuantizationLevel::Q4_0)
// Enable paged attention
.paged_attention(true)
```

**Slow First Load**
- First run downloads the model (~2-8GB)
- Subsequent runs use cached model

**Model Not Found**
- Check HuggingFace model ID is correct
- Ensure internet connection for first download

---

## Related

- [Model Providers](providers.md) - Cloud LLM providers
- [Ollama](ollama.md) - Alternative local model server
- [LlmAgent](../agents/llm-agent.md) - Using models with agents

---

**Previous**: [← Ollama (Local)](ollama.md) | **Next**: [Function Tools →](../tools/function-tools.md)
