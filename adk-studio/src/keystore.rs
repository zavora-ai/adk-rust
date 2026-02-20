//! Encrypted keystore for per-project API key storage.
//!
//! Keys are encrypted at rest using AES-256-GCM with a deterministic key derived
//! from the machine ID via HKDF-SHA256. Each project gets its own keystore file
//! (`{uuid}.keys`) stored alongside the project JSON.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs;

/// Salt used for HKDF key derivation — must remain constant across versions.
const HKDF_SALT: &[u8] = b"adk-studio-keystore-v1";

/// AES-GCM nonce size in bytes.
const NONCE_SIZE: usize = 12;

/// Known provider key names that should be treated as sensitive.
pub const KNOWN_PROVIDER_KEYS: &[&str] = &[
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "DEEPSEEK_API_KEY",
    "GROQ_API_KEY",
    "OLLAMA_HOST",
    "GITHUB_TOKEN",
    "SLACK_BOT_TOKEN",
];

/// The total length of a masked value string.
const MASKED_LENGTH: usize = 12;

/// The number of visible trailing characters in a masked value.
const VISIBLE_TAIL: usize = 4;

/// Check if a key name matches known sensitive provider key patterns.
///
/// Returns `true` if the name is in [`KNOWN_PROVIDER_KEYS`] or matches
/// common sensitive patterns (`*_API_KEY`, `*_TOKEN`, `*_SECRET`).
///
/// # Example
///
/// ```ignore
/// assert!(is_sensitive_key("OPENAI_API_KEY"));
/// assert!(is_sensitive_key("MY_CUSTOM_API_KEY"));
/// assert!(is_sensitive_key("SLACK_BOT_TOKEN"));
/// assert!(!is_sensitive_key("MY_SETTING"));
/// ```
pub fn is_sensitive_key(name: &str) -> bool {
    if KNOWN_PROVIDER_KEYS.contains(&name) {
        return true;
    }
    let upper = name.to_uppercase();
    upper.ends_with("_API_KEY") || upper.ends_with("_TOKEN") || upper.ends_with("_SECRET")
}

/// Mask a key value, showing only the last 4 characters preceded by bullet
/// characters. The total masked length is 12 characters.
///
/// For values shorter than 4 characters the entire value is masked with bullets.
///
/// # Example
///
/// ```ignore
/// assert_eq!(mask_value("sk-proj-abcd1234"), "••••••••1234");
/// assert_eq!(mask_value("ab"),               "••••••••••••");
/// ```
pub fn mask_value(value: &str) -> String {
    if value.len() < VISIBLE_TAIL {
        "•".repeat(MASKED_LENGTH)
    } else {
        let tail = &value[value.len() - VISIBLE_TAIL..];
        let bullet_count = MASKED_LENGTH - VISIBLE_TAIL;
        format!("{}{tail}", "•".repeat(bullet_count))
    }
}

/// Errors specific to keystore operations.
#[derive(Debug, Error)]
pub enum KeystoreError {
    #[error("Failed to obtain machine ID: {0}")]
    MachineId(String),

    #[error("Key derivation failed: {0}")]
    KeyDerivation(String),

    #[error("Encryption failed: {0}")]
    Encryption(String),

    #[error(
        "Decryption failed — the keystore may have been created on a different machine. \
         Re-enter your API keys to create a new keystore. Details: {0}"
    )]
    Decryption(String),

    #[error("IO error on keystore file {path}: {source}")]
    Io { path: String, source: std::io::Error },

    #[error("JSON serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, KeystoreError>;

/// Encrypted keystore for a single project.
pub struct Keystore {
    path: PathBuf,
    cipher: Aes256Gcm,
}

impl Keystore {
    /// Create a keystore for the given project UUID.
    ///
    /// Derives the encryption key from the machine ID and a static salt using
    /// HKDF-SHA256, producing a deterministic 256-bit key without requiring
    /// user-managed passwords.
    ///
    /// The keystore file is stored as `{project_id}.keys` inside `base_dir`.
    pub fn new(base_dir: &Path, project_id: uuid::Uuid) -> Result<Self> {
        let machine_id = get_machine_id()?;
        let cipher = derive_cipher(&machine_id)?;
        let path = base_dir.join(format!("{project_id}.keys"));
        Ok(Self { path, cipher })
    }

