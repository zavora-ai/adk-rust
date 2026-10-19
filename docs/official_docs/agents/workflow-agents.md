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
adk-rust = { version = "3.0.0", features = ["tools"] }
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
```

The `tools` feature provides `ExitLoopTool` for the `LoopAgent` example; the other examples
build with the default features.

Create `.env`:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

---

## SequentialAgent

`SequentialAgent` runs sub-agents one after another. Each agent sees the accumulated conversation history from previous agents.

Each agent's request carries only its own turns as model turns. What an earlier agent said,
called and got back reaches a later agent as user-role context: one message per event, opening
with a preamble and quoting each payload between `<<<BEGIN_QUOTED_AGENT_CONTENT>>>` and
`<<<END_QUOTED_AGENT_CONTENT>>>` markers (`[researcher] said: ...`,
`[researcher] called tool ...`, `[researcher] ... tool returned result: ...`). The quoted
payloads are data, not instructions, and other agents' thinking is left out. Every request
therefore ends on a user turn, which models without assistant prefill support (Anthropic's
current models) require. `LoopAgent` follows the same rule; a round that follows the agent's
own answer gets a short user turn to answer.

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
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

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

The launcher prints each sub-agent's answer in turn, without a label (abridged):

```
You > Tell me about the Rust programming language. Keep every answer under 50 words.
* **Memory Safety:** Rust guarantees memory safety without using a garbage collector, ...
* **Performance:** It offers high performance comparable to C and C++, ...
* **Concurrency:** Rust enables fearless concurrency by preventing data races at compile time.
**Key Insights:**
1. **Compile-time safety:** Rust eliminates entire classes of bugs ...
**Executive Summary**

Rust redefines systems programming by eliminating memory errors and data races at compile time. ...
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
3. Analyzer sees: user message + researcher response, as context
4. Summarizer sees: user message + researcher + analyzer responses, as context
5. Pipeline completes when last agent finishes

---

## ParallelAgent

`ParallelAgent` runs all sub-agents concurrently. Each agent receives the same input and works independently.

Independence is enforced, not just intended: each sub-agent runs on its own
conversation branch (`{parent}.{parallel_agent}.{sub_agent}`), and history reads
are scoped to that branch. A sub-agent sees the conversation that led to the
fan-out but not what its siblings produced, so concurrent branches cannot
contaminate each other's context. Use `ParallelAgent::with_shared_state()` when
you *want* sub-agents to coordinate.

Branches are polled together, so wall-clock cost is roughly the slowest branch
rather than the sum of all of them, and a slow branch does not hold up the
others. Two consequences worth designing around:

- **Events interleave.** Events arrive in the order branches produce them, not
  grouped per sub-agent. Use `event.author` (or `event.branch`) to attribute a
  chunk to its branch. Ordering across branches is not guaranteed.
- **One error ends the run, chosen deterministically.** Every branch still runs
  to completion, and a single terminal error is surfaced afterwards. When more
  than one branch fails, the error reported is the one from the earliest
  sub-agent in declaration order — not whichever failed first in wall-clock
  terms, which concurrency would make a race.

- **An escalation ends the fan-out.** A branch whose event escalates
  (`EventActions::escalate`, as `ExitLoopTool` sets) stops every other branch
  where it is, and the escalation reaches the enclosing workflow, so a
  `LoopAgent` around the `ParallelAgent` ends too.

Dropping the event stream early tears down branches that are still in flight.

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
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

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

