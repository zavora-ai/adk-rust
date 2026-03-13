use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{DeployError, DeployResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployClientConfig {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl DeployClientConfig {
    pub fn default_path() -> DeployResult<PathBuf> {
        let base = dirs::config_dir().ok_or_else(|| DeployError::Config {
            message: "could not determine config directory".to_string(),
        })?;
        Ok(base.join("adk-deploy").join("config.json"))
    }

    pub fn load() -> DeployResult<Self> {
        let path = Self::default_path()?;
        if !path.exists() {
            return Ok(Self {
                endpoint: "http://127.0.0.1:8090".to_string(),
                token: None,
                workspace_id: None,
            });
        }
        let raw = fs::read_to_string(path)?;
        serde_json::from_str(&raw)
            .map_err(|error| DeployError::Config { message: error.to_string() })
    }

    pub fn save(&self) -> DeployResult<()> {
        let path = Self::default_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_string_pretty(self)
            .map_err(|error| DeployError::Config { message: error.to_string() })?;
        fs::write(path, payload)?;
        Ok(())
    }
}
