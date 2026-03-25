use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Approximate user location for Anthropic's web search tool.
#[derive(Debug, Clone, Default)]
pub struct WebSearchUserLocation {
    city: Option<String>,
    country: Option<String>,
    region: Option<String>,
    timezone: Option<String>,
}

impl WebSearchUserLocation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_city(mut self, city: impl Into<String>) -> Self {
        self.city = Some(city.into());
        self
    }

    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.country = Some(country.into());
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "type": "approximate",
            "city": self.city,
            "country": self.country,
            "region": self.region,
            "timezone": self.timezone,
        })
    }
}

/// WebSearch is a built-in tool for Anthropic Claude models that enables
/// server-side web search. The model searches the web internally and returns
/// results as ServerToolUse / WebSearchToolResult content blocks.
#[derive(Debug, Clone, Default)]
pub struct WebSearchTool {
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
    max_uses: Option<i32>,
    user_location: Option<WebSearchUserLocation>,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_allowed_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_domains = Some(domains.into_iter().map(Into::into).collect());
        self.blocked_domains = None;
        self
    }

    pub fn with_blocked_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.blocked_domains = Some(domains.into_iter().map(Into::into).collect());
        self.allowed_domains = None;
        self
    }

    pub fn with_max_uses(mut self, max_uses: i32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    pub fn with_user_location(mut self, user_location: WebSearchUserLocation) -> Self {
        self.user_location = Some(user_location);
        self
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Searches the web for current information (server-side)."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-anthropic-tool": {
                "type": "web_search_20250305",
                "name": "web_search",
                "allowed_domains": self.allowed_domains,
                "blocked_domains": self.blocked_domains,
                "max_uses": self.max_uses,
                "user_location": self.user_location.as_ref().map(WebSearchUserLocation::to_json),
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("WebSearch is handled internally by Anthropic"))
    }
}
