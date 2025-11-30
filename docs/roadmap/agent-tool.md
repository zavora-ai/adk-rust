# Agent-as-a-Tool

> **Status**: Implemented
> **Location**: `adk-tool/src/agent_tool.rs`
> **Example**: `examples/agent_tool/main.rs`

## Overview

Agent-as-a-Tool (AgentTool) enables agents to call other agents as if they were regular tools. This powerful composition pattern allows building complex multi-agent systems where specialized agents can be invoked by a coordinator agent, each handling specific domains or tasks.

## Features

AgentTool wraps an agent as a tool, enabling:

- **Seamless Composition**: Call agents like any other tool
- **Specialized Agents**: Create domain-specific agents as reusable tools
- **Tool Isolation**: Use agents with incompatible tool types together
- **Dynamic Invocation**: Let the LLM decide when to invoke sub-agents

## Basic Usage

```rust
use adk_agent::LlmAgentBuilder;
use adk_tool::AgentTool;
use std::sync::Arc;

// Create a specialized agent
let math_agent = LlmAgentBuilder::new("math_expert")
    .description("A math expert that solves mathematical problems")
    .instruction("You are a math expert. Solve mathematical problems step by step.")
    .model(model.clone())
    .tool(Arc::new(calculator_tool))
    .build()?;

// Wrap it as a tool
let math_tool = AgentTool::new(Arc::new(math_agent));

// Use in coordinator agent
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Help users by delegating to specialized agents")
    .model(model)
    .tool(Arc::new(math_tool))
    .build()?;
```

## Configuration Options

```rust
use adk_tool::{AgentTool, AgentToolConfig};
use std::time::Duration;

// Configure agent tool behavior
let agent_tool = AgentTool::new(Arc::new(specialized_agent))
    .skip_summarization(false)   // Summarize sub-agent output
    .forward_artifacts(true)     // Share artifacts with parent
    .timeout(Duration::from_secs(60));

// Or use AgentToolConfig directly
let config = AgentToolConfig {
    skip_summarization: false,
    forward_artifacts: true,
    timeout: Some(Duration::from_secs(60)),
    input_schema: None,
    output_schema: None,
};
let agent_tool = AgentTool::with_config(Arc::new(agent), config);
```

## Implementation Status

### Core Features (Implemented)
- [x] `AgentTool` struct wrapping an `Agent`
- [x] `Tool` trait implementation for `AgentTool`
- [x] Generate function declaration from agent metadata
- [x] Custom input/output schema support
- [x] Isolated session for sub-agent execution
- [x] Artifact forwarding (configurable)
- [x] Timeout handling
- [x] Telemetry and tracing integration
- [x] Unit tests
- [x] Example: Multi-domain coordinator (`examples/agent_tool/`)

### Future Enhancements
- [ ] State forwarding from parent to sub-agent
- [ ] State delta propagation back to parent
- [ ] Streaming responses from sub-agents
- [ ] Session reuse for repeated calls
- [ ] Nested agent tool chains

## API Design

### AgentTool Structure

```rust,ignore
pub struct AgentTool {
    agent: Arc<dyn Agent>,
    config: AgentToolConfig,
}

pub struct AgentToolConfig {
    /// Skip summarization after sub-agent execution
    pub skip_summarization: bool,
    
    /// Forward artifacts between parent and sub-agent
    pub forward_artifacts: bool,
    
    /// Timeout for sub-agent execution
    pub timeout: Option<Duration>,
}

impl AgentTool {
    pub fn new(agent: Arc<dyn Agent>, config: Option<AgentToolConfig>) -> Self {
        Self {
            agent,
            config: config.unwrap_or_default(),
        }
    }
}
```

### Tool Trait Implementation

```rust,ignore
#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        self.agent.name()
    }
    
    fn description(&self) -> &str {
        self.agent.description()
    }
    
    fn is_long_running(&self) -> bool {
        false
    }
    
    async fn call(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: Value,
    ) -> Result<Value> {
        // Validate input against agent's input schema
        // Create isolated session for sub-agent
        // Execute sub-agent with args
        // Validate output against agent's output schema
        // Return result
    }
}
```

