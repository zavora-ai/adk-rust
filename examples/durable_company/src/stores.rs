//! Every store the company runs on, opened on one backend.
//!
//! The scheduler, the workers, and the operator console share nothing but these stores:
//! on SQLite or PostgreSQL each process opens them itself, so a worker process can be
//! killed and another one continue from what the database holds.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditLogStore, InMemoryAuditLogStore};
use adk_core::{
    ActionLedger, ApprovalStore, DurableRunStore, GovernanceStateStore, InMemoryActionLedger,
    InMemoryApprovalStore, InMemoryGovernanceStateStore, InMemoryRunStore, InMemoryScheduleStore,
    InMemorySpendLedger, ScheduleStore, SpendKey, SpendLedger, SpendLimits, SpendPeriod,
};
use adk_session::{
    InMemorySessionService, PostgresActionLedger, PostgresApprovalStore, PostgresAuditLogStore,
    PostgresGovernanceStateStore, PostgresRunStore, PostgresScheduleStore, PostgresSessionService,
    PostgresSpendLedger, SessionService, SqliteActionLedger, SqliteApprovalStore,
    SqliteAuditLogStore, SqliteGovernanceStateStore, SqliteRunStore, SqliteScheduleStore,
    SqliteSessionService, SqliteSpendLedger,
};
use anyhow::Context;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::company::{APP, DAILY_CAP_USD};

/// Where the stores live.
#[derive(Clone, Debug)]
pub enum Backend {
    /// In this process only; workers run as tasks, and a "kill" aborts one.
    Memory,
    /// One SQLite file shared by every process.
    Sqlite { file: String },
    /// One PostgreSQL schema shared by every process.
    Postgres { url: String, schema: String },
}

impl Backend {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Sqlite { .. } => "sqlite",
            Self::Postgres { .. } => "postgres",
        }
    }

    /// Arguments that make a worker process open the same stores.
    pub fn worker_args(&self) -> Vec<String> {
        match self {
            Self::Memory => vec!["--store".into(), "memory".into()],
            Self::Sqlite { file } => {
                vec!["--store".into(), "sqlite".into(), "--sqlite".into(), file.clone()]
            }
            Self::Postgres { url, schema } => vec![
                "--store".into(),
                "postgres".into(),
                "--database-url".into(),
                url.clone(),
                "--schema".into(),
                schema.clone(),
            ],
        }
    }

    /// A database URL `adk-rust audit export --database-url` reads the audit log from.
    pub fn audit_database_url(&self) -> Option<String> {
        match self {
            Self::Memory => None,
            Self::Sqlite { file } => Some(format!("sqlite://{file}")),
            Self::Postgres { url, schema } => {
                let separator = if url.contains('?') { '&' } else { '?' };
                Some(format!("{url}{separator}options=-c%20search_path%3D{schema}"))
            }
        }
    }
}

/// The company's stores.
#[derive(Clone)]
pub struct Stores {
    pub runs: Arc<dyn DurableRunStore>,
    pub schedules: Arc<dyn ScheduleStore>,
    pub governance: Arc<dyn GovernanceStateStore>,
    pub sessions: Arc<dyn SessionService>,
    pub actions: Arc<dyn ActionLedger>,
    pub approvals: Arc<dyn ApprovalStore>,
    pub spend: Arc<dyn SpendLedger>,
    pub audit: Arc<dyn AuditLogStore>,
}

/// The daily cap on everything the company spends: model calls and payments together.
pub fn spend_limits() -> SpendLimits {
    SpendLimits::new().limit(SpendKey::org(APP).per(SpendPeriod::Day), DAILY_CAP_USD * 1_000_000)
}

