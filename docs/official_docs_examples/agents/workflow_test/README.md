# Workflow Agents Test

Test examples for the [Workflow Agents](../../docs/official_docs/agents/workflow-agents.md) documentation.

## Prerequisites

Set your API key:
```bash
echo 'GOOGLE_API_KEY=your-key' > .env
```

## Examples

### Sequential Pipeline
Research → Analyze → Summarize in sequence:
```bash
cargo run --bin sequential_pipeline
# Try: "Tell me about Rust programming language"
```

### Parallel Analysis  
Technical + Business + UX analysis simultaneously:
```bash
cargo run --bin parallel_analysis
# Try: "Evaluate a mobile banking app"
```

### Loop Refiner
Critique → Refine → Repeat until quality threshold:
```bash
cargo run --bin loop_refiner
# Try: "Write a tagline for a coffee shop"
```

### LLM Conditional Router (Intelligent Routing)
Uses LLM to classify user intent and route to appropriate agent:
```bash
cargo run --bin conditional_router
```

**Example prompts:**
- "How do I fix a borrow error in Rust?" → Routes to `tech_expert`
- "What's the capital of France?" → Routes to `general_helper`
- "Write me a haiku about the moon" → Routes to `creative_writer`

The LLM analyzes each question and classifies it as:
- `technical` → coding, debugging, architecture
- `general` → facts, knowledge, how-to
- `creative` → writing, stories, brainstorming

## Build All

```bash
cargo build
```

## Run All

```bash
cargo run --bin sequential_pipeline
cargo run --bin parallel_analysis
cargo run --bin loop_refiner
cargo run --bin conditional_router
```
