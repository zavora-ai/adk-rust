# adk-mistralrs

Native [mistral.rs](https://github.com/EricLBuehler/mistral.rs) integration for ADK-Rust: local
LLM inference inside your process, without a separate server such as Ollama.

[![Crates.io](https://img.shields.io/crates/v/adk-mistralrs.svg)](https://crates.io/crates/adk-mistralrs)
[![Documentation](https://docs.rs/adk-mistralrs/badge.svg)](https://docs.rs/adk-mistralrs)
[![License](https://img.shields.io/crates/l/adk-mistralrs.svg)](../LICENSE)

The crate uses mistral.rs 0.8 from crates.io, whose loaders cover Gemma 4, Qwen 3.5, Voxtral,
Llama 4, GPT-OSS, and the other architectures listed under [Supported models](#supported-models).

## Installation

```toml
[dependencies]
adk-mistralrs = "3.0.0"
```

### Feature flags

No feature is enabled by default, so the default build runs on the CPU.

| Feature | Enables |
|---------|---------|
| `metal` | Apple Metal acceleration |
| `accelerate` | Apple Accelerate framework |
| `cuda` | NVIDIA CUDA acceleration |
| `cudnn` | cuDNN acceleration |
| `flash-attn` | Flash Attention (implies `cuda`) |
| `nccl` | Multi-GPU through NCCL (implies `cuda`) |
| `mkl` | Intel MKL acceleration |
| `ring` | Ring distributed backend |
| `reqwest` | Image loading from URLs (`image_from_url`, `image_from_url_async`) |
| `bench-inference` | Inference benchmarks that download models |

```toml
# macOS with Metal
adk-mistralrs = { version = "3.0.0", features = ["metal"] }

# NVIDIA GPU with CUDA
adk-mistralrs = { version = "3.0.0", features = ["cuda"] }
```

## Features

- **In-process inference** — mistral.rs runs inside the application; no daemon
- **ISQ (in-situ quantization)** — quantize weights while loading
- **PagedAttention** — memory-efficient attention for long contexts
- **Devices** — CPU, CUDA, and Metal, with layer ranges split across GPUs
- **Multimodal** — vision, speech, image generation, and embedding models
- **LoRA and X-LoRA** — adapters with runtime swapping
- **Tool calling** — through the ADK `Llm` interface
- **MCP client** — mistral.rs connects to MCP servers for tools
- **UQFF** — load pre-quantized models

`MistralRsModel`, `MistralRsAdapterModel`, `MistralRsVisionModel`, and `MistralRsMultiModel`
implement `adk_core::Llm`, so they work with `LlmAgent` and the runner.

## Quick start

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_mistralrs::MistralRsModel;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Downloads the model from Hugging Face on first use.
    let model = MistralRsModel::from_hf("Qwen/Qwen3-4B").await?;

    let request = LlmRequest::new(
        "Qwen/Qwen3-4B",
        vec![Content::new("user").with_text("Name one planet in a short sentence.")],
    );
    let mut responses = model.generate_content(request, false).await?;
    while let Some(response) = responses.next().await {
        if let Some(content) = response?.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    print!("{text}");
                }
            }
        }
    }
    println!();
    Ok(())
}
```

## Loading models

```rust
use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A local GGUF file.
    let gguf = MistralRsModel::from_gguf("/path/to/model.gguf").await?;

    // Pre-quantized UQFF files from a Hugging Face repository.
    let uqff = MistralRsModel::from_uqff(
        "EricB/Phi-3.5-mini-instruct-UQFF",
        vec!["phi3.5-mini-instruct-q8_0.uqff".into()],
    )
    .await?;

    // In-situ quantization and PagedAttention.
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("Qwen/Qwen3-4B"))
        .isq(QuantizationLevel::Q4K)
        .paged_attention(true)
        .build();
    let quantized = MistralRsModel::new(config).await?;

    let _ = (gguf, uqff, quantized);
    Ok(())
}
```

## Tool calling

Tools go in `LlmRequest::tools`, keyed by name, with a `description` and JSON Schema
`parameters`:

```rust
use adk_core::{Content, Llm, LlmRequest};
use adk_mistralrs::MistralRsModel;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = MistralRsModel::from_hf("Qwen/Qwen3-4B").await?;

    let mut request = LlmRequest::new(
        "Qwen/Qwen3-4B",
        vec![Content::new("user").with_text("What's the weather in Tokyo?")],
    );
    request.tools.insert(
        "get_weather".to_string(),
        json!({
            "description": "Get current weather for a location",
            "parameters": {
                "type": "object",
                "properties": { "location": { "type": "string" } },
                "required": ["location"]
            }
        }),
    );

    let _responses = model.generate_content(request, false).await?;
    Ok(())
}
```

## LoRA adapters

```rust
use adk_mistralrs::{AdapterConfig, MistralRsAdapterModel, MistralRsConfig, ModelSource};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("meta-llama/Llama-3.1-8B-Instruct"))
        .adapter(AdapterConfig::lora_multi(["username/adapter-a", "username/adapter-b"]))
        .build();

    let model = MistralRsAdapterModel::new(config).await?;
    println!("adapters: {:?}", model.available_adapters());
    model.swap_adapter("username/adapter-b").await?;
    Ok(())
}
```

`AdapterConfig::lora(source)` loads one adapter, and `AdapterConfig::xlora(source, ordering)`
an X-LoRA model.

## Multimodal models

```rust
use adk_mistralrs::{
    DiffusionParams, MistralRsDiffusionModel, MistralRsEmbeddingModel, MistralRsSpeechModel,
    MistralRsVisionModel, image_from_path,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Vision: images go in as `image::DynamicImage` values.
    let vision = MistralRsVisionModel::from_hf("Qwen/Qwen3-VL-4B-Instruct").await?;
    let description =
        vision.generate_with_image("What's in this image?", vec![image_from_path("photo.jpg")?]).await?;
    println!("{description}");

    // Speech: Dia text-to-speech, saved as WAV.
    let speech = MistralRsSpeechModel::from_hf("nari-labs/Dia-1.6B").await?;
    let audio = speech.generate_dialogue("[S1] Hello! How are you? [S2] Doing great, thanks!").await?;
    std::fs::write("dialogue.wav", audio.to_wav_bytes()?)?;

    // Image generation: FLUX needs a GPU with substantial memory.
    let flux = MistralRsDiffusionModel::from_hf("black-forest-labs/FLUX.1-schnell").await?;
    let image = flux.generate_image("A sunset over mountains", DiffusionParams::new().with_size(1024, 1024)).await?;
    println!("image at {:?}", image.file_path);

    // Embeddings.
    let embedder = MistralRsEmbeddingModel::from_hf("google/embeddinggemma-300m").await?;
    let vectors = embedder.embed_batch(["First text", "Second text"]).await?;
    println!("{} vectors", vectors.len());
    Ok(())
}
```

## Multi-model serving

`MistralRsMultiModel` holds several models and routes each request by name; its `Llm`
implementation uses the default model.

```rust
use adk_core::{Content, LlmRequest};
use adk_mistralrs::{MistralRsConfig, MistralRsMultiModel, ModelSource};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let multi = MistralRsMultiModel::new();
    multi
        .add_model("qwen", MistralRsConfig::builder().model_source(ModelSource::huggingface("Qwen/Qwen3-4B")).build())
        .await?;
    multi.set_default("qwen").await?;

    let request = LlmRequest::new("qwen", vec![Content::new("user").with_text("Hello!")]);
    let _responses = multi.generate_with_model(Some("qwen"), request, false).await?;
    Ok(())
}
```

`MistralRsMultiModel::from_config("models.json")` loads the models listed in a JSON file.

## Multi-GPU splitting

```rust
use adk_mistralrs::{Device, DeviceConfig, LayerDeviceRange, MistralRsConfig, ModelSource};

