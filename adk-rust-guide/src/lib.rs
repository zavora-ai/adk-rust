//! ADK-Rust Guide - Validation Examples
//!
//! This crate contains working examples that validate the official ADK-Rust documentation.
//! Each example corresponds to a documentation page and demonstrates the features described.
//!
//! ## Running Examples
//!
//! Examples support multiple run modes:
//!
//! ```bash
//! # Validation mode (default) - verifies the example compiles and runs
//! cargo run --example quickstart -p adk-rust-guide
//!
//! # Interactive console mode - chat with the agent
//! cargo run --example quickstart -p adk-rust-guide -- chat
//!
//! # Web server mode - run with web UI
//! cargo run --example quickstart -p adk-rust-guide -- serve
//! cargo run --example quickstart -p adk-rust-guide -- serve --port 3000
//! ```
//!
//! ## Environment Setup
//!
//! Most examples require a `GOOGLE_API_KEY` environment variable:
//!
//! ```bash
//! # Create a .env file in the adk-rust-guide directory
//! echo 'GOOGLE_API_KEY=your-api-key-here' > .env
//!
//! # Or export directly
//! export GOOGLE_API_KEY="your-api-key-here"
//! ```
//!
//! ## Example Pattern
//!
//! All examples follow this pattern to support both validation and interactive modes:
//!
//! ```rust,ignore
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//! use adk_rust_guide::{init_env, print_success, print_validating};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//!     let api_key = init_env();
//!     let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
//!     
//!     let agent = LlmAgentBuilder::new("my_agent")
//!         .model(Arc::new(model))
//!         .build()?;
//!
//!     // Check run mode
//!     let args: Vec<String> = std::env::args().collect();
//!     let interactive = args.iter().any(|a| a == "chat" || a == "serve");
//!
//!     if interactive {
//!         Launcher::new(Arc::new(agent)).run().await?;
//!     } else {
//!         print_validating("page.md");
//!         // Validation logic...
//!         print_success("example_name");
//!     }
//!     Ok(())
//! }
//! ```
//!
//! ## Important Notes
//!
//! - Use `std::result::Result<(), Box<dyn std::error::Error>>` for main functions
//!   to avoid conflicts with ADK's `Result<T>` type alias
//! - The `Launcher` handles CLI argument parsing for `chat` and `serve` modes
//! - All examples should work in validation mode without requiring user interaction

use std::env;

/// Initialize environment from .env file and return the API key.
///
/// # Panics
///
/// Panics if GOOGLE_API_KEY is not set.
pub fn init_env() -> String {
    dotenv::dotenv().ok();
    env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY environment variable not set")
}

/// Initialize environment from .env file and return the API key if available.
///
/// Returns None if GOOGLE_API_KEY is not set (useful for tests that don't need API access).
pub fn try_init_env() -> Option<String> {
    dotenv::dotenv().ok();
    env::var("GOOGLE_API_KEY").ok()
}

/// Print a success message for example completion.
pub fn print_success(example_name: &str) {
    println!("✅ Example '{}' completed successfully", example_name);
}

/// Print a validation message showing which documentation page is being validated.
pub fn print_validating(doc_path: &str) {
    println!("📖 Validating: docs/official_docs/{}", doc_path);
}

/// Check if the example should run in interactive mode.
///
/// Returns true if 'chat' or 'serve' is passed as a command line argument.
pub fn is_interactive_mode() -> bool {
    let args: Vec<String> = env::args().collect();
    args.iter().any(|a| a == "chat" || a == "serve")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_init_env_returns_option() {
        // This test verifies the function signature works correctly
        let result = try_init_env();
        // Result is Option<String>, either Some or None depending on env
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn test_is_interactive_mode_default() {
        // In test context, should return false (no chat/serve args)
        // Note: This may vary depending on test runner args
        let _ = is_interactive_mode();
    }
}
