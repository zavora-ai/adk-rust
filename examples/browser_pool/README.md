# Multi-Tenant Browser Pool Example

Demonstrates the production multi-tenant browser path introduced by the browser production hardening spec.

## Features

- **Pool-backed BrowserToolset** — `with_pool()` and `with_pool_and_profile()` resolve per-user sessions from a shared pool
- **Dynamic toolset resolution** — `LlmAgentBuilder::toolset()` resolves tools at runtime using `ctx.user_id()`
- **Automatic session lifecycle** — `ensure_started()` auto-starts sessions on first use
- **Navigation page context** — navigation tools now include page context in responses
- **Backward compatibility** — `BrowserToolset::new()` still works for single-session use

## Requirements

1. WebDriver running:
   ```bash
   docker run -d -p 4444:4444 selenium/standalone-chrome
   ```
2. `GOOGLE_API_KEY` environment variable set

## Running

```bash
cargo run --example browser_pool --features browser
```
