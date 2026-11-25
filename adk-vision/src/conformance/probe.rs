//! Targets, fixtures and the harness the conformance cases run in.

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_core::{BudgetTracker, GovernanceControl, InMemorySpendLedger, ModelCallScope, RunBudget};

use super::{
    DescriberTarget, DetectorTarget, EditorTarget, EmbedderTarget, ReaderTarget, SegmenterTarget,
};
use crate::backend::BackendDescriptor;
use crate::cx::VisionCx;
use crate::edit::{EditOp, Rgba};
use crate::egress::{EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder};
use crate::error::Result;
use crate::geometry::{BBox, Region};
use crate::media::{DecodedImage, MediaScope, MediaStore, SourceImage};
use crate::runtime::{CallOptions, CallRecord, VisionRuntime};
use crate::traits::{DescribeSpec, DetectQuery, ReadOptions, SegmentPrompt};

/// A backend under test: on-device, or remote and handed only released images.
pub enum Target<L: ?Sized, R: ?Sized> {
    /// An on-device backend.
    Local(Arc<L>),
    /// A remote backend.
    Remote(Arc<R>),
}

impl<L: ?Sized, R: ?Sized> Clone for Target<L, R> {
    fn clone(&self) -> Self {
        match self {
            Self::Local(b) => Self::Local(Arc::clone(b)),
            Self::Remote(b) => Self::Remote(Arc::clone(b)),
        }
    }
}

/// An object a fixture holds at a known position.
#[derive(Clone, Debug, PartialEq)]
pub struct Expected {
    /// Its label, when the backend is expected to name it.
    pub label: Option<String>,
    /// Its box, in source pixels.
    pub bbox: BBox,
}

/// The image and inputs a conformance case runs on.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Fixture {
    /// The source image.
    pub image: SourceImage,
    /// Objects at known positions.
    pub objects: Vec<Expected>,
    /// How far, in source pixels, each edge of a found box may be from its object.
    pub tolerance_px: f32,
    /// The detection query.
    pub query: DetectQuery,
    /// The segmentation prompts, in source pixels.
    pub prompts: Vec<SegmentPrompt>,
    /// The reading options.
    pub read: ReadOptions,
    /// What to ask a describer.
    pub describe: DescribeSpec,
    /// The edit to apply.
    pub edit: EditOp,
}

impl Fixture {
    /// Drawn shapes at known positions: a 640×480 white image with a red box at
    /// `(100, 80)–(260, 200)` and a blue one at `(380, 260)–(560, 420)`, labelled `red` and
    /// `blue`, within 2 px.
    ///
    /// # Panics
    ///
    /// Never; the image is built in memory.
    pub fn shapes() -> Self {
        let objects = vec![
            Expected { label: Some("red".into()), bbox: BBox::new(100.0, 80.0, 260.0, 200.0) },
            Expected { label: Some("blue".into()), bbox: BBox::new(380.0, 260.0, 560.0, 420.0) },
        ];
        let mut pixels = DecodedImage::filled(640, 480, [255, 255, 255, 255]);
        crate::edit::ops::fill(&mut pixels, &objects[0].bbox, Rgba([255, 0, 0, 255]));
        crate::edit::ops::fill(&mut pixels, &objects[1].bbox, Rgba([0, 0, 255, 255]));
        let image = SourceImage::from_pixels(pixels).expect("in-memory PNG encoding");
        Self {
            image,
            objects,
            tolerance_px: 2.0,
            query: DetectQuery::Concepts(vec!["red".into(), "blue".into()]),
            prompts: vec![
                SegmentPrompt::Concept { text: "red".into() },
                SegmentPrompt::Concept { text: "blue".into() },
            ],
            read: ReadOptions::default(),
            describe: DescribeSpec::new("Where are the coloured boxes?").with_regions(),
            edit: EditOp::Redact {
                regions: vec![BBox::new(100.0, 80.0, 260.0, 200.0)],
                color: Rgba::BLACK,
            },
        }
    }
}

