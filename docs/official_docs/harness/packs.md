# Browser and Computer-Use Packs

The browser pack puts `adk-browser`'s WebDriver tools under the harness policy, and the
computer-use pack drives a desktop through `adk-computer-use`'s governed reference graph
(spec 3.1 §7.24, §5 H10). Neither pack writes actuation code: `BrowserToolset` and the
reference graph do the work, and the packs are tool classes, wiring and audit records. Both are
off by default (`adk-harness` features `browser` and `computer-use`; on the umbrella,
`harness-browser` and `harness-computer-use`).

## Browser pack

### Quick start

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["browser"] }
adk-browser = "3.0.0"
adk-devtools = "3.0.0"
adk-core = "3.0.0"
anyhow = "1"
```

```rust
use std::sync::Arc;

use adk_browser::{BrowserConfig, BrowserProfile, BrowserSession};
use adk_core::Llm;
use adk_harness::{BrowserPack, HarnessAgent, WebConfig};

/// A harness that browses `docs.rs` without asking and asks before any other host.
fn browsing_harness(model: Arc<dyn Llm>) -> anyhow::Result<HarnessAgent> {
    let session = Arc::new(BrowserSession::new(
        BrowserConfig::default().webdriver_url("http://localhost:4444").headless(true),
    ));
    Ok(HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .web(WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() })
        .pack_browser(BrowserPack::new(session).with_profile(BrowserProfile::FormFilling))
        .build()?)
}
```

`HarnessInfo::packs` then lists `browser`, and the `<environment>` block of the prompt reads
`packs: browser`. `HarnessAgentBuilder::pack(Pack::Browser)` is the pack over
`BrowserConfig::default()`: Chrome, headless, the WebDriver at `http://localhost:4444`,
started on first use.

### Tool classes

| Tools | Class | `read_only` | `plan` | `ask` | `auto` |
|---|---|---|---|---|---|
| `browser_extract_text`, `_extract_attribute`, `_extract_links`, `_page_info`, `_page_source`, `_screenshot`, `_element_state`, `_wait`, `_wait_for_element`, `_wait_for_page_load`, `_wait_for_text`, `_list_windows`, `_scroll`, `_switch_to_frame`, `_switch_to_parent_frame`, `_switch_to_default_content`, `_switch_window`, `_print_to_pdf` | `Read` | Allow | Allow | Allow | Allow |
| `browser_navigate`, `_new_tab`, `_new_window` to a host in `WebConfig::allowed_domains` | `Web { allowlisted: true }` | Allow | Allow | Allow | Allow |
| The same tools to any other host | `Web { allowlisted: false }` | Deny | Deny | Ask | Ask |
| `browser_back`, `_forward`, `_refresh` (the destination is unknown until the page loads) | `Web { allowlisted: false }` | Deny | Deny | Ask | Ask |
| `browser_click`, `_double_click`, `_right_click`, `_type`, `_clear`, `_select`, `_focus`, `_press_key`, `_hover`, `_drag_and_drop`, `_handle_alert`, `_close_window`, `_maximize_window`, `_minimize_window`, `_set_window_size`, `_get_cookies`, `_get_cookie`, `_add_cookie`, `_delete_cookie`, `_delete_all_cookies` | `External` | Deny | Deny | Ask | Ask |

Every name is registered at build, attached or not, so a tool of the same name added with
`HarnessAgentBuilder::tool` is refused as a collision. `browser_evaluate_js` and
`browser_file_upload` are never attached: model-written code in the page and host files handed to
a page have no governed form.

### What the pack does

| Concern | Mechanism |
|---|---|
| Navigation | The URL's host is classified as `web_fetch`'s is; a loopback, link-local, private or `.local` literal is denied before the browser moves; the toolset refuses private addresses, redirect targets included, and non-`http(s)` schemes itself |
| Exfiltration | Before the browser moves, the URL passes the same checks as `web_fetch`: length, non-path budget and secret patterns; text typed through `browser_type` or `browser_press_key` that matches a secret pattern is refused as `harness.browser.secret_in_input`; both refusals are audited by digest, never by value |
| Delivery | Text the browser returns is screened — injection lines labelled, secrets redacted — and delivered inside `<untrusted-content source="browser" url="…" sha256="…">`, as fetched text is; a screenshot or a PDF is bytes, delivered as the tool returns it |
| Prompts | The `ask` prompt for a navigation shows host, path and query separately and offers "any page on `<host>`" as a standing rule; an action's suggestion pins this tool to its exact declaration |
| Standing rules | A navigation or an action is waivable: "always allow" is a `DomainIn` rule on the exact host, or an exact-declaration rule for the action |
| Members | A `MemberTools::Full` member keeps the observation tools and never navigates or acts; a read-only member keeps its fixed read set; what a page asks for is the coordinator's call to put to the person |
| Fixtures | `BrowserPack::with_private_network_access(true)` with `adk_harness::web::fixture::permit_loopback()` (feature `web-fixture`) lets a test drive a loopback page |

### Audit records

| Record | Fields (`ext`) | When |
|---|---|---|
| `harness.browser.navigated` | `functionCallId`, `tool`, `phase` (`request`, `response`, `refused`), `host`, `finalHost`, `allowlisted`, `urlDigest`, `refusal`, `code`, `outcome` | Write-ahead before the browser moves, and after the page loaded; a refusal carries the digest only |
| `harness.browser.acted` | `functionCallId`, `tool`, `outcome` (`executed`, `failed`, `refused`), `refusal`, `inputDigest`, `screened` | After each action, or at its refusal |
| `harness.web.screened` | `tool`, `url`, `contentSha256`, `patterns`, `lines`, `redactions`, `action` | When a result's text matched an injection or secret pattern |

