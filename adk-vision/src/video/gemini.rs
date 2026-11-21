//! [`GeminiVideoAnalyzer`]: Gemini native video (spec 3.7 §7.13, X8, X9), feature
//! `gemini-video`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::GeminiModel;
use adk_model::gemini::{GeminiFile, GeminiRequestOptions, VideoMetadata, video_part_annotations};
use adk_model::pricing::PricingCatalog;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

pub use adk_model::gemini::MediaResolution;

use super::answer::{ModelAnswer, add_usage, ask_schema, generate, parse_time};
use crate::backend::{BackendDescriptor, BackendId, Locality, PriceRef};
use crate::cloud::extract_json;
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{ReleasedClip, ReleasedImage};
use crate::error::{Result, VisionError};
use crate::geometry::{CoordFrame, RawRegion};
use crate::traits::{
    VideoAnalysis, VideoAnalyzer, VideoEvent, VideoInput, VideoPlan, VideoRequest,
};

/// The most a request carries inline: Gemini's 20 MB request limit.
pub const GEMINI_INLINE_LIMIT: usize = 20 * 1024 * 1024;
/// The AI Studio host.
const GEMINI_HOST: &str = "generativelanguage.googleapis.com";
/// Uploaded files expire after 48 hours; reuse one for a little less.
const UPLOAD_REUSE: Duration = Duration::from_secs(47 * 3600);

/// Answers `Ask` over a clip with Gemini native video.
///
/// The clip comes from the egress gate ([`ReleasedClip`]). Up to
/// [`inline_limit`](Self::with_inline_limit) (20 MB) it goes inline; above it, it is
/// uploaded once through the File API, waited on until Gemini has processed it, and sent as
/// a `fileData` part — a second question about the same clip reuses the upload. Gemini
/// samples the clip at the call's [`Sampling::fps`](crate::Sampling) (its default is 1 fps)
/// and tokenises each frame at the [`MediaResolution`]: about 100 tokens a second of video
/// at `Low`, about 300 otherwise. The model answers in words with events as `m:ss.s`
/// times; its tokens and cost are reported in [`VideoUsage`](crate::VideoUsage).
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_model::GeminiModel;
/// use adk_vision::{GeminiVideoAnalyzer, MediaResolution, VisionRuntime};
///
/// let gemini = Arc::new(GeminiModel::new(std::env::var("GEMINI_API_KEY").unwrap(), "gemini-3.8-flash").unwrap());
/// let analyzer = GeminiVideoAnalyzer::new(gemini).with_media_resolution(MediaResolution::Low);
/// let runtime = VisionRuntime::builder().remote_video_analyzer(Arc::new(analyzer)).build().unwrap();
/// ```
pub struct GeminiVideoAnalyzer {
    model: Arc<GeminiModel>,
    descriptor: BackendDescriptor,
    media_resolution: Option<MediaResolution>,
    inline_limit: usize,
    max_output_tokens: u32,
    poll: Duration,
    processing_timeout: Duration,
    uploads: Mutex<HashMap<Digest, (GeminiFile, Instant)>>,
}

impl std::fmt::Debug for GeminiVideoAnalyzer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiVideoAnalyzer")
            .field("descriptor", &self.descriptor)
            .field("media_resolution", &self.media_resolution)
            .field("inline_limit", &self.inline_limit)
            .finish_non_exhaustive()
    }
}

impl GeminiVideoAnalyzer {
    /// Asks `model` on AI Studio.
    pub fn new(model: Arc<GeminiModel>) -> Self {
        let name = model.name().to_string();
        let descriptor = BackendDescriptor::new(
            name.clone(),
            Locality::Remote { hosts: vec![GEMINI_HOST.into()] },
            CoordFrame::SentPixels,
        )
        .with_price(PriceRef::Tokens {
            vendor: adk_core::vendor_for_model(&name).to_string(),
            model: name,
        });
        Self {
            model,
            descriptor,
            media_resolution: None,
            inline_limit: GEMINI_INLINE_LIMIT,
            max_output_tokens: 2_048,
            poll: Duration::from_secs(2),
            processing_timeout: Duration::from_secs(600),
            uploads: Mutex::new(HashMap::new()),
        }
    }

    /// Names the hosts the egress policy checks instead of AI Studio's, for a gateway.
    pub fn with_hosts<H: Into<String>>(mut self, hosts: impl IntoIterator<Item = H>) -> Self {
        self.descriptor.locality =
            Locality::Remote { hosts: hosts.into_iter().map(Into::into).collect() };
        self
    }

