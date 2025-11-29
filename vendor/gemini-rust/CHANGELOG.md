# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.5.0] - 2025-10-01

### ✨ Features

#### Comprehensive Tracing and Telemetry Support

- **Structured Logging with `tracing`**: Added comprehensive tracing instrumentation throughout the library using the `tracing` crate for detailed observability
- **HTTP Request Tracing**: Captures detailed information about every API call including HTTP method, URL, and response status for network diagnostics
- **Token Usage Monitoring**: Records prompt, candidate, and total tokens for each generation request enabling cost analysis and optimization
- **Performance Metrics**: Provides timing information for each API request to identify and address performance bottlenecks
- **Structured Event Emission**: Compatible with modern log aggregation platforms like Elasticsearch, Datadog, and Honeycomb
- **Span Field Generation**: Detailed span instrumentation with contextual field recording throughout the request lifecycle

#### Enhanced Function Calling Capabilities

- **OpenAPI Schema Support**: Enhanced function declarations now leverage OpenAPI support with automatic schema generation using `schemars`
- **Complex Function Example**: Added comprehensive example demonstrating sophisticated function calling patterns with multi-step operations
- **Function Behavior Control**: Added `Behavior` enum with `Blocking` and `NonBlocking` modes for function execution control
- **Improved Function Parameters**: Enhanced parameter handling with optional JSON Schema-based validation
- **Response Schema Definition**: Added optional response schema specification for better function output validation

#### HTTP Client Configuration Builder

- **`GeminiBuilder` Pattern**: New builder pattern for advanced HTTP client configuration including timeouts, proxies, and custom headers
- **Custom HTTP Client Support**: Ability to provide pre-configured `reqwest::Client` instances for advanced networking scenarios
- **Flexible Client Construction**: Enhanced client creation with comprehensive configuration options
- **Base URL Configuration**: Improved handling of custom base URLs with proper `Url` type support

#### Model Deserialization Improvements

- **Enhanced Type Safety**: Improved model deserialization with better error handling and type validation
- **Deserialization Tests**: Added comprehensive test suite for model deserialization scenarios
- **Error Message Clarity**: Better error messages for deserialization failures with detailed context

#### Agent Development Support

- **Agent Development Guide**: New `AGENTS.md` file providing comprehensive guidelines for developing agents and applications
- **Logging Conventions**: Detailed conventions for structured logging including field naming, value formatting, and instrumentation patterns
- **Best Practices Documentation**: Guidelines for span placeholders, log levels, and observability patterns

### 🔧 Technical Improvements

- **New Dependencies**: Added `tracing`, `strum`, `schemars` for enhanced functionality
- **Example Instrumentation**: All examples now include proper tracing initialization with `tracing-subscriber`
- **Centralized Tracing Approach**: Unified tracing implementation across all client operations
- **Generation Builder Cleanup**: Refactored and cleaned up generation builder patterns
- **Client Architecture Refactor**: Major improvements to client architecture for better maintainability

### 📚 Documentation

- **Tracing Chapter**: New comprehensive section in README covering tracing and telemetry features
- **Builder Pattern Examples**: Added examples demonstrating the new `GeminiBuilder` configuration options
- **Agent Development Reference**: Link to new agent development guide for advanced usage patterns

### 🙏 Contributors

- **@eklipse2k8** - Contributed to OpenAPI schema support and function calling improvements
- **@npatsakula** - Major contributions to tracing implementation, client architecture improvements, and technical enhancements

## [1.4.0] - 2025-09-11

### ✨ Features

#### Content Caching API Support
- **New `CacheBuilder`**: Added comprehensive caching API support with fluent builder pattern for creating cached content
- **`CachedContentHandle`**: New handle type for managing cached content lifecycle (get, update, delete)
- **Content Caching Integration**: Added `with_cached_content()` method to `ContentBuilder` for using cached content in generation requests
- **TTL and Expiration Support**: Full support for TTL-based and absolute time-based cache expiration
- **Cache Management**: Complete CRUD operations for cached content with proper error handling and resource cleanup

#### Thought Signature Support for Gemini 2.5

