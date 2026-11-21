//! The [`VideoAnalyzer`] cases: egress, metering, event times and region frames, and the
//! paths that refuse a call.

use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_core::egress::EgressPolicy;
use adk_core::{
    AdkError, BudgetTracker, ErrorCategory, GOVERNANCE_FROZEN_CODE, GovernanceControl,
    InMemorySpendLedger, RunBudget, UsageUnit,
};

use super::Target;
use super::probe::Harness;
use crate::backend::BackendDescriptor;
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{
    EgressAction, EgressConfig, EgressGate, InMemoryEgressRecorder, ReleasedImage,
};
use crate::error::{Result, VisionError};
use crate::geometry::RegionSet;
use crate::media::{FrameMeta, MediaStore, SourceImage};
use crate::runtime::{VideoSource, VisionOutput, VisionRuntime};
use crate::traits::{Sampling, VideoAnalysis, VideoAnalyzer, VideoRequest};
use crate::video::SourceClip;

/// A video analyzer under test.
pub type VideoAnalyzerTarget = Target<dyn VideoAnalyzer, dyn VideoAnalyzer<ReleasedImage>>;

/// What a video conformance case analyses, and what the answer must hold.
///
/// # Example
///
/// ```no_run
/// use adk_vision::conformance::VideoFixture;
/// use adk_vision::{Sampling, SourceClip, VideoRequest};
///
/// let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap()).unwrap();
/// let ask = VideoRequest::Ask { question: "When does the forklift enter the aisle?".into() };
/// // The answer's first event starts within 5 s of 0:45.
/// let fixture = VideoFixture::clip(clip, Sampling::at(1.0), ask).with_expected_event(45_000, 5_000);
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct VideoFixture {
    /// The clip, for an analyzer that reads video itself.
    pub clip: Option<SourceClip>,
    /// Sampled frames, for an analyzer that reads frames.
    pub frames: Vec<(FrameMeta, SourceImage)>,
    /// How the clip is read, or how the frames were sampled.
    pub sampling: Sampling,
    /// The request the analyzer answers.
    pub request: VideoRequest,
    /// A request the analyzer refuses as unsupported: `Caption` by default.
    pub refused: VideoRequest,
    /// When the first event of the answer starts, in milliseconds, and how far from it
    /// it may be.
    pub expected_event: Option<(u64, u64)>,
}

impl VideoFixture {
    /// `request` over `clip`, read at `sampling`.
    pub fn clip(clip: SourceClip, sampling: Sampling, request: VideoRequest) -> Self {
        Self {
            clip: Some(clip),
            frames: Vec::new(),
            sampling,
            request,
            refused: VideoRequest::Caption { chunk_ms: 10_000 },
            expected_event: None,
        }
    }

    /// `request` over `frames`, sampled at `sampling`.
    pub fn frames(
        frames: Vec<(FrameMeta, SourceImage)>,
        sampling: Sampling,
        request: VideoRequest,
    ) -> Self {
        Self {
            clip: None,
            frames,
            sampling,
            request,
            refused: VideoRequest::Caption { chunk_ms: 10_000 },
            expected_event: None,
        }
    }

    /// Requires the answer's first event to start within `tolerance_ms` of `start_ms`.
    pub fn with_expected_event(mut self, start_ms: u64, tolerance_ms: u64) -> Self {
        self.expected_event = Some((start_ms, tolerance_ms));
        self
    }

    /// Uses `request` as the request the analyzer refuses.
    pub fn with_refused(mut self, request: VideoRequest) -> Self {
        self.refused = request;
        self
    }

