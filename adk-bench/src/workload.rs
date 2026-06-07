//! Workload schema, loading, and validation.
//!
//! Defines the JSON workload format for reproducible benchmarks
//! and provides built-in workload definitions.
//!
//! # Schema
//!
//! Workloads are JSON files conforming to the [`Workload`] schema. Each workload
//! describes an agent scenario with instructions, tools, expected turns, and
//! metadata annotations.
//!
//! # Built-in Workloads
//!
//! Three standard workloads are provided via [`builtin_workloads()`]:
//! - **simple_tool_call** — single tool invocation measuring basic dispatch overhead
//! - **multi_step_reasoning** — multi-turn reasoning chain with sequential tool use
//! - **parallel_tool_invocation** — concurrent tool calls measuring parallel dispatch
//!
//! A fourth workload, **multi_agent_delegation**, is available via
//! [`multi_agent_delegation_workload()`] and intended for use when the
//! `experimental` runtime flag is enabled.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::workload::{load_workload, builtin_workloads};
//! use std::path::Path;
//!
//! // Load from file
//! let workload = load_workload(Path::new("my_workload.json"))?;
//!
//! // Use built-in workloads
//! let workloads = builtin_workloads();
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::{BenchError, Result};

/// A benchmark workload definition loaded from JSON.
///
/// Workloads define reproducible agent scenarios for benchmarking,
/// including agent configuration, expected behavior, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workload {
    /// Unique workload name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Agent configuration for this workload.
    pub agent: AgentConfig,
    /// LLM model identifier to use.
    pub model: String,
    /// Structured output schema (JSON Schema for response format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Expected number of agent turns.
    pub expected_turns: usize,
    /// Optional metadata annotations (arbitrary key-value pairs).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
    /// Schema version for forward compatibility.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

/// Agent configuration within a workload.
///
/// Specifies the agent's instructions, available tools, and the
/// initial user message to benchmark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    /// System instructions for the agent.
    pub instructions: String,
    /// Tool definitions available to the agent.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tools: HashMap<String, ToolDefinition>,
    /// User message to send as the initial prompt.
    pub user_message: String,
}

/// Tool definition within a workload.
///
/// Describes a simulated tool for benchmarking purposes, including
/// its schema and optional fixed response for deterministic execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// Tool description for the LLM.
    pub description: String,
    /// JSON Schema for tool parameters.
    pub parameters: serde_json::Value,
    /// Simulated execution time in milliseconds (for benchmarking tool dispatch).
    #[serde(default)]
    pub simulated_latency_ms: u64,
    /// Fixed response value returned by the simulated tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_response: Option<serde_json::Value>,
}

/// Loads and validates a workload from a JSON file path.
///
/// Reads the file, parses JSON, and validates all required fields are present
/// and well-formed. Returns a descriptive [`BenchError::WorkloadValidation`]
/// error on schema violations.
///
/// # Errors
///
/// - [`BenchError::WorkloadNotFound`] if the file does not exist
/// - [`BenchError::WorkloadValidation`] if JSON parsing fails or required fields are invalid
pub fn load_workload(path: &Path) -> Result<Workload> {
    let path_str = path.display().to_string();

    if !path.exists() {
        return Err(BenchError::WorkloadNotFound { path: path_str });
    }

    let content = std::fs::read_to_string(path).map_err(|e| BenchError::WorkloadValidation {
        field: "file".to_string(),
        reason: format!("failed to read workload file '{path_str}': {e}"),
    })?;

    let workload: Workload =
        serde_json::from_str(&content).map_err(|e| BenchError::WorkloadValidation {
            field: parse_error_field(&e),
            reason: format!("invalid workload JSON: {e}"),
        })?;

    validate_workload(&workload)?;

    Ok(workload)
}

