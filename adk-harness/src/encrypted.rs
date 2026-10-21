//! The encrypted store: one keyring for every harness store, resolved once at startup
//! (spec 3.1 §7.12, §8.1, §9.2 I57; feature `encrypted`).
//!
//! | Item | Role |
//! |---|---|
//! | [`StoreKeySource`] | Where the key comes from: a secret provider, the OS keychain, or an environment variable |
//! | [`StoreKeyring`] | The current key and the retired ones, as `EncryptedSession::new(inner, current, previous)` reads them |
//! | [`StoreEnvelope`] | Seals and opens one store's payloads under its own domain |
//! | `SqliteStoreMarker` (feature `sqlite`) | Records in `harness.db` which key the stores are sealed under, so a missing or wrong key fails the open |
//! | [`StoreRotation`] | What `adk-rust store rotate-key` did, and its `harness.store.key_rotated` record |
//!
//! Sessions wrap in `adk_session::EncryptedSession`, approvals in
//! `adk_session::EncryptedApprovalStore`, and the SQLite mode, rule, shell-ledger, goal and
//! fact stores take the keyring through `with_keyring`: every payload column holds an
//! `adk-enc:v1:` envelope and the identifier, timestamp and status columns stay plaintext for
//! indexing. With a key configured, a payload row that is not an envelope is an error, never
//! a plaintext fallback (I57).
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_harness::encrypted::{StoreKeySource, StoreKeyring};
//!
//! # async fn open() -> adk_core::Result<()> {
//! let keyring = StoreKeyring::resolve(&StoreKeySource::Env("ADK_STORE_KEY".into())).await?;
//! println!("store key {}", keyring.key_id());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_session::EncryptionKey;
use adk_session::envelope;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Domain separator of every envelope a harness store writes.
const AAD_DOMAIN: &[u8] = b"adk-harness/store/v1";

/// Where the store key is kept (spec 3.1 §7.12).
///
/// `--store-key keychain:<entry>|secret:<provider>/<name>|env:<VAR>` in the CLI; a library
/// caller builds the variant directly.
///
/// # Example
///
/// ```rust
/// use adk_harness::encrypted::StoreKeySource;
///
/// let source: StoreKeySource = "keychain:adk-rust".parse().unwrap();
/// assert_eq!(source, StoreKeySource::Keychain { service: "adk-rust".into(), account: "store-key".into() });
/// assert_eq!("env:ADK_STORE_KEY".parse::<StoreKeySource>().unwrap(), StoreKeySource::Env("ADK_STORE_KEY".into()));
/// ```
#[derive(Clone)]
pub enum StoreKeySource {
    /// A named secret in an `adk-auth` secret provider: the keyring document or a bare
    /// base64 key.
    #[cfg(feature = "auth")]
    Secret {
        /// The provider.
        provider: Arc<dyn adk_auth::secrets::SecretProvider>,
        /// The secret's name.
        name: String,
    },
    /// An entry in the OS keychain (`service`, `account`); writable, so rotation stores the
    /// new keyring there.
    Keychain {
        /// The keychain service.
        service: String,
        /// The keychain account.
        account: String,
    },
    /// An environment variable holding the keyring document or a bare base64 key.
    Env(String),
}

impl std::fmt::Debug for StoreKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string())
    }
}

impl PartialEq for StoreKeySource {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl std::fmt::Display for StoreKeySource {
    /// The flag form: `keychain:<service>` (the default account) or
    /// `keychain:<service>/<account>`, `secret:<name>`, `env:<VAR>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "auth")]
            Self::Secret { name, .. } => write!(f, "secret:{name}"),
            Self::Keychain { service, account } if account == KEYCHAIN_ACCOUNT => {
                write!(f, "keychain:{service}")
            }
            Self::Keychain { service, account } => write!(f, "keychain:{service}/{account}"),
            Self::Env(var) => write!(f, "env:{var}"),
        }
    }
}

/// The keychain account `keychain:<entry>` names when it gives only the service.
pub const KEYCHAIN_ACCOUNT: &str = "store-key";

impl std::str::FromStr for StoreKeySource {
    type Err = String;

