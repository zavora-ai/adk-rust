# Workflow Agents

Workflow agents orchestrate multiple agents in predictable patterns—sequential pipelines, parallel execution, or iterative loops. Unlike LlmAgent which uses AI reasoning, workflow agents follow deterministic execution paths.

## Quick Start

Create a new project:

```bash
cargo new workflow_demo
cd workflow_demo
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-rust = "0.4"
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
```

Create `.env`:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

---

## SequentialAgent

`SequentialAgent` runs sub-agents one after another. Each agent sees the accumulated conversation history from previous agents.

### When to Use

- Multi-step pipelines where output feeds into next step
- Research → Analysis → Summary workflows
- Data transformation chains

### Complete Example

Replace `src/main.rs`:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Step 1: Research agent gathers information
    let researcher = LlmAgentBuilder::new("researcher")
        .instruction("Research the given topic. List 3-5 key facts or points. \
                     Be factual and concise.")
        .model(model.clone())
        .output_key("research")  // Saves output to state
        .build()?;

    // Step 2: Analyzer agent identifies patterns
    let analyzer = LlmAgentBuilder::new("analyzer")
        .instruction("Based on the research above, identify 2-3 key insights \
                     or patterns. What's the bigger picture?")
        .model(model.clone())
        .output_key("analysis")
        .build()?;

    // Step 3: Summarizer creates final output
    let summarizer = LlmAgentBuilder::new("summarizer")
        .instruction("Create a brief executive summary combining the research \
                     and analysis. Keep it under 100 words.")
        .model(model.clone())
        .build()?;

    // Create the sequential pipeline
    let pipeline = SequentialAgent::new(
        "research_pipeline",
        vec![Arc::new(researcher), Arc::new(analyzer), Arc::new(summarizer)],
    ).with_description("Research → Analyze → Summarize");

    println!("📋 Sequential Pipeline: Research → Analyze → Summarize");
    println!();

    Launcher::new(Arc::new(pipeline)).run().await?;
    Ok(())
}
```

Run it:

```bash
cargo run
```

### Example Interaction

```
You: Tell me about Rust programming language

🔄 [researcher] Researching...
Here are key facts about Rust:
1. Systems programming language created at Mozilla in 2010
2. Memory safety without garbage collection via ownership system
3. Zero-cost abstractions and minimal runtime
4. Voted "most loved language" on Stack Overflow for 7 years
5. Used by Firefox, Discord, Dropbox, and Linux kernel

🔄 [analyzer] Analyzing...
Key insights:
1. Rust solves the memory safety vs performance tradeoff
2. Strong developer satisfaction drives rapid adoption
3. Trust from major tech companies validates production-readiness

🔄 [summarizer] Summarizing...
Rust is a systems language that achieves memory safety without garbage 
collection through its ownership model. Created at Mozilla in 2010, it's 
been rated the most loved language for 7 consecutive years. Major companies 
like Discord and Linux kernel adopt it for its zero-cost abstractions 
and performance guarantees.
```

### How It Works

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Researcher │ →  │   Analyzer  │ →  │  Summarizer │
│   (step 1)  │    │   (step 2)  │    │   (step 3)  │
└─────────────┘    └─────────────┘    └─────────────┘
       ↓                  ↓                  ↓
 "Key facts..."    "Insights..."    "Executive summary"
```

1. User message goes to first agent (Researcher)
2. Researcher's response is added to history
3. Analyzer sees: user message + researcher response
4. Summarizer sees: user message + researcher + analyzer responses
5. Pipeline completes when last agent finishes

---

## ParallelAgent

`ParallelAgent` runs all sub-agents concurrently. Each agent receives the same input and works independently.

### When to Use

- Multiple perspectives on the same topic
- Fan-out processing (same input, different analyses)
- Speed-critical multi-task scenarios

