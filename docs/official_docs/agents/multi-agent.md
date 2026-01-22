# Multi-Agent Systems

Build sophisticated applications by composing specialized agents into teams.

## What You'll Build

In this guide, you'll create a **Customer Service System** where a coordinator routes queries to specialists:

```
                        ┌─────────────────────┐
       User Query       │                     │
      ────────────────▶ │    COORDINATOR      │
                        │  "Route to expert"  │
                        └──────────┬──────────┘
                                   │
                   ┌───────────────┴───────────────┐
                   │                               │
                   ▼                               ▼
        ┌──────────────────┐            ┌──────────────────┐
        │  BILLING AGENT   │            │  SUPPORT AGENT   │
        │                  │            │                  │
        │  💰 Payments     │            │  🔧 Tech Issues  │
        │  📄 Invoices     │            │  🐛 Bug Reports  │
        │  💳 Subscriptions│            │  ❓ How-To       │
        └──────────────────┘            └──────────────────┘
```

**Key Concepts:**
- **Coordinator** - Receives all requests, decides who handles them
- **Specialists** - Focused agents that excel at specific domains
- **Transfer** - Seamless handoff from coordinator to specialist

---

## Quick Start

### 1. Create Your Project

```bash
cargo new multi_agent_demo
cd multi_agent_demo
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "0.2.1", features = ["agents", "models", "cli"] }
tokio = { version = "1", features = ["full"] }
dotenvy = "0.15"
```

Create `.env` with your API key:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

### 2. Customer Service Example

Here's a complete working example:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Specialist: Billing Agent
    let billing_agent = LlmAgentBuilder::new("billing_agent")
        .description("Handles billing questions: payments, invoices, subscriptions, refunds")
        .instruction("You are a billing specialist. Help customers with:\n\
                     - Invoice questions and payment history\n\
                     - Subscription plans and upgrades\n\
                     - Refund requests\n\
                     - Payment method updates\n\
                     Be professional and provide clear information about billing matters.")
        .model(model.clone())
        .build()?;

    // Specialist: Technical Support Agent
    let support_agent = LlmAgentBuilder::new("support_agent")
        .description("Handles technical support: bugs, errors, troubleshooting, how-to questions")
        .instruction("You are a technical support specialist. Help customers with:\n\
                     - Troubleshooting errors and bugs\n\
                     - How-to questions about using the product\n\
                     - Configuration and setup issues\n\
                     - Performance problems\n\
                     Be patient and provide step-by-step guidance.")
        .model(model.clone())
        .build()?;

    // Coordinator: Routes to appropriate specialist
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Main customer service coordinator")
        .instruction("You are a customer service coordinator. Analyze each customer request:\n\n\
                     - For BILLING questions (payments, invoices, subscriptions, refunds):\n\
                       Transfer to billing_agent\n\n\
                     - For TECHNICAL questions (errors, bugs, how-to, troubleshooting):\n\
                       Transfer to support_agent\n\n\
                     - For GENERAL greetings or unclear requests:\n\
                       Respond yourself and ask clarifying questions\n\n\
                     When transferring, briefly acknowledge the customer and explain the handoff.")
        .model(model.clone())
        .sub_agent(Arc::new(billing_agent))
        .sub_agent(Arc::new(support_agent))
        .build()?;

    println!("🏢 Customer Service Center");
    println!("   Coordinator → Billing Agent | Support Agent");
    println!();

    Launcher::new(Arc::new(coordinator)).run().await?;
    Ok(())
}
```

**Example Interaction:**

```
You: I have a question about my last invoice

[Agent: coordinator]
Assistant: I'll connect you with our billing specialist to help with your invoice question.

[Agent: billing_agent]
Assistant: Hello! I can help you with your invoice. What specific question do you have about your last invoice?

You: Why was I charged twice?

[Agent: billing_agent]
Assistant: I understand your concern about the duplicate charge. Let me help you investigate this...
```

## How Multi-Agent Transfer Works

### The Big Picture

When you add sub-agents to a parent agent, the LLM gains the ability to **delegate** tasks:

```
                    ┌─────────────────────┐
    User Message    │                     │
   ─────────────────▶    COORDINATOR      │
                    │                     │
                    └──────────┬──────────┘
                               │
           "This is a billing question..."
                               │
              ┌────────────────┴────────────────┐
              │                                 │
              ▼                                 ▼
   ┌──────────────────┐              ┌──────────────────┐
   │  billing_agent   │              │  support_agent   │
   │  💰 Payments     │              │  🔧 Tech Issues  │
   │  📄 Invoices     │              │  🐛 Bug Reports  │
   └──────────────────┘              └──────────────────┘