- **Enhanced Function Calling**: Added `thought_signature` field to `FunctionCall` struct for encrypted thinking process signatures
- **Text Response Signatures**: Extended `Part::Text` variant with optional `thought_signature` field for text responses with thought context
- **Multi-turn Context Preservation**: New methods for maintaining thinking context across conversation turns using thought signatures
- **New Response Methods**: Added `function_calls_with_thoughts()` and `text_with_thoughts()` for accessing content with thought signatures
- **Content Creation Helpers**: New `Content::text_with_thought_signature()` and `Content::thought_with_signature()` methods
- **New Examples**: Added comprehensive examples for thought signature usage including multi-turn conversations
- **Backward Compatibility**: All existing APIs remain unchanged while providing optional thought signature access

#### File-Based Batch Processing for Large Jobs
- **New `execute_as_file()` method**: Added a new method to the `BatchBuilder` for submitting a large number of requests, ideal for jobs that might exceed API size limits.
- **Automatic Result Handling**: The library now automatically downloads and parses result files for batches processed via the file-based method, delivering results seamlessly.

### 💥 Breaking Changes

#### Constructor Changes
- **All client constructors now return `Result<Gemini, Error>`** instead of `Gemini`
  - `Gemini::new()` now returns `Result<Gemini, ClientError>`
  - `Gemini::pro()` now returns `Result<Gemini, ClientError>`
  - `Gemini::with_model()` now returns `Result<Gemini, ClientError>`
  - `Gemini::with_base_url()` now returns `Result<Gemini, ClientError>`
  - `Gemini::with_model_and_base_url()` now returns `Result<Gemini, ClientError>`
  - **Migration**: Add `?` operator or `.expect()` calls: `let client = Gemini::new(api_key)?;`

#### Model Enum Introduction
- **New `Model` enum for type-safe model selection**
  - `Model::Gemini25Flash` - Fast, efficient model (default)
  - `Model::Gemini25FlashLite` - Lightweight model
  - `Model::Gemini25Pro` - Advanced model with thinking capabilities
  - `Model::TextEmbedding004` - Latest embedding model
  - `Model::Custom(String)` - Custom model names
  - **Migration**: Use `Model::TextEmbedding004` instead of `"models/text-embedding-004".to_string()`

#### Streaming API Updates
- **Updated streaming to use `TryStreamExt` for better error handling**
  - Import changed from `futures::StreamExt` to `futures::TryStreamExt`
  - Stream iteration changed from `stream.next().await` to `stream.try_next().await?`
  - **Migration**: Update imports and use `try_next()` with proper error handling

#### Error Handling Refactor
- **Improved error types with separate error enums**
  - `ClientError` for client-related errors
  - `BatchError` for batch operation errors
  - Removed single `Error` enum in favor of contextual error types
  - **Migration**: Update error handling to use specific error types

#### Batch API Changes
- **Removed `wait_for_completion` method from `Batch` struct**
  - Method moved to standalone helper functions in examples
  - **Migration**: Copy the `wait_for_completion` function from examples if needed

#### Import Changes
- **Module reorganization and new exports**
  - `BatchStatus` moved to batch module
  - New exports: `ClientError`, `BatchError`, `Model`
  - **Migration**: Update imports to use new module structure

#### URL Handling
- **Base URL parameter now requires `Url` type instead of `String`**
  - Custom base URLs must be parsed: `"https://example.com/".parse()?`
  - **Migration**: Add `.parse()?` when providing custom base URLs

### 🔧 Technical Improvements
- **Enhanced error handling with `snafu` library**
- **Improved type safety across all APIs**
- **Better async stream handling**
- **Consolidated error types for better developer experience**

### 🙏 Contributors

- **@npatsakula** - Major contributions to the v1.4.0 release including Content Caching API implementation, file-based batch processing enhancements, and technical improvements to error handling and type safety

## [1.3.1] - 2025-09-01

### 🔧 Maintenance

- **Version Update**: Updated version to 1.3.1
- **Documentation**: Updated README installation instructions to reflect new version

## [1.3.0] - 2025-09-01

### 🎤 New Features

#### Text-to-Speech (TTS) Generation Support

- **Complete Speech Generation Implementation**: Added comprehensive text-to-speech support using Gemini 2.5 TTS capabilities
  - New `SpeechConfig` struct for speech generation configuration
  - `VoiceConfig` and `PrebuiltVoiceConfig` for single-speaker TTS
  - `MultiSpeakerVoiceConfig` and `SpeakerVoiceConfig` for dialogue generation
  - Support for multiple prebuilt voices (Puck, Charon, Kore, Fenrir, Aoede)

