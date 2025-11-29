# Callbacks

Callbacks in ADK-Rust provide hooks to observe, customize, and control agent behavior at key execution points. They enable logging, guardrails, caching, response modification, and more.

## Overview

ADK-Rust supports six callback types that intercept different stages of agent execution:

| Callback Type | When Executed | Use Cases |
|--------------|---------------|-----------|
| `before_agent` | Before agent starts processing | Input validation, logging, early termination |
| `after_agent` | After agent completes | Response modification, logging, cleanup |
| `before_model` | Before LLM call | Request modification, caching, rate limiting |
| `after_model` | After LLM response | Response filtering, logging, caching |
| `before_tool` | Before tool execution | Permission checks, parameter validation |
| `after_tool` | After tool execution | Result modification, logging |

## Callback Types

### Agent Callbacks

Agent callbacks wrap the entire agent execution cycle.

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// BeforeAgentCallback type signature
type BeforeAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) 
        -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> 
    + Send + Sync
>;

// AfterAgentCallback type signature  
type AfterAgentCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) 
        -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> 
    + Send + Sync
>;
```

### Model Callbacks

Model callbacks intercept LLM requests and responses.

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// BeforeModelResult - controls what happens after the callback
pub enum BeforeModelResult {
    Continue(LlmRequest),  // Continue with (possibly modified) request
    Skip(LlmResponse),     // Skip model call, use this response instead
}

// BeforeModelCallback - can modify request or skip model call
type BeforeModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, LlmRequest)
        -> Pin<Box<dyn Future<Output = Result<BeforeModelResult>> + Send>>
    + Send + Sync
>;

// AfterModelCallback - can modify the response
type AfterModelCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>, LlmResponse)
        -> Pin<Box<dyn Future<Output = Result<Option<LlmResponse>>> + Send>>
    + Send + Sync
>;
```

### Tool Callbacks

Tool callbacks intercept tool execution.

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// BeforeToolCallback - can skip tool by returning Some(Content)
type BeforeToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) 
        -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> 
    + Send + Sync
>;

// AfterToolCallback - can modify tool result
type AfterToolCallback = Box<
    dyn Fn(Arc<dyn CallbackContext>) 
        -> Pin<Box<dyn Future<Output = Result<Option<Content>>> + Send>> 
    + Send + Sync
>;
```

## Return Value Semantics

Callbacks use different return values to control execution flow:

### Agent/Tool Callbacks

| Return Value | Effect |
|-------------|--------|
| `Ok(None)` | Continue normal execution |
| `Ok(Some(content))` | Override/skip with provided content |
| `Err(e)` | Abort execution with error |

### Model Callbacks

**BeforeModelCallback** uses `BeforeModelResult`:

| Return Value | Effect |
|-------------|--------|
| `Ok(BeforeModelResult::Continue(request))` | Continue with the (possibly modified) request |
| `Ok(BeforeModelResult::Skip(response))` | Skip model call, use this response instead |
| `Err(e)` | Abort execution with error |

**AfterModelCallback** uses `Option<LlmResponse>`:

| Return Value | Effect |
|-------------|--------|
| `Ok(None)` | Keep the original response |
| `Ok(Some(response))` | Replace with the modified response |
| `Err(e)` | Abort execution with error |

### Summary

- **Before agent/tool callbacks**: Return `None` to continue, `Some(content)` to skip
- **Before model callback**: Return `Continue(request)` to proceed, `Skip(response)` to bypass the model
- **After callbacks**: Return `None` to keep original, `Some(...)` to replace

## Adding Callbacks to Agents

Callbacks are added to agents using the `LlmAgentBuilder`:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    let agent = LlmAgentBuilder::new("my_agent")
        .model(model)
        .instruction("You are a helpful assistant.")
        // Add before_agent callback
        .before_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("Agent starting: {}", ctx.agent_name());
                Ok(None) // Continue execution
            })
        }))
        // Add after_agent callback
        .after_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("Agent completed: {}", ctx.agent_name());
                Ok(None) // Keep original result
            })
        }))
        .build()?;

    Ok(())
}
```

## CallbackContext Interface

The `CallbackContext` trait provides access to execution context:

```rust
use adk_rust::prelude::*;

#[async_trait]
pub trait CallbackContext: ReadonlyContext {
    /// Access artifact storage (if configured)
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
}

// CallbackContext extends ReadonlyContext
#[async_trait]
pub trait ReadonlyContext: Send + Sync {
    /// Current invocation ID
    fn invocation_id(&self) -> &str;
    
    /// Name of the current agent
    fn agent_name(&self) -> &str;
    
    /// User ID from session
    fn user_id(&self) -> &str;
    
    /// Application name
    fn app_name(&self) -> &str;
    
    /// Session ID
    fn session_id(&self) -> &str;
    
    /// Current branch (for multi-agent)
    fn branch(&self) -> &str;
    
    /// The user's input content
    fn user_content(&self) -> &Content;
}
```

## Common Patterns

### Logging Callback

Log all agent interactions:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("logged_agent")
    .model(model)
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            println!("[LOG] Agent '{}' starting", ctx.agent_name());
            println!("[LOG] Session: {}", ctx.session_id());
            println!("[LOG] User: {}", ctx.user_id());
            Ok(None)
        })
    }))
    .after_callback(Box::new(|ctx| {
        Box::pin(async move {
            println!("[LOG] Agent '{}' completed", ctx.agent_name());
            Ok(None)
        })
    }))
    .build()?;