    /// Names the backend `id` instead of the model name.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = BackendId::new(id);
        self
    }

    /// Tokenises frames at `resolution`; Gemini's default otherwise.
    pub fn with_media_resolution(mut self, resolution: MediaResolution) -> Self {
        self.media_resolution = Some(resolution);
        self
    }

    /// Sends clips up to `bytes` inline and uploads larger ones; 0 always uploads.
    pub fn with_inline_limit(mut self, bytes: usize) -> Self {
        self.inline_limit = bytes.min(GEMINI_INLINE_LIMIT);
        self
    }

    /// Caps the answer at `tokens`; 2,048 by default.
    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// Polls an uploaded clip every `interval` for at most `timeout` while Gemini processes
    /// it; every 2 s for up to 10 minutes by default.
    pub fn with_processing_wait(mut self, interval: Duration, timeout: Duration) -> Self {
        self.poll = interval;
        self.processing_timeout = timeout;
        self
    }

    /// Input tokens of a clip of `duration_ms` sampled at `fps`: each frame at the media
    /// resolution (66 tokens at `Low`, 258 otherwise) and 32 tokens a second of audio.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{GeminiVideoAnalyzer, MediaResolution};
    ///
    /// // Twenty minutes at 1 fps: about 100 tokens a second at low resolution.
    /// assert_eq!(GeminiVideoAnalyzer::video_tokens(1_200_000, 1.0, Some(MediaResolution::Low)), 117_600);
    /// assert_eq!(GeminiVideoAnalyzer::video_tokens(1_200_000, 1.0, None), 348_000);
    /// ```
    pub fn video_tokens(duration_ms: u64, fps: f32, resolution: Option<MediaResolution>) -> u64 {
        let per_frame = match resolution {
            Some(MediaResolution::Low) => 66,
            _ => 258,
        };
        let fps = if fps.is_finite() && fps > 0.0 { f64::from(fps) } else { 1.0 };
        let frames = (duration_ms as f64 * fps / 1_000.0).ceil() as u64;
        frames * per_frame + duration_ms.div_ceil(1_000) * 32
    }

    fn prompt(question: &str, duration_ms: u64) -> String {
        format!(
            "Question about the video above, which is {} long: {question}\n\nAnswer with JSON \
             only: {{\"answer\": <the answer in words>, \"events\": [{{\"start\": \"m:ss.s\", \
             \"end\": \"m:ss.s\", \"text\": <what happens>}}]}}, with times from the start of \
             the video. List an event only for what the question asks about; if it does not \
             happen, say so in the answer and give no events.",
            super::timestamp(duration_ms)
        )
    }

    /// The clip's part: inline, or a file uploaded once and processed.
    async fn part(
        &self,
        cx: &VisionCx,
        clip: &ReleasedClip,
        metadata: &VideoMetadata,
    ) -> Result<Part> {
        let annotations = Some(video_part_annotations(metadata));
        let mime_type = clip.mime_type().to_string();
        if clip.bytes().len() <= self.inline_limit {
            return Ok(Part::InlineData {
                mime_type,
                data: clip.bytes().to_vec(),
                uri: None,
                annotations,
            });
        }
        let digest = clip.receipt().egress;
        let mut uploads = self.uploads.lock().await;
        uploads.retain(|_, (_, at)| at.elapsed() < UPLOAD_REUSE);
        let file = match uploads.get(&digest) {
            Some((file, _)) => file.clone(),
            None => {
                cx.check()?;
                let backend = self.descriptor.id.to_string();
                let unavailable = |e: adk_core::AdkError| VisionError::Unavailable {
                    backend: backend.clone(),
                    message: e.to_string(),
                };
                let uploaded = self
                    .model
                    .upload_file(clip.bytes().to_vec(), &mime_type, Some(&digest.to_string()))
                    .await
                    .map_err(unavailable)?;
                tracing::info!(vision.backend = %backend, gemini.file = %uploaded.name, clip.bytes = clip.bytes().len(), "clip uploaded through the File API");
                let ready = tokio::select! {
                    () = cx.cancellation().cancelled() => return Err(VisionError::Cancelled),
                    ready = self.model.wait_for_file(&uploaded.name, self.poll, self.processing_timeout) => ready.map_err(unavailable)?,
                };
                uploads.insert(digest, (ready.clone(), Instant::now()));
                ready
            }
        };
        Ok(Part::FileData { mime_type, file_uri: file.uri, annotations })
    }
}