    /// Parses `keychain:<entry>` (optionally `keychain:<service>/<account>`) and
    /// `env:<VAR>`. `secret:<provider>/<name>` needs a provider, so the CLI resolves it
    /// itself and this returns an error naming the form.
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let value = value.trim();
        if let Some(entry) = value.strip_prefix("keychain:") {
            let (service, account) = match entry.split_once('/') {
                Some((service, account)) if !account.trim().is_empty() => (service, account),
                _ => (entry, KEYCHAIN_ACCOUNT),
            };
            if service.trim().is_empty() {
                return Err("keychain: needs an entry name, for example keychain:adk-rust".into());
            }
            return Ok(Self::Keychain {
                service: service.trim().to_string(),
                account: account.trim().to_string(),
            });
        }
        if let Some(var) = value.strip_prefix("env:") {
            if var.trim().is_empty() {
                return Err("env: needs a variable name, for example env:ADK_STORE_KEY".into());
            }
            return Ok(Self::Env(var.trim().to_string()));
        }
        if value.starts_with("secret:") {
            return Err(
                "secret:<provider>/<name> needs a secret provider; the CLI builds one for the \
                 providers compiled in, a library caller passes StoreKeySource::Secret"
                    .into(),
            );
        }
        Err(format!(
            "unknown store key source '{value}'; use keychain:<entry>, secret:<provider>/<name> \
             or env:<VAR>"
        ))
    }
}

/// The keyring as it is written to a key source: base64 keys with their ids.
#[derive(Serialize, Deserialize)]
struct KeyringDocument {
    current: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    previous: Vec<String>,
}

fn store_error(
    code: &'static str,
    category: ErrorCategory,
    message: impl Into<String>,
) -> AdkError {
    AdkError::new(ErrorComponent::Session, category, code, message)
}

/// The current store key and the retired ones (spec 3.1 §7.12).
///
/// Writes seal with the current key; reads open with it, then with each previous key.
/// [`rotated`](Self::rotated) retires the current key to `previous` behind a new one, as
/// `adk-rust store rotate-key` does before re-sealing every store.
///
/// # Example
///
/// ```rust
/// use adk_harness::encrypted::StoreKeyring;
///
/// let keyring = StoreKeyring::generate();
/// let rotated = keyring.rotated(adk_session::EncryptionKey::generate());
/// assert_eq!(rotated.previous().len(), 1);
/// assert_eq!(rotated.previous_key_id().as_deref(), Some(keyring.key_id().as_str()));
/// let again = StoreKeyring::from_document(&rotated.to_document()).unwrap();
/// assert_eq!(again.key_id(), rotated.key_id());
/// ```
#[derive(Clone)]
pub struct StoreKeyring {
    current: Arc<EncryptionKey>,
    previous: Vec<Arc<EncryptionKey>>,
}

impl std::fmt::Debug for StoreKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreKeyring")
            .field("key_id", &self.key_id())
            .field("previous", &self.previous.len())
            .finish()
    }
}

impl StoreKeyring {
    /// A keyring with a fresh random current key and no previous keys.
    pub fn generate() -> Self {
        Self::new(EncryptionKey::generate(), Vec::new())
    }

    /// A keyring over `current` and the retired `previous` keys.
    pub fn new(current: EncryptionKey, previous: Vec<EncryptionKey>) -> Self {
        Self { current: Arc::new(current), previous: previous.into_iter().map(Arc::new).collect() }
    }

    /// Resolves the keyring from `source` once, at startup (spec 3.1 §7.12).
    ///
    /// The source holds either a keyring document (`{"current": "<base64>", "previous":
    /// [...]}`) or a bare base64 key.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.key_unavailable` when the source has no key, and
    /// `harness.store.key_invalid` when what it holds is neither a keyring document nor a
    /// 32-byte base64 key.
    pub async fn resolve(source: &StoreKeySource) -> Result<Self> {
        let text = match source {
            #[cfg(feature = "auth")]
            StoreKeySource::Secret { provider, name } => {
                provider.get_secret(name).await.map_err(|error| {
                    store_error(
                        "harness.store.key_unavailable",
                        ErrorCategory::Unavailable,
                        format!("the store key secret '{name}' could not be read: {error}"),
                    )
                })?
            }
            StoreKeySource::Keychain { service, account } => keychain_read(service, account)?,
            StoreKeySource::Env(var) => std::env::var(var).map_err(|_| {
                store_error(
                    "harness.store.key_unavailable",
                    ErrorCategory::NotFound,
                    format!(
                        "the store key variable {var} is not set; export it, or pass another \
                         --store-key source"
                    ),
                )
            })?,
        };
        Self::from_document(&text)
    }

