//! The runtime's video call: [`VisionRuntime::analyze_video`].

use adk_core::{UsageCall, UsageUnit, UsageVector};

use super::{
    Backend, CallOptions, Inputs, Preflight, Prepared, VisionOutput, VisionRuntime, mismatch,
    planned_size,
};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::egress::{ClipReleaseRequest, DataClass, ReleasedClip, ReleasedImage};
use crate::error::{Result, VisionError};
use crate::geometry::{RegionNormalizer, TransformChain};
use crate::manifest::VisionTask;
use crate::media::{FrameMeta, SourceImage};
use crate::sent::SentImage;
use crate::traits::{
    Sampling, SegmentPrompt, VideoAnalysis, VideoFrame, VideoInput, VideoPlan, VideoRequest,
};
use crate::video::{SentClip, SourceClip};

/// What a video call analyses: a clip, for a backend that reads video itself, or frames a
/// frame source already sampled.
///
/// # Example
///
/// ```no_run
/// use adk_vision::{Sampling, SourceClip, VideoSource};
///
/// let clip = SourceClip::decode(std::fs::read("aisle.mp4").unwrap()).unwrap();
/// let video = VideoSource::clip(&clip, Sampling::at(1.0));
/// assert!(matches!(video, VideoSource::Clip { .. }));
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum VideoSource<'a> {
    /// A clip, which a backend that samples video itself reads at `sampling`.
    Clip {
        /// The clip.
        clip: &'a SourceClip,
        /// How the backend samples it.
        sampling: Sampling,
    },
    /// Frames already sampled, each with its stream identity and timing.
    Frames {
        /// `(frame, image)` pairs, in order.
        frames: &'a [(FrameMeta, SourceImage)],
        /// How they were sampled.
        sampling: Sampling,
    },
}

impl<'a> VideoSource<'a> {
    /// A clip, read by the backend at `sampling`.
    pub fn clip(clip: &'a SourceClip, sampling: Sampling) -> Self {
        Self::Clip { clip, sampling }
    }

    /// Frames already sampled at `sampling`.
    pub fn frames(frames: &'a [(FrameMeta, SourceImage)], sampling: Sampling) -> Self {
        Self::Frames { frames, sampling }
    }
}

impl VisionRuntime {
    /// Analyses a video on the video route.
    ///
    /// # Errors
    ///
    /// As [`analyze_video_with`](Self::analyze_video_with).
    pub async fn analyze_video(
        &self,
        cx: &VisionCx,
        video: VideoSource<'_>,
        request: &VideoRequest,
    ) -> Result<VisionOutput<VideoAnalysis>> {
        self.analyze_video_with(cx, video, request, &CallOptions::default()).await
    }

