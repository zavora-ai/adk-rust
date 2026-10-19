# adk-browser

Browser automation tools for ADK-Rust agents using WebDriver (via [thirtyfour](https://crates.io/crates/thirtyfour)).

[![Crates.io](https://img.shields.io/crates/v/adk-browser.svg)](https://crates.io/crates/adk-browser)
[![Documentation](https://docs.rs/adk-browser/badge.svg)](https://docs.rs/adk-browser)
[![License](https://img.shields.io/crates/l/adk-browser.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## Installation

```toml
[dependencies]
adk-browser = "3.0"
```

Or via the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["browser"] }
```

## Overview

This crate provides 46 browser automation tools (44 enabled by default) as ADK `Tool` implementations, allowing LLM agents to interact with web pages. Tools are organized into categories and can be selectively enabled via profiles or builder toggles.

`BrowserToolset` implements the `adk_core::Toolset` trait, so it integrates directly with `LlmAgentBuilder::toolset()`.

## Security Defaults

| Default | Opt out |
|---------|---------|
| `browser_evaluate_js`, which runs model-written JavaScript in the page, is not included in any toolset or profile | `.with_evaluate_js(true)` (or `.with_js(true)`) |
| `browser_navigate`, `browser_new_tab`, and `browser_new_window` accept only `http` and `https` URLs, so `file:`, `javascript:`, `data:`, and `chrome:` URLs are refused | `.with_allowed_schemes(["https", "file"])` |
| The same tools refuse loopback, private, link-local, and cloud metadata addresses (`localhost`, `10.0.0.0/8`, `169.254.169.254`, and host names that resolve to them), and check the page they and `browser_back`, `browser_forward`, and `browser_refresh` end on after redirects, replacing a refused page with `about:blank` | `.with_private_network_access(true)` |
| The same tools refuse host names that do not resolve on the agent host | `.with_unresolved_hosts(true)` |
| `browser_file_upload` is not included in any toolset or profile; once enabled, it uploads only existing files inside the configured directories, after resolving symlinks | `.with_file_upload(["/srv/agent/uploads"])` |
| Chrome runs with its sandbox on; `--no-sandbox` is never added, and `chrome_options` cannot replace the argument list | `BrowserConfig::new().add_arg("--no-sandbox")` for containers that run Chrome as root |

```rust
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use std::sync::Arc;

fn toolset() -> BrowserToolset {
    let browser = Arc::new(BrowserSession::new(BrowserConfig::new()));
    BrowserToolset::new(browser)
        .with_evaluate_js(true) // opt in to arbitrary JavaScript
        .with_allowed_schemes(["https"]) // HTTPS only
        .with_file_upload(["/srv/agent/uploads"]) // opt in to uploads from one directory
}
```

The policy applies to the tools. `BrowserSession` methods such as `navigate` are called by your
own code and are not checked.

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

This program gives a Gemini agent the `Scraping` profile and asks it to read a page. It needs a
WebDriver server on `http://localhost:4444` (see [Requirements](#requirements)), a
`GOOGLE_API_KEY`, and `adk-agent`, `adk-core`, `adk-model`, `adk-runner`, `adk-session`,
`futures`, and `tokio` alongside `adk-browser`.

```rust
use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset};
use adk_core::Content;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The session starts on first use; stop it before exiting
    let config = BrowserConfig::new().headless(true).viewport(1920, 1080);
    let browser = Arc::new(BrowserSession::new(config));

    // Use a profile to limit tools (recommended)
    let toolset = BrowserToolset::with_profile(browser.clone(), BrowserProfile::Scraping);

    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
    let agent = LlmAgentBuilder::new("browser_agent")
        .instruction("Use the browser tools to answer. Reply with the answer only.")
        .model(Arc::new(model))
        .toolset(Arc::new(toolset))
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "browser".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("browser")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()?;

    let task = "Open https://example.com and tell me the page's main heading.";
    let mut events = runner.run_str("user", "s1", Content::new("user").with_text(task)).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        if event.is_final_response() {
            for part in event.content().map(|c| c.parts.as_slice()).unwrap_or_default() {
                if let Some(text) = part.text() {
                    println!("{text}");
                }
            }
        }
    }

    // Clean up
    browser.stop().await?;
    Ok(())
}
```

## Tool Profiles

Instead of using all 44 default tools (which can overwhelm LLM context windows), use a profile:

| Profile | Tools | Use Case |
|---------|-------|----------|
| `Minimal` | 19 | Navigation + interaction + extraction + wait + screenshot |
| `FormFilling` | 19 | Same as Minimal — optimized for form-filling agents |
| `Scraping` | 13 | Navigation + extraction + screenshot + scroll/hover/alert (no interaction) |
| `Full` | 44 | Every tool except `browser_evaluate_js` and `browser_file_upload` — use only when full browser control is needed |

`BrowserToolset::new` is the `Full` profile. For even fewer tools, use the helper functions,
which apply the default URL policy:

```rust
use adk_browser::{
    BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset, minimal_browser_tools,
    readonly_browser_tools,
};
use std::sync::Arc;

fn main() {
    let browser = Arc::new(BrowserSession::new(BrowserConfig::new()));

    let form_filling = BrowserToolset::with_profile(browser.clone(), BrowserProfile::FormFilling);
    assert_eq!(form_filling.all_tools().len(), 19);

    // 6 tools: navigate, click, type, extract_text, wait_for_element, screenshot
    assert_eq!(minimal_browser_tools(browser.clone()).len(), 6);

    // 7 tools: navigate, extract_text, extract_attribute, extract_links, page_info, screenshot, scroll
    assert_eq!(readonly_browser_tools(browser.clone()).len(), 7);

    // Or use the builder for fine-grained control
    let custom = BrowserToolset::new(browser)
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
    assert_eq!(custom.all_tools().len(), 19);
}
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

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_browser::{BrowserConfig, BrowserProfile, BrowserSessionPool, BrowserToolset};
use adk_core::Llm;
use std::sync::Arc;

fn agent(model: Arc<dyn Llm>) -> adk_core::Result<LlmAgent> {
    // Create a session pool (shared across all invocations), at most 10 sessions
    let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::default(), 10));

    // Pool-backed toolset with every default tool; sessions resolved per-user at runtime
    let _full = BrowserToolset::with_pool(pool.clone());

    // Or with a profile to limit tool categories
    let toolset = BrowserToolset::with_pool_and_profile(pool, BrowserProfile::FormFilling);

    // Register with an agent via .toolset()
    LlmAgentBuilder::new("browser_agent").model(model).toolset(Arc::new(toolset)).build()
}
```

Pool-backed toolsets resolve sessions lazily — `pool.get_or_create(user_id)` is called inside `Toolset::tools(ctx)`. The synchronous `all_tools()` method returns an empty vec for pool-backed toolsets (with a warning log). Use `Toolset::tools(ctx)` or `try_all_tools()` instead.

For direct pool access without the toolset abstraction:

```rust
use adk_browser::{BrowserConfig, BrowserSessionPool};

async fn visit() -> adk_core::Result<()> {
    let pool = BrowserSessionPool::new(BrowserConfig::default(), 10);

    let session = pool.get_or_create("user-123").await?;
    session.navigate("https://example.com").await?;

    // Graceful shutdown
    pool.cleanup_all().await;
    Ok(())
}
```

## Session Lifecycle

`BrowserSession` automatically starts or reconnects the WebDriver when any browser method is called. You do not need to call `start()` manually — all public methods that access the WebDriver go through an internal `live_driver()` path that calls `ensure_started()` first.

A host that manages the browser itself sets `require_explicit_start(true)`: tools then return an error until the host calls `start()`, and again after the session is lost. `start()` keeps a session that still responds and replaces one that does not. `chrome_option(name, value)` passes Chrome options such as `binary` and `prefs`; command-line flags go through `add_arg`, and an `args` option is rejected.

```rust
use adk_browser::{BrowserConfig, BrowserSession};
use std::sync::Arc;

async fn lifecycle() -> adk_core::Result<()> {
    let browser = Arc::new(BrowserSession::new(BrowserConfig::new()));

    // No need to call start() — navigate will auto-start the session
    browser.navigate("https://example.com").await?;

    // If the WebDriver dies (Selenium restart, timeout, etc.),
    // the next operation transparently recreates the session
    browser.click("a").await?; // auto-reconnects if stale

    // Check health (pings WebDriver, not just Option::is_some)
    if browser.is_active().await {
        // Session is alive
    }

    // Always stop before dropping to avoid orphaned WebDriver sessions
    browser.stop().await?;
    Ok(())
}
```

## Observable Mode (noVNC)

When using Selenium's noVNC viewer for debugging:

```rust
use adk_browser::BrowserConfig;

fn config() -> BrowserConfig {
    BrowserConfig::new().observable() // headless=false, 1280x720
}
```

Then open `http://localhost:7900` to watch the browser in real-time.

### Category Filtering

Fine-tune which tool categories are included:

```rust
use adk_browser::{BrowserSession, BrowserToolset};
use std::sync::Arc;

fn toolset(session: Arc<BrowserSession>) -> BrowserToolset {
    BrowserToolset::new(session)
        .with_navigation(true)    // navigate, back, forward, refresh
        .with_interaction(true)   // click, double_click, type, clear, select
        .with_extraction(true)    // extract_text, extract_attribute, extract_links, page_info, page_source
        .with_wait(true)          // wait_for_element, wait, wait_for_page_load, wait_for_text
        .with_screenshot(true)    // screenshot
        .with_js(false)           // scroll, hover, handle_alert (and evaluate_js when true)
        .with_cookies(false)      // get_cookies, get_cookie, add_cookie, delete_cookie, delete_all_cookies
        .with_windows(false)      // list_windows, new_tab, new_window, switch_window, close_window, etc.
        .with_frames(false)       // switch_to_frame, switch_to_parent_frame, switch_to_default_content
        .with_actions(false)      // drag_and_drop, right_click, focus, element_state, press_key, print_to_pdf
}
```

## Available Tools (46, 44 by default)

### Navigation (4 tools)
| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL (`http`/`https` by default) |
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
| `browser_evaluate_js` | Execute JavaScript (sync or async); opt-in via `with_evaluate_js(true)` |
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

### Advanced Actions (6 tools, plus `browser_file_upload`)
| Tool | Description |
|------|-------------|
| `browser_drag_and_drop` | Drag element to target |
| `browser_right_click` | Right-click (context menu) |
| `browser_focus` | Focus an element |
| `browser_element_state` | Check displayed/enabled/selected/clickable state |
| `browser_press_key` | Press keyboard key with optional modifiers (Ctrl, Alt, Shift, Meta) |
| `browser_file_upload` | Upload a file from an allowed directory to an input element; opt-in via `with_file_upload(roots)` |
| `browser_print_to_pdf` | Print page to PDF (base64) |

## Configuration

```rust
use adk_browser::{BrowserConfig, BrowserType};

fn configs() -> (BrowserConfig, BrowserConfig) {
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
    (config, observable_config)
}
```

The defaults are `http://localhost:4444`, Chrome, headless, 1920x1080, and 30-second page-load
and script timeouts.

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

```rust
use adk_browser::{BrowserSession, BrowserSessionPool};

async fn shutdown(browser: &BrowserSession, pool: &BrowserSessionPool) -> adk_core::Result<()> {
    // Single session
    browser.stop().await?;

    // Session pool, once the process receives Ctrl-C
    let _ = tokio::signal::ctrl_c().await;
    pool.cleanup_all().await;
    Ok(())
}
```

## Utility Functions

### shared_session

Convenience function to create an `Arc<BrowserSession>`:

```rust
use adk_browser::{BrowserConfig, BrowserSession, shared_session};
use std::sync::Arc;

fn browser() -> Arc<BrowserSession> {
    shared_session(BrowserConfig::default())
}
```

### escape_js_string

Escapes a string for safe interpolation into JavaScript code, preventing CSS selector injection:

```rust
use adk_browser::escape_js_string;

fn main() {
    let safe = escape_js_string("div[data-id='test']");
    assert!(safe.contains("\\'")); // single quotes are escaped
}
```

### ElementState

Returned by `browser_element_state` and `BrowserSession::get_element_state()`:

```rust
pub struct ElementState {
    pub is_displayed: bool,
    pub is_enabled: bool,
    pub is_selected: bool,
    pub is_clickable: bool,
}
```

### Prelude

For convenient imports:

```rust
#[allow(unused_imports)]
use adk_browser::prelude::*;
// Imports: BrowserConfig, BrowserType, BrowserSessionPool,
//          BrowserSession, shared_session, BrowserProfile,
//          BrowserToolset, minimal_browser_tools, readonly_browser_tools

fn main() {}
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
