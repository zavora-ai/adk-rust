//! Persona registry for loading and managing persona definitions.

use std::collections::HashMap;
use std::path::Path;

use super::profile::PersonaProfile;
use crate::error::{EvalError, Result};

/// Loads and manages [`PersonaProfile`] definitions.
///
/// Supports loading profiles from a directory of JSON files and
/// looking them up by name.
///
/// # Example
///
/// ```rust,ignore
/// use adk_eval::personas::PersonaRegistry;
///
/// // Load all persona JSON files from a directory
/// let registry = PersonaRegistry::load_directory("personas/")?;
///
/// // Look up a specific persona
/// if let Some(persona) = registry.get("impatient-developer") {
///     println!("Found persona: {}", persona.name);
/// }
///
/// // List all personas
/// for persona in registry.list() {
///     println!("- {}: {}", persona.name, persona.description);
/// }
/// ```
pub struct PersonaRegistry {
    personas: HashMap<String, PersonaProfile>,
}

impl PersonaRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { personas: HashMap::new() }
    }

    /// Load all `.json` files from a directory and parse them as [`PersonaProfile`] definitions.
    ///
    /// Each JSON file in the directory should contain a single `PersonaProfile` object.
    /// Files that are not valid JSON or do not match the `PersonaProfile` schema are
    /// reported as errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read or if any JSON file
    /// fails to parse as a `PersonaProfile`.
    pub fn load_directory(dir: &Path) -> Result<Self> {
        let mut personas = HashMap::new();

        let entries = std::fs::read_dir(dir).map_err(|e| {
            EvalError::LoadError(format!("failed to read persona directory {}: {e}", dir.display()))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                EvalError::LoadError(format!("failed to read directory entry: {e}"))
            })?;

            let path = entry.path();

            // Only process .json files
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let contents = std::fs::read_to_string(&path).map_err(|e| {
                EvalError::LoadError(format!("failed to read {}: {e}", path.display()))
            })?;

            let profile: PersonaProfile = serde_json::from_str(&contents).map_err(|e| {
                EvalError::ParseError(format!("failed to parse {}: {e}", path.display()))
            })?;

            personas.insert(profile.name.clone(), profile);
        }

        Ok(Self { personas })
    }

    /// Look up a persona by name.
    pub fn get(&self, name: &str) -> Option<&PersonaProfile> {
        self.personas.get(name)
    }

    /// List all registered personas.
    pub fn list(&self) -> Vec<&PersonaProfile> {
        self.personas.values().collect()
    }

    /// Register a persona profile.
    pub fn register(&mut self, profile: PersonaProfile) {
        self.personas.insert(profile.name.clone(), profile);
    }
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self::new()
    }
}
