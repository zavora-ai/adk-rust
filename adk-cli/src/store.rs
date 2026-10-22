//! The encrypted store in `adk-rust`: the keyring the harness commands open `harness.db`
//! with, `adk-rust store status|rotate-key`, and the first-run keychain offer
//! (spec 3.1 §7.12, §8.1, §9.2 I57).

use std::path::PathBuf;
use std::time::Instant;

use adk_harness::encrypted::{
    KEYCHAIN_ACCOUNT, SqliteStoreMarker, StoreKeySource, StoreKeyring, StoreRotation,
};
use adk_session::{EncryptedApprovalStore, EncryptedSession, EncryptionKey};
use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::StoreCommands;

/// The keychain service the first-run offer and `keychain:adk-rust` use.
pub const KEYCHAIN_SERVICE: &str = "adk-rust";
/// What the first-run offer writes to the user config.
const DEFAULT_SOURCE: &str = "keychain:adk-rust";

/// The user's harness config file.
fn user_config() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("adk-rust").join("harness.toml"))
}

/// `[store] key` of the user config, if set.
pub fn configured_source() -> Option<String> {
    let text = std::fs::read_to_string(user_config()?).ok()?;
    let table: toml::Table = text.parse().ok()?;
    table.get("store")?.get("key")?.as_str().map(str::to_string)
}

/// Parses a `--store-key` value, building the secret provider for `secret:<provider>/<name>`
/// when its feature is compiled in.
///
/// # Errors
///
/// Returns an error for an unknown form, or a secret provider this build leaves out.
pub async fn parse_source(text: &str) -> Result<StoreKeySource> {
    if let Some(rest) = text.trim().strip_prefix("secret:") {
        let Some((provider, name)) =
            rest.split_once('/').filter(|(p, n)| !p.is_empty() && !n.is_empty())
        else {
            bail!("secret: needs <provider>/<name>, for example secret:gcp/adk-store-key");
        };
        return secret_source(provider, name).await;
    }
    text.parse::<StoreKeySource>().map_err(anyhow::Error::msg)
}

#[allow(unused_variables)]
async fn secret_source(provider: &str, name: &str) -> Result<StoreKeySource> {
    match provider {
        #[cfg(feature = "secrets-gcp")]
        "gcp" => {
            let project = std::env::var("GOOGLE_CLOUD_PROJECT")
                .context("secret:gcp needs GOOGLE_CLOUD_PROJECT in the environment")?;
            let provider = adk_auth::secrets::GcpSecretProvider::new(project).await?;
            Ok(StoreKeySource::Secret {
                provider: std::sync::Arc::new(provider),
                name: name.into(),
            })
        }
        #[cfg(feature = "secrets-aws")]
        "aws" => {
            let provider = adk_auth::secrets::AwsSecretProvider::new().await;
            Ok(StoreKeySource::Secret {
                provider: std::sync::Arc::new(provider),
                name: name.into(),
            })
        }
        other => bail!(
            "secret:{other} is not available in this adk-cli build; reinstall with \
             `--features secrets-gcp` or `--features secrets-aws`, or use keychain:<entry> \
             or env:<VAR>"
        ),
    }
}

/// The keyring the harness commands open `harness.db` with: from `--store-key`, else
/// `[store] key` in the user config, else none. A `keychain:` source with no entry yet gets
/// a fresh key (the first run with the flag creates it). Returns the keyring and a banner
/// line describing the store.
///
/// # Errors
///
/// Returns an error when the source cannot be parsed or read.
pub async fn resolve_keyring(flag: Option<&str>) -> Result<(Option<StoreKeyring>, Option<String>)> {
    let Some(text) = flag.map(str::to_string).or_else(configured_source) else {
        return Ok((None, None));
    };
    let source = parse_source(&text).await?;
    let keyring = match StoreKeyring::resolve(&source).await {
        Ok(keyring) => keyring,
        Err(error)
            if error.code == "harness.store.key_unavailable"
                && error.category == adk_core::ErrorCategory::NotFound
                && matches!(source, StoreKeySource::Keychain { .. }) =>
        {
            let keyring = StoreKeyring::generate();
            keyring.store(&source)?;
            println!(
                "Created a store key in the OS keychain at {source}; harness.db is sealed under it."
            );
            keyring
        }
        Err(error) => return Err(error.into()),
    };
    let note = format!("encrypted under key {} from {source}", keyring.key_id());
    Ok((Some(keyring), Some(note)))
}

