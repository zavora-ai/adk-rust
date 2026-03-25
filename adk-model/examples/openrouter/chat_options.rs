use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

pub fn web_plugin_enabled() -> bool {
    optional_value("OPENROUTER_ENABLE_WEB_PLUGIN").is_some_and(|value| {
        matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
    })
}

fn optional_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| dotenv_values().get(key).cloned())
}

fn dotenv_values() -> BTreeMap<String, String> {
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
