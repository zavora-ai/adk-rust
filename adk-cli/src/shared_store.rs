//! `adk-rust serve --store <url>`: every store a served harness keeps, in one backend that
//! every replica shares (spec 3.1 §8.2, §13 Q2, Q5).

use std::sync::Arc;

use adk_core::{ApprovalStore, DurableRunStore};
use adk_graph::Checkpointer;
use adk_harness::{ModeStore, StandingRuleStore, redact_store_url};
use adk_session::SessionService;
use anyhow::{Result, bail};

/// The stores of a served harness in one backend: sessions, approvals, durable runs, modes,
/// standing rules and plan checkpoints.
pub struct SharedStores {
    /// The store URL with its password redacted, for the banner.
    pub label: String,
    pub sessions: Arc<dyn SessionService>,
    pub modes: Arc<dyn ModeStore>,
    pub rules: Arc<dyn StandingRuleStore>,
    pub approvals: Arc<dyn ApprovalStore>,
    pub runs: Arc<dyn DurableRunStore>,
    pub checkpointer: Arc<dyn Checkpointer>,
}

/// Opens and migrates the standing-rule store at `url`, for `adk-rust rules --store`.
pub async fn open_rules(url: &str) -> Result<Arc<dyn StandingRuleStore>> {
    Ok(open(url).await?.rules)
}

/// Opens and migrates every store at `url`: `postgres://…` or `redis://…`.
pub async fn open(url: &str) -> Result<SharedStores> {
    let label = redact_store_url(url);
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        Some("postgres" | "postgresql") => postgres(url, label).await,
        Some("redis" | "rediss") => redis(url, label).await,
        _ => bail!(
            "--store takes a postgres:// or redis:// URL, which every replica shares; {label} is \
             neither"
        ),
    }
}

#[cfg(feature = "postgres")]
async fn postgres(url: &str, label: String) -> Result<SharedStores> {
    use anyhow::Context as _;
    let pool = sqlx::PgPool::connect(url)
        .await
        .with_context(|| format!("could not connect to the store at {label}"))?;
    let sessions = adk_session::PostgresSessionService::from_pool(pool.clone());
    sessions.migrate().await?;
    let modes = adk_harness::PostgresModeStore::new(pool.clone());
    modes.migrate().await?;
    let rules = adk_harness::PostgresStandingRuleStore::new(pool.clone());
    rules.migrate().await?;
    let approvals = adk_session::PostgresApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let runs = adk_session::PostgresRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let checkpointer = adk_graph::PostgresCheckpointer::from_pool(pool);
    checkpointer.migrate().await?;
    Ok(SharedStores {
        label,
        sessions: Arc::new(sessions),
        modes: Arc::new(modes),
        rules: Arc::new(rules),
        approvals: Arc::new(approvals),
        runs: Arc::new(runs),
        checkpointer: Arc::new(checkpointer),
    })
}

#[cfg(not(feature = "postgres"))]
async fn postgres(_url: &str, _label: String) -> Result<SharedStores> {
    bail!(
        "--store postgres://… needs adk-cli's `postgres` feature, which this build does not \
         include: cargo install adk-cli --features postgres"
    )
}

#[cfg(feature = "redis")]
async fn redis(url: &str, label: String) -> Result<SharedStores> {
    let sessions = adk_session::RedisSessionService::new(adk_session::RedisSessionConfig {
        url: url.to_string(),
        ttl: None,
        cluster_nodes: None,
    })
    .await?;
    let modes = adk_harness::RedisModeStore::connect(url).await?;
    let rules = adk_harness::RedisStandingRuleStore::connect(url).await?;
    rules.migrate().await?;
    let approvals = adk_session::RedisApprovalStore::new(url).await?;
    approvals.migrate().await?;
    let runs = adk_session::RedisRunStore::new(url).await?;
    runs.migrate().await?;
    let checkpointer = adk_harness::checkpointer_from_url(url).await?;
    Ok(SharedStores {
        label,
        sessions: Arc::new(sessions),
        modes: Arc::new(modes),
        rules: Arc::new(rules),
        approvals: Arc::new(approvals),
        runs: Arc::new(runs),
        checkpointer,
    })
}

#[cfg(not(feature = "redis"))]
async fn redis(_url: &str, _label: String) -> Result<SharedStores> {
    bail!(
        "--store redis://… needs adk-cli's `redis` feature, which this build does not include: \
         cargo install adk-cli --features redis"
    )
}