/// On the first run, offers to seal `harness.db` with a key in the OS keychain
/// (spec 3.1 §8.1); the choice is kept as `[store] key = "keychain:adk-rust"` in the user
/// config. Declining keeps the store in plaintext; `--store-key` sets it later.
pub fn offer_keychain_key() {
    use std::io::{IsTerminal, Write};
    if configured_source().is_some() || !std::io::stdin().is_terminal() {
        return;
    }
    println!();
    println!("  harness.db keeps your sessions, approvals, rules and long-term facts.");
    print!("  Encrypt it with a key kept in your OS keychain? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return;
    }
    if !matches!(answer.trim(), "" | "y" | "Y" | "yes") {
        println!(
            "  Kept in plaintext; `adk-rust store rotate-key --store-key keychain:adk-rust` seals it later.\n"
        );
        return;
    }
    let source = StoreKeySource::Keychain {
        service: KEYCHAIN_SERVICE.into(),
        account: KEYCHAIN_ACCOUNT.into(),
    };
    match StoreKeyring::generate().store(&source).and_then(|()| record_source(DEFAULT_SOURCE)) {
        Ok(()) => println!("  Store key created at {source}; harness.db is sealed from now on.\n"),
        Err(error) => {
            println!("  Could not create the store key ({error}); harness.db stays in plaintext.\n")
        }
    }
}

/// Writes `[store] key = "<source>"` to the user config, keeping its other keys.
fn record_source(source: &str) -> adk_core::Result<()> {
    let path = user_config().ok_or_else(|| adk_core::AdkError::config("no config directory"))?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut table: toml::Table = text.parse().map_err(|error| {
        adk_core::AdkError::config(format!("{} is not valid TOML: {error}", path.display()))
    })?;
    let store = table.entry("store").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(store) = store {
        store.insert("key".into(), toml::Value::String(source.into()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            adk_core::AdkError::config(format!("failed to create {}: {error}", parent.display()))
        })?;
    }
    std::fs::write(&path, table.to_string()).map_err(|error| {
        adk_core::AdkError::config(format!("failed to write {}: {error}", path.display()))
    })
}

/// `adk-rust store …`.
pub async fn run(command: StoreCommands, store_key: Option<String>) -> Result<()> {
    match command {
        StoreCommands::Status => status(store_key).await,
        StoreCommands::RotateKey { key } => rotate_key(store_key, key).await,
    }
}

/// The payload columns `status` and `rotate-key` report on: every store of `harness.db`.
const PAYLOADS: &[(&str, &str)] = &[
    (
        "sessions",
        "SELECT count(*), sum(CASE WHEN state LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM sessions",
    ),
    (
        "events",
        "SELECT count(*), sum(CASE WHEN llm_response LIKE '%adk-enc:v1:%' THEN 1 ELSE 0 END) FROM events",
    ),
    (
        "approvals (held)",
        "SELECT count(*), sum(CASE WHEN request LIKE '%adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_approval_pending",
    ),
    (
        "approvals (decided)",
        "SELECT count(*), sum(CASE WHEN approval LIKE '%adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_approval_decisions",
    ),
    (
        "modes",
        "SELECT count(*), sum(CASE WHEN record LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_harness_modes",
    ),
    (
        "rules",
        "SELECT count(*), sum(CASE WHEN rule LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_harness_rules",
    ),
    (
        "shell ledger",
        "SELECT count(*), sum(CASE WHEN entry LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_harness_shell_ledger",
    ),
    (
        "goals",
        "SELECT count(*), sum(CASE WHEN record LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_harness_goals",
    ),
    (
        "facts",
        "SELECT count(*), sum(CASE WHEN record LIKE 'adk-enc:v1:%' THEN 1 ELSE 0 END) FROM adk_harness_facts",
    ),
    (
        "plan checkpoints",
        "SELECT count(*), sum(CASE WHEN state LIKE '%adk-enc:v1:%' THEN 1 ELSE 0 END) FROM graph_checkpoints",
    ),
];

/// `adk-rust store status`: the key source, the key the database is sealed under, and per
/// store how many rows are envelopes.
async fn status(store_key: Option<String>) -> Result<()> {
    let source = store_key.or_else(configured_source);
    let keyring = match &source {
        Some(text) => match parse_source(text).await {
            Ok(source) => match StoreKeyring::resolve(&source).await {
                Ok(keyring) => {
                    println!("key source : {source}");
                    println!(
                        "key        : {} ({} previous)",
                        keyring.key_id(),
                        keyring.previous().len()
                    );
                    Some(keyring)
                }
                Err(error) => {
                    println!("key source : {source}");
                    println!("key        : unavailable — {}", error.message);
                    None
                }
            },
            Err(error) => {
                println!("key source : {text} (invalid: {error})");
                None
            }
        },
        None => {
            println!("key source : none (pass --store-key or set [store] key in the user config)");
            None
        }
    };
    let db = crate::sessions::open().await?;
    let pool = db.pool().clone();
    let marker = SqliteStoreMarker::new(pool.clone());
    marker.migrate().await?;
    match marker.key_id().await? {
        Some(id) => {
            let held = keyring.as_ref().is_some_and(|keyring| keyring.holds(&id));
            println!(
                "harness.db : sealed under key {id}{}",
                if held { "" } else { " — not in the configured keyring" }
            );
        }
        None => println!(
            "harness.db : not sealed (rows are plaintext until `adk-rust store rotate-key`)"
        ),
    }
    for (name, statement) in PAYLOADS {
        let (rows, sealed): (i64, Option<i64>) =
            match sqlx::query_as(*statement).fetch_one(&pool).await {
                Ok(row) => row,
                Err(_) => continue,
            };
        println!("  {name:<20} {rows:>6} rows, {:>6} sealed", sealed.unwrap_or(0));
    }
    Ok(())
}

