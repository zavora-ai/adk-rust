//! The Telegram chat thread of `adk-rust up` (spec 3.3 §5.3), started when
//! `TELEGRAM_BOT_TOKEN` is set; without it nothing of it is built, started or logged.
//!
//! The bot's updates are read by long polling, so the kernel keeps its loopback bind. A
//! paired account's messages are turns on the harness; each decision rings its thread; the
//! account is paired with `adk-rust pair accept <code>` and listed in the home under "Where
//! you can say yes from", in the store the kernel's other pairings are in.

use std::net::SocketAddr;
use std::sync::Arc;

use adk_core::ApprovalService;
use adk_core::audit_log::AuditTrail;
use adk_kernel::Kernel;
use adk_kernel::channels::{ChannelBindingStore, ChannelDoorbell, ChannelGateway};
use adk_surface::channels::TelegramAdapter;
use anyhow::{Context, Result};

use super::super::{APP_NAME, Setup};

/// The bot's token, from `@BotFather`.
const TOKEN_VAR: &str = "TELEGRAM_BOT_TOKEN";
/// Another Bot API address, such as a local Bot API server; `https://api.telegram.org` when
/// unset.
const API_VAR: &str = "TELEGRAM_API_BASE";

/// The chat thread before the kernel is built: the doorbell the approval service rings, and
/// where paired accounts are kept.
pub(super) struct Chat {
    token: String,
    api_base: Option<String>,
    doorbell: Arc<ChannelDoorbell>,
    bindings: Arc<dyn ChannelBindingStore>,
}

impl Chat {
    /// The chat thread when `TELEGRAM_BOT_TOKEN` is set, its accounts kept beside the
    /// kernel's other stores; `None` otherwise.
    pub(super) async fn from_env(setup: &Setup, store: Option<&str>) -> Result<Option<Self>> {
        let Some(token) = env(TOKEN_VAR) else { return Ok(None) };
        let api_base = env(API_VAR);
        // A malformed token or address stops `up` before anything is opened.
        adapter(&token, api_base.as_deref())
            .build()
            .with_context(|| format!("{TOKEN_VAR} or {API_VAR} cannot be used"))?;
        Ok(Some(Self {
            token,
            api_base,
            doorbell: Arc::new(ChannelDoorbell::new()),
            bindings: bindings(setup, store).await?,
        }))
    }

    /// The doorbell to register on the approval service.
    pub(super) fn doorbell(&self) -> Arc<ChannelDoorbell> {
        self.doorbell.clone()
    }

    /// Where paired accounts are kept, for the control plane's pairing.
    pub(super) fn bindings(&self) -> Arc<dyn ChannelBindingStore> {
        self.bindings.clone()
    }

    /// Starts the gateway over `kernel` and the bot's polling. Cards read their decisions
    /// from `approvals`, link to the home at `bind`, and each answer given in Telegram is
    /// recorded on `trail`, so the home says "You said yes in Telegram".
    pub(super) async fn start(
        self,
        kernel: &Kernel,
        approvals: Arc<dyn ApprovalService>,
        trail: Option<AuditTrail>,
        bind: SocketAddr,
    ) -> Result<()> {
        let mut adapter = adapter(&self.token, self.api_base.as_deref()).approvals(approvals);
        if let Some(trail) = trail {
            adapter = adapter.audit(trail);
        }
        let adapter = adapter.build()?;
        let gateway = ChannelGateway::builder(Arc::new(kernel.clone()), self.bindings)
            .sink(Arc::new(adapter.clone()))
            .target(APP_NAME)
            .home_url(format!("http://{bind}/ui/"))
            .doorbell(self.doorbell)
            .build();
        gateway.start().await?;
        // Decisions that waited through a restart reach the thread again.
        gateway.ring_waiting().await?;
        let polling = adapter.start(gateway);
        tokio::spawn(async move {
            if let Err(error) = polling.join().await {
                tracing::error!(error = %error, "the telegram bot stopped reading messages");
            }
        });
        println!(
            "  chat  Telegram: an account that writes to the bot is sent a code; pair it with \
             `adk-rust pair accept <code>`"
        );
        Ok(())
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn adapter(token: &str, api_base: Option<&str>) -> adk_surface::channels::TelegramAdapterBuilder {
    let builder = TelegramAdapter::builder(token);
    match api_base {
        Some(base) => builder.api_base(base),
        None => builder,
    }
}

/// Paired chat accounts in the `--store` backend, on `harness.db`, or in memory with
/// `--ephemeral`, as the kernel's paired devices are.
async fn bindings(setup: &Setup, store: Option<&str>) -> Result<Arc<dyn ChannelBindingStore>> {
    if let Some(url) = store {
        return shared_bindings(url).await;
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        let store = adk_kernel::channels::SqliteChannelBindingStore::new(db.pool.clone());
        store.migrate().await?;
        return Ok(Arc::new(store));
    }
    let _ = setup;
    Ok(Arc::new(adk_kernel::channels::InMemoryChannelBindingStore::new()))
}

/// Paired chat accounts in the `--store` backend every replica shares.
async fn shared_bindings(url: &str) -> Result<Arc<dyn ChannelBindingStore>> {
    let label = adk_harness::redact_store_url(url);
    match url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase()).as_deref() {
        #[cfg(feature = "postgres")]
        Some("postgres" | "postgresql") => {
            let store = adk_kernel::channels::PostgresChannelBindingStore::connect(url).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        #[cfg(feature = "redis")]
        Some("redis" | "rediss") => {
            let store = adk_kernel::channels::RedisChannelBindingStore::connect(url).await?;
            store.migrate().await?;
            Ok(Arc::new(store))
        }
        // `Setup::open` already refused a scheme this build cannot open.
        _ => anyhow::bail!("--store {label}: no chat pairing backend for it in this build"),
    }
}
