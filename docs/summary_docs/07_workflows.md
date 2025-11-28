# Workflow Patterns

Learn how to orchestrate multiple agents to build complex, multi-step workflows.

## Overview

Workflow agents allow you to chain, parallelize, loop, and conditionally execute agents. This enables you to build sophisticated agentic systems that break down complex tasks into manageable steps.

## Sequential Workflows

Execute agents in order, where each agent receives the output from the previous agent.

### Basic Sequential

```rust
use adk_agent::{LlmAgentBuilder, SequentialAgent};

let analyzer = LlmAgentBuilder::new("analyzer")
    .instruction("Analyze the user's request and identify key topics")
    .model(model.clone())
    .build()?;

let researcher = LlmAgentBuilder::new("researcher")
    .instruction("Research the identified topics using search")
    .model(model.clone())
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;

let summarizer = LlmAgentBuilder::new("summarizer")
    .instruction("Summarize the research findings concisely")
    .model(model.clone())
    .build()?;

let workflow = SequentialAgent::new(
    "research-pipeline",
    vec![
        Arc::new(analyzer),
        Arc::new(researcher),
        Arc::new(summarizer),
    ],
);
```

**Flow**:
```
User Input → Analyzer → Researcher → Summarizer → Final Output
```

### Code Generation Pipeline

```rust
let designer = LlmAgentBuilder::new("designer")
    .instruction("Design the solution architecture and API")
    .model(model.clone())
    .build()?;

let implementer = LlmAgentBuilder::new("implementer")
    .instruction("Implement the code based on the design")
    .model(model.clone())
    .build()?;

let reviewer = LlmAgentBuilder::new("reviewer")
    .instruction("Review code for bugs, style, and best practices")
    .model(model.clone())
    .build()?;

let code_workflow = SequentialAgent::new(
    "code-gen-pipeline",
    vec![
        Arc::new(designer),
        Arc::new(implementer),
        Arc::new(reviewer),
    ],
);
```

**Use Cases**:
- Multi-stage content generation
- Data processing pipelines
- Research and summarization
- Review and approval workflows

## Parallel Workflows

Run multiple agents concurrently with the same input.

### Multi-Perspective Analysis

```rust
use adk_agent::ParallelAgent;

let technical = LlmAgentBuilder::new("technical-analyst")
    .instruction("Analyze from a technical perspective: feasibility, architecture, performance")
    .model(model.clone())
    .build()?;

let business = LlmAgentBuilder::new("business-analyst")
    .instruction("Analyze from a business perspective: ROI, market fit, revenue")
    .model(model.clone())
    .build()?;

let user_experience = LlmAgentBuilder::new("ux-analyst")
    .instruction("Analyze from a UX perspective: usability, accessibility, design")
    .model(model.clone())
    .build()?;

let analysis = ParallelAgent::new(
    "multi-perspective-analysis",
    vec![
        Arc::new(technical),
        Arc::new(business),
        Arc::new(user_experience),
    ],
);
```

**Output**: All three analyses returned simultaneously.

### Competitive Research

```rust
let competitor_a = LlmAgentBuilder::new("research-a")
    .instruction("Research Competitor A's features and pricing")
    .model(model.clone())
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;

let competitor_b = LlmAgentBuilder::new("research-b")
    .instruction("Research Competitor B's features and pricing")
    .model(model.clone())
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;

let competitor_c = LlmAgentBuilder::new("research-c")
    .instruction("Research Competitor C's features and pricing")
    .model(model.clone())
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;

let competitive_research = ParallelAgent::new(
    "competitive-research",
    vec![
        Arc::new(competitor_a),
        Arc::new(competitor_b),
        Arc::new(competitor_c),
    ],
);
```

**Use Cases**:
- Independent analyses
- Parallel data gathering
- Multiple perspectives on same problem
- A/B testing different approaches

## Loop Workflows

Iterate until a condition is met or max iterations reached.

### Iterative Refinement

```rust
use adk_agent::LoopAgent;
use adk_tool::ExitLoopTool;

let refiner = LlmAgentBuilder::new("refiner")
    .instruction("Improve the content. Call exit_loop when quality is excellent.")
    .model(model.clone())
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;

let loop_workflow = LoopAgent::new(
    "refinement-loop",
    Arc::new(refiner),
    Some(5),  // Max 5 iterations
)?;
```

**Exit conditions**:
1. Agent calls `exit_loop` tool
2. Max iterations reached
3. Agent returns empty response

### Progressive Enhancement

```rust
let enhancer = LlmAgentBuilder::new("enhancer")
    .instruction(
        "Add more detail and examples to the content. \
         Call exit_loop if content exceeds 1000 words."
    )
    .model(model.clone())
    .tool(Arc::new(ExitLoopTool::new()))
    .build()?;

let enhancement_loop = LoopAgent::new(
    "enhancement",
    Arc::new(enhancer),
    Some(10),
)?;
```

