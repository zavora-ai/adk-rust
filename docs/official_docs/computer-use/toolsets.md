# Governed Computer Use and Browser Use

Claude's computer use toolset (`computer_toolset_20260801`) and browser use toolset
(`browser_toolset_20260801`) let a model click, type, scroll and read pages in an environment
your application runs. ADK-Rust runs each action the model asks for as an ordinary tool call, so
every action passes through the governed tool path before anything acts:

| Control | Applies to each action |
|---------|------------------------|
| Tool policy (`DeclarativePolicy`, `ToolPolicy`) | Allow, deny, or require approval per member (`computer.left_click`, `browser.navigate`), with argument predicates such as `DomainIn` on `/url` |
| Confirmations and standing approvals | A held action waits for a person or a recorded decision, bound to its exact arguments |
| Kill switch | `runner.governance().freeze(reason)` refuses every later action |
| Run budget | Each action counts as a tool call; a batch that does not fit runs nothing |
| Action ledger and audit trail | Each action is ledgered and its decision recorded, the skipped ones included |
| Egress policy | `browser.navigate` declares its destination; observations declare none |

Nothing a model decides overrides a deny: a refused, failed or held action stops the rest of the
model's turn. Claude often sends a batch — click a field, type, press Enter — in one turn, and
the later actions assume the earlier ones succeeded. The members share one ordered batch
(`Tool::batch_group`), so `LlmAgent` runs them in order, one at a time, and answers every action
after the first that does not complete with the halt text (`Not executed: an earlier computer
action in this turn failed.`) instead of running it. A denied click never leads to typing into
whatever field had focus.

## Browser use over WebDriver

`adk_browser::BrowserUseToolset` implements the browser use members over a WebDriver session
(ChromeDriver, geckodriver, Selenium). It offers the toolset's 27 default members;
`javascript_exec` and `file_upload` are offered only when enabled, and `read_console` and
`read_network`, which WebDriver cannot capture, are never offered.

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserUseToolset};
use adk_core::{DeclarativePolicy, RunConfig};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

fn build() -> Result<(adk_agent::LlmAgent, RunConfig), Box<dyn std::error::Error>> {
    let browser = Arc::new(BrowserSession::new(
        BrowserConfig::new().headless(true).viewport(1280, 800),
    ));
    let model = AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-sonnet-5-5",
    ))?;
    let agent = LlmAgentBuilder::new("researcher")
        .instruction("Answer from the pages you open. Read the page before you click.")
        .model(Arc::new(model))
        .toolset(Arc::new(BrowserUseToolset::new(browser)))
        .build()?;
    // Reading is free; opening a site outside the allowlist, and typing, need a person.
    let policy = DeclarativePolicy::builder()
        .allow("browser.read_page")
        .allow("browser.get_page_text")
        .allow("browser.screenshot")
        .require_approval("browser.type", "typing needs a person")
        .allow("browser.*")
        .build();
    Ok((agent, RunConfig::builder().tool_policy(Arc::new(policy)).build()))
}
```

| Member | How it runs |
|--------|-------------|
| `navigate` | `http` and `https` only; loopback, private, link-local and metadata addresses refused, also after redirects (`with_private_network_access` to allow them); `back`, `forward`, `reload` |
| `read_page`, `find` | An accessibility-style tree from the rendered page; each element gets a reference (`[ref_2]`) a later click, `form_input` or `scroll_to` targets; a stale reference is an error asking for a fresh read |
| `get_page_text` | The visible text of `main`, `article` or the body, up to 50,000 characters |
| Pointer members | A target is a viewport coordinate in screenshot pixels or a reference, scrolled into view first; WebDriver actions perform the click |
| `screenshot`, `zoom` | PNG of the viewport, resized when needed to fit Claude's image limits (2,576 px long edge, 4,784 visual tokens); `zoom` crops and scales a region |
| Tab members | Results carry the `browser_state` block; a click that opens a tab reports `tab_opened` |

## Computer use over an actuator

`adk_computer_use::ComputerUseToolset` implements the computer use members over an
`adk_core::ComputerActuator`, the trait an environment implements to perform a
`ComputerAction` (click, drag, scroll, type, key, screenshot, zoom). The toolset offers only the
members the actuator supports, bounds coordinates by the display, and returns screenshots as
images. Every action passes through the governed path, and an application hands the actuator
over explicitly.

| Actuator | Environment |
|----------|-------------|
| `adk_browser::WebDriverComputer` | A browser viewport as the display, over WebDriver; every action, `zoom` included |
| `adk_computer_use::McpComputerActuator` | A desktop, through a running `computer-use-mcp` server's plain tools; see [A desktop through computer-use-mcp](#a-desktop-through-computer-use-mcp) |
| `adk_vision::screen::FittedActuator` | Wraps any actuator: screenshots resized to the model's image limits with the scale kept, clicks and `zoom` regions scaled back, `zoom` cropped from a full-resolution capture when the inner actuator has none ([Screen Grounding](../vision/screen.md#claudes-computer-use-toolset)) |
| Your own `ComputerActuator` | A virtual desktop, a remote desktop session, or a device |

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, WebDriverComputer};
use adk_computer_use::{ComputerUseMember, ComputerUseToolset};
use adk_core::Llm;

fn build(model: Arc<dyn Llm>) -> Result<adk_agent::LlmAgent, adk_core::AdkError> {
    let browser = Arc::new(BrowserSession::new(BrowserConfig::new().viewport(1280, 800)));
    let computer = ComputerUseToolset::new(Arc::new(WebDriverComputer::new(browser)))
        .without(ComputerUseMember::HoldKey);
    LlmAgentBuilder::new("operator").model(model).toolset(Arc::new(computer)).build()
}
```

