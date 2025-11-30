# Multi-Agent Systems

Multi-agent systems allow you to build sophisticated applications by composing multiple agents into hierarchies. This enables modularity, specialization, and structured control flows.

## Overview

In ADK-Rust, a multi-agent system is created by configuring agents with sub-agents, forming a parent-child hierarchy. The parent agent can coordinate execution, delegate tasks, and manage communication between specialized sub-agents.

Key benefits of multi-agent systems:
- **Modularity**: Break complex tasks into smaller, focused agents
- **Specialization**: Each agent can be optimized for specific tasks
- **Reusability**: Sub-agents can be shared across different parent agents
- **Maintainability**: Easier to understand and modify individual agents

## Sub-Agents Configuration

You can add sub-agents to an `LlmAgent` using the `sub_agent()` builder method:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Create specialized sub-agents
let greeter = LlmAgentBuilder::new("greeter")
    .description("Handles greetings and welcomes users")
    .instruction("Greet users warmly and professionally.")
    .model(model.clone())
    .build()?;

let task_executor = LlmAgentBuilder::new("task_executor")
    .description("Executes specific tasks requested by users")
    .instruction("Execute the requested task efficiently.")
    .model(model.clone())
    .build()?;

// Create parent agent with sub-agents
let coordinator = LlmAgentBuilder::new("coordinator")
    .description("Coordinates between greeting and task execution")
    .instruction("Route user requests to the appropriate sub-agent.")
    .model(model.clone())
    .sub_agent(Arc::new(greeter))
    .sub_agent(Arc::new(task_executor))
    .build()?;
```

### Agent Hierarchy

The sub-agent relationship creates a tree structure:
- Each agent can have multiple sub-agents
- Sub-agents can themselves have their own sub-agents
- This enables multi-level hierarchies for complex applications

## Agent Transfer Behavior

When an `LlmAgent` has sub-agents configured, it gains the ability to transfer execution to those sub-agents. The LLM can dynamically decide which sub-agent should handle a particular request.

### How Transfer Works

1. The parent agent's LLM analyzes the user request
2. Based on sub-agent descriptions, it decides which agent is most suitable
3. The LLM generates a `transfer_to_agent` function call
4. The framework routes execution to the specified sub-agent
5. The sub-agent processes the request and returns results

### Transfer Scope

By default, an agent can transfer to:
- Its direct sub-agents
- Its parent agent
- Its sibling agents (other sub-agents of the same parent)

You can control transfer behavior using builder methods:

```rust
let restricted_agent = LlmAgentBuilder::new("restricted")
    .description("An agent with restricted transfer capabilities")
    .model(model.clone())
    .disallow_transfer_to_parent(true)  // Cannot transfer back to parent
    .disallow_transfer_to_peers(true)   // Cannot transfer to siblings
    .build()?;
```

### Writing Effective Transfer Instructions

For agent transfer to work well, provide clear instructions and descriptions:

```rust
// Parent agent with clear delegation instructions
let coordinator = LlmAgentBuilder::new("coordinator")
    .description("Main coordinator for customer service")
    .instruction(
        "You are a customer service coordinator. \
         Analyze each request and delegate appropriately:\n\
         - For billing questions, transfer to the billing_agent\n\
         - For technical issues, transfer to the support_agent\n\
         - For general inquiries, handle them yourself"
    )
    .model(model.clone())
    .sub_agent(Arc::new(billing_agent))
    .sub_agent(Arc::new(support_agent))
    .build()?;

// Sub-agents with descriptive names and descriptions
let billing_agent = LlmAgentBuilder::new("billing_agent")
    .description("Handles all billing, payment, and invoice questions")
    .instruction("Answer billing questions accurately using available tools.")
    .model(model.clone())
    .build()?;

let support_agent = LlmAgentBuilder::new("support_agent")
    .description("Provides technical support and troubleshooting assistance")
    .instruction("Help users resolve technical issues step by step.")
    .model(model.clone())
    .build()?;
```

## Global Instruction

The `global_instruction` provides tree-wide configuration that applies to all agents in the hierarchy. This is useful for setting consistent personality, tone, or context across your entire agent system.

### Basic Usage

```rust
let agent = LlmAgentBuilder::new("assistant")
    .description("A helpful assistant")
    .global_instruction(
        "You are a professional assistant for Acme Corp. \
         Always maintain a friendly but professional tone. \
         Our company values are: customer-first, innovation, and integrity."
    )
    .instruction("Help users with their questions and tasks.")
    .model(model.clone())
    .build()?;
```

### Global vs Agent Instruction

- **Global Instruction**: Applied to all agents in the hierarchy, sets overall personality/context
- **Agent Instruction**: Specific to each agent, defines its particular role and behavior

Both instructions are included in the conversation history, with global instruction appearing first.

### Dynamic Global Instructions

For more advanced scenarios, you can use a global instruction provider that computes the instruction dynamically:

```rust
use adk_core::GlobalInstructionProvider;

