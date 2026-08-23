//! Configuration for Ollama client.

/// Configuration for connecting to an Ollama server.
#[derive(Debug, Clone)]
pub struct OllamaConfig {
    /// Ollama server host URL. Default: `http://localhost:11434`
    pub host: String,
    /// Model name to use (e.g., "qwen3.5", "mistral", "gemma4")
    pub model: String,
    /// Context window size (num_ctx). None uses model default.
    pub num_ctx: Option<u32>,
    /// Temperature for generation. None uses model default.
    pub temperature: Option<f32>,
    /// Top-p sampling. None uses model default.
    pub top_p: Option<f32>,
    /// Top-k sampling. None uses model default.
    pub top_k: Option<i32>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            host: "http://localhost:11434".to_string(),
            model: crate::catalog::OLLAMA_DEFAULT.to_string(),
            num_ctx: None,
            temperature: None,
            top_p: None,
            top_k: None,
        }
    }
}

impl OllamaConfig {
    /// Create a new config with just the model name, using default host.
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into(), ..Default::default() }
    }

    /// Create a config with custom host and model.
    pub fn with_host(host: impl Into<String>, model: impl Into<String>) -> Self {
        Self { host: host.into(), model: model.into(), ..Default::default() }
    }
}