**Use Cases**:
- Iterative improvement
- Gradual expansion
- Search/optimization loops
- Retry logic with refinement

## Conditional Workflows

Branch based on runtime conditions.

### Content Router

```rust
use adk_agent::ConditionalAgent;

let code_agent = LlmAgentBuilder::new("code-expert")
    .instruction("Expert coding assistant")
    .model(model.clone())
    .build()?;

let general_agent = LlmAgentBuilder::new("general-assistant")
    .instruction("General purpose assistant")
    .model(model.clone())
    .build()?;

let router = ConditionalAgent::new(
    "smart-router",
    |ctx| async move {
        // Route based on input content
        let input = ctx.get_input_text();
        Ok(input.contains("code") || input.contains("programming"))
    },
    Arc::new(code_agent),      // If true
    Arc::new(general_agent),   // If false
)?;
```

### Feature Flag Agent

```rust
let experimental = LlmAgentBuilder::new("experimental")
    .instruction("Using experimental features")
    .model(model.clone())
    .build()?;

let stable = LlmAgentBuilder::new("stable")
    .instruction("Using stable features only")
    .model(model.clone())
    .build()?;

let feature_flag = ConditionalAgent::new(
    "feature-gated",
    |ctx| async move {
        // Check feature flag in session state
        let session = ctx.get_session().await?;
        let state = session.state();
        Ok(state.get("use_experimental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    },
    Arc::new(experimental),
    Arc::new(stable),
)?;
```

**Use Cases**:
- Input-based routing
- Feature flags
- A/B testing
- Fallback logic

## Nested Workflows

Combine workflow patterns for complex orchestrations.

### Sequential of Parallel

```rust
// Stage 1: Parallel research
let parallel_research = ParallelAgent::new(
    "research",
    vec![researcher_a, researcher_b, researcher_c],
);

// Stage 2: Synthesize findings
let synthesizer = LlmAgentBuilder::new("synthesizer")
    .instruction("Combine and synthesize all research findings")
    .model(model.clone())
    .build()?;

// Stage 3: Generate recommendations
let recommender = LlmAgentBuilder::new("recommender")
    .instruction("Generate actionable recommendations")
    .model(model.clone())
    .build()?;

let workflow = SequentialAgent::new(
    "research-and-recommend",
    vec![
        Arc::new(parallel_research),
        Arc::new(synthesizer),
        Arc::new(recommender),
    ],
);
```

**Flow**:
```
Input → [Research A, Research B, Research C] → Synthesize → Recommend → Output
           (parallel)                            (sequential)
```

### Loop with Sequential

```rust
// Inner sequential workflow
let inner_workflow = SequentialAgent::new(
    "draft-and-review",
    vec![
        Arc::new(drafter),
        Arc::new(reviewer),
    ],
);

// Outer loop
let iterative_workflow = LoopAgent::new(
    "iterative-drafting",
    Arc::new(inner_workflow),
    Some(3),
)?;
```

**Flow**:
```
Loop (max 3 iterations):
  Draft → Review → (back to Draft if not done)
```

### Complex Decision Tree

```rust
// Left branch: Code-related
let code_branch = SequentialAgent::new(
    "code-pipeline",
    vec![
        Arc::new(code_analyzer),
        Arc::new(code_generator),
        Arc::new(code_reviewer),
    ],
);

// Right branch: Content-related
let content_branch = SequentialAgent::new(
    "content-pipeline",
    vec![
        Arc::new(content_planner),
        Arc::new(content_writer),
        Arc::new(content_editor),
    ],
);

// Router
let router = ConditionalAgent::new(
    "content-router",
    |ctx| async move {
        let input = ctx.get_input_text();
        Ok(input.contains("code"))
    },
    Arc::new(code_branch),
    Arc::new(content_branch),
)?;
```

## Practical Examples

### Content Generation Workflow

```rust
async fn build_content_workflow(
    model: Arc<GeminiModel>
) -> Result<Arc<dyn Agent>> {
    // 1. Outline creator
    let outliner = LlmAgentBuilder::new("outliner")
        .instruction("Create a detailed outline for the topic")
        .model(model.clone())
        .build()?;
    
    // 2. Section writers (parallel)
    let intro_writer = LlmAgentBuilder::new("intro-writer")
        .instruction("Write introduction section")
        .model(model.clone())
        .build()?;
    
    let body_writer = LlmAgentBuilder::new("body-writer")
        .instruction("Write main body sections")
        .model(model.clone())
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;
    
    let conclusion_writer = LlmAgentBuilder::new("conclusion-writer")
        .instruction("Write conclusion section")
        .model(model.clone())
        .build()?;
    
    let parallel_writers = ParallelAgent::new(
        "writers",
        vec![
            Arc::new(intro_writer),
            Arc::new(body_writer),
            Arc::new(conclusion_writer),
        ],
    );
    
    // 3. Assembler
    let assembler = LlmAgentBuilder::new("assembler")
        .instruction("Assemble sections into coherent document")
        .model(model.clone())
        .build()?;
    
    // 4. Editor (loop)
    let editor = LlmAgentBuilder::new("editor")
        .instruction("Edit for clarity, grammar, and flow. Exit when perfect.")
        .model(model.clone())
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;
    
    let editing_loop = LoopAgent::new("editing", Arc::new(editor), Some(3))?;
    
    // Combine into workflow
    let workflow = SequentialAgent::new(
        "content-generation",
        vec![
            Arc::new(outliner),
            Arc::new(parallel_writers),
            Arc::new(assembler),
            Arc::new(editing_loop),
        ],
    );
    
    Ok(Arc::new(workflow))
}
```