#### Enhanced Content Generation

- **Response Modalities Support**: Extended `GenerationConfig` with `response_modalities` field for multimodal outputs
- **Fluent Speech API**: Added convenience methods to `ContentBuilder`
  - `with_audio_output()` - Enable audio output mode
  - `with_speech_config()` - Set custom speech configuration
  - `with_voice()` - Quick single-voice setup
  - `with_multi_speaker_config()` - Multi-speaker dialogue setup

#### New Examples

- **Speech Generation Examples**: Added comprehensive TTS examples
  - `simple_speech_generation.rs` - Basic single-speaker text-to-speech
  - `multi_speaker_tts.rs` - Multi-speaker dialogue generation
- **Updated Documentation**: Enhanced README with speech generation examples and API documentation

### 🔧 API Enhancements

- **Non-Breaking Changes**: All speech generation features are backward compatible
- **Type Safety**: Full `serde` support for all new speech-related structures
- **Audio Processing**: Base64 audio data handling with proper MIME type detection

## [1.2.3] - 2025-09-01

### 🚀 Major Features

#### Batch API Response Decoding and Type-Safe Interface

- **Complete Batch API Implementation**: Added comprehensive batch operation support with type-safe interfaces
  - New `Batch` struct for managing long-running batch operations
  - Type-safe `BatchStatus` enum with structured status information
  - Resource-safe API design that prevents invalid operations on consumed batches
  - Error recovery mechanism allowing operation retries on transient failures

#### Enhanced Batch Operations

- **Batch Creation and Management**: Full lifecycle management for batch operations
  - `batch_generate_content_sync()` for synchronous batch content generation
  - Automatic status polling with `wait_for_completion()` method
  - Manual status checking with `status()` method for fine-grained control

- **Batch Control Operations**: Complete operational control over batch processes
  - `cancel()` method for terminating running batch operations
  - `delete()` method for cleanup of completed batch metadata
  - Graceful cancellation handling with CTRL-C signal support

- **Batch Listing and Discovery**: Stream-based batch operation discovery
  - `list_batches()` method with pagination support
  - Asynchronous stream interface using `futures::Stream`
  - Efficient memory usage for large batch lists

### ✨ New Examples

#### Comprehensive Batch API Examples

- **Batch Generate Example**: `examples/batch_generate.rs`
  - Demonstrates creating batch requests with multiple content generation tasks
  - Shows proper waiting and result handling patterns
  - Includes error handling for individual request failures

- **Batch Cancellation Example**: `examples/batch_cancel.rs`
  - Advanced example with CTRL-C signal handling
  - Demonstrates safe batch cancellation in concurrent environments
  - Shows retry patterns for network failure scenarios

- **Batch Management Examples**:
  - `examples/batch_list.rs` - Stream-based batch operation listing
  - `examples/batch_delete.rs` - Cleanup of completed batch operations

### 🔧 Technical Improvements

#### Type Safety Enhancements

- **Enhanced Model Definitions**: Added `PartialEq` trait to core data structures
  - Improved testability and debugging capabilities
  - Better support for structured comparisons and assertions

#### Error Handling Improvements

- **Batch-Specific Error Types**: New error variants for batch operations
  - `BatchFailed` - for operations that completed with errors
  - `BatchExpired` - for operations that exceeded time limits
  - `InconsistentBatchState` - for API state inconsistencies
  - `UnsupportedBatchOutput` - for unsupported output formats

#### API Consistency

- **Streaming Interface**: Consistent use of async streams for paginated results
- **Resource Management**: Consumer-based API design preventing resource leaks
- **Documentation**: Comprehensive inline documentation with usage examples

### 🙏 Contributors

- **@npatsakula** - Complete batch API implementation, type-safe interfaces, and comprehensive examples
- **@flachesis** - Integration, testing, and release management

### 📝 Notes

- This release significantly expands the library's batch processing capabilities
- The new batch API follows Rust best practices for resource management and error handling
- All new functionality is fully documented with practical examples
- Breaking changes are minimal - existing API remains fully compatible

## [1.2.2] - 2025-09-01

### 🛠️ Bug Fixes

#### Content Builder Improvements

- **Fixed `with_inline_data` Role Assignment**: Resolved API validation error when using inline data (blobs)
  - `ContentBuilder::with_inline_data()` now automatically sets `Role::User` for blob content
  - Prevents "Please use a valid role: user, model" API errors when sending media files
  - Ensures consistency with other content builder methods that properly handle roles

