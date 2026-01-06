# OpenAI Provider Implementation Plan

## Overview

This document outlines the implementation plan for adding OpenAI, Azure OpenAI, and OpenAI-compatible provider support to ADK-Rust using the `async-openai` crate (v0.31.1).

## Goals

1. Support OpenAI models (GPT-4o, GPT-4o-mini, GPT-4-turbo, etc.)
2. Support Azure OpenAI deployments
3. Support OpenAI-compatible APIs (Ollama, vLLM, LocalAI, etc.)
4. Full tool/function calling with proper `tool_call_id` tracking
5. Streaming support
6. Multimodal support (vision)

## Dependencies

### New Dependency

```toml
[dependencies]
async-openai = { version = "0.31.1", optional = true }
```

### Feature Flag

```toml
[features]
default = ["gemini"]
gemini = ["gemini-rust"]
openai = ["async-openai"]
all-providers = ["gemini", "openai"]
```

---

## Architecture

### Module Structure

```
adk-model/
├── src/
│   ├── lib.rs              # Re-exports, feature gates
│   ├── gemini/
│   │   ├── mod.rs
│   │   └── client.rs       # Existing Gemini implementation
│   └── openai/
│       ├── mod.rs          # Module exports
│       ├── client.rs       # OpenAI client implementation
│       ├── config.rs       # Provider configurations
│       └── convert.rs      # Type conversions ADK <-> async-openai
```

### Provider Configuration

```rust
// adk-model/src/openai/config.rs

/// Configuration for OpenAI API
pub struct OpenAIProvider {
    pub api_key: String,
    pub model: String,
    pub organization_id: Option<String>,
    pub project_id: Option<String>,
}

/// Configuration for Azure OpenAI
pub struct AzureOpenAIProvider {
    pub api_key: String,
    pub api_base: String,           // e.g., "https://my-resource.openai.azure.com"
    pub api_version: String,        // e.g., "2024-02-15-preview"
    pub deployment_id: String,      // The deployment name
}

/// Configuration for OpenAI-compatible APIs
pub struct OpenAICompatibleProvider {
    pub api_key: String,
    pub api_base: String,           // e.g., "http://localhost:11434/v1"
    pub model: String,
}

/// Unified provider enum
pub enum OpenAIProviderConfig {
    OpenAI(OpenAIProvider),
    Azure(AzureOpenAIProvider),
    Compatible(OpenAICompatibleProvider),
}
```

---

## Type Mappings

### ADK to async-openai

| ADK Type | async-openai Type | Notes |
|----------|-------------------|-------|
| `Content` | `ChatCompletionRequestMessage` | Role-based conversion |
| `Part::Text` | `ChatCompletionRequestUserMessageContent::Text` | Direct |
| `Part::InlineData` | `ImageUrl` / base64 | Vision support |
| `Part::FunctionCall` | `ChatCompletionMessageToolCall` | Includes `id` |
| `Part::FunctionResponse` | `ChatCompletionRequestToolMessage` | Includes `tool_call_id` |
| `LlmRequest.tools` | `Vec<ChatCompletionTool>` | JSON schema conversion |
| `GenerationConfig` | Request builder methods | Partial mapping |

### async-openai to ADK

| async-openai Type | ADK Type | Notes |
|-------------------|----------|-------|
| `CreateChatCompletionResponse` | `LlmResponse` | Extract from choices |
| `ChatCompletionResponseMessage` | `Content` | With parts |
| `ChatCompletionMessageToolCall` | `Part::FunctionCall` | Preserve `id` |
| `CompletionUsage` | `UsageMetadata` | Token counts |
| `FinishReason` | `FinishReason` | Enum mapping |

---

## Core Implementation

### OpenAI Client