    fn source(&self) -> VideoSource<'_> {
        match &self.clip {
            Some(clip) => VideoSource::clip(clip, self.sampling),
            None => VideoSource::frames(&self.frames, self.sampling),
        }
    }

    /// The time the video covers, in milliseconds from its start.
    fn span(&self) -> (u64, u64) {
        if let Some(clip) = &self.clip {
            return (0, clip.info().duration_ms);
        }
        let step = if self.sampling.fps > 0.0 { (1_000.0 / self.sampling.fps) as u64 } else { 0 };
        let first = self.frames.first().map_or(0, |(f, _)| f.pts_ms);
        let last = self.frames.last().map_or(0, |(f, _)| f.pts_ms);
        (first, last + step)
    }

    /// Every digest the call may release: the clip's, or each frame's.
    fn digests(&self) -> Vec<Digest> {
        match &self.clip {
            Some(clip) => vec![clip.digest()],
            None => self.frames.iter().map(|(_, image)| image.digest()).collect(),
        }
    }
}

/// A runtime holding `target` with an egress gate that allows every destination when
/// `gate` is set, metered against `budget`.
fn harness(target: &VideoAnalyzerTarget, gate: bool, budget: RunBudget) -> Harness {
    let recorder = Arc::new(InMemoryEgressRecorder::new());
    let mut builder = VisionRuntime::builder()
        .media_store(MediaStore::new(Arc::new(InMemoryArtifactService::new())));
    if gate {
        builder = builder.egress_gate(EgressGate::new(
            EgressPolicy::public_only(),
            EgressConfig::default().with_default(EgressAction::Allow),
            recorder.clone(),
        ));
    }
    builder = match target {
        Target::Local(b) => builder.video_analyzer(Arc::clone(b)),
        Target::Remote(b) => builder.remote_video_analyzer(Arc::clone(b)),
    };
    let runtime = builder.build().unwrap_or_else(|err| {
        panic!("the runtime refused video analyzer `{}`: {err}", descriptor(target).id)
    });
    Harness {
        runtime,
        recorder,
        budget: Arc::new(BudgetTracker::new(budget)),
        ledger: Arc::new(InMemorySpendLedger::default()),
        governance: GovernanceControl::new(),
    }
}

fn descriptor(target: &VideoAnalyzerTarget) -> BackendDescriptor {
    match target {
        Target::Local(b) => b.descriptor().clone(),
        Target::Remote(b) => b.descriptor().clone(),
    }
}

async fn analyze(
    harness: &Harness,
    cx: &VisionCx,
    fixture: &VideoFixture,
    request: &VideoRequest,
) -> Result<VisionOutput<VideoAnalysis>> {
    harness.runtime.analyze_video(cx, fixture.source(), request).await
}

/// One call records its frames as `UsageUnit::Frames` in its call record and the run's
/// budget, and reports frames and requests in its usage; a local analyzer costs nothing, a
/// priced remote one reports a cost and spends it.
pub async fn video_usage(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let descriptor = descriptor(&target);
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    let out = analyze(&harness, &cx, fixture, &fixture.request)
        .await
        .unwrap_or_else(|err| panic!("`{}` failed on the fixture: {err}", descriptor.id));
    assert_eq!(out.calls.len(), 1, "`{}`: a video call is one call record", descriptor.id);
    let frames = out.calls[0].units.get(UsageUnit::Frames);
    assert!(frames > 0, "`{}`: the call recorded no frame unit", descriptor.id);
    assert_eq!(
        harness.budget.units().get(UsageUnit::Frames),
        frames,
        "`{}`: the run budget did not count the frames the call recorded",
        descriptor.id
    );
    let usage = &out.value.usage;
    assert!(usage.frames > 0 && usage.requests > 0, "`{}` reported {usage:?}", descriptor.id);
    if descriptor.locality.is_remote() {
        if descriptor.price.is_some() {
            assert!(
                usage.cost_micro_usd.is_some_and(|c| c > 0),
                "`{}` is priced but reported cost {:?}",
                descriptor.id,
                usage.cost_micro_usd
            );
            assert!(
                harness.budget.usage().cost_micro_usd > 0,
                "`{}` is priced but spent nothing",
                descriptor.id
            );
        }
    } else {
        assert_eq!(usage.cost_micro_usd, Some(0), "`{}` is local but has a cost", descriptor.id);
        assert_eq!(harness.budget.usage().cost_micro_usd, 0, "a local analyzer spent budget");
    }
}

