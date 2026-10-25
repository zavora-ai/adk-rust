//! Site status (spec 3.5 §7.5).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::identity::Proof;
use super::signal::ReasonCode;

/// The signed status document served at `GET /awp/v2/status` (spec 3.5 §7.5).
///
/// The edge signs it with a delegated `receipt` key and emits `site.status.changed` on every
/// transition.
///
/// # Example
///
/// ```
/// use awp_types::{Proof, SiteState, SiteStatus};
/// use chrono::Utc;
///
/// let status = SiteStatus::new(SiteState::ReadOnly, Utc::now(), Proof::eddsa("edge-2026-10", "e30..c2ln"));
/// let json = serde_json::to_value(&status).unwrap();
/// assert_eq!(json["state"], "read_only");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteStatus {
    /// The current state.
    pub state: SiteState,
    /// When the site entered the state.
    pub since: DateTime<Utc>,
    /// Why, as a snake_case code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<ReasonCode>,
    /// Sequence number of the release being served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_seq: Option<u64>,
    /// The edge's detached JWS over the JCS form of the status without `proof`.
    pub proof: Proof,
}

impl SiteStatus {
    /// Creates a status with no reason or release.
    pub fn new(state: SiteState, since: DateTime<Utc>, proof: Proof) -> Self {
        Self { state, since, reason_code: None, release_seq: None, proof }
    }
}

/// A site's state (spec 3.5 §7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SiteState {
    /// Everything works.
    Operational,
    /// Working with reduced function.
    Degraded,
    /// Only `ReadOnly` capabilities answer; the rest answer 503 with the status document.
    ReadOnly,
    /// Planned maintenance.
    Maintenance,
    /// An incident is in progress.
    Incident,
}
