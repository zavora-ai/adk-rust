# Browser Tools

The `adk-browser` crate provides 46 comprehensive browser automation tools (45 enabled by default) that enable AI agents to interact with web pages. Built on the WebDriver protocol (Selenium), it works with any WebDriver-compatible browser.

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
adk-browser = "3.0.0"
adk-agent = "3.0.0"
adk-model = "3.0.0"
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

    // Create toolset with the 44 default tools (browser_evaluate_js and
    // browser_file_upload are opt-in)
    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();

    // Create AI agent with browser tools
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

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
    .with_js(true)           // scroll, hover, handle_alert, and evaluate_js
    .with_cookies(false)     // Disable cookie tools
    .with_frames(false)      // Disable frame tools
    .with_windows(false)     // Disable window tools
    .with_actions(false);    // Disable advanced actions

let tools = toolset.all_tools();
```

## Security Defaults

Four defaults limit what a model can reach through the browser:

| Default | Why | Opt out |
|---------|-----|---------|
| `browser_evaluate_js` is excluded from every toolset and profile | It runs model-written JavaScript in the page, with the page's cookies and session | `.with_evaluate_js(true)` or `.with_js(true)` |
| `browser_file_upload` is excluded from every toolset and profile | It hands a local file to the page, and the model chooses the path | `.with_file_upload([roots])` — uploads only existing files inside `roots`, after resolving symlinks |
| `browser_navigate`, `browser_new_tab`, and `browser_new_window` accept only `http` and `https` | `file:` reads the host filesystem, `javascript:` and `data:` run script, and `chrome:` reaches browser settings | `.with_allowed_schemes([...])` |
| The same tools refuse loopback, private, link-local, and cloud metadata addresses | A page on `localhost` or `169.254.169.254` reaches services the operator never exposed, such as a metadata endpoint holding credentials | `.with_private_network_access(true)` |

```rust
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use std::sync::Arc;

let browser = Arc::new(BrowserSession::new(BrowserConfig::new()));
let toolset = BrowserToolset::new(browser)
    .with_evaluate_js(true)           // opt in to arbitrary JavaScript
    .with_allowed_schemes(["https"]); // HTTPS only
```

A refused URL fails with `URL scheme '<scheme>' is not allowed` or `points at a private
network address` before the browser is touched. Host names are resolved, and one that
resolves to a private address is refused; the check covers the URL the model asks for,
not redirects or subresources the page loads afterwards, so isolate the browser's network
when it must not reach internal services at all.

## Multi-Tenant Usage with Pool-Backed Toolsets

For production multi-tenant deployments, use `BrowserToolset::with_pool()` together with `LlmAgentBuilder::toolset()`. Each user gets an isolated browser session resolved from the pool at runtime:

```rust
use adk_browser::{BrowserSessionPool, BrowserToolset, BrowserConfig, BrowserProfile};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::new(), 10));

// Pool-backed toolset — sessions resolved per-user via ctx.user_id()
let toolset = Arc::new(BrowserToolset::with_pool_and_profile(
    pool,
    BrowserProfile::Scraping,
));

let agent = LlmAgentBuilder::new("web_agent")
    .model(model)
    .instruction("You are a web research assistant.")
    .toolset(toolset)
    .build()?;
```

Browser sessions auto-start and auto-recover from stale WebDriver connections, so `browser.start()` is optional. A host that manages the browser itself sets `BrowserConfig::require_explicit_start(true)`: tools then return an error until the host calls `start()`, and again after the session is lost.

## Available Tools (46 Total, 45 by Default)

### Navigation (4 tools)

| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL (`http`/`https` by default) |
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
| `browser_evaluate_js` | Execute JavaScript code (opt-in) |
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
| `browser_file_upload` | Upload a file from an allowed directory (opt-in) |
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
cargo check -p adk-browser
cargo check -p adk-rust --no-default-features --features browser
```

## Best Practices

1. **Use Waits**: Always use `browser_wait_*` tools before interacting with dynamic content
2. **Minimize Screenshots**: Screenshots are expensive; use them strategically
3. **Close Sessions**: Always close browser sessions when done
4. **Handle Errors**: Browser automation can fail; handle timeouts gracefully
5. **Filter Tools**: Only give agents the tools they need to reduce complexity

---

**Previous**: [← Built-in Tools](built-in-tools.md) | **Next**: [UI Tools →](ui-tools.md)
