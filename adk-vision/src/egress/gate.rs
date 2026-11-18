//! [`EgressGate`]: destination, minimise, redact, decide, record, release.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::egress::{EgressPolicy, EgressTarget};
use chrono::Utc;

use super::{
    DataClass, EgressAction, EgressConfig, EgressDecision, EgressEvidence, EgressReceipt,
    EgressRecord, EgressRecorder, EgressRefusal, RedactionClass, Redactor, ReleasedImage,
};
use crate::backend::{BackendDescriptor, Locality, Oversize};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::edit::{Rgba, ops};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, Transform, TransformChain};
use crate::media::{DecodedImage, SourceImage};
use crate::sent::SentImage;

/// One request to release an image to a remote backend.
///
/// # Example
///
/// ```
/// use adk_vision::{BBox, BackendDescriptor, CoordFrame, DataClass, Locality, ReleaseRequest};
/// # let source = adk_vision::SourceImage::from_pixels(adk_vision::DecodedImage::filled(64, 64, [255; 4])).unwrap();
///
/// let gemini = BackendDescriptor::new(
///     "gemini",
///     Locality::Remote { hosts: vec!["generativelanguage.googleapis.com".into()] },
///     CoordFrame::SentPixels,
/// );
/// let request = ReleaseRequest::new(&source, &gemini, DataClass::new("document_crop"))
///     .with_crop(BBox::new(8.0, 8.0, 40.0, 24.0));
/// assert!(request.crop.is_some());
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReleaseRequest<'a> {
    /// The source image.
    pub source: &'a SourceImage,
    /// The backend the image is for.
    pub backend: &'a BackendDescriptor,
    /// The data class the egress rules are written per.
    pub data_class: DataClass,
    /// Only this part of the source is sent, in source pixels: a field and its margin, not
    /// the page.
    pub crop: Option<BBox>,
    /// Regions to fill before release, in source pixels.
    pub marked: Vec<BBox>,
    /// Egress digests a person approved, for a rule that asks.
    pub approved: Vec<Digest>,
}

impl<'a> ReleaseRequest<'a> {
    /// A request to release the whole source image.
    pub fn new(
        source: &'a SourceImage,
        backend: &'a BackendDescriptor,
        data_class: DataClass,
    ) -> Self {
        Self { source, backend, data_class, crop: None, marked: Vec::new(), approved: Vec::new() }
    }

    /// Sends only `crop` of the source.
    pub fn with_crop(mut self, crop: BBox) -> Self {
        self.crop = Some(crop);
        self
    }

    /// Fills `regions` before release.
    pub fn with_marked(mut self, regions: Vec<BBox>) -> Self {
        self.marked = regions;
        self
    }

    /// Releases the image if its egress digest is one a person approved.
    pub fn approved(mut self, digest: Digest) -> Self {
        self.approved.push(digest);
        self
    }
}

/// The minimised, redacted image the gate would release, before the rule decides.
#[derive(Clone, Debug)]
pub struct EgressPreview {
    image: SentImage,
    bytes: Arc<[u8]>,
    egress: Digest,
    redactions: BTreeMap<RedactionClass, u32>,
}

impl EgressPreview {
    /// The digest of the exact bytes that would leave.
    pub fn digest(&self) -> Digest {
        self.egress
    }

    /// The pixels that would leave.
    pub fn pixels(&self) -> &DecodedImage {
        self.image.pixels()
    }

    /// The PNG bytes that would leave.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The transforms from the source, redaction included.
    pub fn chain(&self) -> &TransformChain {
        self.image.chain()
    }

    /// Boxes filled, by redaction class.
    pub fn redactions(&self) -> &BTreeMap<RedactionClass, u32> {
        &self.redactions
    }

    /// The approval evidence for this image.
    pub fn evidence(&self, caption: impl Into<String>) -> EgressEvidence {
        let (width, height) = self.image.size();
        EgressEvidence {
            digest: self.egress,
            mime_type: "image/png".into(),
            width,
            height,
            overlay: None,
            caption: caption.into(),
        }
    }
}

/// The only code that releases an image to a remote backend.
///
/// [`release`](Self::release) runs six steps:
///
/// 1. **Destination.** The adk-core [`EgressPolicy`] decides each of the backend's hosts;
///    an unlisted host is refused here.
/// 2. **Minimise.** The image is cropped to the request's crop and downscaled to the
///    backend's limit; both are recorded in the transform chain.
/// 3. **Redact.** Every class the configuration requires is found by its [`Redactor`] and
///    filled with solid colour, then the caller's marked regions. A required class with no
///    redactor refuses the release.
/// 4. **Decide.** The rule for the host and data class says `deny`, `ask` or `allow`. `ask`
///    holds the release unless a person approved this exact egress digest.
/// 5. **Record.** The receipt goes to the [`EgressRecorder`] before the image is returned;
///    a recorder failure refuses the release.
/// 6. **Release.** The gate constructs the [`ReleasedImage`].
///
/// Every refusal is recorded too. A frozen kill switch or a cancelled call refuses at the
/// first step and again right before release.
pub struct EgressGate {
    policy: EgressPolicy,
    config: EgressConfig,
    redactors: Vec<Arc<dyn Redactor>>,
    recorder: Arc<dyn EgressRecorder>,
}

