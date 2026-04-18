//! Slack toolset structure and `Toolset` trait implementation.

use crate::slack::tools::{SlackAddReaction, SlackListThreads, SlackReadChannel, SlackSendMessage};
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::sync::Arc;

/// How the Slack Bot Token is resolved at runtime.
#[derive(Debug, Clone)]
pub enum TokenSource {
    /// A token provided directly at construction time.
    Direct(String),
    /// A secret name resolved via `ToolContext::get_secret()` at execution time.
    SecretRef(String),
}

/// Native Slack toolset providing channel messaging, reactions, and thread tools.
///
/// Authenticates using a Slack Bot Token supplied either directly or via the
/// configured [`SecretProvider`](adk_core::ToolContext::get_secret).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::slack::SlackToolset;
///
/// // Direct token
/// let toolset = SlackToolset::new("xoxb-your-bot-token");
///
/// // Via secret provider (resolved at tool execution time)
/// let toolset = SlackToolset::from_secret("slack-bot-token");
/// ```
pub struct SlackToolset {
    pub(crate) client: reqwest::Client,
    pub(crate) token_source: TokenSource,
}

impl SlackToolset {
    /// Create a new `SlackToolset` with a direct Slack Bot Token.
    pub fn new(token: impl Into<String>) -> Self {
        Self { client: reqwest::Client::new(), token_source: TokenSource::Direct(token.into()) }
    }

    /// Create a new `SlackToolset` that resolves the token from the secret
    /// provider at execution time via `ctx.get_secret(secret_name)`.
    pub fn from_secret(secret_name: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token_source: TokenSource::SecretRef(secret_name.into()),
        }
    }
}

#[async_trait]
impl Toolset for SlackToolset {
    fn name(&self) -> &str {
        "slack"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let client = self.client.clone();
        let token_source = self.token_source.clone();

        Ok(vec![
            Arc::new(SlackSendMessage::new(client.clone(), token_source.clone())),
            Arc::new(SlackReadChannel::new(client.clone(), token_source.clone())),
            Arc::new(SlackAddReaction::new(client.clone(), token_source.clone())),
            Arc::new(SlackListThreads::new(client, token_source)),
        ])
    }
}
