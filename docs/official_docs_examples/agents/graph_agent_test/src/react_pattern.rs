use adk_agent::LlmAgentBuilder;
use adk_core::{Part, Tool};
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use adk_tool::FunctionTool;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("🔄 Starting ReAct Pattern Example");
    println!("This demonstrates iterative reasoning with tools\n");

    // Create tools
    let weather_tool = Arc::new(FunctionTool::new(
        "get_weather",
        "Get the current weather for a location. Takes a 'location' parameter (city name).",
        |_ctx, args| async move {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            println!("🌤️  Getting weather for: {}", location);
            Ok(json!({
                "location": location,
                "temperature": "72°F",
                "condition": "Sunny",
                "humidity": "45%"
            }))
        },
    )) as Arc<dyn Tool>;

    let calculator_tool = Arc::new(FunctionTool::new(
        "calculator",
        "Perform mathematical calculations. Takes an 'expression' parameter (string).",
        |_ctx, args| async move {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            println!("🧮 Calculating: {}", expr);
            let result = match expr {
                "2 + 2" => "4",
                "10 * 5" => "50",
                "100 / 4" => "25",
                "15 - 7" => "8",
                "15 + 25" => "40",
                _ => "Unable to evaluate",
            };
            Ok(json!({ "result": result, "expression": expr }))
        },
    )) as Arc<dyn Tool>;

    // Create reasoner agent with tools
    let reasoner_agent = Arc::new(
        LlmAgentBuilder::new("reasoner")
            .description("Reasoning agent with tools")
            .model(model.clone())
            .instruction(
                "You are a helpful assistant with access to tools. Use tools when needed to answer questions. \
                When you have enough information, provide a final answer without using more tools.",
            )
            .tool(weather_tool)
            .tool(calculator_tool)
            .build()?,
    );

    // Create reasoner node that detects tool usage
    let reasoner_node = AgentNode::new(reasoner_agent)
        .with_input_mapper(|state| {
            let question = state.get("question").and_then(|v| v.as_str()).unwrap_or("");
            let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
            
            if iteration == 0 {
                println!("🤔 Initial reasoning: {}", question);
            } else {
                println!("🔄 Iteration {}: Continuing reasoning...", iteration);
            }
            
            adk_core::Content::new("user").with_text(question)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let mut has_tool_calls = false;
            let mut response = String::new();

            for event in events {
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        match part {
                            Part::FunctionCall { name, .. } => {
                                println!("🔧 Tool called: {}", name);
                                has_tool_calls = true;
                            }
                            Part::Text { text } => {
                                response.push_str(text);
                            }
                            _ => {}
                        }
                    }
                }
            }

            if has_tool_calls {
                println!("⚡ Tools used - will continue reasoning");
            } else {
                println!("✅ No tools needed - final answer ready");
            }

            updates.insert("has_tool_calls".to_string(), json!(has_tool_calls));
            updates.insert("response".to_string(), json!(response));
            updates
        });

    // Build ReAct graph with cycle
    let graph = StateGraph::with_channels(&["question", "has_tool_calls", "response", "iteration"])
        .add_node(reasoner_node)
        .add_node_fn("counter", |ctx| async move {
            let i = ctx.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("📊 Iteration: {}", i + 1);
            Ok(NodeOutput::new().with_update("iteration", json!(i + 1)))
        })
        .add_edge(START, "counter")
        .add_edge("counter", "reasoner")
        .add_conditional_edges(
            "reasoner",
            |state| {
                let has_tools = state.get("has_tool_calls").and_then(|v| v.as_bool()).unwrap_or(false);
                let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);

                // Safety limit - max 3 iterations
                if iteration >= 3 { 
                    println!("⚠️  Maximum iterations reached");
                    return END.to_string(); 
                }

                if has_tools {
                    "counter".to_string()  // Loop back for more reasoning
                } else {
                    END.to_string()  // Done - final answer
                }
            },
            [("counter", "counter"), (END, END)],
        )
        .compile()?
        .with_recursion_limit(5);

    // Test the ReAct agent
    let mut input = State::new();
    input.insert("question".to_string(), json!("What's the weather in Paris and what's 15 + 25?"));

    println!("🚀 Starting ReAct reasoning cycle...\n");
    let result = graph.invoke(input, ExecutionConfig::new("react-1")).await?;
    
    println!("\n🎯 Final Results:");
    println!("Answer: {}", result.get("response").and_then(|v| v.as_str()).unwrap_or(""));
    println!("Total iterations: {}", result.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0));

    Ok(())
}