    /// Parses a keyring document or a bare base64 key.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.key_invalid` when the text is neither.
    pub fn from_document(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.starts_with('{') {
            let document: KeyringDocument = serde_json::from_str(text).map_err(|error| {
                store_error(
                    "harness.store.key_invalid",
                    ErrorCategory::InvalidInput,
                    format!("the store keyring document does not parse: {error}"),
                )
            })?;
            let current = EncryptionKey::from_base64(&document.current).map_err(invalid_key)?;
            let previous = document
                .previous
                .iter()
                .map(|key| EncryptionKey::from_base64(key).map_err(invalid_key))
                .collect::<Result<Vec<_>>>()?;
            return Ok(Self::new(current, previous));
        }
        Ok(Self::new(EncryptionKey::from_base64(text).map_err(invalid_key)?, Vec::new()))
    }

    /// The keyring as the document a key source stores: the current key, then the
    /// previous ones, base64.
    pub fn to_document(&self) -> String {
        serde_json::to_string(&KeyringDocument {
            current: self.current.to_base64(),
            previous: self.previous.iter().map(|key| key.to_base64()).collect(),
        })
        .expect("a keyring document serializes")
    }

    /// Writes the keyring to `source`.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.key_source_read_only` for `Env` and `Secret` sources, which
    /// the harness never writes: the operator stores [`to_document`](Self::to_document)
    /// there.
    pub fn store(&self, source: &StoreKeySource) -> Result<()> {
        match source {
            StoreKeySource::Keychain { service, account } => {
                keychain_write(service, account, &self.to_document())
            }
            other => Err(store_error(
                "harness.store.key_source_read_only",
                ErrorCategory::Unsupported,
                format!(
                    "the harness does not write to {other}; store the new keyring document there \
                     yourself: `adk-rust store rotate-key` prints it"
                ),
            )),
        }
    }

    /// The current key.
    pub fn current(&self) -> &EncryptionKey {
        &self.current
    }

    /// The retired keys, newest first.
    pub fn previous(&self) -> Vec<&EncryptionKey> {
        self.previous.iter().map(Arc::as_ref).collect()
    }

    /// Clones of the retired keys, for `EncryptedSession::new` and
    /// `EncryptedApprovalStore::new`.
    pub fn previous_keys(&self) -> Vec<EncryptionKey> {
        self.previous.iter().map(|key| clone_key(key)).collect()
    }

    /// A clone of the current key, for `EncryptedSession::new` and
    /// `EncryptedApprovalStore::new`.
    pub fn current_key(&self) -> EncryptionKey {
        clone_key(&self.current)
    }

    /// The public id of the current key (`EncryptionKey::id`).
    pub fn key_id(&self) -> String {
        self.current.id()
    }

    /// The public id of the most recently retired key, if any.
    pub fn previous_key_id(&self) -> Option<String> {
        self.previous.first().map(|key| key.id())
    }

    /// Whether `id` names the current key or a previous one.
    pub fn holds(&self, id: &str) -> bool {
        self.current.id() == id || self.previous.iter().any(|key| key.id() == id)
    }

    /// This keyring with `new_current` in front and the current key retired to `previous`.
    #[must_use]
    pub fn rotated(&self, new_current: EncryptionKey) -> Self {
        let mut previous = vec![self.current.clone()];
        previous.extend(self.previous.iter().cloned());
        Self { current: Arc::new(new_current), previous }
    }

    /// Seals `plaintext` for `store` at `address` with the current key.
    ///
    /// # Errors
    ///
    /// Returns the envelope's `session.encryption.encrypt_failed`.
    pub fn seal(&self, store: &str, address: &[&str], plaintext: &[u8]) -> Result<String> {
        envelope::seal(&self.current, plaintext, &aad(store, address))
    }