let provider: GlobalInstructionProvider = Arc::new(|ctx| {
    Box::pin(async move {
        // Access context information
        let user_id = ctx.user_id();
        
        // Compute dynamic instruction
        let instruction = format!(
            "You are assisting user {}. Tailor your responses to their preferences.",
            user_id
        );
        
        Ok(instruction)
    })
});

let agent = LlmAgentBuilder::new("assistant")
    .description("A personalized assistant")
    .global_instruction_provider(provider)
    .model(model.clone())
    .build()?;
```

### State Variable Injection

Both global and agent instructions support state variable injection using `{variable}` syntax:

```rust
// Set state in a previous agent or tool
// state["company_name"] = "Acme Corp"
// state["user_role"] = "manager"

let agent = LlmAgentBuilder::new("assistant")
    .global_instruction(
        "You are an assistant for {company_name}. \
         The user is a {user_role}."
    )
    .instruction("Help with {user_role}-level tasks.")
    .model(model.clone())
    .build()?;
```

The framework automatically injects values from the session state into the instruction templates.

## Common Multi-Agent Patterns

### Coordinator/Dispatcher Pattern

A central agent routes requests to specialized sub-agents:

```rust
let billing = LlmAgentBuilder::new("billing")
    .description("Handles billing and payment questions")
    .model(model.clone())
    .build()?;

let support = LlmAgentBuilder::new("support")
    .description("Provides technical support")
    .model(model.clone())
    .build()?;

let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Route requests to billing or support agents as appropriate.")
    .sub_agent(Arc::new(billing))
    .sub_agent(Arc::new(support))
    .model(model.clone())
    .build()?;
```

### Hierarchical Task Decomposition

Multi-level hierarchies for breaking down complex tasks:

```rust
// Low-level specialists
let researcher = LlmAgentBuilder::new("researcher")
    .description("Researches topics and gathers information")
    .model(model.clone())
    .build()?;

let writer = LlmAgentBuilder::new("writer")
    .description("Writes content based on research")
    .model(model.clone())
    .build()?;

// Mid-level coordinator
let content_creator = LlmAgentBuilder::new("content_creator")
    .description("Creates content by coordinating research and writing")
    .sub_agent(Arc::new(researcher))
    .sub_agent(Arc::new(writer))
    .model(model.clone())
    .build()?;

// Top-level manager
let project_manager = LlmAgentBuilder::new("project_manager")
    .description("Manages content creation projects")
    .sub_agent(Arc::new(content_creator))
    .model(model.clone())
    .build()?;
```

### Combining with Workflow Agents

Multi-agent systems work well with workflow agents (Sequential, Parallel, Loop):

```rust
use adk_agent::workflow::{SequentialAgent, ParallelAgent};

// Create specialized agents
let validator = LlmAgentBuilder::new("validator")
    .instruction("Validate the input data.")
    .output_key("validation_result")
    .model(model.clone())
    .build()?;

let processor = LlmAgentBuilder::new("processor")
    .instruction("Process data if {validation_result} is valid.")
    .output_key("processed_data")
    .model(model.clone())
    .build()?;

// Combine in a sequential workflow
let pipeline = SequentialAgent::new(
    "validation_pipeline",
    vec![Arc::new(validator), Arc::new(processor)]
);

// Use the pipeline as a sub-agent
let coordinator = LlmAgentBuilder::new("coordinator")
    .description("Coordinates data processing")
    .sub_agent(Arc::new(pipeline))
    .model(model.clone())
    .build()?;
```

## Communication Between Agents

Agents in a hierarchy communicate through shared session state:

```rust
// Agent A saves data to state
let agent_a = LlmAgentBuilder::new("agent_a")
    .instruction("Analyze the topic and save key points.")
    .output_key("key_points")  // Automatically saves output to state
    .model(model.clone())
    .build()?;

// Agent B reads data from state
let agent_b = LlmAgentBuilder::new("agent_b")
    .instruction("Expand on the key points: {key_points}")
    .model(model.clone())
    .build()?;
```

The `output_key` configuration automatically saves an agent's final response to the session state, making it available to subsequent agents.

## Best Practices

1. **Clear Descriptions**: Write descriptive agent names and descriptions to help the LLM make good transfer decisions
2. **Specific Instructions**: Give each agent clear, focused instructions for its role
3. **Use Global Instruction**: Set consistent personality and context across all agents
4. **State Management**: Use `output_key` and state variables for agent communication
5. **Limit Hierarchy Depth**: Keep hierarchies shallow (2-3 levels) for better maintainability
6. **Test Transfer Logic**: Verify that agents transfer to the correct sub-agents for different requests

## Related

- [LLM Agent](llm-agent.md) - Core agent configuration
- [Workflow Agents](workflow-agents.md) - Sequential, Parallel, and Loop agents
- [Sessions](../sessions/sessions.md) - Session state management
