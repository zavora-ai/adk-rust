//! ReAct Agent Pattern with Cycles
//!
//! This example demonstrates the core LangGraph pattern: a ReAct (Reasoning + Acting)
//! agent that loops between LLM reasoning and tool execution until complete.
//!
//! Graph: START -> agent -> [has_tool_calls: tools | done: END] -> agent (cycle)
//!
//! Key concepts demonstrated:
//! - Cycles in graph execution
//! - Conditional routing based on state
//! - Tool call detection and execution
//! - Recursion limits for safety

use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;

/// Simulated tool execution
fn execute_tool(name: &str, args: &serde_json::Value) -> serde_json::Value {
    match name {
        "get_weather" => {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            json!({
                "tool": "get_weather",
                "result": format!("Weather in {}: Sunny, 72°F", location)
            })
        }
        "search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "tool": "search",
                "result": format!("Search results for '{}': Found 3 relevant articles about the topic.", query)
            })
        }
        "calculator" => {
            let expression = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            // Simple eval for demo
            let result = match expression {
                "2 + 2" => "4",
                "10 * 5" => "50",
                _ => "Unable to calculate",
            };
            json!({
                "tool": "calculator",
                "result": result
            })
        }
        _ => json!({
            "tool": name,
            "result": "Unknown tool"
        }),
    }
}

/// Simulated LLM that decides what to do next
fn simulate_agent_response(
    messages: &[serde_json::Value],
    iteration: i64,
) -> (String, Option<Vec<serde_json::Value>>) {
    // Look at the conversation history to decide next action
    let last_message = messages.last();
    let has_tool_result =
        last_message.and_then(|m| m.get("role")).and_then(|r| r.as_str()) == Some("tool");

    // Simulate agent reasoning based on iteration
    match iteration {
        0 => {
            // First iteration: decide to use tools
            let thought =
                "I need to check the weather and do a calculation to answer the user's question.";
            let tool_calls = vec![
                json!({
                    "name": "get_weather",
                    "args": {"location": "San Francisco"}
                }),
                json!({
                    "name": "calculator",
                    "args": {"expression": "2 + 2"}
                }),
            ];
            (thought.to_string(), Some(tool_calls))
        }
        1 if has_tool_result => {
            // Second iteration: got tool results, maybe need more info
            let thought = "I got the weather and calculation. Let me search for more context.";
            let tool_calls = vec![json!({
                "name": "search",
                "args": {"query": "San Francisco events today"}
            })];
            (thought.to_string(), Some(tool_calls))
        }
        _ => {
            // Final iteration: synthesize answer
            let thought = "I now have all the information I need. The weather in San Francisco is sunny at 72°F, 2+2=4, and there are several events happening today.";
            (thought.to_string(), None) // No more tool calls = done
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== ReAct Agent Pattern Example ===\n");
    println!("This demonstrates a ReAct agent that cycles between reasoning and tool use.\n");

    // Build the ReAct agent graph
    let graph = StateGraph::with_channels(&["messages", "tool_calls", "iteration"])
        // Agent node: decides what to do next
        .add_node_fn("agent", |ctx| async move {
            let messages =
                ctx.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let iteration = ctx.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);

            println!("[agent] Iteration {}: Reasoning...", iteration);

            // Simulate LLM response
            let (thought, tool_calls) = simulate_agent_response(&messages, iteration);

            println!("[agent] Thought: {}", thought);

            // Build new message
            let mut agent_message = json!({
                "role": "assistant",
                "content": thought
            });

            // Add tool calls if any
            let has_tools = tool_calls.is_some();
            if let Some(calls) = &tool_calls {
                agent_message["tool_calls"] = json!(calls);
                println!(
                    "[agent] Tool calls: {}",
                    calls
                        .iter()
                        .map(|c| c.get("name").and_then(|n| n.as_str()).unwrap_or("?"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                println!("[agent] No more tools needed, ready to respond.");
            }

            // Append to messages
            let mut new_messages = messages;
            new_messages.push(agent_message);

            Ok(NodeOutput::new()
                .with_update("messages", json!(new_messages))
                .with_update("tool_calls", tool_calls.map(|tc| json!(tc)).unwrap_or(json!(null)))
                .with_update("iteration", json!(iteration + 1))
                .with_update("has_tool_calls", json!(has_tools)))
        })
        // Tools node: executes tool calls
        .add_node_fn("tools", |ctx| async move {
            let messages =
                ctx.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let tool_calls =
                ctx.get("tool_calls").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            println!("[tools] Executing {} tool(s)...", tool_calls.len());

            let mut new_messages = messages;

            // Execute each tool
            for call in tool_calls {
                let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let empty_args = json!({});
                let args = call.get("args").unwrap_or(&empty_args);

                let result = execute_tool(name, args);
                println!("  - {}: {}", name, result.get("result").unwrap_or(&json!("?")));

                // Add tool result as message
                new_messages.push(json!({
                    "role": "tool",
                    "name": name,
                    "content": result
                }));
            }

            Ok(NodeOutput::new().with_update("messages", json!(new_messages)))
        })
        // Define the graph structure
        .add_edge(START, "agent")
        // Conditional: if tool calls exist, go to tools; otherwise end
        .add_conditional_edges(
            "agent",
            |state| {
                let has_tools =
                    state.get("has_tool_calls").and_then(|v| v.as_bool()).unwrap_or(false);
                if has_tools {
                    "tools".to_string()
                } else {
                    END.to_string()
                }
            },
            [("tools", "tools"), (END, END)],
        )
        // After tools, cycle back to agent
        .add_edge("tools", "agent")
        .compile()?
        .with_recursion_limit(10); // Safety limit

    // Run the agent
    let mut input = State::new();
    input.insert(
        "messages".to_string(),
        json!([{
            "role": "user",
            "content": "What's the weather in San Francisco and what's 2+2?"
        }]),
    );

    println!("User: What's the weather in San Francisco and what's 2+2?\n");
    println!("{}", "=".repeat(60));

    let result = graph.invoke(input, ExecutionConfig::new("react-thread")).await?;

    println!("{}", "=".repeat(60));

    // Extract final response
    let messages = result.get("messages").and_then(|v| v.as_array()).unwrap();
    let iterations = result.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);

    println!("\nCompleted in {} iterations", iterations);
    println!("\nConversation history ({} messages):", messages.len());
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if role == "tool" {
            let name = msg.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            println!("  [{}] {}: {:?}", role, name, msg.get("content"));
        } else {
            println!("  [{}] {}", role, &content[..content.len().min(80)]);
        }
    }

    println!("\n=== Complete ===");
    Ok(())
}
