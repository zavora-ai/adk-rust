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
adk-browser = "0.1"
adk-agent = "0.1"
adk-model = "0.1"
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
    // Create browser session
    let config = BrowserConfig::new("http://localhost:4444");
    let session = BrowserSession::new(config).await?;

    // Create toolset with all 46 tools
    let toolset = BrowserToolset::new(session);
    let tools = toolset.all_tools();

    // Create AI agent with browser tools
    let model = Arc::new(GeminiModel::from_env("gemini-2.0-flash")?);

    let mut builder = LlmAgentBuilder::new("web_agent")
        .model(model)
        .instruction("You are a web automation assistant. Use browser tools to help users.");

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    Ok(())
}
```

## Filtered Tools

Select only the tools your agent needs:

```rust
let toolset = BrowserToolset::new(session)
    .with_navigation(true)   // Navigate, back, forward, refresh
    .with_extraction(true)   // Extract text, links, HTML
    .with_interaction(true)  // Click, type, select
    .with_forms(false)       // Disable form tools
    .with_screenshots(true)  // Screenshots
    .with_javascript(false)  // Disable JS execution
    .with_cookies(false)     // Disable cookie tools
    .with_frames(false)      // Disable frame tools
    .with_windows(false)     // Disable window tools
    .with_actions(false);    // Disable advanced actions

let tools = toolset.selected_tools();
```

## Available Tools (46 Total)

### Navigation (6 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_navigate` | Navigate to a URL | `url: string` |
| `browser_back` | Go back in history | none |
| `browser_forward` | Go forward in history | none |
| `browser_refresh` | Refresh current page | none |
| `browser_page_info` | Get current URL and title | none |
| `browser_close` | Close the browser session | none |

### Extraction (6 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_extract_text` | Extract visible text | `selector?: string` |
| `browser_extract_html` | Get HTML source | `selector?: string` |
| `browser_extract_links` | Extract all links | none |
| `browser_extract_images` | Extract image sources | none |
| `browser_extract_tables` | Extract tables as JSON | `selector?: string` |
| `browser_extract_metadata` | Get page metadata | none |

### Interaction (6 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_click` | Click on an element | `selector: string` |
| `browser_type` | Type text into element | `selector: string, text: string` |
| `browser_clear` | Clear an input field | `selector: string` |
| `browser_select` | Select dropdown option | `selector: string, value: string` |
| `browser_submit` | Submit a form | `selector: string` |
| `browser_hover` | Hover over element | `selector: string` |

### Forms (5 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_fill_form` | Fill multiple fields | `fields: object` |
| `browser_get_form_fields` | List form fields | `selector?: string` |
| `browser_get_field_value` | Get field value | `selector: string` |
| `browser_set_checkbox` | Set checkbox state | `selector: string, checked: bool` |
| `browser_upload_file` | Upload file to input | `selector: string, path: string` |

### Screenshots (3 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_screenshot` | Full page screenshot | `path?: string` |
| `browser_screenshot_element` | Element screenshot | `selector: string, path?: string` |
| `browser_print_pdf` | Generate PDF | `path?: string, landscape?: bool` |

### JavaScript (3 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_evaluate` | Execute JS sync | `script: string` |
| `browser_evaluate_async` | Execute async JS | `script: string` |
| `browser_scroll` | Scroll page/element | `x?: int, y?: int, selector?: string` |

### Wait (4 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_wait_element` | Wait for element | `selector: string, timeout?: int` |
| `browser_wait_text` | Wait for text | `text: string, timeout?: int` |
| `browser_wait_url` | Wait for URL match | `pattern: string, timeout?: int` |
| `browser_wait_load` | Wait for page load | `timeout?: int` |

### Cookies (4 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_get_cookies` | Get all cookies | none |
| `browser_get_cookie` | Get specific cookie | `name: string` |
| `browser_set_cookie` | Set a cookie | `name: string, value: string, ...` |
| `browser_delete_cookies` | Delete cookies | `name?: string` |

### Frames (3 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_switch_frame` | Switch to iframe | `selector: string` |
| `browser_switch_parent` | Switch to parent | none |
| `browser_switch_default` | Switch to main doc | none |

### Windows (4 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_get_windows` | List windows/tabs | none |
| `browser_switch_window` | Switch to window | `handle: string` |
| `browser_new_tab` | Open new tab | `url?: string` |
| `browser_close_window` | Close current window | none |

### Actions (2 tools)

| Tool | Description | Arguments |
|------|-------------|-----------|
| `browser_drag_drop` | Drag and drop | `source: string, target: string` |
| `browser_double_click` | Double-click | `selector: string` |

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

let session = BrowserSession::new(BrowserConfig::new("http://localhost:4444")).await?;

let toolset = BrowserToolset::new(session)
    .with_navigation(true)
    .with_extraction(true)
    .with_screenshots(true);

let agent = LlmAgentBuilder::new("researcher")
    .model(model)
    .instruction(r#"
        You are a web research assistant. When asked about a topic:
        1. Navigate to relevant websites using browser_navigate
        2. Extract key information using browser_extract_text
        3. Take screenshots of important content using browser_screenshot
        4. Summarize your findings
    "#)
    .tools(toolset.selected_tools())
    .build()?;
```

## Example: Form Automation

```rust
let agent = LlmAgentBuilder::new("form_filler")
    .model(model)
    .instruction(r#"
        You are a form automation assistant. To fill forms:
        1. Use browser_navigate to go to the form page
        2. Use browser_get_form_fields to discover fields
        3. Use browser_fill_form to fill multiple fields at once
        4. Use browser_submit to submit the form
    "#)
    .tools(toolset.all_tools())
    .build()?;
```

## Configuration

```rust
let config = BrowserConfig::new("http://localhost:4444")
    .with_headless(true)          // Run headless if supported
    .with_timeout(Duration::from_secs(30))
    .with_implicit_wait(Duration::from_secs(10));

let session = BrowserSession::new(config).await?;
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

**Next**: [MCP Tools →](mcp-tools.md)
