//! Browser session management wrapping thirtyfour WebDriver.

use crate::config::{BrowserConfig, BrowserType};
use adk_core::{AdkError, Result};
use std::sync::Arc;
use std::time::Duration;
use thirtyfour::common::print::{PrintOrientation, PrintParameters};
use thirtyfour::prelude::*;
use tokio::sync::RwLock;

/// State information about an element.
#[derive(Debug, Clone)]
pub struct ElementState {
    /// Whether the element is displayed (visible).
    pub is_displayed: bool,
    /// Whether the element is enabled (not disabled).
    pub is_enabled: bool,
    /// Whether the element is selected (for checkboxes, radio buttons, options).
    pub is_selected: bool,
    /// Whether the element is clickable (displayed and enabled).
    pub is_clickable: bool,
}

/// A browser session that wraps thirtyfour's WebDriver.
///
/// This is the core abstraction for browser automation in ADK.
/// It can be shared across multiple tools via `Arc<BrowserSession>`.
pub struct BrowserSession {
    driver: RwLock<Option<WebDriver>>,
    config: BrowserConfig,
}

impl BrowserSession {
    /// Create a new browser session with the given configuration.
    ///
    /// Note: This does not start the browser immediately.
    /// Call `start()` to initialize the WebDriver connection.
    pub fn new(config: BrowserConfig) -> Self {
        Self { driver: RwLock::new(None), config }
    }

