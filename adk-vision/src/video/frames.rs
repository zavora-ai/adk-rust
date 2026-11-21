//! [`FramesAnalyzer`]: questions over sampled frames on any vision-language model.

use std::marker::PhantomData;
use std::sync::Arc;

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use async_trait::async_trait;
use serde_json::Value;

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
    VideoRequest,
};

/// Answers `Ask` over frames a frame source sampled, on the model of an [`LlmDescriber`]:
/// Qwen3-VL in this process ([`LlmDescriber::mistralrs`], feature `mistralrs`), a model
/// on a loopback server, or a remote model whose frames each pass the egress gate.
///
/// The frames go in batches of [`frames_per_call`](Self::with_frames_per_call), each image
/// numbered and stamped with its time in the prompt; the model names events by image
/// number, so their times come from the frames' presentation timestamps, never from the
/// model's arithmetic. Each call's tokens are reported in
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
    input: PhantomData<fn() -> I>,
}

impl<I> std::fmt::Debug for FramesAnalyzer<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramesAnalyzer")
            .field("model", &self.llm.name())
            .field("descriptor", &self.descriptor)
            .field("frames_per_call", &self.frames_per_call)
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

impl<I> FramesAnalyzer<I> {
    fn with(llm: Arc<dyn Llm>, descriptor: BackendDescriptor) -> Self {
        Self { llm, descriptor, frames_per_call: 8, max_output_tokens: 1_024, input: PhantomData }
    }

    /// Sends at most `frames` images per model call; 8 by default, at least 1.
    pub fn with_frames_per_call(mut self, frames: usize) -> Self {
        self.frames_per_call = frames.max(1);
        self
    }

    /// Caps each call's output at `tokens`; 1,024 by default.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// Names the backend `id` instead of the model name.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }

    fn prompt(question: &str, frames: &[FrameMeta], sampling: Sampling) -> String {
        let stamps: Vec<String> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| format!("image {} at {}", i + 1, timestamp(f.pts_ms)))
            .collect();
        format!(
            "These {} images are frames sampled in order from one video at {} frames per \
             second: {}.\n\nQuestion: {question}\n\nAnswer with JSON only: {{\"answer\": \
             <the answer in words>, \"events\": [{{\"start\": <first image number>, \"end\": \
             <last image number>, \"text\": <what happens>}}]}}. List an event only for what \
             the question asks about, naming the first and last image in which it is seen. If \
             it is not seen in these images, say so in the answer and give no events.",
            frames.len(),
            sampling.fps,
            stamps.join(", ")
        )
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
        let backend = self.descriptor.id.to_string();
        let mut analysis = VideoAnalysis::default();
        let mut answers: Vec<(u64, String)> = Vec::new();
        for batch in video.frames.chunks(self.frames_per_call) {
            cx.check()?;
            let metas: Vec<FrameMeta> = batch.iter().map(|(m, _)| m.clone()).collect();
            let sizes: Vec<(u32, u32)> = batch.iter().map(|(_, i)| i.sent().size()).collect();
            let mut parts =
                batch.iter().map(|(_, image)| part(image)).collect::<Result<Vec<_>>>()?;
            let prompt = Self::prompt(question, &metas, video.sampling);
            let bound = self
                .descriptor
                .token_estimate
                .map(|e| e.estimate_micro_usd(&sizes, prompt.len(), Some(self.max_output_tokens)));
            parts.push(Part::Text { text: prompt });
            let config = GenerateContentConfig {
                max_output_tokens: i32::try_from(self.max_output_tokens).ok(),
                response_schema: Some(ask_schema("image")),
                ..GenerateContentConfig::default()
            };
            let llm_request = LlmRequest {
                config: Some(config),
                ..LlmRequest::new(self.llm.name(), vec![Content { role: "user".into(), parts }])
            };
            let ModelAnswer { text, usage } =
                generate(cx, &self.llm, bound, llm_request, &backend).await?;
            add_usage(&mut analysis.usage, &self.descriptor, usage.as_ref());
            let root = extract_json(&text).ok_or_else(|| VisionError::Parse {
                backend: backend.clone(),
                message: format!("the answer holds no JSON: {text}"),
            })?;
            let at = |value: Option<&Value>| -> Option<u64> {
                let n = value?.as_u64().or_else(|| value?.as_str()?.trim().parse().ok())?;
                metas.get(usize::try_from(n).ok()?.checked_sub(1)?).map(|m| m.pts_ms)
            };
            let mut found = false;
            for event in root.get("events").and_then(Value::as_array).into_iter().flatten() {
                let Some(start) = at(event.get("start")) else { continue };
                let end = at(event.get("end")).unwrap_or(start).max(start);
                let text = event.get("text").and_then(Value::as_str).unwrap_or_default();
                analysis.events.push(VideoEvent {
                    start_ms: start,
                    end_ms: end,
                    text: text.into(),
                });
                found = true;
            }
            if let Some(answer) = root.get("answer").and_then(Value::as_str) {
                let from = if found { metas[0].pts_ms } else { u64::MAX };
                answers.push((from, answer.to_string()));
            }
        }
        analysis.events.sort_by_key(|e| e.start_ms);
        analysis.usage.frames = video.frames.len() as u64;
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
        Ok(analysis)
    }

    fn estimate(&self, plan: &VideoPlan) -> Option<u64> {
        let estimate = self.descriptor.token_estimate?;
        let total = plan
            .frame_sizes
            .chunks(self.frames_per_call)
            .map(|sizes| estimate.estimate_micro_usd(sizes, 1_024, Some(self.max_output_tokens)))
            .fold(0u64, u64::saturating_add);
        Some(total)
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