```rust
// adk-model/src/openai/client.rs

use async_openai::{
    Client,
    config::{OpenAIConfig, AzureConfig},
    types::chat::*,
};
use adk_core::{Llm, LlmRequest, LlmResponse, Content, Part, AdkError};
use async_stream::try_stream;
use futures::Stream;
use std::pin::Pin;

pub struct OpenAIClient {
    client: Client<OpenAIConfig>,
    model: String,
}

pub struct AzureOpenAIClient {
    client: Client<AzureConfig>,
    deployment_id: String,
}

impl OpenAIClient {
    pub fn new(config: OpenAIProvider) -> Result<Self, AdkError> {
        let openai_config = OpenAIConfig::new()
            .with_api_key(&config.api_key);

        let openai_config = if let Some(org) = config.organization_id {
            openai_config.with_org_id(&org)
        } else {
            openai_config
        };

        Ok(Self {
            client: Client::with_config(openai_config),
            model: config.model,
        })
    }

    pub fn compatible(config: OpenAICompatibleProvider) -> Result<Self, AdkError> {
        let openai_config = OpenAIConfig::new()
            .with_api_key(&config.api_key)
            .with_api_base(&config.api_base);

        Ok(Self {
            client: Client::with_config(openai_config),
            model: config.model,
        })
    }
}

#[async_trait::async_trait]
impl Llm for OpenAIClient {
    fn name(&self) -> &str {
        &self.model
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = Result<LlmResponse, AdkError>> + Send + '_>> {
        let stream = try_stream! {
            // Convert ADK request to OpenAI request
            let openai_request = convert::to_openai_request(&request, &self.model)?;

            // Make streaming request
            let mut stream = self.client
                .chat()
                .create_stream(openai_request)
                .await
                .map_err(|e| AdkError::Model(format!("OpenAI error: {}", e)))?;

            // Process stream chunks
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| AdkError::Model(format!("Stream error: {}", e)))?;
                let response = convert::from_openai_chunk(&chunk)?;
                yield response;
            }
        };

        Box::pin(stream)
    }
}
```

### Type Conversions

```rust
// adk-model/src/openai/convert.rs

use adk_core::{Content, Part, LlmRequest, LlmResponse, UsageMetadata, FinishReason};
use async_openai::types::chat::*;
use std::collections::HashMap;

/// Convert ADK Content to OpenAI message
pub fn content_to_message(content: &Content) -> ChatCompletionRequestMessage {
    match content.role.as_str() {
        "user" => {
            let text = extract_text(&content.parts);
            ChatCompletionRequestUserMessage::from(text).into()
        }
        "model" | "assistant" => {
            let mut msg = ChatCompletionRequestAssistantMessageArgs::default();

            // Extract text content
            if let Some(text) = get_text_content(&content.parts) {
                msg = msg.content(text);
            }

            // Extract tool calls
            let tool_calls = extract_tool_calls(&content.parts);
            if !tool_calls.is_empty() {
                msg = msg.tool_calls(tool_calls);
            }

            msg.build().unwrap().into()
        }
        "system" => {
            let text = extract_text(&content.parts);
            ChatCompletionRequestSystemMessage::from(text).into()
        }
        "function" | "tool" => {
            // Tool response message
            if let Some(Part::FunctionResponse { name, response, id }) = content.parts.first() {
                let tool_call_id = id.clone().unwrap_or_else(|| format!("call_{}", name));
                ChatCompletionRequestToolMessage {
                    tool_call_id,
                    content: ChatCompletionRequestToolMessageContent::Text(
                        serde_json::to_string(response).unwrap_or_default()
                    ),
                }.into()
            } else {
                // Fallback
                ChatCompletionRequestUserMessage::from("").into()
            }
        }
        _ => ChatCompletionRequestUserMessage::from(extract_text(&content.parts)).into()
    }
}

/// Extract tool calls from parts
fn extract_tool_calls(parts: &[Part]) -> Vec<ChatCompletionMessageToolCall> {
    parts.iter().filter_map(|part| {
        if let Part::FunctionCall { name, args, id } = part {
            Some(ChatCompletionMessageToolCall {
                id: id.clone().unwrap_or_else(|| format!("call_{}", name)),
                r#type: ChatCompletionToolType::Function,
                function: FunctionCall {
                    name: name.clone(),
                    arguments: serde_json::to_string(args).unwrap_or_default(),
                },
            })
        } else {
            None
        }
    }).collect()
}

/// Convert ADK tools to OpenAI tools
pub fn convert_tools(tools: &HashMap<String, serde_json::Value>) -> Vec<ChatCompletionTool> {
    tools.iter().map(|(name, decl)| {
        let description = decl.get("description")
            .and_then(|d| d.as_str())
            .map(String::from);

        let parameters = decl.get("parameters").cloned();

        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: name.clone(),
                description,
                parameters,
                strict: None,
            },
        }
    }).collect()
}

/// Convert OpenAI response to ADK LlmResponse
pub fn from_openai_response(resp: &CreateChatCompletionResponse) -> Result<LlmResponse, AdkError> {
    let content = resp.choices.first().map(|choice| {
        let mut parts = Vec::new();

        // Add text content
        if let Some(text) = &choice.message.content {
            parts.push(Part::Text { text: text.clone() });
        }

        // Add tool calls with IDs
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::json!({}));
                parts.push(Part::FunctionCall {
                    name: tc.function.name.clone(),
                    args,
                    id: Some(tc.id.clone()),  // Preserve the tool call ID!
                });
            }
        }

        Content {
            role: "model".to_string(),
            parts,
        }
    });

    let usage_metadata = resp.usage.as_ref().map(|u| UsageMetadata {
        prompt_token_count: u.prompt_tokens as i32,
        candidates_token_count: u.completion_tokens as i32,
        total_token_count: u.total_tokens as i32,
    });

    let finish_reason = resp.choices.first()
        .and_then(|c| c.finish_reason.as_ref())
        .map(|fr| match fr {
            FinishReason::Stop => adk_core::FinishReason::Stop,
            FinishReason::Length => adk_core::FinishReason::MaxTokens,
            FinishReason::ToolCalls => adk_core::FinishReason::Stop,
            FinishReason::ContentFilter => adk_core::FinishReason::Safety,
            _ => adk_core::FinishReason::Other,
        });

    Ok(LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    })
}
```

