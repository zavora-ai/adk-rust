//! Notification action node executor (requires `action-http` feature).
//!
//! Sends notifications to Slack, Discord, Teams, or generic webhooks
//! by POSTing channel-specific JSON payloads to the configured webhook URL.
//! Uses `reqwest` (shared with the HTTP node via the `action-http` feature).

use adk_action::{NotificationChannel, NotificationNodeConfig, interpolate_variables};
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a Notification action node.
pub async fn execute_notification(
    config: &NotificationNodeConfig,
    ctx: &NodeContext,
) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;
    let state = &ctx.state;

    // Interpolate the message text
    let message_text = interpolate_variables(&config.message.text, state);

    // Build channel-specific payload
    let payload = build_payload(config, &message_text);

    tracing::debug!(
        node = %node_id,
        channel = ?config.notification_channel,
        webhook_url = %config.webhook_url,
        "sending notification"
    );

    // Interpolate webhook URL
    let webhook_url = interpolate_variables(&config.webhook_url, state);

    // POST the payload
    let client = reqwest::Client::new();
    let response = client
        .post(&webhook_url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| GraphError::NodeExecutionFailed {
            node: node_id.clone(),
            message: format!("notification request failed: {e}"),
        })?;

    let status = response.status().as_u16();
    let success = response.status().is_success();

    let body = response.text().await.unwrap_or_default();

    if !success {
        return Err(GraphError::NodeExecutionFailed {
            node: node_id.clone(),
            message: format!("notification webhook returned HTTP {status}: {body}"),
        });
    }

    let result = json!({
        "success": true,
        "channel": format!("{:?}", config.notification_channel).to_lowercase(),
        "status": status,
    });

    Ok(NodeOutput::new().with_update(output_key, result))
}

/// Build a channel-specific JSON payload for the notification.
fn build_payload(config: &NotificationNodeConfig, message_text: &str) -> Value {
    match config.notification_channel {
        NotificationChannel::Slack => build_slack_payload(config, message_text),
        NotificationChannel::Discord => build_discord_payload(config, message_text),
        NotificationChannel::Teams => build_teams_payload(message_text),
        NotificationChannel::Webhook => build_generic_payload(message_text),
    }
}

/// Build a Slack webhook payload.
///
/// Format: `{"text": "...", "username": "...", "icon_url": "...", "channel": "..."}`
fn build_slack_payload(config: &NotificationNodeConfig, message_text: &str) -> Value {
    let mut payload = json!({
        "text": message_text,
    });

    if let Some(username) = &config.message.username {
        payload["username"] = json!(username);
    }
    if let Some(icon_url) = &config.message.icon_url {
        payload["icon_url"] = json!(icon_url);
    }
    if let Some(channel) = &config.message.channel {
        payload["channel"] = json!(channel);
    }

    payload
}

/// Build a Discord webhook payload.
///
/// Format: `{"content": "...", "username": "...", "avatar_url": "..."}`
fn build_discord_payload(config: &NotificationNodeConfig, message_text: &str) -> Value {
    let mut payload = json!({
        "content": message_text,
    });

    if let Some(username) = &config.message.username {
        payload["username"] = json!(username);
    }
    if let Some(icon_url) = &config.message.icon_url {
        payload["avatar_url"] = json!(icon_url);
    }

    payload
}

/// Build a Microsoft Teams MessageCard payload.
///
/// Format: `{"@type": "MessageCard", "text": "..."}`
fn build_teams_payload(message_text: &str) -> Value {
    json!({
        "@type": "MessageCard",
        "@context": "http://schema.org/extensions",
        "text": message_text,
    })
}

/// Build a generic webhook payload.
///
/// Format: `{"message": "...", "timestamp": "..."}`
fn build_generic_payload(message_text: &str) -> Value {
    json!({
        "message": message_text,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_action::{NotificationChannel, NotificationMessage};

    fn make_config(channel: NotificationChannel) -> NotificationNodeConfig {
        NotificationNodeConfig {
            standard: adk_action::StandardProperties {
                id: "test_notif".to_string(),
                name: "Test Notification".to_string(),
                description: None,
                position: None,
                error_handling: adk_action::ErrorHandling {
                    mode: adk_action::ErrorMode::Stop,
                    retry_count: None,
                    retry_delay: None,
                    fallback_value: None,
                },
                tracing: adk_action::Tracing {
                    enabled: false,
                    log_level: adk_action::LogLevel::None,
                },
                callbacks: adk_action::Callbacks {
                    on_start: None,
                    on_complete: None,
                    on_error: None,
                },
                execution: adk_action::ExecutionControl { timeout: 30000, condition: None },
                mapping: adk_action::InputOutputMapping {
                    input_mapping: None,
                    output_key: "result".to_string(),
                },
            },
            notification_channel: channel,
            webhook_url: "https://hooks.example.com/test".to_string(),
            message: NotificationMessage {
                text: "Hello world".to_string(),
                format: None,
                username: Some("TestBot".to_string()),
                icon_url: Some("https://example.com/icon.png".to_string()),
                channel: Some("#general".to_string()),
            },
        }
    }

    #[test]
    fn test_slack_payload() {
        let config = make_config(NotificationChannel::Slack);
        let payload = build_payload(&config, "Hello world");
        assert_eq!(payload["text"], "Hello world");
        assert_eq!(payload["username"], "TestBot");
        assert_eq!(payload["icon_url"], "https://example.com/icon.png");
        assert_eq!(payload["channel"], "#general");
    }

    #[test]
    fn test_discord_payload() {
        let config = make_config(NotificationChannel::Discord);
        let payload = build_payload(&config, "Hello world");
        assert_eq!(payload["content"], "Hello world");
        assert_eq!(payload["username"], "TestBot");
        assert_eq!(payload["avatar_url"], "https://example.com/icon.png");
    }

    #[test]
    fn test_teams_payload() {
        let config = make_config(NotificationChannel::Teams);
        let payload = build_payload(&config, "Hello world");
        assert_eq!(payload["@type"], "MessageCard");
        assert_eq!(payload["text"], "Hello world");
    }

    #[test]
    fn test_generic_payload() {
        let config = make_config(NotificationChannel::Webhook);
        let payload = build_payload(&config, "Hello world");
        assert_eq!(payload["message"], "Hello world");
        assert!(payload["timestamp"].is_string());
    }
}
