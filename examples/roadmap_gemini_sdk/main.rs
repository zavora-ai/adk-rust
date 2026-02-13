//! Roadmap example: adk-gemini SDK surface for v1 + Vertex.
//!
//! Run with:
//!   cargo run --example roadmap_gemini_sdk
//!
//! Modes (ROADMAP_SDK_MODE):
//!   v1_api_key | vertex_api_key | vertex_adc | vertex_service_account | vertex_wif
//!
//! Optional:
//!   ROADMAP_RUN_EMBED=1 (default) to also run embedContent
//!   ROADMAP_PROMPT

#[cfg(feature = "vertex")]
use adk_gemini::client::extract_service_account_project_id;
use adk_gemini::{Gemini, GeminiBuilder, Model, TaskType};
use anyhow::{Context, Result, bail};
use std::env;
#[cfg(feature = "vertex")]
// Used to read the service account JSON key file, which is required for Vertex AI authentication.
use std::fs;

#[derive(Debug, Clone, Copy)]
enum SdkMode {
    V1ApiKey,
    #[cfg(feature = "vertex")]
    VertexApiKey,
    #[cfg(feature = "vertex")]
    VertexAdc,
    #[cfg(feature = "vertex")]
    VertexServiceAccount,
    #[cfg(feature = "vertex")]
    VertexWif,
}

impl SdkMode {
    fn from_env() -> Self {
        match env::var("ROADMAP_SDK_MODE")
            .unwrap_or_else(|_| "v1_api_key".to_string())
            .to_lowercase()
            .as_str()
        {
            #[cfg(feature = "vertex")]
            "vertex_api_key" => Self::VertexApiKey,
            #[cfg(feature = "vertex")]
            "vertex_adc" => Self::VertexAdc,
            #[cfg(feature = "vertex")]
            "vertex_service_account" => Self::VertexServiceAccount,
            #[cfg(feature = "vertex")]
            "vertex_wif" => Self::VertexWif,
            _ => Self::V1ApiKey,
        }
    }
}

fn google_api_key() -> Option<String> {
    env::var("GOOGLE_API_KEY").ok().or_else(|| env::var("GEMINI_API_KEY").ok())
}

#[cfg(feature = "vertex")]
fn project_id() -> Result<String> {
    env::var("GOOGLE_PROJECT_ID")
        .or_else(|_| env::var("GOOGLE_CLOUD_PROJECT"))
        .context("set GOOGLE_PROJECT_ID (or GOOGLE_CLOUD_PROJECT)")
}

#[cfg(feature = "vertex")]
fn cloud_location() -> String {
    env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".to_string())
}

#[cfg(feature = "vertex")]
fn read_json_value(json_var: &str, path_var: &str) -> Result<String> {
    if let Ok(value) = env::var(json_var) {
        return Ok(value);
    }
    let path = env::var(path_var).with_context(|| format!("set {} or {}", json_var, path_var))?;
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path))
}

fn build_generate_client(mode: SdkMode, model: String) -> Result<Gemini> {
    match mode {
        SdkMode::V1ApiKey => {
            let Some(api_key) = google_api_key() else {
                bail!("set GOOGLE_API_KEY (or GEMINI_API_KEY) for ROADMAP_SDK_MODE=v1_api_key");
            };
            Ok(GeminiBuilder::new(api_key).with_model(model).build()?)
        }
        #[cfg(feature = "vertex")]
        SdkMode::VertexApiKey => {
            let Some(api_key) = google_api_key() else {
                bail!("set GOOGLE_API_KEY (or GEMINI_API_KEY) for ROADMAP_SDK_MODE=vertex_api_key");
            };
            let pid = project_id()?;
            let location = cloud_location();
            Ok(GeminiBuilder::new(api_key)
                .with_model(model)
                .with_google_cloud(pid, location)
                .build()?)
        }
        #[cfg(feature = "vertex")]
        SdkMode::VertexAdc => {
            let pid = project_id()?;
            let location = cloud_location();
            Ok(GeminiBuilder::new_without_api_key()
                .with_model(model)
                .with_google_cloud(pid, location)
                .with_google_cloud_adc()?
                .build()?)
        }
        #[cfg(feature = "vertex")]
        SdkMode::VertexServiceAccount => {
            let service_account_json =
                read_json_value("GOOGLE_SERVICE_ACCOUNT_JSON", "GOOGLE_SERVICE_ACCOUNT_PATH")?;
            let pid = extract_service_account_project_id(&service_account_json)?;
            let location = cloud_location();
            Ok(GeminiBuilder::new_without_api_key()
                .with_model(model)
                .with_service_account_json(&service_account_json)?
                .with_google_cloud(pid, location)
                .build()?)
        }
        #[cfg(feature = "vertex")]
        SdkMode::VertexWif => {
            let wif_json = read_json_value("GOOGLE_WIF_JSON", "GOOGLE_WIF_PATH")?;
            let pid = project_id()?;
            let location = cloud_location();
            Ok(GeminiBuilder::new_without_api_key()
                .with_model(model)
                .with_google_cloud(pid, location)
                .with_google_cloud_wif_json(&wif_json)?
                .build()?)
        }
    }
}

fn build_embedding_client(mode: SdkMode, model: String) -> Result<Gemini> {
    // Re-use the same builder logic but with the embedding model
    build_generate_client(mode, model)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let mode = SdkMode::from_env();
    let model = env::var("GEMINI_MODEL").unwrap_or_else(|_| "models/gemini-2.5-flash".to_string());
    let prompt = env::var("ROADMAP_PROMPT")
        .unwrap_or_else(|_| "List two concrete advantages of v1 API stability.".to_string());
    let run_embed =
        env::var("ROADMAP_RUN_EMBED").unwrap_or_else(|_| "1".to_string()).to_lowercase();
    let run_embed = matches!(run_embed.as_str(), "1" | "true" | "yes");

    let client = build_generate_client(mode, model.clone())?;
    let response = client.generate_content().with_user_message(&prompt).execute().await?;

    println!("Mode: {:?}\nModel: {}", mode, model);
    println!("\nGenerate response:\n{}\n", response.text());

    if run_embed {
        let embedding_client = build_embedding_client(mode, Model::GeminiEmbedding001.to_string())?;
        match embedding_client
            .embed_content()
            .with_text("roadmap feature validation embedding probe")
            .with_task_type(TaskType::RetrievalDocument)
            .execute()
            .await
        {
            Ok(embedding) => {
                println!("Embedding succeeded. Length: {}", embedding.embedding.values.len());
            }
            Err(err) => {
                println!("Embedding call reached provider but returned error: {}", err);
            }
        }
    }

    Ok(())
}
