//! # Durable Company
//!
//! A small autonomous accounts-payable process, run end to end on the durable runtime
//! against a real model: a cron schedule fires, a worker process running the clerk agent
//! is killed mid-run and another process resumes it, a payment over the policy threshold
//! parks the run until two approvers decide, the payment executes once through the
//! governed checkout under a daily spend cap, a kill switch stops further work, and every
//! process's signed audit log verifies with `adk-rust audit verify`.
//!
//! ```bash
//! cargo run --manifest-path examples/durable_company/Cargo.toml -- --provider openai
//! cargo run --manifest-path examples/durable_company/Cargo.toml -- --provider anthropic --store postgres
//! ```

mod company;
mod scenario;
mod stores;
mod worker;

use std::path::PathBuf;

use anyhow::{Context, bail};
use tracing_subscriber::EnvFilter;

use crate::company::Provider;
use crate::stores::{Backend, Stores};

/// `--name value` from `args`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|index| args.get(index + 1)).cloned()
}

fn backend(args: &[String], data_dir: &std::path::Path, tag: &str) -> anyhow::Result<Backend> {
    match flag(args, "--store").as_deref().unwrap_or("sqlite") {
        "memory" => Ok(Backend::Memory),
        "sqlite" => Ok(Backend::Sqlite {
            file: flag(args, "--sqlite")
                .unwrap_or_else(|| data_dir.join("company.db").display().to_string()),
        }),
        "postgres" => Ok(Backend::Postgres {
            url: flag(args, "--database-url")
                .or_else(|| std::env::var("DATABASE_URL").ok())
                .context("--store postgres needs DATABASE_URL or --database-url")?,
            schema: flag(args, "--schema").unwrap_or_else(|| format!("durable_company_{tag}")),
        }),
        other => bail!("unknown store '{other}'; use memory, sqlite, or postgres"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::var("ADK_ENV_FILE") {
        Ok(path) => {
            dotenvy::from_path_override(&path).with_context(|| format!("loading {path}"))?;
        }
        Err(_) => {
            // The example's own .env first, then one in the working directory or above.
            dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
            dotenvy::dotenv().ok();
        }
    }
    adk_core::ensure_crypto_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let provider = Provider::parse(flag(&args, "--provider").as_deref().unwrap_or("openai"))?;

    if args.first().map(String::as_str) == Some("worker") {
        // A replica process; the operator reads its log from <data-dir>/<worker-id>.log.
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .init();
        let data_dir = PathBuf::from(flag(&args, "--data-dir").context("--data-dir")?);
        let worker_id = flag(&args, "--worker-id").context("--worker-id")?;
        let stores = Stores::open(&backend(&args, &data_dir, "")?).await?;
        return worker::run_process(
            stores,
            provider,
            data_dir,
            worker_id,
            flag(&args, "--crash-marker").map(PathBuf::from),
        )
        .await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let tag = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let data_dir = match flag(&args, "--data-dir") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from("target").join("durable-company").join(&tag),
    };
    std::fs::create_dir_all(&data_dir)?;
    let data_dir = data_dir.canonicalize()?;
    let backend = backend(&args, &data_dir, &tag)?;

    println!("Durable Company: one accounts-payable day on the durable runtime\n");
    let evidence = scenario::run(backend, provider, data_dir.clone()).await?;
    println!("\nPASS: {} steps, evidence in {}", evidence.len(), data_dir.display());
    Ok(())
}
