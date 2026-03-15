//! Roadmap example: Vertex auth modes via adk-model GeminiModel constructors.
//!
//! Run with:
//!   cargo run --example roadmap_vertex_auth
//!
//! Modes (ROADMAP_VERTEX_MODE):
//!   api_key | adc | service_account | wif
//!
//! Required env by mode:
//!   api_key:
//!     GOOGLE_API_KEY (or GEMINI_API_KEY), GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)
//!   adc:
//!     GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)
//!   service_account:
//!     GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT) and one of:
//!       GOOGLE_SERVICE_ACCOUNT_JSON
//!       GOOGLE_SERVICE_ACCOUNT_PATH
//!   wif:
//!     GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT) and one of:
//!       GOOGLE_WIF_JSON
//!       GOOGLE_WIF_PATH
//!
//! Optional:
//!   GOOGLE_CLOUD_LOCATION (default: us-central1)
//!   GEMINI_MODEL (default: gemini-2.5-flash)

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::{GeminiModel, RetryConfig};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::{env, fs};

#[derive(Debug, Clone, Copy)]
enum VertexAuthMode {
    ApiKey,
    Adc,
    ServiceAccount,
    Wif,
}

impl VertexAuthMode {
    fn from_env() -> Self {
        match env::var("ROADMAP_VERTEX_MODE")
            .unwrap_or_else(|_| "adc".to_string())
            .to_lowercase()
            .as_str()
        {
            "api_key" => Self::ApiKey,
            "service_account" => Self::ServiceAccount,
            "wif" => Self::Wif,
            _ => Self::Adc,
        }
    }
}

fn google_api_key() -> Option<String> {
    env::var("GOOGLE_API_KEY").ok().or_else(|| env::var("GEMINI_API_KEY").ok())
}

fn project_id() -> Result<String> {
    env::var("GOOGLE_PROJECT_ID")
        .or_else(|_| env::var("GOOGLE_CLOUD_PROJECT"))
        .context("set GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)")
}

fn read_json_value(json_var: &str, path_var: &str) -> Result<String> {
    if let Ok(value) = env::var(json_var) {
        return Ok(value);
    }
    let path = env::var(path_var).with_context(|| format!("set {} or {}", json_var, path_var))?;
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path))
}

async fn call_once(model: &GeminiModel, prompt: &str) -> Result<String> {
    let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text(prompt)]);
    let mut stream = model.generate_content(request, false).await?;
    let mut output = String::new();

    while let Some(chunk) = stream.next().await {
        let response = chunk?;
        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    output.push_str(&text);
                }
            }
        }
    }

    Ok(output)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let mode = VertexAuthMode::from_env();
    let project_id = project_id()?;
    let location = env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".to_string());
    let model_name = env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
    let prompt = env::var("ROADMAP_PROMPT").unwrap_or_else(|_| {
        "Explain in two bullets why SDK-backed Vertex auth paths reduce maintenance risk."
            .to_string()
    });

    let mut model = match mode {
        VertexAuthMode::ApiKey => {
            let Some(api_key) = google_api_key() else {
                bail!("set GOOGLE_API_KEY (or GEMINI_API_KEY) for ROADMAP_VERTEX_MODE=api_key");
            };
            GeminiModel::new_google_cloud(api_key, &project_id, &location, &model_name)?
        }
        VertexAuthMode::Adc => {
            GeminiModel::new_google_cloud_adc(&project_id, &location, &model_name)?
        }
        VertexAuthMode::ServiceAccount => {
            let service_account_json =
                read_json_value("GOOGLE_SERVICE_ACCOUNT_JSON", "GOOGLE_SERVICE_ACCOUNT_PATH")?;
            GeminiModel::new_google_cloud_service_account(
                &service_account_json,
                &project_id,
                &location,
                &model_name,
            )?
        }
        VertexAuthMode::Wif => {
            let wif_json = read_json_value("GOOGLE_WIF_JSON", "GOOGLE_WIF_PATH")?;
            GeminiModel::new_google_cloud_wif(&wif_json, &project_id, &location, &model_name)?
        }
    };

    let retry = RetryConfig::default().with_max_retries(3);
    println!(
        "Mode: {:?}\nProject: [configured]\nLocation: {}\nModel: {}",
        mode, location, model_name
    );
    println!("Retry: enabled={}, max_retries={}", retry.enabled, retry.max_retries);
    model.set_retry_config(retry);

    let response = call_once(&model, &prompt).await?;
    println!("\nResponse:\n{}\n", response);
    Ok(())
}
