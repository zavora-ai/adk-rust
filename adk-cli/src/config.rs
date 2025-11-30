use anyhow::Result;

#[allow(dead_code)] // Part of CLI API, not currently used
pub struct Config {
    pub api_key: String,
}

impl Config {
    #[allow(dead_code)] // Part of CLI API, not currently used
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GEMINI_API_KEY"))
            .map_err(|_| {
                anyhow::anyhow!("GOOGLE_API_KEY or GEMINI_API_KEY environment variable not set")
            })?;

        Ok(Self { api_key })
    }
}
