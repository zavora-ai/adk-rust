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
    /// Create a new empty user location.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the city name.
    pub fn with_city(mut self, city: impl Into<String>) -> Self {
        self.city = Some(city.into());
        self
    }

    /// Set the country name.
    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.country = Some(country.into());
        self
    }

    /// Set the region name.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Set the timezone identifier.
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
///
/// The tool declares `web_search_20250305` unless
/// [`with_dynamic_filtering`](Self::with_dynamic_filtering) selects
/// `web_search_20260209`.
#[derive(Debug, Clone, Default)]
pub struct WebSearchTool {
    allowed_domains: Option<Vec<String>>,
    blocked_domains: Option<Vec<String>>,
    max_uses: Option<i32>,
    user_location: Option<WebSearchUserLocation>,
    dynamic_filtering: bool,
    allowed_callers: Option<Vec<String>>,
}

impl WebSearchTool {
    /// Create a new `WebSearchTool` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict search results to the specified domains.
    pub fn with_allowed_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_domains = Some(domains.into_iter().map(Into::into).collect());
        self.blocked_domains = None;
        self
    }

    /// Block search results from the specified domains.
    pub fn with_blocked_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.blocked_domains = Some(domains.into_iter().map(Into::into).collect());
        self.allowed_domains = None;
        self
    }

    /// Set the maximum number of search invocations per turn.
    pub fn with_max_uses(mut self, max_uses: i32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Set the approximate user location for localized results.
    pub fn with_user_location(mut self, user_location: WebSearchUserLocation) -> Self {
        self.user_location = Some(user_location);
        self
    }

    /// Declare the `web_search_20260209` version, which filters results with code
    /// before they reach the context window.
    ///
    /// Supported on Claude Opus 5.5, Opus 5, Opus 4.8, Opus 4.7, Opus 4.6,
    /// Sonnet 5.5, Sonnet 5, and Sonnet 4.6. Do not combine it with a code
    /// execution tool in the same request.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::Tool;
    /// use adk_tool::WebSearchTool;
    ///
    /// let tool = WebSearchTool::new().with_dynamic_filtering().with_max_uses(3);
    /// let declaration = tool.declaration();
    /// assert_eq!(declaration["x-adk-anthropic-tool"]["type"], "web_search_20260209");
    /// ```
    pub fn with_dynamic_filtering(mut self) -> Self {
        self.dynamic_filtering = true;
        self
    }

    /// Set who may call the `web_search_20260209` tool selected by
    /// [`with_dynamic_filtering`](Self::with_dynamic_filtering).
    ///
    /// The API defaults that version to `["code_execution_20260120"]`, which filters
    /// results with code. `["direct"]` keeps the newer tool version but turns dynamic
    /// filtering off, which Zero Data Retention and models without programmatic tool
    /// calling require. `web_search_20250305` already defaults to direct calls, so the
    /// setting is sent only with dynamic filtering.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::Tool;
    /// use adk_tool::WebSearchTool;
    ///
    /// let tool = WebSearchTool::new().with_dynamic_filtering().with_allowed_callers(["direct"]);
    /// let declaration = tool.declaration();
    /// assert_eq!(
    ///     declaration["x-adk-anthropic-tool"]["allowed_callers"],
    ///     serde_json::json!(["direct"])
    /// );
    /// ```
    pub fn with_allowed_callers(
        mut self,
        callers: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_callers = Some(callers.into_iter().map(Into::into).collect());
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
        let allowed_callers = self.allowed_callers.as_ref().filter(|_| self.dynamic_filtering);
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-anthropic-tool": {
                "type": if self.dynamic_filtering { "web_search_20260209" } else { "web_search_20250305" },
                "name": "web_search",
                "allowed_domains": self.allowed_domains,
                "blocked_domains": self.blocked_domains,
                "max_uses": self.max_uses,
                "user_location": self.user_location.as_ref().map(WebSearchUserLocation::to_json),
                "allowed_callers": allowed_callers,
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("WebSearch is handled internally by Anthropic"))
    }
}
