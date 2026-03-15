//! Configuration for Ralph

use anyhow::Result;

/// Ralph configuration loaded from environment
#[derive(Debug, Clone)]
pub struct RalphConfig {
    pub prd_path: String,
    pub progress_path: String,
    pub max_iterations: u32,
    pub api_key: String,
    pub model_name: String,
}

impl RalphConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .unwrap_or_else(|_| "demo-key".to_string());

        Ok(Self {
            prd_path: std::env::var("RALPH_PRD_PATH").unwrap_or_else(|_| "prd.json".to_string()),
            progress_path: std::env::var("RALPH_PROGRESS_PATH")
                .unwrap_or_else(|_| "progress.txt".to_string()),
            max_iterations: std::env::var("RALPH_MAX_ITERATIONS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
            api_key,
            model_name: std::env::var("RALPH_MODEL")
                .unwrap_or_else(|_| "gemini-2.5-flash".to_string()),
        })
    }
}
