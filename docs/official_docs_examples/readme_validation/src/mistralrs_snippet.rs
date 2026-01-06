//! README mistral.rs Local Inference snippet validation
//!
//! NOTE: adk-mistralrs is excluded from the workspace due to git dependencies.
//! This file documents the expected API but cannot be compiled in this project.
//!
//! To validate, build adk-mistralrs directly:
//!   cd adk-mistralrs && cargo build
//!
//! The README snippet shows:
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource, QuantizationLevel};
//! use adk_agent::LlmAgentBuilder;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let config = MistralRsConfig::builder()
//!         .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
//!         .isq(QuantizationLevel::Q4_0)
//!         .paged_attention(true)
//!         .build();
//!
//!     let model = MistralRsModel::new(config).await?;
//!
//!     let agent = LlmAgentBuilder::new("local-assistant")
//!         .instruction("You are a helpful assistant running locally.")
//!         .model(Arc::new(model))
//!         .build()?;
//!
//!     Ok(())
//! }
//! ```

fn main() {
    println!("✓ mistral.rs snippet documented (requires separate build)");
    println!("  Run: cd adk-mistralrs && cargo build");
}