### ✨ New Examples

#### Video Processing Example

- **Added MP4 Video Description Example**: New `examples/mp4_describe.rs` demonstrating video analysis
  - Shows how to read MP4 files and send them to Gemini API for content description
  - Demonstrates two approaches: using `Message` struct and using `with_inline_data` builder method
  - Includes proper base64 encoding of video content for API transmission
  - English-language prompts and documentation for broader accessibility

### 🔧 Technical Improvements

#### Content Builder Consistency

- **Role Handling Standardization**: Improved consistency across content builder methods
  - `with_inline_data()` now matches behavior of `with_function_response()` by auto-setting user role
  - Reduces boilerplate code when working with media content
  - Maintains backward compatibility while fixing the underlying issue

### 📋 Usage Impact

#### Enhanced Developer Experience

- **Simplified Media Handling**: Media files can now be added more easily without manual role assignment

  ```rust
  // Now works correctly without additional role setup
  let response = gemini
      .generate_content()
      .with_user_message("Describe this video")
      .with_inline_data(base64_video, "video/mp4")
      .execute()
      .await?;
  ```

#### Example Enhancements

- **Video Processing Workflow**: Complete example showing video file handling from disk to API
- **Base64 Encoding Integration**: Demonstrates proper encoding of binary media for API transmission

### 🙏 Contributors

- **@flachesis** - Content builder role assignment fix and video example implementation

### 📝 Notes

- This release focuses on improving the developer experience when working with media content
- The fix ensures that `with_inline_data()` works correctly out of the box without additional configuration
- Video example provides a practical template for media analysis applications

## [1.2.1] - 2025-08-29

### 🛠️ Bug Fixes

#### Content Structure Improvements
- **Fixed Content Serialization**: Resolved issues with Content structure serialization to match Gemini API requirements
  - Changed `Content.parts` from `Vec<Part>` to `Option<Vec<Part>>` to handle API responses where parts may be absent
  - Added `#[serde(rename_all = "camelCase")]` to `Content` struct for proper JSON field naming
  - Added `#[serde(rename_all = "camelCase")]` to `GenerateContentRequest` for consistent API formatting
  - Fixed `GenerationConfig` serialization with proper camelCase field naming

#### API Response Handling
- **Enhanced Response Parsing**: Improved handling of Gemini API responses with missing content parts
  - Updated `GenerationResponse.text()` method to safely handle `Option<Vec<Part>>`
  - Updated `GenerationResponse.function_calls()` method with proper option handling
  - Updated `GenerationResponse.thoughts()` and `GenerationResponse.all_text()` methods for safe access
  - Added support for missing parts in API responses (common with certain model configurations)

#### Example Updates
- **Fixed Example Code**: Updated examples to work with new Content structure
  - Updated `examples/advanced.rs` to use `Option<Vec<Part>>` when manually building content
  - Updated `examples/curl_equivalent.rs` with proper Content construction
  - Updated `examples/curl_google_search.rs` for compatibility
  - Improved `examples/simple.rs` with better token limits (`max_output_tokens: 1000`)

#### Additional Response Fields
- **Extended Response Model**: Added missing fields to `GenerationResponse`
  - Added `model_version: Option<String>` field for tracking model version information
  - Added `response_id: Option<String>` field for response identification
  - Enhanced `UsageMetadata` with proper field structure for token counting

### 🔧 Technical Improvements

#### Serialization Consistency
- **Unified camelCase Naming**: Ensured all API-facing structs use consistent camelCase field naming
  - Prevents JSON serialization mismatches with Gemini API
  - Improves reliability of API communication
  - Maintains backward compatibility in Rust code

#### Error Resilience
- **Robust Content Handling**: Improved handling of edge cases in API responses
  - Better support for responses with empty or missing content parts
  - Safer default values for optional fields
  - Reduced likelihood of deserialization failures

### 📋 Usage Impact

#### Breaking Changes
- **Content Construction**: Direct manipulation of `Content.parts` now requires `Option` wrapping
  ```rust
  // Old (no longer works)
  content.parts.push(part);

  // New (correct approach)
  content.parts = Some(vec![part]);
  ```

