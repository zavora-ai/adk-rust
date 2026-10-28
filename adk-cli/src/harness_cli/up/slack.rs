//! Slack for `adk-rust up` (spec 3.5 §10 X12): started when `SLACK_APP_TOKEN` and
//! `SLACK_BOT_TOKEN` are set, over Socket Mode, so the home keeps its loopback bind.

use adk_surface::channels::{SlackAdapter, SlackAdapterBuilder};
use anyhow::{Context, Result, bail};

use super::chats::env;

/// The app-level token (`xapp-…`, with `connections:write`), for Socket Mode.
const APP_TOKEN_VAR: &str = "SLACK_APP_TOKEN";
/// The bot token (`xoxb-…`, with `chat:write` and `im:history`).
const BOT_TOKEN_VAR: &str = "SLACK_BOT_TOKEN";
/// Another Web API address; `https://slack.com/api` when unset.
const API_VAR: &str = "SLACK_API_BASE";

/// The adapter's builder when both tokens are set; `None` when neither is.
///
/// # Errors
///
/// One token without the other, or a malformed token or address.
pub(super) fn from_env() -> Result<Option<SlackAdapterBuilder>> {
    let (app, bot) = match (env(APP_TOKEN_VAR), env(BOT_TOKEN_VAR)) {
        (None, None) => return Ok(None),
        (Some(app), Some(bot)) => (app, bot),
        _ => bail!(
            "Slack needs both {APP_TOKEN_VAR} and {BOT_TOKEN_VAR}: set the other one, or neither"
        ),
    };
    let builder = || match env(API_VAR) {
        Some(base) => SlackAdapter::builder(&app, &bot).api_base(base),
        None => SlackAdapter::builder(&app, &bot),
    };
    // A malformed token or address stops `up` before anything is opened.
    builder()
        .build()
        .with_context(|| format!("{APP_TOKEN_VAR}, {BOT_TOKEN_VAR} or {API_VAR} cannot be used"))?;
    Ok(Some(builder()))
}
