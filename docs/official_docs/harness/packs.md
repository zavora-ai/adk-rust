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
| `harness.browser.acted` | `functionCallId`, `tool`, `outcome` (`executed`, `failed`, `refused`), `refusal`, `inputDigest`, `screening` | After each action, or at its refusal |
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

The runtime is any `ComputerUseRuntime`. `adk_computer_use::ComputerUseMcpRuntime` calls
governed tools that no released `computer-use-mcp` serves; see
[Governed Desktop Automation](../computer-use/index.md). `Pack::ComputerUse` alone, and `adk-rust --pack
computer-use`, refuse at `build`: a runtime is a connection to a server, which only the library
API takes.

### The three tools

| Tool | Declaration | Class | Behaviour |
|---|---|---|---|
| `computer_use_observe` | `{}` | `Read` | The runtime's capabilities, a visual frame and a semantic frame, captured together |
| `computer_use_preview` | `{ action }` or `{ actions }` | `Read` | Observation, then the runtime's preview of `action`; returns `actionDigest`, `policyDigest`, `route` (`allowed`, `approval`, `blocked`) and a summary of the action; the preview is kept for the session. With `actions` (up to 64, in the order they run), previews each one and returns `previews` in that order, each naming its `plan` and `step`, and `plans`: the [screen plans](#screen-plans-one-approval-per-plan) they form |
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

### Screen plans: one approval per plan