    /// Analyses a video with per-call options.
    ///
    /// The call opens one hold before anything is prepared (V-I6): the frames the backend
    /// analyses as [`UsageUnit::Frames`], at its per-frame price (with its per-request
    /// minimum), at zero cost on the device, or up to its own estimate for a token-billed
    /// backend. Sampled frames are prepared as images are, released one by one through the
    /// egress gate for a remote backend; a clip is released through
    /// [`EgressGate::release_clip`](crate::EgressGate::release_clip) as data class `video`
    /// unless the options name another. Track regions come back in source pixels of the
    /// clip or of their frame, with their track identifiers.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a call with no clip and no frame, and the errors
    /// of [`detect_with`](Self::detect_with).
    pub async fn analyze_video_with(
        &self,
        cx: &VisionCx,
        video: VideoSource<'_>,
        request: &VideoRequest,
        options: &CallOptions,
    ) -> Result<VisionOutput<VideoAnalysis>> {
        let route = self.route_for(VisionTask::AnalyzeVideo, options, self.videos.default_id())?;
        let (descriptor, backend) = self.videos.get(&route.primary)?;
        let (clip, frames, sampling) = match video {
            VideoSource::Clip { clip, sampling } => (Some(clip), &[][..], sampling),
            VideoSource::Frames { frames, sampling } => (None, frames, sampling),
        };
        if clip.is_none() && frames.is_empty() {
            return Err(VisionError::InvalidInput(
                "a video call needs a clip or at least one frame".into(),
            ));
        }
        let duration_ms = match clip {
            Some(clip) => clip.info().duration_ms,
            None => {
                let first = frames.first().map_or(0, |(f, _)| f.pts_ms);
                let last = frames.last().map_or(0, |(f, _)| f.pts_ms);
                let step = if sampling.fps > 0.0 { (1_000.0 / sampling.fps) as u64 } else { 0 };
                last.saturating_sub(first) + step
            }
        };
        let plan = VideoPlan {
            clip: clip.map(|c| c.info().clone()),
            frame_sizes: frames
                .iter()
                .map(|(_, image)| planned_size(image.size(), options.region, &descriptor.limits))
                .collect(),
            duration_ms,
            sampling,
        };
        let (billed, bound) = match backend {
            Backend::Local(b) => (b.frames(&plan), b.max_cost_micro_usd(&plan, request)),
            Backend::Remote(b) => (b.frames(&plan), b.max_cost_micro_usd(&plan, request)),
        };
        // A backend taking one concept per request reads the whole video once per request.
        let requests = match request {
            VideoRequest::Track { prompts } => {
                let concepts =
                    prompts.iter().filter(|p| matches!(p, SegmentPrompt::Concept { .. })).count();
                descriptor.requests_for(concepts, concepts < prompts.len())
            }
            _ => 1,
        };
        let hold = self.preflight_frames(cx, descriptor, billed, requests, bound).await?;
        let estimate = hold.cost.or(bound);
        let mut prepared = Vec::with_capacity(frames.len());
        for (_, image) in frames {
            prepared.push(
                self.prepare(cx, descriptor, backend.is_remote(), image, options.region, options)
                    .await?,
            );
        }
        let normalizers: Vec<(&FrameMeta, RegionNormalizer)> = frames
            .iter()
            .zip(&prepared)
            .map(|((meta, _), p)| (meta, Self::normalizer(descriptor, p)))
            .collect();
        let mut inputs = Inputs::of(&prepared);
        let metas: Vec<FrameMeta> = frames.iter().map(|(meta, _)| meta.clone()).collect();
        let task = VisionTask::AnalyzeVideo;
        let (raw, mut record) = match backend {
            Backend::Local(b) => {
                let images = prepared
                    .into_iter()
                    .map(|p| match p {
                        Prepared::Local(image) => Ok(image),
                        Prepared::Remote(_) => Err(mismatch(descriptor)),
                    })
                    .collect::<Result<Vec<SentImage>>>()?;
                if let Some(clip) = clip {
                    inputs.sources.push(clip.digest());
                }
                let input = VideoInput::new(
                    metas.into_iter().zip(images).collect(),
                    clip.map(|c| SentClip::new(c.clone())),
                    sampling,
                );
                self.metered(cx, task, descriptor, inputs, hold, b.analyze(cx, &input, request))
                    .await?
            }
            Backend::Remote(b) => {
                let images = prepared
                    .into_iter()
                    .map(|p| match p {
                        Prepared::Remote(released) => Ok(*released),
                        Prepared::Local(_) => Err(mismatch(descriptor)),
                    })
                    .collect::<Result<Vec<ReleasedImage>>>()?;
                let released = match clip {
                    Some(clip) => {
                        let released =
                            self.release_clip(cx, descriptor, clip, estimate, options).await?;
                        inputs.sources.push(clip.digest());
                        inputs.egress.push(released.receipt().egress);
                        Some(released)
                    }
                    None => None,
                };
                let input =
                    VideoInput::new(metas.into_iter().zip(images).collect(), released, sampling);
                self.metered(cx, task, descriptor, inputs, hold, b.analyze(cx, &input, request))
                    .await?
            }
        };
        let clip_normalizer = clip.map(|c| {
            let (w, h) = c.size();
            RegionNormalizer::new(
                c.digest(),
                TransformChain::new(w, h),
                descriptor.frame,
                descriptor.id.clone(),
            )
            .with_manifest(descriptor.manifest.clone())
        });
        let mut found = Vec::with_capacity(raw.frames.len());
        let mut regions = 0usize;
        for frame in raw.frames {
            let (normalizer, pts) = match &clip_normalizer {
                Some(normalizer) => (normalizer, frame.pts_ms),
                None => {
                    let (meta, normalizer) = normalizers
                        .iter()
                        .find(|(meta, _)| meta.index == frame.index)
                        .ok_or_else(|| VisionError::Parse {
                            backend: descriptor.id.to_string(),
                            message: format!(
                                "regions name frame {}, which the call did not send",
                                frame.index
                            ),
                        })?;
                    (normalizer, frame.pts_ms.or(Some(meta.pts_ms)))
                }
            };
            let mapped = normalizer.regions(frame.regions)?;
            regions += mapped.len();
            found.push(VideoFrame { index: frame.index, pts_ms: pts, regions: mapped });
        }
        record.regions = regions;
        let mut usage = raw.usage;
        if usage.frames == 0 {
            usage.frames = billed.saturating_mul(requests);
        }
        if usage.cost_micro_usd.is_none() {
            usage.cost_micro_usd = record.cost_micro_usd;
        }
        let value = VideoAnalysis { answer: raw.answer, events: raw.events, frames: found, usage };
        Ok(VisionOutput { value, calls: vec![record] })
    }