/// What a probe call returned.
#[derive(Clone, Debug, Default)]
pub struct ProbeOutput {
    /// Regions, for backends that return them.
    pub regions: Vec<Region>,
    /// The backend calls.
    pub calls: Vec<CallRecord>,
}

/// A runtime holding the backend under test, with an egress gate that allows every
/// destination, a recorder, a budget tracker, a spend ledger, a kill switch and a media
/// store.
pub struct Harness {
    /// The runtime.
    pub runtime: VisionRuntime,
    /// Every release and refusal.
    pub recorder: Arc<InMemoryEgressRecorder>,
    /// The run budget the calls count against.
    pub budget: Arc<BudgetTracker>,
    /// The spend ledger.
    pub ledger: Arc<InMemorySpendLedger>,
    /// The kill switch.
    pub governance: GovernanceControl,
}

impl Harness {
    /// A context metered against the harness's budget and ledger, under its kill switch.
    pub fn cx(&self) -> VisionCx {
        let scope = ModelCallScope::new()
            .with_budget(Arc::clone(&self.budget))
            .with_spend_ledger(self.ledger.clone(), "conformance")
            .with_governance(self.governance.clone());
        VisionCx::new().with_scope(scope).with_media_scope(MediaScope::new(
            "conformance",
            "user",
            "session",
        ))
    }
}

enum Kind {
    Detector(DetectorTarget),
    Segmenter(SegmenterTarget),
    Reader(ReaderTarget),
    Embedder(EmbedderTarget),
    Describer(DescriberTarget),
    Editor(EditorTarget),
}

/// Any backend under test, so one case runs against every trait.
pub struct AnyProbe {
    kind: Kind,
}

macro_rules! probe_from {
    ($target:ty, $variant:ident) => {
        impl From<$target> for AnyProbe {
            fn from(target: $target) -> Self {
                Self { kind: Kind::$variant(target) }
            }
        }
    };
}

probe_from!(DetectorTarget, Detector);
probe_from!(SegmenterTarget, Segmenter);
probe_from!(ReaderTarget, Reader);
probe_from!(EmbedderTarget, Embedder);
probe_from!(DescriberTarget, Describer);
probe_from!(EditorTarget, Editor);

macro_rules! descriptor_of {
    ($target:expr) => {
        match $target {
            Target::Local(b) => b.descriptor().clone(),
            Target::Remote(b) => b.descriptor().clone(),
        }
    };
}

impl AnyProbe {
    /// The backend's descriptor.
    pub fn descriptor(&self) -> BackendDescriptor {
        match &self.kind {
            Kind::Detector(t) => descriptor_of!(t),
            Kind::Segmenter(t) => descriptor_of!(t),
            Kind::Reader(t) => descriptor_of!(t),
            Kind::Embedder(t) => descriptor_of!(t),
            Kind::Describer(t) => descriptor_of!(t),
            Kind::Editor(t) => descriptor_of!(t),
        }
    }

    /// `true` for a remote backend.
    pub fn is_remote(&self) -> bool {
        match &self.kind {
            Kind::Detector(t) => matches!(t, Target::Remote(_)),
            Kind::Segmenter(t) => matches!(t, Target::Remote(_)),
            Kind::Reader(t) => matches!(t, Target::Remote(_)),
            Kind::Embedder(t) => matches!(t, Target::Remote(_)),
            Kind::Describer(t) => matches!(t, Target::Remote(_)),
            Kind::Editor(t) => matches!(t, Target::Remote(_)),
        }
    }

    /// `true` when the backend answers with regions the coordinates case can check.
    pub fn returns_regions(&self) -> bool {
        matches!(
            self.kind,
            Kind::Detector(_) | Kind::Segmenter(_) | Kind::Reader(_) | Kind::Describer(_)
        )
    }

