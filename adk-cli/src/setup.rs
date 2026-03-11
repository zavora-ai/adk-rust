//! First-run setup: interactive provider selection and secure API key storage.
//!
//! Config is stored at `~/.config/adk-rust/config.json`.

use crate::cli::ALL_PROVIDERS;
use adk_model::ModelProvider as Provider;
use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "adk-rust";

/// Persisted CLI configuration.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CliConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    /// Legacy plaintext key field kept only for migration from older configs.
    #[serde(default, rename = "api_key", skip_serializing)]
    pub legacy_api_key: Option<String>,
}

/// Where the config file lives.
fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine config directory")?.join("adk-rust");
    Ok(dir.join("config.json"))
}

/// Load saved config, or return default if none exists.
pub fn load_config() -> CliConfig {
    let Ok(path) = config_path() else {
        return CliConfig::default();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return CliConfig::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Save config to disk.
fn save_config(config: &CliConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let stripped = CliConfig {
        provider: config.provider.clone(),
        model: config.model.clone(),
        legacy_api_key: None,
    };
    let json = serde_json::to_string_pretty(&stripped)?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    println!("  Config saved to {}\n", path.display());
    Ok(())
}

fn keyring_entry(provider: Provider) -> Result<Entry> {
    Entry::new(KEYRING_SERVICE, &provider.to_string()).with_context(|| {
        format!("failed to initialize secure credential storage for {}", provider.display_name())
    })
}

fn load_api_key_from_keyring(provider: Provider) -> Result<Option<String>> {
    match keyring_entry(provider)?.get_password() {
        Ok(api_key) => Ok(Some(api_key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(anyhow::anyhow!(
            "failed to load {} API key from secure storage: {err}",
            provider.display_name()
        )),
    }
}

fn save_api_key_to_keyring(provider: Provider, api_key: &str) -> Result<()> {
    keyring_entry(provider)?.set_password(api_key).with_context(|| {
        format!("failed to save {} API key to secure storage", provider.display_name())
    })
}

fn migrate_legacy_api_key(saved: &CliConfig, provider: Provider) -> Result<()> {
    let Some(legacy_api_key) = saved.legacy_api_key.as_deref() else {
        return Ok(());
    };

    if !legacy_api_key.trim().is_empty() {
        save_api_key_to_keyring(provider, legacy_api_key)?;
    }

    save_config(saved)
}

/// Resolve the provider, model, and API key from CLI flags, env vars,
/// secure credential storage, saved config, or interactive setup.
pub struct ResolvedConfig {
    pub provider: Provider,
    pub model: String,
    pub api_key: Option<String>,
    pub instruction: String,
}

pub fn resolve(
    cli_provider: Option<Provider>,
    cli_model: Option<String>,
    cli_api_key: Option<String>,
    cli_instruction: Option<String>,
) -> Result<ResolvedConfig> {
    let saved = load_config();

    if let Some(saved_provider) = saved
        .provider
        .as_deref()
        .map(str::parse::<Provider>)
        .transpose()
        .map_err(anyhow::Error::msg)?
    {
        migrate_legacy_api_key(&saved, saved_provider)?;
    }

    // 1. Determine provider
    let provider = if let Some(p) = cli_provider {
        p
    } else if let Some(ref name) = saved.provider {
        name.parse::<Provider>().map_err(anyhow::Error::msg)?
    } else {
        // No provider anywhere — run interactive setup
        return interactive_setup(cli_instruction);
    };

    // 2. Determine model
    let model = cli_model.or(saved.model).unwrap_or_else(|| provider.default_model().to_string());

    // 3. Determine API key
    let api_key = if !provider.requires_key() {
        None
    } else {
        let env_api_key = std::env::var(provider.env_var()).ok();
        let alt_env_api_key = provider.alt_env_var().and_then(|v| std::env::var(v).ok());
        let key = resolve_api_key_sources(
            cli_api_key,
            env_api_key,
            alt_env_api_key,
            || load_api_key_from_keyring(provider),
            saved.legacy_api_key.clone(),
        )?;

        match key {
            Some(k) if !k.trim().is_empty() => Some(k),
            _ => {
                // Have a provider but no key — prompt for it
                let k = prompt_api_key(provider)?;
                if let Err(err) = save_api_key_to_keyring(provider, &k) {
                    println!(
                        "  Warning: could not persist your API key securely ({err}).\n  \
Use {} in your environment or re-enter the key next run.\n",
                        provider.env_var()
                    );
                }
                let config = CliConfig {
                    provider: Some(provider.to_string()),
                    model: Some(model.clone()),
                    legacy_api_key: None,
                };
                let _ = save_config(&config);
                Some(k)
            }
        }
    };

    let instruction = cli_instruction.unwrap_or_else(default_instruction);

    Ok(ResolvedConfig { provider, model, api_key, instruction })
}

fn resolve_api_key_sources<F>(
    cli_api_key: Option<String>,
    env_api_key: Option<String>,
    alt_env_api_key: Option<String>,
    load_secure_key: F,
    legacy_api_key: Option<String>,
) -> Result<Option<String>>
where
    F: FnOnce() -> Result<Option<String>>,
{
    if let Some(api_key) = cli_api_key {
        return Ok(Some(api_key));
    }

    if let Some(api_key) = env_api_key {
        return Ok(Some(api_key));
    }

    if let Some(api_key) = alt_env_api_key {
        return Ok(Some(api_key));
    }

    if let Some(api_key) = load_secure_key()? {
        return Ok(Some(api_key));
    }

    Ok(legacy_api_key)
}

/// Interactive first-run setup.
fn interactive_setup(cli_instruction: Option<String>) -> Result<ResolvedConfig> {
    println!();
    println!("  Welcome to ADK-Rust! Let's set up your LLM provider.\n");
    println!("  Choose a provider:\n");

    for (i, p) in ALL_PROVIDERS.iter().enumerate() {
        println!("    {}) {:<35} default: {}", i + 1, p.display_name(), p.default_model());
    }
    println!();

    let provider = loop {
        print!("  Enter number (1-{}): ", ALL_PROVIDERS.len());
        let _ = io::stdout().flush();
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim();
        if let Ok(n) = trimmed.parse::<usize>() {
            if n >= 1 && n <= ALL_PROVIDERS.len() {
                break ALL_PROVIDERS[n - 1];
            }
        }
        println!("  Invalid choice, try again.");
    };

    let model = provider.default_model().to_string();

    let api_key = if provider.requires_key() { Some(prompt_api_key(provider)?) } else { None };

    // Save config
    let config = CliConfig {
        provider: Some(provider.to_string()),
        model: Some(model.clone()),
        legacy_api_key: None,
    };
    let _ = save_config(&config);

    if let Some(api_key) = api_key.as_deref() {
        if let Err(err) = save_api_key_to_keyring(provider, api_key) {
            println!(
                "  Warning: could not persist your API key securely ({err}).\n  \
Use {} in your environment or re-enter the key next run.\n",
                provider.env_var()
            );
        }
    }

    let instruction = cli_instruction.unwrap_or_else(default_instruction);

    Ok(ResolvedConfig { provider, model, api_key, instruction })
}

/// Prompt the user for an API key.
fn prompt_api_key(provider: Provider) -> Result<String> {
    let env_hint = provider.env_var();
    println!();
    println!("  {} requires an API key.", provider.display_name());
    println!("  (You can also set {} in your environment.)", env_hint);
    println!();

    loop {
        print!("  API key: ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let trimmed = input.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
        println!("  Key cannot be empty.");
    }
}

fn default_instruction() -> String {
    "\
You are a helpful AI assistant powered by ADK-Rust (Rust Agent Development Kit).

You are knowledgeable, concise, and practical. Answer questions directly. \
When writing code, prefer Rust unless the user asks for another language.

When the user asks about ADK-Rust — its APIs, agents, tools, models, sessions, \
graph workflows, or any ADK-Rust feature — always refer to the official \
documentation at https://adk-rust.com/en/docs for accurate, up-to-date information. \
Search or fetch from that site rather than guessing.

Use tools when they help: search the web for current events, fetch URLs the user \
provides, and ground your answers in real sources."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{CliConfig, resolve_api_key_sources};
    use anyhow::Result;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn cli_config_does_not_serialize_plaintext_api_keys() {
        let config = CliConfig {
            provider: Some("openai".to_string()),
            model: Some("gpt-4.1".to_string()),
            legacy_api_key: Some("sk-secret".to_string()),
        };

        let json = serde_json::to_string(&config).expect("config should serialize");
        assert!(!json.contains("api_key"));
        assert!(!json.contains("sk-secret"));
    }

    #[test]
    fn cli_config_can_read_legacy_plaintext_api_key_field() {
        let config: CliConfig = serde_json::from_str(
            r#"{"provider":"gemini","model":"gemini-2.5-flash","api_key":"legacy-secret"}"#,
        )
        .expect("legacy config should deserialize");

        assert_eq!(config.provider.as_deref(), Some("gemini"));
        assert_eq!(config.model.as_deref(), Some("gemini-2.5-flash"));
        assert_eq!(config.legacy_api_key.as_deref(), Some("legacy-secret"));
    }

    #[test]
    fn explicit_api_key_skips_secure_storage_lookup() {
        let keyring_called = Arc::new(AtomicBool::new(false));
        let called = keyring_called.clone();

        let key = resolve_api_key_sources(
            Some("cli-key".to_string()),
            None,
            None,
            move || -> Result<Option<String>> {
                called.store(true, Ordering::SeqCst);
                Ok(Some("keyring-key".to_string()))
            },
            Some("legacy-key".to_string()),
        )
        .expect("api key resolution should succeed");

        assert_eq!(key.as_deref(), Some("cli-key"));
        assert!(!keyring_called.load(Ordering::SeqCst));
    }
}