/// A local analyzer leaves the egress recorder empty; a remote one's every release — the
/// clip, or each frame — is in its call record and names the fixture's own media.
pub async fn video_egress(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let descriptor = descriptor(&target);
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    let out = analyze(&harness, &cx, fixture, &fixture.request)
        .await
        .unwrap_or_else(|err| panic!("`{}` failed on the fixture: {err}", descriptor.id));
    let released = harness.recorder.released();
    if descriptor.locality.is_remote() {
        assert!(!released.is_empty(), "`{}`: a remote call released nothing", descriptor.id);
        assert_eq!(
            released.iter().map(|r| r.egress).collect::<Vec<_>>(),
            out.calls[0].egress,
            "`{}`: every release is in the call record, and only those",
            descriptor.id
        );
        let digests = fixture.digests();
        for receipt in &released {
            assert!(
                digests.contains(&receipt.source),
                "`{}`: a release names media the call was not given",
                descriptor.id
            );
            assert_eq!(receipt.backend, descriptor.id, "a release names another backend");
        }
    } else {
        assert!(
            harness.recorder.records().is_empty(),
            "`{}` is local but the egress recorder holds {:?}",
            descriptor.id,
            harness.recorder.records()
        );
        assert!(out.calls[0].egress.is_empty(), "a local call recorded egress");
    }
}

/// Event times are milliseconds from the start of the video, ordered, each ending no
/// earlier than it starts and inside the time the video covers, the first within the
/// fixture's tolerance; an `Ask` has an answer. Track regions are in source pixels of the
/// clip or of their frame, inside it, in frame order, each with its track.
pub async fn video_normalisation(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let descriptor = descriptor(&target);
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    let out = analyze(&harness, &cx, fixture, &fixture.request)
        .await
        .unwrap_or_else(|err| panic!("`{}` failed on the fixture: {err}", descriptor.id));
    let analysis = &out.value;
    let (from, to) = fixture.span();
    for event in &analysis.events {
        assert!(
            event.start_ms <= event.end_ms && event.start_ms >= from && event.end_ms <= to,
            "`{}`: event {event:?} is not inside the video's {from}–{to} ms",
            descriptor.id
        );
    }
    assert!(
        analysis.events.windows(2).all(|w| w[0].start_ms <= w[1].start_ms),
        "`{}`: events are not in time order",
        descriptor.id
    );
    if matches!(fixture.request, VideoRequest::Ask { .. }) {
        assert!(analysis.answer.is_some(), "`{}` answered no `Ask`", descriptor.id);
    }
    if let Some((start, tolerance)) = fixture.expected_event {
        let first = analysis.events.first().unwrap_or_else(|| {
            panic!("`{}` named no event; expected one at {start} ms", descriptor.id)
        });
        assert!(
            first.start_ms.abs_diff(start) <= tolerance,
            "`{}`: the first event starts at {} ms, not within {tolerance} ms of {start} ms",
            descriptor.id,
            first.start_ms
        );
    }
    assert!(
        analysis.frames.windows(2).all(|w| w[0].index < w[1].index),
        "`{}`: track frames are not in frame order",
        descriptor.id
    );
    for frame in &analysis.frames {
        let (digest, (w, h)) = match &fixture.clip {
            Some(clip) => (clip.digest(), clip.size()),
            None => {
                let (_, image) = fixture
                    .frames
                    .iter()
                    .find(|(meta, _)| meta.index == frame.index)
                    .unwrap_or_else(|| panic!("regions name frame {}, never sent", frame.index));
                (image.digest(), image.size())
            }
        };
        if let Some(pts) = frame.pts_ms {
            assert!(
                pts <= to,
                "`{}`: frame {} is at {pts} ms, after the video",
                descriptor.id,
                frame.index
            );
        }
        RegionSet::new(digest, w, h, frame.regions.clone()).validate().unwrap_or_else(|err| {
            panic!("`{}`: frame {} has a region outside it: {err}", descriptor.id, frame.index)
        });
        for region in &frame.regions {
            assert_eq!(region.origin.backend, descriptor.id, "a region names another backend");
            assert!(region.track.is_some(), "`{}`: a track region has no track", descriptor.id);
        }
    }
}