### Customer Support Workflow

```rust
async fn build_support_workflow(
    model: Arc<GeminiModel>
) -> Result<Arc<dyn Agent>> {
    // Classifier
    let classifier = LlmAgentBuilder::new("classifier")
        .instruction("Classify support request: technical, billing, or general")
        .model(model.clone())
        .build()?;
    
    // Specialized agents
    let technical = LlmAgentBuilder::new("technical-support")
        .instruction("Provide technical support")
        .model(model.clone())
        .tool(Arc::new(search_kb_tool))
        .build()?;
    
    let billing = LlmAgentBuilder::new("billing-support")
        .instruction("Handle billing inquiries")
        .model(model.clone())
        .tool(Arc::new(billing_system_tool))
        .build()?;
    
    let general = LlmAgentBuilder::new("general-support")
        .instruction("Handle general questions")
        .model(model.clone())
        .build()?;
    
    // Custom router based on classification
    let router = CustomAgentBuilder::new("router")
        .handler(|ctx| async move {
            // Get classification from previous agent
            let session = ctx.get_session().await?;
            let category = session.state()
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");
            
            // Route to appropriate agent
            let agent: Arc<dyn Agent> = match category {
                "technical" => technical.clone(),
                "billing" => billing.clone(),
                _ => general.clone(),
            };
            
            agent.run(ctx).await
        })
        .build()?;
    
    let workflow = SequentialAgent::new(
        "support-pipeline",
        vec![
            Arc::new(classifier),
            Arc::new(router),
        ],
    );
    
    Ok(Arc::new(workflow))
}
```

## Best Practices

### 1. Keep Workflows Focused

Each agent should have a single, clear responsibility.

✅ **Good**:
```rust
let analyzer = LlmAgentBuilder::new("analyzer")
    .instruction("Extract key topics from user input")
    .build()?;
```

❌ **Bad**:
```rust
let do_everything = LlmAgentBuilder::new("agent")
    .instruction("Analyze, research, summarize, and format the response")
    .build()?;
```

### 2. Use Descriptive Names

```rust
// Good names clearly indicate purpose
let "sentiment-analyzer"
let "fact-checker"
let "content-summarizer"
```

### 3. Limit Sequential Depth

Too many sequential steps can be slow and fragile.

✅ **Good**: 3-5 sequential steps  
⚠️ **Caution**: 6-10 steps  
❌ **Bad**: 10+ steps

### 4. Set Reasonable Loop Limits

```rust
LoopAgent::new("refiner", agent, Some(5))?;  // Good
LoopAgent::new("refiner", agent, Some(100))?;  // Too high
LoopAgent::new("refiner", agent, None)?;  // Risky (no limit)
```

### 5. Handle Errors Gracefully

```rust
let mut events = workflow.run(ctx).await?;

while let Some(event) = events.next().await {
    match event {
        Ok(evt) => { /* process */ },
        Err(e) => {
            eprintln!("Agent error: {}", e);
            // Decide: continue, retry, or abort
        }
    }
}
```

### 6. Use State for Communication

Agents can share data via session state:

```rust
// Agent 1: Store data
session.state().set("findings".to_string(), json!({
    "key": "value"
}));

// Agent 2: Read data
let findings = session.state().get("findings");
```

## Performance Considerations

### Parallel vs Sequential

- **Parallel**: Faster but uses more resources
- **Sequential**: Slower but more controlled

Choose based on:
- Independence of tasks
- Resource constraints
- Latency requirements

### Loop Efficiency

Loops can be expensive. Optimize by:
- Setting low max iterations
- Providing clear exit criteria
- Caching intermediate results

### Streaming

All workflows support streaming for real-time updates:

```rust
let mut events = workflow.run(ctx).await?;

while let Some(event) = events.next().await {
    // Update UI immediately
    println!("Agent {}: {:?}", event?.agent_name, event?.content);
}
```

## Next Steps

- **[MCP Integration →](06_mcp.md)**: Extend agents with MCP tools
- **[Deployment →](08_deployment.md)**: Deploy workflows to production
- **[Examples →](../examples/)**: See working workflow examples

---

**Previous**: [API Reference](05_api_reference.md) | **Next**: [MCP Integration](06_mcp.md)