    /// Read and decrypt all keys from the keystore file.
    ///
    /// Returns an empty map if the file does not exist (Requirement 3.5).
    pub async fn load(&self) -> Result<HashMap<String, String>> {
        match fs::read(&self.path).await {
            Ok(data) => self.decrypt(&data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(KeystoreError::Io { path: self.path.display().to_string(), source: e }),
        }
    }

    /// Encrypt and write all keys to the keystore file.
    ///
    /// Creates the file with 0600 permissions on Unix (Requirement 10.3).
    pub async fn save(&self, keys: &HashMap<String, String>) -> Result<()> {
        let data = self.encrypt(keys)?;

        // Write to a temp file first, then rename for atomicity.
        let tmp_path = self.path.with_extension("keys.tmp");
        fs::write(&tmp_path, &data)
            .await
            .map_err(|e| KeystoreError::Io { path: tmp_path.display().to_string(), source: e })?;

        // Set restrictive permissions before rename (Unix only).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp_path, perms).await.map_err(|e| KeystoreError::Io {
                path: tmp_path.display().to_string(),
                source: e,
            })?;
        }

        fs::rename(&tmp_path, &self.path)
            .await
            .map_err(|e| KeystoreError::Io { path: self.path.display().to_string(), source: e })?;

        Ok(())
    }

    /// Add or update a single key in the keystore.
    pub async fn set(&self, name: &str, value: &str) -> Result<()> {
        let mut keys = self.load().await?;
        keys.insert(name.to_string(), value.to_string());
        self.save(&keys).await
    }

    /// Remove a single key from the keystore.
    ///
    /// No-op if the key does not exist.
    pub async fn remove(&self, name: &str) -> Result<()> {
        let mut keys = self.load().await?;
        keys.remove(name);
        self.save(&keys).await
    }

    /// Encrypt a key map into the wire format: `[12-byte nonce][ciphertext+tag]`.
    fn encrypt(&self, keys: &HashMap<String, String>) -> Result<Vec<u8>> {
        let plaintext = serde_json::to_vec(keys)?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|e| KeystoreError::Encryption(e.to_string()))?;

        let mut output = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    /// Decrypt the wire format back into a key map.
    fn decrypt(&self, data: &[u8]) -> Result<HashMap<String, String>> {
        if data.len() < NONCE_SIZE {
            return Err(KeystoreError::Decryption(
                "keystore file is too short to contain a valid nonce".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| KeystoreError::Decryption(e.to_string()))?;

        serde_json::from_slice(&plaintext).map_err(KeystoreError::from)
    }
}

/// Derive an AES-256-GCM cipher from the machine ID using HKDF-SHA256.
fn derive_cipher(machine_id: &str) -> Result<Aes256Gcm> {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), machine_id.as_bytes());
    let mut okm = [0u8; 32]; // 256 bits for AES-256
    hk.expand(b"aes-256-gcm-key", &mut okm)
        .map_err(|e| KeystoreError::KeyDerivation(e.to_string()))?;
    Aes256Gcm::new_from_slice(&okm).map_err(|e| KeystoreError::KeyDerivation(e.to_string()))
}

/// Obtain a stable machine identifier.
///
/// Uses the `machine-uid` crate which reads `/etc/machine-id` on Linux,
/// `IOPlatformUUID` via `ioreg` on macOS, or the Windows registry.
fn get_machine_id() -> Result<String> {
    machine_uid::get().map_err(|e| KeystoreError::MachineId(e.to_string()))
}