### Complete Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Three analysts with DISTINCT personas (important for parallel execution)
    let technical = LlmAgentBuilder::new("technical_analyst")
        .instruction("You are a senior software architect. \
                     FOCUS ONLY ON: code quality, system architecture, scalability, \
                     security vulnerabilities, and tech stack choices. \
                     Start your response with '🔧 TECHNICAL:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    let business = LlmAgentBuilder::new("business_analyst")
        .instruction("You are a business strategist and MBA graduate. \
                     FOCUS ONLY ON: market opportunity, revenue model, competition, \
                     cost structure, and go-to-market strategy. \
                     Start your response with '💼 BUSINESS:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    let user_exp = LlmAgentBuilder::new("ux_analyst")
        .instruction("You are a UX researcher and designer. \
                     FOCUS ONLY ON: user journey, accessibility, pain points, \
                     visual design, and user satisfaction metrics. \
                     Start your response with '🎨 UX:' and give 2-3 bullet points.")
        .model(model.clone())
        .build()?;

    // Create parallel agent
    let multi_analyst = ParallelAgent::new(
        "multi_perspective",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user_exp)],
    ).with_description("Technical + Business + UX analysis in parallel");

    println!("⚡ Parallel Analysis: Technical | Business | UX");
    println!("   (All three run simultaneously!)");
    println!();

    Launcher::new(Arc::new(multi_analyst)).run().await?;
    Ok(())
}
```

> **💡 Tip**: Make parallel agent instructions highly distinct with unique personas, focus areas, and response prefixes. This ensures each agent produces unique output.

### Example Interaction

```
You: Evaluate a mobile banking app

🔧 TECHNICAL:
• Requires robust API security: OAuth 2.0, certificate pinning, encrypted storage
• Offline mode with sync requires complex state management and conflict resolution
• Biometric auth integration varies significantly across iOS/Android platforms

💼 BUSINESS:
• Highly competitive market - need unique differentiator (neobanks, traditional banks)
• Revenue model: interchange fees, premium tiers, or lending products cross-sell
• Regulatory compliance costs significant: PCI-DSS, regional banking laws, KYC/AML

🎨 UX:
• Critical: fast task completion - check balance must be < 3 seconds
• Accessibility essential: screen reader support, high contrast mode, large touch targets
• Trust indicators important: security badges, familiar banking patterns
```

### How It Works

```
                    ┌─────────────────┐
                    │  User Message   │
                    └────────┬────────┘
         ┌───────────────────┼───────────────────┐
         ↓                   ↓                   ↓
  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
  │  Technical  │    │  Business   │    │     UX      │
  │   Analyst   │    │   Analyst   │    │   Analyst   │
  └──────┬──────┘    └──────┬──────┘    └──────┬──────┘
         ↓                   ↓                   ↓
    (response 1)       (response 2)       (response 3)
```

All agents start simultaneously and results stream as they complete.

---

## LoopAgent

`LoopAgent` runs sub-agents repeatedly until an exit condition is met or max iterations reached.

### When to Use

- Iterative refinement (draft → critique → improve → repeat)
- Retry logic with improvement
- Quality gates that require multiple passes

### ExitLoopTool

To exit a loop early, give an agent the `ExitLoopTool`. When called, it signals the loop to stop.

### Complete Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Critic agent evaluates content
    let critic = LlmAgentBuilder::new("critic")
        .instruction("Review the content for quality. Score it 1-10 and list \
                     specific improvements needed. Be constructive but critical.")
        .model(model.clone())
        .build()?;

    // Refiner agent improves based on critique
    let refiner = LlmAgentBuilder::new("refiner")
        .instruction("Apply the critique to improve the content. \
                     If the score is 8 or higher, call exit_loop to finish. \
                     Otherwise, provide an improved version.")
        .model(model.clone())
        .tool(Arc::new(ExitLoopTool::new()))  // Can exit the loop
        .build()?;

    // Create inner sequential: critic → refiner
    let critique_refine = SequentialAgent::new(
        "critique_refine_step",
        vec![Arc::new(critic), Arc::new(refiner)],
    );

    // Wrap in loop with max 3 iterations
    let iterative_improver = LoopAgent::new(
        "iterative_improver",
        vec![Arc::new(critique_refine)],
    ).with_max_iterations(3)
     .with_description("Critique-refine loop (max 3 passes)");

    println!("🔄 Iterative Improvement Loop");
    println!("   critic → refiner → repeat (max 3x or until quality >= 8)");
    println!();

    Launcher::new(Arc::new(iterative_improver)).run().await?;
    Ok(())
}
```