impl std::fmt::Debug for EgressGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressGate")
            .field("policy", &self.policy)
            .field("config", &self.config)
            .field("redactors", &self.redactors.iter().map(|r| r.class()).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl EgressGate {
    /// A gate with a destination policy, egress rules and a recorder.
    pub fn new(
        policy: EgressPolicy,
        config: EgressConfig,
        recorder: Arc<dyn EgressRecorder>,
    ) -> Self {
        Self { policy, config, redactors: Vec::new(), recorder }
    }

    /// Installs a redactor.
    pub fn with_redactor(mut self, redactor: Arc<dyn Redactor>) -> Self {
        self.redactors.push(redactor);
        self
    }

    /// The egress rules.
    pub fn config(&self) -> &EgressConfig {
        &self.config
    }

    /// Runs the destination, minimise and redact steps and returns what would leave, for an
    /// approval card. Records nothing unless it refuses.
    ///
    /// # Errors
    ///
    /// The refusals of [`release`](Self::release)'s first three steps.
    pub async fn preview(
        &self,
        cx: &VisionCx,
        request: &ReleaseRequest<'_>,
    ) -> Result<EgressPreview> {
        let hosts = remote_hosts(request.backend)?;
        self.prepare(cx, request, &hosts).await
    }

    /// Releases an image to a remote backend, or refuses.
    ///
    /// # Errors
    ///
    /// [`VisionError::EgressRefused`] at the destination or decide step,
    /// [`VisionError::InputTooLarge`] for an oversized image a backend refuses to have
    /// resized, [`VisionError::RedactionUnavailable`], [`VisionError::EgressHeld`] for a
    /// rule that asks, [`VisionError::Cancelled`] or the kill switch's error, and the
    /// recorder's error.
    pub async fn release(
        &self,
        cx: &VisionCx,
        request: ReleaseRequest<'_>,
    ) -> Result<ReleasedImage> {
        let hosts = remote_hosts(request.backend)?;
        let preview = self.prepare(cx, &request, &hosts).await?;
        let (action, rule) = hosts
            .iter()
            .map(|host| self.config.decide(host, &request.data_class))
            .max_by_key(|(action, _)| *action)
            .unwrap_or((self.config.default, None));
        let decision = match action {
            EgressAction::Allow => EgressDecision::Allowed { rule },
            EgressAction::Ask if request.approved.contains(&preview.egress) => {
                EgressDecision::Approved { rule }
            }
            EgressAction::Ask => {
                let reason = format!("held for a person's approval of {}", preview.egress);
                self.refuse(&request, &hosts, Some(preview.egress), "decide", &reason).await?;
                return Err(VisionError::EgressHeld {
                    host: hosts.join(", "),
                    data_class: request.data_class.to_string(),
                    egress: preview.egress,
                });
            }
            EgressAction::Deny => {
                let reason = match rule {
                    Some(index) => format!("egress rule {index} denies `{}`", request.data_class),
                    None => format!("the default egress action denies `{}`", request.data_class),
                };
                self.refuse(&request, &hosts, Some(preview.egress), "decide", &reason).await?;
                return Err(VisionError::EgressRefused { host: hosts.join(", "), reason });
            }
        };
        if let Err(err) = cx.check() {
            self.refuse(&request, &hosts, Some(preview.egress), "release", &err.to_string())
                .await?;
            return Err(err);
        }
        let receipt = EgressReceipt {
            id: uuid::Uuid::new_v4().to_string(),
            source: request.source.digest(),
            egress: preview.egress,
            backend: request.backend.id.clone(),
            hosts: hosts.clone(),
            data_class: request.data_class.clone(),
            chain: preview.image.chain().clone(),
            redactions: preview.redactions.clone(),
            decision,
            estimated_cost_micro_usd: request
                .backend
                .price
                .as_ref()
                .and_then(|p| p.request_cost(1)),
            released_at: Utc::now(),
        };
        self.recorder.record(EgressRecord::Released(receipt.clone())).await?;
        tracing::info!(
            egress.backend = %receipt.backend,
            egress.source = %receipt.source,
            egress.digest = %receipt.egress,
            egress.data_class = %receipt.data_class,
            "image released"
        );
        Ok(ReleasedImage::from_gate(preview.image, preview.bytes, receipt))
    }

    async fn prepare(
        &self,
        cx: &VisionCx,
        request: &ReleaseRequest<'_>,
        hosts: &[String],
    ) -> Result<EgressPreview> {
        if let Err(err) = cx.check() {
            self.refuse(request, hosts, None, "destination", &err.to_string()).await?;
            return Err(err);
        }
        for host in hosts {
            let (name, port) = split_host_port(host);
            if let Err(denial) = self.policy.decide(&EgressTarget::Name { host: name, port }) {
                let reason = denial.to_string();
                self.refuse(request, hosts, None, "destination", &reason).await?;
                return Err(VisionError::EgressRefused { host: host.clone(), reason });
            }
        }

        let source = request.source;
        let (w, h) = source.size();
        let mut chain = TransformChain::new(w, h);
        let mut pixels = source.shared_pixels();
        if let Some(crop) = request.crop {
            let (x, y, cw, ch) = crop.pixel_bounds(w, h);
            if cw == 0 || ch == 0 {
                let reason = format!("crop {crop:?} leaves no pixels of the {w}x{h} image");
                self.refuse(request, hosts, None, "minimise", &reason).await?;
                return Err(VisionError::InvalidInput(reason));
            }
            chain.push(Transform::Crop { x, y, width: cw, height: ch })?;
            pixels = Arc::new(ops::crop(&pixels, x, y, cw, ch));
        }
        let limits = request.backend.limits;
        if let Some((fw, fh)) = limits.fit(pixels.size()) {
            if limits.oversize == Oversize::Refuse {
                let (pw, ph) = pixels.size();
                let err = VisionError::InputTooLarge {
                    backend: request.backend.id.to_string(),
                    limit: limits.describe(),
                    actual: format!("{pw}x{ph}"),
                };
                self.refuse(request, hosts, None, "minimise", &err.to_string()).await?;
                return Err(err);
            }
            chain.push(Transform::Resize { width: fw, height: fh })?;
            pixels = Arc::new(ops::resize(&pixels, fw, fh));
        }

        let minimised = SentImage::new(source.digest(), Arc::clone(&pixels), chain.clone());
        let mut boxes: Vec<(RedactionClass, Vec<BBox>)> = Vec::new();
        let mut required: Vec<&RedactionClass> = Vec::new();
        for class in &self.config.redact {
            if !required.contains(&class) {
                required.push(class);
            }
        }
        for class in required {
            let redactors: Vec<&Arc<dyn Redactor>> =
                self.redactors.iter().filter(|r| &r.class() == class).collect();
            if redactors.is_empty() {
                let err = VisionError::RedactionUnavailable { class: class.to_string() };
                self.refuse(request, hosts, None, "redact", &err.to_string()).await?;
                return Err(err);
            }
            let mut found = Vec::new();
            for redactor in redactors {
                match redactor.find(cx, &minimised).await {
                    Ok(regions) => found.extend(regions),
                    Err(err) => {
                        let reason = format!("the `{class}` redactor failed: {err}");
                        self.refuse(request, hosts, None, "redact", &reason).await?;
                        return Err(err);
                    }
                }
            }
            boxes.push((class.clone(), found));
        }
        if !request.marked.is_empty() {
            let marked = request.marked.iter().map(|b| chain.bbox_to_target(b)).collect();
            boxes.push((RedactionClass::marked(), marked));
        }

        let mut redacted = (*pixels).clone();
        let mut redactions = BTreeMap::new();
        for (class, regions) in boxes {
            let mut count = 0u32;
            for region in &regions {
                if ops::fill(&mut redacted, region, Rgba::BLACK) {
                    count += 1;
                }
            }
            redactions.insert(class, count);
        }
        if redactions.values().any(|&n| n > 0) {
            chain.push(Transform::Redact)?;
        }
        let bytes: Arc<[u8]> = redacted.encode_png()?.into();
        let egress = Digest::of(&bytes);
        Ok(EgressPreview {
            image: SentImage::new(source.digest(), Arc::new(redacted), chain),
            bytes,
            egress,
            redactions,
        })
    }

    async fn refuse(
        &self,
        request: &ReleaseRequest<'_>,
        hosts: &[String],
        egress: Option<Digest>,
        step: &str,
        reason: &str,
    ) -> Result<()> {
        tracing::warn!(
            egress.backend = %request.backend.id,
            egress.step = step,
            egress.reason = reason,
            "image not released"
        );
        self.recorder
            .record(EgressRecord::Refused(EgressRefusal {
                source: request.source.digest(),
                egress,
                backend: request.backend.id.clone(),
                hosts: hosts.to_vec(),
                data_class: request.data_class.clone(),
                step: step.to_string(),
                reason: reason.to_string(),
                refused_at: Utc::now(),
            }))
            .await
    }
}

/// `host` or `host:port`; HTTPS's port when none is given.
fn split_host_port(host: &str) -> (&str, u16) {
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => {
            port.parse().map_or((host, 443), |port| (name, port))
        }
        _ => (host, 443),
    }
}

fn remote_hosts(backend: &BackendDescriptor) -> Result<Vec<String>> {
    match &backend.locality {
        Locality::Remote { hosts } if !hosts.is_empty() => Ok(hosts.clone()),
        Locality::Remote { .. } => Err(VisionError::InvalidInput(format!(
            "remote backend `{}` declares no hosts, so no egress rule can apply to it",
            backend.id
        ))),
        Locality::InProcess | Locality::Loopback => Err(VisionError::InvalidInput(format!(
            "backend `{}` runs on the device; only remote backends need a release",
            backend.id
        ))),
    }
}
