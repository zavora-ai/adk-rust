//! [`VisionRuntime`]: routing, image preparation, the egress gate, metering and escalation
//! (spec 3.7 §7.5).
//!
//! The runtime is the only place a [`SentImage`] is made. For each call it picks the route's
//! backend; prepares the image — downscaled to the backend's limit for an on-device
//! backend, released through the [`EgressGate`] for a remote one; meters the call against
//! the context's scope (units always, cost for priced backends, zero for local ones);
//! honours cancellation; maps every answer to source pixels; and escalates by cropping the
//! uncertain region, never sending a whole image when a region will do.

mod calls;
mod route;
mod video;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use adk_core::{MeteredUsage, UsageCall, UsageHold, UsageUnit, UsageVector};

pub use calls::Edited;
pub use route::{CallOptions, CallRecord, Escalation, Route, Trigger, VisionOutput};
pub use video::VideoSource;

use crate::backend::{BackendDescriptor, BackendId, InputLimits, Locality, Oversize};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::edit::ops;
use crate::egress::{EgressGate, ReleaseRequest, ReleasedImage};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, RegionNormalizer, Transform, TransformChain};
use crate::manifest::VisionTask;
use crate::media::{MediaStore, SourceImage};
use crate::sent::SentImage;
use crate::traits::{
    Describer, Detector, DocumentReader, Editor, Embedder, EmbeddingSpace, Segmenter, VideoAnalyzer,
};

/// A registered backend: on-device, handed [`SentImage`]s, or remote, handed
/// [`ReleasedImage`]s.
enum Backend<L: ?Sized, R: ?Sized> {
    Local(Arc<L>),
    Remote(Arc<R>),
}

impl<L: ?Sized, R: ?Sized> Clone for Backend<L, R> {
    fn clone(&self) -> Self {
        match self {
            Self::Local(b) => Self::Local(Arc::clone(b)),
            Self::Remote(b) => Self::Remote(Arc::clone(b)),
        }
    }
}

impl<L: ?Sized, R: ?Sized> Backend<L, R> {
    fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }
}

/// The backends of one trait, in registration order.
struct Registry<L: ?Sized, R: ?Sized> {
    entries: Vec<(BackendDescriptor, Backend<L, R>)>,
}

impl<L: ?Sized, R: ?Sized> Default for Registry<L, R> {
    fn default() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<L: ?Sized, R: ?Sized> Registry<L, R> {
    fn add(
        &mut self,
        task: VisionTask,
        descriptor: BackendDescriptor,
        backend: Backend<L, R>,
    ) -> Result<()> {
        let remote = descriptor.locality.is_remote();
        if remote != backend.is_remote() {
            return Err(VisionError::InvalidInput(format!(
                "backend `{}` declares {} locality but was registered as {}; register remote \
                 backends with the `remote_` builder methods, which hand them only released images",
                descriptor.id,
                if remote { "remote" } else { "on-device" },
                if backend.is_remote() { "remote" } else { "on-device" },
            )));
        }
        if self.entries.iter().any(|(d, _)| d.id == descriptor.id) {
            return Err(VisionError::InvalidInput(format!(
                "a {task:?} backend `{}` is already registered",
                descriptor.id
            )));
        }
        self.entries.push((descriptor, backend));
        Ok(())
    }

    fn get(&self, id: &BackendId) -> Result<(&BackendDescriptor, &Backend<L, R>)> {
        self.entries
            .iter()
            .find(|(d, _)| &d.id == id)
            .map(|(d, b)| (d, b))
            .ok_or_else(|| VisionError::BackendNotFound(id.to_string()))
    }

    /// The first on-device backend, else the first remote one: registration order decides,
    /// no backend is privileged.
    fn default_id(&self) -> Option<&BackendId> {
        self.entries
            .iter()
            .find(|(d, _)| !d.locality.is_remote())
            .or_else(|| self.entries.first())
            .map(|(d, _)| &d.id)
    }

    fn contains(&self, id: &BackendId) -> bool {
        self.entries.iter().any(|(d, _)| &d.id == id)
    }
}

type Detectors = Registry<dyn Detector, dyn Detector<ReleasedImage>>;
type Segmenters = Registry<dyn Segmenter, dyn Segmenter<ReleasedImage>>;
type Readers = Registry<dyn DocumentReader, dyn DocumentReader<ReleasedImage>>;
type Embedders = Registry<dyn Embedder, dyn Embedder<ReleasedImage>>;
type Describers = Registry<dyn Describer, dyn Describer<ReleasedImage>>;
type Editors = Registry<dyn Editor, dyn Editor<ReleasedImage>>;
type Videos = Registry<dyn VideoAnalyzer, dyn VideoAnalyzer<ReleasedImage>>;

/// The image a backend is handed for one call.
enum Prepared {
    Local(SentImage),
    Remote(Box<ReleasedImage>),
}

impl Prepared {
    fn image(&self) -> &SentImage {
        match self {
            Self::Local(image) => image,
            Self::Remote(released) => released.image(),
        }
    }

