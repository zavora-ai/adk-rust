//! Performance benchmarks for mistral.rs inference.
//!
//! These benchmarks compare mistral.rs performance against Ollama
//! for common inference scenarios.
//!
//! ## Running Benchmarks
//!
//! ```bash
//! # Run all benchmarks (requires models to be downloaded)
//! cargo bench -p adk-mistralrs
//!
//! # Run specific benchmark
//! cargo bench -p adk-mistralrs -- inference
//! ```
//!
//! ## Requirements
//!
//! - For mistral.rs benchmarks: Models will be downloaded from HuggingFace
//! - For Ollama benchmarks: Ollama must be running with models pulled
//!
//! ## Benchmark Scenarios
//!
//! 1. **Short prompt inference**: Single-turn response to a short prompt
//! 2. **Long context inference**: Response with extended context window
//! 3. **Streaming inference**: Time to first token and throughput
//! 4. **Batch inference**: Multiple requests in sequence

use criterion::{Criterion, black_box, criterion_group, criterion_main};

/// Benchmark configuration (used by real_benchmarks feature)
#[cfg(feature = "bench-inference")]
struct BenchConfig {
    /// Short prompt for quick inference tests
    short_prompt: &'static str,
    /// Medium prompt for typical use cases
    medium_prompt: &'static str,
    /// Long prompt for context window tests
    #[allow(dead_code)]
    long_prompt: &'static str,
    /// Number of tokens to generate
    #[allow(dead_code)]
    max_tokens: i32,
}

#[cfg(feature = "bench-inference")]
impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            short_prompt: "What is 2 + 2?",
            medium_prompt: "Explain the concept of recursion in programming with a simple example.",
            long_prompt: include_str!("../tests/fixtures/long_prompt.txt"),
            max_tokens: 100,
        }
    }
}

/// Placeholder benchmark that doesn't require model downloads.
/// Real benchmarks are in the `real_benchmarks` module below.
fn placeholder_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("placeholder");
    group.sample_size(10);

    group.bench_function("noop", |b| {
        b.iter(|| {
            // Placeholder - real benchmarks require model downloads
            black_box(42)
        });
    });

    group.finish();
}

/// Configuration parsing benchmarks (no model required)
fn config_benchmark(c: &mut Criterion) {
    use adk_mistralrs::{Device, DeviceConfig, MistralRsConfig, ModelSource, QuantizationLevel};

    let mut group = c.benchmark_group("config");
    group.sample_size(100);

    group.bench_function("builder_simple", |b| {
        b.iter(|| {
            black_box(
                MistralRsConfig::builder()
                    .model_source(ModelSource::huggingface("test/model"))
                    .build(),
            )
        });
    });

    group.bench_function("builder_full", |b| {
        b.iter(|| {
            black_box(
                MistralRsConfig::builder()
                    .model_source(ModelSource::huggingface("test/model"))
                    .isq(QuantizationLevel::Q4K)
                    .device(DeviceConfig::new(Device::Auto))
                    .paged_attention(true)
                    .temperature(0.7)
                    .top_p(0.9)
                    .max_tokens(2048)
                    .num_ctx(4096)
                    .build(),
            )
        });
    });

    group.finish();
}

/// Error creation benchmarks (no model required)
fn error_benchmark(c: &mut Criterion) {
    use adk_mistralrs::MistralRsError;

    let mut group = c.benchmark_group("error");
    group.sample_size(100);

    group.bench_function("model_load_error", |b| {
        b.iter(|| black_box(MistralRsError::model_load("test/model", "connection timeout")));
    });

    group.bench_function("out_of_memory_error", |b| {
        b.iter(|| {
            black_box(MistralRsError::out_of_memory("loading model", "GPU memory exhausted"))
        });
    });

    group.bench_function("adapter_not_found_error", |b| {
        b.iter(|| {
            black_box(MistralRsError::adapter_not_found(
                "missing-adapter",
                vec!["adapter1".to_string(), "adapter2".to_string(), "adapter3".to_string()],
            ))
        });
    });

    group.finish();
}

/// Type conversion benchmarks (no model required)
fn conversion_benchmark(c: &mut Criterion) {
    use adk_core::{Content, Part};
    use adk_mistralrs::convert::{
        AudioFormat, ImageFormat, content_to_message, tools_to_mistralrs,
    };
    use serde_json::json;

    let mut group = c.benchmark_group("conversion");
    group.sample_size(100);

    // Simple content conversion
    let simple_content = Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "Hello, world!".to_string() }],
    };

    group.bench_function("content_to_message_simple", |b| {
        b.iter(|| black_box(content_to_message(&simple_content)));
    });

    // Complex content with function call
    let complex_content = Content {
        role: "model".to_string(),
        parts: vec![
            Part::Text { text: "I'll help you with that.".to_string() },
            Part::FunctionCall {
                id: Some("call_123".to_string()),
                name: "get_weather".to_string(),
                args: json!({"location": "Tokyo", "units": "celsius"}),
                thought_signature: None,
            },
        ],
    };

    group.bench_function("content_to_message_complex", |b| {
        b.iter(|| black_box(content_to_message(&complex_content)));
    });

    // Tool conversion
    let mut tools = serde_json::Map::new();
    tools.insert(
        "get_weather".to_string(),
        json!({
            "description": "Get weather for a location",
            "parameters": {
                "type": "object",
                "properties": {
                    "location": {"type": "string"},
                    "units": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["location"]
            }
        }),
    );
    tools.insert(
        "search".to_string(),
        json!({
            "description": "Search the web",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }
        }),
    );

    group.bench_function("tools_to_mistralrs", |b| {
        b.iter(|| black_box(tools_to_mistralrs(&tools).unwrap()));
    });

    // MIME type detection
    let mime_types = vec![
        "image/jpeg",
        "image/png",
        "image/webp",
        "image/gif",
        "audio/wav",
        "audio/mp3",
        "audio/flac",
        "text/plain",
    ];

    group.bench_function("image_format_detection", |b| {
        b.iter(|| {
            for mime in &mime_types {
                black_box(ImageFormat::from_mime_type(mime));
            }
        });
    });

    group.bench_function("audio_format_detection", |b| {
        b.iter(|| {
            for mime in &mime_types {
                black_box(AudioFormat::from_mime_type(mime));
            }
        });
    });

    group.finish();
}

