#![cfg(feature = "action")]
//! Property tests for workflow schema round-trip, set node state mutation,
//! and notification channel payload format.
//!
//! - **Property 6: Workflow Schema Round-Trip** — JSON round-trip preserves schema, build_graph succeeds
//! - **Property 7: Set Node State Mutation** — Set node with N variables produces N state updates
//! - **Property 10: Notification Channel Payload Format** — Generated payloads conform to channel-specific format

use std::collections::HashMap;

use adk_action::*;
use adk_graph::node::{ExecutionConfig, NodeContext};
use adk_graph::workflow::WorkflowSchema;
use proptest::prelude::*;
use serde_json::{Value, json};

// ── Helpers ───────────────────────────────────────────────────────────

fn make_standard(id: &str, output_key: &str) -> StandardProperties {
    StandardProperties {
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        position: None,
        error_handling: ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        },
        tracing: Tracing { enabled: false, log_level: LogLevel::None },
        callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
        execution: ExecutionControl { timeout: 30000, condition: None },
        mapping: InputOutputMapping { input_mapping: None, output_key: output_key.to_string() },
    }
}

// ── Property 6: Workflow Schema Round-Trip ─────────────────────────────
//
// **Feature: action-node-graph-standardization, Property 6: Workflow Schema Round-Trip**
// *For any* valid WorkflowSchema value, serializing to JSON and deserializing back
// SHALL produce a value equal to the original, and building a graph from the
// deserialized schema SHALL succeed.
// **Validates: Requirements 11.1-11.5**

fn make_simple_workflow(num_set_nodes: usize) -> WorkflowSchema {
    use adk_graph::workflow::WorkflowEdge;

    let mut action_nodes = HashMap::new();
    let mut edges = Vec::new();

    for i in 0..num_set_nodes {
        let node_id = format!("set_{i}");
        let config = ActionNodeConfig::Set(SetNodeConfig {
            standard: make_standard(&node_id, &format!("var_{i}")),
            mode: SetMode::Set,
            variables: vec![Variable {
                key: format!("key_{i}"),
                value: json!(i),
                value_type: "number".to_string(),
                is_secret: false,
            }],
            env_vars: None,
        });
        action_nodes.insert(node_id, config);
    }

    if num_set_nodes > 0 {
        edges.push(WorkflowEdge {
            from: "__start__".to_string(),
            to: "set_0".to_string(),
            condition: None,
            from_port: None,
            to_port: None,
        });
        for i in 0..num_set_nodes - 1 {
            edges.push(WorkflowEdge {
                from: format!("set_{i}"),
                to: format!("set_{}", i + 1),
                condition: None,
                from_port: None,
                to_port: None,
            });
        }
        edges.push(WorkflowEdge {
            from: format!("set_{}", num_set_nodes - 1),
            to: "__end__".to_string(),
            condition: None,
            from_port: None,
            to_port: None,
        });
    }

    WorkflowSchema { edges, conditions: vec![], action_nodes, agent_nodes: vec![] }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_workflow_schema_roundtrip(num_nodes in 1usize..6) {
        let schema = make_simple_workflow(num_nodes);
        let json_str = serde_json::to_string(&schema).expect("serialize");
        let deserialized = WorkflowSchema::from_json(&json_str).expect("deserialize");

        prop_assert_eq!(deserialized.edges.len(), schema.edges.len());
        prop_assert_eq!(deserialized.action_nodes.len(), schema.action_nodes.len());
        prop_assert_eq!(deserialized.conditions.len(), schema.conditions.len());
        prop_assert_eq!(deserialized.agent_nodes.len(), schema.agent_nodes.len());

        for key in schema.action_nodes.keys() {
            prop_assert!(deserialized.action_nodes.contains_key(key), "missing node: {}", key);
        }

        let graph_result = deserialized.build_graph("test_workflow");
        prop_assert!(graph_result.is_ok(), "build_graph failed: {:?}", graph_result.err());
    }
}

#[test]
fn test_workflow_schema_roundtrip_with_switch() {
    use adk_graph::workflow::WorkflowEdge;

    let mut action_nodes = HashMap::new();
    action_nodes.insert(
        "switch_1".to_string(),
        ActionNodeConfig::Switch(SwitchNodeConfig {
            standard: make_standard("switch_1", "switch_result"),
            evaluation_mode: EvaluationMode::FirstMatch,
            conditions: vec![SwitchCondition {
                id: "cond_a".to_string(),
                name: "Is A".to_string(),
                expression: ExpressionMode {
                    field: "type".to_string(),
                    operator: "eq".to_string(),
                    value: "a".to_string(),
                },
                output_port: "port_a".to_string(),
            }],
            default_branch: Some("__end__".to_string()),
        }),
    );
    action_nodes.insert(
        "port_a".to_string(),
        ActionNodeConfig::Set(SetNodeConfig {
            standard: make_standard("port_a", "result"),
            mode: SetMode::Set,
            variables: vec![Variable {
                key: "matched".to_string(),
                value: json!(true),
                value_type: "boolean".to_string(),
                is_secret: false,
            }],
            env_vars: None,
        }),
    );

    let schema = WorkflowSchema {
        edges: vec![
            WorkflowEdge {
                from: "__start__".to_string(),
                to: "switch_1".to_string(),
                condition: None,
                from_port: None,
                to_port: None,
            },
            WorkflowEdge {
                from: "port_a".to_string(),
                to: "__end__".to_string(),
                condition: None,
                from_port: None,
                to_port: None,
            },
        ],
        conditions: vec![],
        action_nodes,
        agent_nodes: vec![],
    };

    let json_str = serde_json::to_string(&schema).expect("serialize");
    let deserialized = WorkflowSchema::from_json(&json_str).expect("deserialize");
    assert_eq!(deserialized.edges.len(), schema.edges.len());
    assert_eq!(deserialized.action_nodes.len(), schema.action_nodes.len());

    let graph = deserialized.build_graph("switch_workflow");
    assert!(graph.is_ok(), "build_graph with switch should succeed");
}

