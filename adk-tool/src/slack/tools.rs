//! Individual Slack tool implementations.
//!
//! Each tool calls a Slack Web API endpoint using `reqwest` and maps
//! Slack API errors to [`AdkError`].

use crate::slack::toolset::TokenSource;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Base URL for the Slack Web API.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Resolve the Slack Bot Token from the configured [`TokenSource`].
///
/// For [`TokenSource::Direct`], returns the token directly.
/// For [`TokenSource::SecretRef`], calls `ctx.get_secret()` and returns an
/// error if the secret is not configured or not found.
async fn resolve_token(token_source: &TokenSource, ctx: &Arc<dyn ToolContext>) -> Result<String> {
    match token_source {
        TokenSource::Direct(token) => Ok(token.clone()),
        TokenSource::SecretRef(secret_name) => {
            let secret = ctx.get_secret(secret_name).await?;
            secret.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unauthorized,
                    "tool.slack.missing_token",
                    format!("Slack bot token secret '{secret_name}' not found. Configure a SecretProvider or use SlackToolset::new() with a direct token."),
                )
            })
        }
    }
}

/// Parse a Slack API JSON response, returning the response body on success
/// or mapping the Slack error to an [`AdkError`].
fn parse_slack_response(body: Value) -> Result<Value> {
    let ok = body["ok"].as_bool().unwrap_or(false);
    if ok {
        Ok(body)
    } else {
        let error_code = body["error"].as_str().unwrap_or("unknown_error");
        let category = match error_code {
            "not_authed" | "invalid_auth" | "token_revoked" | "token_expired"
            | "account_inactive" => ErrorCategory::Unauthorized,
            "channel_not_found" | "not_in_channel" | "message_not_found" => ErrorCategory::NotFound,
            "ratelimited" => ErrorCategory::RateLimited,
            "invalid_arguments" | "missing_scope" | "too_many_attachments" | "no_text" => {
                ErrorCategory::InvalidInput
            }
            _ => ErrorCategory::Internal,
        };
        Err(AdkError::new(
            ErrorComponent::Tool,
            category,
            "tool.slack.api_error",
            format!("Slack API error: {error_code}"),
        ))
    }
}

// ---------------------------------------------------------------------------
// slack_send_message
// ---------------------------------------------------------------------------

/// Post a message to a Slack channel or thread.
///
/// Calls the [`chat.postMessage`](https://api.slack.com/methods/chat.postMessage)
/// Slack Web API endpoint.
pub(crate) struct SlackSendMessage {
    client: reqwest::Client,
    token_source: TokenSource,
}

impl SlackSendMessage {
    pub fn new(client: reqwest::Client, token_source: TokenSource) -> Self {
        Self { client, token_source }
    }
}

#[async_trait]
impl Tool for SlackSendMessage {
    fn name(&self) -> &str {
        "slack_send_message"
    }

    fn description(&self) -> &str {
        "Post a message to a Slack channel or thread. Returns the message timestamp on success."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The Slack channel ID to post to (e.g. C01234ABCDE)."
                },
                "text": {
                    "type": "string",
                    "description": "The message text to post."
                },
                "thread_ts": {
                    "type": "string",
                    "description": "Optional thread timestamp to reply in a thread."
                }
            },
            "required": ["channel", "text"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let token = resolve_token(&self.token_source, &ctx).await?;

        let channel = args["channel"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_channel",
                "Missing required parameter 'channel'",
            )
        })?;
        let text = args["text"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_text",
                "Missing required parameter 'text'",
            )
        })?;

        let mut body = json!({
            "channel": channel,
            "text": text,
        });
        if let Some(thread_ts) = args["thread_ts"].as_str() {
            body["thread_ts"] = json!(thread_ts);
        }

        let response = self
            .client
            .post(format!("{SLACK_API_BASE}/chat.postMessage"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "tool.slack.request_failed",
                    format!("Slack API request failed: {e}"),
                )
            })?;

        let resp_body: Value = response.json().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.slack.invalid_response",
                format!("Failed to parse Slack API response: {e}"),
            )
        })?;

        let resp = parse_slack_response(resp_body)?;
        Ok(json!({
            "ok": true,
            "ts": resp["ts"],
            "channel": resp["channel"],
        }))
    }
}

// ---------------------------------------------------------------------------
// slack_read_channel
// ---------------------------------------------------------------------------

/// Retrieve recent messages from a Slack channel.
///
/// Calls the [`conversations.history`](https://api.slack.com/methods/conversations.history)
/// Slack Web API endpoint.
pub(crate) struct SlackReadChannel {
    client: reqwest::Client,
    token_source: TokenSource,
}

impl SlackReadChannel {
    pub fn new(client: reqwest::Client, token_source: TokenSource) -> Self {
        Self { client, token_source }
    }
}

#[async_trait]
impl Tool for SlackReadChannel {
    fn name(&self) -> &str {
        "slack_read_channel"
    }

