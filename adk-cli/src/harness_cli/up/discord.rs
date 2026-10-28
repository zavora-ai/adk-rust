//! Discord for `adk-rust up` (spec 3.5 §10 X12): started when `DISCORD_BOT_TOKEN` is set, over
//! the Gateway, so the home keeps its loopback bind.

use adk_surface::channels::{DiscordAdapter, DiscordAdapterBuilder};
use anyhow::{Context, Result};

use super::chats::env;

/// The bot's token, from the Developer Portal.
const TOKEN_VAR: &str = "DISCORD_BOT_TOKEN";
/// Another REST address; `https://discord.com/api/v10` when unset.
const API_VAR: &str = "DISCORD_API_BASE";

/// The adapter's builder when `DISCORD_BOT_TOKEN` is set; `None` otherwise.
///
/// # Errors
///
/// A malformed token or address.
pub(super) fn from_env() -> Result<Option<DiscordAdapterBuilder>> {
    let Some(token) = env(TOKEN_VAR) else { return Ok(None) };
    let builder = || match env(API_VAR) {
        Some(base) => DiscordAdapter::builder(&token).api_base(base),
        None => DiscordAdapter::builder(&token),
    };
    builder().build().with_context(|| format!("{TOKEN_VAR} or {API_VAR} cannot be used"))?;
    Ok(Some(builder()))
}
