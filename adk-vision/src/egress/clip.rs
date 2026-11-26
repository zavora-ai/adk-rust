//! Releasing a video clip to a remote backend: [`ReleasedClip`] and
//! [`EgressGate::release_clip`].

use std::collections::BTreeMap;

use adk_core::egress::EgressTarget;
use chrono::Utc;

use super::gate::{deny_reason, on_device_reason, remote_hosts, split_host_port};
use super::{
    DataClass, EgressAction, EgressDecision, EgressGate, EgressReceipt, EgressRecord, EgressRefusal,
};
use crate::backend::{BackendDescriptor, BackendId};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::TransformChain;
use crate::video::{ClipInfo, SourceClip};

/// One request to release a clip to a remote backend.
///
/// # Example
///
/// ```no_run
/// use adk_vision::{BackendDescriptor, ClipReleaseRequest, CoordFrame, DataClass, Locality, SourceClip};
///
/// let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap()).unwrap();
/// let gemini = BackendDescriptor::new(
///     "gemini-3.8-flash",
///     Locality::Remote { hosts: vec!["generativelanguage.googleapis.com".into()] },
///     CoordFrame::SentPixels,
/// );
/// let request = ClipReleaseRequest::new(&clip, &gemini, DataClass::new("video"));
/// assert!(request.approved.is_empty());
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ClipReleaseRequest<'a> {
    /// The clip.
    pub clip: &'a SourceClip,
    /// The backend the clip is for.
    pub backend: &'a BackendDescriptor,
    /// The data class the egress rules are written per.
    pub data_class: DataClass,
    /// Clip digests a person approved, for a rule that asks.
    pub approved: Vec<Digest>,
    /// The call's estimated cost for the receipt: by default the backend's per-unit price
    /// for every frame of the clip in one request.
    pub estimated_cost_micro_usd: Option<u64>,
}

impl<'a> ClipReleaseRequest<'a> {
    /// A request to release `clip` to `backend` as `data_class`.
    pub fn new(
        clip: &'a SourceClip,
        backend: &'a BackendDescriptor,
        data_class: DataClass,
    ) -> Self {
        let estimated_cost_micro_usd =
            backend.price.as_ref().and_then(|p| p.request_cost(clip.info().frames));
        Self { clip, backend, data_class, approved: Vec::new(), estimated_cost_micro_usd }
    }

    /// Releases the clip if its digest is one a person approved.
    pub fn approved(mut self, digest: Digest) -> Self {
        self.approved.push(digest);
        self
    }

    /// Records `micro_usd` as the call's estimated cost.
    pub fn with_estimated_cost(mut self, micro_usd: Option<u64>) -> Self {
        self.estimated_cost_micro_usd = micro_usd;
        self
    }
}

/// A clip the egress gate released to one backend: the exact bytes sent and the receipt
/// the gate recorded. The only clip a remote video backend accepts; only [`EgressGate`]
/// constructs one:
///
/// ```compile_fail,E0599
/// use adk_vision::ReleasedClip;
///
/// let forged = ReleasedClip::new();
/// ```
#[derive(Clone, Debug)]
pub struct ReleasedClip {
    clip: SourceClip,
    receipt: EgressReceipt,
}

impl ReleasedClip {
    /// The exact bytes released; their digest is the receipt's `egress`.
    pub fn bytes(&self) -> &[u8] {
        self.clip.bytes()
    }

    /// The released clip.
    pub fn clip(&self) -> &SourceClip {
        &self.clip
    }

    /// What the container states.
    pub fn info(&self) -> &ClipInfo {
        self.clip.info()
    }

    /// `video/mp4` or `video/quicktime`.
    pub fn mime_type(&self) -> &str {
        &self.clip.info().mime_type
    }

    /// The receipt the gate recorded.
    pub fn receipt(&self) -> &EgressReceipt {
        &self.receipt
    }

    /// `true` when this clip was released to `backend`.
    pub fn is_for(&self, backend: &BackendId) -> bool {
        &self.receipt.backend == backend
    }
}

