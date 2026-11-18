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

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use adk_core::{AdkError, ErrorComponent, MeteredUsage, UsageCall, UsageUnit, UsageVector};

pub use calls::Edited;
pub use route::{CallOptions, CallRecord, Escalation, Route, Trigger, VisionOutput};

use crate::backend::{BackendDescriptor, BackendId, Oversize};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::edit::ops;
use crate::egress::{EgressGate, ReleaseRequest, ReleasedImage};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, RegionNormalizer, Transform, TransformChain};
use crate::manifest::VisionTask;
use crate::media::{MediaStore, SourceImage};
use crate::sent::SentImage;
use crate::traits::{Describer, Detector, DocumentReader, Editor, Embedder, Segmenter};

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
                routes: BTreeMap::new(),
                gate: None,
                store: None,
            },
            error: None,
        }
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
            VisionTask::AnalyzeVideo => None,
        }
    }

    /// The egress gate, if one is installed.
    pub fn egress_gate(&self) -> Option<&EgressGate> {
        self.gate.as_deref()
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
            VisionTask::AnalyzeVideo => false,
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

    /// Runs one backend call under the context's metering scope.
    ///
    /// The budget is checked and the spend reserved before the call starts; on success the
    /// call's images are committed as units, at the per-unit price for a priced backend and
    /// at zero cost for a local one. Cancellation ends the call at once.
    async fn metered<T, F>(
        &self,
        cx: &VisionCx,
        task: VisionTask,
        descriptor: &BackendDescriptor,
        inputs: Inputs,
        work: F,
    ) -> Result<(T, CallRecord)>
    where
        F: Future<Output = Result<T>> + Send,
    {
        cx.check()?;
        let images = inputs.sources.len() as u64;
        // X3: frame calls (phase B) count `UsageUnit::Frames` once adk-core has the unit.
        let units = UsageVector::new().with(UsageUnit::Images, images);
        let (vendor, model, cost) = match &descriptor.price {
            None => ("local".to_string(), descriptor.id.to_string(), Some(0)),
            Some(price) => {
                let amount = if price.unit() == Some(UsageUnit::Requests) { 1 } else { images };
                (price.vendor().to_string(), price.model().to_string(), price.request_cost(amount))
            }
        };
        // X2: chain `.with_units(units.clone())` once adk-core's `UsageCall::with_units` is on
        // main, so `RunBudget::max_units` refuses the call over a per-unit cap before its
        // request (spec 3.7 S7).
        let call = UsageCall::new(vendor, model)
            .with_max_cost_micro_usd(cost.unwrap_or(0))
            .with_units(units.clone());
        let failure: Arc<Mutex<Option<VisionError>>> = Arc::new(Mutex::new(None));
        let stash = Arc::clone(&failure);
        let cancel = cx.cancellation().clone();
        let committed = units.clone();
        let started = Instant::now();
        let inner = async move {
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => Err(VisionError::Cancelled),
                result = work => result,
            };
            match outcome {
                Ok(value) => {
                    Ok((value, MeteredUsage::new(committed).with_cost_micro_usd(cost.unwrap_or(0))))
                }
                Err(err) => {
                    let adk = AdkError::new(
                        ErrorComponent::Tool,
                        err.category(),
                        err.code(),
                        err.to_string(),
                    );
                    *stash.lock().unwrap_or_else(|e| e.into_inner()) = Some(err);
                    Err(adk)
                }
            }
        };
        let scope = cx.scope().clone();
        let metering = scope.meter(call, inner);
        // An unmetered context leaves the enclosing run's scope current for the model calls a
        // token-billed backend makes.
        let result = if scope.is_unmetered() {
            metering.await
        } else {
            scope.clone().within(metering).await
        };
        let value = match result {
            Ok(value) => value,
            Err(adk) => {
                let original = failure.lock().unwrap_or_else(|e| e.into_inner()).take();
                return Err(original.unwrap_or(VisionError::Core(adk)));
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