/// Returns built-in benchmark workloads.
///
/// Provides three standard workloads for common benchmarking scenarios:
/// - `simple_tool_call` — single tool invocation
/// - `multi_step_reasoning` — multi-turn reasoning chain
/// - `parallel_tool_invocation` — concurrent tool calls
///
/// The multi-agent delegation workload is intentionally excluded here;
/// use [`multi_agent_delegation_workload()`] when the `experimental`
/// runtime flag is enabled.
pub fn builtin_workloads() -> Vec<Workload> {
    vec![
        simple_tool_call_workload(),
        multi_step_reasoning_workload(),
        parallel_tool_invocation_workload(),
    ]
}

/// Returns the multi-agent delegation workload.
///
/// This workload exercises multi-agent orchestration where a coordinator
/// agent delegates subtasks to specialist agents. It is intended for use
/// only when the `experimental` runtime configuration flag is enabled,
/// as the multi-agent API may not be stable.
pub fn multi_agent_delegation_workload() -> Workload {
    let mut tools = HashMap::new();
    tools.insert(
        "delegate_to_researcher".to_string(),
        ToolDefinition {
            description: "Delegate a research subtask to the researcher agent".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The research query to investigate"
                    },
                    "depth": {
                        "type": "string",
                        "enum": ["shallow", "deep"],
                        "description": "How thorough the research should be"
                    }
                },
                "required": ["query"]
            }),
            simulated_latency_ms: 50,
            fixed_response: Some(serde_json::json!({
                "findings": "Research results on the topic",
                "confidence": 0.85,
                "sources": ["source_1", "source_2"]
            })),
        },
    );
    tools.insert(
        "delegate_to_writer".to_string(),
        ToolDefinition {
            description: "Delegate a writing subtask to the writer agent".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "The topic to write about"
                    },
                    "style": {
                        "type": "string",
                        "enum": ["formal", "casual", "technical"],
                        "description": "Writing style"
                    },
                    "max_words": {
                        "type": "integer",
                        "description": "Maximum word count"
                    }
                },
                "required": ["topic", "style"]
            }),
            simulated_latency_ms: 75,
            fixed_response: Some(serde_json::json!({
                "content": "Generated content based on research findings",
                "word_count": 250
            })),
        },
    );

    let mut metadata = HashMap::new();
    metadata.insert("category".to_string(), serde_json::Value::String("multi-agent".to_string()));
    metadata.insert("stability".to_string(), serde_json::Value::String("experimental".to_string()));

    Workload {
        name: "multi_agent_delegation".to_string(),
        description: "Coordinator agent delegates research and writing subtasks to specialist agents, measuring multi-agent orchestration overhead".to_string(),
        agent: AgentConfig {
            instructions: "You are a project coordinator. Break down the user's request into research and writing subtasks. First delegate research to gather information, then delegate writing to produce the final output.".to_string(),
            tools,
            user_message: "Write a technical summary about the performance benefits of async runtimes in systems programming.".to_string(),
        },
        model: "gemini-2.5-flash".to_string(),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
                "research_quality": { "type": "number" },
                "delegations_made": { "type": "integer" }
            },
            "required": ["summary", "delegations_made"]
        })),
        expected_turns: 5,
        metadata,
        schema_version: 1,
    }
}

fn simple_tool_call_workload() -> Workload {
    let mut tools = HashMap::new();
    tools.insert(
        "get_weather".to_string(),
        ToolDefinition {
            description: "Get the current weather for a given city".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {
                        "type": "string",
                        "description": "The city name to get weather for"
                    },
                    "units": {
                        "type": "string",
                        "enum": ["celsius", "fahrenheit"],
                        "description": "Temperature units"
                    }
                },
                "required": ["city"]
            }),
            simulated_latency_ms: 10,
            fixed_response: Some(serde_json::json!({
                "temperature": 22.5,
                "condition": "sunny",
                "humidity": 45
            })),
        },
    );

    Workload {
        name: "simple_tool_call".to_string(),
        description: "Single tool invocation measuring basic dispatch overhead. The agent receives a weather query and must call one tool to respond."
            .to_string(),
        agent: AgentConfig {
            instructions: "You are a helpful weather assistant. When asked about weather, use the get_weather tool to retrieve current conditions.".to_string(),
            tools,
            user_message: "What is the weather in San Francisco?".to_string(),
        },
        model: "gemini-2.5-flash".to_string(),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "temperature": { "type": "number" },
                "condition": { "type": "string" },
                "city": { "type": "string" }
            },
            "required": ["temperature", "condition", "city"]
        })),
        expected_turns: 2,
        metadata: HashMap::new(),
        schema_version: 1,
    }
}

