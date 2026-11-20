# A2UI Quickstart

This guide gives an agent the render tools of `adk-surface`'s `genui` feature, shows what it
draws in the web home, and reads a drawing as A2UI v0.9.1 JSON lines. A drawing never carries
approval controls: a form's **Send** asks the agent for the call, and the person decides on
the home's own card. [Agent-Drawn UI](surfaces/genui.md) covers the feature in full.

> **Note:** `adk-ui` 2, which this guide used to cover, is retired (spec 3.3 decision 11); `adk-ui` 3
> adds [extension catalogs](surfaces/genui.md#extension-catalogs) to `genui`. See
> [Moving from adk-ui 2](surfaces/genui.md#moving-from-adk-ui-2).

## 1) Add dependencies

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["web", "genui"] }
adk-agent = "3.0.0"
adk-model = "3.0.0"
adk-tool = "3.0.0"
anyhow = "1"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

## 2) Give the agent the render tools

```rust
use std::sync::Arc;

use adk_agent::{Agent, LlmAgentBuilder};
use adk_model::GeminiModel;
use adk_surface::genui::DrawToolset;
use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct InvoiceArgs {
    /// The address the invoice goes to
    to: String,
    /// The amount, in cents
    amount: u64,
}

/// Sends an invoice to an address.
#[tool]
async fn send_invoice(args: InvoiceArgs) -> std::result::Result<Value, AdkError> {
    Ok(json!({ "sent": true, "to": args.to, "amount": args.amount }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.8-flash")?);

    let agent = LlmAgentBuilder::new("finance")
        .model(model)
        .instruction("Show the person forms and tables with the render tools when they help.")
        .tool(Arc::new(SendInvoice))
        .require_tool_confirmation("send_invoice")
        .toolset(Arc::new(DrawToolset::forms()))
        .build()?;

    println!("{} has the render tools", agent.name());
    Ok(())
}
```

`DrawToolset::forms()` adds `render_form`, `render_card`, `render_table`, `render_screen`,
`patch_surface` and `delete_surface`, each checked against the `forms` 1.0 catalog. They are
read-only, so they need no approval. `require_tool_confirmation` holds each `send_invoice` call
until the person decides.

## 3) See a drawing in the web home

The `scripted_genui` example is a complete home over a kernel and a scripted model, with no
API key. Run it from a clone of the repository:

```bash
cargo run -p adk-surface --features web,genui --example scripted_genui -- 8805 /tmp/genui-home
```

1. Open `http://127.0.0.1:8805/ui/` and send Finance any message.
2. Finance draws the invoice form, then a screen with an "Allow once" button, which is refused:
   the thread shows "Finance tried to show something that looks like a decision. It wasn't
   shown."
3. Type an address and press **Send**. The form reads "Sent at 10:03 · waiting for you" and a
   decision waits under it.
4. Press **Allow once**. `send_invoice` runs once and appends its arguments to
   `/tmp/genui-home/sent.txt`; the form reads "Sent · done at 10:03".

`adk-rust serve` and `adk-rust up` built with `adk-cli`'s `genui` feature give every agent
they host the render tools and serve the same home.

## 4) Prompt → A2UI

Prompt the agent:

```
Prepare the October invoice for ACME
```

The model calls `render_form`. The tool checks the drawing and records it in its result under
`drawn` (`DrawnEnvelope`): the A2UI messages that draw the whole surface on the `forms` 1.0
catalog, and the actions its buttons ask for.

```
{"version":"v0.9.1","createSurface":{"surfaceId":"invoice-10","catalogId":"https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json"}}
{"version":"v0.9.1","updateComponents":{"surfaceId":"invoice-10","components":[{"id":"root","component":"Column",...}]}}
{"version":"v0.9.1","updateDataModel":{"surfaceId":"invoice-10","value":{"to":"","amount":4200}}}
```

## 5) Read and check A2UI in Rust

```rust,ignore
use adk_surface::genui::{DrawnEnvelope, Validator, encode_jsonl};

let envelope = DrawnEnvelope::from_response(&response).expect("a render tool's result");
println!("{}", encode_jsonl(&envelope.messages));

let draft = envelope.draft().expect("not a removal");
match Validator::forms().validate(&draft) {
    Ok(surface) => println!("{} strings a person reads", surface.texts.len()),
    Err(refusal) => println!("{}: {}", refusal.code(), refusal.message()),
}
```

`decode_jsonl` reads A2UI JSON lines back into `A2uiMessage`s.

## 6) Render it in your own client

`GET /api/surface/sessions/{session}/thread` returns each drawing as a `drawn` item: the
component tree with every string resolved and checked on the server, the fields with their
values and checks, and the actions with the tool each asks for. Render its strings as text.
A form is sent with the `submitDrawn` command at the version shown; see
[Sending a form](surfaces/genui.md#sending-a-form).

## Next steps

- [Agent-Drawn UI](surfaces/genui.md) — what a drawing may not contain, the requested call, the terminal
- [UI Tools](tools/ui-tools.md) — the render tools and the server's UI protocol profiles
