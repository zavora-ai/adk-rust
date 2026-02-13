# Examples

This directory contains comprehensive examples demonstrating all features of the gemini-rust library. Each example is a complete, runnable program that showcases specific functionality.

## Quick Start

Run any example with:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example <example_name>
```

Get your API key from [Google AI Studio](https://aistudio.google.com/apikey).

## Example Categories

### 🚀 Getting Started

| Example | Description |
|---------|-------------|
| [`basic_generation.rs`](basic_generation.rs) | Simple content generation - best starting point for beginners |
| [`basic_streaming.rs`](basic_streaming.rs) | Simple streaming responses - real-time content display |
| [`simple.rs`](simple.rs) | Basic text generation and function calling - comprehensive example |
| [`advanced.rs`](advanced.rs) | Advanced content generation with comprehensive parameter configuration |

### 💬 Content Generation

| Example | Description |
|---------|-------------|
| [`streaming.rs`](streaming.rs) | Real-time streaming responses for interactive applications |
| [`generation_config.rs`](generation_config.rs) | Custom generation parameters (temperature, tokens, etc.) |
| [`structured_response.rs`](structured_response.rs) | Generate structured JSON output with schema validation |
| [`gemini_pro_example.rs`](gemini_pro_example.rs) | Using Gemini 2.5 Pro for advanced tasks |

### 🛠️ Function Calling & Tools

| Example | Description |
|---------|-------------|
| [`tools.rs`](tools.rs) | Custom function declarations and tool integration |
| [`complex_function.rs`](complex_function.rs) | Advanced function calling with OpenAPI schema support using `schemars` |
| [`google_search.rs`](google_search.rs) | Google Search tool integration for real-time information |
| [`google_search_with_functions.rs`](google_search_with_functions.rs) | Combining Google Search with custom functions |
| [`curl_google_search.rs`](curl_google_search.rs) | Google Search functionality with cURL equivalent commands |
| [`url_context.rs`](url_context.rs) | URL Context tool for analyzing web content |

### 🧠 Thinking Mode (Gemini 2.5)

| Example | Description |
|---------|-------------|
| [`thinking_basic.rs`](thinking_basic.rs) | Basic thinking mode for step-by-step reasoning |
| [`thinking_advanced.rs`](thinking_advanced.rs) | Advanced thinking capabilities with custom budgets |
| [`thinking_curl_equivalent.rs`](thinking_curl_equivalent.rs) | Thinking mode with cURL command equivalents |
| [`simple_thought_signature.rs`](simple_thought_signature.rs) | Simple thought signature examples |
| [`text_thought_signature_example.rs`](text_thought_signature_example.rs) | Text-based thought signature demonstrations |
| [`thought_signature_example.rs`](thought_signature_example.rs) | Comprehensive thought signature usage |

### 📦 Batch Processing

| Example | Description |
|---------|-------------|
| [`batch_generate.rs`](batch_generate.rs) | Batch content generation for multiple requests |
| [`batch_embedding.rs`](batch_embedding.rs) | Batch text embedding generation |
| [`batch_list.rs`](batch_list.rs) | List and manage batch operations with streaming |
| [`batch_cancel.rs`](batch_cancel.rs) | Cancel running batch operations |
| [`batch_delete.rs`](batch_delete.rs) | Delete completed batch operations |

### 💾 Content Caching

| Example | Description |
|---------|-------------|
| [`cache_basic.rs`](cache_basic.rs) | Cache system instructions and conversation history for cost optimization |

### 📊 Text Embeddings

| Example | Description |
|---------|-------------|
| [`embedding.rs`](embedding.rs) | Generate text embeddings with various task types |

### 🖼️ Multimodal & Media

| Example | Description |
|---------|-------------|
| [`blob.rs`](blob.rs) | Process images and binary data with base64 encoding |
| [`mp4_describe.rs`](mp4_describe.rs) | Analyze and describe video content |

### 🎨 Image Generation

| Example | Description |
|---------|-------------|
| [`simple_image_generation.rs`](simple_image_generation.rs) | Basic text-to-image generation |
| [`image_generation.rs`](image_generation.rs) | Advanced image generation with detailed prompts |
| [`image_editing.rs`](image_editing.rs) | Edit existing images with text prompts |

### 🎤 Speech Generation

| Example | Description |
|---------|-------------|
| [`simple_speech_generation.rs`](simple_speech_generation.rs) | Basic text-to-speech generation |
| [`multi_speaker_tts.rs`](multi_speaker_tts.rs) | Multi-speaker text-to-speech dialogue generation |

### 📁 File Management

| Example | Description |
|---------|-------------|
| [`files_lifecycle.rs`](files_lifecycle.rs) | Complete file management lifecycle (upload, list, get, delete) |
| [`files_delete_all.rs`](files_delete_all.rs) | Bulk delete all uploaded files |

### ⚙️ Configuration & Setup

| Example | Description |
|---------|-------------|
| [`custom_models.rs`](custom_models.rs) | Configure different Gemini models (Flash, Pro, Lite, custom models) with all available options |
| [`custom_base_url.rs`](custom_base_url.rs) | Use custom API endpoints and configurations |
| [`http_client_builder.rs`](http_client_builder.rs) | Advanced HTTP client configuration with timeouts, proxies, and connection pooling |
| [`tracing_telemetry.rs`](tracing_telemetry.rs) | Comprehensive tracing and telemetry setup for observability and monitoring |
| [`curl_equivalent.rs`](curl_equivalent.rs) | See equivalent cURL commands for API calls |

### 🚨 Error Handling

| Example | Description |
|---------|-------------|
| [`error_handling.rs`](error_handling.rs) | Comprehensive error handling patterns and best practices |

## Usage Patterns

### Basic Content Generation

Start with `simple.rs` to understand the basic API usage:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example simple
```

### Streaming Responses

For real-time applications, use streaming:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example streaming
```

### Function Calling

Learn how to integrate custom functions:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example tools
GEMINI_API_KEY="your-api-key" cargo run --example complex_function
```

### Thinking Mode

Explore Gemini 2.5's reasoning capabilities:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example thinking_basic
GEMINI_API_KEY="your-api-key" cargo run --example thinking_advanced
```

### Batch Processing

For high-throughput applications:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example batch_generate
```

### Image and Media

Work with multimodal content:

```bash
GEMINI_API_KEY="your-api-key" cargo run --example blob
GEMINI_API_KEY="your-api-key" cargo run --example image_generation
```

## Notes

- All examples include comprehensive error handling
- Examples demonstrate both async/await patterns and streaming
- Code includes detailed comments explaining each step
- Examples use environment variables for API keys (never hardcode credentials)
- Some examples require specific model capabilities (e.g., image generation requires image-enabled models)

## Contributing

When adding new examples:

1. Follow the existing naming conventions
2. Include comprehensive error handling
3. Add detailed comments explaining the functionality
4. Update this README.md with the new example
5. Ensure the example runs successfully with `cargo run --example <name>`

## Media Files

The examples directory includes sample media files for testing:

- `image-example.webp` - Sample image for image processing examples
- `sample.mp4` - Sample video for video analysis examples

These files are used by various examples to demonstrate multimodal capabilities.
