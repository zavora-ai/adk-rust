//! The adk-harness walkthrough on Anthropic (spec 3.1 §5, §11).
//!
//! The scenarios are shared with `examples/harness_openai`; this crate supplies the model.
//!
//! ```text
//! cargo run                         # every scenario, scripted, no key
//! cargo run -- --live               # every scenario on claude-sonnet-5-5 (ANTHROPIC_API_KEY)
//! cargo run -- --live approvals     # one scenario
//! cargo run -- --kernel             # K2, K4 and K5 under an in-process kernel (spec 3.2)
//! cargo run -- --kernel --live      # the same on claude-sonnet-5-5
//! ```

use std::sync::Arc;

use adk_core::Llm;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

#[path = "../../harness_walkthrough/mod.rs"]
mod walkthrough;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    walkthrough::main(walkthrough::Provider {
        name: "anthropic",
        key_var: "ANTHROPIC_API_KEY",
        model_var: "ANTHROPIC_MODEL",
        default_model: "claude-sonnet-5-5",
        build: |key, model| {
            Ok(Arc::new(AnthropicClient::new(AnthropicConfig::new(key, model))?) as Arc<dyn Llm>)
        },
    })
    .await
}