---

## Required Changes to adk-core

### Part Enum Update

The `Part` enum needs an `id` field for tool call tracking:

```rust
// adk-core/src/types.rs

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text { text: String },
    InlineData { mime_type: String, data: Vec<u8> },
    FunctionCall {
        name: String,
        args: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,  // NEW: Tool call ID for OpenAI
    },
    FunctionResponse {
        name: String,
        response: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,  // NEW: Tool call ID for OpenAI
    },
}
```

### Impact Analysis

Files requiring updates when adding `id` field:

| File | Changes Required |
|------|------------------|
| `adk-core/src/types.rs` | Add `id` field to Part variants |
| `adk-core/src/event.rs` | Update test fixtures |
| `adk-model/src/gemini/client.rs` | Set `id: None` for Gemini |
| `adk-agent/src/llm_agent.rs` | Propagate `id` through tool execution |
| `adk-server/src/a2a/parts.rs` | Handle `id` in A2A conversion |
| `adk-tool/src/function_tool.rs` | No change (doesn't construct Parts) |

---

## Usage Examples

### Basic OpenAI Usage

```rust
use adk_model::openai::{OpenAIClient, OpenAIProvider};
use adk_agent::LlmAgent;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = OpenAIClient::new(OpenAIProvider {
        api_key: std::env::var("OPENAI_API_KEY")?,
        model: "gpt-4o-mini".to_string(),
        organization_id: None,
        project_id: None,
    })?;

    let agent = LlmAgent::builder()
        .name("assistant")
        .model(Arc::new(model))
        .instruction("You are a helpful assistant.")
        .build();

    // Use agent...
    Ok(())
}
```

### Azure OpenAI Usage

```rust
use adk_model::openai::{AzureOpenAIClient, AzureOpenAIProvider};

let model = AzureOpenAIClient::new(AzureOpenAIProvider {
    api_key: std::env::var("AZURE_OPENAI_API_KEY")?,
    api_base: "https://my-resource.openai.azure.com".to_string(),
    api_version: "2024-02-15-preview".to_string(),
    deployment_id: "gpt-4o-deployment".to_string(),
})?;
```

### OpenAI-Compatible (Ollama)

```rust
use adk_model::openai::{OpenAIClient, OpenAICompatibleProvider};

let model = OpenAIClient::compatible(OpenAICompatibleProvider {
    api_key: "ollama".to_string(),  // Ollama doesn't need real key
    api_base: "http://localhost:11434/v1".to_string(),
    model: "llama3.2".to_string(),
})?;
```

### With Tool Calling

```rust
use adk_tool::FunctionTool;
use serde_json::json;

fn get_weather(city: String) -> String {
    format!("Weather in {}: 72F, sunny", city)
}

let weather_tool = FunctionTool::new(
    "get_weather",
    "Get current weather for a city",
    json!({
        "type": "object",
        "properties": {
            "city": { "type": "string", "description": "City name" }
        },
        "required": ["city"]
    }),
    |args| {
        let city = args["city"].as_str().unwrap_or("Unknown");
        Ok(json!({ "weather": get_weather(city.to_string()) }))
    },
);

let agent = LlmAgentBuilder::new("weather_assistant")
    .model(Arc::new(model))
    .instruction("You help users check weather.")
    .tool(Arc::new(weather_tool))
    .build()?;
```

---

## Documentation Updates

### Files to Update

1. **README.md** - Add OpenAI to supported providers
2. **docs/official_docs/models/models.md** - Document OpenAI models
3. **docs/official_docs/models/azure.md** - New file for Azure setup
4. **adk-model/README.md** - Update with OpenAI examples
5. **CHANGELOG.md** - Document new feature

### New Example Files

```
examples/
├── openai_basic/           # Basic chat with OpenAI
├── openai_tools/           # Function calling example
├── openai_streaming/       # Streaming responses
├── azure_openai/           # Azure OpenAI setup
└── ollama_local/           # Local Ollama usage
```

---

## Testing Plan

### Unit Tests

1. Type conversion tests (ADK <-> OpenAI)
2. Tool declaration conversion
3. Tool call ID preservation
4. Error handling

### Integration Tests

1. Basic chat completion
2. Multi-turn conversation
3. Tool calling flow
4. Streaming responses
5. Azure OpenAI connection
6. Compatible API (mock server)

### Manual Testing

1. Test with real OpenAI API key
2. Test with Azure deployment
3. Test with Ollama locally

---

## Implementation Phases

### Phase 1: Core Implementation
- [ ] Add `async-openai` dependency with feature flag
- [ ] Create `openai/` module structure
- [ ] Implement `OpenAIClient` with basic chat
- [ ] Add type conversions

### Phase 2: Tool Calling
- [ ] Update `Part` enum with `id` field
- [ ] Update all affected files
- [ ] Implement tool call conversion
- [ ] Test tool calling flow

### Phase 3: Azure & Compatible
- [ ] Implement `AzureOpenAIClient`
- [ ] Implement `compatible()` constructor
- [ ] Test with various providers

### Phase 4: Documentation & Examples
- [ ] Update documentation
- [ ] Create example projects
- [ ] Update CHANGELOG

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Breaking change from `id` field | Medium | High | Use `Option<String>` with serde skip |
| async-openai API changes | Low | Medium | Pin to specific version |
| Azure auth complexity | Medium | Medium | Clear documentation |
| Streaming edge cases | Medium | Medium | Comprehensive tests |

---

## Timeline Estimate

- Phase 1: Core Implementation - 2-3 hours
- Phase 2: Tool Calling - 2-3 hours
- Phase 3: Azure & Compatible - 1-2 hours
- Phase 4: Documentation - 1-2 hours

**Total: 6-10 hours**

---

## Approval Checklist

- [ ] Architecture review
- [ ] API design review
- [ ] Breaking change assessment
- [ ] Test plan review
- [ ] Documentation plan review
