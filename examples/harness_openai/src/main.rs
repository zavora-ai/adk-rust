//! The adk-harness walkthrough on OpenAI (spec 3.1 §5, §11).
//!
//! The scenarios are shared with `examples/harness_anthropic`; this crate supplies the model.
//!
//! ```text
//! cargo run                         # every scenario, scripted, no key
//! cargo run -- --live               # every scenario on gpt-5.6-terra (OPENAI_API_KEY)
//! cargo run -- --live approvals     # one scenario
//! cargo run -- --kernel             # K2, K4 and K5 under an in-process kernel (spec 3.2)
//! cargo run -- --kernel --live      # the same on gpt-5.6-terra
//! ```

use std::sync::Arc;

use adk_core::Llm;
use adk_model::openai::{OpenAIClient, OpenAIConfig};

#[path = "../../harness_walkthrough/mod.rs"]
mod walkthrough;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    walkthrough::main(walkthrough::Provider {
        name: "openai",
        key_var: "OPENAI_API_KEY",
        model_var: "OPENAI_MODEL",
        default_model: "gpt-5.6-terra",
        build: |key, model| {
            Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?) as Arc<dyn Llm>)
        },
    })
    .await
}