fn main() {
    // Split a 32-layer model across two GPUs.
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("meta-llama/Llama-3.1-8B-Instruct"))
        .device(DeviceConfig::multi_gpu(vec![
            LayerDeviceRange::new(0, 16, Device::Cuda(0)),
            LayerDeviceRange::new(16, 32, Device::Cuda(1)),
        ]))
        .build();
    assert_eq!(config.device.device_count(), 2);
}
```

## MCP client

```rust
use adk_mistralrs::{McpClientConfig, McpServerConfig, MistralRsConfig, ModelSource};

fn main() {
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("Qwen/Qwen3-4B"))
        .mcp_client(McpClientConfig::with_server(
            McpServerConfig::process("Filesystem", "mcp-server-filesystem")
                .with_args(vec!["--root".to_string(), "/tmp".to_string()]),
        ))
        .build();
    assert!(config.mcp_client.is_some());
}
```

## Supported models

These are the loader architectures of mistral.rs 0.8.1, with example models.

### Text models

| Architecture | Example model |
|---|---|
| Mistral | mistralai/Mistral-7B-Instruct-v0.3 |
| Gemma, Gemma2 | google/gemma-2-9b-it |
| Mixtral | mistralai/Mixtral-8x7B-Instruct-v0.1 |
| Llama | meta-llama/Llama-3.1-8B-Instruct |
| Phi2, Phi3, Phi3_5MoE | microsoft/Phi-3.5-MoE-instruct |
| Qwen2, Qwen3, Qwen3Moe, Qwen3Next | Qwen/Qwen3-4B |
| DeepSeekV2, DeepSeekV3 | deepseek-ai/DeepSeek-V3 |
| GLM4, GLM4Moe, GLM4MoeLite | zai-org/GLM-4.7-Flash |
| SmolLm3 | HuggingFaceTB/SmolLM3-3B |
| GraniteMoeHybrid | ibm-granite/granite-4.0-micro |
| GptOss | openai/gpt-oss-20b |
| Starcoder2 | bigcode/starcoder2-7b |

### Multimodal models

| Architecture | Example model | Modalities |
|---|---|---|
| Gemma4 | google/gemma-4-E4B-it | Text, image, audio, video |
| Gemma3, Gemma3n | google/gemma-3n-E4B-it | Text, image (Gemma3n: audio, video) |
| Qwen3_5, Qwen3_5Moe | Qwen/Qwen3.5-27B | Text, image |
| Qwen3VL, Qwen3VLMoE, Qwen2VL, Qwen2_5VL | Qwen/Qwen3-VL-4B-Instruct | Text, image, video |
| Llama4 | meta-llama/Llama-4-Scout-17B-16E-Instruct | Text, image |
| Mistral3 | mistralai/Mistral-Small-3.2-24B-Instruct-2506 | Text, image |
| Voxtral | mistralai/Voxtral-Mini-3B-2507 | Text, audio |
| VLlama | meta-llama/Llama-3.2-11B-Vision-Instruct | Text, image |
| Phi3V, Phi4MM | microsoft/Phi-4-multimodal-instruct | Text, image (Phi4MM: audio) |
| MiniCpmO | openbmb/MiniCPM-o-2_6 | Text, image, audio |
| LLaVA, LLaVANext | llava-hf/llava-v1.6-mistral-7b-hf | Text, image |
| Idefics2, Idefics3 | HuggingFaceM4/Idefics3-8B-Llama3 | Text, image |

### Speech, image generation, and embeddings

| Architecture | Example model |
|---|---|
| Dia (speech) | nari-labs/Dia-1.6B |
| Flux, FluxOffloaded (image generation) | black-forest-labs/FLUX.1-schnell |
| EmbeddingGemma, Qwen3Embedding | google/embeddinggemma-300m |

## Configuration reference

### MistralRsConfig

| Field | Type | Description |
|-------|------|-------------|
| `model_source` | `ModelSource` | `huggingface`, `local`, `gguf`, or `uqff` |
| `architecture` | `ModelArchitecture` | `Plain`, `Vision`, `Diffusion`, `Speech`, `Embedding`, `Lora`, `XLora` |
| `dtype` | `DataType` | `F32`, `F16`, `BF16`, `Auto` |
| `device` | `DeviceConfig` | `Device::Auto`, `Cpu`, `Cuda(index)`, `Metal`, or layer ranges |
| `isq` | `Option<IsqConfig>` | In-situ quantization |
| `adapter` | `Option<AdapterConfig>` | LoRA or X-LoRA adapters |
| `temperature`, `top_p`, `top_k`, `max_tokens` | `Option<_>` | Sampling defaults |
| `num_ctx` | `Option<usize>` | Context length |
| `paged_attention` | `bool` | PagedAttention |
| `topology_path`, `tokenizer_path`, `chat_template` | `Option<_>` | Topology file, tokenizer, chat template |
| `matformer` | `Option<MatFormerConfig>` | MatFormer slicing |
| `mcp_config`, `mcp_client` | `Option<_>` | MCP client from a file or in code |

### Quantization levels

`QuantizationLevel` has `Q4_0`, `Q4_1`, `Q5_0`, `Q5_1`, `Q8_0`, `Q8_1`, `Q2K`, `Q3K`, `Q4K`,
`Q5K`, and `Q6K`.

## Benchmarks

```bash
# Configuration and conversion benchmarks (no model download)
cargo bench -p adk-mistralrs

# Inference benchmarks (downloads models)
cargo bench -p adk-mistralrs --features bench-inference
```

## Diagnostic logging

The crate logs through `tracing`: `info` for model loading and inference completion with timing,
`debug` for configuration details. Enable it with a subscriber and, for example,
`RUST_LOG=adk_mistralrs=debug`.

## Further reading

- [mistral.rs guide](../docs/official_docs/models/mistralrs.md) in the ADK-Rust documentation
- The [adk-playground](https://github.com/zavora-ai/adk-playground) `mistral_quickstart` example

## License

Apache-2.0