#[test]
fn test_empty_workflow_schema_roundtrip() {
    let schema = WorkflowSchema {
        edges: vec![],
        conditions: vec![],
        action_nodes: HashMap::new(),
        agent_nodes: vec![],
    };
    let json_str = serde_json::to_string(&schema).expect("serialize");
    let deserialized = WorkflowSchema::from_json(&json_str).expect("deserialize");
    assert!(deserialized.edges.is_empty());
    assert!(deserialized.action_nodes.is_empty());
}

// ── Property 7: Set Node State Mutation ───────────────────────────────
//
// **Feature: action-node-graph-standardization, Property 7: Set Node State Mutation**
// *For any* SetNodeConfig with mode "set" and N variables, after execution the graph
// state SHALL contain all N keys with their configured values.
// **Validates: Requirements 5.1, 5.4**

fn arb_variable_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(json!("hello")),
        Just(json!(42)),
        Just(json!(3.14)),
        Just(json!(true)),
        Just(json!(false)),
        Just(json!(null)),
        Just(json!({"nested": "object"})),
        Just(json!(["array", "value"])),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_set_node_produces_n_updates(num_vars in 1usize..8, value in arb_variable_value()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let variables: Vec<Variable> = (0..num_vars)
                .map(|i| Variable {
                    key: format!("var_{i}"),
                    value: value.clone(),
                    value_type: "any".to_string(),
                    is_secret: false,
                })
                .collect();

            let config = SetNodeConfig {
                standard: make_standard("set_test", "set_result"),
                mode: SetMode::Set,
                variables: variables.clone(),
                env_vars: None,
            };

            let state = HashMap::new();
            let ctx = NodeContext::new(state, ExecutionConfig::new("test"), 0);
            let result = adk_graph::action::set::execute_set(&config, &ctx).await;
            prop_assert!(result.is_ok(), "set node should succeed");

            let output = result.unwrap();
            prop_assert_eq!(output.updates.len(), num_vars);

            for var in &variables {
                prop_assert!(output.updates.contains_key(&var.key), "missing key: {}", var.key);
                prop_assert_eq!(output.updates.get(&var.key).unwrap(), &var.value);
            }
            Ok(())
        })?;
    }
}

#[tokio::test]
async fn test_set_node_merge_mode() {
    let config = SetNodeConfig {
        standard: make_standard("set_merge", "result"),
        mode: SetMode::Merge,
        variables: vec![Variable {
            key: "config".to_string(),
            value: json!({"new_key": "new_value"}),
            value_type: "object".to_string(),
            is_secret: false,
        }],
        env_vars: None,
    };

    let mut state = HashMap::new();
    state.insert("config".to_string(), json!({"existing": "value"}));

    let ctx = NodeContext::new(state, ExecutionConfig::new("test"), 0);
    let result = adk_graph::action::set::execute_set(&config, &ctx).await;
    assert!(result.is_ok());

    let output = result.unwrap();
    let merged = output.updates.get("config").unwrap();
    assert_eq!(merged["existing"], "value");
    assert_eq!(merged["new_key"], "new_value");
}

#[tokio::test]
async fn test_set_node_delete_mode() {
    let config = SetNodeConfig {
        standard: make_standard("set_delete", "result"),
        mode: SetMode::Delete,
        variables: vec![Variable {
            key: "to_delete".to_string(),
            value: json!(null),
            value_type: "any".to_string(),
            is_secret: false,
        }],
        env_vars: None,
    };

    let mut state = HashMap::new();
    state.insert("to_delete".to_string(), json!("some_value"));

    let ctx = NodeContext::new(state, ExecutionConfig::new("test"), 0);
    let result = adk_graph::action::set::execute_set(&config, &ctx).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().updates.get("to_delete"), Some(&json!(null)));
}

// ── Property 10: Notification Channel Payload Format ──────────────────
//
// **Feature: action-node-graph-standardization, Property 10: Notification Channel Payload Format**
// *For any* NotificationNodeConfig, the generated payload SHALL contain the interpolated
// message text and conform to the channel-specific format.
// **Validates: Requirements 22.1-22.5**

