//! The edge's modes, the kill switch and the signed site status (spec 3.5 §7.5, §7.9, A-I12).
//!
//! | Mode | Capabilities | Status published |
//! |---|---|---|
//! | [`EdgeMode::Serving`] | Every visible capability | `operational` |
//! | [`EdgeMode::ReadOnly`] | `read_only` capabilities; the rest answer `503 read_only` with the status | `read_only` with the reason |
//! | [`EdgeMode::Off`] | None: every edge route answers `404`, leaving only the static content | — |
//!
//! The owner's kill switch ([`GovernanceControl`]) puts a serving edge in read-only mode with
//! reason `kill_switch` for as long as it is frozen (A-I12).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use adk_awp::signing::jws::{DocumentSigner, sign};
use adk_awp::v2::store::{StatusStore, StoreError};
use adk_core::GovernanceControl;
use awp_types::{Proof, ReasonCode, SiteState, SiteStatus};
use chrono::Utc;

/// The reason code of the status the kill switch publishes (AWP 2.0 §10.3).
pub const KILL_SWITCH_REASON: &str = "kill_switch";

/// How the edge answers (spec 3.5 §7.9).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EdgeMode {
    /// Every visible capability answers.
    Serving,
    /// Only `read_only` capabilities answer; the rest answer `503 read_only` with the signed
    /// status document.
    ReadOnly {
        /// The status `reasonCode`, a snake_case code such as `drift_detected`.
        reason: String,
    },
    /// The edge is off: its routes answer `404` and only the static content remains.
    Off,
}

impl EdgeMode {
    /// Read-only mode for `reason`.
    pub fn read_only(reason: impl Into<String>) -> Self {
        Self::ReadOnly { reason: reason.into() }
    }
}

/// Why the edge could not publish a status (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StatusError {
    /// The receipt key could not sign the document.
    #[error("the edge's receipt key could not sign the status: {0}")]
    Signing(String),
    /// The status store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Shared state behind the edge's mode, kill switch and status.
pub(crate) struct EdgeControl {
    mode: RwLock<EdgeMode>,
    governance: OnceLock<GovernanceControl>,
    status: Arc<dyn StatusStore>,
    receipt: Arc<dyn DocumentSigner>,
    release_seq: AtomicU64,
    frozen_seen: AtomicBool,
    events: OnceLock<super::events::EdgeEvents>,
}

impl std::fmt::Debug for EdgeControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeControl").field("mode", &self.mode()).finish_non_exhaustive()
    }
}

impl EdgeControl {
    pub(crate) fn new(status: Arc<dyn StatusStore>, receipt: Arc<dyn DocumentSigner>) -> Self {
        Self {
            mode: RwLock::new(EdgeMode::Serving),
            governance: OnceLock::new(),
            status,
            receipt,
            release_seq: AtomicU64::new(0),
            frozen_seen: AtomicBool::new(false),
            events: OnceLock::new(),
        }
    }

    /// Attaches the owner's kill switch unless one is attached; returns whether it was.
    pub(crate) fn attach_governance(&self, control: GovernanceControl) -> bool {
        self.governance.set(control).is_ok()
    }

    /// Publishes `site.status.changed` on every transition through `events`.
    pub(crate) fn attach_events(&self, events: super::events::EdgeEvents) {
        let _ = self.events.set(events);
    }

    pub(crate) fn governance(&self) -> Option<&GovernanceControl> {
        self.governance.get()
    }

    pub(crate) fn mode(&self) -> EdgeMode {
        self.mode.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub(crate) fn set_release_seq(&self, seq: u64) {
        self.release_seq.store(seq, Ordering::SeqCst);
    }

    /// Whether the owner's kill switch is engaged.
    pub(crate) fn frozen(&self) -> bool {
        self.governance.get().is_some_and(GovernanceControl::is_frozen)
    }

    pub(crate) fn is_off(&self) -> bool {
        matches!(self.mode(), EdgeMode::Off)
    }

    /// Whether non-`read_only` capabilities are refused: the kill switch or read-only mode.
    pub(crate) fn is_read_only(&self) -> bool {
        self.frozen() || matches!(self.mode(), EdgeMode::ReadOnly { .. })
    }

    /// Signs and stores a status in `state` for `reason` (spec 3.5 §7.5).
    pub(crate) async fn publish(
        &self,
        state: SiteState,
        reason: Option<&str>,
    ) -> Result<SiteStatus, StatusError> {
        let mut status = SiteStatus::new(state, Utc::now(), Proof::eddsa(self.receipt.kid(), ""));
        status.reason_code = reason.map(|reason| ReasonCode(reason.to_owned()));
        let seq = self.release_seq.load(Ordering::SeqCst);
        status.release_seq = (seq > 0).then_some(seq);
        status.proof = sign(&status, self.receipt.as_ref())
            .await
            .map_err(|error| StatusError::Signing(error.to_string()))?;
        let previous = self.status.set(status.clone()).await?;
        let changed = previous.as_ref().is_none_or(|previous| {
            previous.state != status.state || previous.reason_code != status.reason_code
        });
        if changed && let Some(events) = self.events.get() {
            let change = awp_types::StatusChanged {
                previous: previous.map(|previous| previous.state),
                status: status.clone(),
            };
            if let Err(error) = events.status_changed(change).await {
                tracing::warn!(error = %error, "awp site.status.changed not published");
            }
        }
        tracing::info!(
            awp.status = ?status.state,
            awp.reason = reason.unwrap_or(""),
            "awp edge status published"
        );
        Ok(status)
    }

    /// Switches the mode and publishes the status it implies; `Off` publishes nothing.
    pub(crate) async fn set_mode(&self, mode: EdgeMode) -> Result<Option<SiteStatus>, StatusError> {
        *self.mode.write().unwrap_or_else(std::sync::PoisonError::into_inner) = mode.clone();
        if self.frozen() {
            return Ok(None);
        }
        match mode {
            EdgeMode::Serving => self.publish(SiteState::Operational, None).await.map(Some),
            EdgeMode::ReadOnly { reason } => {
                self.publish(SiteState::ReadOnly, Some(&reason)).await.map(Some)
            }
            EdgeMode::Off => Ok(None),
        }
    }

    /// Publishes the kill switch's status when it engages, and the mode's status when it
    /// lifts (A-I12). Cheap when nothing changed.
    pub(crate) async fn sync_kill_switch(&self) {
        let frozen = self.frozen();
        if self.frozen_seen.swap(frozen, Ordering::SeqCst) == frozen {
            return;
        }
        let published = if frozen {
            tracing::warn!("awp edge is read-only: the owner's kill switch is engaged");
            self.publish(SiteState::ReadOnly, Some(KILL_SWITCH_REASON)).await
        } else {
            match self.mode() {
                EdgeMode::ReadOnly { reason } => {
                    self.publish(SiteState::ReadOnly, Some(&reason)).await
                }
                EdgeMode::Serving | EdgeMode::Off => {
                    self.publish(SiteState::Operational, None).await
                }
            }
        };
        if let Err(error) = published {
            tracing::error!(error = %error, "awp edge could not publish its status");
        }
    }

    /// The current status, if one was published.
    pub(crate) async fn current_status(&self) -> Option<SiteStatus> {
        match self.status.current().await {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(error = %error, "awp status store failed");
                None
            }
        }
    }
}
