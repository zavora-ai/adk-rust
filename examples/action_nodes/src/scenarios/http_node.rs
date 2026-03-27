//! HTTP Node scenarios (requires `http` feature).
//!
//! Demonstrates GET/POST requests with JSON parsing, headers, auth, and status validation.
//! Uses httpbin.org as a live test endpoint.

use adk_action::*;
use adk_graph::agent::GraphAgent;
use adk_graph::edge::{END, START};
use adk_graph::state::State;
use adk_graph::ExecutionConfig;
use anyhow::Result;
use serde_json::json;

use super::set_node::standard;

pub async fn run() -> Result<()> {
    println!("── 11. HTTP Node (action-http) ─────────────────");

    // 11a. GET request with JSON response
    let graph = GraphAgent::builder("http-get")
        .description("HTTP GET demo")
        .channels(&["httpResult"])
        .action_node(ActionNodeConfig::Http(HttpNodeConfig {
            standard: standard("get_ip", "Get IP Info", "httpResult"),
            method: HttpMethod::Get,
            url: "https://httpbin.org/get".into(),
            headers: std::collections::HashMap::new(),
            auth: HttpAuth::None,
            body: HttpBody::None,
            response: HttpResponse {
                response_type: "json".into(),
                status_validation: Some("200-299".into()),
            },
            rate_limit: None,
        }))
        .edge(START, "get_ip")
        .edge("get_ip", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let http = &result["httpResult"];
    println!("  GET:         status={}, has_data={}", http["status"], http["data"].is_object());
    assert_eq!(http["status"], json!(200));
    assert!(http["data"]["url"].as_str().unwrap().contains("httpbin.org"));

    // 11b. POST with JSON body
    let graph = GraphAgent::builder("http-post")
        .description("HTTP POST demo")
        .channels(&["httpResult"])
        .action_node(ActionNodeConfig::Http(HttpNodeConfig {
            standard: standard("post_data", "Post Data", "httpResult"),
            method: HttpMethod::Post,
            url: "https://httpbin.org/post".into(),
            headers: std::collections::HashMap::new(),
            auth: HttpAuth::None,
            body: HttpBody::Json {
                data: json!({"name": "ADK", "version": "0.5.0"}),
            },
            response: HttpResponse {
                response_type: "json".into(),
                status_validation: Some("200".into()),
            },
            rate_limit: None,
        }))
        .edge(START, "post_data")
        .edge("post_data", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    let http = &result["httpResult"];
    println!("  POST:        status={}, echoed_name={}",
        http["status"], http["data"]["json"]["name"]);
    assert_eq!(http["status"], json!(200));
    assert_eq!(http["data"]["json"]["name"], json!("ADK"));

    // 11c. URL interpolation from state
    let mut headers = std::collections::HashMap::new();
    headers.insert("X-Custom".to_string(), "{{custom_header}}".to_string());

    let graph = GraphAgent::builder("http-interpolate")
        .description("HTTP interpolation demo")
        .channels(&["endpoint", "custom_header", "httpResult"])
        .action_node(ActionNodeConfig::Http(HttpNodeConfig {
            standard: standard("interp_get", "Interpolated GET", "httpResult"),
            method: HttpMethod::Get,
            url: "https://httpbin.org/headers".into(),
            headers,
            auth: HttpAuth::None,
            body: HttpBody::None,
            response: HttpResponse {
                response_type: "json".into(),
                status_validation: None,
            },
            rate_limit: None,
        }))
        .edge(START, "interp_get")
        .edge("interp_get", END)
        .build()?;

    let mut input = State::new();
    input.insert("custom_header".into(), json!("adk-test-value"));
    let result = graph.invoke(input, ExecutionConfig::new("test")).await?;
    let http = &result["httpResult"];
    let echoed = http["data"]["headers"]["X-Custom"].as_str().unwrap_or("");
    println!("  Interpolate: X-Custom header echoed={}", echoed);
    assert_eq!(echoed, "adk-test-value");

    // 11d. Status validation failure
    let graph = GraphAgent::builder("http-status-fail")
        .description("HTTP status validation")
        .channels(&["httpResult"])
        .action_node(ActionNodeConfig::Http(HttpNodeConfig {
            standard: StandardProperties {
                id: "bad_status".into(),
                name: "Expect 201".into(),
                description: None,
                position: None,
                error_handling: ErrorHandling {
                    mode: ErrorMode::Fallback,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: Some(json!({"error": "status mismatch"})),
                },
                tracing: Tracing { enabled: true, log_level: LogLevel::Debug },
                callbacks: Callbacks::default(),
                execution: ExecutionControl { timeout: 10000, condition: None },
                mapping: InputOutputMapping {
                    input_mapping: None,
                    output_key: "httpResult".into(),
                },
            },
            method: HttpMethod::Get,
            url: "https://httpbin.org/get".into(), // returns 200, not 201
            headers: std::collections::HashMap::new(),
            auth: HttpAuth::None,
            body: HttpBody::None,
            response: HttpResponse {
                response_type: "json".into(),
                status_validation: Some("201".into()), // expect 201 only
            },
            rate_limit: None,
        }))
        .edge(START, "bad_status")
        .edge("bad_status", END)
        .build()?;

    let result = graph.invoke(State::new(), ExecutionConfig::new("test")).await?;
    println!("  Validation:  fallback={}", result["httpResult"]);
    assert_eq!(result["httpResult"]["error"], json!("status mismatch"));

    println!("  ✓ All HTTP node scenarios passed\n");
    Ok(())
}