    /// Opens an envelope sealed for `store` at `address`, with any key the ring holds, and
    /// reports whether the current key opened it.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.unencrypted` when `text` is not an envelope and
    /// `harness.store.decrypt_failed` when no key opens it: a wrong key fails the open
    /// (I57).
    pub fn open(&self, store: &str, address: &[&str], text: &str) -> Result<(Vec<u8>, bool)> {
        if !envelope::is_envelope(text) {
            return Err(store_error(
                "harness.store.unencrypted",
                ErrorCategory::InvalidInput,
                format!(
                    "the {store} store holds a row in plaintext while a store key is configured; \
                     seal existing rows with `adk-rust store rotate-key`"
                ),
            ));
        }
        let keys =
            std::iter::once(self.current.as_ref()).chain(self.previous.iter().map(Arc::as_ref));
        let (plaintext, index) =
            envelope::open(keys, text, &aad(store, address)).ok_or_else(|| {
                store_error(
                    "harness.store.decrypt_failed",
                    ErrorCategory::Unauthorized,
                    format!(
                        "the {store} store was written under a key this keyring does not hold (row \
                     {address:?}); pass the right --store-key"
                    ),
                )
            })?;
        Ok((plaintext, index == 0))
    }
}

fn clone_key(key: &EncryptionKey) -> EncryptionKey {
    EncryptionKey::from_bytes(key.as_bytes()).expect("a key clones")
}

fn invalid_key(error: AdkError) -> AdkError {
    store_error(
        "harness.store.key_invalid",
        ErrorCategory::InvalidInput,
        format!("the store key is not a base64 32-byte key: {}", error.message),
    )
}

fn aad(store: &str, address: &[&str]) -> Vec<u8> {
    let mut parts = Vec::with_capacity(address.len() + 1);
    parts.push(store);
    parts.extend_from_slice(address);
    envelope::associated_data(AAD_DOMAIN, &parts)
}

fn keychain_read(service: &str, account: &str) -> Result<String> {
    let entry = keyring::Entry::new(service, account).map_err(|error| {
        store_error(
            "harness.store.key_unavailable",
            ErrorCategory::Unavailable,
            format!("the OS keychain is unavailable for {service}/{account}: {error}"),
        )
    })?;
    match entry.get_password() {
        Ok(text) => Ok(text),
        Err(keyring::Error::NoEntry) => Err(store_error(
            "harness.store.key_unavailable",
            ErrorCategory::NotFound,
            format!(
                "no store key in the OS keychain at {service}/{account}; `adk-rust store \
                 rotate-key --store-key keychain:{service}` creates one"
            ),
        )),
        Err(error) => Err(store_error(
            "harness.store.key_unavailable",
            ErrorCategory::Unavailable,
            format!("the OS keychain refused {service}/{account}: {error}"),
        )),
    }
}

fn keychain_write(service: &str, account: &str, document: &str) -> Result<()> {
    keyring::Entry::new(service, account).and_then(|entry| entry.set_password(document)).map_err(
        |error| {
            store_error(
                "harness.store.key_unavailable",
                ErrorCategory::Unavailable,
                format!("the OS keychain refused to store {service}/{account}: {error}"),
            )
        },
    )
}

/// One store's envelope: its name under the keyring's domain (spec 3.1 §7.12).
///
/// The SQLite stores hold one of these when built `with_keyring`; the address is the row's
/// identifier columns, so a payload copied to another row fails to open.
///
/// # Example
///
/// ```rust
/// use adk_harness::encrypted::{StoreEnvelope, StoreKeyring};
///
/// let envelope = StoreEnvelope::new(StoreKeyring::generate(), "modes");
/// let sealed = envelope.seal(&["app", "alice", "s1"], b"{\"mode\":\"auto\"}").unwrap();
/// assert_eq!(envelope.open(&["app", "alice", "s1"], &sealed).unwrap().0, b"{\"mode\":\"auto\"}");
/// assert!(envelope.open(&["app", "alice", "s2"], &sealed).is_err());
/// ```
#[derive(Clone, Debug)]
pub struct StoreEnvelope {
    keyring: StoreKeyring,
    store: &'static str,
}

impl StoreEnvelope {
    /// The envelope of the store named `store` under `keyring`.
    pub fn new(keyring: StoreKeyring, store: &'static str) -> Self {
        Self { keyring, store }
    }