impl EgressGate {
    /// Releases a video clip to a remote backend, or refuses, in the steps of
    /// [`release`](Self::release):
    ///
    /// 1. **Destination.** The on-device profile refuses every host; otherwise the adk-core
    ///    egress policy decides each of the backend's hosts.
    /// 2. **Minimise and redact.** A clip is not re-encoded on the device, so a
    ///    configuration that requires any redaction class refuses every clip (V-I2): send
    ///    sampled frames instead, which the gate redacts one by one.
    /// 3. **Decide.** The rule for the host and data class says `deny`, `ask` or `allow`;
    ///    `ask` holds the clip unless a person approved its digest.
    /// 4. **Record and release.** The receipt, with the clip's digest as both source and
    ///    egress digest, goes to the recorder and the audit log before the clip is returned.
    ///
    /// # Errors
    ///
    /// [`VisionError::EgressRefused`] at the destination, redact or decide step,
    /// [`VisionError::EgressHeld`] for a rule that asks, [`VisionError::Cancelled`] or the
    /// kill switch's error, and the recorder's error.
    pub async fn release_clip(
        &self,
        cx: &VisionCx,
        request: ClipReleaseRequest<'_>,
    ) -> Result<ReleasedClip> {
        let hosts = remote_hosts(request.backend)?;
        let digest = request.clip.digest();
        if let Err(err) = cx.check() {
            self.refuse_clip(cx, &request, &hosts, None, "destination", &err.to_string()).await?;
            return Err(err);
        }
        if self.config.on_device {
            let reason = on_device_reason(&hosts);
            self.refuse_clip(cx, &request, &hosts, None, "destination", &reason).await?;
            return Err(VisionError::EgressRefused { host: hosts.join(", "), reason });
        }
        let policy = self.destination_policy();
        for host in &hosts {
            let (name, port) = split_host_port(host);
            if let Err(denial) = policy.decide(&EgressTarget::Name { host: name, port }) {
                let reason = denial.to_string();
                self.refuse_clip(cx, &request, &hosts, None, "destination", &reason).await?;
                return Err(VisionError::EgressRefused { host: host.clone(), reason });
            }
        }
        if let Some(class) = self.config.redact.first() {
            let reason = format!(
                "the egress rules require `{class}` redaction, and a clip cannot be redacted on \
                 the device; analyse sampled frames instead, which the gate redacts one by one"
            );
            self.refuse_clip(cx, &request, &hosts, Some(digest), "redact", &reason).await?;
            return Err(VisionError::EgressRefused { host: hosts.join(", "), reason });
        }
        let (action, rule) = self.decide(request.backend, &request.data_class);
        let decision = match action {
            EgressAction::Allow => EgressDecision::Allowed { rule },
            EgressAction::Ask if request.approved.contains(&digest) => {
                EgressDecision::Approved { rule }
            }
            EgressAction::Ask => {
                let reason = format!("held for a person's approval of {digest}");
                self.refuse_clip(cx, &request, &hosts, Some(digest), "decide", &reason).await?;
                return Err(VisionError::EgressHeld {
                    host: hosts.join(", "),
                    data_class: request.data_class.to_string(),
                    egress: digest,
                });
            }
            EgressAction::Deny => {
                let reason = deny_reason(rule, &request.data_class);
                self.refuse_clip(cx, &request, &hosts, Some(digest), "decide", &reason).await?;
                return Err(VisionError::EgressRefused { host: hosts.join(", "), reason });
            }
        };
        if let Err(err) = cx.check() {
            self.refuse_clip(cx, &request, &hosts, Some(digest), "release", &err.to_string())
                .await?;
            return Err(err);
        }
        let (width, height) = request.clip.size();
        let receipt = EgressReceipt {
            id: uuid::Uuid::new_v4().to_string(),
            source: digest,
            egress: digest,
            backend: request.backend.id.clone(),
            hosts,
            data_class: request.data_class.clone(),
            chain: TransformChain::new(width, height),
            redactions: BTreeMap::new(),
            decision,
            estimated_cost_micro_usd: request.estimated_cost_micro_usd,
            artifact: request.clip.artifact().cloned(),
            released_at: Utc::now(),
        };
        let record = EgressRecord::Released(receipt.clone());
        self.recorder.record(record.clone()).await?;
        cx.audit(crate::audit::egress_draft(&record)).await?;
        self.released.lock().unwrap_or_else(|e| e.into_inner()).insert(digest);
        tracing::info!(
            egress.backend = %receipt.backend,
            egress.digest = %digest,
            egress.data_class = %receipt.data_class,
            clip.bytes = request.clip.bytes().len(),
            "clip released"
        );
        Ok(ReleasedClip { clip: request.clip.clone(), receipt })
    }

    async fn refuse_clip(
        &self,
        cx: &VisionCx,
        request: &ClipReleaseRequest<'_>,
        hosts: &[String],
        egress: Option<Digest>,
        step: &str,
        reason: &str,
    ) -> Result<()> {
        tracing::warn!(
            egress.backend = %request.backend.id,
            egress.step = step,
            egress.reason = reason,
            "clip not released"
        );
        let record = EgressRecord::Refused(EgressRefusal {
            source: request.clip.digest(),
            egress,
            backend: request.backend.id.clone(),
            hosts: hosts.to_vec(),
            data_class: request.data_class.clone(),
            step: step.to_string(),
            reason: reason.to_string(),
            refused_at: Utc::now(),
        });
        self.recorder.record(record.clone()).await?;
        cx.audit(crate::audit::egress_draft(&record)).await
    }
}