    fn egress(&self) -> Option<Digest> {
        match self {
            Self::Local(_) => None,
            Self::Remote(released) => Some(released.receipt().egress),
        }
    }
}

/// The images of one call, for its record.
#[derive(Default)]
struct Inputs {
    sources: Vec<Digest>,
    egress: Vec<Digest>,
}

impl Inputs {
    fn of(prepared: &[Prepared]) -> Self {
        Self {
            sources: prepared.iter().map(|p| p.image().source()).collect(),
            egress: prepared.iter().filter_map(Prepared::egress).collect(),
        }
    }
}

/// The budget hold of one call, opened before its images are prepared (V-I6).
struct Preflight {
    hold: Option<UsageHold>,
    units: UsageVector,
    /// The cost committed on success: zero for local, the unit price for per-unit, `None`
    /// for token-billed backends, whose model call commits its own cost.
    cost: Option<u64>,
}

/// The size a backend is sent for an image of `size`: its crop, fitted to the backend's
/// limits, as [`VisionRuntime::prepare`] and the egress gate make it.
fn planned_size(size: (u32, u32), crop: Option<BBox>, limits: &InputLimits) -> (u32, u32) {
    let base = match crop {
        Some(crop) => {
            let (_, _, width, height) = crop.pixel_bounds(size.0, size.1);
            (width, height)
        }
        None => size,
    };
    limits.fit(base).unwrap_or(base)
}

/// Routes vision calls to backends, prepares and releases their images, meters them, maps
/// their answers to source pixels and escalates.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::{DeterministicEditor, VisionRuntime};
///
/// let runtime = VisionRuntime::builder()
///     .editor(Arc::new(DeterministicEditor::new()))
///     .build()
///     .unwrap();
/// assert!(runtime.backends().iter().any(|id| id.as_str() == "deterministic-editor"));
/// ```
pub struct VisionRuntime {
    detectors: Detectors,
    segmenters: Segmenters,
    readers: Readers,
    embedders: Embedders,
    describers: Describers,
    editors: Editors,
    videos: Videos,
    routes: BTreeMap<VisionTask, Route>,
    gate: Option<Arc<EgressGate>>,
    store: Option<MediaStore>,
}

impl std::fmt::Debug for VisionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisionRuntime")
            .field("backends", &self.backends())
            .field("routes", &self.routes)
            .field("egress_gate", &self.gate.is_some())
            .finish_non_exhaustive()
    }
}

/// Builds a [`VisionRuntime`].
#[must_use]
pub struct VisionRuntimeBuilder {
    runtime: VisionRuntime,
    error: Option<VisionError>,
}

macro_rules! register {
    ($local:ident, $remote:ident, $field:ident, $task:expr, $trait:ident) => {
        #[doc = concat!("Registers an on-device `", stringify!($trait), "`.")]
        pub fn $local(mut self, backend: Arc<dyn $trait>) -> Self {
            let descriptor = backend.descriptor().clone();
            self.record(|rt| rt.$field.add($task, descriptor, Backend::Local(backend)));
            self
        }

        #[doc = concat!("Registers a remote `", stringify!($trait), "`, handed only images the egress gate released.")]
        pub fn $remote(mut self, backend: Arc<dyn $trait<ReleasedImage>>) -> Self {
            let descriptor = backend.descriptor().clone();
            self.record(|rt| rt.$field.add($task, descriptor, Backend::Remote(backend)));
            self
        }
    };
}

