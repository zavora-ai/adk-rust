use serde::{Deserialize, Serialize};

use crate::types::cache_control_ephemeral::CacheControlEphemeral;

/// Parameters for the user's location.
///
/// Used to provide more relevant search results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserLocation {
    /// Type of location data - currently only supports "approximate"
    #[serde(default = "default_type")]
    pub r#type: String,

    /// The city of the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,

    /// The two letter [ISO country code](https://en.wikipedia.org/wiki/ISO_3166-1_alpha-2) of the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,

    /// The region of the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,

    /// The [IANA timezone](https://nodatime.org/TimeZones) of the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

fn default_type() -> String {
    "approximate".to_string()
}

impl UserLocation {
    /// Creates a new UserLocation instance with default values
    pub fn new() -> Self {
        Self { r#type: default_type(), city: None, country: None, region: None, timezone: None }
    }

    /// Sets the city for the user location
    pub fn with_city(mut self, city: impl Into<String>) -> Self {
        self.city = Some(city.into());
        self
    }

    /// Sets the country for the user location
    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.country = Some(country.into());
        self
    }

    /// Sets the region for the user location
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Sets the timezone for the user location
    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }
}

impl Default for UserLocation {
    fn default() -> Self {
        Self::new()
    }
}

/// Parameters for the web search tool.
///
/// This tool allows the model to search the web for information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebSearchTool20250305 {
    /// Name of the tool. This is how the tool will be called by the model and in `tool_use` blocks.
    #[serde(default = "default_name")]
    pub name: String,

    /// If provided, only these domains will be included in results.
    ///
    /// Cannot be used alongside `blocked_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,

    /// If provided, these domains will never appear in results.
    ///
    /// Cannot be used alongside `allowed_domains`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Maximum number of times the tool can be used in the API request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,

    /// Parameters for the user's location. Used to provide more relevant search results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<UserLocation>,
}

fn default_name() -> String {
    "web_search".to_string()
}

impl WebSearchTool20250305 {
    /// Creates a new WebSearchTool20250305 instance with default values
    pub fn new() -> Self {
        Self {
            name: default_name(),
            allowed_domains: None,
            blocked_domains: None,
            cache_control: None,
            max_uses: None,
            user_location: None,
        }
    }

    /// Sets the allowed domains for the web search
    ///
    /// If provided, only these domains will be included in results.
    /// Cannot be used alongside `blocked_domains`.
    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = Some(domains);
        self.blocked_domains = None; // Reset blocked_domains as they can't be used together
        self
    }

    /// Sets the blocked domains for the web search
    ///
    /// If provided, these domains will never appear in results.
    /// Cannot be used alongside `allowed_domains`.
    pub fn with_blocked_domains(mut self, domains: Vec<String>) -> Self {
        self.blocked_domains = Some(domains);
        self.allowed_domains = None; // Reset allowed_domains as they can't be used together
        self
    }

    /// Sets the cache control for the web search
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Sets the maximum number of times the tool can be used in the API request
    pub fn with_max_uses(mut self, max_uses: i32) -> Self {
        self.max_uses = Some(max_uses);
        self
    }

    /// Sets the user location for the web search
    pub fn with_user_location(mut self, user_location: UserLocation) -> Self {
        self.user_location = Some(user_location);
        self
    }
}

impl Default for WebSearchTool20250305 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_location_serialization() {
        let user_location = UserLocation::new()
            .with_city("San Francisco")
            .with_country("US")
            .with_region("California")
            .with_timezone("America/Los_Angeles");

        let json = serde_json::to_string(&user_location).unwrap();
        let expected = r#"{"type":"approximate","city":"San Francisco","country":"US","region":"California","timezone":"America/Los_Angeles"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn user_location_deserialization() {
        let json = r#"{
            "type": "approximate",
            "city": "San Francisco",
            "country": "US",
            "region": "California",
            "timezone": "America/Los_Angeles"
        }"#;

        let user_location: UserLocation = serde_json::from_str(json).unwrap();

        assert_eq!(user_location.r#type, "approximate");
        assert_eq!(user_location.city, Some("San Francisco".to_string()));
        assert_eq!(user_location.country, Some("US".to_string()));
        assert_eq!(user_location.region, Some("California".to_string()));
        assert_eq!(user_location.timezone, Some("America/Los_Angeles".to_string()));
    }

    #[test]
    fn web_search_tool_serialization() {
        let user_location = UserLocation::new().with_city("San Francisco").with_country("US");

        let web_search_tool = WebSearchTool20250305::new()
            .with_allowed_domains(vec!["example.com".to_string(), "example.org".to_string()])
            .with_max_uses(5)
            .with_user_location(user_location)
            .with_cache_control(CacheControlEphemeral::new());

        let json = serde_json::to_string(&web_search_tool).unwrap();
        let expected = r#"{"name":"web_search","allowed_domains":["example.com","example.org"],"cache_control":{"type":"ephemeral"},"max_uses":5,"user_location":{"type":"approximate","city":"San Francisco","country":"US"}}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn web_search_tool_deserialization() {
        let json = r#"{
            "name": "web_search",
            "allowed_domains": ["example.com", "example.org"],
            "cache_control": {"type": "ephemeral"},
            "max_uses": 5,
            "user_location": {
                "type": "approximate",
                "city": "San Francisco",
                "country": "US"
            }
        }"#;

        let web_search_tool: WebSearchTool20250305 = serde_json::from_str(json).unwrap();

        assert_eq!(web_search_tool.name, "web_search");
        assert_eq!(
            web_search_tool.allowed_domains,
            Some(vec!["example.com".to_string(), "example.org".to_string()])
        );
        assert_eq!(web_search_tool.blocked_domains, None);
        assert_eq!(web_search_tool.max_uses, Some(5));
        assert!(web_search_tool.cache_control.is_some());
        assert!(web_search_tool.user_location.is_some());

        let user_location = web_search_tool.user_location.unwrap();
        assert_eq!(user_location.city, Some("San Francisco".to_string()));
        assert_eq!(user_location.country, Some("US".to_string()));
    }

    #[test]
    fn allowed_blocked_domains_mutual_exclusivity() {
        // Test that setting allowed_domains clears blocked_domains
        let mut web_search_tool =
            WebSearchTool20250305::new().with_blocked_domains(vec!["blocked.com".to_string()]);

        // Verify blocked_domains is set
        assert!(web_search_tool.blocked_domains.is_some());
        assert!(web_search_tool.allowed_domains.is_none());

        // Now set allowed_domains
        web_search_tool = web_search_tool.with_allowed_domains(vec!["allowed.com".to_string()]);

        // Verify blocked_domains is cleared
        assert!(web_search_tool.allowed_domains.is_some());
        assert!(web_search_tool.blocked_domains.is_none());

        // Now test the reverse
        web_search_tool = web_search_tool.with_blocked_domains(vec!["blocked.com".to_string()]);

        // Verify allowed_domains is cleared
        assert!(web_search_tool.blocked_domains.is_some());
        assert!(web_search_tool.allowed_domains.is_none());
    }
}