```

### Input Guardrails

Block inappropriate content before processing:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("guarded_agent")
    .model(model)
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            // Check user input for blocked content
            let user_content = ctx.user_content();
            for part in &user_content.parts {
                if let Part::Text { text } = part {
                    if text.to_lowercase().contains("blocked_word") {
                        // Return early with rejection message
                        return Ok(Some(Content {
                            role: "model".to_string(),
                            parts: vec![Part::Text {
                                text: "I cannot process that request.".to_string(),
                            }],
                        }));
                    }
                }
            }
            Ok(None) // Continue normal execution
        })
    }))
    .build()?;
```

### Response Caching (Before Model)

Cache LLM responses to reduce API calls:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;
use std::collections::HashMap;
use std::sync::Mutex;

// Simple in-memory cache
let cache: Arc<Mutex<HashMap<String, LlmResponse>>> = Arc::new(Mutex::new(HashMap::new()));
let cache_clone = cache.clone();

let agent = LlmAgentBuilder::new("cached_agent")
    .model(model)
    .before_model_callback(Box::new(move |ctx, request| {
        let cache = cache_clone.clone();
        Box::pin(async move {
            // Create cache key from request contents
            let key = format!("{:?}", request.contents);

            // Check cache
            if let Some(cached) = cache.lock().unwrap().get(&key) {
                println!("[CACHE] Hit for request");
                return Ok(BeforeModelResult::Skip(cached.clone()));
            }

            println!("[CACHE] Miss, calling model");
            Ok(BeforeModelResult::Continue(request)) // Continue to model
        })
    }))
    .build()?;
```

### Injecting Multimodal Content (Before Model)

Inject images or other binary content into LLM requests for multimodal analysis:

```rust
use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, LoadRequest};
use std::sync::Arc;

// Artifact service with pre-loaded image
let artifact_service: Arc<dyn ArtifactService> = /* ... */;
let callback_service = artifact_service.clone();

let agent = LlmAgentBuilder::new("image_analyst")
    .model(model)
    .instruction("Describe the image provided by the user.")
    .before_model_callback(Box::new(move |_ctx, mut request| {
        let service = callback_service.clone();
        Box::pin(async move {
            // Load image from artifact storage
            if let Ok(response) = service.load(LoadRequest {
                app_name: "my_app".to_string(),
                user_id: "user".to_string(),
                session_id: "session".to_string(),
                file_name: "user:photo.png".to_string(),
                version: None,
            }).await {
                // Inject image into the user's message
                if let Some(last_content) = request.contents.last_mut() {
                    if last_content.role == "user" {
                        last_content.parts.push(response.part);
                    }
                }
            }

            Ok(BeforeModelResult::Continue(request))
        })
    }))
    .build()?;
```

This pattern is essential for multimodal AI because tool responses are JSON text - the model can't "see" images returned by tools. By injecting the image directly into the request, the model receives actual image data.

### Response Modification (After Model)

Modify or filter model responses:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("filtered_agent")
    .model(model)
    .after_model_callback(Box::new(|ctx, mut response| {
        Box::pin(async move {
            // Modify the response content
            if let Some(ref mut content) = response.content {
                for part in &mut content.parts {
                    if let Part::Text { text } = part {
                        // Add disclaimer to all responses
                        *text = format!("{}\n\n[AI-generated response]", text);
                    }
                }
            }
            Ok(Some(response))
        })
    }))
    .build()?;
```

### Tool Permission Check (Before Tool)

Validate tool execution permissions:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("permission_agent")
    .model(model)
    .tool(Arc::new(GoogleSearchTool::new()))
    .before_tool_callback(Box::new(|ctx| {
        Box::pin(async move {
            // Check if user has permission for tools
            let user_id = ctx.user_id();
            
            // Example: block certain users from using tools
            if user_id == "restricted_user" {
                return Ok(Some(Content {
                    role: "function".to_string(),
                    parts: vec![Part::Text {
                        text: "Tool access denied for this user.".to_string(),
                    }],
                }));
            }
            
            Ok(None) // Allow tool execution
        })
    }))
    .build()?;
```

### Tool Result Logging (After Tool)

Log all tool executions:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("tool_logged_agent")
    .model(model)
    .tool(Arc::new(GoogleSearchTool::new()))
    .after_tool_callback(Box::new(|ctx| {
        Box::pin(async move {
            println!("[TOOL LOG] Tool executed for agent: {}", ctx.agent_name());
            println!("[TOOL LOG] Session: {}", ctx.session_id());
            Ok(None) // Keep original result
        })
    }))
    .build()?;
```

## Multiple Callbacks

You can add multiple callbacks of the same type. They execute in order:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("multi_callback_agent")
    .model(model)
    // First before callback - logging
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            println!("[1] Logging callback");
            Ok(None)
        })
    }))
    // Second before callback - validation
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            println!("[2] Validation callback");
            Ok(None)
        })
    }))
    .build()?;
```

When a callback returns `Some(content)`, subsequent callbacks of the same type are skipped.

## Error Handling

Callbacks can return errors to abort execution:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("error_handling_agent")
    .model(model)
    .before_callback(Box::new(|ctx| {
        Box::pin(async move {
            // Validate something critical
            if ctx.user_id().is_empty() {
                return Err(AdkError::Agent("User ID is required".to_string()));
            }
            Ok(None)
        })
    }))
    .build()?;
```

## Best Practices

1. **Keep callbacks lightweight**: Avoid heavy computation in callbacks
2. **Handle errors gracefully**: Return meaningful error messages
3. **Use logging sparingly**: Too much logging can impact performance
4. **Cache wisely**: Consider cache invalidation strategies
5. **Test callbacks independently**: Unit test callback logic separately

## Related

- [LlmAgent](../agents/llm-agent.md) - Agent configuration
- [Tools](../tools/function-tools.md) - Tool system
- [Events](../events/events.md) - Event structure