A copilot that fills a form makes a dozen desktop actions, and no standing rule may cover
`computer_use_execute`. A screen plan lets a person approve up to N previewed actions in one
window with one answer, as a [change set](modes-and-approvals.md#change-sets) does for edits (spec 3.7 Q8).

1. The model previews the actions together: `computer_use_preview { actions }`. Consecutive
   actions the runtime holds for approval, in one window, join one plan of at most N actions
   (`ComputerUsePack::with_plan_max_actions`, 10 by default). The window is the application,
   window and display of each step's target evidence; a step without a target, such as typing,
   joins the step before it. A longer run, a change of window, or an action the runtime allows
   or blocks starts a new plan; a lone step is approved on its own.
2. The plan's digest is its fingerprint: SHA-256 over the canonical JSON of its window and its
   steps — each step's action and policy digests, action summary, proposed action and previewed
   target.
3. The model runs the steps in order with `computer_use_execute`. At the first step, the
   prompter shows the plan's card (`ApprovalPrompter::prompt_screen_plan`): the window, then
   each step's operation, target in points and proposed action, escaped. One answer approves or
   denies every step; `harness.computer_use.plan_decided` records it.
4. Each later step whose arguments are exactly the plan's next step is answered from that
   decision, once. The approval adds `plan: { digest, step }` to the call's arguments, so the
   call's fingerprint and its execution bind the plan that was shown.
5. Before each step runs, the reference graph previews it again on a fresh observation, and at
   its approval interrupt the pack checks the fresh target against the previewed one: same
   window, same label, and each box covers the other's centre.

A step the plan's approval does not cover is held on its own and needs a fresh approval:

| Case | What happens |
|---|---|
| The fresh target moved, changed label, left the window or is gone | The step runs nothing (`harness.computer_use.plan_deviation`), the plan ends, and the step, previewed again, is held for a fresh approval |
| A step fails, does not verify, or runs without the graph's approval interrupt | The plan ends after it |
| A step out of order, a step already taken, or arguments that differ from the previewed step | Held on its own; a step asked about before the plan's first one ends the plan |
| An action of another plan, another window or another target | Held on its own, or shown in its own plan's card |
| A call whose `plan` names a plan it is not a step of | Refused, `harness.computer_use.plan_deviation`; nothing runs |
| The same actions previewed again | A new plan instance: its card is shown again |
| The person denies the plan | Every step is refused with the person's message; the turn's remaining calls are denied |

A prompter that shows one call at a time keeps the default `prompt_screen_plan`, which returns
`None`: each step is then asked about on its own. A plan is never a standing rule, a
`tool_approvals` entry or a hook decision.

#### On a surface that parks held calls

Without a live prompter, a held step parks in the approval inbox. `Harness::held_screen_plan`
tells a surface which plan a parked `computer_use_execute` is a step of — by its fingerprint,
so only a call with exactly the previewed arguments, agent and tool matches — and adk-surface
(feature `computer-use`) shows the plan's first step as one card for the whole plan (see
[Screen plans](../surfaces/index.md#screen-plans)).

| Step | What `Harness::decide_held_screen_plan` does |
|---|---|
| 1 | Checks the plan is still the one the card showed and that no one decided it |
| 2 | Settles the plan, then files the decision of each later step in the approval store under that step's fingerprint: an approval with the step's own binding, or the denial; the open inbox requests of later steps already held are withdrawn |
| 3 | Records `harness.computer_use.plan_decided` with `approvalId` and the person as the actor |
| 4 | Returns the decision for the first step: an edit that adds `plan: { digest, step: 1 }`, or the denial. The surface records it on the first step's request; when that fails, `revert_held_screen_plan` sets the plan back and takes the filed decisions back |

Each step still runs only in order and only after its fresh preview finds the target the card
showed. When the plan ends — a deviation, a failed or unverified step, a step out of order — or
the same actions are previewed again, the decisions filed for its remaining steps are taken
back, so each remaining step is held for a fresh card. `Harness::end_held_screen_plan` ends a
plan whose card a surface closes, as **Stop all** does.

### Audit records

| Record | Fields (`ext`) | When |
|---|---|---|
| `harness.computer_use.previewed` | `functionCallId`, `actionDigest`, `policyDigest`, `route`, `actionClass`, `requestedMode`; `planDigest`, `step` for a plan's step | After each preview |
| `harness.computer_use.plan_decided` | `planDigest`, `window`, `steps` (action digests), `functionCallId`, `answer` (`approve`, `deny`); `approvalId` for a card a surface parked, whose actor is the person | When a person answers a screen plan's card |
| `harness.computer_use.executed` | `functionCallId`, `actionDigest`, `policyDigest`, `receiptId`, `status`, `verified`, `committed`, and `planDigest`, `step` for a step run under a plan; or `refusal` (`previewMismatch`, `planDeviation` with `deviation`), `reason` | After the graph ran, or at a refusal |

### Known limits

- A kept preview lives in the process that previewed it. A served harness whose process restarts
  between the preview and the approved execution refuses the execution; the model previews again.
- The pack passes the harness decision to the graph as the runtime-held authority
  (`runtimeApproved`), never a bearer grant. A `computer-use-mcp` server that requires a grant it
  issued itself refuses the action at its own boundary.

## Vision pack

Feature `vision` adds `adk-vision`'s governed tools under the same modes: a call that stays
on the device is `Read`; one that sends an image off it is `External`, trusted read-only
under an `allow` rule — the default — and held in Ask and Auto with the exact image it would
send under a rule that asks. ReadOnly and Plan deny every call that sends an image off the
machine. `.adk/vision.toml` applies as written; `VisionPack::distrust_workspace_rules` makes
its `allow` rules ask. The image privacy guardrail runs on every request to a remote harness
model. See
[Governed Vision Tools](../vision/governed-tools.md#the-harness-vision-pack).

## Scenarios

`examples/harness_agent`:

| Scenario | What passes |
|---|---|
| `browser_tools_under_modes` (H10) | Every browser tool is decided as the table says in each mode; in `plan` a navigation to another host, a typed value and a click are denied and nothing is held; in `ask` the navigation is held, the prompt shows the host and offers the exact-host rule |
| `browser_form_fill_after_approval` (H10) | Against a local fixture page: the allowlisted navigation and the read run, the typing and the submit are each held and run once after approval, the page thanks the user, the fixture received exactly one submission, the result text is inside the untrusted block. Needs a WebDriver (`ADK_WEBDRIVER_URL`, `CHROMEDRIVER`, or `chromedriver` on `PATH`); the 3.0 `CodingAgent` with the same tools submits the form with nobody asked |
| `computer_use_digest_bound_approval` (H10) | With an in-process runtime: preview, held execution, approval, one mutation with the receipt audited; a changed digest and a misdescribed action run nothing |
| `live_browser_form_fill` | A live model reads the fixture page and fills its form; every action was approved first; the fixture received one submission |
| `live_computer_use_clipboard` | A live model previews and performs one clipboard write through the pack; one mutation, one approval, the receipt audited |