/// A request the analyzer does not answer is refused as `Unsupported`, and its frames go
/// back to the budget.
pub async fn video_refuses_unsupported(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let descriptor = descriptor(&target);
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    let err = analyze(&harness, &cx, fixture, &fixture.refused).await.err().unwrap_or_else(|| {
        panic!("`{}` answered {:?}, which it should refuse", descriptor.id, fixture.refused)
    });
    assert!(matches!(err, VisionError::Unsupported(_)), "`{}` refused with {err}", descriptor.id);
    assert_eq!(
        harness.budget.units().get(UsageUnit::Frames),
        0,
        "`{}`: a refused call kept its frames",
        descriptor.id
    );
}

/// A call whose context is already cancelled ends with `Cancelled`, and nothing leaves.
pub async fn video_cancellation(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    cx.cancellation().cancel();
    let err = analyze(&harness, &cx, fixture, &fixture.request).await.err();
    assert!(
        matches!(err, Some(VisionError::Cancelled)),
        "`{}`: a cancelled call returned {err:?}",
        descriptor(&target).id
    );
    assert!(harness.recorder.released().is_empty(), "a cancelled call released media");
}

/// A frozen kill switch refuses the call before the analyzer runs, and nothing leaves.
pub async fn video_kill_switch(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let harness = harness(&target, true, RunBudget::new());
    let cx = harness.cx();
    harness.governance.freeze("conformance: kill switch");
    let err = analyze(&harness, &cx, fixture, &fixture.request)
        .await
        .err()
        .unwrap_or_else(|| panic!("`{}` ran while frozen", descriptor(&target).id));
    let adk: AdkError = err.into();
    assert_eq!(adk.code, GOVERNANCE_FROZEN_CODE, "a frozen call failed with {adk}");
    assert!(harness.recorder.released().is_empty(), "media was released while frozen");
}

/// A run capped at one frame is refused before anything is released, naming the unit.
pub async fn video_frame_cap(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    let descriptor = descriptor(&target);
    let harness = harness(&target, true, RunBudget::new().max_units(UsageUnit::Frames, 1));
    let cx = harness.cx();
    let err = analyze(&harness, &cx, fixture, &fixture.request)
        .await
        .err()
        .unwrap_or_else(|| panic!("`{}` ran past a one-frame cap", descriptor.id));
    let adk: AdkError = err.into();
    assert!(adk.message.contains("frames"), "the cap refusal does not name the unit: {adk}");
    assert!(harness.recorder.released().is_empty(), "media was released past the cap");
    assert_eq!(harness.budget.units().get(UsageUnit::Frames), 0, "a refused call kept frames");
}

/// A remote analyzer without an egress gate is refused before it runs, as `Forbidden`.
pub async fn video_unreleased(target: VideoAnalyzerTarget, fixture: &VideoFixture) {
    if matches!(target, Target::Local(_)) {
        return;
    }
    let harness = harness(&target, false, RunBudget::new());
    let cx = harness.cx();
    let err = analyze(&harness, &cx, fixture, &fixture.request).await.err().unwrap_or_else(|| {
        panic!("remote `{}` ran without an egress gate", descriptor(&target).id)
    });
    assert!(matches!(err, VisionError::NotReleased { .. }), "expected NotReleased, got {err}");
    assert_eq!(AdkError::from(err).category, ErrorCategory::Forbidden);
}