impl Stores {
    /// Opens (and migrates) every store on `backend`.
    pub async fn open(backend: &Backend) -> anyhow::Result<Self> {
        match backend {
            Backend::Memory => Ok(Self {
                runs: Arc::new(InMemoryRunStore::new()),
                schedules: Arc::new(InMemoryScheduleStore::new()),
                governance: Arc::new(InMemoryGovernanceStateStore::new()),
                sessions: Arc::new(InMemorySessionService::new()),
                actions: Arc::new(InMemoryActionLedger::new()),
                approvals: Arc::new(InMemoryApprovalStore::new()),
                spend: Arc::new(InMemorySpendLedger::new(spend_limits())),
                audit: Arc::new(InMemoryAuditLogStore::new()),
            }),
            Backend::Sqlite { file } => {
                // WAL and a busy timeout let several processes share the file.
                let options = SqliteConnectOptions::from_str(&format!("sqlite://{file}"))?
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(30))
                    .foreign_keys(true);
                let pool = SqlitePoolOptions::new()
                    .max_connections(8)
                    .connect_with(options)
                    .await
                    .with_context(|| format!("opening {file}"))?;
                let runs = SqliteRunStore::from_pool(pool.clone());
                runs.migrate().await?;
                let schedules = SqliteScheduleStore::from_pool(pool.clone());
                schedules.migrate().await?;
                let governance = SqliteGovernanceStateStore::from_pool(pool.clone());
                governance.migrate().await?;
                let sessions = SqliteSessionService::from_pool(pool.clone());
                sessions.migrate().await?;
                let actions = SqliteActionLedger::from_pool(pool.clone());
                actions.migrate().await?;
                let approvals = SqliteApprovalStore::from_pool(pool.clone());
                approvals.migrate().await?;
                let spend = SqliteSpendLedger::from_pool(pool.clone()).with_limits(spend_limits());
                spend.migrate().await?;
                let audit = SqliteAuditLogStore::from_pool(pool);
                audit.migrate().await?;
                Ok(Self {
                    runs: Arc::new(runs),
                    schedules: Arc::new(schedules),
                    governance: Arc::new(governance),
                    sessions: Arc::new(sessions),
                    actions: Arc::new(actions),
                    approvals: Arc::new(approvals),
                    spend: Arc::new(spend),
                    audit: Arc::new(audit),
                })
            }
            Backend::Postgres { url, schema } => {
                let base = PgConnectOptions::from_str(url).context("parsing DATABASE_URL")?;
                // Each run of the scenario gets its own schema, so runs never see each
                // other's queue, schedules, spend, or kill switch.
                let admin =
                    PgPoolOptions::new().max_connections(1).connect_with(base.clone()).await?;
                sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
                    .execute(&admin)
                    .await?;
                admin.close().await;
                let pool = PgPoolOptions::new()
                    .max_connections(10)
                    .connect_with(base.options([("search_path", schema.as_str())]))
                    .await?;
                let runs = PostgresRunStore::from_pool(pool.clone());
                runs.migrate().await?;
                let schedules = PostgresScheduleStore::from_pool(pool.clone());
                schedules.migrate().await?;
                let governance = PostgresGovernanceStateStore::from_pool(pool.clone());
                governance.migrate().await?;
                let sessions = PostgresSessionService::from_pool(pool.clone());
                sessions.migrate().await?;
                let actions = PostgresActionLedger::from_pool(pool.clone());
                actions.migrate().await?;
                let approvals = PostgresApprovalStore::from_pool(pool.clone());
                approvals.migrate().await?;
                let spend =
                    PostgresSpendLedger::from_pool(pool.clone()).with_limits(spend_limits());
                spend.migrate().await?;
                let audit = PostgresAuditLogStore::from_pool(pool);
                audit.migrate().await?;
                Ok(Self {
                    runs: Arc::new(runs),
                    schedules: Arc::new(schedules),
                    governance: Arc::new(governance),
                    sessions: Arc::new(sessions),
                    actions: Arc::new(actions),
                    approvals: Arc::new(approvals),
                    spend: Arc::new(spend),
                    audit: Arc::new(audit),
                })
            }
        }
    }
}