```

### Step-by-Step Transfer Flow

Here's exactly what happens when a user asks a billing question:

```
┌──────────────────────────────────────────────────────────────────────┐
│ STEP 1: User sends message                                           │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   User: "Why was I charged twice on my invoice?"                     │
│                                                                      │
│                              ↓                                       │
│                                                                      │
│   ┌──────────────────────────────────────┐                          │
│   │         COORDINATOR AGENT            │                          │
│   │  Receives message first              │                          │
│   └──────────────────────────────────────┘                          │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────────────┐
│ STEP 2: LLM analyzes and decides to transfer                         │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   🧠 LLM thinks: "This is about an invoice charge..."                │
│                  "Invoice = billing topic..."                        │
│                  "I should transfer to billing_agent"                │
│                                                                      │
│   📞 LLM calls: transfer_to_agent(agent_name="billing_agent")        │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────────────┐
│ STEP 3: Runner detects transfer and invokes target                   │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   ┌─────────┐     transfer event      ┌─────────────────┐           │
│   │ Runner  │ ─────────────────────▶  │  billing_agent  │           │
│   └─────────┘   (same user message)   └─────────────────┘           │
│                                                                      │
│   • Runner finds "billing_agent" in agent tree                       │
│   • Creates new context with SAME user message                       │
│   • Invokes billing_agent immediately                                │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────────────┐
│ STEP 4: Target agent responds                                        │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   ┌─────────────────────────────────────────┐                       │
│   │           billing_agent responds        │                       │
│   │                                         │                       │
│   │  "I can help with your duplicate        │                       │
│   │   charge. Let me investigate..."        │                       │
│   └─────────────────────────────────────────┘                       │
│                                                                      │
│   ✅ User sees seamless response - no interruption!                  │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

### What Makes It Work

| Component | Role |
|-----------|------|
| `.sub_agent()` | Registers specialists under parent |
| `transfer_to_agent` tool | Auto-injected when sub-agents exist |
| Agent descriptions | Help LLM decide which agent handles what |
| Runner | Detects transfer events and invokes target agent |
| Shared session | State and history preserved across transfers |

### Before vs After Adding Sub-Agents

**Without sub-agents** - One agent does everything:
```
User ──▶ coordinator ──▶ Response (handles billing AND support)
```

**With sub-agents** - Specialists handle their domain:
```
User ──▶ coordinator ──▶ billing_agent ──▶ Response (billing expert)
                    ──▶ support_agent ──▶ Response (tech expert)
```

---

## Hierarchical Multi-Agent Systems

For complex scenarios, you can create **multi-level hierarchies**. Each agent can have its own sub-agents, forming a tree:

### Visual: 3-Level Content Team

```
                    ┌─────────────────────┐
                    │  PROJECT MANAGER    │  ← Level 1: Top-level coordinator
                    │  "Manage projects"  │
                    └──────────┬──────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │  CONTENT CREATOR    │  ← Level 2: Mid-level coordinator  
                    │  "Coordinate R&W"   │
                    └──────────┬──────────┘
                               │
              ┌────────────────┴────────────────┐
              │                                 │
              ▼                                 ▼
   ┌──────────────────┐              ┌──────────────────┐
   │   RESEARCHER     │              │     WRITER       │  ← Level 3: Specialists
   │                  │              │                  │
   │  📚 Gather facts │              │  ✍️ Write content │
   │  🔍 Analyze data │              │  📝 Polish text  │
   │  📊 Find sources │              │  🎨 Style & tone │
   └──────────────────┘              └──────────────────┘
```

### How Requests Flow Down

```
User: "Create a blog post about electric vehicles"
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  PROJECT MANAGER: "This is a content task"                  │
│  → transfers to content_creator                             │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  CONTENT CREATOR: "Need research first, then writing"       │
│  → transfers to researcher                                  │
└─────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────┐
│  RESEARCHER: "Here's what I found about EVs..."             │
│  → provides research summary                                │
└─────────────────────────────────────────────────────────────┘
```