The model adapter declares the members as one toolset entry offering exactly the members the
agent has, at the version the model accepts: `computer_toolset_20260801` for Claude Sonnet 5.5,
Opus 5.5, Haiku 5.5 and Fable 5.2, and `computer_20251124` with the display size for Claude
Opus 4.7. See [Anthropic: Computer Use and Browser Use](../models/anthropic.md#computer-use-and-browser-use).

### A desktop through computer-use-mcp

`McpComputerActuator` performs each action with one of the server's plain tools.
`McpComputerActuator::builder(toolset).connect()` lists the server's tools and takes one
screenshot, which fixes the display size the toolset declares.

| Action | Server tool | Profile |
|--------|-------------|---------|
| Screenshot | `screenshot` of the whole display, as a PNG at `screenshot_width` pixels (1,280 by default) | every profile |
| Click, move, scroll, type, key, hold key, cursor position | `click`, `mouse_move`, `scroll`, `type`, `key`, `hold_key`, `cursor_position` | `core` and above |
| Drag, mouse down, mouse up | `mouse_drag` or `left_click_drag`, `left_mouse_down`, `left_mouse_up` | `ax` and above (`COMPUTER_USE_PROFILE=ax`) |
| Zoom | None: the server returns the region unscaled, and the toolset withholds `zoom` | — |

The model works in screenshot pixels, and the server takes logical screen points. Each screenshot
reply states its mapping (`screen_x = 0 + image_x * 2.0000`), and the actuator converts every
coordinate with the latest one. An action whose tool the server's profile does not list is
withheld from the model. The server's own guards still apply: input from the person, Pause or
Stop on its status card, and its policy refuse an action, and the refusal reaches the model as the
tool's error.

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_computer_use::{ComputerUseToolset, McpComputerActuator};
use adk_core::Llm;
use adk_tool::McpToolset;
use adk_tool::mcp::rmcp::{ServiceExt, transport::TokioChildProcess};
use tokio::process::Command;

async fn build(model: Arc<dyn Llm>) -> Result<adk_agent::LlmAgent, Box<dyn std::error::Error>> {
    let mut command = Command::new("npx");
    command.args(["-y", "@zavora-ai/computer-use-mcp"]).env("COMPUTER_USE_PROFILE", "ax");
    let server = Arc::new(McpToolset::new(().serve(TokioChildProcess::new(command)?).await?));

    let actuator = McpComputerActuator::builder(server).screenshot_width(1280).connect().await?;
    let computer = ComputerUseToolset::new(Arc::new(actuator));
    Ok(LlmAgentBuilder::new("operator").model(model).toolset(Arc::new(computer)).build()?)
}
```

> **Note:** `ComputerUseMcpRuntime`, which drives the [reference graph](index.md), calls governed
> tools (`preview_action`, `execute_action`, control leases, target reservations,
> `emergency_stop`) that no released `computer-use-mcp` serves. Use `McpComputerActuator` to act
> through the server.

## Security

Run the browser or desktop in a dedicated container or virtual machine with a fresh profile that
holds no credentials. Everything a page shows is untrusted input that can try to redirect the
model. The governed path decides what runs; isolation limits what a mistake can reach.

- Require approval for actions that buy, send, post or change accounts.
- Leave `javascript_exec` and `file_upload` off unless the task needs them; `file_upload` reads
  only under the directories `with_file_upload` names.
- With a run egress policy, `browser.navigate` is checked against it. Other acting members
  declare their network use as undeclared, which such a policy refuses, unless
  `with_egress(ToolEgress::Enforced)` says the environment's traffic goes through a proxy that
  enforces the policy.

### Remaining work

- `ComputerUseMcpRuntime` calls governed tools no released `computer-use-mcp` serves. Whether it
  becomes an adapter over the plain tools or is retired waits on the server's agent hand-off and
  control lease design.
- A desktop `ComputerActuator` that needs no server (X11, macOS, Windows): use
  `McpComputerActuator`, or supply your own actuator.
- `read_console` and `read_network` need a browser automation layer that captures console and
  network events, such as the Chrome DevTools Protocol.
