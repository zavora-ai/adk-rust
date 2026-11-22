//! Routes, escalation triggers, call options and call records.

use adk_core::{UsageUnit, UsageVector};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendId, Locality};
use crate::digest::Digest;
use crate::egress::DataClass;
use crate::geometry::{BBox, EscalationReason};
use crate::manifest::VisionTask;

/// When the runtime escalates from the primary backend.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum Trigger {
    /// A region, block or description scored below `threshold`.
    LowConfidence {
        /// The threshold, in `[0, 1]`.
        threshold: f32,
    },
    /// The primary found nothing. The only trigger that sends a whole image, since no
    /// region exists to crop.
    NoResult,
    /// A second backend, run on the same image, found a region the primary did not, or the
    /// primary found one it did not: no match at `min_iou` or better. Detection and
    /// segmentation only.
    Disagreement {
        /// The second backend; it must run on the device.
        with: BackendId,
        /// The overlap two regions need to agree.
        min_iou: f32,
    },
}

/// Where a route escalates, on which triggers, and how much margin a crop keeps.
///
/// An escalation the egress gate refuses — a deny rule, an unlisted host, the on-device
/// profile — leaves the primary backend's answer standing, and the gate records the refusal
/// with its reason. An escalation the gate holds for approval stops the call, as any held
/// release does.
///
/// # Example
///
/// ```
/// use adk_vision::{Escalation, Trigger};
///
/// let to_cloud = Escalation::to("sam-3.1")
///     .on(Trigger::LowConfidence { threshold: 0.5 })
///     .with_margin(16.0)
///     .with_data_class("document_crop");
/// assert_eq!(to_cloud.triggers.len(), 1);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Escalation {
    /// The backend escalated to.
    pub backend: BackendId,
    /// The triggers, checked in order.
    pub triggers: Vec<Trigger>,
    /// Pixels of context kept around an escalated region's crop.
    pub margin: f32,
    /// The data class escalated crops are released as, when the escalation backend is
    /// remote; the call's own class otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_class: Option<DataClass>,
}

impl Escalation {
    /// Escalates to `backend`, with a 16-pixel margin and no trigger yet.
    pub fn to(backend: impl Into<String>) -> Self {
        Self {
            backend: BackendId::new(backend),
            triggers: Vec::new(),
            margin: 16.0,
            data_class: None,
        }
    }

    /// Adds a trigger.
    pub fn on(mut self, trigger: Trigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Sets the crop margin.
    pub fn with_margin(mut self, margin: f32) -> Self {
        self.margin = margin.max(0.0);
        self
    }

    /// Sets the data class of escalated crops.
    pub fn with_data_class(mut self, class: impl Into<String>) -> Self {
        self.data_class = Some(DataClass::new(class));
        self
    }
}

/// A primary backend and an optional escalation.
///
/// # Example
///
/// ```
/// use adk_vision::{Escalation, Route, Trigger};
///
/// let route = Route::to("rf-detr-m")
///     .escalate(Escalation::to("sam-3.1").on(Trigger::LowConfidence { threshold: 0.4 }));
/// assert_eq!(route.primary.as_str(), "rf-detr-m");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Route {
    /// The backend every call goes to first.
    pub primary: BackendId,
    /// Where the runtime escalates, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<Escalation>,
}

impl Route {
    /// A route to `backend`, with no escalation.
    pub fn to(backend: impl Into<String>) -> Self {
        Self { primary: BackendId::new(backend), escalation: None }
    }

    /// Adds an escalation.
    pub fn escalate(mut self, escalation: Escalation) -> Self {
        self.escalation = Some(escalation);
        self
    }
}

/// Per-call options.
///
/// # Example
///
/// ```
/// use adk_vision::{CallOptions, Route};
///
/// let options = CallOptions::new().with_route(Route::to("gemini")).with_data_class("photo");
/// assert_eq!(options.data_class.unwrap().as_str(), "photo");
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct CallOptions {
    /// The route for this call, instead of the task's configured one.
    pub route: Option<Route>,
    /// The data class of images released to a remote backend; `image` when unset.
    pub data_class: Option<DataClass>,
    /// Egress digests a person approved, for rules that ask.
    pub approved: Vec<Digest>,
    /// Regions to fill before any release, in source pixels.
    pub marked: Vec<BBox>,
    /// Only this region of each image, in source pixels: the backend is sent its crop, and
    /// answers come back in source pixels with the crop in their transform chain.
    pub region: Option<BBox>,
    /// Meters each image as a frame of a stream ([`UsageUnit::Frames`]) instead of an image
    /// ([`UsageUnit::Images`]), so a stream's frames count against frame caps and quotas
    /// (spec 3.7 X3, §7.9). The stream pipeline sets it on every detector call.
    pub frames: bool,
}

impl CallOptions {
    /// No options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Routes this call.
    pub fn with_route(mut self, route: Route) -> Self {
        self.route = Some(route);
        self
    }

    /// Sets the data class.
    pub fn with_data_class(mut self, class: impl Into<String>) -> Self {
        self.data_class = Some(DataClass::new(class));
        self
    }

    /// Adds an approved egress digest.
    pub fn approving(mut self, digest: Digest) -> Self {
        self.approved.push(digest);
        self
    }

    /// Marks regions for redaction.
    pub fn with_marked(mut self, regions: Vec<BBox>) -> Self {
        self.marked = regions;
        self
    }

    /// Works on `region` of each image only; a remote backend is sent nothing else.
    pub fn with_region(mut self, region: BBox) -> Self {
        self.region = Some(region);
        self
    }

    /// Meters each image as a stream frame: units go to [`UsageUnit::Frames`], and a backend
    /// priced per image is billed one unit per frame.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::UsageUnit;
    /// use adk_vision::CallOptions;
    ///
    /// assert_eq!(CallOptions::new().image_unit(), UsageUnit::Images);
    /// assert_eq!(CallOptions::new().as_frames().image_unit(), UsageUnit::Frames);
    /// ```
    pub fn as_frames(mut self) -> Self {
        self.frames = true;
        self
    }

    /// The unit each image of the call is metered in: [`UsageUnit::Frames`] for a stream's
    /// frames ([`as_frames`](Self::as_frames)), [`UsageUnit::Images`] otherwise.
    pub fn image_unit(&self) -> UsageUnit {
        if self.frames { UsageUnit::Frames } else { UsageUnit::Images }
    }

    pub(crate) fn data_class(&self) -> DataClass {
        self.data_class.clone().unwrap_or_else(|| DataClass::new("image"))
    }
}

/// One backend call, for the `vision.call` audit record: backend, locality, image digests,
/// units, cost, region count and duration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRecord {
    /// The task.
    pub task: VisionTask,
    /// The backend.
    pub backend: BackendId,
    /// Where it ran.
    pub locality: Locality,
    /// The source images' digests.
    pub images: Vec<Digest>,
    /// The digests released through the egress gate, for a remote backend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress: Vec<Digest>,
    /// Units recorded.
    pub units: UsageVector,
    /// Cost recorded in micro-USD: zero for local backends, the unit price for per-unit
    /// ones, `None` for token-billed ones, whose model call meters its own tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    /// Regions returned.
    pub regions: usize,
    /// Wall time in milliseconds.
    pub duration_ms: u64,
    /// Why this call was an escalation, if it was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<EscalationReason>,
}

/// A runtime result and the backend calls that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct VisionOutput<T> {
    /// The result.
    pub value: T,
    /// Every backend call, in order.
    pub calls: Vec<CallRecord>,
}
