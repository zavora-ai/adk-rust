use anyhow::Result;
use std::sync::Arc;

pub struct Config {
    pub api_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY or GEMINI_API_KEY environment variable not set"))?;
        
        Ok(Self { api_key })
    }
}