    fn description(&self) -> &str {
        "Retrieve recent messages from a Slack channel."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The Slack channel ID to read from."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of messages to return (default 20, max 1000)."
                }
            },
            "required": ["channel"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let token = resolve_token(&self.token_source, &ctx).await?;

        let channel = args["channel"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_channel",
                "Missing required parameter 'channel'",
            )
        })?;
        let limit = args["limit"].as_u64().unwrap_or(20);

        let response = self
            .client
            .get(format!("{SLACK_API_BASE}/conversations.history"))
            .bearer_auth(&token)
            .query(&[("channel", channel), ("limit", &limit.to_string())])
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "tool.slack.request_failed",
                    format!("Slack API request failed: {e}"),
                )
            })?;

        let resp_body: Value = response.json().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.slack.invalid_response",
                format!("Failed to parse Slack API response: {e}"),
            )
        })?;

        let resp = parse_slack_response(resp_body)?;
        Ok(json!({
            "ok": true,
            "messages": resp["messages"],
        }))
    }
}

// ---------------------------------------------------------------------------
// slack_add_reaction
// ---------------------------------------------------------------------------

/// Add an emoji reaction to a Slack message.
///
/// Calls the [`reactions.add`](https://api.slack.com/methods/reactions.add)
/// Slack Web API endpoint.
pub(crate) struct SlackAddReaction {
    client: reqwest::Client,
    token_source: TokenSource,
}

impl SlackAddReaction {
    pub fn new(client: reqwest::Client, token_source: TokenSource) -> Self {
        Self { client, token_source }
    }
}

#[async_trait]
impl Tool for SlackAddReaction {
    fn name(&self) -> &str {
        "slack_add_reaction"
    }

    fn description(&self) -> &str {
        "Add an emoji reaction to a message in a Slack channel."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The Slack channel ID containing the message."
                },
                "timestamp": {
                    "type": "string",
                    "description": "The timestamp of the message to react to."
                },
                "name": {
                    "type": "string",
                    "description": "The emoji name without colons (e.g. 'thumbsup')."
                }
            },
            "required": ["channel", "timestamp", "name"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let token = resolve_token(&self.token_source, &ctx).await?;

        let channel = args["channel"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_channel",
                "Missing required parameter 'channel'",
            )
        })?;
        let timestamp = args["timestamp"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_timestamp",
                "Missing required parameter 'timestamp'",
            )
        })?;
        let name = args["name"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_name",
                "Missing required parameter 'name' (emoji name without colons)",
            )
        })?;

        let body = json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": name,
        });

        let response = self
            .client
            .post(format!("{SLACK_API_BASE}/reactions.add"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "tool.slack.request_failed",
                    format!("Slack API request failed: {e}"),
                )
            })?;

        let resp_body: Value = response.json().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.slack.invalid_response",
                format!("Failed to parse Slack API response: {e}"),
            )
        })?;

        parse_slack_response(resp_body)?;
        Ok(json!({ "ok": true }))
    }
}

// ---------------------------------------------------------------------------
// slack_list_threads
// ---------------------------------------------------------------------------

/// List active threads in a Slack channel.
///
/// Calls the [`conversations.replies`](https://api.slack.com/methods/conversations.replies)
/// Slack Web API endpoint for each threaded message found in the channel history.
/// Returns thread root messages that have replies.
pub(crate) struct SlackListThreads {
    client: reqwest::Client,
    token_source: TokenSource,
}

impl SlackListThreads {
    pub fn new(client: reqwest::Client, token_source: TokenSource) -> Self {
        Self { client, token_source }
    }
}

#[async_trait]
impl Tool for SlackListThreads {
    fn name(&self) -> &str {
        "slack_list_threads"
    }

    fn description(&self) -> &str {
        "List active threads in a Slack channel. Returns messages that have thread replies."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The Slack channel ID to list threads from."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of messages to scan for threads (default 50, max 1000)."
                }
            },
            "required": ["channel"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let token = resolve_token(&self.token_source, &ctx).await?;

        let channel = args["channel"].as_str().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "tool.slack.missing_channel",
                "Missing required parameter 'channel'",
            )
        })?;
        let limit = args["limit"].as_u64().unwrap_or(50);

        // First, fetch channel history to find messages with threads
        let response = self
            .client
            .get(format!("{SLACK_API_BASE}/conversations.history"))
            .bearer_auth(&token)
            .query(&[("channel", channel), ("limit", &limit.to_string())])
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::Unavailable,
                    "tool.slack.request_failed",
                    format!("Slack API request failed: {e}"),
                )
            })?;

        let resp_body: Value = response.json().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "tool.slack.invalid_response",
                format!("Failed to parse Slack API response: {e}"),
            )
        })?;

        let resp = parse_slack_response(resp_body)?;

        // Filter messages that have thread replies (reply_count > 0)
        let threads: Vec<Value> = resp["messages"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter(|msg| msg["reply_count"].as_u64().is_some_and(|count| count > 0))
            .map(|msg| {
                json!({
                    "thread_ts": msg["ts"],
                    "text": msg["text"],
                    "user": msg["user"],
                    "reply_count": msg["reply_count"],
                    "latest_reply": msg["latest_reply"],
                })
            })
            .collect();

        Ok(json!({
            "ok": true,
            "threads": threads,
        }))
    }
}