    /// A harness holding the backend, with an egress gate when `gate` is set.
    ///
    /// # Panics
    ///
    /// When the runtime refuses the backend, which the suite reports as a failure.
    pub fn harness(&self, gate: bool) -> Harness {
        let recorder = Arc::new(InMemoryEgressRecorder::new());
        let mut builder = VisionRuntime::builder()
            .media_store(MediaStore::new(Arc::new(InMemoryArtifactService::new())));
        if gate {
            let config = EgressConfig::default().with_default(EgressAction::Allow);
            builder = builder.egress_gate(EgressGate::new(
                EgressPolicy::allow_private_networks(),
                config,
                recorder.clone(),
            ));
        }
        builder = match &self.kind {
            Kind::Detector(Target::Local(b)) => builder.detector(Arc::clone(b)),
            Kind::Detector(Target::Remote(b)) => builder.remote_detector(Arc::clone(b)),
            Kind::Segmenter(Target::Local(b)) => builder.segmenter(Arc::clone(b)),
            Kind::Segmenter(Target::Remote(b)) => builder.remote_segmenter(Arc::clone(b)),
            Kind::Reader(Target::Local(b)) => builder.reader(Arc::clone(b)),
            Kind::Reader(Target::Remote(b)) => builder.remote_reader(Arc::clone(b)),
            Kind::Embedder(Target::Local(b)) => builder.embedder(Arc::clone(b)),
            Kind::Embedder(Target::Remote(b)) => builder.remote_embedder(Arc::clone(b)),
            Kind::Describer(Target::Local(b)) => builder.describer(Arc::clone(b)),
            Kind::Describer(Target::Remote(b)) => builder.remote_describer(Arc::clone(b)),
            Kind::Editor(Target::Local(b)) => builder.editor(Arc::clone(b)),
            Kind::Editor(Target::Remote(b)) => builder.remote_editor(Arc::clone(b)),
        };
        let runtime = builder.build().unwrap_or_else(|err| {
            panic!("the runtime refused backend `{}`: {err}", self.descriptor().id)
        });
        Harness {
            runtime,
            recorder,
            budget: Arc::new(BudgetTracker::new(RunBudget::new())),
            ledger: Arc::new(InMemorySpendLedger::default()),
            governance: GovernanceControl::new(),
        }
    }

    /// Runs the backend's task on `image` through the harness's runtime.
    ///
    /// # Errors
    ///
    /// The runtime's or the backend's error.
    pub async fn call(
        &self,
        harness: &Harness,
        cx: &VisionCx,
        fixture: &Fixture,
        image: &SourceImage,
    ) -> Result<ProbeOutput> {
        let rt = &harness.runtime;
        let options = CallOptions::default();
        Ok(match &self.kind {
            Kind::Detector(_) => {
                let out = rt.detect_with(cx, image, &fixture.query, &options).await?;
                ProbeOutput { regions: out.value, calls: out.calls }
            }
            Kind::Segmenter(_) => {
                let out = rt.segment_with(cx, image, &fixture.prompts, &options).await?;
                ProbeOutput { regions: out.value, calls: out.calls }
            }
            Kind::Reader(_) => {
                let out = rt.read_with(cx, image, &fixture.read, &options).await?;
                ProbeOutput {
                    regions: out.value.blocks.into_iter().map(|b| b.region).collect(),
                    calls: out.calls,
                }
            }
            Kind::Embedder(_) => {
                let out = rt.embed_images(cx, &[image], &options).await?;
                ProbeOutput { regions: Vec::new(), calls: out.calls }
            }
            Kind::Describer(_) => {
                let out = rt.describe_with(cx, &[image], &fixture.describe, &options).await?;
                ProbeOutput { regions: out.value.regions, calls: out.calls }
            }
            Kind::Editor(_) => {
                let out = rt.edit(cx, image, &fixture.edit, &options).await?;
                ProbeOutput { regions: Vec::new(), calls: out.calls }
            }
        })
    }
}