The three analysts answer at the same time, so the console shows their streamed chunks
interleaved as they arrive. To present each answer separately, run the agent with a `Runner`
and group the events by `event.author` (see
[Observing Workflow Execution](#observing-workflow-execution)).

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

All agents start simultaneously and their events stream as they are produced.

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
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

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

Each pass prints the critic's review and the refiner's answer. When the refiner calls
`exit_loop`, the launcher prints the call and the loop ends (abridged):

```
You > Write a five-word tagline for a coffee shop.
**Tagline:**
Fuel your daily morning grind.
...
* **Score:** 6/10
...
[tool] exit_loop({})
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
use adk_rust::prelude::*;
use std::sync::Arc;

fn router(premium_agent: Arc<dyn Agent>, basic_agent: Arc<dyn Agent>) -> ConditionalAgent {
    ConditionalAgent::new(
        "router",
        |ctx| {
            ctx.session()
                .state()
                .get("premium")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        },
        premium_agent,
    )
    .with_else(basic_agent)
}
```

The condition receives the `InvocationContext` and runs once per invocation. Without
`with_else`, a false condition runs no agent.

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
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

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

The router emits a `[Routing to: <label>]` event before the chosen agent answers:

```
You > Write me a haiku about the moon
[Routing to: creative]Silver pearl adrift,
Glowing in the velvet night,
Tears of ancient skies.
```

The label is the model's reply, trimmed and lowercased. An exact match selects its route;
otherwise the longest route label that the reply contains is used, and then `default_route`.

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

## Held Tool Calls

A workflow agent stops when one of its sub-agents holds a tool call for confirmation (see
[Tool Authorization](../security/tool-authorization.md#resuming-a-held-call)), and the next
run resumes the paused turn at that sub-agent instead of starting over.

| Workflow agent | The run that holds the call | The next run |
|----------------|-----------------------------|--------------|
| `SequentialAgent` | Later sub-agents do not run | Starts at the sub-agent holding the call; earlier sub-agents do not run again |
| `LoopAgent` | Later sub-agents and iterations do not run | Starts at the holding sub-agent in the iteration it held in, then runs the remaining iterations, so `max_iterations` counts across the hold |
| `ParallelAgent` | Every branch runs to its end, then the workflow stops | Runs only the sub-agents holding calls; siblings that finished do not run again |
| `ConditionalAgent` | The chosen branch stops | Runs the branch holding the call without evaluating the condition again |
| `LlmConditionalAgent` | The chosen route stops | Runs the route holding the call without classifying the input again |

- **Positions.** Each workflow agent records its position on the confirmation event as the
  event passes through it (`EventActions::workflow_positions`, one `WorkflowPosition` per
  workflow name) and reads it back with `Session::workflow_resume_positions`. Nested workflows
  each resume at their own position.
- **Callbacks.** A paused workflow's after-agent callbacks run in the run that finishes it, not
  in the run that paused it.
- **One response per call.** A later sub-agent sees the resumed call's result in place of its
  confirmation-pending response.
- **A turn that moves on.** A paused turn resumes once. When the holding sub-agent runs without
  a decision — its model answers past the pending response — the workflow continues to its
  later sub-agents, and the next run starts over. The call stays held, and resumes the next
  time its agent runs with a decision.

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

fn pipeline(model: Arc<dyn Llm>, transfer: Arc<dyn Tool>) -> Result<SequentialAgent> {
    let intake = LlmAgentBuilder::new("intake").model(model.clone()).build()?;
    let ops = LlmAgentBuilder::new("ops")
        .model(model.clone())
        .tool(transfer)
        .require_tool_confirmation("transfer")
        .build()?;
    let report = LlmAgentBuilder::new("report").model(model).build()?;
    // Run 1: intake runs, ops holds `transfer`, report does not run.
    // Run 2, with the decision: ops resumes the call, then report runs; intake does not.
    Ok(SequentialAgent::new(
        "pipeline",
        vec![Arc::new(intake), Arc::new(ops), Arc::new(report)],
    ))
}
```

---

## Observing Workflow Execution

To see which agent produced each event, run the workflow with a `Runner` and read
`event.author`:

```rust
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::{EventTextDeltas, SessionId, UserId};
use std::sync::Arc;

async fn trace(pipeline: SequentialAgent) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("workflow_trace")
        .agent(Arc::new(pipeline))
        .session_service(session_service.clone())
        .build()?;

    let session = session_service
        .create(CreateRequest {
            app_name: "workflow_trace".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session.id())?,
            Content::new("user").with_text("Analyze Rust"),
        )
        .await?;

    // Streamed deltas end with a snapshot of the whole answer; EventTextDeltas
    // reduces that snapshot to the text not yet printed.
    let mut text_deltas = EventTextDeltas::default();
    let mut current_author = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if event.author != current_author {
            current_author = event.author.clone();
            println!("\n[{current_author}]");
        }
        let event = text_deltas.push(&event);
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!();
    Ok(())
}
```

Events from a `ParallelAgent` interleave, so a consumer that renders each branch separately
keys its buffers by `event.author` or `event.branch`.

---

## API Reference

In these signatures, `before` and `after` are `BeforeAgentCallback` and `AfterAgentCallback`
values (see [Callbacks](../callbacks/callbacks.md)), the agents are `Arc<dyn Agent>`, and
`is_premium` is a `fn(&dyn InvocationContext) -> bool`.

### SequentialAgent

```rust
SequentialAgent::new("name", vec![agent1, agent2, agent3])
    .with_description("Optional description")
    .before_callback(before)  // Runs before the sub-agents
    .after_callback(after)    // Runs after the last sub-agent
```

### ParallelAgent

```rust
ParallelAgent::new("name", vec![agent1, agent2, agent3])
    .with_description("Optional description")
    .before_callback(before)
    .after_callback(after)
```

If a sub-agent fails, the other branches run to their end, and then `ParallelAgent` returns the error of the earliest-declared sub-agent that failed. `with_shared_state()` gives the branches a `SharedState` for coordination.

### LoopAgent

```rust
LoopAgent::new("name", vec![agent1, agent2])
    .with_max_iterations(5)     // Safety limit (recommended, default: 1000)
    .with_description("Optional description")
    .before_callback(before)
    .after_callback(after)
```

### ConditionalAgent

```rust
ConditionalAgent::new("name", |ctx| is_premium(ctx), if_agent)
    .with_else(else_agent)      // Optional else branch
    .with_description("Optional description")
    .before_callback(before)
    .after_callback(after)
```

### LlmConditionalAgent

```rust
LlmConditionalAgent::builder("name", model)
    .instruction("Classification prompt")
    .route("label", agent)
    .default_route(fallback_agent) // Optional
    .description("Optional description")
    .before_callback(before)
    .after_callback(after)
    .build()?
```

### ExitLoopTool

```rust
// Add to an agent to let it exit a LoopAgent
.tool(Arc::new(ExitLoopTool::new()))
```

---

**Previous**: [LlmAgent](./llm-agent.md) | **Next**: [Multi-Agent Systems →](./multi-agent.md)