fn multi_step_reasoning_workload() -> Workload {
    let mut tools = HashMap::new();
    tools.insert(
        "search_database".to_string(),
        ToolDefinition {
            description: "Search a product database by query".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "category": {
                        "type": "string",
                        "description": "Product category filter"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return"
                    }
                },
                "required": ["query"]
            }),
            simulated_latency_ms: 15,
            fixed_response: Some(serde_json::json!({
                "results": [
                    {"id": "p1", "name": "Widget A", "price": 29.99, "rating": 4.5},
                    {"id": "p2", "name": "Widget B", "price": 19.99, "rating": 4.2},
                    {"id": "p3", "name": "Widget C", "price": 39.99, "rating": 4.8}
                ],
                "total_count": 3
            })),
        },
    );
    tools.insert(
        "get_product_details".to_string(),
        ToolDefinition {
            description: "Get detailed information about a specific product".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "product_id": {
                        "type": "string",
                        "description": "The product identifier"
                    }
                },
                "required": ["product_id"]
            }),
            simulated_latency_ms: 10,
            fixed_response: Some(serde_json::json!({
                "id": "p3",
                "name": "Widget C",
                "price": 39.99,
                "rating": 4.8,
                "reviews": 128,
                "in_stock": true,
                "description": "Premium widget with advanced features"
            })),
        },
    );
    tools.insert(
        "calculate_shipping".to_string(),
        ToolDefinition {
            description: "Calculate shipping cost for a product to a destination".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "product_id": {
                        "type": "string",
                        "description": "The product identifier"
                    },
                    "destination": {
                        "type": "string",
                        "description": "Shipping destination (zip code or city)"
                    }
                },
                "required": ["product_id", "destination"]
            }),
            simulated_latency_ms: 10,
            fixed_response: Some(serde_json::json!({
                "cost": 5.99,
                "estimated_days": 3,
                "carrier": "standard"
            })),
        },
    );

    Workload {
        name: "multi_step_reasoning".to_string(),
        description: "Multi-turn reasoning chain with sequential tool use. The agent must search products, get details on the best match, and calculate shipping — each step depends on previous results."
            .to_string(),
        agent: AgentConfig {
            instructions: "You are a shopping assistant. Help the user find the best product by searching the database, getting details on the top-rated result, and calculating shipping to their location.".to_string(),
            tools,
            user_message: "Find me the best-rated widget and tell me the total cost including shipping to 94105.".to_string(),
        },
        model: "gemini-2.5-flash".to_string(),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "product_name": { "type": "string" },
                "product_price": { "type": "number" },
                "shipping_cost": { "type": "number" },
                "total_cost": { "type": "number" },
                "estimated_delivery_days": { "type": "integer" }
            },
            "required": ["product_name", "total_cost"]
        })),
        expected_turns: 4,
        metadata: HashMap::new(),
        schema_version: 1,
    }
}

