use adk_model::openrouter::{
    OPENROUTER_API_BASE, OpenRouterApiMode, OpenRouterClient, OpenRouterConfig,
};
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_MODEL: &str = "openai/gpt-4.1-mini";
const DEFAULT_SITE_URL: &str = "https://github.com/zavora-ai/adk-rust";
const DEFAULT_APP_NAME: &str = "ADK-Rust OpenRouter Examples";

#[derive(Debug, Clone)]
pub struct ExampleConfig {
    pub model: String,
}

pub fn build_client(
    default_api_mode: OpenRouterApiMode,
) -> Result<(OpenRouterClient, ExampleConfig)> {
    let env_file_values = load_env_file_values();
    let api_key = required_value("OPENROUTER_API_KEY", &env_file_values)?;
    let config = ExampleConfig {
        model: optional_value("OPENROUTER_MODEL", &env_file_values)
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
    };

    let client = OpenRouterClient::new(
        OpenRouterConfig::new(api_key, &config.model)
            .with_base_url(
                optional_value("OPENROUTER_BASE_URL", &env_file_values)
                    .unwrap_or_else(|| OPENROUTER_API_BASE.to_string()),
            )
            .with_http_referer(
                optional_value("OPENROUTER_SITE_URL", &env_file_values)
                    .unwrap_or_else(|| DEFAULT_SITE_URL.to_string()),
            )
            .with_title(
                optional_value("OPENROUTER_APP_NAME", &env_file_values)
                    .unwrap_or_else(|| DEFAULT_APP_NAME.to_string()),
            )
            .with_default_api_mode(default_api_mode),
    )?;

    Ok((client, config))
}

pub fn print_section(title: &str) {
    println!("\n== {title} ==");
}

fn required_value(key: &str, env_file_values: &BTreeMap<String, String>) -> Result<String> {
    optional_value(key, env_file_values)
        .ok_or_else(|| anyhow!("missing {key}; set it in the shell or .env"))
}

fn optional_value(key: &str, env_file_values: &BTreeMap<String, String>) -> Option<String> {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env_file_values.get(key).cloned())
}

fn load_env_file_values() -> BTreeMap<String, String> {
    find_env_file()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|contents| parse_env_file(&contents))
        .unwrap_or_default()
}

fn find_env_file() -> Option<PathBuf> {
    let mut current = env::current_dir().ok()?;

    loop {
        let candidate = current.join(".env");
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn parse_env_file(contents: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        values.insert(key.trim().to_string(), normalize_env_value(value.trim()));
    }

    values
}

fn normalize_env_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quoted_with_double = bytes[0] == b'"' && bytes[value.len() - 1] == b'"';
        let quoted_with_single = bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'';

        if quoted_with_double || quoted_with_single {
            return value[1..value.len() - 1].to_string();
        }
    }

    value.to_string()
}
