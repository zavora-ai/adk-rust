/// Provider-level configuration for regex-based dynamic tool discovery.
///
/// `ToolSearchConfig` allows filtering tools by name using a regex pattern.
/// When set on an Anthropic provider, only tools whose names match the pattern
/// are loaded per request. When not set, all available tools are loaded.
///
/// # Example
///
/// ```
/// use adk_anthropic::ToolSearchConfig;
///
/// let config = ToolSearchConfig::new("^(search|fetch)_.*");
/// assert!(config.matches("search_web").unwrap());
/// assert!(!config.matches("delete_all").unwrap());
/// ```
#[derive(Debug, Clone)]
pub struct ToolSearchConfig {
    /// Regex pattern for matching tool names.
    pub pattern: String,
}

impl ToolSearchConfig {
    /// Create a new `ToolSearchConfig` with the given regex pattern.
    ///
    /// The pattern is compiled on each call to [`matches`](Self::matches).
    /// An invalid regex will produce an error at match time.
    pub fn new(pattern: impl Into<String>) -> Self {
        Self { pattern: pattern.into() }
    }

    /// Check whether `tool_name` matches the configured regex pattern.
    ///
    /// # Errors
    ///
    /// Returns an error if the pattern is not a valid regex.
    pub fn matches(&self, tool_name: &str) -> Result<bool, regex::Error> {
        let re = regex::Regex::new(&self.pattern)?;
        Ok(re.is_match(tool_name))
    }
}