impl VisionRuntimeBuilder {
    fn record(&mut self, f: impl FnOnce(&mut VisionRuntime) -> Result<()>) {
        if self.error.is_none()
            && let Err(err) = f(&mut self.runtime)
        {
            self.error = Some(err);
        }
    }

    register!(detector, remote_detector, detectors, VisionTask::Detect, Detector);
    register!(segmenter, remote_segmenter, segmenters, VisionTask::Segment, Segmenter);
    register!(reader, remote_reader, readers, VisionTask::Read, DocumentReader);
    register!(embedder, remote_embedder, embedders, VisionTask::Embed, Embedder);
    register!(describer, remote_describer, describers, VisionTask::Describe, Describer);
    register!(editor, remote_editor, editors, VisionTask::Edit, Editor);
    register!(
        video_analyzer,
        remote_video_analyzer,
        videos,
        VisionTask::AnalyzeVideo,
        VideoAnalyzer
    );

    /// Sets a task's route.
    pub fn route(mut self, task: VisionTask, route: Route) -> Self {
        self.runtime.routes.insert(task, route);
        self
    }

    /// Installs the egress gate; without one, every remote call is refused.
    pub fn egress_gate(mut self, gate: EgressGate) -> Self {
        self.runtime.gate = Some(Arc::new(gate));
        self
    }

    /// Installs an egress gate shared with other parts of the application, such as the
    /// `ImagePrivacyGuardrail` (feature `guardrail`), so every path releases through one gate.
    pub fn shared_egress_gate(mut self, gate: Arc<EgressGate>) -> Self {
        self.runtime.gate = Some(gate);
        self
    }

    /// Stores edited images in `store`.
    pub fn media_store(mut self, store: MediaStore) -> Self {
        self.runtime.store = Some(store);
        self
    }

    /// Builds the runtime.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a backend whose locality does not match how it was
    /// registered, a duplicate backend, or a route that names an unknown backend or a
    /// trigger its task does not support.
    pub fn build(self) -> Result<VisionRuntime> {
        if let Some(err) = self.error {
            return Err(err);
        }
        let runtime = self.runtime;
        for (task, route) in &runtime.routes {
            runtime.check_route(*task, route)?;
        }
        Ok(runtime)
    }
}

impl VisionRuntime {
    /// A builder with no backend.
    pub fn builder() -> VisionRuntimeBuilder {
        VisionRuntimeBuilder {
            runtime: VisionRuntime {
                detectors: Registry::default(),
                segmenters: Registry::default(),
                readers: Registry::default(),
                embedders: Registry::default(),
                describers: Registry::default(),
                editors: Registry::default(),
                videos: Registry::default(),
                routes: BTreeMap::new(),
                gate: None,
                store: None,
            },
            error: None,
        }
    }

    /// The embedding space a call with `options` embeds into: the space of its embedding
    /// route's backend. A visual memory collection records the space it was built in and
    /// refuses vectors of any other.
    ///
    /// # Errors
    ///
    /// [`VisionError::BackendNotFound`] when no embedder serves the call.
    pub fn embedding_space(&self, options: &CallOptions) -> Result<EmbeddingSpace> {
        let route = self.route_for(VisionTask::Embed, options, self.embedders.default_id())?;
        let (_, backend) = self.embedders.get(&route.primary)?;
        Ok(match backend {
            Backend::Local(b) => b.space().clone(),
            Backend::Remote(b) => b.space().clone(),
        })
    }

    /// Every registered backend's id.
    pub fn backends(&self) -> Vec<BackendId> {
        let mut all = Vec::new();
        all.extend(self.detectors.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.segmenters.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.readers.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.embedders.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.describers.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.editors.entries.iter().map(|(d, _)| d.id.clone()));
        all.extend(self.videos.entries.iter().map(|(d, _)| d.id.clone()));
        all
    }

