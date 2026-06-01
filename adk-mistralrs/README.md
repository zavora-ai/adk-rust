# adk-mistralrs

Native [mistral.rs](https://github.com/EricLBuehler/mistral.rs) integration for ADK-Rust, providing blazingly fast local LLM inference without external dependencies like Ollama.

Uses **mistral.rs v0.8** from crates.io with support for **Gemma 4**, **Qwen 3.5**, **Voxtral**, **Llama 4**, **GPT-OSS**, MXFP4 quantization, agentic runtime, and 50+ model architectures.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-mistralrs = "0.10"
```

### With Hardware Acceleration

```toml
# macOS with Metal
adk-mistralrs = { version = "0.10", features = ["metal"] }

# NVIDIA GPU with CUDA
adk-mistralrs = { version = "0.10", features = ["cuda"] }

# CUDA with Flash Attention
adk-mistralrs = { version = "0.10", features = ["flash-attn"] }

# Multi-GPU via NCCL
adk-mistralrs = { version = "0.10", features = ["nccl"] }

# Intel MKL
adk-mistralrs = { version = "0.10", features = ["mkl"] }
```

## Features

- **Native Rust Integration**: Direct embedding of mistral.rs, no daemon required
- **ISQ (In-Situ Quantization)**: Quantize models on-the-fly at load time
- **PagedAttention**: Memory-efficient attention for longer contexts
- **Multi-Device Support**: CPU, CUDA, Metal acceleration with multi-GPU splitting
- **Multimodal**: Vision, speech, diffusion, and embedding model support
- **LoRA/X-LoRA**: Adapter support with hot-swapping
- **Tool Calling**: Full function calling support via ADK interface
- **MCP Integration**: Connect to Model Context Protocol servers for external tools
- **UQFF Support**: Load pre-quantized models for faster startup

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-core = "0.10"
adk-agent = "0.10"
adk-mistralrs = "0.10"
```

### Feature Flags

| Feature | Description |
|---------|-------------|
| `metal` | Apple Metal acceleration (macOS) |
| `cuda` | NVIDIA CUDA acceleration |
| `flash-attn` | Flash Attention (requires CUDA) |
| `cudnn` | cuDNN acceleration |
| `mkl` | Intel MKL acceleration |
| `accelerate` | Apple Accelerate framework |
| `nccl` | Multi-GPU via NCCL (requires CUDA) |
| `ring` | Ring distributed backend |
| `reqwest` | URL-based image loading |

## Quick Start

### Basic Text Generation

```rust
use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource};
use adk_core::{Llm, LlmRequest, Content};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load a model from HuggingFace
    let model = MistralRsModel::from_hf("microsoft/Phi-3.5-mini-instruct").await?;
    
    // Create a request
    let request = LlmRequest {
        contents: vec![Content::new("user").with_text("Hello, how are you?")],
        ..Default::default()
    };
    
    // Generate response
    let mut stream = model.generate_content(request, false).await?;
    
    use futures::StreamExt;
    while let Some(response) = stream.next().await {
        if let Ok(resp) = response {
            if let Some(content) = resp.content {
                for part in content.parts {
                    if let Some(text) = part.text() {
                        print!("{}", text);
                    }
                }
            }
        }
    }
    
    Ok(())
}
```

### Load from GGUF File

```rust
use adk_mistralrs::MistralRsModel;

let model = MistralRsModel::from_gguf("/path/to/model.gguf").await?;
```

### With ISQ Quantization

Quantize models on-the-fly to reduce memory usage:

```rust
use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource, QuantizationLevel};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("mistralai/Mistral-7B-v0.1"))
    .isq(QuantizationLevel::Q4_0)  // 4-bit quantization
    .paged_attention(true)         // Memory-efficient attention
    .build();

let model = MistralRsModel::new(config).await?;
```

### With Tool Calling

```rust
use adk_mistralrs::MistralRsModel;
use adk_core::{Llm, LlmRequest, Content};
use serde_json::json;

let model = MistralRsModel::from_hf("mistralai/Mistral-7B-Instruct-v0.3").await?;

let tools = json!({
    "get_weather": {
        "description": "Get current weather for a location",
        "parameters": {
            "type": "object",
            "properties": {
                "location": { "type": "string" }
            },
            "required": ["location"]
        }
    }
});

let request = LlmRequest {
    contents: vec![Content::new("user").with_text("What's the weather in Tokyo?")],
    tools: Some(serde_json::from_value(tools)?),
    ..Default::default()
};

let response = model.generate_content(request, false).await?;
```

### With LoRA Adapters

```rust
use adk_mistralrs::{MistralRsAdapterModel, AdapterConfig, MistralRsConfig, ModelSource};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
    .adapter(AdapterConfig::lora("username/my-lora-adapter"))
    .build();

let model = MistralRsAdapterModel::new(config).await?;

// List available adapters
println!("Adapters: {:?}", model.available_adapters());

// Swap adapters at runtime
model.swap_adapter("another-adapter").await?;
```

### Multi-Adapter LoRA

```rust
use adk_mistralrs::{MistralRsAdapterModel, AdapterConfig, MistralRsConfig, ModelSource};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
    .adapter(AdapterConfig::lora_multi(vec![
        "adapter1",
        "adapter2",
        "adapter3",
    ]))
    .build();

let model = MistralRsAdapterModel::new(config).await?;
```

### Vision Models

```rust
use adk_mistralrs::{MistralRsVisionModel, MistralRsConfig, ModelSource, ModelArchitecture};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("llava-hf/llava-1.5-7b-hf"))
    .architecture(ModelArchitecture::Vision)
    .build();

let model = MistralRsVisionModel::new(config).await?;

// Generate with image
let response = model.generate_with_image(
    "What's in this image?",
    "/path/to/image.jpg"
).await?;
```

### Speech Models (Text-to-Speech)

```rust
use adk_mistralrs::{MistralRsSpeechModel, SpeechConfig, VoiceConfig, ModelSource};

let config = SpeechConfig::builder()
    .model_source(ModelSource::huggingface("nari-labs/Dia-1.6B"))
    .voice(VoiceConfig::new().with_speed(1.0))
    .build();

let model = MistralRsSpeechModel::new(config).await?;

// Generate speech from text
let audio = model.generate_speech("Hello, world!").await?;

// Save as WAV file
let wav_bytes = audio.to_wav_bytes()?;
std::fs::write("output.wav", wav_bytes)?;

// Multi-speaker dialogue
let dialogue = model.generate_dialogue(
    "[S1] Hello! How are you? [S2] I'm doing great, thanks!"
).await?;
```

### Diffusion Models (Image Generation)

```rust
use adk_mistralrs::{MistralRsDiffusionModel, DiffusionConfig, DiffusionParams, ModelSource};

let config = DiffusionConfig::builder()
    .model_source(ModelSource::huggingface("black-forest-labs/FLUX.1-schnell"))
    .build();

let model = MistralRsDiffusionModel::new(config).await?;

// Generate an image
let params = DiffusionParams::new().with_size(1024, 1024);
let image = model.generate_image(
    "A beautiful sunset over mountains",
    params,
).await?;

println!("Image saved at: {:?}", image.file_path);

// Generate as base64
let image_b64 = model.generate_image_base64(
    "A cat sitting on a windowsill",
    DiffusionParams::default(),
).await?;
```

> **Note:** FLUX models require significant GPU memory (~12-24GB VRAM).

### Embedding Models

```rust
use adk_mistralrs::{MistralRsEmbeddingModel, MistralRsConfig, ModelSource};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("BAAI/bge-small-en-v1.5"))
    .build();

let model = MistralRsEmbeddingModel::new(config).await?;

// Single embedding
let embedding = model.embed("Hello, world!").await?;

// Batch embeddings
let embeddings = model.embed_batch(vec![
    "First text",
    "Second text",
    "Third text",
]).await?;
```

### Multi-Model Server

```rust
use adk_mistralrs::{MistralRsMultiModel, MultiModelConfig};

// Load from TOML configuration
let multi_model = MistralRsMultiModel::from_config("models.toml").await?;

// Or configure programmatically
let config = MultiModelConfig::new()
    .add_model("phi", ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
    .add_model("mistral", ModelSource::huggingface("mistralai/Mistral-7B-v0.1"))
    .default_model("phi");

let multi_model = MistralRsMultiModel::new(config).await?;

// Route to specific model
let response = multi_model.generate("phi", request).await?;
```

### Multi-GPU Model Splitting

```rust
use adk_mistralrs::{MistralRsConfig, ModelSource, DeviceConfig, Device, LayerDeviceRange};

// Split a 32-layer model across 2 GPUs
let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("meta-llama/Llama-2-70b-hf"))
    .device(DeviceConfig::multi_gpu(vec![
        LayerDeviceRange::new(0, 16, Device::Cuda(0)),
        LayerDeviceRange::new(16, 32, Device::Cuda(1)),
    ]))
    .build();

let model = MistralRsModel::new(config).await?;
```

### UQFF Pre-Quantized Models

Load pre-quantized models for faster startup:

```rust
use adk_mistralrs::MistralRsModel;

let model = MistralRsModel::from_uqff(
    "EricB/Phi-3.5-mini-instruct-UQFF",
    vec!["phi3.5-mini-instruct-q8_0.uqff".into()]
).await?;
```

### MCP Client Integration

Connect to MCP servers for external tools:

```rust
use adk_mistralrs::{MistralRsConfig, ModelSource, McpClientConfig, McpServerConfig};

let config = MistralRsConfig::builder()
    .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
    .mcp_client(McpClientConfig::with_server(
        McpServerConfig::process("Filesystem", "mcp-server-filesystem")
            .with_args(vec!["--root".to_string(), "/tmp".to_string()])
    ))
    .build();

let model = MistralRsModel::new(config).await?;
```

## Supported Models

### Text Models (21 architectures)
| Architecture | Example Model | Notes |
|---|---|---|
| Mistral | mistralai/Mistral-7B-Instruct-v0.3 | |
| Gemma | google/gemma-7b-it | |
| Gemma2 | google/gemma-2-9b-it | |
| Mixtral | mistralai/Mixtral-8x7B-Instruct-v0.1 | MoE |
| Llama | meta-llama/Llama-3.1-8B-Instruct | Llama 2/3/3.1 |
| Phi2 | microsoft/phi-2 | |
| Phi3 | microsoft/Phi-3-medium-4k-instruct | |
| Phi3_5MoE | microsoft/Phi-3.5-MoE-instruct | MoE |
| Qwen2 | Qwen/Qwen2-7B-Instruct | |
| Qwen3 | Qwen/Qwen3-4B | Latest |
| Qwen3Moe | Qwen/Qwen3-30B-A3B | MoE |
| Qwen3Next | Qwen/Qwen3-Next-80B-A3B-Instruct | MoE |
| DeepSeekV2 | deepseek-ai/DeepSeek-V2-Chat | |
| DeepSeekV3 | deepseek-ai/DeepSeek-V3 | |
| GLM4 | zai-org/GLM-4-32B-0414 | |
| GLM4Moe | zai-org/GLM-4.7 | MoE |
| GLM4MoeLite | zai-org/GLM-4.7-Flash | MoE |
| SmolLm3 | HuggingFaceTB/SmolLM3-3B | |
| GraniteMoeHybrid | ibm-granite/granite-4.0-micro | Hybrid |
| GptOss | openai/gpt-oss-20b | |
| Starcoder2 | bigcode/starcoder2-7b | Code |

### Multimodal Models (20 architectures)
| Architecture | Example Model | Modalities |
|---|---|---|
| **Gemma4** | google/gemma-4-E4B-it | Text, image, audio, video |
| Gemma3 | google/gemma-3-12b-it | Text, image |
| Gemma3n | google/gemma-3n-E4B-it | Text, image, audio, video |
| **Qwen3_5** | Qwen/Qwen3.5-27B | Text, image |
| Qwen3_5Moe | Qwen/Qwen3.5-35B-A3B | Text, image |
| Qwen3VL | Qwen/Qwen3-VL-4B-Instruct | Text, image, video |
| Qwen3VLMoE | Qwen/Qwen3-VL-235B-A22B-Instruct | Text, image, video |
| Qwen2VL | Qwen/Qwen2-VL-7B-Instruct | Text, image, video |
| Qwen2_5VL | Qwen/Qwen2.5-VL-7B-Instruct | Text, image, video |
| **Llama4** | meta-llama/Llama-4-Scout-17B-16E-Instruct | Text, image |
| **Mistral3** | mistralai/Mistral-Small-3.2-24B-Instruct-2506 | Text, image |
| **Voxtral** | mistralai/Voxtral-Mini-3B-2507 | Text, audio |
| VLlama | meta-llama/Llama-3.2-11B-Vision-Instruct | Text, image |
| Phi3V | microsoft/Phi-3.5-vision-instruct | Text, image |
| Phi4MM | microsoft/Phi-4-multimodal-instruct | Text, image, audio |
| MiniCpmO | openbmb/MiniCPM-o-2_6 | Text, image, audio |
| LLaVANext | llava-hf/llava-v1.6-mistral-7b-hf | Text, image |
| LLaVA | llava-hf/llava-1.5-7b-hf | Text, image |
| Idefics2 | HuggingFaceM4/idefics2-8b | Text, image |
| Idefics3 | HuggingFaceM4/Idefics3-8B-Llama3 | Text, image |

### Speech Models
| Architecture | Example Model | Capability |
|---|---|---|
| Dia | nari-labs/Dia-1.6B | Text-to-speech |

### Image Generation Models
| Architecture | Example Model | Notes |
|---|---|---|
| Flux | black-forest-labs/FLUX.1-schnell | |
| FluxOffloaded | black-forest-labs/FLUX.1-schnell | CPU offload for low VRAM |

### Embedding Models
| Architecture | Example Model |
|---|---|
| EmbeddingGemma | google/embeddinggemma-300m |
| Qwen3Embedding | Qwen/Qwen3-Embedding-0.6B |

## Configuration Reference

### MistralRsConfig

| Field | Type | Description |
|-------|------|-------------|
| `model_source` | `ModelSource` | HuggingFace ID, local path, GGUF, or UQFF |
| `architecture` | `ModelArchitecture` | Plain, Vision, Diffusion, Speech, Embedding |
| `dtype` | `DataType` | F32, F16, BF16, Auto |
| `device` | `DeviceConfig` | CPU, CUDA, Metal, Auto |
| `isq` | `Option<IsqConfig>` | In-situ quantization settings |
| `adapter` | `Option<AdapterConfig>` | LoRA/X-LoRA adapter config |
| `temperature` | `Option<f32>` | Sampling temperature |
| `top_p` | `Option<f32>` | Nucleus sampling |
| `top_k` | `Option<i32>` | Top-k sampling |
| `max_tokens` | `Option<i32>` | Maximum output tokens |
| `num_ctx` | `Option<usize>` | Context length |
| `paged_attention` | `bool` | Enable PagedAttention |
| `chat_template` | `Option<String>` | Custom chat template |

### Quantization Levels

| Level | Description | Memory Reduction |
|-------|-------------|------------------|
| `Q4_0` | 4-bit (variant 0) | ~75% |
| `Q4_1` | 4-bit (variant 1) | ~75% |
| `Q5_0` | 5-bit (variant 0) | ~69% |
| `Q5_1` | 5-bit (variant 1) | ~69% |
| `Q8_0` | 8-bit (variant 0) | ~50% |
| `Q8_1` | 8-bit (variant 1) | ~50% |
| `Q2K` | 2-bit K-quant | ~88% |
| `Q3K` | 3-bit K-quant | ~81% |
| `Q4K` | 4-bit K-quant | ~75% |
| `Q5K` | 5-bit K-quant | ~69% |
| `Q6K` | 6-bit K-quant | ~63% |

## Comparison with Ollama

| Feature | adk-mistralrs | adk-model (Ollama) |
|---------|---------------|-------------------|
| Daemon Required | No | Yes |
| ISQ Quantization | Yes | No |
| PagedAttention | Yes | Limited |
| Multi-GPU Splitting | Yes | Limited |
| LoRA Hot-Swap | Yes | No |
| X-LoRA | Yes | No |
| Vision Models | Yes | Yes |
| Speech Models | Yes | No |
| Diffusion Models | Yes | No |
| Embedding Models | Yes | Yes |
| MCP Integration | Yes | Via adk-tool |
| UQFF Support | Yes | No |
| MatFormer | Yes | No |

## Publishing

As of v0.10.0, `adk-mistralrs` is publishable to crates.io. The blocker (mistral.rs using git dependencies) was resolved when mistral.rs published v0.8.0 to crates.io in April 2026.

## Examples

See the `examples/` directory for complete working examples:

- `mistralrs_basic` - Basic text generation
- `mistralrs_tools` - Function calling
- `mistralrs_vision` - Image understanding
- `mistralrs_isq` - In-situ quantization
- `mistralrs_lora` - LoRA adapter usage
- `mistralrs_multimodel` - Multi-model serving
- `mistralrs_mcp` - MCP client integration
- `mistralrs_speech` - Text-to-speech synthesis
- `mistralrs_diffusion` - Image generation with FLUX

## License

MIT License - see [LICENSE](../LICENSE)

## Benchmarks

The crate includes benchmarks for measuring performance of configuration, error handling, and type conversions. Real inference benchmarks require model downloads.

### Running Benchmarks

```bash
# Run configuration and conversion benchmarks (no model required)
cargo bench -p adk-mistralrs

# Run with inference benchmarks (requires model downloads)
cargo bench -p adk-mistralrs --features bench-inference
```

### Benchmark Results

Configuration and conversion operations are highly optimized:

| Operation | Time |
|-----------|------|
| Config builder (simple) | ~50 ns |
| Config builder (full) | ~200 ns |
| Error creation | ~100-300 ns |
| Content to message | ~500 ns |
| Tool conversion | ~1 μs |
| MIME type detection | ~10 ns |

### Performance Tips

1. **Use ISQ quantization** for memory-constrained environments
2. **Enable PagedAttention** for long context windows
3. **Use UQFF models** for faster startup (skip quantization step)
4. **Batch embeddings** when processing multiple texts
5. **Reuse model instances** - loading is expensive

## Diagnostic Logging

The crate uses `tracing` for structured logging. Enable logging in your application:

```rust
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

tracing_subscriber::registry()
    .with(fmt::layer())
    .with(EnvFilter::from_default_env())
    .init();

// Set log level via environment variable
// RUST_LOG=adk_mistralrs=debug cargo run
```

### Log Levels

- `INFO`: Model loading, inference completion with timing
- `DEBUG`: Configuration details, intermediate steps
- `WARN`: Non-fatal issues, fallback behaviors
- `ERROR`: Operation failures

### Timing Information

The crate automatically logs timing information for:
- Model loading (with configuration details)
- Inference (with token counts and throughput)
- Embedding generation (with batch size)
- Image generation (with dimensions)
- Speech generation (with realtime factor)
