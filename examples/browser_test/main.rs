//! Browser Session Integration Test
//!
//! Tests BrowserSession methods directly to verify WebDriver connectivity.

use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Browser Session Integration Test ===\n");

    // Check WebDriver
    let webdriver_url =
        std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:4444".to_string());

    let available = reqwest::Client::new()
        .get(format!("{}/status", webdriver_url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    if !available {
        println!("WebDriver not available at {}", webdriver_url);
        println!("Start with: docker run -d -p 4444:4444 selenium/standalone-chrome");
        return Ok(());
    }

    println!("WebDriver available at {}\n", webdriver_url);

    // Setup browser
    let config =
        BrowserConfig::new().webdriver_url(&webdriver_url).headless(true).viewport(1280, 720);

    let browser = Arc::new(BrowserSession::new(config));
    browser.start().await?;
    println!("Browser session started\n");

    // Show tool count
    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();
    println!("Available browser tools: {}\n", tools.len());

    // =========================================================================
    // Test 1: Navigate
    // =========================================================================
    println!("Test 1: Navigate to example.com");
    browser.navigate("https://example.com").await?;
    println!("  OK\n");

    // =========================================================================
    // Test 2: Get Page Info
    // =========================================================================
    println!("Test 2: Get page info");
    let title = browser.title().await?;
    let url = browser.current_url().await?;
    println!("  Title: {}", title);

    println!("  URL: {}\n", url);

    // =========================================================================
    // Test 3: Extract Text
    // =========================================================================
    println!("Test 3: Extract heading text");
    let text = browser.get_text("h1").await?;
    println!("  H1: {}\n", text);

    // =========================================================================
    // Test 4: Find Elements
    // =========================================================================
    println!("Test 4: Find elements");
    let links = browser.find_elements("a").await?;
    println!("  Found {} links\n", links.len());

    // =========================================================================
    // Test 5: Screenshot
    // =========================================================================
    println!("Test 5: Take screenshot");
    let screenshot = browser.screenshot().await?;
    println!("  Screenshot captured: {} bytes\n", screenshot.len());

    // =========================================================================
    // Test 6: Execute JavaScript
    // =========================================================================
    println!("Test 6: Execute JavaScript");
    let result = browser.execute_script("return document.querySelectorAll('p').length").await?;
    println!("  Paragraph count: {}\n", result);

    // =========================================================================
    // Test 7: Window Management
    // =========================================================================
    println!("Test 7: Window management");
    let (windows, current) = browser.list_windows().await?;
    println!("  Windows: {}", windows.len());
    println!("  Current: {}\n", current);

    // =========================================================================
    // Test 8: New Tab
    // =========================================================================
    println!("Test 8: New tab");
    let new_handle = browser.new_tab().await?;
    println!("  New tab: {}", new_handle);
    // Navigate to a fast page
    browser.navigate("about:blank").await?;
    let new_title = browser.title().await?;
    println!("  New tab title: '{}'\n", new_title);

    // Switch back
    let (all_windows, _) = browser.list_windows().await?;
    println!("  Total windows now: {}", all_windows.len());

    // =========================================================================
    // Test 9: Cookies
    // =========================================================================
    println!("\nTest 9: Cookie management");
    // Navigate to a real domain first (cookies require a valid domain)
    browser.navigate("https://example.com").await?;
    browser.add_cookie("test_cookie", "adk_test_value", None, None, None, None).await?;
    println!("  Added cookie: test_cookie");
    let cookies = browser.get_all_cookies().await?;
    println!("  Total cookies: {}\n", cookies.len());

    // =========================================================================
    // Test 10: Element State
    // =========================================================================
    println!("Test 10: Element state");
    browser.navigate("https://example.com").await?;
    let state = browser.get_element_state("h1").await?;
    println!("  h1 is_displayed: {}", state.is_displayed);
    println!("  h1 is_enabled: {}", state.is_enabled);
    println!("  h1 is_clickable: {}\n", state.is_clickable);

    // =========================================================================
    // Test 11: Scroll
    // =========================================================================
    println!("Test 11: Scroll");
    browser.execute_script("window.scrollTo(0, 100)").await?;
    let scroll_y = browser.execute_script("return window.scrollY").await?;
    println!("  Scrolled to Y: {}\n", scroll_y);

    // =========================================================================
    // Test 12: Wait for element
    // =========================================================================
    println!("Test 12: Wait for element");
    let element = browser.wait_for_element("h1", 5).await?;
    let h1_text = element.text().await?;
    println!("  Found h1 with text: {}\n", h1_text);

    // =========================================================================
    // Cleanup
    // =========================================================================
    browser.stop().await?;
    println!("Browser session stopped");
    println!("\n=== All 12 Tests Passed ===");

    Ok(())
}