    async fn release_clip(
        &self,
        cx: &VisionCx,
        descriptor: &BackendDescriptor,
        clip: &SourceClip,
        estimate: Option<u64>,
        options: &CallOptions,
    ) -> Result<ReleasedClip> {
        let gate = self
            .gate
            .as_ref()
            .ok_or_else(|| VisionError::NotReleased { backend: descriptor.id.to_string() })?;
        let class = options.data_class.clone().unwrap_or_else(|| DataClass::new("video"));
        let mut request =
            ClipReleaseRequest::new(clip, descriptor, class).with_estimated_cost(estimate);
        for digest in &options.approved {
            request = request.approved(*digest);
        }
        gate.release_clip(cx, request).await
    }

    /// Opens the hold of one video call of `requests` requests reading `frames` frames
    /// each: the frames as [`UsageUnit::Frames`], and the cost bound — zero on the device,
    /// the per-frame price of every request (each with its minimum) for a per-unit backend,
    /// the backend's estimate for a token-billed one.
    async fn preflight_frames(
        &self,
        cx: &VisionCx,
        descriptor: &BackendDescriptor,
        frames: u64,
        requests: u64,
        estimate: Option<u64>,
    ) -> Result<Preflight> {
        cx.check()?;
        let units = UsageVector::new().with(UsageUnit::Frames, frames.saturating_mul(requests));
        let (vendor, model, cost, bound) = match &descriptor.price {
            None => {
                let vendor = match &descriptor.locality {
                    Locality::Remote { hosts } => hosts.first().cloned(),
                    Locality::InProcess | Locality::Loopback => None,
                };
                (vendor.unwrap_or_else(|| "local".into()), descriptor.id.to_string(), Some(0), 0)
            }
            Some(price) => match price.unit() {
                Some(unit) => {
                    let amount = if unit == UsageUnit::Requests { 1 } else { frames };
                    let cost = price.request_cost(amount).map(|c| c.saturating_mul(requests));
                    (price.vendor().to_string(), price.model().to_string(), cost, cost.unwrap_or(0))
                }
                None => (
                    price.vendor().to_string(),
                    price.model().to_string(),
                    None,
                    estimate.unwrap_or(0),
                ),
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
}