    /// The store's name.
    pub fn store(&self) -> &'static str {
        self.store
    }

    /// The keyring.
    pub fn keyring(&self) -> &StoreKeyring {
        &self.keyring
    }

    /// Seals `plaintext` for the row at `address`.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::seal`].
    pub fn seal(&self, address: &[&str], plaintext: &[u8]) -> Result<String> {
        self.keyring.seal(self.store, address, plaintext)
    }

    /// Seals `text` for the row at `address`.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::seal`].
    pub fn seal_str(&self, address: &[&str], text: &str) -> Result<String> {
        self.seal(address, text.as_bytes())
    }

    /// Opens the row at `address`; the flag says whether the current key opened it.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::open`].
    pub fn open(&self, address: &[&str], text: &str) -> Result<(Vec<u8>, bool)> {
        self.keyring.open(self.store, address, text)
    }

    /// Opens the row at `address` as text.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::open`]; `harness.store.decrypt_failed` when the plaintext is
    /// not UTF-8.
    pub fn open_str(&self, address: &[&str], text: &str) -> Result<(String, bool)> {
        let (bytes, current) = self.open(address, text)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            store_error(
                "harness.store.decrypt_failed",
                ErrorCategory::Internal,
                format!("the {} store's row {address:?} is not UTF-8", self.store),
            )
        })?;
        Ok((text, current))
    }

    /// `text` sealed again with the current key when a previous key sealed it; `None`
    /// when the current key already did.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::open`].
    pub fn reseal(&self, address: &[&str], text: &str) -> Result<Option<String>> {
        let (plaintext, current) = self.open(address, text)?;
        if current { Ok(None) } else { Ok(Some(self.seal(address, &plaintext)?)) }
    }

    /// [`reseal`](Self::reseal), and a row still in plaintext — written before the key was
    /// configured — sealed under the current key: what a rotation does to encrypt a store
    /// in place.
    ///
    /// # Errors
    ///
    /// See [`StoreKeyring::open`].
    pub fn reseal_or_seal(&self, address: &[&str], text: &str) -> Result<Option<String>> {
        if envelope::is_envelope(text) {
            self.reseal(address, text)
        } else {
            self.seal_str(address, text).map(Some)
        }
    }
}

/// Seals a SQLite store's payload row, or passes it through without a keyring.
#[cfg(feature = "sqlite")]
pub(crate) fn encode_row(
    envelope: Option<&StoreEnvelope>,
    address: &[&str],
    text: String,
) -> Result<String> {
    match envelope {
        Some(envelope) => envelope.seal_str(address, &text),
        None => Ok(text),
    }
}

/// Opens a SQLite store's payload row. Without a keyring, a row that is an envelope is
/// refused: a database written under a key never opens in plaintext (I57).
#[cfg(feature = "sqlite")]
pub(crate) fn decode_row(
    envelope: Option<&StoreEnvelope>,
    store: &str,
    address: &[&str],
    text: String,
) -> Result<String> {
    match envelope {
        Some(envelope) => Ok(envelope.open_str(address, &text)?.0),
        None if envelope::is_envelope(&text) => Err(store_error(
            "harness.store.key_required",
            ErrorCategory::Unauthorized,
            format!(
                "the {store} store was written under a store key; pass --store-key (or set \
                 [store] key in the user config) — it never opens in plaintext"
            ),
        )),
        None => Ok(text),
    }
}

/// Compacts a SQLite database after a rotation: `VACUUM` rewrites the file so pages the
/// re-sealed rows freed carry no plaintext, and the write-ahead log is checkpointed and
/// truncated (spec 3.1 §7.12, I57).
///
/// # Errors
///
/// Returns the database error.
#[cfg(feature = "sqlite")]
pub async fn compact(pool: &sqlx::SqlitePool) -> Result<()> {
    sqlx::query("VACUUM").execute(pool).await.map_err(crate::fsutil::db_error)?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .map_err(crate::fsutil::db_error)?;
    Ok(())
}