/// MCP configuration benchmarks (no model required)
fn mcp_config_benchmark(c: &mut Criterion) {
    use adk_mistralrs::{McpClientConfig, McpServerConfig};

    let mut group = c.benchmark_group("mcp_config");
    group.sample_size(100);

    group.bench_function("server_config_http", |b| {
        b.iter(|| {
            black_box(
                McpServerConfig::http("Test Server", "https://api.example.com/mcp")
                    .with_bearer_token("secret-token")
                    .with_timeout(30),
            )
        });
    });

    group.bench_function("server_config_process", |b| {
        b.iter(|| {
            black_box(
                McpServerConfig::process("Filesystem", "mcp-server-filesystem")
                    .with_args(vec!["--root".to_string(), "/tmp".to_string()])
                    .with_tool_prefix("fs"),
            )
        });
    });

    group.bench_function("client_config_multi_server", |b| {
        b.iter(|| {
            black_box(
                McpClientConfig::new()
                    .add_server(McpServerConfig::http("Server1", "https://api1.example.com"))
                    .add_server(McpServerConfig::http("Server2", "https://api2.example.com"))
                    .add_server(McpServerConfig::process("Local", "mcp-local"))
                    .with_tool_timeout(60)
                    .with_max_concurrent_calls(5),
            )
        });
    });

    // Validation benchmark
    let valid_config =
        McpClientConfig::with_server(McpServerConfig::http("Test", "https://api.example.com"));

    group.bench_function("config_validation", |b| {
        b.iter(|| black_box(valid_config.validate().is_ok()));
    });

    group.finish();
}

// Register benchmark groups
criterion_group!(
    benches,
    placeholder_benchmark,
    config_benchmark,
    error_benchmark,
    conversion_benchmark,
    mcp_config_benchmark,
);

criterion_main!(benches);

// ============================================================================
// Real inference benchmarks (require model downloads)
// These are in a separate module and marked with #[ignore] equivalent
// ============================================================================

#[cfg(feature = "bench-inference")]
mod real_benchmarks {
    use super::*;
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};
    use tokio::runtime::Runtime;

    /// Benchmark inference with a small model.
    /// Requires: microsoft/Phi-3.5-mini-instruct to be accessible
    fn inference_benchmark(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        // Load model once for all benchmarks
        let model =
            rt.block_on(async { MistralRsModel::from_hf("microsoft/Phi-3.5-mini-instruct").await });

        let model = match model {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Failed to load model for benchmarks: {}", e);
                return;
            }
        };

        let config = BenchConfig::default();

        let mut group = c.benchmark_group("inference");
        group.sample_size(10);
        group.measurement_time(Duration::from_secs(30));

        // Short prompt benchmark
        group.bench_function("short_prompt", |b| {
            let request = LlmRequest {
                contents: vec![Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text { text: config.short_prompt.to_string() }],
                }],
                ..Default::default()
            };

            b.to_async(&rt).iter(|| async {
                let response = model.generate_content(request.clone(), false).await;
                black_box(response)
            });
        });

        // Medium prompt benchmark
        group.bench_function("medium_prompt", |b| {
            let request = LlmRequest {
                contents: vec![Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text { text: config.medium_prompt.to_string() }],
                }],
                ..Default::default()
            };

            b.to_async(&rt).iter(|| async {
                let response = model.generate_content(request.clone(), false).await;
                black_box(response)
            });
        });

        group.finish();
    }

    /// Benchmark with ISQ quantization
    fn isq_benchmark(c: &mut Criterion) {
        let rt = Runtime::new().unwrap();

        let model = rt.block_on(async {
            let config = MistralRsConfig::builder()
                .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
                .isq(QuantizationLevel::Q4K)
                .build();
            MistralRsModel::new(config).await
        });

        let model = match model {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Failed to load ISQ model for benchmarks: {}", e);
                return;
            }
        };

        let mut group = c.benchmark_group("isq_inference");
        group.sample_size(10);
        group.measurement_time(Duration::from_secs(30));

        group.bench_function("q4k_short_prompt", |b| {
            let request = LlmRequest {
                contents: vec![Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text { text: "What is 2 + 2?".to_string() }],
                }],
                ..Default::default()
            };

            b.to_async(&rt).iter(|| async {
                let response = model.generate_content(request.clone(), false).await;
                black_box(response)
            });
        });

        group.finish();
    }
}