### Complete Example Code

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Level 3: Leaf specialists
    let researcher = LlmAgentBuilder::new("researcher")
        .description("Researches topics and gathers comprehensive information")
        .instruction("You are a research specialist. When asked to research a topic:\n\
                     - Gather key facts and data\n\
                     - Identify main themes and subtopics\n\
                     - Note important sources or references\n\
                     Provide thorough, well-organized research summaries.")
        .model(model.clone())
        .build()?;

    let writer = LlmAgentBuilder::new("writer")
        .description("Writes polished content based on research")
        .instruction("You are a content writer. When asked to write:\n\
                     - Create engaging, clear content\n\
                     - Use appropriate tone for the audience\n\
                     - Structure content logically\n\
                     - Polish for grammar and style\n\
                     Produce professional, publication-ready content.")
        .model(model.clone())
        .build()?;

    // Level 2: Content coordinator
    let content_creator = LlmAgentBuilder::new("content_creator")
        .description("Coordinates content creation by delegating research and writing")
        .instruction("You are a content creation lead. For content requests:\n\n\
                     - If RESEARCH is needed: Transfer to researcher\n\
                     - If WRITING is needed: Transfer to writer\n\
                     - For PLANNING or overview: Handle yourself\n\n\
                     Coordinate between research and writing phases.")
        .model(model.clone())
        .sub_agent(Arc::new(researcher))
        .sub_agent(Arc::new(writer))
        .build()?;

    // Level 1: Top-level manager
    let project_manager = LlmAgentBuilder::new("project_manager")
        .description("Manages projects and coordinates with content team")
        .instruction("You are a project manager. For incoming requests:\n\n\
                     - For CONTENT creation tasks: Transfer to content_creator\n\
                     - For PROJECT STATUS or general questions: Handle yourself\n\n\
                     Keep track of overall project goals and deadlines.")
        .model(model.clone())
        .sub_agent(Arc::new(content_creator))
        .build()?;

    println!("📊 Hierarchical Multi-Agent System");
    println!();
    println!("   project_manager");
    println!("       └── content_creator");
    println!("               ├── researcher");
    println!("               └── writer");
    println!();

    Launcher::new(Arc::new(project_manager)).run().await?;
    Ok(())
}
```

**Agent Hierarchy:**
```
project_manager
└── content_creator
    ├── researcher
    └── writer
```

**Example prompts:**
- "Create a blog post about AI in healthcare" → PM → Content Creator → Writer
- "Research electric vehicles" → PM → Content Creator → Researcher

## Sub-Agent Configuration

Add sub-agents to any `LlmAgent` using the `sub_agent()` builder method:

```rust
let parent = LlmAgentBuilder::new("parent")
    .description("Coordinates specialized tasks")
    .instruction("Route requests to appropriate specialists.")
    .model(model.clone())
    .sub_agent(Arc::new(specialist_a))
    .sub_agent(Arc::new(specialist_b))
    .build()?;
```

**Key Points:**
- Each agent can have multiple sub-agents
- Sub-agents can have their own sub-agents (multi-level hierarchies)
- Agent names must be unique within the hierarchy
- Descriptions help the LLM decide which agent to transfer to

## Writing Effective Transfer Instructions

For successful agent transfers, provide clear instructions and descriptions:

### Parent Agent Instructions

```rust
let coordinator = LlmAgentBuilder::new("coordinator")
    .description("Main customer service coordinator")
    .instruction("You are a customer service coordinator. Analyze each request:\n\n\
                 - For BILLING questions (payments, invoices, subscriptions):\n\
                   Transfer to billing_agent\n\n\
                 - For TECHNICAL questions (errors, bugs, troubleshooting):\n\
                   Transfer to support_agent\n\n\
                 - For GENERAL greetings or unclear requests:\n\
                   Respond yourself and ask clarifying questions")
    .model(model.clone())
    .sub_agent(Arc::new(billing_agent))
    .sub_agent(Arc::new(support_agent))
    .build()?;
```

### Sub-Agent Descriptions

```rust
let billing_agent = LlmAgentBuilder::new("billing_agent")
    .description("Handles billing questions: payments, invoices, subscriptions, refunds")
    .instruction("You are a billing specialist. Help with payment and subscription issues.")
    .model(model.clone())
    .build()?;

let support_agent = LlmAgentBuilder::new("support_agent")
    .description("Handles technical support: bugs, errors, troubleshooting, how-to questions")
    .instruction("You are a technical support specialist. Provide step-by-step guidance.")
    .model(model.clone())
    .build()?;