### Example Interaction

```
You: Write a tagline for a coffee shop

🔄 Iteration 1
[critic] Score: 5/10. "Good coffee here" is too generic. Needs:
- Unique value proposition
- Emotional connection
- Memorable phrasing

[refiner] Improved: "Where every cup tells a story"

🔄 Iteration 2
[critic] Score: 7/10. Better! But could be stronger:
- More action-oriented
- Hint at the experience

[refiner] Improved: "Brew your perfect moment"

🔄 Iteration 3
[critic] Score: 8/10. Strong, action-oriented, experiential.
Minor: could be more distinctive.

[refiner] Score is 8+, quality threshold met!
[exit_loop called]

Final: "Brew your perfect moment"
```

### How It Works

```
     ┌──────────────────────────────────────────┐
     │              LoopAgent                    │
     │  ┌────────────────────────────────────┐  │
     │  │        SequentialAgent              │  │
     │  │  ┌──────────┐    ┌──────────────┐  │  │
  →  │  │  │  Critic  │ →  │   Refiner    │  │  │  →
     │  │  │ (review) │    │ (improve or  │  │  │
     │  │  └──────────┘    │  exit_loop)  │  │  │
     │  │                  └──────────────┘  │  │
     │  └────────────────────────────────────┘  │
     │         ↑_____________↓                  │
     │         repeat until exit                │
     └──────────────────────────────────────────┘
```

---

## ConditionalAgent (Rule-Based)

`ConditionalAgent` branches execution based on a **synchronous, rule-based** condition. Use this for deterministic routing like A/B testing or environment-based routing.

```rust
ConditionalAgent::new("router", |ctx| ctx.session().state().get("premium")..., premium_agent)
    .with_else(basic_agent)
```

> **Note:** For LLM-based intelligent routing, use `LlmConditionalAgent` instead.

---

## LlmConditionalAgent (LLM-Based)

`LlmConditionalAgent` uses an **LLM to classify** user input and route to the appropriate sub-agent. This is ideal for intelligent routing where the routing decision requires understanding the content.

### When to Use

- **Intent classification** - Route based on what the user is asking
- **Multi-way routing** - More than 2 destinations
- **Context-aware routing** - Needs understanding, not just keywords

### Complete Example

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create specialist agents
    let tech_agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("tech_expert")
            .instruction("You are a senior software engineer. Be precise and technical.")
            .model(model.clone())
            .build()?
    );

    let general_agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("general_helper")
            .instruction("You are a friendly assistant. Explain simply, use analogies.")
            .model(model.clone())
            .build()?
    );

    let creative_agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("creative_writer")
            .instruction("You are a creative writer. Be imaginative and expressive.")
            .model(model.clone())
            .build()?
    );

    // LLM classifies the query and routes accordingly
    let router = LlmConditionalAgent::builder("smart_router", model.clone())
        .instruction("Classify the user's question as exactly ONE of: \
                     'technical' (coding, debugging, architecture), \
                     'general' (facts, knowledge, how-to), \
                     'creative' (writing, stories, brainstorming). \
                     Respond with ONLY the category name.")
        .route("technical", tech_agent)
        .route("general", general_agent.clone())
        .route("creative", creative_agent)
        .default_route(general_agent)
        .build()?;

    println!("🧠 LLM-Powered Intelligent Router");
    Launcher::new(Arc::new(router)).run().await?;
    Ok(())
}
```

### Example Interaction

```
You: How do I fix a borrow error in Rust?
[Routing to: technical]
[Agent: tech_expert]
A borrow error occurs when Rust's ownership rules are violated...

You: What's the capital of France?
[Routing to: general]
[Agent: general_helper]
The capital of France is Paris! It's a beautiful city...

