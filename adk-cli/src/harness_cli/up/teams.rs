//! Microsoft Teams for `adk-rust up`: started when `TEAMS_APP_ID` and `TEAMS_APP_PASSWORD` are
//! set. Teams reaches a bot only at its messaging endpoint, so the webhook gets a listener of
//! its own — `TEAMS_BIND`, `127.0.0.1:3978` by default — serving `POST /api/messages` and
//! nothing else. A tunnel or reverse proxy publishes that port alone over HTTPS; the home keeps
//! its loopback bind and is never reachable through it.

use std::net::SocketAddr;

use adk_surface::channels::{TeamsAdapter, TeamsAdapterBuilder};
use anyhow::{Context, Result, bail};

use super::chats::env;

/// The bot's Microsoft App ID, from its Azure Bot resource.
const APP_ID_VAR: &str = "TEAMS_APP_ID";
/// A client secret of the bot's Microsoft Entra app registration.
const PASSWORD_VAR: &str = "TEAMS_APP_PASSWORD";
/// A single-tenant bot's tenant; unset for a multi-tenant bot.
const TENANT_VAR: &str = "TEAMS_TENANT_ID";
/// Where the messaging endpoint listens.
const BIND_VAR: &str = "TEAMS_BIND";
/// The Bot Connector address for wake-ups to a conversation not heard from since `up`
/// started; Teams' global address when unset.
const SERVICE_URL_VAR: &str = "TEAMS_SERVICE_URL";
/// Where the messaging endpoint listens when `TEAMS_BIND` is unset: the Bot Framework's
/// conventional port, on loopback.
pub(super) const DEFAULT_BIND: &str = "127.0.0.1:3978";
/// The messaging endpoint's path.
pub(super) const PATH: &str = "/api/messages";

/// Teams as `up` starts it: the adapter's builder and where its endpoint listens.
pub(super) struct Teams {
    pub(super) builder: TeamsAdapterBuilder,
    pub(super) bind: SocketAddr,
}

/// Teams when both `TEAMS_APP_ID` and `TEAMS_APP_PASSWORD` are set; `None` when neither is.
///
/// # Errors
///
/// One variable without the other, a malformed id, password, tenant or address, or a
/// `TEAMS_BIND` that is not `host:port`.
pub(super) fn from_env() -> Result<Option<Teams>> {
    let (app_id, password) = match (env(APP_ID_VAR), env(PASSWORD_VAR)) {
        (None, None) => return Ok(None),
        (Some(app_id), Some(password)) => (app_id, password),
        _ => bail!(
            "Microsoft Teams needs both {APP_ID_VAR} and {PASSWORD_VAR}: set the other one, or \
             neither"
        ),
    };
    let tenant = env(TENANT_VAR);
    let service_url = env(SERVICE_URL_VAR);
    let builder = || {
        let mut builder = TeamsAdapter::builder(&app_id, &password);
        if let Some(tenant) = &tenant {
            builder = builder.tenant(tenant);
        }
        if let Some(url) = &service_url {
            builder = builder.service_url(url);
        }
        builder
    };
    // A malformed value stops `up` before anything is opened.
    builder().build().with_context(|| {
        format!("{APP_ID_VAR}, {PASSWORD_VAR}, {TENANT_VAR} or {SERVICE_URL_VAR} cannot be used")
    })?;
    let bind = env(BIND_VAR).unwrap_or_else(|| DEFAULT_BIND.to_string());
    let bind: SocketAddr = bind
        .parse()
        .with_context(|| format!("{BIND_VAR} `{bind}` is not an address such as 127.0.0.1:3978"))?;
    Ok(Some(Teams { builder: builder(), bind }))
}