/// The marker row in `harness.db` naming the key the stores are sealed under
/// (spec 3.1 §8.1, I57): a database written under a key refuses to open without it, and
/// never falls back to plaintext.
///
/// # Example
///
/// ```rust,ignore
/// let marker = SqliteStoreMarker::new(pool);
/// marker.migrate().await?;
/// match marker.key_id().await? {
///     Some(id) if !keyring.holds(&id) => return Err(wrong_key(id)),
///     _ => {}
/// }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteStoreMarker {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteStoreMarker {
    /// A marker over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates the marker table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_store_meta (\
                name TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    /// The id of the key the stores are sealed under, if the database was written under one.
    ///
    /// # Errors
    ///
    /// Returns the database error.
    pub async fn key_id(&self) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM adk_harness_store_meta WHERE name = 'key_id'")
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        Ok(row.map(|(value,)| value))
    }

    /// Records `key_id` as the key the stores are sealed under.
    ///
    /// # Errors
    ///
    /// Returns the database error.
    pub async fn set_key_id(&self, key_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO adk_harness_store_meta (name, value) VALUES ('key_id', ?) \
             ON CONFLICT (name) DO UPDATE SET value = excluded.value",
        )
        .bind(key_id)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    /// Refuses to open a database written under a key `keyring` does not hold, or written
    /// under a key when `keyring` is `None` (I57).
    ///
    /// # Errors
    ///
    /// Returns `harness.store.key_required` when the database is sealed and no keyring is
    /// given, `harness.store.wrong_key` when the keyring holds neither the recorded key nor
    /// a previous one, or the database error.
    pub async fn check(&self, keyring: Option<&StoreKeyring>) -> Result<()> {
        match (self.key_id().await?, keyring) {
            (None, _) => Ok(()),
            (Some(id), None) => Err(store_error(
                "harness.store.key_required",
                ErrorCategory::Unauthorized,
                format!(
                    "harness.db is encrypted under key {id}; pass --store-key (or set [store] \
                     key in the user config) — it never opens in plaintext"
                ),
            )),
            (Some(id), Some(keyring)) if keyring.holds(&id) => Ok(()),
            (Some(id), Some(keyring)) => Err(store_error(
                "harness.store.wrong_key",
                ErrorCategory::Unauthorized,
                format!(
                    "harness.db is encrypted under key {id}, which the configured keyring \
                     ({}) does not hold",
                    keyring.key_id()
                ),
            )),
        }
    }
}

/// What a key rotation did (spec 3.1 §7.12, §9.3 `harness.store.key_rotated`).
///
/// # Example
///
/// ```rust
/// use adk_harness::encrypted::StoreRotation;
///
/// let rotation = StoreRotation {
///     key_id: "0123456789abcdef".into(),
///     previous_key_id: Some("fedcba9876543210".into()),
///     stores: vec![("sessions".into(), 12), ("facts".into(), 3)],
///     duration: std::time::Duration::from_millis(40),
/// };
/// assert_eq!(rotation.rows(), 15);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreRotation {
    /// The id of the new current key.
    pub key_id: String,
    /// The id of the key it retired, if the stores were sealed before.
    pub previous_key_id: Option<String>,
    /// Rows re-sealed per store.
    pub stores: Vec<(String, u64)>,
    /// How long the rotation took.
    pub duration: Duration,
}

impl StoreRotation {
    /// Rows re-sealed over every store.
    pub fn rows(&self) -> u64 {
        self.stores.iter().map(|(_, rows)| rows).sum()
    }