#### Migration Guide
- **Automatic Migration**: Most users won't need changes as the `Content::text()`, `Content::function_call()`, etc. helper methods handle the Option wrapping automatically
- **Direct Content Building**: Only users manually constructing Content structs need to wrap parts in `Some()`

### 🙏 Contributors

- **@flachesis** - Comprehensive Content structure refactoring and API compatibility improvements

### 📝 Notes

- This release improves compatibility with various Gemini API response formats
- No functional changes to public API methods - all builder patterns work unchanged
- Enhanced error resilience when processing API responses with missing content parts
- Better support for different model configurations that may return sparse content

## [1.2.0] - 2025-08-29

### ✨ Features

#### Batch Content Generation API
- **Asynchronous Batch Processing**: Complete implementation of Gemini API's batch content generation
  - Support for submitting multiple content generation requests in a single batch operation
  - Proper handling of asynchronous batch processing with batch tracking
  - Detailed batch status monitoring with request counts and state tracking
  - Full compliance with Google's batch API format including nested request structures

#### Enhanced API Structure
- **Batch Request Models**: New comprehensive type system for batch operations
  - `BatchGenerateContentRequest` with proper nested structure (`batch.input_config.requests.requests`)
  - `BatchConfig` for batch configuration with display names
  - `InputConfig` and `RequestsContainer` for structured request organization
  - `BatchRequestItem` with metadata support for individual requests
  - `RequestMetadata` for request identification and tracking

#### Improved Response Handling
- **Batch Response Processing**: Detailed batch operation response handling
  - `BatchGenerateContentResponse` with batch creation confirmation
  - `BatchMetadata` including creation/update timestamps and model information
  - `BatchStats` with comprehensive request counting (pending, completed, failed)
  - Proper state tracking for batch operations (`BATCH_STATE_PENDING`, etc.)

#### Public API Enhancements
- **Extended Type Exports**: Additional types now available from crate root
  - `ContentBuilder` now publicly exported for advanced usage patterns
  - `GenerateContentRequest` accessible for custom request building
  - All batch-related types exported for external batch management
  - Enhanced builder pattern accessibility

### 🛠️ Technical Improvements

#### Dependency Management
- **Development Dependencies Optimization**: Moved `tokio` to dev-dependencies
  - Reduced production bundle size by moving tokio to development-only dependencies
  - Maintained full async functionality while optimizing dependency tree
  - Better separation of concerns between runtime and library dependencies

#### Code Organization
- **Enhanced Builder Architecture**: Improved batch builder implementation
  - Automatic metadata generation for batch requests
  - Streamlined batch creation with fluent API
  - Better error handling and validation for batch operations

### 📋 Usage Examples

#### Batch Content Generation
```rust
use gemini_rust::{Gemini, Message};

let client = Gemini::new(api_key);

// Create individual requests
let request1 = client
    .generate_content()
    .with_message(Message::user("What is the meaning of life?"))
    .build();

let request2 = client
    .generate_content()
    .with_message(Message::user("What is the best programming language?"))
    .build();

// Submit batch request
let batch_response = client
    .batch_generate_content_sync()
    .with_request(request1)
    .with_request(request2)
    .execute()
    .await?;

println!("Batch ID: {}", batch_response.name);
println!("State: {}", batch_response.metadata.state);
```

#### Advanced Request Building
```rust
use gemini_rust::{ContentBuilder, GenerateContentRequest};

// Direct access to ContentBuilder for advanced patterns
let mut builder: ContentBuilder = client.generate_content();
let request: GenerateContentRequest = builder
    .with_user_message("Custom request")
    .build();
```

### 🙏 Contributors