    /// The descriptor of a backend registered for `task`.
    pub fn descriptor(&self, task: VisionTask, id: &BackendId) -> Option<&BackendDescriptor> {
        match task {
            VisionTask::Detect => self.detectors.get(id).ok().map(|(d, _)| d),
            VisionTask::Segment => self.segmenters.get(id).ok().map(|(d, _)| d),
            VisionTask::Read => self.readers.get(id).ok().map(|(d, _)| d),
            VisionTask::Embed => self.embedders.get(id).ok().map(|(d, _)| d),
            VisionTask::Describe => self.describers.get(id).ok().map(|(d, _)| d),
            VisionTask::Edit => self.editors.get(id).ok().map(|(d, _)| d),
            VisionTask::AnalyzeVideo => self.videos.get(id).ok().map(|(d, _)| d),
        }
    }

    /// The route a call of `task` with `options` takes: the call's own route, the task's
    /// configured route, or the task's default backend (the first on-device one registered,
    /// else the first remote one).
    ///
    /// # Errors
    ///
    /// [`VisionError::BackendNotFound`] when no backend serves the task, and
    /// [`VisionError::InvalidInput`] for a call route that names an unknown backend.
    pub fn route(&self, task: VisionTask, options: &CallOptions) -> Result<Route> {
        let default = match task {
            VisionTask::Detect => self.detectors.default_id(),
            VisionTask::Segment => self.segmenters.default_id(),
            VisionTask::Read => self.readers.default_id(),
            VisionTask::Embed => self.embedders.default_id(),
            VisionTask::Describe => self.describers.default_id(),
            VisionTask::Edit => self.editors.default_id(),
            VisionTask::AnalyzeVideo => self.videos.default_id(),
        };
        self.route_for(task, options, default)
    }

    /// The egress gate, if one is installed.
    pub fn egress_gate(&self) -> Option<&EgressGate> {
        self.gate.as_deref()
    }

    /// The egress gate as a shared handle, for an `ImagePrivacyGuardrail` that releases
    /// through the same gate.
    pub fn shared_egress_gate(&self) -> Option<Arc<EgressGate>> {
        self.gate.clone()
    }

    /// The media store, if one is installed.
    pub fn media_store(&self) -> Option<&MediaStore> {
        self.store.as_ref()
    }

    fn has(&self, task: VisionTask, id: &BackendId) -> bool {
        match task {
            VisionTask::Detect => self.detectors.contains(id),
            VisionTask::Segment => self.segmenters.contains(id),
            VisionTask::Read => self.readers.contains(id),
            VisionTask::Embed => self.embedders.contains(id),
            VisionTask::Describe => self.describers.contains(id),
            VisionTask::Edit => self.editors.contains(id),
            VisionTask::AnalyzeVideo => self.videos.contains(id),
        }
    }