/// `adk-rust store rotate-key`: re-seals every store under a new key, retires the current
/// one, seals rows written before any key, and records `harness.store.key_rotated`.
async fn rotate_key(store_key: Option<String>, key: Option<String>) -> Result<()> {
    let text = store_key.or_else(configured_source).context(
        "no store key source: pass --store-key keychain:adk-rust (or secret:<provider>/<name>, \
         env:<VAR>), or set [store] key in the user config",
    )?;
    let source = parse_source(&text).await?;
    let current = match StoreKeyring::resolve(&source).await {
        Ok(keyring) => Some(keyring),
        Err(error)
            if error.code == "harness.store.key_unavailable"
                && error.category == adk_core::ErrorCategory::NotFound =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let new_key = match key {
        Some(text) => EncryptionKey::from_base64(&text)?,
        None => EncryptionKey::generate(),
    };
    let rotated = match &current {
        Some(current) => current.rotated(new_key),
        None => StoreKeyring::new(new_key, Vec::new()),
    };

    let db = crate::sessions::open().await?;
    let pool = db.pool().clone();
    let marker = SqliteStoreMarker::new(pool.clone());
    marker.migrate().await?;
    marker.check(current.as_ref()).await?;
    let started = Instant::now();
    let mut stores = Vec::new();

    let sessions = EncryptedSession::new(
        adk_session::SqliteSessionService::from_pool(pool.clone()),
        rotated.current_key(),
        rotated.previous_keys(),
    )
    .with_legacy_migration(true);
    stores.push(("sessions".to_string(), sessions.rewrap_sqlite().await?));
    let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let approvals =
        EncryptedApprovalStore::new(approvals, rotated.current_key(), rotated.previous_keys())
            .with_legacy_migration(true);
    stores.push(("approvals".to_string(), approvals.rewrap_sqlite().await?));
    let modes = adk_harness::SqliteModeStore::new(pool.clone()).with_keyring(rotated.clone());
    modes.migrate().await?;
    stores.push(("modes".to_string(), modes.rewrap().await?));
    let rules =
        adk_harness::SqliteStandingRuleStore::new(pool.clone()).with_keyring(rotated.clone());
    rules.migrate().await?;
    stores.push(("rules".to_string(), rules.rewrap().await?));
    let ledger = adk_harness::SqliteShellLedger::new(pool.clone()).with_keyring(rotated.clone());
    ledger.migrate().await?;
    stores.push(("shell-ledger".to_string(), ledger.rewrap().await?));
    let goals = adk_harness::SqliteGoalStore::new(pool.clone()).with_keyring(rotated.clone());
    goals.migrate().await?;
    stores.push(("goals".to_string(), goals.rewrap().await?));
    let facts = adk_harness::SqliteFactStore::new(pool.clone()).with_keyring(rotated.clone());
    facts.migrate().await?;
    stores.push(("facts".to_string(), facts.rewrap().await?));
    let plans = adk_harness::SealedCheckpointer::new(
        std::sync::Arc::new(adk_graph::SqliteCheckpointer::from_pool(pool.clone()).await?),
        rotated.clone(),
    );
    stores.push(("plan-checkpoints".to_string(), plans.rewrap_sqlite(&pool).await?));
    marker.set_key_id(&rotated.key_id()).await?;
    adk_harness::encrypted::compact(&pool).await?;

    let rotation = StoreRotation {
        key_id: rotated.key_id(),
        previous_key_id: current.as_ref().map(StoreKeyring::key_id),
        stores,
        duration: started.elapsed(),
    };
    match rotated.store(&source) {
        Ok(()) => println!("stored the new keyring in {source}"),
        Err(error) if error.code == "harness.store.key_source_read_only" => {
            println!(
                "{source} is read-only: store this keyring document there before the next run \
                 (it holds the new key and the retired one):\n{}",
                rotated.to_document()
            );
        }
        Err(error) => return Err(error.into()),
    }
    let record = rotation.audit_record(adk_core::audit_log::AuditActor::subject(
        adk_core::principal::SubjectRef::human(crate::setup::os_user()),
    ));
    tracing::info!(
        key.id = %rotation.key_id,
        key.previous = ?rotation.previous_key_id,
        rows = rotation.rows(),
        duration.ms = rotation.duration.as_millis() as u64,
        "harness.store.key_rotated"
    );
    println!(
        "{}",
        json!({ "action": record.action, "ext": record.ext, "stores": rotation.stores, "rows": rotation.rows() })
    );
    println!(
        "re-sealed {} rows under key {} in {} ms; the previous key {} opens nothing now",
        rotation.rows(),
        rotation.key_id,
        rotation.duration.as_millis(),
        rotation.previous_key_id.as_deref().unwrap_or("(none: the store was plaintext)")
    );
    Ok(())
}