### Function Declaration Generation

```rust,ignore
impl AgentTool {
    fn generate_declaration(&self) -> FunctionDeclaration {
        let mut decl = FunctionDeclaration {
            name: self.agent.name().to_string(),
            description: self.agent.description().to_string(),
            parameters: None,
        };
        
        // Use agent's input schema if available
        if let Some(input_schema) = self.agent.input_schema() {
            decl.parameters = Some(input_schema.clone());
        } else {
            // Default schema with "request" parameter
            decl.parameters = Some(json!({
                "type": "object",
                "properties": {
                    "request": {"type": "string"}
                },
                "required": ["request"]
            }));
        }
        
        decl
    }
}
```

## Use Cases

### Multi-Domain Coordinator

```rust,ignore
// Create specialized agents
let weather_agent = LlmAgentBuilder::new("weather_expert")
    .instruction("Provide weather information")
    .tools(vec![weather_api_tool])
    .build()?;

let calendar_agent = LlmAgentBuilder::new("calendar_expert")
    .instruction("Manage calendar and scheduling")
    .tools(vec![calendar_api_tool])
    .build()?;

let email_agent = LlmAgentBuilder::new("email_expert")
    .instruction("Handle email operations")
    .tools(vec![email_api_tool])
    .build()?;

// Wrap as tools
let weather_tool = AgentTool::new(weather_agent, None);
let calendar_tool = AgentTool::new(calendar_agent, None);
let email_tool = AgentTool::new(email_agent, None);

// Coordinator delegates to specialists
let coordinator = LlmAgentBuilder::new("assistant")
    .instruction("Help users by delegating to specialized agents")
    .tools(vec![
        Arc::new(weather_tool),
        Arc::new(calendar_tool),
        Arc::new(email_tool),
    ])
    .build()?;

// User: "What's the weather tomorrow and schedule a meeting"
// Coordinator calls weather_tool and calendar_tool automatically
```

### Tool Type Isolation

```rust,ignore
// Agent using Google Search (code execution incompatible)
let research_agent = LlmAgentBuilder::new("researcher")
    .instruction("Research topics using web search")
    .tools(vec![google_search_tool])
    .build()?;

// Agent using Code Execution (search incompatible)
let coder_agent = LlmAgentBuilder::new("coder")
    .instruction("Write and execute code")
    .tools(vec![code_execution_tool])
    .build()?;

// Wrap as tools to use together
let research_tool = AgentTool::new(research_agent, None);
let coder_tool = AgentTool::new(coder_agent, None);

// Main agent can use both!
let main_agent = LlmAgentBuilder::new("assistant")
    .instruction("Help with research and coding")
    .tools(vec![
        Arc::new(research_tool),
        Arc::new(coder_tool),
    ])
    .build()?;
```

### Structured Data Processing

```rust,ignore
// Agent with strict input/output schemas
let data_processor = LlmAgentBuilder::new("data_processor")
    .instruction("Process and transform data")
    .input_schema(json!({
        "type": "object",
        "properties": {
            "data": {"type": "array"},
            "operation": {"type": "string", "enum": ["filter", "map", "reduce"]}
        },
        "required": ["data", "operation"]
    }))
    .output_schema(json!({
        "type": "object",
        "properties": {
            "result": {"type": "array"},
            "count": {"type": "number"}
        }
    }))
    .build()?;

// Wrap as tool - schemas enforced automatically
let processor_tool = AgentTool::new(data_processor, None);

// Coordinator can call with structured data
let coordinator = LlmAgentBuilder::new("coordinator")
    .tools(vec![Arc::new(processor_tool)])
    .build()?;
```

## State Management

### State Forwarding