    fn check_route(&self, task: VisionTask, route: &Route) -> Result<()> {
        if !self.has(task, &route.primary) {
            return Err(VisionError::InvalidInput(format!(
                "the {task:?} route names `{}`, which is not a registered {task:?} backend",
                route.primary
            )));
        }
        let Some(escalation) = &route.escalation else {
            return Ok(());
        };
        if matches!(task, VisionTask::Embed | VisionTask::Edit | VisionTask::AnalyzeVideo) {
            return Err(VisionError::InvalidInput(format!("{task:?} routes do not escalate")));
        }
        if !self.has(task, &escalation.backend) {
            return Err(VisionError::InvalidInput(format!(
                "the {task:?} route escalates to `{}`, which is not a registered {task:?} backend",
                escalation.backend
            )));
        }
        for trigger in &escalation.triggers {
            if let Trigger::Disagreement { with, .. } = trigger {
                if !matches!(task, VisionTask::Detect | VisionTask::Segment) {
                    return Err(VisionError::InvalidInput(
                        "a disagreement trigger applies to detection and segmentation only".into(),
                    ));
                }
                let local = self.descriptor(task, with).is_some_and(|d| !d.locality.is_remote());
                if !local {
                    return Err(VisionError::InvalidInput(format!(
                        "the disagreement trigger's second backend `{with}` must be a registered \
                         on-device {task:?} backend, so the whole image never leaves for a second opinion"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The route for a call: the call's own, else the task's, else the default backend.
    fn route_for(
        &self,
        task: VisionTask,
        options: &CallOptions,
        default: Option<&BackendId>,
    ) -> Result<Route> {
        let route = match (&options.route, self.routes.get(&task)) {
            (Some(route), _) => {
                self.check_route(task, route)?;
                route.clone()
            }
            (None, Some(route)) => route.clone(),
            (None, None) => Route {
                primary: default
                    .cloned()
                    .ok_or_else(|| VisionError::BackendNotFound(format!("any {task:?} backend")))?,
                escalation: None,
            },
        };
        Ok(route)
    }

    /// Prepares the image a backend is handed: released through the gate for a remote
    /// backend, downscaled to its limit for an on-device one.
    async fn prepare(
        &self,
        cx: &VisionCx,
        descriptor: &BackendDescriptor,
        remote: bool,
        source: &SourceImage,
        crop: Option<BBox>,
        options: &CallOptions,
    ) -> Result<Prepared> {
        cx.check()?;
        if remote {
            let gate = self
                .gate
                .as_ref()
                .ok_or_else(|| VisionError::NotReleased { backend: descriptor.id.to_string() })?;
            let mut request = ReleaseRequest::new(source, descriptor, options.data_class())
                .with_marked(options.marked.clone());
            if let Some(crop) = crop {
                request = request.with_crop(crop);
            }
            for digest in &options.approved {
                request = request.approved(*digest);
            }
            return Ok(Prepared::Remote(Box::new(gate.release(cx, request).await?)));
        }
        let (w, h) = source.size();
        let mut chain = TransformChain::new(w, h);
        let mut pixels = source.shared_pixels();
        if let Some(crop) = crop {
            let (x, y, cw, ch) = crop.pixel_bounds(w, h);
            if cw == 0 || ch == 0 {
                return Err(VisionError::InvalidInput(format!(
                    "crop {crop:?} leaves no pixels of the {w}x{h} image"
                )));
            }
            chain.push(Transform::Crop { x, y, width: cw, height: ch })?;
            pixels = Arc::new(ops::crop(&pixels, x, y, cw, ch));
        }
        let limits = descriptor.limits;
        if let Some((fw, fh)) = limits.fit(pixels.size()) {
            if limits.oversize == Oversize::Refuse {
                let (pw, ph) = pixels.size();
                return Err(VisionError::InputTooLarge {
                    backend: descriptor.id.to_string(),
                    limit: limits.describe(),
                    actual: format!("{pw}x{ph}"),
                });
            }
            chain.push(Transform::Resize { width: fw, height: fh })?;
            pixels = Arc::new(ops::resize(&pixels, fw, fh));
        }
        Ok(Prepared::Local(SentImage::new(source.digest(), pixels, chain)))
    }

    /// Opens the budget hold of one call before any image is prepared or released (V-I6):
    /// the call's images (and pages, for reading) as units, and its cost bound — zero for an
    /// on-device backend, the unit price for a per-unit one, the [`TokenEstimate`] of a
    /// token-billed one. A hold the budget or spend ledger cannot cover refuses the call
    /// here, so nothing is sent and no egress is recorded.
    ///
    /// `sizes` are the sizes the backend will be sent, `prompt_chars` the length of any
    /// prompt, and `max_output_tokens` the call's output cap. `unit` is what each image
    /// counts as: [`UsageUnit::Images`], or [`UsageUnit::Frames`] for a stream's frames
    /// ([`CallOptions::as_frames`], spec 3.7 X3).
    ///
    /// [`TokenEstimate`]: crate::TokenEstimate
    #[allow(clippy::too_many_arguments)] // One call's metering inputs; a struct would only rename them.
    async fn preflight(
        &self,
        cx: &VisionCx,
        task: VisionTask,
        descriptor: &BackendDescriptor,
        sizes: &[(u32, u32)],
        prompt_chars: usize,
        max_output_tokens: Option<u32>,
        unit: UsageUnit,
    ) -> Result<Preflight> {
        cx.check()?;
        let images = sizes.len() as u64;
        let mut units = UsageVector::new().with(unit, images);
        if task == VisionTask::Read {
            units = units.with(UsageUnit::Pages, images);
        }
        let (vendor, model, cost, bound) = match &descriptor.price {
            // A remote backend with no published price (NVIDIA's hosted NIMs) is capped by
            // units, and its ledger entries name its host rather than `local`.
            None => {
                let vendor = match &descriptor.locality {
                    Locality::Remote { hosts } => hosts.first().cloned(),
                    Locality::InProcess | Locality::Loopback => None,
                };
                (vendor.unwrap_or_else(|| "local".into()), descriptor.id.to_string(), Some(0), 0)
            }
            Some(price) => match price.unit() {
                Some(priced) => {
                    let amount = if priced == UsageUnit::Requests { 1 } else { images };
                    let cost = price.request_cost(amount);
                    (price.vendor().to_string(), price.model().to_string(), cost, cost.unwrap_or(0))
                }
                // The model call meters its own tokens; the hold only has to fit the estimate.
                None => {
                    let estimate = descriptor.token_estimate.map_or(0, |estimate| {
                        estimate.estimate_micro_usd(sizes, prompt_chars, max_output_tokens)
                    });
                    (price.vendor().to_string(), price.model().to_string(), None, estimate)
                }
            },
        };
        let scope = cx.scope();
        if scope.is_unmetered() {
            return Ok(Preflight { hold: None, units, cost });
        }
        let call =
            UsageCall::new(vendor, model).with_max_cost_micro_usd(bound).with_units(units.clone());
        let hold = scope.begin_usage(&call).await?;
        Ok(Preflight { hold: Some(hold), units, cost })
    }

    /// Runs one backend call under the hold [`preflight`](Self::preflight) opened.
    ///
    /// On success the call's units are committed, at the per-unit price for a priced
    /// backend and at zero cost for a local or token-billed one; on failure the hold is
    /// released. The context's scope is current while the call runs, so a token-billed
    /// backend's model call meters itself against the same run. Cancellation ends the call
    /// at once.
    async fn metered<T, F>(
        &self,
        cx: &VisionCx,
        task: VisionTask,
        descriptor: &BackendDescriptor,
        inputs: Inputs,
        preflight: Preflight,
        work: F,
    ) -> Result<(T, CallRecord)>
    where
        F: Future<Output = Result<T>> + Send,
    {
        let Preflight { hold, units, cost } = preflight;
        if let Err(err) = cx.check() {
            if let Some(hold) = hold {
                hold.release().await;
            }
            return Err(err);
        }
        let cancel = cx.cancellation().clone();
        let started = Instant::now();
        let run = async move {
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(VisionError::Cancelled),
                result = work => result,
            }
        };
        let scope = cx.scope().clone();
        // An unmetered context leaves the enclosing run's scope current for the model calls a
        // token-billed backend makes.
        let outcome = if scope.is_unmetered() { run.await } else { scope.within(run).await };
        let value = match outcome {
            Ok(value) => {
                if let Some(hold) = hold {
                    let usage =
                        MeteredUsage::new(units.clone()).with_cost_micro_usd(cost.unwrap_or(0));
                    hold.commit(usage).await;
                }
                value
            }
            Err(err) => {
                if let Some(hold) = hold {
                    hold.release().await;
                }
                return Err(err);
            }
        };
        let record = CallRecord {
            task,
            backend: descriptor.id.clone(),
            locality: descriptor.locality.clone(),
            images: inputs.sources,
            egress: inputs.egress,
            units,
            cost_micro_usd: cost,
            regions: 0,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            escalation: None,
        };
        tracing::debug!(
            vision.task = ?task,
            vision.backend = %descriptor.id,
            vision.duration_ms = record.duration_ms,
            "vision call finished"
        );
        Ok((value, record))
    }

    fn normalizer(descriptor: &BackendDescriptor, prepared: &Prepared) -> RegionNormalizer {
        let image = prepared.image();
        RegionNormalizer::new(
            image.source(),
            image.chain().clone(),
            descriptor.frame,
            descriptor.id.clone(),
        )
        .with_manifest(descriptor.manifest.clone())
    }
}

fn mismatch(descriptor: &BackendDescriptor) -> VisionError {
    VisionError::Backend {
        backend: descriptor.id.to_string(),
        message: "the prepared image does not match the backend's locality".into(),
    }
}