You: Write me a haiku about the moon
[Routing to: creative]
[Agent: creative_writer]
Silver orb above,
Shadows dance on silent waves—
Night whispers secrets.
```

### How It Works

```
┌─────────────────┐
│  User Message   │
└────────┬────────┘
         ↓
┌─────────────────┐
│   LLM Classifies│  "technical" / "general" / "creative"
│   (smart_router)│
└────────┬────────┘
         ↓
    ┌────┴────┬──────────┐
    ↓         ↓          ↓
┌───────┐ ┌───────┐ ┌─────────┐
│ tech  │ │general│ │creative │
│expert │ │helper │ │ writer  │
└───────┘ └───────┘ └─────────┘
```


---

## Combining Workflow Agents

Workflow agents can be nested for complex patterns.

### Sequential + Parallel + Loop

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// 1. Parallel analysis from multiple perspectives
let parallel_analysis = ParallelAgent::new(
    "multi_analysis",
    vec![Arc::new(tech_analyst), Arc::new(biz_analyst)],
);

// 2. Synthesize the parallel results
let synthesizer = LlmAgentBuilder::new("synthesizer")
    .instruction("Combine all analyses into a unified recommendation.")
    .model(model.clone())
    .build()?;

// 3. Quality loop: critique and refine
let quality_loop = LoopAgent::new(
    "quality_check",
    vec![Arc::new(critic), Arc::new(refiner)],
).with_max_iterations(2);

// Final pipeline: parallel → synthesize → quality loop
let full_pipeline = SequentialAgent::new(
    "full_analysis_pipeline",
    vec![
        Arc::new(parallel_analysis),
        Arc::new(synthesizer),
        Arc::new(quality_loop),
    ],
);
```

---

## Tracing Workflow Execution

To see what's happening inside a workflow, enable tracing:

```rust
use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::futures::StreamExt;
use std::sync::Arc;

// Create pipeline as before...

// Use Runner instead of Launcher for detailed control
let session_service = Arc::new(InMemorySessionService::new());
let runner = Runner::new(RunnerConfig {
    app_name: "workflow_trace".to_string(),
    agent: Arc::new(pipeline),
    session_service: session_service.clone(),
    artifact_service: None,
    memory_service: None,
    run_config: None,
})?;

let session = session_service.create(CreateRequest {
    app_name: "workflow_trace".to_string(),
    user_id: "user".to_string(),
    session_id: None,
    state: Default::default(),
}).await?;

let mut stream = runner.run(
    "user".to_string(),
    session.id().to_string(), 
    Content::new("user").with_text("Analyze Rust"),
).await?;

// Process each event to see workflow execution
while let Some(event) = stream.next().await {
    let event = event?;
    
    // Show which agent is responding
    println!("📍 Agent: {}", event.author);
    
    // Show the response content
    if let Some(content) = event.content() {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("   {}", text);
            }
        }
    }
    println!();
}
```

---

## API Reference

### SequentialAgent

```rust
SequentialAgent::new("name", vec![agent1, agent2, agent3])
    .with_description("Optional description")
    .before_callback(callback)  // Called before execution
    .after_callback(callback)   // Called after execution
```

### ParallelAgent

```rust
ParallelAgent::new("name", vec![agent1, agent2, agent3])
    .with_description("Optional description")
    .before_callback(callback)
    .after_callback(callback)
```

If any sub-agent fails, `ParallelAgent` drains all remaining futures before propagating the first error, preventing resource leaks.

### LoopAgent

```rust
LoopAgent::new("name", vec![agent1, agent2])
    .with_max_iterations(5)     // Safety limit (recommended, default: 1000)
    .with_description("Optional description")
    .before_callback(callback)
    .after_callback(callback)
```

### ConditionalAgent

```rust
ConditionalAgent::new("name", |ctx| condition_fn, if_agent)
    .with_else(else_agent)      // Optional else branch
    .with_description("Optional description")
```

### ExitLoopTool

```rust
// Add to an agent to let it exit a LoopAgent
.tool(Arc::new(ExitLoopTool::new()))
```

---

**Previous**: [LlmAgent](./llm-agent.md) | **Next**: [Multi-Agent Systems →](./multi-agent.md)