    /// Create a browser session with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(BrowserConfig::default())
    }

    /// Start the browser session by connecting to WebDriver.
    pub async fn start(&self) -> Result<()> {
        let mut driver_guard = self.driver.write().await;

        if driver_guard.is_some() {
            return Ok(()); // Already started
        }

        let caps = self.build_capabilities()?;
        let driver = WebDriver::new(&self.config.webdriver_url, caps)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to start browser: {}", e)))?;

        // Set timeouts
        driver
            .set_page_load_timeout(Duration::from_secs(self.config.page_load_timeout_secs))
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to set page load timeout: {}", e)))?;

        driver
            .set_script_timeout(Duration::from_secs(self.config.script_timeout_secs))
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to set script timeout: {}", e)))?;

        driver
            .set_implicit_wait_timeout(Duration::from_secs(self.config.implicit_wait_secs))
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to set implicit wait: {}", e)))?;

        // Set viewport size
        driver
            .set_window_rect(0, 0, self.config.viewport_width, self.config.viewport_height)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to set viewport: {}", e)))?;

        *driver_guard = Some(driver);
        Ok(())
    }

    /// Stop the browser session.
    pub async fn stop(&self) -> Result<()> {
        let mut driver_guard = self.driver.write().await;

        if let Some(driver) = driver_guard.take() {
            driver
                .quit()
                .await
                .map_err(|e| AdkError::Tool(format!("Failed to quit browser: {}", e)))?;
        }

        Ok(())
    }

    /// Check if the session is active.
    pub async fn is_active(&self) -> bool {
        self.driver.read().await.is_some()
    }

    /// Get the configuration.
    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }

    /// Navigate to a URL.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver.goto(url).await.map_err(|e| AdkError::Tool(format!("Navigation failed: {}", e)))?;

        Ok(())
    }

    /// Get the current URL.
    pub async fn current_url(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .current_url()
            .await
            .map(|u| u.to_string())
            .map_err(|e| AdkError::Tool(format!("Failed to get URL: {}", e)))
    }

    /// Get the page title.
    pub async fn title(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver.title().await.map_err(|e| AdkError::Tool(format!("Failed to get title: {}", e)))
    }

    /// Find an element by CSS selector.
    pub async fn find_element(&self, selector: &str) -> Result<WebElement> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .find(By::Css(selector))
            .await
            .map_err(|e| AdkError::Tool(format!("Element not found '{}': {}", selector, e)))
    }

    /// Find multiple elements by CSS selector.
    pub async fn find_elements(&self, selector: &str) -> Result<Vec<WebElement>> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .find_all(By::Css(selector))
            .await
            .map_err(|e| AdkError::Tool(format!("Elements query failed '{}': {}", selector, e)))
    }

    /// Find element by XPath.
    pub async fn find_by_xpath(&self, xpath: &str) -> Result<WebElement> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .find(By::XPath(xpath))
            .await
            .map_err(|e| AdkError::Tool(format!("XPath not found '{}': {}", xpath, e)))
    }

    /// Click an element by selector.
    pub async fn click(&self, selector: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        element
            .click()
            .await
            .map_err(|e| AdkError::Tool(format!("Click failed on '{}': {}", selector, e)))
    }

    /// Type text into an element.
    pub async fn type_text(&self, selector: &str, text: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        element
            .send_keys(text)
            .await
            .map_err(|e| AdkError::Tool(format!("Type failed on '{}': {}", selector, e)))
    }

    /// Clear an input field.
    pub async fn clear(&self, selector: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        element
            .clear()
            .await
            .map_err(|e| AdkError::Tool(format!("Clear failed on '{}': {}", selector, e)))
    }

    /// Get text content of an element.
    pub async fn get_text(&self, selector: &str) -> Result<String> {
        let element = self.find_element(selector).await?;
        element
            .text()
            .await
            .map_err(|e| AdkError::Tool(format!("Get text failed on '{}': {}", selector, e)))
    }

    /// Get an attribute value.
    pub async fn get_attribute(&self, selector: &str, attribute: &str) -> Result<Option<String>> {
        let element = self.find_element(selector).await?;
        element
            .attr(attribute)
            .await
            .map_err(|e| AdkError::Tool(format!("Get attribute failed: {}", e)))
    }

    /// Take a screenshot (returns base64-encoded PNG).
    pub async fn screenshot(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let screenshot = driver
            .screenshot_as_png_base64()
            .await
            .map_err(|e| AdkError::Tool(format!("Screenshot failed: {}", e)))?;

        Ok(screenshot)
    }

    /// Take a screenshot of a specific element.
    pub async fn screenshot_element(&self, selector: &str) -> Result<String> {
        let element = self.find_element(selector).await?;
        let screenshot = element
            .screenshot_as_png_base64()
            .await
            .map_err(|e| AdkError::Tool(format!("Element screenshot failed: {}", e)))?;

        Ok(screenshot)
    }

    /// Execute JavaScript and return result.
    pub async fn execute_script(&self, script: &str) -> Result<serde_json::Value> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let result = driver
            .execute(script, vec![])
            .await
            .map_err(|e| AdkError::Tool(format!("Script execution failed: {}", e)))?;

        // thirtyfour's ScriptRet provides .json() which returns &Value directly
        Ok(result.json().clone())
    }

    /// Execute async JavaScript and return result.
    pub async fn execute_async_script(&self, script: &str) -> Result<serde_json::Value> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let result = driver
            .execute_async(script, vec![])
            .await
            .map_err(|e| AdkError::Tool(format!("Async script failed: {}", e)))?;

        Ok(result.json().clone())
    }

    /// Wait for an element to be present.
    pub async fn wait_for_element(&self, selector: &str, timeout_secs: u64) -> Result<WebElement> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .query(By::Css(selector))
            .wait(Duration::from_secs(timeout_secs), Duration::from_millis(100))
            .first()
            .await
            .map_err(|e| {
                AdkError::Tool(format!(
                    "Timeout waiting for '{}' after {}s: {}",
                    selector, timeout_secs, e
                ))
            })
    }

    /// Wait for an element to be clickable.
    pub async fn wait_for_clickable(
        &self,
        selector: &str,
        timeout_secs: u64,
    ) -> Result<WebElement> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .query(By::Css(selector))
            .wait(Duration::from_secs(timeout_secs), Duration::from_millis(100))
            .and_clickable()
            .first()
            .await
            .map_err(|e| {
                AdkError::Tool(format!("Timeout waiting for clickable '{}': {}", selector, e))
            })
    }

    /// Get page source HTML.
    pub async fn page_source(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .source()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to get page source: {}", e)))
    }

    /// Go back in history.
    pub async fn back(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver.back().await.map_err(|e| AdkError::Tool(format!("Back navigation failed: {}", e)))
    }

    /// Go forward in history.
    pub async fn forward(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .forward()
            .await
            .map_err(|e| AdkError::Tool(format!("Forward navigation failed: {}", e)))
    }

    /// Refresh the current page.
    pub async fn refresh(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver.refresh().await.map_err(|e| AdkError::Tool(format!("Refresh failed: {}", e)))
    }

    // =========================================================================
    // Cookie Management
    // =========================================================================

    /// Get all cookies.
    pub async fn get_all_cookies(&self) -> Result<Vec<serde_json::Value>> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let cookies = driver
            .get_all_cookies()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to get cookies: {}", e)))?;

        Ok(cookies
            .into_iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "value": c.value,
                    "domain": c.domain,
                    "path": c.path,
                    "secure": c.secure,
                })
            })
            .collect())
    }

    /// Get a cookie by name.
    pub async fn get_cookie(&self, name: &str) -> Result<serde_json::Value> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let cookie = driver
            .get_named_cookie(name)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to get cookie '{}': {}", name, e)))?;

        Ok(serde_json::json!({
            "name": cookie.name,
            "value": cookie.value,
            "domain": cookie.domain,
            "path": cookie.path,
            "secure": cookie.secure,
        }))
    }

    /// Add a cookie.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_cookie(
        &self,
        name: &str,
        value: &str,
        domain: Option<&str>,
        path: Option<&str>,
        secure: Option<bool>,
        _expiry: Option<i64>,
    ) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let mut cookie = thirtyfour::Cookie::new(name, value);
        if let Some(d) = domain {
            cookie.set_domain(d);
        }
        if let Some(p) = path {
            cookie.set_path(p);
        }
        if let Some(s) = secure {
            cookie.set_secure(s);
        }

        driver
            .add_cookie(cookie)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to add cookie: {}", e)))
    }

    /// Delete a cookie.
    pub async fn delete_cookie(&self, name: &str) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .delete_cookie(name)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to delete cookie: {}", e)))
    }

    /// Delete all cookies.
    pub async fn delete_all_cookies(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .delete_all_cookies()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to delete all cookies: {}", e)))
    }

    // =========================================================================
    // Window Management
    // =========================================================================

    /// List all windows/tabs.
    pub async fn list_windows(&self) -> Result<(Vec<String>, String)> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let windows = driver
            .windows()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to get windows: {}", e)))?;

        let current = driver
            .window()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to get current window: {}", e)))?;

        Ok((windows.into_iter().map(|w| w.to_string()).collect(), current.to_string()))
    }

    /// Open a new tab.
    pub async fn new_tab(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let handle = driver
            .new_tab()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to open new tab: {}", e)))?;

        Ok(handle.to_string())
    }

    /// Open a new window.
    pub async fn new_window(&self) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let handle = driver
            .new_window()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to open new window: {}", e)))?;

        Ok(handle.to_string())
    }

    /// Switch to a window by handle.
    pub async fn switch_to_window(&self, handle: &str) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let window_handle = thirtyfour::WindowHandle::from(handle.to_string());
        driver
            .switch_to_window(window_handle)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to switch window: {}", e)))
    }

    /// Close the current window.
    pub async fn close_window(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .close_window()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to close window: {}", e)))
    }

    /// Maximize window.
    pub async fn maximize_window(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .maximize_window()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to maximize window: {}", e)))
    }

    /// Minimize window.
    pub async fn minimize_window(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .minimize_window()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to minimize window: {}", e)))
    }

    /// Set window size and position.
    pub async fn set_window_rect(&self, x: i32, y: i32, width: u32, height: u32) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .set_window_rect(x as i64, y as i64, width, height)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to set window rect: {}", e)))
    }

    // =========================================================================
    // Frame Management
    // =========================================================================

    /// Switch to frame by index.
    pub async fn switch_to_frame_by_index(&self, index: u16) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .enter_frame(index)
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to switch to frame {}: {}", index, e)))
    }

    /// Switch to frame by selector.
    pub async fn switch_to_frame_by_selector(&self, selector: &str) -> Result<()> {
        let element = self.find_element(selector).await?;

        element
            .enter_frame()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to switch to frame: {}", e)))
    }

    /// Switch to parent frame.
    pub async fn switch_to_parent_frame(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .enter_parent_frame()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to switch to parent frame: {}", e)))
    }

    /// Switch to default content.
    pub async fn switch_to_default_content(&self) -> Result<()> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        driver
            .enter_default_frame()
            .await
            .map_err(|e| AdkError::Tool(format!("Failed to switch to default content: {}", e)))
    }

    // =========================================================================
    // Advanced Actions
    // =========================================================================

    /// Drag and drop.
    pub async fn drag_and_drop(&self, source_selector: &str, target_selector: &str) -> Result<()> {
        let source = self.find_element(source_selector).await?;
        let target = self.find_element(target_selector).await?;

        // Use JavaScript-based drag and drop for broader compatibility
        source
            .js_drag_to(&target)
            .await
            .map_err(|e| AdkError::Tool(format!("Drag and drop failed: {}", e)))
    }

    /// Right-click (context click).
    pub async fn right_click(&self, selector: &str) -> Result<()> {
        let script = format!(
            r#"
            var element = document.querySelector('{}');
            if (element) {{
                var event = new MouseEvent('contextmenu', {{
                    'view': window,
                    'bubbles': true,
                    'cancelable': true
                }});
                element.dispatchEvent(event);
                return true;
            }}
            return false;
            "#,
            selector.replace('\'', "\\'")
        );

        let result = self.execute_script(&script).await?;
        if result.as_bool() != Some(true) {
            return Err(AdkError::Tool(format!("Element not found: {}", selector)));
        }
        Ok(())
    }

    /// Focus an element.
    pub async fn focus_element(&self, selector: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        element.focus().await.map_err(|e| AdkError::Tool(format!("Focus failed: {}", e)))
    }

    /// Get element state.
    pub async fn get_element_state(&self, selector: &str) -> Result<ElementState> {
        let element = self.find_element(selector).await?;

        let is_displayed = element.is_displayed().await.unwrap_or(false);
        let is_enabled = element.is_enabled().await.unwrap_or(false);
        let is_selected = element.is_selected().await.unwrap_or(false);
        let is_clickable = element.is_clickable().await.unwrap_or(false);

        Ok(ElementState { is_displayed, is_enabled, is_selected, is_clickable })
    }

    /// Press a key.
    pub async fn press_key(
        &self,
        key: &str,
        selector: Option<&str>,
        _modifiers: &[&str],
    ) -> Result<()> {
        let key_str = match key.to_lowercase().as_str() {
            "enter" => "\u{E007}",
            "tab" => "\u{E004}",
            "escape" | "esc" => "\u{E00C}",
            "backspace" => "\u{E003}",
            "delete" => "\u{E017}",
            "arrowup" | "up" => "\u{E013}",
            "arrowdown" | "down" => "\u{E015}",
            "arrowleft" | "left" => "\u{E012}",
            "arrowright" | "right" => "\u{E014}",
            "home" => "\u{E011}",
            "end" => "\u{E010}",
            "pageup" => "\u{E00E}",
            "pagedown" => "\u{E00F}",
            "space" => " ",
            _ => key,
        };

        if let Some(sel) = selector {
            let element = self.find_element(sel).await?;
            element
                .send_keys(key_str)
                .await
                .map_err(|e| AdkError::Tool(format!("Key press failed: {}", e)))?;
        } else {
            // Send to active element via JavaScript
            let script = format!(
                "document.activeElement.dispatchEvent(new KeyboardEvent('keydown', {{'key': '{}'}}));",
                key
            );
            self.execute_script(&script).await?;
        }

        Ok(())
    }

    /// Upload a file.
    pub async fn upload_file(&self, selector: &str, file_path: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        element
            .send_keys(file_path)
            .await
            .map_err(|e| AdkError::Tool(format!("File upload failed: {}", e)))
    }

    /// Print page to PDF.
    pub async fn print_to_pdf(&self, landscape: bool, scale: f64) -> Result<String> {
        let driver_guard = self.driver.read().await;
        let driver = driver_guard
            .as_ref()
            .ok_or_else(|| AdkError::Tool("Browser session not started".to_string()))?;

        let params = PrintParameters {
            orientation: if landscape {
                PrintOrientation::Landscape
            } else {
                PrintOrientation::Portrait
            },
            scale,
            ..Default::default()
        };

        driver
            .print_page_base64(params)
            .await
            .map_err(|e| AdkError::Tool(format!("Print to PDF failed: {}", e)))
    }

    /// Build browser capabilities based on configuration.
    fn build_capabilities(&self) -> Result<Capabilities> {
        let caps = match self.config.browser {
            BrowserType::Chrome => {
                let mut caps = DesiredCapabilities::chrome();
                if self.config.headless {
                    caps.add_arg("--headless=new").map_err(|e| {
                        AdkError::Tool(format!("Failed to add headless arg: {}", e))
                    })?;
                }
                caps.add_arg("--no-sandbox")
                    .map_err(|e| AdkError::Tool(format!("Failed to add no-sandbox: {}", e)))?;
                caps.add_arg("--disable-dev-shm-usage")
                    .map_err(|e| AdkError::Tool(format!("Failed to add disable-dev-shm: {}", e)))?;

                if let Some(ref ua) = self.config.user_agent {
                    caps.add_arg(&format!("--user-agent={}", ua))
                        .map_err(|e| AdkError::Tool(format!("Failed to add user-agent: {}", e)))?;
                }

                for arg in &self.config.browser_args {
                    caps.add_arg(arg).map_err(|e| {
                        AdkError::Tool(format!("Failed to add arg '{}': {}", arg, e))
                    })?;
                }

                caps.into()
            }
            BrowserType::Firefox => {
                let mut caps = DesiredCapabilities::firefox();
                if self.config.headless {
                    caps.add_arg("-headless")
                        .map_err(|e| AdkError::Tool(format!("Failed to add headless: {}", e)))?;
                }
                caps.into()
            }
            BrowserType::Safari => DesiredCapabilities::safari().into(),
            BrowserType::Edge => {
                let mut caps = DesiredCapabilities::edge();
                if self.config.headless {
                    caps.add_arg("--headless")
                        .map_err(|e| AdkError::Tool(format!("Failed to add headless: {}", e)))?;
                }
                caps.into()
            }
        };

        Ok(caps)
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // Note: Can't do async cleanup in Drop, but thirtyfour handles this gracefully
        tracing::debug!("BrowserSession dropped");
    }
}

/// Create a shared browser session.
pub fn shared_session(config: BrowserConfig) -> Arc<BrowserSession> {
    Arc::new(BrowserSession::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation() {
        let session = BrowserSession::with_defaults();
        assert!(!session.config().headless || session.config().headless); // Always true, just testing creation
    }

    #[tokio::test]
    async fn test_session_not_started() {
        let session = BrowserSession::with_defaults();
        assert!(!session.is_active().await);

        let result = session.navigate("https://example.com").await;
        assert!(result.is_err());
    }
}