/// Migrate sensitive keys from project `env_vars` to the encrypted keystore.
///
/// Scans `project.settings.env_vars` for keys matching [`is_sensitive_key`],
/// writes them to the keystore (skipping keys already present), removes the
/// migrated entries from `env_vars`, and saves the updated project JSON.
///
/// This operation is idempotent — running it multiple times on the same project
/// produces the same result (Requirement 9.3).
///
/// # Returns
///
/// The list of key names that were migrated in this call.
///
/// # Errors
///
/// Returns an error if keystore operations or project saving fails.
pub async fn migrate_project_keys(
    storage: &crate::storage::FileStorage,
    keystore: &Keystore,
    project: &mut crate::schema::ProjectSchema,
) -> Result<Vec<String>> {
    // 1. Identify sensitive keys in env_vars
    let sensitive_entries: Vec<(String, String)> = project
        .settings
        .env_vars
        .iter()
        .filter(|(name, _)| is_sensitive_key(name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if sensitive_entries.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Load existing keystore to avoid overwriting (Requirement 9.4)
    let existing_keys = keystore.load().await?;

    // 3. Merge: only insert keys not already in the keystore
    let mut to_store = existing_keys;
    let mut migrated = Vec::new();

    for (name, value) in &sensitive_entries {
        if !to_store.contains_key(name) {
            to_store.insert(name.clone(), value.clone());
        }
        migrated.push(name.clone());
    }

    // 4. Save the updated keystore
    keystore.save(&to_store).await?;

    // 5. Remove migrated keys from env_vars
    for name in &migrated {
        project.settings.env_vars.remove(name);
    }

    // 6. Save the updated project JSON
    storage.save(project).await.map_err(|e| KeystoreError::Io {
        path: format!("project {}", project.id),
        source: std::io::Error::other(e.to_string()),
    })?;

    Ok(migrated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    async fn round_trip_encrypt_decrypt() {
        let dir = TempDir::new().unwrap();
        let ks = Keystore::new(dir.path(), Uuid::new_v4()).unwrap();

        let mut keys = HashMap::new();
        keys.insert("OPENAI_API_KEY".to_string(), "sk-test-1234".to_string());
        keys.insert("GOOGLE_API_KEY".to_string(), "AIza-abcd".to_string());

        ks.save(&keys).await.unwrap();
        let loaded = ks.load().await.unwrap();
        assert_eq!(keys, loaded);
    }

    #[tokio::test]
    async fn load_nonexistent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let ks = Keystore::new(dir.path(), Uuid::new_v4()).unwrap();
        let loaded = ks.load().await.unwrap();
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn set_and_remove() {
        let dir = TempDir::new().unwrap();
        let ks = Keystore::new(dir.path(), Uuid::new_v4()).unwrap();

        ks.set("MY_KEY", "secret").await.unwrap();
        let loaded = ks.load().await.unwrap();
        assert_eq!(loaded.get("MY_KEY").unwrap(), "secret");

        ks.remove("MY_KEY").await.unwrap();
        let loaded = ks.load().await.unwrap();
        assert!(!loaded.contains_key("MY_KEY"));
    }

    #[tokio::test]
    async fn empty_map_round_trip() {
        let dir = TempDir::new().unwrap();
        let ks = Keystore::new(dir.path(), Uuid::new_v4()).unwrap();

        let keys = HashMap::new();
        ks.save(&keys).await.unwrap();
        let loaded = ks.load().await.unwrap();
        assert!(loaded.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let ks = Keystore::new(dir.path(), Uuid::new_v4()).unwrap();

        ks.save(&HashMap::new()).await.unwrap();

        let metadata = std::fs::metadata(&ks.path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn decrypt_with_wrong_key_fails() {
        let dir = TempDir::new().unwrap();
        let project_id = Uuid::new_v4();
        let ks = Keystore::new(dir.path(), project_id).unwrap();

        let mut keys = HashMap::new();
        keys.insert("KEY".to_string(), "value".to_string());
        ks.save(&keys).await.unwrap();

        // Create a keystore with a different cipher (simulating a different machine)
        let different_cipher = derive_cipher("different-machine-id").unwrap();
        let ks2 = Keystore {
            path: dir.path().join(format!("{project_id}.keys")),
            cipher: different_cipher,
        };

        let result = ks2.load().await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Decryption failed"));
    }

    #[test]
    fn known_keys_are_sensitive() {
        for key in KNOWN_PROVIDER_KEYS {
            assert!(is_sensitive_key(key), "{key} should be sensitive");
        }
    }

    #[test]
    fn pattern_matching_api_key_suffix() {
        assert!(is_sensitive_key("MY_CUSTOM_API_KEY"));
        assert!(is_sensitive_key("some_api_key")); // case-insensitive
    }

    #[test]
    fn pattern_matching_token_suffix() {
        assert!(is_sensitive_key("GITHUB_TOKEN"));
        assert!(is_sensitive_key("my_bot_token"));
    }

    #[test]
    fn pattern_matching_secret_suffix() {
        assert!(is_sensitive_key("CLIENT_SECRET"));
        assert!(is_sensitive_key("app_secret"));
    }

    #[test]
    fn non_sensitive_keys() {
        assert!(!is_sensitive_key("MY_SETTING"));
        assert!(!is_sensitive_key("PORT"));
        assert!(!is_sensitive_key("DATABASE_URL"));
        assert!(!is_sensitive_key("LOG_LEVEL"));
    }

    #[test]
    fn mask_value_normal() {
        assert_eq!(mask_value("sk-proj-abcd1234"), "••••••••1234");
    }

    #[test]
    fn mask_value_exactly_4_chars() {
        assert_eq!(mask_value("abcd"), "••••••••abcd");
    }

    #[test]
    fn mask_value_short_value() {
        assert_eq!(mask_value("ab"), "••••••••••••");
        assert_eq!(mask_value("a"), "••••••••••••");
        assert_eq!(mask_value(""), "••••••••••••");
    }

    #[test]
    fn mask_value_length_is_always_12() {
        let long_key = "sk-very-long-api-key-value-here-1234567890";
        let masked = mask_value(long_key);
        assert_eq!(masked.chars().count(), 12);

        let short_key = "xyz";
        let masked = mask_value(short_key);
        assert_eq!(masked.chars().count(), 12);
    }

    /// Helper to create a project with given env_vars for migration tests.
    fn make_project_with_env_vars(
        id: Uuid,
        env_vars: HashMap<String, String>,
    ) -> crate::schema::ProjectSchema {
        let mut project = crate::schema::ProjectSchema::new("test-project");
        project.id = id;
        project.settings.env_vars = env_vars;
        project
    }

    #[tokio::test]
    async fn migrate_moves_sensitive_keys_to_keystore() {
        let dir = TempDir::new().unwrap();
        let storage = crate::storage::FileStorage::new(dir.path().to_path_buf()).await.unwrap();
        let project_id = Uuid::new_v4();
        let ks = Keystore::new(dir.path(), project_id).unwrap();

        let mut env_vars = HashMap::new();
        env_vars.insert("OPENAI_API_KEY".to_string(), "sk-test-123".to_string());
        env_vars.insert("MY_SETTING".to_string(), "some-value".to_string());

        let mut project = make_project_with_env_vars(project_id, env_vars);
        // Save the project first so FileStorage can find it
        storage.save(&project).await.unwrap();

        let migrated = migrate_project_keys(&storage, &ks, &mut project).await.unwrap();

        assert_eq!(migrated, vec!["OPENAI_API_KEY".to_string()]);
        // Sensitive key removed from env_vars
        assert!(!project.settings.env_vars.contains_key("OPENAI_API_KEY"));
        // Non-sensitive key remains
        assert_eq!(project.settings.env_vars.get("MY_SETTING").unwrap(), "some-value");
        // Key is in the keystore
        let stored = ks.load().await.unwrap();
        assert_eq!(stored.get("OPENAI_API_KEY").unwrap(), "sk-test-123");
    }

    #[tokio::test]
    async fn migrate_does_not_overwrite_existing_keystore_value() {
        let dir = TempDir::new().unwrap();
        let storage = crate::storage::FileStorage::new(dir.path().to_path_buf()).await.unwrap();
        let project_id = Uuid::new_v4();
        let ks = Keystore::new(dir.path(), project_id).unwrap();

        // Pre-populate keystore with a different value
        ks.set("OPENAI_API_KEY", "sk-existing-value").await.unwrap();

        let mut env_vars = HashMap::new();
        env_vars.insert("OPENAI_API_KEY".to_string(), "sk-env-value".to_string());

        let mut project = make_project_with_env_vars(project_id, env_vars);
        storage.save(&project).await.unwrap();

        let migrated = migrate_project_keys(&storage, &ks, &mut project).await.unwrap();

        // Key is reported as migrated (removed from env_vars)
        assert!(migrated.contains(&"OPENAI_API_KEY".to_string()));
        assert!(!project.settings.env_vars.contains_key("OPENAI_API_KEY"));
        // But keystore retains the original value (Req 9.4)
        let stored = ks.load().await.unwrap();
        assert_eq!(stored.get("OPENAI_API_KEY").unwrap(), "sk-existing-value");
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let storage = crate::storage::FileStorage::new(dir.path().to_path_buf()).await.unwrap();
        let project_id = Uuid::new_v4();
        let ks = Keystore::new(dir.path(), project_id).unwrap();

        let mut env_vars = HashMap::new();
        env_vars.insert("GOOGLE_API_KEY".to_string(), "AIza-xyz".to_string());
        env_vars.insert("PORT".to_string(), "8080".to_string());

        let mut project = make_project_with_env_vars(project_id, env_vars);
        storage.save(&project).await.unwrap();

        // First migration
        let migrated1 = migrate_project_keys(&storage, &ks, &mut project).await.unwrap();
        assert_eq!(migrated1, vec!["GOOGLE_API_KEY".to_string()]);
        let stored1 = ks.load().await.unwrap();

        // Second migration — no sensitive keys left, should be a no-op
        let migrated2 = migrate_project_keys(&storage, &ks, &mut project).await.unwrap();
        assert!(migrated2.is_empty());
        let stored2 = ks.load().await.unwrap();

        assert_eq!(stored1, stored2);
        assert_eq!(project.settings.env_vars.len(), 1);
        assert_eq!(project.settings.env_vars.get("PORT").unwrap(), "8080");
    }

    #[tokio::test]
    async fn migrate_no_sensitive_keys_is_noop() {
        let dir = TempDir::new().unwrap();
        let storage = crate::storage::FileStorage::new(dir.path().to_path_buf()).await.unwrap();
        let project_id = Uuid::new_v4();
        let ks = Keystore::new(dir.path(), project_id).unwrap();

        let mut env_vars = HashMap::new();
        env_vars.insert("PORT".to_string(), "3000".to_string());
        env_vars.insert("LOG_LEVEL".to_string(), "debug".to_string());

        let mut project = make_project_with_env_vars(project_id, env_vars.clone());
        storage.save(&project).await.unwrap();

        let migrated = migrate_project_keys(&storage, &ks, &mut project).await.unwrap();

        assert!(migrated.is_empty());
        assert_eq!(project.settings.env_vars, env_vars);
        let stored = ks.load().await.unwrap();
        assert!(stored.is_empty());
    }
}
