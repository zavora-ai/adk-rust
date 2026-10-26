//! The model a run's agents use: `ScriptedLlm` by default, a real model with `--live` (spec 3.5
//! §11 "Scripted runs", "Live").

use std::path::PathBuf;
use std::sync::Arc;

use adk_core::Llm;

/// The Anthropic live model the harness runs on.
pub const ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
/// The Gemini model examples default to.
pub const GEMINI_MODEL: &str = "gemini-3.7-flash";

/// How a scenario's agents think.
#[derive(Clone)]
pub enum Mode {
    /// Every agent turn is scripted; no keys, no network.
    Scripted,
    /// Agent turns run on a real model.
    Live {
        /// The model.
        model: Arc<dyn Llm>,
        /// Its name, for the report.
        name: String,
    },
}

impl Mode {
    /// The live model, when this is a live run.
    pub fn live_model(&self) -> Option<Arc<dyn Llm>> {
        match self {
            Self::Scripted => None,
            Self::Live { model, .. } => Some(model.clone()),
        }
    }

    /// The model's name, or `scripted`.
    pub fn name(&self) -> &str {
        match self {
            Self::Scripted => "scripted",
            Self::Live { name, .. } => name,
        }
    }

    /// Whether this is a live run.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live { .. })
    }
}

/// Loads the keys from `WEBSITE_ENV_FILE`, else the repository's `.env`, overriding the
/// environment; the keys are never printed.
pub fn load_env() {
    let path = std::env::var_os("WEBSITE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
    let _ = dotenvy::from_path_override(path);
}

/// The live model for `provider` (`anthropic` or `gemini`), or why there is none.
///
/// # Errors
///
/// When the provider's key is not set or the client refuses its configuration.
pub fn model_for(provider: &str) -> anyhow::Result<Mode> {
    match provider {
        "anthropic" => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY is not set"))?;
            let client = adk_model::anthropic::AnthropicClient::new(
                adk_model::anthropic::AnthropicConfig::new(key, ANTHROPIC_MODEL),
            )?;
            Ok(Mode::Live { model: Arc::new(client), name: ANTHROPIC_MODEL.into() })
        }
        "gemini" => {
            let key = std::env::var("GOOGLE_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY is not set"))?;
            let client = adk_model::GeminiModel::new(key, GEMINI_MODEL)?;
            Ok(Mode::Live { model: Arc::new(client), name: GEMINI_MODEL.into() })
        }
        other => anyhow::bail!("unknown provider {other}: use anthropic or gemini"),
    }
}