```rust,ignore
// Parent agent state
parent_state = {
    "user:name": "Alice",
    "user:preferences": {"theme": "dark"},
    "temp:current_task": "analysis",
    "_adk_internal": "hidden"
}

// Forwarded to sub-agent (filtered)
sub_agent_state = {
    "user:name": "Alice",
    "user:preferences": {"theme": "dark"},
    "temp:current_task": "analysis"
    // _adk_internal filtered out
}
```

### State Isolation

```rust,ignore
// Sub-agent modifications don't affect parent
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        isolate_state: true,  // Changes stay in sub-agent
        ..Default::default()
    })
);
```

## Artifact Handling

### Artifact Forwarding

```rust,ignore
// Enable artifact sharing
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        forward_artifacts: true,
        ..Default::default()
    })
);

// Sub-agent can access parent's artifacts
// Sub-agent's artifacts visible to parent
```

### Artifact Isolation

```rust,ignore
// Isolate artifacts
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        forward_artifacts: false,
        ..Default::default()
    })
);

// Sub-agent has separate artifact namespace
```

## Error Handling

```rust,ignore
// Handle sub-agent errors
match agent_tool.call(ctx, args).await {
    Ok(result) => process_result(result),
    Err(AdkError::AgentTimeout(_)) => {
        // Sub-agent exceeded timeout
        handle_timeout()
    }
    Err(AdkError::ValidationError(_)) => {
        // Input/output schema validation failed
        handle_validation_error()
    }
    Err(e) => handle_other_error(e),
}
```

## Performance Considerations

### Timeout Configuration

```rust,ignore
// Set reasonable timeouts
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    })
);
```

### Summarization Control

```rust,ignore
// Skip summarization for faster responses
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        skip_summarization: true,  // Return raw output
        ..Default::default()
    })
);
```

### Session Reuse

```rust,ignore
// Reuse sessions for repeated calls (future enhancement)
let agent_tool = AgentTool::new(
    sub_agent,
    Some(AgentToolConfig {
        reuse_session: true,
        ..Default::default()
    })
);
```

## Comparison with adk-go

ADK-Go has full AgentTool support with:
- `AgentTool` wrapper for agents
- Input/output schema validation
- State forwarding and isolation
- Artifact forwarding
- Summarization control
- Comprehensive examples

ADK-Rust will achieve feature parity with these capabilities.

## Best Practices

### Agent Design

1. **Single Responsibility**: Each agent should have a clear, focused purpose
2. **Clear Descriptions**: Write descriptive agent descriptions for LLM understanding
3. **Schema Definition**: Define input/output schemas for structured agents
4. **Error Messages**: Provide clear error messages for debugging

### Coordinator Patterns

1. **Delegation**: Let coordinator decide which specialist to call
2. **Context Passing**: Forward relevant context to sub-agents
3. **Result Aggregation**: Combine results from multiple sub-agents
4. **Error Recovery**: Handle sub-agent failures gracefully

### Performance

1. **Timeout Management**: Set appropriate timeouts for sub-agents
2. **Minimize Nesting**: Avoid deep agent-in-agent hierarchies
3. **Cache Results**: Cache sub-agent results when appropriate
4. **Monitor Costs**: Track token usage across agent calls

## Timeline

AgentTool support is planned for a future release. The implementation will follow the design patterns established in ADK-Go while leveraging Rust's type safety and async capabilities.

Key milestones:
1. Core AgentTool implementation
2. Schema validation integration
3. State and artifact forwarding
4. Configuration options
5. Example applications
6. Comprehensive testing and documentation

## Contributing

If you're interested in contributing to AgentTool support in ADK-Rust, please:

1. Review the existing code in `adk-agent/`
2. Familiarize yourself with the Tool trait in `adk-tool/`
3. Check the ADK-Go implementation for reference
4. Open an issue to discuss your approach

---

**Related**:
- [Multi-Agent Documentation](../official_docs/agents/multi-agent.md)
- [Function Tools Documentation](../official_docs/tools/function-tools.md)
- [Sessions Documentation](../official_docs/sessions/sessions.md)

**Note**: This is a roadmap document. The APIs and examples shown here are illustrative and subject to change during implementation.
