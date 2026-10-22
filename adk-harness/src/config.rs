//! `HarnessConfig`: the project file `.adk/harness.toml` for a harness an application
//! builds (spec 3.1 §8.3).

use std::collections::BTreeMap;
use std::path::Path;

use adk_core::{AdkError, Result};
use serde::Deserialize;

use crate::mode::HarnessMode;

/// The project file, relative to the workspace root (spec 3.1 §8.3).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::HARNESS_CONFIG_FILE, ".adk/harness.toml");
/// ```
pub const HARNESS_CONFIG_FILE: &str = ".adk/harness.toml";

/// The `[deploy]` table is read by deployment tooling (spec 3.1 §8.5), not by the harness.
const DEPLOY_TABLE: &str = "deploy";

/// Keys `adk-rust` reads from its own config that a project file may not set.
const LOOSENING_KEYS: [&str; 5] = ["yes", "allow_unsandboxed", "trust", "packs", "members"];

/// What `.adk/harness.toml` asks of a harness (spec 3.1 §8.3), applied with
/// [`HarnessAgentBuilder::config`](crate::HarnessAgentBuilder::config).
///
/// The file sits in the workspace the agent works in, so it may only tighten
/// (spec 3.1 §9.2 I3): a stricter mode, the OS sandbox, and `fail_if_unavailable`. Keys
/// that would loosen what the agent may do (`yes`, `allow_unsandboxed`, `trust`, `packs`,
/// `members`) and unknown keys are ignored and reported in [`notes`](Self::notes); set
/// them in code. The `[deploy]` table is left to deployment tooling (spec 3.1 §8.5). An
/// unknown mode reads as `read_only` (spec 3.1 §9.2 I16).
///
/// ```toml
/// mode = "plan"
/// sandbox = true
/// fail_if_unavailable = true
///
/// [deploy]
/// mode = "plan"
/// yes_policy = "deny"
/// ```
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessConfig, HarnessMode};
///
/// let config = HarnessConfig::from_toml("mode = \"plan\"\nmembers = [\"x=acp:evil\"]\n")?;
/// assert_eq!(config.mode, Some(HarnessMode::Plan));
/// assert_eq!(config.notes.len(), 1);
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HarnessConfig {
    /// The mode the file asks for; applied only when it is stricter than the builder's.
    pub mode: Option<HarnessMode>,
    /// `sandbox = true`: run `bash` in the OS sandbox.
    pub sandbox: bool,
    /// `fail_if_unavailable = true`: refuse to build when the sandbox is unavailable.
    pub fail_if_unavailable: bool,
    /// Settings the file made that were ignored, and why.
    pub notes: Vec<String>,
}

/// The file as written; every key optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    mode: Option<String>,
    sandbox: Option<bool>,
    fail_if_unavailable: Option<bool>,
    /// `[[hooks]]` tables are read by `hooks::HookSet::load_workspace` (spec 3.1 §7.22),
    /// so they are neither applied here nor reported as unknown.
    #[serde(default)]
    #[allow(dead_code)]
    hooks: Vec<toml::Value>,
    /// The `[ultracode]` table is read by `adk-rust ultracode` (spec 3.1 §7.10), so it is
    /// neither applied here nor reported as unknown.
    #[serde(default)]
    #[allow(dead_code)]
    ultracode: Option<toml::Value>,
    #[serde(flatten)]
    other: BTreeMap<String, toml::Value>,
}