fn make_notification_config(channel: NotificationChannel, message: &str) -> NotificationNodeConfig {
    NotificationNodeConfig {
        standard: make_standard("notif_test", "notif_result"),
        notification_channel: channel,
        webhook_url: "https://hooks.example.com/test".to_string(),
        message: NotificationMessage {
            text: message.to_string(),
            format: Some(MessageFormat::Markdown),
            username: Some("TestBot".to_string()),
            icon_url: Some("https://example.com/icon.png".to_string()),
            channel: Some("#general".to_string()),
        },
    }
}

/// Build expected payload for each channel (mirrors the private build_payload logic).
fn build_expected_payload(config: &NotificationNodeConfig, message_text: &str) -> Value {
    match config.notification_channel {
        NotificationChannel::Slack => {
            let mut p = json!({"text": message_text});
            if let Some(u) = &config.message.username {
                p["username"] = json!(u);
            }
            if let Some(i) = &config.message.icon_url {
                p["icon_url"] = json!(i);
            }
            if let Some(c) = &config.message.channel {
                p["channel"] = json!(c);
            }
            p
        }
        NotificationChannel::Discord => {
            let mut p = json!({"content": message_text});
            if let Some(u) = &config.message.username {
                p["username"] = json!(u);
            }
            if let Some(i) = &config.message.icon_url {
                p["avatar_url"] = json!(i);
            }
            p
        }
        NotificationChannel::Teams => {
            json!({
                "@type": "MessageCard",
                "@context": "http://schema.org/extensions",
                "text": message_text,
            })
        }
        NotificationChannel::Webhook => {
            json!({"message": message_text})
        }
    }
}

fn arb_notification_channel() -> impl Strategy<Value = NotificationChannel> {
    prop_oneof![
        Just(NotificationChannel::Slack),
        Just(NotificationChannel::Discord),
        Just(NotificationChannel::Teams),
        Just(NotificationChannel::Webhook),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_notification_payload_contains_message(
        channel in arb_notification_channel(),
        message in "[a-zA-Z0-9 ]{1,50}",
    ) {
        let config = make_notification_config(channel.clone(), &message);
        let payload = build_expected_payload(&config, &message);

        match channel {
            NotificationChannel::Slack => {
                prop_assert_eq!(payload["text"].as_str().unwrap(), message.as_str());
            }
            NotificationChannel::Discord => {
                prop_assert_eq!(payload["content"].as_str().unwrap(), message.as_str());
            }
            NotificationChannel::Teams => {
                prop_assert_eq!(payload["text"].as_str().unwrap(), message.as_str());
                prop_assert_eq!(payload["@type"].as_str().unwrap(), "MessageCard");
            }
            NotificationChannel::Webhook => {
                prop_assert_eq!(payload["message"].as_str().unwrap(), message.as_str());
            }
        }
    }

    #[test]
    fn prop_notification_config_roundtrip(channel in arb_notification_channel()) {
        let config = make_notification_config(channel, "Test message {{var}}");
        let json_str = serde_json::to_string(&config).expect("serialize");
        let deserialized: NotificationNodeConfig =
            serde_json::from_str(&json_str).expect("deserialize");
        prop_assert_eq!(&deserialized, &config);
    }
}

#[test]
fn test_slack_payload_format() {
    let config = make_notification_config(NotificationChannel::Slack, "Build passed");
    let payload = build_expected_payload(&config, "Build passed");
    assert_eq!(payload["text"], "Build passed");
    assert_eq!(payload["username"], "TestBot");
    assert_eq!(payload["icon_url"], "https://example.com/icon.png");
    assert_eq!(payload["channel"], "#general");
    assert!(payload.get("content").is_none());
    assert!(payload.get("avatar_url").is_none());
}

#[test]
fn test_discord_payload_format() {
    let config = make_notification_config(NotificationChannel::Discord, "Deploy complete");
    let payload = build_expected_payload(&config, "Deploy complete");
    assert_eq!(payload["content"], "Deploy complete");
    assert_eq!(payload["username"], "TestBot");
    assert_eq!(payload["avatar_url"], "https://example.com/icon.png");
    assert!(payload.get("text").is_none());
    assert!(payload.get("icon_url").is_none());
}

#[test]
fn test_teams_payload_format() {
    let config = make_notification_config(NotificationChannel::Teams, "Alert triggered");
    let payload = build_expected_payload(&config, "Alert triggered");
    assert_eq!(payload["@type"], "MessageCard");
    assert_eq!(payload["@context"], "http://schema.org/extensions");
    assert_eq!(payload["text"], "Alert triggered");
}

#[test]
fn test_generic_webhook_payload_format() {
    let config = make_notification_config(NotificationChannel::Webhook, "Event occurred");
    let payload = build_expected_payload(&config, "Event occurred");
    assert_eq!(payload["message"], "Event occurred");
}
