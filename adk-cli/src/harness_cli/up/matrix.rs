//! Matrix for `adk-rust up` (spec 3.5 §10 X12): started when `MATRIX_HOMESERVER` and
//! `MATRIX_ACCESS_TOKEN` are set, by `/sync` long polling, so the home keeps its loopback bind.

use adk_surface::channels::{MatrixAdapter, MatrixAdapterBuilder};
use anyhow::{Context, Result, bail};

use super::chats::env;

/// The homeserver's address, such as `https://matrix.example.org`.
const HOMESERVER_VAR: &str = "MATRIX_HOMESERVER";
/// The bot account's access token.
const TOKEN_VAR: &str = "MATRIX_ACCESS_TOKEN";

/// The adapter's builder when both are set; `None` when neither is.
///
/// # Errors
///
/// One without the other, or a malformed token or address.
pub(super) fn from_env() -> Result<Option<MatrixAdapterBuilder>> {
    let (homeserver, token) = match (env(HOMESERVER_VAR), env(TOKEN_VAR)) {
        (None, None) => return Ok(None),
        (Some(homeserver), Some(token)) => (homeserver, token),
        _ => bail!(
            "Matrix needs both {HOMESERVER_VAR} and {TOKEN_VAR}: set the other one, or neither"
        ),
    };
    MatrixAdapter::builder(&homeserver, &token)
        .build()
        .with_context(|| format!("{HOMESERVER_VAR} or {TOKEN_VAR} cannot be used"))?;
    Ok(Some(MatrixAdapter::builder(homeserver, token)))
}