#[async_trait]
impl VideoAnalyzer<ReleasedImage> for GeminiVideoAnalyzer {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn frames(&self, plan: &VideoPlan) -> u64 {
        match &plan.clip {
            Some(clip) => clip.sampled_frames(plan.sampling.fps),
            None => plan.frames(),
        }
    }

    fn max_cost_micro_usd(&self, plan: &VideoPlan, request: &VideoRequest) -> Option<u64> {
        let PriceRef::Tokens { vendor, model } = self.descriptor.price.as_ref()? else {
            return None;
        };
        let price = PricingCatalog::standard().lookup(Some(vendor), model)?;
        let question = match request {
            VideoRequest::Ask { question } => question.len() as u64,
            _ => 0,
        };
        let input = Self::video_tokens(plan.duration_ms, plan.sampling.fps, self.media_resolution)
            + question
            + 1_024;
        let today = chrono::Utc::now().date_naive();
        let usd = price.max_cost_usd(input, u64::from(self.max_output_tokens), today);
        adk_core::usd_to_micro_usd(usd).map(|m| m.saturating_add(1))
    }

    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<ReleasedImage>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis<RawRegion>> {
        let VideoRequest::Ask { question } = request else {
            return Err(VisionError::Unsupported(format!(
                "`{}` answers `Ask`; track with `sam-3.1` video",
                self.descriptor.id
            )));
        };
        let Some(clip) = video.clip.as_ref() else {
            return Err(VisionError::Unsupported(format!(
                "`{}` reads clips; ask about sampled frames with a `FramesAnalyzer`",
                self.descriptor.id
            )));
        };
        if !clip.is_for(&self.descriptor.id) {
            return Err(VisionError::NotReleased {
                backend: format!(
                    "{} (the clip was released to `{}`)",
                    self.descriptor.id,
                    clip.receipt().backend
                ),
            });
        }
        let info = clip.info();
        let fps = f64::from(video.sampling.fps);
        let metadata = if fps.is_finite() && fps > 0.0 {
            VideoMetadata::default().with_fps(fps)
        } else {
            VideoMetadata::default()
        };
        let part = self.part(cx, clip, &metadata).await?;
        let mut config = GenerateContentConfig {
            max_output_tokens: i32::try_from(self.max_output_tokens).ok(),
            response_schema: Some(ask_schema("time")),
            ..GenerateContentConfig::default()
        };
        if let Some(resolution) = self.media_resolution {
            GeminiRequestOptions::default()
                .with_media_resolution(resolution)
                .insert_into(&mut config);
        }
        let parts = vec![part, Part::Text { text: Self::prompt(question, info.duration_ms) }];
        let llm_request = LlmRequest {
            config: Some(config),
            ..LlmRequest::new(self.model.name(), vec![Content { role: "user".into(), parts }])
        };
        let plan = VideoPlan {
            clip: Some(info.clone()),
            frame_sizes: Vec::new(),
            duration_ms: info.duration_ms,
            sampling: video.sampling,
        };
        let bound = self.max_cost_micro_usd(&plan, request);
        let backend = self.descriptor.id.to_string();
        let llm: Arc<dyn Llm> = self.model.clone();
        let ModelAnswer { text, usage } = generate(cx, &llm, bound, llm_request, &backend).await?;
        let mut analysis = VideoAnalysis::default();
        add_usage(&mut analysis.usage, &self.descriptor, usage.as_ref());
        analysis.usage.frames = info.sampled_frames(video.sampling.fps);
        let root = extract_json(&text).ok_or_else(|| VisionError::Parse {
            backend: backend.clone(),
            message: format!("the answer holds no JSON: {text}"),
        })?;
        analysis.answer = root.get("answer").and_then(Value::as_str).map(str::to_string);
        for event in root.get("events").and_then(Value::as_array).into_iter().flatten() {
            let Some(start) = event.get("start").and_then(parse_time) else { continue };
            let start = start.min(info.duration_ms);
            let end = event
                .get("end")
                .and_then(parse_time)
                .unwrap_or(start)
                .clamp(start, info.duration_ms);
            let text = event.get("text").and_then(Value::as_str).unwrap_or_default();
            analysis.events.push(VideoEvent { start_ms: start, end_ms: end, text: text.into() });
        }
        analysis.events.sort_by_key(|e| e.start_ms);
        Ok(analysis)
    }
}