    /// The `harness.store.key_rotated` record: keyId, previousKeyId, stores, rows,
    /// durationMs (spec 3.1 §9.3). Never the key.
    pub fn audit_record(&self, actor: AuditActor) -> AuditRecordDraft {
        let stores: serde_json::Map<String, serde_json::Value> =
            self.stores.iter().map(|(name, rows)| (name.clone(), json!(rows))).collect();
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(crate::audit::HARNESS_AUDIT_KIND.to_string()),
            "harness.store.key_rotated",
        )
        .actor(actor)
        .ext("keyId", json!(self.key_id))
        .ext("stores", serde_json::Value::Object(stores))
        .ext("rows", json!(self.rows()))
        .ext("durationMs", json!(u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX)));
        if let Some(previous) = &self.previous_key_id {
            draft = draft.ext("previousKeyId", json!(previous));
        }
        draft
    }

    /// Records `harness.store.key_rotated` through `trail`.
    ///
    /// # Errors
    ///
    /// Returns the trail's error.
    pub async fn record(&self, trail: &AuditTrail, actor: AuditActor) -> Result<()> {
        trail.record(self.audit_record(actor)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_key_and_a_document_both_resolve_and_the_document_round_trips() {
        let key = EncryptionKey::generate();
        let bare = StoreKeyring::from_document(&key.to_base64()).unwrap();
        assert_eq!(bare.key_id(), key.id());
        assert!(bare.previous().is_empty());
        let rotated = bare.rotated(EncryptionKey::generate());
        let parsed = StoreKeyring::from_document(&rotated.to_document()).unwrap();
        assert_eq!(parsed.key_id(), rotated.key_id());
        assert_eq!(parsed.previous_key_id(), Some(key.id()));
        assert!(parsed.holds(&key.id()) && parsed.holds(&rotated.key_id()));
        assert_eq!(
            StoreKeyring::from_document("not a key").unwrap_err().code,
            "harness.store.key_invalid"
        );
        assert_eq!(
            StoreKeyring::from_document("{\"current\": 1}").unwrap_err().code,
            "harness.store.key_invalid"
        );
    }

    #[tokio::test]
    async fn env_sources_resolve_and_missing_ones_say_so() {
        let key = EncryptionKey::generate();
        // SAFETY: tests in this module touch one unique variable each.
        unsafe { std::env::set_var("ADK_HARNESS_TEST_STORE_KEY", key.to_base64()) };
        let keyring =
            StoreKeyring::resolve(&StoreKeySource::Env("ADK_HARNESS_TEST_STORE_KEY".into()))
                .await
                .unwrap();
        assert_eq!(keyring.key_id(), key.id());
        let missing =
            StoreKeyring::resolve(&StoreKeySource::Env("ADK_HARNESS_TEST_UNSET_KEY".into()))
                .await
                .unwrap_err();
        assert_eq!(missing.code, "harness.store.key_unavailable");
        assert_eq!(
            keyring.store(&StoreKeySource::Env("X".into())).unwrap_err().code,
            "harness.store.key_source_read_only"
        );
    }

    #[test]
    fn envelopes_are_bound_to_their_store_and_address_and_a_wrong_key_fails() {
        let keyring = StoreKeyring::generate();
        let modes = StoreEnvelope::new(keyring.clone(), "modes");
        let rules = StoreEnvelope::new(keyring.clone(), "rules");
        let sealed = modes.seal_str(&["app", "alice", "s1"], "payload").unwrap();
        assert_eq!(
            modes.open_str(&["app", "alice", "s1"], &sealed).unwrap(),
            ("payload".into(), true)
        );
        assert_eq!(
            rules.open(&["app", "alice", "s1"], &sealed).unwrap_err().code,
            "harness.store.decrypt_failed"
        );
        assert_eq!(
            modes.open(&["app", "alice", "s2"], &sealed).unwrap_err().code,
            "harness.store.decrypt_failed"
        );
        assert_eq!(
            modes.open(&["app", "alice", "s1"], "plain").unwrap_err().code,
            "harness.store.unencrypted"
        );
        let other = StoreEnvelope::new(StoreKeyring::generate(), "modes");
        assert_eq!(
            other.open(&["app", "alice", "s1"], &sealed).unwrap_err().code,
            "harness.store.decrypt_failed"
        );
        // After a rotation the old envelope opens under a previous key and reseals.
        let rotated = StoreEnvelope::new(keyring.rotated(EncryptionKey::generate()), "modes");
        assert_eq!(
            rotated.open_str(&["app", "alice", "s1"], &sealed).unwrap(),
            ("payload".into(), false)
        );
        let resealed = rotated.reseal(&["app", "alice", "s1"], &sealed).unwrap().unwrap();
        assert!(rotated.reseal(&["app", "alice", "s1"], &resealed).unwrap().is_none());
    }

    #[test]
    fn sources_parse_and_display_in_the_flag_form() {
        for text in ["keychain:adk-rust", "keychain:adk-rust/other", "env:ADK_STORE_KEY"] {
            let source: StoreKeySource = text.parse().unwrap();
            assert_eq!(source.to_string(), text);
        }
        assert!("secret:gcp/store-key".parse::<StoreKeySource>().is_err());
        assert!("nonsense".parse::<StoreKeySource>().is_err());
        assert!("keychain:".parse::<StoreKeySource>().is_err());
    }

    #[test]
    fn the_rotation_record_carries_ids_rows_and_duration_never_a_key() {
        let rotation = StoreRotation {
            key_id: "new".into(),
            previous_key_id: Some("old".into()),
            stores: vec![("sessions".into(), 2), ("facts".into(), 3)],
            duration: Duration::from_millis(12),
        };
        let draft = rotation.audit_record(AuditActor::default());
        assert_eq!(draft.action, "harness.store.key_rotated");
        assert_eq!(draft.ext["keyId"], "new");
        assert_eq!(draft.ext["previousKeyId"], "old");
        assert_eq!(draft.ext["rows"], 5);
        assert_eq!(draft.ext["durationMs"], 12);
        assert_eq!(draft.ext["stores"]["facts"], 3);
    }
}
