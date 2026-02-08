# Browser Tools

The `adk-browser` crate provides 46 comprehensive browser automation tools that enable AI agents to interact with web pages. Built on the WebDriver protocol (Selenium), it works with any WebDriver-compatible browser.

## Overview

Browser tools allow agents to:

- Navigate web pages and manage browser history
- Extract text, links, images, and structured data
- Fill forms and interact with page elements
- Take screenshots and generate PDFs
- Execute JavaScript for advanced automation
- Manage cookies, frames, and multiple windows

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-browser = "0.3.0"
adk-agent = "0.3.0"
adk-model = "0.3.0"
```

### Prerequisites

Start a WebDriver server:

```bash
# Using Docker (recommended)
docker run -d -p 4444:4444 -p 7900:7900 --shm-size=2g selenium/standalone-chrome:latest

# Or use ChromeDriver directly
chromedriver --port=4444
```

### Basic Usage

```rust
use adk_browser::{BrowserSession, BrowserToolset, BrowserConfig};
use adk_agent::LlmAgentBuilder;
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure browser session
    let config = BrowserConfig::new()
        .webdriver_url("http://localhost:4444")
        .headless(true)
        .viewport(1920, 1080);

    // Create and start browser session
    let browser = Arc::new(BrowserSession::new(config));
    browser.start().await?;

    // Create toolset with all 46 tools
    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();

    // Create AI agent with browser tools
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    let mut builder = LlmAgentBuilder::new("web_agent")
        .model(model)
        .instruction("You are a web automation assistant. Use browser tools to help users.");

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    // Clean up when done
    browser.stop().await?;

    Ok(())
}
```

## Filtered Tools

Select only the tools your agent needs:

```rust
let toolset = BrowserToolset::new(browser)
    .with_navigation(true)   // navigate, back, forward, refresh
    .with_extraction(true)   // extract_text, extract_attribute, extract_links, page_info, page_source
    .with_interaction(true)  // click, double_click, type, clear, select
    .with_wait(true)         // wait_for_element, wait, wait_for_page_load, wait_for_text
    .with_screenshot(true)   // screenshot
    .with_js(true)           // evaluate_js, scroll, hover, handle_alert
    .with_cookies(false)     // Disable cookie tools
    .with_frames(false)      // Disable frame tools
    .with_windows(false)     // Disable window tools
    .with_actions(false);    // Disable advanced actions

let tools = toolset.all_tools();
```

## Available Tools (46 Total)

### Navigation (4 tools)

| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL |
| `browser_back` | Go back in history |
| `browser_forward` | Go forward in history |
| `browser_refresh` | Refresh current page |

### Extraction (5 tools)

| Tool | Description |
|------|-------------|
| `browser_extract_text` | Extract visible text from element |
| `browser_extract_attribute` | Get attribute value from element |
| `browser_extract_links` | Extract all links on page |
| `browser_page_info` | Get current URL and title |
| `browser_page_source` | Get HTML source |

### Interaction (5 tools)

| Tool | Description |
|------|-------------|
| `browser_click` | Click on an element |
| `browser_double_click` | Double-click an element |
| `browser_type` | Type text into element |
| `browser_clear` | Clear an input field |
| `browser_select` | Select dropdown option |

### Wait (4 tools)

| Tool | Description |
|------|-------------|
| `browser_wait_for_element` | Wait for element to appear |
| `browser_wait` | Wait for a duration |
| `browser_wait_for_page_load` | Wait for page to load |
| `browser_wait_for_text` | Wait for text to appear |

### Screenshots (1 tool)

| Tool | Description |
|------|-------------|
| `browser_screenshot` | Capture page or element screenshot |

### JavaScript (4 tools)

| Tool | Description |
|------|-------------|
| `browser_evaluate_js` | Execute JavaScript code |
| `browser_scroll` | Scroll the page |
| `browser_hover` | Hover over an element |
| `browser_handle_alert` | Handle JavaScript alerts |

### Cookies (5 tools)

| Tool | Description |
|------|-------------|
| `browser_get_cookies` | Get all cookies |
| `browser_get_cookie` | Get specific cookie |
| `browser_add_cookie` | Add a cookie |
| `browser_delete_cookie` | Delete a cookie |
| `browser_delete_all_cookies` | Delete all cookies |

### Windows/Tabs (8 tools)

| Tool | Description |
|------|-------------|
| `browser_list_windows` | List all windows/tabs |
| `browser_new_tab` | Open new tab |
| `browser_new_window` | Open new window |
| `browser_switch_window` | Switch to window |
| `browser_close_window` | Close current window |
| `browser_maximize_window` | Maximize window |
| `browser_minimize_window` | Minimize window |
| `browser_set_window_size` | Set window size |

### Frames (3 tools)

| Tool | Description |
|------|-------------|
| `browser_switch_to_frame` | Switch to iframe |
| `browser_switch_to_parent_frame` | Switch to parent frame |
| `browser_switch_to_default_content` | Switch to main document |

### Actions (7 tools)

| Tool | Description |
|------|-------------|
| `browser_drag_and_drop` | Drag and drop elements |
| `browser_right_click` | Right-click on element |
| `browser_focus` | Focus on element |
| `browser_element_state` | Get element state (visible, enabled, selected) |
| `browser_press_key` | Press keyboard key |
| `browser_file_upload` | Upload file to input |
| `browser_print_to_pdf` | Generate PDF from page |

## Element Selectors

Tools that target elements accept CSS selectors:

```rust
// By ID
"#login-button"