```

**Best Practices:**
- Use **descriptive agent names** that clearly indicate their purpose
- Write **detailed descriptions** - the LLM uses these to decide transfers
- Include **specific keywords** in descriptions that match likely user requests
- Give **clear delegation rules** in parent agent instructions
- Use **consistent terminology** across agent descriptions

## Testing Your Multi-Agent System

### Running Examples

```bash
# Run the customer service example
cargo run --bin customer_service

# Run the hierarchical example  
cargo run --bin hierarchical
```

### Example Test Prompts

**Customer Service:**
- "I have a question about my last invoice" → Should route to `billing_agent`
- "The app keeps crashing" → Should route to `support_agent`
- "How do I upgrade my plan?" → Should route to `billing_agent`
- "Hello, I need help" → Should stay with `coordinator` for clarification

**Hierarchical:**
- "Create a blog post about AI in healthcare" → PM → Content Creator → Writer
- "Research the history of electric vehicles" → PM → Content Creator → Researcher
- "What's the status of our current projects?" → Should stay with `project_manager`

### Debugging Transfer Issues

If transfers aren't working as expected:

1. **Check agent names** - Must match exactly in transfer calls
2. **Review descriptions** - Make them more specific and keyword-rich
3. **Clarify instructions** - Be explicit about when to transfer
4. **Test edge cases** - Try ambiguous requests to see routing behavior
5. **Look for transfer indicators** - `[Agent: name]` shows which agent is responding

## Global Instruction

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

**Example Conversation:**

```
User: I have a question about my last invoice

[Agent: coordinator]
Assistant: I'll connect you with our billing specialist.
🔄 [Transfer requested to: billing]

[Agent: billing]
Assistant: Hello! I can help you with your invoice. 
What specific question do you have?

User: Why was I charged twice?

[Agent: billing]
Assistant: Let me investigate that duplicate charge for you...
```

**Key Points:**
- The coordinator analyzes the request and transfers to the billing agent
- The billing agent responds **immediately** in the same turn
- Subsequent messages continue with the billing agent
- Transfer indicators (`🔄`) show when handoffs occur

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

### AgentTool State and Artifact Forwarding

When using `AgentTool` to wrap agents as tools, state changes and artifacts from sub-agents are automatically forwarded to the parent context:

```rust
use adk_tool::AgentTool;

// Create a sub-agent that modifies state
let data_processor = LlmAgentBuilder::new("data_processor")
    .instruction("Process the data and save results.")
    .output_key("processed_data")
    .model(model.clone())
    .build()?;

// Wrap as a tool - state_delta and artifact_delta are forwarded
let processor_tool = AgentTool::new(Arc::new(data_processor));

// Parent agent can use the tool and see state changes
let coordinator = LlmAgentBuilder::new("coordinator")
    .instruction("Use the data_processor tool, then access {processed_data}.")
    .model(model.clone())
    .tool(Arc::new(processor_tool))
    .build()?;
```

This enables seamless data flow between parent and child agents when using the AgentTool pattern.

## Running Multi-Agent Systems

### Using the Launcher

The `Launcher` provides an easy way to run and test multi-agent systems:

```rust
use adk_rust::Launcher;

let coordinator = /* your multi-agent setup */;

Launcher::new(Arc::new(coordinator))
    .run()
    .await?;
```

**Run Modes:**

```bash
# Interactive console mode
cargo run --example multi_agent -- chat

# Web server mode with UI
cargo run --example multi_agent -- serve
cargo run --example multi_agent -- serve --port 3000
```

**Features:**
- **Agent indicators**: Shows which agent is responding `[Agent: coordinator]`
- **Transfer visualization**: Displays transfer events `🔄 [Transfer requested to: billing_agent]`
- **Seamless handoffs**: Target agent responds immediately after transfer
- **Conversation history**: Maintains context across agent transfers

### Testing Transfers

To verify your multi-agent system works correctly:

1. **Check agent names** appear in brackets when they respond
2. **Look for transfer indicators** (`🔄`) when agents hand off
3. **Verify immediate responses** from target agents without re-prompting
4. **Test different request types** to ensure proper routing
5. **Check edge cases** like transferring to non-existent agents

### Debugging Transfer Issues

If transfers aren't working:

- **Verify sub-agents are added** via `.sub_agent()` 
- **Check agent descriptions** - the LLM uses these to decide transfers
- **Review instructions** - parent should mention when to transfer
- **Check agent names** - must match exactly in transfer calls
- **Enable logging** to see transfer actions in event stream

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

---

**Previous**: [← Workflow Agents](./workflow-agents.md) | **Next**: [Graph Agents →](./graph-agents.md)
