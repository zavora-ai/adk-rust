# State Management in ADK-Rust

## Overview

State management enables agents to coordinate by saving their outputs to session state, which can be accessed by subsequent agents or tools. This is implemented via the `output_key` feature in `LlmAgent`.

## How It Works

### 1. OutputKey Field

When you set an `output_key` on an `LlmAgent`, the agent automatically saves its text output to the `state_delta` field of the event:

```rust
let agent = LlmAgentBuilder::new("summarizer")
    .model(Arc::new(model))
    .instruction("Summarize the text")
    .output_key("summary")  // ← Saves output to state
    .build()?;
```

### 2. State Delta

The `state_delta` is a `HashMap<String, serde_json::Value>` in `EventActions`:

```rust
pub struct EventActions {
    pub state_delta: HashMap<String, serde_json::Value>,
    pub artifact_delta: HashMap<String, i64>,
    pub escalate: bool,
}
```

### 3. Automatic Extraction

When an agent with `output_key` produces a response:
1. All text parts are concatenated
2. The result is stored in `event.actions.state_delta[output_key]`
3. Downstream agents/tools can access this state

## Use Cases

### 1. Single Agent Output Capture

Extract agent output for later use:

```rust
let agent = LlmAgentBuilder::new("analyzer")
    .model(Arc::new(model))
    .instruction("Analyze sentiment")
    .output_key("sentiment")
    .build()?;

// Run agent
let mut stream = agent.run(ctx).await?;
while let Some(result) = stream.next().await {
    let event = result?;
    if let Some(sentiment) = event.actions.state_delta.get("sentiment") {
        println!("Sentiment: {}", sentiment);
    }
}
```

### 2. Agent Coordination

Connect agents to work together:

```rust
let analyzer = LlmAgentBuilder::new("analyzer")
    .model(Arc::new(model1))
    .instruction("Analyze sentiment. Reply: positive, negative, or neutral")
    .output_key("sentiment")
    .build()?;

let responder = LlmAgentBuilder::new("responder")
    .model(Arc::new(model2))
    .instruction("Generate response. Sentiment was: {sentiment}")
    .output_key("response")
    .build()?;

let pipeline = SequentialAgent::new(
    "sentiment_pipeline",
    vec![Arc::new(analyzer), Arc::new(responder)],
);
```

**Note**: Template variable substitution (`{sentiment}`) is not yet implemented. This will be added in Phase 6.

### 3. State Accumulation

Collect state across multiple agents:

```rust
let mut accumulated_state = HashMap::new();

let mut stream = pipeline.run(ctx).await?;
while let Some(result) = stream.next().await {
    let event = result?;
    for (key, value) in &event.actions.state_delta {
        accumulated_state.insert(key.clone(), value.clone());
    }
}

// Now accumulated_state contains all outputs
```

## Demo Output

```
Demo 1: Single Agent with OutputKey
-------------------------------------
Agent: summarizer
Response: Fox jumps dog.

✅ State Delta:
  summary = String("Fox jumps dog.")


Demo 2: Sequential Agents with State Coordination
--------------------------------------------------
Agent: analyzer
Response: positive
State Delta:
  sentiment = String("positive")

Agent: responder
Response: That's fantastic to hear! We're thrilled you love our product!
State Delta:
  response = String("That's fantastic to hear! We're thrilled you love our product!")

✅ Final Accumulated State:
  sentiment = String("positive")
  response = String("That's fantastic to hear! We're thrilled you love our product!")
```

## Implementation Details

### In LlmAgent

```rust
pub struct LlmAgent {
    name: String,
    model: Arc<dyn Llm>,
    output_key: Option<String>,  // ← New field
    // ...
}

// In run() method:
if let Some(ref output_key) = output_key {
    if let Some(ref content) = event.content {
        let mut text_parts = String::new();
        for part in &content.parts {
            if let Part::Text { text } = part {
                text_parts.push_str(text);
            }
        }
        if !text_parts.is_empty() {
            event.actions.state_delta.insert(
                output_key.clone(),
                serde_json::Value::String(text_parts),
            );
        }
    }
}
```

## Comparison with Go Implementation

| Feature | Go ADK | Rust ADK | Status |
|---------|--------|----------|--------|
| OutputKey field | ✅ | ✅ | Complete |
| StateDelta map | ✅ | ✅ | Complete |
| Automatic text extraction | ✅ | ✅ | Complete |
| OutputSchema validation | ✅ | ❌ | Not implemented |
| Template variables | ✅ | ❌ | Planned for Phase 6 |
| Session state persistence | ✅ | ⏳ | Requires Runner |

## Future Enhancements

### Phase 6: Runner & Execution
1. **Template Variable Substitution**: Replace `{var_name}` in instructions with state values
2. **Session State Integration**: Persist state_delta to session storage
3. **State Scoping**: Support app-level, user-level, and temp state

### Phase 7: Advanced Features
1. **OutputSchema**: Validate and parse JSON outputs
2. **State Callbacks**: Trigger actions on state changes
3. **State Queries**: Query state from tools and callbacks

## Running the Demo

```bash
GEMINI_API_KEY=your_key cargo run --package adk-agent --example state_management_demo
```

## Related Files

- `adk-agent/src/llm_agent.rs` - OutputKey implementation
- `adk-core/src/event.rs` - EventActions with state_delta
- `adk-session/src/state.rs` - State trait definitions
- `adk-agent/examples/state_management_demo.rs` - Working demo
- `adk-agent/tests/llm_agent_tests.rs` - Test: `test_llm_agent_output_key`
