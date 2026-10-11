//! `adk-rust audit`: verify signed audit log exports and manage checkpoint keys.

use adk_auth::signed_audit::{
    AuditExport, AuditSigner, Ed25519Signer, FileCheckpointAnchor, NoteVerifier,
};
use anyhow::{Context, Result, bail};

use crate::cli::AuditCommands;

/// Execute an audit CLI command.
pub async fn run(command: AuditCommands) -> Result<()> {
    match command {
        AuditCommands::Verify { export, keys, key_file, checkpoints, json } => {
            if !verify(&export, keys, key_file.as_deref(), &checkpoints, json)? {
                std::process::exit(1);
            }
            Ok(())
        }
        #[cfg(feature = "audit-sql")]
        AuditCommands::Export { database_url, log_id, out } => {
            export(&database_url, &log_id, out.as_deref()).await
        }
        AuditCommands::Keygen { name, out } => keygen(&name, &out),
    }
}

fn verify(
    export_path: &str,
    mut keys: Vec<String>,
    key_file: Option<&str>,
    checkpoint_files: &[String],
    json: bool,
) -> Result<bool> {
    if let Some(path) = key_file {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        keys.extend(text.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from));
    }
    let trusted = keys
        .iter()
        .map(|key| {
            key.parse::<NoteVerifier>().with_context(|| format!("invalid verifier key '{key}'"))
        })
        .collect::<Result<Vec<_>>>()?;
    let text =
        std::fs::read_to_string(export_path).with_context(|| format!("reading {export_path}"))?;
    let mut export = AuditExport::from_jsonl(&text)
        .map_err(|(line, error)| anyhow::anyhow!("{export_path}:{line}: {error}"))?;
    for path in checkpoint_files {
        export.checkpoints.extend(
            FileCheckpointAnchor::read(path)
                .with_context(|| format!("reading checkpoints {path}"))?,
        );
    }
    let report = export.verify(&trusted);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(report.is_valid())
}

#[cfg(feature = "audit-sql")]
async fn export(database_url: &str, log_id: &str, out: Option<&str>) -> Result<()> {
    use adk_core::audit_log::AuditLogStore;
    let store: Box<dyn AuditLogStore> = if database_url.starts_with("sqlite:") {
        Box::new(adk_session::SqliteAuditLogStore::new(database_url).await?)
    } else if database_url.starts_with("postgres:") || database_url.starts_with("postgresql:") {
        Box::new(adk_session::PostgresAuditLogStore::new(database_url).await?)
    } else {
        bail!("unsupported database URL; use sqlite://... or postgres://...");
    };
    if !store.logs().await?.iter().any(|id| id == log_id) {
        bail!("no audit log '{log_id}' in the store");
    }
    let jsonl = AuditExport::from_store(store.as_ref(), log_id).await?.to_jsonl();
    match out {
        Some(path) => std::fs::write(path, jsonl).with_context(|| format!("writing {path}"))?,
        None => print!("{jsonl}"),
    }
    Ok(())
}

fn keygen(name: &str, out: &str) -> Result<()> {
    let signer = Ed25519Signer::generate(name)?;
    let key_path = format!("{out}.key");
    let vkey_path = format!("{out}.vkey");
    if std::path::Path::new(&key_path).exists() {
        bail!("{key_path} already exists; refusing to overwrite a signing key");
    }
    write_secret(&key_path, signer.to_note_key().as_str())?;
    std::fs::write(&vkey_path, format!("{}\n", signer.verifier_key()))
        .with_context(|| format!("writing {vkey_path}"))?;
    println!("signer key:   {key_path} (secret)");
    println!("verifier key: {}", signer.verifier_key());
    Ok(())
}

fn write_secret(path: &str, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).with_context(|| format!("creating {path}"))?;
    writeln!(file, "{contents}").with_context(|| format!("writing {path}"))?;
    Ok(())
}