### In the CLI

`adk-rust --pack browser` (adk-cli feature `browser`) installs the pack over the WebDriver at
`ADK_WEBDRIVER_URL`, else `http://localhost:4444`; `--allow-domain <host>` is the navigation
allowlist. Start a `chromedriver` on that port first.

## Computer-use pack

### Quick start

```toml
[dependencies]
adk-harness = { version = "3.0.0", features = ["computer-use"] }
adk-computer-use = "3.0.0"
adk-devtools = "3.0.0"
adk-core = "3.0.0"
anyhow = "1"
```

```rust
use std::sync::Arc;

use adk_computer_use::{ComputerUseRuntime, ScopeAuthorizer};
use adk_core::Llm;
use adk_harness::{ComputerUsePack, HarnessAgent};

/// A harness that may plan and perform background desktop actions through `runtime`.
fn desktop_harness(
    model: Arc<dyn Llm>,
    runtime: Arc<dyn ComputerUseRuntime>,
) -> anyhow::Result<HarnessAgent> {
    let authorizer = ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]);
    Ok(HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new("."))
        .pack_computer_use(ComputerUsePack::new(runtime, authorizer))
        .build()?)
}
```

The runtime is `adk_computer_use::ComputerUseMcpRuntime` over a running `computer-use-mcp`
server, or any `ComputerUseRuntime`. `Pack::ComputerUse` alone, and `adk-rust --pack
computer-use`, refuse at `build`: a runtime is a connection to a server, which only the library
API takes.

### The three tools

| Tool | Declaration | Class | Behaviour |
|---|---|---|---|
| `computer_use_observe` | `{}` | `Read` | The runtime's capabilities, a visual frame and a semantic frame, captured together |
| `computer_use_preview` | `{ action }` | `Read` | Observation, then the runtime's preview of `action`; returns `actionDigest`, `policyDigest`, `route` (`allowed`, `approval`, `blocked`) and a summary of the action; the preview is kept for the session |
| `computer_use_execute` | `{ actionDigest, policyDigest, action }` | `ComputerUse` | `Deny` in `read_only` and `plan`; a non-waivable one-shot approval in `ask` and `auto`. Runs the reference graph; a `request_approval` interrupt whose digests equal the approved arguments is resumed with the harness decision as the runtime-held authority; the graph checks the digests again before its single `execute_action`. Returns `status`, `receiptId`, `verified`, `committed` |

### Approvals

1. The model calls `computer_use_preview` and learns the digests and the action summary.
2. It calls `computer_use_execute` with all three unchanged. The harness holds the call: the
   prompt, or the approvals inbox entry, shows the digests and the echoed action.
3. The person approves. The call runs once: the graph observes, previews, interrupts for the
   digest-bound approval, which the harness decision answers, acquires the lease and executes.
4. The preview and the approval are spent; a second action needs a new preview and approval.

An approval of a misdescribed action runs nothing: before the graph starts, the pack compares the
echoed `action` with the kept preview and refuses a difference as
`harness.computer_use.preview_mismatch`. A digest the runtime no longer produces, or a preview this
process does not hold, is refused the same way. A standing rule, a `tool_approvals` entry or a hook
never authorizes a desktop action, and a team member never gets a `computer_use_*` tool.

### Audit records

| Record | Fields (`ext`) | When |
|---|---|---|
| `harness.computer_use.previewed` | `functionCallId`, `actionDigest`, `policyDigest`, `route`, `actionClass`, `requestedMode` | After each preview |
| `harness.computer_use.executed` | `functionCallId`, `actionDigest`, `policyDigest`, `receiptId`, `status`, `verified`, `committed`; or `refusal`, `reason` | After the graph ran, or at a refusal |

### Known limits

- A kept preview lives in the process that previewed it. A served harness whose process restarts
  between the preview and the approved execution refuses the execution; the model previews again.
- The pack passes the harness decision to the graph as the runtime-held authority
  (`runtimeApproved`), never a bearer grant. A `computer-use-mcp` server that requires a grant it
  issued itself refuses the action at its own boundary.

## Scenarios

`examples/harness_agent`:

| Scenario | What passes |
|---|---|
| `browser_tools_under_modes` (H10) | Every browser tool is decided as the table says in each mode; in `plan` a navigation to another host, a typed value and a click are denied and nothing is held; in `ask` the navigation is held, the prompt shows the host and offers the exact-host rule |
| `browser_form_fill_after_approval` (H10) | Against a local fixture page: the allowlisted navigation and the read run, the typing and the submit are each held and run once after approval, the page thanks the user, the fixture received exactly one submission, the result text is inside the untrusted block. Needs a WebDriver (`ADK_WEBDRIVER_URL`, `CHROMEDRIVER`, or `chromedriver` on `PATH`); the 3.0 `CodingAgent` with the same tools submits the form with nobody asked |
| `computer_use_digest_bound_approval` (H10) | With an in-process runtime: preview, held execution, approval, one mutation with the receipt audited; a changed digest and a misdescribed action run nothing |
| `live_browser_form_fill` | A live model reads the fixture page and fills its form; every action was approved first; the fixture received one submission |
| `live_computer_use_clipboard` | A live model previews and performs one clipboard write through the pack; one mutation, one approval, the receipt audited |
