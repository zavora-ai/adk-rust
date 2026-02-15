# adk-browser

Browser automation tools for Rust Agent Development Kit (ADK-Rust) agents using WebDriver.

[![Crates.io](https://img.shields.io/crates/v/adk-browser.svg)](https://crates.io/crates/adk-browser)
[![Documentation](https://docs.rs/adk-browser/badge.svg)](https://docs.rs/adk-browser)
[![License](https://img.shields.io/crates/l/adk-browser.svg)](LICENSE)

## Overview

`adk-browser` provides 46 comprehensive browser automation tools that enable AI agents to interact with web pages, extract information, fill forms, take screenshots, and more. Built on the WebDriver protocol (Selenium), it works with any WebDriver-compatible browser.

## Features

- **46 Browser Tools**: Complete web automation toolkit for AI agents
- **WebDriver Compatible**: Works with Selenium, ChromeDriver, GeckoDriver, etc.
- **ADK Integration**: Tools implement `adk_core::Tool` for seamless agent integration
- **Configurable**: Filter tools by category based on agent needs
- **Async**: Built on Tokio for efficient async operations

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-browser = "0.3.1"
adk-agent = "0.3.1"
adk-model = "0.3.1"
```

### Prerequisites

Start a WebDriver server (e.g., Selenium):

```bash
docker run -d -p 4444:4444 -p 7900:7900 --shm-size=2g selenium/standalone-chrome:latest
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
    let config = BrowserConfig::new().webdriver_url("http://localhost:4444");
    let session = Arc::new(BrowserSession::new(config));

    // Create toolset with all 46 tools
    let toolset = BrowserToolset::new(session);
    let tools = toolset.all_tools();

    // Create AI agent with browser tools
    let model = Arc::new(GeminiModel::from_env("gemini-2.5-flash")?);

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

### Filtered Tools

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
| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL |
| `browser_back` | Go back in history |
| `browser_forward` | Go forward in history |
| `browser_refresh` | Refresh current page |
| `browser_page_info` | Get current URL and title |
| `browser_close` | Close the browser session |

### Extraction (6 tools)
| Tool | Description |
|------|-------------|
| `browser_extract_text` | Extract visible text from page or element |
| `browser_extract_html` | Get HTML source |
| `browser_extract_links` | Extract all links from page |
| `browser_extract_images` | Extract all image sources |
| `browser_extract_tables` | Extract table data as JSON |
| `browser_extract_metadata` | Get page metadata (title, description, etc.) |

### Interaction (6 tools)
| Tool | Description |
|------|-------------|
| `browser_click` | Click on an element |
| `browser_type` | Type text into an element |
| `browser_clear` | Clear an input field |
| `browser_select` | Select option from dropdown |
| `browser_submit` | Submit a form |
| `browser_hover` | Hover over an element |

### Forms (5 tools)
| Tool | Description |
|------|-------------|
| `browser_fill_form` | Fill multiple form fields at once |
| `browser_get_form_fields` | List all form fields |
| `browser_get_field_value` | Get value of a form field |
| `browser_set_checkbox` | Set checkbox state |
| `browser_upload_file` | Upload file to input |

### Screenshots (3 tools)
| Tool | Description |
|------|-------------|
| `browser_screenshot` | Take full page screenshot |
| `browser_screenshot_element` | Screenshot specific element |
| `browser_print_pdf` | Generate PDF of page |

### JavaScript (3 tools)
| Tool | Description |
|------|-------------|
| `browser_evaluate` | Execute JavaScript synchronously |
| `browser_evaluate_async` | Execute async JavaScript |
| `browser_scroll` | Scroll page or element |

### Wait (4 tools)
| Tool | Description |
|------|-------------|
| `browser_wait_element` | Wait for element to appear |
| `browser_wait_text` | Wait for text to appear |
| `browser_wait_url` | Wait for URL to match |
| `browser_wait_load` | Wait for page load complete |

### Cookies (4 tools)
| Tool | Description |
|------|-------------|
| `browser_get_cookies` | Get all cookies |
| `browser_get_cookie` | Get specific cookie |
| `browser_set_cookie` | Set a cookie |
| `browser_delete_cookies` | Delete cookies |

### Frames (3 tools)
| Tool | Description |
|------|-------------|
| `browser_switch_frame` | Switch to iframe |
| `browser_switch_parent` | Switch to parent frame |
| `browser_switch_default` | Switch to main document |

### Windows (4 tools)
| Tool | Description |
|------|-------------|
| `browser_get_windows` | List all windows/tabs |
| `browser_switch_window` | Switch to window |
| `browser_new_tab` | Open new tab |
| `browser_close_window` | Close current window |

### Actions (2 tools)
| Tool | Description |
|------|-------------|
| `browser_drag_drop` | Drag and drop elements |
| `browser_double_click` | Double-click element |

## Element Selectors

Tools that target elements accept CSS selectors:

```rust
// By ID
"#login-button"

// By class
".submit-btn"

// By tag
"input[type='email']"

// By attribute
"[data-testid='search']"

// Complex selectors
"form.login input[name='password']"
```

## Example: Web Research Agent

```rust
use adk_browser::{BrowserSession, BrowserToolset, BrowserConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let config = BrowserConfig::new().webdriver_url("http://localhost:4444");
let session = Arc::new(BrowserSession::new(config));

let toolset = BrowserToolset::new(session)
    .with_navigation(true)
    .with_extraction(true)
    .with_screenshots(true);

let tools = toolset.selected_tools();
let mut builder = LlmAgentBuilder::new("researcher")
    .model(model)
    .instruction(r#"
        You are a web research assistant. When asked about a topic:
        1. Navigate to relevant websites
        2. Extract key information using browser_extract_text
        3. Take screenshots of important content
        4. Summarize your findings
    "#);

for tool in tools {
    builder = builder.tool(tool);
}

let agent = builder.build()?;
```

## Configuration

```rust
let config = BrowserConfig::new()
    .webdriver_url("http://localhost:4444")
    .headless(true)
    .page_load_timeout(30)
    .implicit_wait(10);

let session = Arc::new(BrowserSession::new(config));
```

## WebDriver Options

Works with any WebDriver-compatible server:

| Server | Command |
|--------|---------|
| Selenium (Chrome) | `docker run -d -p 4444:4444 selenium/standalone-chrome` |
| Selenium (Firefox) | `docker run -d -p 4444:4444 selenium/standalone-firefox` |
| ChromeDriver | `chromedriver --port=4444` |
| GeckoDriver | `geckodriver --port=4444` |

## Examples

Run the included examples:

```bash
# Basic browser session
cargo run --example browser_basic

# AI agent with browser tools
cargo run --example browser_agent

# Full interactive example with all 46 tools
cargo run --example browser_interactive

# OpenAI-powered browser agent
cargo run --example browser_openai --features openai
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
