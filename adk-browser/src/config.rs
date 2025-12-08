//! Browser configuration options.

use serde::{Deserialize, Serialize};

/// Configuration for browser sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    /// WebDriver server URL (e.g., "http://localhost:4444")
    pub webdriver_url: String,

    /// Browser type to use
    pub browser: BrowserType,

    /// Run in headless mode (no visible window)
    pub headless: bool,

    /// Viewport width in pixels
    pub viewport_width: u32,

    /// Viewport height in pixels
    pub viewport_height: u32,

    /// Page load timeout in seconds
    pub page_load_timeout_secs: u64,

    /// Script execution timeout in seconds
    pub script_timeout_secs: u64,

    /// Implicit wait timeout in seconds
    pub implicit_wait_secs: u64,

    /// User agent string override
    pub user_agent: Option<String>,

    /// Additional browser arguments
    pub browser_args: Vec<String>,
}

/// Supported browser types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserType {
    Chrome,
    Firefox,
    Safari,
    Edge,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            webdriver_url: "http://localhost:4444".to_string(),
            browser: BrowserType::Chrome,
            headless: true,
            viewport_width: 1920,
            viewport_height: 1080,
            page_load_timeout_secs: 30,
            script_timeout_secs: 30,
            implicit_wait_secs: 10,
            user_agent: None,
            browser_args: Vec::new(),
        }
    }
}

impl BrowserConfig {
    /// Create a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the WebDriver URL.
    pub fn webdriver_url(mut self, url: impl Into<String>) -> Self {
        self.webdriver_url = url.into();
        self
    }

    /// Set the browser type.
    pub fn browser(mut self, browser: BrowserType) -> Self {
        self.browser = browser;
        self
    }

    /// Enable or disable headless mode.
    pub fn headless(mut self, headless: bool) -> Self {
        self.headless = headless;
        self
    }

    /// Set the viewport size.
    pub fn viewport(mut self, width: u32, height: u32) -> Self {
        self.viewport_width = width;
        self.viewport_height = height;
        self
    }

    /// Set page load timeout.
    pub fn page_load_timeout(mut self, secs: u64) -> Self {
        self.page_load_timeout_secs = secs;
        self
    }

    /// Set a custom user agent.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Add a browser argument.
    pub fn add_arg(mut self, arg: impl Into<String>) -> Self {
        self.browser_args.push(arg.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BrowserConfig::default();
        assert_eq!(config.webdriver_url, "http://localhost:4444");
        assert!(config.headless);
        assert_eq!(config.viewport_width, 1920);
    }

    #[test]
    fn test_builder() {
        let config =
            BrowserConfig::new().browser(BrowserType::Firefox).headless(false).viewport(1280, 720);

        assert_eq!(config.browser, BrowserType::Firefox);
        assert!(!config.headless);
        assert_eq!(config.viewport_width, 1280);
    }
}
