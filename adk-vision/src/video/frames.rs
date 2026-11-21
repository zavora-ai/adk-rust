//! [`FramesAnalyzer`]: questions over sampled frames on any vision-language model.

use std::marker::PhantomData;
use std::sync::Arc;

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::answer::{ModelAnswer, add_usage, ask_schema, generate};
use super::timestamp;
use crate::backend::BackendDescriptor;
use crate::cloud::{LlmDescriber, extract_json};
use crate::cx::VisionCx;
use crate::egress::ReleasedImage;
use crate::error::{Result, VisionError};
use crate::geometry::RawRegion;
use crate::media::FrameMeta;
use crate::sent::{SentImage, VisionInput};
use crate::traits::{
    Describer, Sampling, VideoAnalysis, VideoAnalyzer, VideoEvent, VideoInput, VideoPlan,
    VideoRequest, VideoUsage,
};

/// How [`FramesAnalyzer`] finds when something is first seen, for questions such as "when
/// does the forklift enter the aisle?" that a small model answers poorly over many frames
/// at once: it names events in frames without the subject, or misplaces them.
///
/// 1. **Target.** The thing the question is about: [`with_target`](Self::with_target), or
///    named by the model from the question in one text-only call.
/// 2. **Visibility.** Every [`stride`](Self::with_stride)-th frame, in order, is checked for
///    the target — one yes-or-no question per image, or
///    [`frames_per_check`](Self::with_frames_per_check) images a call — until one shows it.
/// 3. **Refine.** The question is asked over every frame from the last check that did not
///    show the target to `stride` frames after the first that did. When the model names an
///    event there, that is the answer; when it names none, the positive counts as a false
///    alarm and the checks resume after it.
///
/// An event's time is still the presentation time of the frame the model names. When no
/// frame shows the target, the answer says so and names no event.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_model::MockLlm;
/// use adk_vision::{FramesAnalyzer, LlmDescriber, VisibilityPass};
///
/// let model = Arc::new(MockLlm::new("claude-sonnet-5-5"));
/// let describer = LlmDescriber::remote(model, ["api.anthropic.com"]).unwrap();
/// // Frames sampled at 1 fps: check one in five, then ask over the eleven seconds around
/// // the first that shows the forklift.
/// let pass = VisibilityPass::new().with_target("the orange forklift").with_stride(5);
/// let analyzer = FramesAnalyzer::remote(&describer).with_visibility_pass(pass);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibilityPass {
    target: Option<String>,
    stride: usize,
    frames_per_check: usize,
}

impl Default for VisibilityPass {
    fn default() -> Self {
        Self { target: None, stride: 1, frames_per_check: 1 }
    }
}

impl VisibilityPass {
    /// Checks every frame, one per call, for the target the model names from the question.
    pub fn new() -> Self {
        Self::default()
    }

    /// Checks for `target`, a short noun phrase such as "the orange forklift", instead of
    /// asking the model what the question is about.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Checks every `frames`-th frame, at least 1 (every frame, the default); the refine
    /// step then reads every frame within `frames` of the first positive.
    pub fn with_stride(mut self, frames: usize) -> Self {
        self.stride = frames.max(1);
        self
    }

    /// Checks `frames` images per call, at least 1 (the default); more is cheaper, one is
    /// what a small model answers most reliably.
    pub fn with_frames_per_check(mut self, frames: usize) -> Self {
        self.frames_per_check = frames.max(1);
        self
    }
}

/// Answers `Ask` over frames a frame source sampled, on the model of an [`LlmDescriber`]:
/// Qwen3-VL in this process ([`LlmDescriber::mistralrs`], feature `mistralrs`), a model
/// on a loopback server, or a remote model whose frames each pass the egress gate.
///
/// The frames go in batches of [`frames_per_call`](Self::with_frames_per_call), each image
/// numbered and stamped with its time in the prompt; the model names events by image
/// number, so their times come from the frames' presentation timestamps, never from the
/// model's arithmetic. A [`VisibilityPass`] first finds the frames that show what the
/// question is about and asks only over those. Each call's tokens are reported in
/// [`VideoUsage`](crate::VideoUsage), at zero cost on the device.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_model::MockLlm;
/// use adk_vision::{FramesAnalyzer, LlmDescriber, VideoAnalyzer};
///
/// let claude = Arc::new(MockLlm::new("claude-sonnet-5-5"));
/// let describer = LlmDescriber::remote(claude, ["api.anthropic.com"]).unwrap();
/// let analyzer = FramesAnalyzer::remote(&describer).with_frames_per_call(16);
/// assert!(analyzer.descriptor().locality.is_remote());
/// ```
pub struct FramesAnalyzer<I = SentImage> {
    llm: Arc<dyn Llm>,
    descriptor: BackendDescriptor,
    frames_per_call: usize,
    max_output_tokens: u32,
    visibility: Option<VisibilityPass>,
    input: PhantomData<fn() -> I>,
}