// By class
".submit-btn"

// By tag and attribute
"input[type='email']"

// By data attribute
"[data-testid='search']"

// Complex selectors
"form.login input[name='password']"

// Nth child
"ul.menu li:nth-child(3)"
```

## Example: Web Research Agent

```rust
use adk_browser::{BrowserSession, BrowserToolset, BrowserConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let config = BrowserConfig::new().webdriver_url("http://localhost:4444");
let browser = Arc::new(BrowserSession::new(config));
browser.start().await?;

let toolset = BrowserToolset::new(browser.clone())
    .with_navigation(true)
    .with_extraction(true)
    .with_screenshot(true);

let mut builder = LlmAgentBuilder::new("researcher")
    .model(model)
    .instruction(r#"
        You are a web research assistant. When asked about a topic:
        1. Navigate to relevant websites using browser_navigate
        2. Extract key information using browser_extract_text
        3. Take screenshots of important content using browser_screenshot
        4. Summarize your findings
    "#);

for tool in toolset.all_tools() {
    builder = builder.tool(tool);
}

let agent = builder.build()?;
```

## Example: Form Automation

```rust
let agent = LlmAgentBuilder::new("form_filler")
    .model(model)
    .instruction(r#"
        You are a form automation assistant. To fill forms:
        1. Use browser_navigate to go to the form page
        2. Use browser_extract_text to see form labels
        3. Use browser_type to fill text fields
        4. Use browser_select for dropdowns
        5. Use browser_click to submit
    "#)
    .build()?;
```

## Configuration

```rust
let config = BrowserConfig::new()
    .webdriver_url("http://localhost:4444")
    .headless(true)
    .viewport(1920, 1080)
    .page_load_timeout(30)
    .user_agent("Custom User Agent");

let browser = Arc::new(BrowserSession::new(config));
browser.start().await?;
```

## WebDriver Options

Works with any WebDriver-compatible server:

| Server | Command |
|--------|---------|
| Selenium (Chrome) | `docker run -d -p 4444:4444 selenium/standalone-chrome` |
| Selenium (Firefox) | `docker run -d -p 4444:4444 selenium/standalone-firefox` |
| ChromeDriver | `chromedriver --port=4444` |
| GeckoDriver | `geckodriver --port=4444` |

## Error Handling

Browser tools return structured errors:

```rust
match result {
    Ok(value) => println!("Success: {:?}", value),
    Err(e) => {
        match e {
            BrowserError::ElementNotFound(selector) => {
                println!("Could not find element: {}", selector);
            }
            BrowserError::Timeout(duration) => {
                println!("Operation timed out after {:?}", duration);
            }
            BrowserError::SessionClosed => {
                println!("Browser session was closed");
            }
            _ => println!("Browser error: {}", e),
        }
    }
}
```

## Examples

```bash
# Basic browser session
cargo run --example browser_basic

# AI agent with browser tools
cargo run --example browser_agent

# Full 46-tool interactive example
cargo run --example browser_interactive

# OpenAI-powered browser agent
cargo run --example browser_openai --features openai
```

## Best Practices

1. **Use Waits**: Always use `browser_wait_*` tools before interacting with dynamic content
2. **Minimize Screenshots**: Screenshots are expensive; use them strategically
3. **Close Sessions**: Always close browser sessions when done
4. **Handle Errors**: Browser automation can fail; handle timeouts gracefully
5. **Filter Tools**: Only give agents the tools they need to reduce complexity

---

**Previous**: [← Built-in Tools](built-in-tools.md) | **Next**: [UI Tools →](ui-tools.md)
