//! What a served site adds around the runtime (spec 3.5 §7.2, §7.5, D-11, A-I5): the gate
//! check that refuses to publish until the owner has delegated the release key, payments
//! through the owner's own payment links, and audit records in the logs.
//!
//! | Part | Does |
//! |---|---|
//! | [`ReleaseDelegation`] | Fails every build whose `/.well-known/awp/keys.json` does not delegate the release key the site log signs with; the finding names `adk-rust awp keys rotate` and `adk-rust awp keys delegate` |
//! | [`PaymentLinks`] | `pay_order`, `donate`, `subscribe` and `buy_ticket` answer with the owner's payment instructions from `payments.json`; the money never passes through the site |
//! | [`LogRecorder`] | Writes audit records to the logs, for the edge's meter |

use std::sync::Arc;

use adk_awp::v2::capability::{CapabilityCall, CapabilityError};
use adk_core::audit_log::{AuditRecordDraft, AuditRecorder};
use async_trait::async_trait;
use awp_types::ErrorCode;
use serde_json::{Value, json};

use super::project::PaymentLink;
use crate::edge::handlers::PaymentDesk;

/// Refuses to publish until the owner's root has delegated the release key (spec 3.5 §7.2,
/// D-11, A-I5); it lives in `adk-site`'s gate and reports as `GateCheck::Signing`.
pub use adk_site::gate::ReleaseDelegation;

/// Payments through the owner's own payment links (spec 3.5 §7.5 "Commerce", §7.14): the
/// buyer is told how to pay with the order as the reference; nothing is charged here.
pub struct PaymentLinks {
    links: Vec<PaymentLink>,
}

impl PaymentLinks {
    /// The desk over `links`, from `payments.json`.
    pub fn new(links: Vec<PaymentLink>) -> Self {
        Self { links }
    }
}

#[async_trait]
impl PaymentDesk for PaymentLinks {
    async fn take(&self, call: &CapabilityCall) -> Result<Value, CapabilityError> {
        let wanted = call.input["rail"].as_str();
        let link = match wanted {
            Some(rail) => self.links.iter().find(|link| link.rail == rail),
            None => self.links.first(),
        }
        .ok_or_else(|| CapabilityError::Refused {
            code: ErrorCode::PaymentPolicyRejected,
            message: format!(
                "the site does not take payment by {}; it accepts {}",
                wanted.unwrap_or("that rail"),
                self.links.iter().map(|link| link.rail.as_str()).collect::<Vec<_>>().join(", ")
            ),
        })?;
        let reference = call.input["orderId"].as_str().map_or_else(
            || format!("{}-{}", call.capability, call.idempotency_key),
            str::to_string,
        );
        Ok(json!({
            "status": "awaiting_payment",
            "reference": reference,
            "rail": link.rail,
            "instructions": link.instructions,
            "url": link.url,
        }))
    }
}

/// Writes audit records to the logs.
#[derive(Debug, Default, Clone)]
pub struct LogRecorder;

#[async_trait]
impl AuditRecorder for LogRecorder {
    fn digest(&self, value: &Value) -> String {
        adk_core::json_digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        tracing::info!(audit.action = %draft.action, "audit record");
        Ok(())
    }
}

/// The edge's meter over [`LogRecorder`], counting into `counts`.
pub fn metered_audit(
    counts: Arc<dyn adk_server::awp::BeaconStore>,
) -> adk_core::audit_log::AuditTrail {
    adk_core::audit_log::AuditTrail::new(Arc::new(crate::measurement::EdgeMeter::new(
        Arc::new(LogRecorder),
        counts,
    )))
}
