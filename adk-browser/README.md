# adk-browser

Browser automation tools for ADK-Rust agents using WebDriver (via [thirtyfour](https://crates.io/crates/thirtyfour)).

## Overview

This crate provides 46 browser automation tools as ADK `Tool` implementations, allowing LLM agents to interact with web pages. Tools are organized into categories and can be selectively enabled via profiles or builder toggles.

## Requirements

A WebDriver-compatible server must be running:

```bash
# ChromeDriver
brew install chromedriver && chromedriver --port=4444

# Selenium (Docker)
docker run -d -p 4444:4444 -p 7900:7900 selenium/standalone-chrome

# With noVNC viewer (port 7900) — use observable() config
```

## Quick Start

```rust,ignore
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset, BrowserProfile};
use std::sync::Arc;

// Create and start a browser session
let config = BrowserConfig::new().headless(true).viewport(1920, 1080);
let browser = Arc::new(BrowserSession::new(config));
browser.start().await?;

// Use a profile to limit tools (recommended)
let toolset = BrowserToolset::with_profile(browser.clone(), BrowserProfile::FormFilling);
let tools = toolset.all_tools();

// Or use minimal_browser_tools() for the smallest set
let tools = adk_browser::minimal_browser_tools(browser.clone());

// Clean up
browser.stop().await?;
```

## Tool Profiles

Instead of using all 46 tools (which can overwhelm LLM context windows), use a profile:

| Profile | Tools | Use Case |
|---------|-------|----------|
| `Minimal` | 19 | Navigation + interaction + extraction + wait + screenshot |
| `FormFilling` | 19 | Same as Minimal — optimized for form-filling agents |
| `Scraping` | 13 | Navigation + extraction + screenshot + scroll (no interaction) |
| `Full` | 46 | All tools — use only when full browser control is needed |

```rust,ignore
let toolset = BrowserToolset::with_profile(browser, BrowserProfile::FormFilling);
```

For even fewer tools, use the helper functions:

```rust,ignore
// 6 tools: navigate, click, type, extract_text, wait_for_element, screenshot
let tools = minimal_browser_tools(browser.clone());

// 7 tools: navigate, extract_text, extract_attribute, extract_links, page_info, screenshot, scroll
let tools = readonly_browser_tools(browser.clone());
```

Or use the builder for fine-grained control:

```rust,ignore
let toolset = BrowserToolset::new(browser)
    .with_navigation(true)
    .with_interaction(true)
    .with_extraction(true)
    .with_wait(true)
    .with_screenshot(true)
    .with_js(false)
    .with_cookies(false)
    .with_windows(false)
    .with_frames(false)
    .with_actions(false);
```

## Tool Response Format

All navigation tools (`browser_navigate`, `browser_back`, `browser_forward`, `browser_refresh`) and interaction tools (`browser_click`, `browser_type`, `browser_clear`, `browser_select`) include a `"page"` field in their JSON response containing the current page context (URL, title, and truncated page text). This gives the LLM consistent situational awareness after any browser operation.

```json
{
  "success": true,
  "url": "https://example.com",
  "title": "Example",
  "page": { "url": "https://example.com", "title": "Example", "text": "..." }
}
```

If page context capture fails after a successful operation, the response includes a `"page_context_error"` field instead of `"page"`.

## Multi-Tenant Browser Agents

For production multi-tenant use, create a pool-backed `BrowserToolset` and register it with `LlmAgentBuilder` via `.toolset()`. The toolset resolves a per-user `BrowserSession` from the pool at each invocation using the context's `user_id`.

```rust,ignore
use adk_browser::{BrowserConfig, BrowserSessionPool, BrowserToolset, BrowserProfile};
use std::sync::Arc;

// Create a session pool (shared across all invocations)
let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::default(), 10));

// Pool-backed toolset — sessions resolved per-user at runtime
let toolset = BrowserToolset::with_pool(pool.clone());

// Or with a profile to limit tool categories
let toolset = BrowserToolset::with_pool_and_profile(pool.clone(), BrowserProfile::FormFilling);

// Register with an agent via .toolset()
let agent = LlmAgent::builder()
    .model(model)
    .toolset(Arc::new(toolset))
    .build();
```

Pool-backed toolsets resolve sessions lazily — `pool.get_or_create(user_id)` is called inside `Toolset::tools(ctx)`. The synchronous `all_tools()` method returns an empty vec for pool-backed toolsets (with a warning log). Use `Toolset::tools(ctx)` or `try_all_tools()` instead.

For direct pool access without the toolset abstraction:

```rust,ignore
let pool = BrowserSessionPool::new(BrowserConfig::default(), 10);

let session = pool.get_or_create("user-123").await?;
session.navigate("https://example.com").await?;

// Graceful shutdown
pool.cleanup_all().await;
```

## Session Lifecycle

`BrowserSession` automatically starts or reconnects the WebDriver when any browser method is called. You do not need to call `start()` manually — all public methods that access the WebDriver go through an internal `live_driver()` path that calls `ensure_started()` first.

```rust,ignore
let browser = Arc::new(BrowserSession::new(config));

// No need to call start() — navigate will auto-start the session
browser.navigate("https://example.com").await?;

// If the WebDriver dies (Selenium restart, timeout, etc.),
// the next operation transparently recreates the session
browser.click("#submit").await?; // auto-reconnects if stale

// Explicit start/stop are still available for manual control
browser.start().await?;
browser.stop().await?;

// Check health (pings WebDriver, not just Option::is_some)
if browser.is_active().await {
    // Session is alive
}

// Always stop before dropping to avoid orphaned WebDriver sessions
browser.stop().await?;
```

## Observable Mode (noVNC)

When using Selenium's noVNC viewer for debugging:

```rust,ignore
let config = BrowserConfig::new().observable(); // headless=false, 1280x720
```

Then open `http://localhost:7900` to watch the browser in real-time.

### Category Filtering

Fine-tune which tool categories are included:

```rust,ignore
let toolset = BrowserToolset::new(session)
    .with_navigation(true)    // navigate, back, forward, refresh
    .with_interaction(true)   // click, double_click, type, clear, select
    .with_extraction(true)    // extract_text, extract_attribute, extract_links, page_info, page_source
    .with_wait(true)          // wait_for_element, wait, wait_for_page_load, wait_for_text
    .with_screenshot(true)    // screenshot
    .with_js(false)           // evaluate_js, scroll, hover, handle_alert
    .with_cookies(false)      // get_cookies, get_cookie, add_cookie, delete_cookie, delete_all_cookies
    .with_windows(false)      // list_windows, new_tab, new_window, switch_window, close_window, etc.
    .with_frames(false)       // switch_to_frame, switch_to_parent_frame, switch_to_default_content
    .with_actions(false);     // drag_and_drop, right_click, focus, element_state, press_key, etc.

let tools = toolset.all_tools();
```

## Available Tools (46)

### Navigation (4 tools)
| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL |
| `browser_back` | Go back in history |
| `browser_forward` | Go forward in history |
| `browser_refresh` | Refresh current page |

### Interaction (5 tools)
| Tool | Description |
|------|-------------|
| `browser_click` | Click an element (waits for clickable, returns page context) |
| `browser_double_click` | Double-click an element |
| `browser_type` | Type text into an input (optional clear_first, press_enter) |
| `browser_clear` | Clear an input field |
| `browser_select` | Select from dropdown by value, text, or index |

### Extraction (5 tools)
| Tool | Description |
|------|-------------|
| `browser_extract_text` | Extract text from one or all matching elements |
| `browser_extract_attribute` | Get an attribute value (href, src, value, etc.) |
| `browser_extract_links` | Extract all links from page or container |
| `browser_page_info` | Get current URL and title |
| `browser_page_source` | Get HTML source (with max_length truncation) |

### Screenshots (1 tool)
| Tool | Description |
|------|-------------|
| `browser_screenshot` | Capture page or element screenshot (optional artifact save) |

### Waiting (4 tools)
| Tool | Description |
|------|-------------|
| `browser_wait_for_element` | Wait for element to appear (optional visible check) |
| `browser_wait` | Wait for a fixed duration (max 30s) |
| `browser_wait_for_page_load` | Wait for document.readyState === 'complete' |
| `browser_wait_for_text` | Wait for specific text to appear on page |

### JavaScript (4 tools)
| Tool | Description |
|------|-------------|
| `browser_evaluate_js` | Execute JavaScript (sync or async) |
| `browser_scroll` | Scroll by direction, amount, or to element |
| `browser_hover` | Hover over an element (dispatches mouseenter + mouseover) |
| `browser_handle_alert` | Handle alerts/confirms/prompts (accept or dismiss) |

### Cookies (5 tools)
| Tool | Description |
|------|-------------|
| `browser_get_cookies` | Get all cookies |
| `browser_get_cookie` | Get a specific cookie by name |
| `browser_add_cookie` | Add a cookie (with optional domain, path, secure, expiry) |
| `browser_delete_cookie` | Delete a cookie by name |
| `browser_delete_all_cookies` | Delete all cookies |

### Windows/Tabs (8 tools)
| Tool | Description |
|------|-------------|
| `browser_list_windows` | List all windows/tabs |
| `browser_new_tab` | Open a new tab (optional URL) |
| `browser_new_window` | Open a new window (optional URL) |
| `browser_switch_window` | Switch to a window by handle |
| `browser_close_window` | Close current window |
| `browser_maximize_window` | Maximize window |
| `browser_minimize_window` | Minimize window |
| `browser_set_window_size` | Set window size and position |

### Frames (3 tools)
| Tool | Description |
|------|-------------|
| `browser_switch_to_frame` | Switch to iframe by index or selector |
| `browser_switch_to_parent_frame` | Exit current iframe |
| `browser_switch_to_default_content` | Exit all iframes |

### Advanced Actions (7 tools)
| Tool | Description |
|------|-------------|
| `browser_drag_and_drop` | Drag element to target |
| `browser_right_click` | Right-click (context menu) |
| `browser_focus` | Focus an element |
| `browser_element_state` | Check displayed/enabled/selected/clickable state |
| `browser_press_key` | Press keyboard key with optional modifiers (Ctrl, Alt, Shift, Meta) |
| `browser_file_upload` | Upload file to input element |
| `browser_print_to_pdf` | Print page to PDF (base64) |

## Configuration

```rust,ignore
let config = BrowserConfig::new()
    .webdriver_url("http://localhost:4444")
    .browser(BrowserType::Chrome)
    .headless(true)
    .viewport(1920, 1080)
    .page_load_timeout(30)
    .user_agent("MyAgent/1.0")
    .add_arg("--disable-gpu");

// For noVNC-compatible viewing (headless=false, 1280x720)
let observable_config = BrowserConfig::new().observable();
```

## Element Selectors

Tools that target elements accept CSS selectors:

```text
#login-button                    // By ID
.submit-btn                      // By class
input[type='email']              // By attribute
[data-testid='search']           // By data attribute
form.login input[name='password'] // Complex selector
```

## WebDriver Servers

| Server | Command |
|--------|---------|
| Selenium (Chrome) | `docker run -d -p 4444:4444 selenium/standalone-chrome` |
| Selenium + noVNC | `docker run -d -p 4444:4444 -p 7900:7900 --shm-size=2g selenium/standalone-chrome` |
| Selenium (Firefox) | `docker run -d -p 4444:4444 selenium/standalone-firefox` |
| ChromeDriver | `chromedriver --port=4444` |
| GeckoDriver | `geckodriver --port=4444` |

## Architecture

```text
┌─────────────────────────────────────────────────┐
│                   LlmAgent                       │
│  .toolset(browser_toolset) or .tool(...)        │
└─────────────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────┐
│          BrowserToolset (impl Toolset)           │
│  Fixed session or pool-backed per-user session  │
│  Profile / builder-based tool selection          │
└─────────────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────┐
│    BrowserSession / BrowserSessionPool           │
│  Auto-start and reconnect via ensure_started()  │
└─────────────────────────────────────────────────┘
                       │
                       ▼
              WebDriver Server
           (ChromeDriver, Selenium)
```

## Shutdown

Always stop sessions before exiting to avoid orphaned WebDriver processes:

```rust,ignore
// Single session
browser.stop().await?;

// Session pool
pool.cleanup_all().await;

// With tokio shutdown signal
tokio::select! {
    _ = tokio::signal::ctrl_c() => {
        pool.cleanup_all().await;
    }
}
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