impl<I> std::fmt::Debug for FramesAnalyzer<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramesAnalyzer")
            .field("model", &self.llm.name())
            .field("descriptor", &self.descriptor)
            .field("frames_per_call", &self.frames_per_call)
            .field("visibility", &self.visibility)
            .finish_non_exhaustive()
    }
}

impl FramesAnalyzer<SentImage> {
    /// Asks `describer`'s model, on this machine, over frames as the runtime prepared them.
    pub fn local(describer: &LlmDescriber<SentImage>) -> Self {
        Self::with(Arc::clone(describer.model()), Describer::descriptor(describer).clone())
    }
}

impl FramesAnalyzer<ReleasedImage> {
    /// Asks `describer`'s remote model over frames the egress gate released to it.
    pub fn remote(describer: &LlmDescriber<ReleasedImage>) -> Self {
        Self::with(Arc::clone(describer.model()), Describer::descriptor(describer).clone())
    }
}

/// One frame as the model receives it.
struct Shown {
    meta: FrameMeta,
    size: (u32, u32),
    part: Part,
}

impl<I> FramesAnalyzer<I> {
    fn with(llm: Arc<dyn Llm>, descriptor: BackendDescriptor) -> Self {
        Self {
            llm,
            descriptor,
            frames_per_call: 8,
            max_output_tokens: 1_024,
            visibility: None,
            input: PhantomData,
        }
    }

    /// Sends at most `frames` images per model call; 8 by default, at least 1. With a
    /// [`VisibilityPass`] this caps the refine step's frames.
    pub fn with_frames_per_call(mut self, frames: usize) -> Self {
        self.frames_per_call = frames.max(1);
        self
    }

    /// Caps each call's output at `tokens`; 1,024 by default.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// Finds the frames that show what the question is about before asking it; see
    /// [`VisibilityPass`].
    pub fn with_visibility_pass(mut self, pass: VisibilityPass) -> Self {
        self.visibility = Some(pass);
        self
    }

    /// Names the backend `id` instead of the model name.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }

    fn prompt(question: &str, frames: &[&Shown], fps: f32) -> String {
        let stamps: Vec<String> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| format!("image {} at {}", i + 1, timestamp(f.meta.pts_ms)))
            .collect();
        format!(
            "These {} images are frames sampled in order from one video at {} frames per \
             second: {}.\n\nQuestion: {question}\n\nAnswer with JSON only: {{\"answer\": \
             <the answer in words>, \"events\": [{{\"start\": <first image number>, \"end\": \
             <last image number>, \"text\": <what happens>}}]}}. List an event only for what \
             the question asks about, naming the first and last image in which it is seen. If \
             it is not seen in these images, say so in the answer and give no events.",
            frames.len(),
            fps,
            stamps.join(", ")
        )
    }

    /// One model call over `frames` with `prompt`, its usage added to `usage`: the answer's
    /// JSON.
    async fn call(
        &self,
        cx: &VisionCx,
        frames: &[&Shown],
        prompt: String,
        schema: Value,
        usage: &mut VideoUsage,
    ) -> Result<Value> {
        let backend = self.descriptor.id.to_string();
        let sizes: Vec<(u32, u32)> = frames.iter().map(|f| f.size).collect();
        let bound = self
            .descriptor
            .token_estimate
            .map(|e| e.estimate_micro_usd(&sizes, prompt.len(), Some(self.max_output_tokens)));
        let mut parts: Vec<Part> = frames.iter().map(|f| f.part.clone()).collect();
        parts.push(Part::Text { text: prompt });
        let config = GenerateContentConfig {
            max_output_tokens: i32::try_from(self.max_output_tokens).ok(),
            response_schema: Some(schema),
            ..GenerateContentConfig::default()
        };
        let llm_request = LlmRequest {
            config: Some(config),
            ..LlmRequest::new(self.llm.name(), vec![Content { role: "user".into(), parts }])
        };
        let ModelAnswer { text, usage: reported } =
            generate(cx, &self.llm, bound, llm_request, &backend).await?;
        add_usage(usage, &self.descriptor, reported.as_ref());
        extract_json(&text).ok_or_else(|| VisionError::Parse {
            backend,
            message: format!("the answer holds no JSON: {text}"),
        })
    }

    /// The question over `frames`: its answer in words, and the events it names, timed by
    /// the frames they name.
    async fn ask(
        &self,
        cx: &VisionCx,
        question: &str,
        frames: &[&Shown],
        fps: f32,
        usage: &mut VideoUsage,
    ) -> Result<(Option<String>, Vec<VideoEvent>)> {
        let prompt = Self::prompt(question, frames, fps);
        let root = self.call(cx, frames, prompt, ask_schema("image"), usage).await?;
        let at = |value: Option<&Value>| -> Option<u64> {
            let n = value?.as_u64().or_else(|| value?.as_str()?.trim().parse().ok())?;
            frames.get(usize::try_from(n).ok()?.checked_sub(1)?).map(|f| f.meta.pts_ms)
        };
        let mut events = Vec::new();
        for event in root.get("events").and_then(Value::as_array).into_iter().flatten() {
            let Some(start) = at(event.get("start")) else { continue };
            let end = at(event.get("end")).unwrap_or(start).max(start);
            let text = event.get("text").and_then(Value::as_str).unwrap_or_default();
            events.push(VideoEvent { start_ms: start, end_ms: end, text: text.into() });
        }
        let answer = root.get("answer").and_then(Value::as_str).map(str::to_string);
        Ok((answer, events))
    }

    /// The question over every frame, in batches of `frames_per_call`.
    async fn ask_in_batches(
        &self,
        cx: &VisionCx,
        question: &str,
        shown: &[Shown],
        sampling: Sampling,
        analysis: &mut VideoAnalysis<RawRegion>,
    ) -> Result<()> {
        let mut answers: Vec<(u64, String)> = Vec::new();
        for batch in shown.chunks(self.frames_per_call) {
            let frames: Vec<&Shown> = batch.iter().collect();
            let (answer, events) =
                self.ask(cx, question, &frames, sampling.fps, &mut analysis.usage).await?;
            if let Some(answer) = answer {
                let from = if events.is_empty() { u64::MAX } else { batch[0].meta.pts_ms };
                answers.push((from, answer));
            }
            analysis.events.extend(events);
        }
        // One batch answers for itself; across batches, the batches that saw the event do.
        analysis.answer = match answers.len() {
            0 => None,
            1 => Some(answers.remove(0).1),
            _ if answers.iter().all(|(from, _)| *from == u64::MAX) => Some(answers.remove(0).1),
            _ => Some(
                answers
                    .into_iter()
                    .filter(|(from, _)| *from != u64::MAX)
                    .map(|(from, answer)| format!("[{}] {answer}", timestamp(from)))
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        };
        Ok(())
    }

    /// The visibility pass, then the question around the first frame that shows its target.
    async fn ask_after_visibility(
        &self,
        cx: &VisionCx,
        question: &str,
        pass: &VisibilityPass,
        shown: &[Shown],
        sampling: Sampling,
        analysis: &mut VideoAnalysis<RawRegion>,
    ) -> Result<()> {
        let target = match &pass.target {
            Some(target) => target.clone(),
            None => {
                let prompt = format!(
                    "A question about a video: {question}\n\nWhat must be visible in a frame of \
                     the video to answer it? Name that object, person or thing as a short noun \
                     phrase. Answer with JSON only: {{\"target\": <the noun phrase>}}."
                );
                let schema = json!({
                    "type": "object",
                    "properties": {"target": {"type": "string"}},
                    "required": ["target"]
                });
                let root = self.call(cx, &[], prompt, schema, &mut analysis.usage).await?;
                root.get("target")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| VisionError::Parse {
                        backend: self.descriptor.id.to_string(),
                        message: format!("the model named no target for the question: {root}"),
                    })?
            }
        };
        let stride = pass.stride;
        let checked: Vec<usize> = (0..shown.len()).step_by(stride).collect();
        // `seen[i]` is whether `checked[i]` showed the target, once asked.
        let mut seen: Vec<Option<bool>> = vec![None; checked.len()];
        let mut next = 0;
        while next < checked.len() {
            if seen[next].is_none() {
                let end = (next + pass.frames_per_check).min(checked.len());
                let frames: Vec<&Shown> = checked[next..end].iter().map(|&i| &shown[i]).collect();
                let visible = self.visible(cx, &target, &frames, &mut analysis.usage).await?;
                for (offset, slot) in seen[next..end].iter_mut().enumerate() {
                    *slot = Some(visible.contains(&(offset + 1)));
                }
            }
            if seen[next] != Some(true) {
                next += 1;
                continue;
            }
            // From the last frame checked without the target to `stride` frames past the
            // first with it, at most `frames_per_call` of them, evenly spaced.
            let first = checked[next];
            let lo = first.saturating_sub(stride);
            let hi = (first + stride).min(shown.len() - 1);
            let step = (hi - lo + 1).div_ceil(self.frames_per_call);
            let frames: Vec<&Shown> = (lo..=hi).step_by(step).map(|i| &shown[i]).collect();
            let fps = sampling.fps / step as f32;
            let (answer, events) =
                self.ask(cx, question, &frames, fps, &mut analysis.usage).await?;
            tracing::debug!(
                vision.backend = %self.descriptor.id,
                vision.target = %target,
                frame.pts_ms = shown[first].meta.pts_ms,
                vision.events = events.len(),
                "visibility pass refined a positive"
            );
            if !events.is_empty() {
                analysis.answer = answer;
                analysis.events = events;
                return Ok(());
            }
            next += 1;
        }
        analysis.answer =
            Some(format!("{target} is not seen in any of the {} frames checked.", checked.len()));
        Ok(())
    }

    /// The numbers, from 1, of the frames in `frames` that the model says show `target`.
    async fn visible(
        &self,
        cx: &VisionCx,
        target: &str,
        frames: &[&Shown],
        usage: &mut VideoUsage,
    ) -> Result<Vec<usize>> {
        let (prompt, schema) = if frames.len() == 1 {
            (
                format!(
                    "Is {target} visible in this image? Answer with JSON only: {{\"visible\": \
                     true}} or {{\"visible\": false}}."
                ),
                json!({
                    "type": "object",
                    "properties": {"visible": {"type": "boolean"}},
                    "required": ["visible"]
                }),
            )
        } else {
            (
                format!(
                    "These {} images are frames from one video, numbered 1 to {} in order. In \
                     which of them is {target} visible? Answer with JSON only: {{\"visible\": \
                     [<the numbers of the images that show it>]}}, an empty list when none does.",
                    frames.len(),
                    frames.len()
                ),
                json!({
                    "type": "object",
                    "properties": {"visible": {"type": "array", "items": {"type": "integer"}}},
                    "required": ["visible"]
                }),
            )
        };
        let root = self.call(cx, frames, prompt, schema, usage).await?;
        let visible = match root.get("visible") {
            Some(Value::Bool(true)) => vec![1],
            Some(Value::Array(numbers)) => numbers
                .iter()
                .filter_map(|n| n.as_u64().or_else(|| n.as_str()?.trim().parse().ok()))
                .filter_map(|n| usize::try_from(n).ok())
                .filter(|n| (1..=frames.len()).contains(n))
                .collect(),
            _ => Vec::new(),
        };
        Ok(visible)
    }

    async fn run<T: VisionInput>(
        &self,
        cx: &VisionCx,
        video: &VideoInput<T>,
        request: &VideoRequest,
        part: impl Fn(&T) -> Result<Part>,
    ) -> Result<VideoAnalysis<RawRegion>> {
        let VideoRequest::Ask { question } = request else {
            return Err(VisionError::Unsupported(format!(
                "`{}` answers `Ask` over sampled frames; track with a `Tracker` or `sam-3.1` video",
                self.descriptor.id
            )));
        };
        if video.frames.is_empty() {
            return Err(VisionError::Unsupported(format!(
                "`{}` reads sampled frames, not clips; sample the clip with a frame source first",
                self.descriptor.id
            )));
        }
        cx.check()?;
        let shown = video
            .frames
            .iter()
            .map(|(meta, image)| {
                Ok(Shown { meta: meta.clone(), size: image.sent().size(), part: part(image)? })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut analysis = VideoAnalysis::default();
        match &self.visibility {
            None => {
                self.ask_in_batches(cx, question, &shown, video.sampling, &mut analysis).await?
            }
            Some(pass) => {
                self.ask_after_visibility(
                    cx,
                    question,
                    pass,
                    &shown,
                    video.sampling,
                    &mut analysis,
                )
                .await?;
            }
        }
        analysis.events.sort_by_key(|e| e.start_ms);
        analysis.usage.frames = video.frames.len() as u64;
        Ok(analysis)
    }

    /// The most the calls on `plan` can cost: every batch, or with a visibility pass, the
    /// target call, a check of every `stride`-th frame and a refine call after each.
    fn estimate(&self, plan: &VideoPlan) -> Option<u64> {
        let estimate = self.descriptor.token_estimate?;
        let cost = |sizes: &[(u32, u32)]| {
            estimate.estimate_micro_usd(sizes, 1_024, Some(self.max_output_tokens))
        };
        let Some(pass) = &self.visibility else {
            return Some(
                plan.frame_sizes
                    .chunks(self.frames_per_call)
                    .map(cost)
                    .fold(0, u64::saturating_add),
            );
        };
        let largest =
            plan.frame_sizes.iter().copied().max_by_key(|(w, h)| u64::from(*w) * u64::from(*h));
        let largest = largest.unwrap_or_default();
        let checked = plan.frame_sizes.len().div_ceil(pass.stride);
        let refine = vec![largest; (2 * pass.stride + 1).min(self.frames_per_call)];
        let checks = vec![largest; checked]
            .chunks(pass.frames_per_check)
            .map(cost)
            .fold(cost(&[]), u64::saturating_add);
        let refines = cost(&refine).saturating_mul(checked as u64);
        Some(checks.saturating_add(refines))
    }
}

fn check_inline(len: usize, backend: &str) -> Result<()> {
    if len > adk_core::MAX_INLINE_DATA_SIZE {
        return Err(VisionError::InputTooLarge {
            backend: backend.to_string(),
            limit: format!("{} bytes inline", adk_core::MAX_INLINE_DATA_SIZE),
            actual: format!("{len} bytes"),
        });
    }
    Ok(())
}

#[async_trait]
impl VideoAnalyzer<SentImage> for FramesAnalyzer<SentImage> {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn max_cost_micro_usd(&self, plan: &VideoPlan, _request: &VideoRequest) -> Option<u64> {
        self.estimate(plan)
    }

    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<SentImage>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis<RawRegion>> {
        let backend = self.descriptor.id.to_string();
        self.run(cx, video, request, |image| {
            let data = image.encode_png()?;
            check_inline(data.len(), &backend)?;
            Ok(Part::InlineData {
                mime_type: "image/png".into(),
                data,
                uri: None,
                annotations: None,
            })
        })
        .await
    }
}

#[async_trait]
impl VideoAnalyzer<ReleasedImage> for FramesAnalyzer<ReleasedImage> {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn max_cost_micro_usd(&self, plan: &VideoPlan, _request: &VideoRequest) -> Option<u64> {
        self.estimate(plan)
    }

    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<ReleasedImage>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis<RawRegion>> {
        if let Some((_, other)) = video.frames.iter().find(|(_, i)| !i.is_for(&self.descriptor.id))
        {
            return Err(VisionError::NotReleased {
                backend: format!(
                    "{} (the frame was released to `{}`)",
                    self.descriptor.id,
                    other.receipt().backend
                ),
            });
        }
        self.run(cx, video, request, |image| {
            check_inline(image.bytes().len(), image.receipt().backend.as_str())?;
            Ok(image.to_part())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Locality;
    use crate::budget::{ImageTokens, TokenEstimate};
    use crate::geometry::CoordFrame;

    #[test]
    fn the_hold_covers_every_check_and_refine_of_a_visibility_pass() {
        let estimate = TokenEstimate::new(ImageTokens::Claude, 3_000_000, 15_000_000);
        let descriptor = BackendDescriptor::new(
            "claude-sonnet-5-5",
            Locality::Remote { hosts: vec!["api.anthropic.com".into()] },
            CoordFrame::SentPixels,
        )
        .with_token_estimate(estimate);
        let llm: Arc<dyn Llm> = Arc::new(adk_model::MockLlm::new("claude-sonnet-5-5"));
        let plan = VideoPlan {
            clip: None,
            frame_sizes: vec![(448, 252); 24],
            duration_ms: 120_000,
            sampling: Sampling::at(0.2),
        };
        let cost = |frames: usize| {
            estimate.estimate_micro_usd(&vec![(448, 252); frames], 1_024, Some(1_024))
        };
        let direct = FramesAnalyzer::<ReleasedImage>::with(Arc::clone(&llm), descriptor.clone());
        assert_eq!(direct.estimate(&plan), Some(3 * cost(8)));
        // The target, 24 checks of one frame, and a refine of three frames after each.
        let pass = FramesAnalyzer::<ReleasedImage>::with(Arc::clone(&llm), descriptor.clone())
            .with_visibility_pass(VisibilityPass::new());
        assert_eq!(pass.estimate(&plan), Some(cost(0) + 24 * cost(1) + 24 * cost(3)));
        // Every fifth frame, four a check, and refines of at most eight frames.
        let strided = FramesAnalyzer::<ReleasedImage>::with(llm, descriptor)
            .with_visibility_pass(VisibilityPass::new().with_stride(5).with_frames_per_check(4));
        assert_eq!(strided.estimate(&plan), Some(cost(0) + cost(4) + cost(1) + 5 * cost(8)));
    }
}