impl HarnessConfig {
    /// Reads `<dir>/.adk/harness.toml`; a missing file is the default configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// let dir = tempfile::tempdir().unwrap();
    /// let config = adk_harness::HarnessConfig::discover(dir.path())?;
    /// assert_eq!(config, adk_harness::HarnessConfig::default());
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the file cannot be read or is not valid TOML.
    pub fn discover(dir: impl AsRef<Path>) -> Result<Self> {
        let path = dir.as_ref().join(HARNESS_CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_toml(&text)
                .map_err(|error| AdkError::config(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => {
                Err(AdkError::config(format!("failed to read {}: {error}", path.display())))
            }
        }
    }

    /// Parses the contents of a `.adk/harness.toml`.
    ///
    /// # Example
    ///
    /// ```rust
    /// let config = adk_harness::HarnessConfig::from_toml("sandbox = true")?;
    /// assert!(config.sandbox);
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `text` is not valid harness configuration.
    pub fn from_toml(text: &str) -> Result<Self> {
        let file: ConfigFile = toml::from_str(text).map_err(|error| {
            AdkError::config(format!("not valid harness configuration: {error}"))
        })?;
        let mut notes = Vec::new();
        let mode = file.mode.as_deref().map(|value| {
            value.trim().parse::<HarnessMode>().unwrap_or_else(|_| {
                notes.push(format!(
                    "{HARNESS_CONFIG_FILE}: unknown mode `{value}` read as read_only; use \
                     read_only, plan, ask or auto"
                ));
                HarnessMode::ReadOnly
            })
        });
        for key in file.other.keys().filter(|key| *key != DEPLOY_TABLE) {
            notes.push(if LOOSENING_KEYS.contains(&key.as_str()) {
                format!(
                    "{HARNESS_CONFIG_FILE}: `{key}` ignored — it would loosen what the agent may \
                     do, and workspace files may only tighten. Set it in code"
                )
            } else {
                format!("{HARNESS_CONFIG_FILE}: unknown key `{key}` ignored")
            });
        }
        Ok(Self {
            mode,
            sandbox: file.sandbox.unwrap_or(false),
            fail_if_unavailable: file.fail_if_unavailable.unwrap_or(false),
            notes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_can_only_tighten_and_reports_what_it_ignored() {
        let config = HarnessConfig::from_toml(
            "mode = \"plan\"\nsandbox = true\nfail_if_unavailable = true\nyes = true\n\
             trust = true\nallow_unsandboxed = true\npacks = [\"payments\"]\n\
             members = [\"x=acp:evil\"]\nsurprise = 1\n\n[deploy]\nmode = \"plan\"\n",
        )
        .unwrap();
        assert_eq!(
            (config.mode, config.sandbox, config.fail_if_unavailable),
            (Some(HarnessMode::Plan), true, true)
        );
        for key in ["yes", "trust", "allow_unsandboxed", "packs", "members", "surprise"] {
            assert!(
                config.notes.iter().any(|note| note.contains(&format!("`{key}`"))),
                "{key}: {:?}",
                config.notes
            );
        }
        assert!(!config.notes.iter().any(|note| note.contains("deploy")), "{:?}", config.notes);
    }

    #[test]
    fn the_builder_applies_only_what_tightens() {
        let dir = tempfile::tempdir().unwrap();
        let build = |text: &str, mode: HarnessMode| {
            let model =
                adk_model::ScriptedLlm::new("scripted", Vec::<adk_model::ScriptedResponse>::new());
            crate::HarnessAgent::builder("harness")
                .model(std::sync::Arc::new(model))
                .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
                .project_instructions(crate::InstructionsConfig {
                    user_file: None,
                    ..crate::InstructionsConfig::default()
                })
                .mode(mode)
                .config(HarnessConfig::from_toml(text).unwrap())
                .build()
                .unwrap()
        };
        assert_eq!(
            build("mode = \"auto\"", HarnessMode::Ask).info().default_mode,
            HarnessMode::Ask
        );
        assert_eq!(
            build("mode = \"read_only\"", HarnessMode::Ask).info().default_mode,
            HarnessMode::ReadOnly
        );
        assert_eq!(build("", HarnessMode::Ask).info().isolation, crate::IsolationStatus::Host);
        #[cfg(feature = "sandbox")]
        assert_ne!(
            build("sandbox = true", HarnessMode::Ask).info().isolation,
            crate::IsolationStatus::Host,
            "sandbox = true must request the OS sandbox"
        );
    }

    #[test]
    fn the_ultracode_table_is_known() {
        let config = HarnessConfig::from_toml("[ultracode]\nmax_rounds = 1\n").unwrap();
        assert!(config.notes.is_empty(), "{:?}", config.notes);
    }

    #[test]
    fn an_unknown_mode_reads_as_read_only() {
        let config = HarnessConfig::from_toml("mode = \"yolo\"").unwrap();
        assert_eq!(config.mode, Some(HarnessMode::ReadOnly));
        assert_eq!(config.notes.len(), 1);
    }

    #[test]
    fn a_missing_file_is_the_default_and_a_broken_one_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(HarnessConfig::discover(dir.path()).unwrap(), HarnessConfig::default());
        std::fs::create_dir_all(dir.path().join(".adk")).unwrap();
        std::fs::write(dir.path().join(HARNESS_CONFIG_FILE), "mode = ").unwrap();
        let error = HarnessConfig::discover(dir.path()).unwrap_err();
        assert!(error.to_string().contains("harness.toml"), "{error}");
    }
}