- **@npatsakula** - Implemented basic batch API foundation ([#8](https://github.com/flachesis/gemini-rust/pull/8))
- **@brekkylab** - Optimized dependency management ([#7](https://github.com/flachesis/gemini-rust/pull/7))
- **@flachesis** - Enhanced batch API with detailed request/response structures

### 🔄 Breaking Changes

None. This release maintains full backward compatibility with v1.1.0.

### 📝 Notes

- Batch operations are asynchronous and require polling for completion status
- Batch API follows Google's official format specification exactly
- Enhanced type safety with comprehensive batch operation modeling
- Improved error handling for batch-specific scenarios

## [1.1.0] - 2025-07-21

### ✨ Features

#### Public API Enhancements
- **`Blob` Type Export**: The `Blob` struct is now publicly exported from the crate root
  - Enables direct usage of `Blob` for inline data handling without importing from internal modules
  - Improves ergonomics for multimodal applications working with images and binary data
  - Maintains all existing functionality including `new()` constructor and base64 encoding support

### 🙏 Contributors

- **@anandkapdi** - Made `Blob` struct publicly accessible ([#6](https://github.com/flachesis/gemini-rust/pull/6))

### 📋 Usage Examples

#### Direct Blob Usage
```rust
use gemini_rust::{Gemini, Blob};

// Now you can use Blob directly from the crate root
let blob = Blob::new("image/jpeg", base64_encoded_data);
let response = client
    .generate_content()
    .with_user_message("What's in this image?")
    .with_inline_data(blob)
    .execute()
    .await?;
```

## [1.0.0] - 2025-07-12

### 🎉 Initial Stable Release

This marks the first stable release of `gemini-rust`, a comprehensive Rust client library for Google's Gemini 2.0 API. This release consolidates all the features developed during the pre-1.0 phase into a stable, production-ready library.

### ✨ Features

#### Core API Support
- **Content Generation**: Complete implementation of Gemini 2.0 content generation API
  - Support for system prompts and user messages
  - Multi-turn conversations with conversation history
  - Configurable generation parameters (temperature, max tokens, etc.)
  - Safety settings and content filtering
- **Streaming Responses**: Real-time streaming of generated content using async streams
- **Text Embeddings**: Full support for text embedding generation
  - Multiple task types: `RetrievalDocument`, `RetrievalQuery`, `SemanticSimilarity`, `Classification`, `Clustering`
  - Batch embedding support for processing multiple texts efficiently
  - Support for `text-embedding-004` and other embedding models

#### Advanced Features
- **Function Calling & Tools**: Complete tools and function calling implementation
  - Custom function declarations with JSON schema validation
  - Google Search tool integration for real-time web search capabilities
  - Function response handling and multi-turn tool conversations
  - Support for multiple tools per request
- **Thinking Mode**: Support for Gemini 2.5 series thinking capabilities
  - Dynamic thinking (model determines thinking budget automatically)
  - Fixed thinking budget with configurable token limits
  - Access to thinking process summaries
  - Thought inclusion in responses for transparency
- **Multimodal Support**: Support for images and binary data
  - Inline data support with base64 encoding
  - Multiple MIME type support
  - Blob handling for various media types

#### Technical Excellence
- **Async/Await**: Full async support built on `tokio` runtime
- **Type Safety**: Comprehensive type definitions with `serde` serialization
- **Error Handling**: Robust error handling with detailed error types using `thiserror`
- **Builder Pattern**: Fluent, ergonomic API design for easy usage
- **HTTP/2 Support**: Modern HTTP features with `reqwest` client
- **Configurable Endpoints**: Support for custom base URLs and API endpoints

### 🏗️ Architecture

#### Core Components
- **`Gemini` Client**: Main client struct with model configuration and API key management
- **`ContentBuilder`**: Fluent API for building content generation requests
- **`EmbedBuilder`**: Specialized builder for embedding requests
- **Type System**: Complete type definitions matching Gemini API specifications
- **Error Types**: Comprehensive error handling covering HTTP, JSON, and API errors

#### Models & Types
- **`GenerationResponse`**: Complete response parsing with candidates, safety ratings, and metadata
- **`Content` & `Part`**: Flexible content representation supporting text, function calls, and media
- **`Tool` & `FunctionDeclaration`**: Full function calling type system
- **`ThinkingConfig`**: Configuration for Gemini 2.5 thinking capabilities
- **`UsageMetadata`**: Token usage tracking and billing information

### 📚 Examples & Documentation

#### Comprehensive Examples
- **`simple.rs`**: Basic text generation with system prompts
- **`streaming.rs`**: Real-time streaming response handling
- **`embedding.rs`**: Text embedding generation
- **`batch_embedding.rs`**: Efficient batch processing of embeddings
- **`google_search.rs`**: Google Search tool integration
- **`tools.rs`**: Custom function calling examples
- **`thinking_basic.rs`** & **`thinking_advanced.rs`**: Gemini 2.5 thinking mode usage
- **`blob.rs`**: Image and binary data handling
- **`structured_response.rs`**: Structured output generation
- **`generation_config.rs`**: Advanced generation configuration
- **`custom_base_url.rs`**: Custom endpoint configuration

#### CURL Equivalents
- **`curl_equivalent.rs`**: Direct API comparison examples
- **`curl_google_search.rs`**: Google Search API equivalent
- **`curl_thinking_equivalent.rs`**: Thinking mode API equivalent

### 🔧 Dependencies

#### Production Dependencies
- **`reqwest ^0.12.15`**: HTTP client with JSON, streaming, and HTTP/2 support
- **`serde ^1.0`**: Serialization framework with derive support
- **`serde_json ^1.0`**: JSON serialization support
- **`tokio ^1.28`**: Async runtime with full feature set
- **`thiserror ^2.0.12`**: Error handling and derivation
- **`url ^2.4`**: URL parsing and manipulation
- **`async-trait ^0.1`**: Async trait support
- **`futures ^0.3.1`** & **`futures-util ^0.3`**: Async utilities and stream handling
- **`base64 0.22.1`**: Base64 encoding for binary data

### 🛡️ API Compatibility

#### Supported Models
- **Gemini 2.5 Flash**: Default model with thinking capabilities
- **Gemini 2.5 Pro**: Advanced model with enhanced thinking features
- **Gemini 2.0 Flash**: Fast generation model
- **Text Embedding 004**: Latest embedding model
- **Custom Models**: Support for any Gemini API compatible model

#### API Endpoints
- **Generate Content**: `/v1beta/models/{model}:generateContent`
- **Stream Generate Content**: `/v1beta/models/{model}:streamGenerateContent`
- **Embed Content**: `/v1beta/models/{model}:embedContent`
- **Batch Embed Content**: `/v1beta/models/{model}:batchEmbedContents`
- **Custom Base URLs**: Configurable endpoint support

### 🔒 Security & Safety

- **API Key Management**: Secure API key handling through environment variables
- **Content Safety**: Built-in safety rating parsing and handling
- **Error Resilience**: Comprehensive error handling for network and API issues
- **Input Validation**: Type-safe request building preventing malformed requests

### 📋 Usage Examples

#### Basic Text Generation
```rust
use gemini_rust::Gemini;

let client = Gemini::new(api_key);
let response = client
    .generate_content()
    .with_system_prompt("You are a helpful assistant.")
    .with_user_message("Hello!")
    .execute()
    .await?;
```

#### Streaming Responses
```rust
let mut stream = client
    .generate_content()
    .with_user_message("Tell me a story")
    .execute_stream()
    .await?;

while let Some(chunk) = stream.next().await {
    print!("{}", chunk?.text());
}
```

#### Google Search Integration
```rust
use gemini_rust::Tool;

let response = client
    .generate_content()
    .with_user_message("What's the current weather?")
    .with_tool(Tool::google_search())
    .execute()
    .await?;
```

#### Text Embeddings
```rust
let response = client
    .embed_content()
    .with_text("Hello, world!")
    .with_task_type(TaskType::RetrievalDocument)
    .execute()
    .await?;
```

#### Thinking Mode (Gemini 2.5)
```rust
let client = Gemini::with_model(api_key, "models/gemini-2.5-pro");
let response = client
    .generate_content()
    .with_user_message("Explain quantum physics")
    .with_dynamic_thinking()
    .with_thoughts_included(true)
    .execute()
    .await?;
```

### 🚀 Performance

- **Async/Await**: Non-blocking I/O for high concurrency
- **HTTP/2**: Efficient connection reuse and multiplexing
- **Streaming**: Memory-efficient processing of large responses
- **Batch Processing**: Optimized batch embedding support
- **Connection Pooling**: Automatic connection management

### 📝 Documentation

- **Comprehensive README**: Detailed usage examples and API overview
- **Inline Documentation**: Complete rustdoc documentation for all public APIs
- **Example Collection**: 20+ examples covering all major features
- **Type Documentation**: Full documentation of all models and types

### 🔗 Links

- **Repository**: [https://github.com/flachesis/gemini-rust](https://github.com/flachesis/gemini-rust)
- **Documentation**: Available on docs.rs
- **Crates.io**: [https://crates.io/crates/gemini-rust](https://crates.io/crates/gemini-rust)
- **License**: MIT

---

**Note**: This 1.0.0 release represents a stable API that follows semantic versioning. Future releases will maintain backward compatibility within the 1.x series, with breaking changes reserved for major version increments.