fn parallel_tool_invocation_workload() -> Workload {
    let mut tools = HashMap::new();
    tools.insert(
        "fetch_stock_price".to_string(),
        ToolDefinition {
            description: "Fetch the current stock price for a ticker symbol".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ticker": {
                        "type": "string",
                        "description": "Stock ticker symbol (e.g., AAPL, GOOGL)"
                    }
                },
                "required": ["ticker"]
            }),
            simulated_latency_ms: 20,
            fixed_response: Some(serde_json::json!({
                "ticker": "AAPL",
                "price": 178.50,
                "change": 2.30,
                "change_percent": 1.31
            })),
        },
    );
    tools.insert(
        "fetch_company_news".to_string(),
        ToolDefinition {
            description: "Fetch recent news headlines for a company".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ticker": {
                        "type": "string",
                        "description": "Stock ticker symbol"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of headlines"
                    }
                },
                "required": ["ticker"]
            }),
            simulated_latency_ms: 25,
            fixed_response: Some(serde_json::json!({
                "headlines": [
                    "Company reports strong Q4 earnings",
                    "New product launch announced for next quarter"
                ]
            })),
        },
    );
    tools.insert(
        "fetch_analyst_rating".to_string(),
        ToolDefinition {
            description: "Fetch analyst consensus rating for a stock".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ticker": {
                        "type": "string",
                        "description": "Stock ticker symbol"
                    }
                },
                "required": ["ticker"]
            }),
            simulated_latency_ms: 15,
            fixed_response: Some(serde_json::json!({
                "rating": "buy",
                "target_price": 195.00,
                "analyst_count": 32
            })),
        },
    );

    Workload {
        name: "parallel_tool_invocation".to_string(),
        description: "Concurrent tool calls measuring parallel dispatch efficiency. The agent must fetch stock price, news, and analyst rating simultaneously for a portfolio analysis."
            .to_string(),
        agent: AgentConfig {
            instructions: "You are a financial analyst assistant. When asked about a stock, fetch the current price, recent news, and analyst rating in parallel to provide a comprehensive summary.".to_string(),
            tools,
            user_message: "Give me a complete analysis of AAPL including current price, recent news, and analyst consensus.".to_string(),
        },
        model: "gemini-2.5-flash".to_string(),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "ticker": { "type": "string" },
                "current_price": { "type": "number" },
                "analyst_rating": { "type": "string" },
                "target_price": { "type": "number" },
                "summary": { "type": "string" }
            },
            "required": ["ticker", "current_price", "analyst_rating"]
        })),
        expected_turns: 2,
        metadata: HashMap::new(),
        schema_version: 1,
    }
}

/// Validates a workload's required fields and constraints.
fn validate_workload(workload: &Workload) -> Result<()> {
    if workload.name.is_empty() {
        return Err(BenchError::WorkloadValidation {
            field: "name".to_string(),
            reason: "workload name must not be empty".to_string(),
        });
    }

    if workload.description.is_empty() {
        return Err(BenchError::WorkloadValidation {
            field: "description".to_string(),
            reason: "workload description must not be empty".to_string(),
        });
    }

    if workload.model.is_empty() {
        return Err(BenchError::WorkloadValidation {
            field: "model".to_string(),
            reason: "model identifier must not be empty".to_string(),
        });
    }

    if workload.agent.instructions.is_empty() {
        return Err(BenchError::WorkloadValidation {
            field: "agent.instructions".to_string(),
            reason: "agent instructions must not be empty".to_string(),
        });
    }

    if workload.agent.user_message.is_empty() {
        return Err(BenchError::WorkloadValidation {
            field: "agent.userMessage".to_string(),
            reason: "agent user message must not be empty".to_string(),
        });
    }

    if workload.expected_turns == 0 {
        return Err(BenchError::WorkloadValidation {
            field: "expectedTurns".to_string(),
            reason: "expected turns must be at least 1".to_string(),
        });
    }

    if workload.schema_version == 0 {
        return Err(BenchError::WorkloadValidation {
            field: "schemaVersion".to_string(),
            reason: "schema version must be at least 1".to_string(),
        });
    }

    // Validate tool definitions
    for (tool_name, tool_def) in &workload.agent.tools {
        if tool_def.description.is_empty() {
            return Err(BenchError::WorkloadValidation {
                field: format!("agent.tools.{tool_name}.description"),
                reason: "tool description must not be empty".to_string(),
            });
        }
    }

    Ok(())
}

/// Extracts the field name from a serde_json parse error when possible.
fn parse_error_field(error: &serde_json::Error) -> String {
    // serde_json errors include line/column but not always the field name.
    // We provide the best context available.
    let msg = error.to_string();
    if msg.contains("missing field") {
        // Extract field name from "missing field `fieldName`"
        if let Some(start) = msg.find('`')
            && let Some(end) = msg[start + 1..].find('`')
        {
            return msg[start + 1..start + 1 + end].to_string();
        }
    }
    "root".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_builtin_workloads_count() {
        let workloads = builtin_workloads();
        assert_eq!(workloads.len(), 3);
    }

    #[test]
    fn test_builtin_workload_names() {
        let workloads = builtin_workloads();
        let names: Vec<&str> = workloads.iter().map(|w| w.name.as_str()).collect();
        assert!(names.contains(&"simple_tool_call"));
        assert!(names.contains(&"multi_step_reasoning"));
        assert!(names.contains(&"parallel_tool_invocation"));
    }

    #[test]
    fn test_multi_agent_delegation_not_in_builtin() {
        let workloads = builtin_workloads();
        let names: Vec<&str> = workloads.iter().map(|w| w.name.as_str()).collect();
        assert!(!names.contains(&"multi_agent_delegation"));
    }

    #[test]
    fn test_multi_agent_delegation_workload() {
        let workload = multi_agent_delegation_workload();
        assert_eq!(workload.name, "multi_agent_delegation");
        assert_eq!(workload.expected_turns, 5);
        assert!(workload.agent.tools.contains_key("delegate_to_researcher"));
        assert!(workload.agent.tools.contains_key("delegate_to_writer"));
        assert!(workload.metadata.contains_key("stability"));
    }

    #[test]
    fn test_workload_serialization_round_trip() {
        let workloads = builtin_workloads();
        for workload in &workloads {
            let json = serde_json::to_string(workload).unwrap();
            let deserialized: Workload = serde_json::from_str(&json).unwrap();
            assert_eq!(workload, &deserialized);
        }
    }

    #[test]
    fn test_load_workload_not_found() {
        let result = load_workload(Path::new("/nonexistent/path.json"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BenchError::WorkloadNotFound { .. }));
    }

    #[test]
    fn test_load_workload_invalid_json() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "not valid json").unwrap();
        let result = load_workload(file.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BenchError::WorkloadValidation { .. }));
    }

    #[test]
    fn test_load_workload_missing_field() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"name": "test"}}"#).unwrap();
        let result = load_workload(file.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BenchError::WorkloadValidation { .. }));
    }

    #[test]
    fn test_load_workload_valid() {
        let workload = simple_tool_call_workload();
        let json = serde_json::to_string_pretty(&workload).unwrap();

        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{json}").unwrap();

        let loaded = load_workload(file.path()).unwrap();
        assert_eq!(workload, loaded);
    }

    #[test]
    fn test_validate_empty_name() {
        let mut workload = simple_tool_call_workload();
        workload.name = String::new();
        let result = validate_workload(&workload);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_zero_expected_turns() {
        let mut workload = simple_tool_call_workload();
        workload.expected_turns = 0;
        let result = validate_workload(&workload);
        assert!(result.is_err());
    }

    #[test]
    fn test_schema_version_defaults_to_1() {
        let json = r#"{
            "name": "test",
            "description": "test workload",
            "agent": {
                "instructions": "do something",
                "userMessage": "hello"
            },
            "model": "gemini-2.5-flash",
            "expectedTurns": 2
        }"#;
        let workload: Workload = serde_json::from_str(json).unwrap();
        assert_eq!(workload.schema_version, 1);
    }

    #[test]
    fn test_metadata_preserved_in_round_trip() {
        let mut workload = simple_tool_call_workload();
        workload
            .metadata
            .insert("author".to_string(), serde_json::Value::String("test-user".to_string()));
        workload.metadata.insert("version".to_string(), serde_json::json!(2));

        let json = serde_json::to_string(&workload).unwrap();
        let deserialized: Workload = serde_json::from_str(&json).unwrap();
        assert_eq!(workload.metadata, deserialized.metadata);
    }
}
