//! [`SamVideoTracker`]: `sam-3.1` video, masks with stable object identifiers over a clip
//! (spec 3.7 §7.7, §7.13), for an ambiguous track's escalation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::UsageUnit;
use adk_model::pricing::PricingCatalog;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{
    Answer, Job, SamConfig, SamMask, SamMaskDecoder, SamSegmenter, VENDOR, object_prompt, place,
    redact, status_error,
};
use crate::backend::{BackendDescriptor, PriceRef};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::egress::{ReleasedClip, ReleasedImage};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, RawRegion, Shape, TrackId};
use crate::traits::{
    SegmentPrompt, VideoAnalysis, VideoAnalyzer, VideoFrame, VideoInput, VideoRequest, VideoUsage,
};

/// Which frames `sam-3.1` tracks from a prompt: `metadata.propagation_direction`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Propagation {
    /// Before and after the earliest prompted frame (the API's default).
    #[default]
    Both,
    /// From the earliest prompted frame to the end of the clip.
    Forward,
    /// From the earliest prompted frame back to the start.
    Backward,
}

/// The most a request carries inline before the clip is uploaded through `/v1/files`.
pub const SAM_VIDEO_INLINE_LIMIT: usize = 20 * 1024 * 1024;

/// Meta's `sam-3.1` over a video clip, as a remote [`VideoAnalyzer`] answering `Track`.
///
/// Each concept phrase is one `POST /v1/responses` request with the released clip as an
/// `input_video` part — a data URL up to [`inline_limit`](Self::with_inline_limit), else a
/// `file_id` uploaded once through `/v1/files` — streamed through `response.completed`. Box
/// and point prompts go in one request on frame 0. The answer is one line per frame with
/// objects; each object becomes a region with a [`TrackId`] that is stable across the
/// frames of the analysis (objects of different concepts never share one), a mask when a
/// [`SamMaskDecoder`] is installed and its box otherwise, in pixels of the clip's frames.
///
/// | Item | Behaviour |
/// |------|-----------|
/// | Price | $0.20 per 1,000 frames, at least 50 frames per request, from the `adk-model` catalog; a three-concept query is three requests over every frame |
/// | Limits | Up to 15,000 frames (about 10 minutes) and 16 objects per frame per request; MP4 or MOV with H.264, H.265 or MPEG-4 |
/// | Credentials | As [`SamSegmenter`]: the Meta Model API key and Muse's base URL |
///
/// # Example
///
/// ```
/// use adk_vision::sam::{SamConfig, SamVideoTracker};
/// use adk_vision::{ReleasedImage, VideoAnalyzer};
///
/// let sam = SamVideoTracker::new(SamConfig::new("meta-key")).unwrap();
/// let price = VideoAnalyzer::<ReleasedImage>::descriptor(&sam).price.clone().unwrap();
/// // Twelve frames bill the 50-frame minimum; 900 frames are 900.
/// assert_eq!(price.request_cost(12), Some(10_000));
/// assert_eq!(price.request_cost(900), Some(180_000));
/// ```
pub struct SamVideoTracker {
    inner: SamSegmenter,
    descriptor: BackendDescriptor,
    propagation: Propagation,
    inline_limit: usize,
    uploads: Mutex<HashMap<Digest, String>>,
}

impl std::fmt::Debug for SamVideoTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamVideoTracker")
            .field("inner", &self.inner)
            .field("descriptor", &self.descriptor)
            .field("propagation", &self.propagation)
            .field("inline_limit", &self.inline_limit)
            .finish_non_exhaustive()
    }
}

impl SamVideoTracker {
    /// A client for `config`.
    ///
    /// # Errors
    ///
    /// As [`SamSegmenter::new`], and [`VisionError::InvalidInput`] when the catalog has no
    /// frame price for the model.
    pub fn new(config: SamConfig) -> Result<Self> {
        let model = config.model.clone();
        let inner = SamSegmenter::new(config)?;
        let rate = PricingCatalog::standard()
            .unit_price(Some(VENDOR), &model, UsageUnit::Frames)
            .map(|p| p.rate)
            .ok_or_else(|| {
                VisionError::InvalidInput(format!(
                    "the pricing catalog has no frame price for `{model}`"
                ))
            })?;
        let descriptor = BackendDescriptor::new(
            model.clone(),
            inner.descriptor.locality.clone(),
            CoordFrame::SentPixels,
        )
        .with_price(PriceRef::PerUnit {
            vendor: VENDOR.into(),
            model,
            rate,
            min_units_per_request: rate.minimum_per_request,
        })
        .with_concepts_per_request(1);
        Ok(Self {
            inner,
            descriptor,
            propagation: Propagation::Both,
            inline_limit: SAM_VIDEO_INLINE_LIMIT,
            uploads: Mutex::new(HashMap::new()),
        })
    }

    /// Answers with masks, decoded by `decoder`.
    pub fn with_mask_decoder(mut self, decoder: Arc<dyn SamMaskDecoder>) -> Self {
        self.inner = self.inner.with_mask_decoder(decoder);
        self
    }

    /// Names the hosts the egress policy checks instead of the base URL's.
    pub fn with_hosts<H: Into<String>>(mut self, hosts: impl IntoIterator<Item = H>) -> Self {
        let hosts: Vec<String> = hosts.into_iter().map(Into::into).collect();
        self.inner = self.inner.with_hosts(hosts.clone());
        self.descriptor.locality = crate::backend::Locality::Remote { hosts };
        self
    }

    /// Tracks in `direction` from the earliest prompted frame.
    pub fn with_propagation(mut self, direction: Propagation) -> Self {
        self.propagation = direction;
        self
    }

    /// Sends clips up to `bytes` inline and uploads larger ones; 0 always uploads.
    pub fn with_inline_limit(mut self, bytes: usize) -> Self {
        self.inline_limit = bytes;
        self
    }

    /// The `input_video` part: a data URL, or a file uploaded once.
    async fn media(&self, cx: &VisionCx, clip: &ReleasedClip) -> Result<Value> {
        if clip.bytes().len() <= self.inline_limit {
            let data = base64::engine::general_purpose::STANDARD.encode(clip.bytes());
            return Ok(json!({
                "type": "input_video",
                "video_url": format!("data:{};base64,{data}", clip.mime_type())
            }));
        }
        let digest = clip.receipt().egress;
        let mut uploads = self.uploads.lock().await;
        if let Some(id) = uploads.get(&digest) {
            return Ok(json!({"type": "input_video", "file_id": id}));
        }
        cx.check()?;
        let backend = self.descriptor.id.to_string();
        let extension = if clip.mime_type() == "video/quicktime" { "mov" } else { "mp4" };
        let file = reqwest::multipart::Part::bytes(clip.bytes().to_vec())
            .file_name(format!("clip.{extension}"))
            .mime_str(clip.mime_type())
            .map_err(|e| VisionError::InvalidInput(e.to_string()))?;
        let form = reqwest::multipart::Form::new().text("purpose", "user_data").part("file", file);
        let response = self
            .inner
            .http
            .post(format!("{}/files", self.inner.config.base_url))
            .bearer_auth(&self.inner.config.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| VisionError::Unavailable {
                backend: backend.clone(),
                message: redact(&e.to_string()),
            })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(status_error(&backend, status.as_u16(), &text));
        }
        let id = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
            .ok_or_else(|| VisionError::Parse {
                backend: backend.clone(),
                message: format!("the upload answer names no file id: {}", redact(&text)),
            })?;
        tracing::info!(vision.backend = %backend, sam.file = %id, clip.bytes = clip.bytes().len(), "clip uploaded for sam-3.1");
        uploads.insert(digest, id.clone());
        Ok(json!({"type": "input_video", "file_id": id}))
    }

    /// The request body for one text part and the clip's part.
    pub(crate) fn body(&self, text: &str, media: &Value) -> Value {
        let mut body = json!({
            "model": self.inner.config.model,
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}, media]
            }]
        });
        let mut metadata = serde_json::Map::new();
        if self.inner.config.include_confidence {
            metadata.insert("include_confidence".into(), json!("true"));
        }
        if self.inner.config.one_bit {
            metadata.insert("mask_encoding".into(), json!("one_bit"));
        }
        match self.propagation {
            Propagation::Both => {}
            Propagation::Forward => {
                metadata.insert("propagation_direction".into(), json!("forward"));
            }
            Propagation::Backward => {
                metadata.insert("propagation_direction".into(), json!("backward"));
            }
        }
        if !metadata.is_empty() {
            body["metadata"] = Value::Object(metadata);
        }
        body
    }

    /// One answer's records as regions by frame, objects numbered from `base`; returns the
    /// next free identifier.
    async fn collect(
        &self,
        answer: Answer,
        label: Option<String>,
        size: (u32, u32),
        base: u64,
        frames: &mut BTreeMap<u64, Vec<RawRegion>>,
    ) -> Result<u64> {
        let parse = |message: String| VisionError::Parse {
            backend: self.descriptor.id.to_string(),
            message,
        };
        for r in &answer.records {
            if r.source != size {
                return Err(parse(format!(
                    "a record is in a {}x{} frame, but the clip's frames are {}x{}",
                    r.source.0, r.source.1, size.0, size.1
                )));
            }
        }
        let rasters = match &self.inner.decoder {
            Some(decoder) => {
                let masks: Vec<SamMask> = answer.records.iter().map(|r| r.mask.clone()).collect();
                let rasters = decoder.decode(&masks).await?;
                if rasters.len() != masks.len() {
                    return Err(parse(format!(
                        "{} rasters for {} masks",
                        rasters.len(),
                        masks.len()
                    )));
                }
                Some(rasters)
            }
            None => None,
        };
        let mut next = base;
        for (i, record) in answer.records.iter().enumerate() {
            let object: u64 = record
                .object
                .parse()
                .map_err(|_| parse(format!("object id `{}` is not a number", record.object)))?;
            let id =
                base.checked_add(object).ok_or_else(|| parse("an object id overflows".into()))?;
            next = next.max(id + 1);
            let [x0, y0, x1, y1] = record.bounds;
            let shape = match rasters.as_ref().map(|r| &r[i]) {
                Some(bits) => Shape::Mask(place(bits, record, size).map_err(parse)?),
                None => Shape::Box(BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)),
            };
            frames.entry(record.frame).or_default().push(RawRegion {
                shape,
                label: label.clone().or_else(|| Some(format!("object {object}"))),
                score: record.confidence,
                track: Some(TrackId(id)),
            });
        }
        Ok(next)
    }
}

#[async_trait]
impl VideoAnalyzer<ReleasedImage> for SamVideoTracker {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn analyze(
        &self,
        cx: &VisionCx,
        video: &VideoInput<ReleasedImage>,
        request: &VideoRequest,
    ) -> Result<VideoAnalysis<RawRegion>> {
        let VideoRequest::Track { prompts } = request else {
            return Err(VisionError::Unsupported(format!(
                "`{}` video answers `Track`; ask questions with a `FramesAnalyzer` or Gemini",
                self.descriptor.id
            )));
        };
        let Some(clip) = video.clip.as_ref() else {
            return Err(VisionError::Unsupported(format!(
                "`{}` video reads a clip; segment sampled frames with `SamSegmenter`",
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
        let size = clip.clip().size();
        let mut jobs = Vec::new();
        let mut objects = Vec::new();
        for prompt in prompts {
            match prompt {
                SegmentPrompt::Concept { text } => {
                    jobs.push(Job { text: text.clone(), label: Some(text.clone()) });
                }
                other => objects.push(other),
            }
        }
        if !objects.is_empty() {
            jobs.push(Job { text: object_prompt(&objects, size)?, label: None });
        }
        if jobs.is_empty() {
            return Err(VisionError::InvalidInput("track needs at least one prompt".into()));
        }
        let media = self.media(cx, clip).await?;
        let mut frames = BTreeMap::new();
        let mut base = 0u64;
        let requests = jobs.len() as u64;
        for job in jobs {
            cx.check()?;
            let answer = self.inner.send(cx, &self.body(&job.text, &media)).await?;
            base = self.collect(answer, job.label, size, base, &mut frames).await?;
        }
        let info = clip.info();
        let pts = |frame: u64| {
            (info.frames > 0).then(|| {
                u64::try_from(
                    u128::from(frame) * u128::from(info.duration_ms) / u128::from(info.frames),
                )
                .unwrap_or(u64::MAX)
            })
        };
        Ok(VideoAnalysis {
            frames: frames
                .into_iter()
                .map(|(index, regions)| VideoFrame { index, pts_ms: pts(index), regions })
                .collect(),
            usage: VideoUsage { requests, ..VideoUsage::default() },
            ..VideoAnalysis::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_requests_carry_the_clip_and_the_propagation() {
        let sam = SamVideoTracker::new(SamConfig::new("k").with_one_bit_masks(true))
            .unwrap()
            .with_propagation(Propagation::Forward);
        let media = json!({"type": "input_video", "file_id": "file-1"});
        let body = sam.body("forklift", &media);
        assert_eq!(
            body,
            json!({
                "model": "sam-3.1",
                "stream": true,
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "forklift"}, media]
                }],
                "metadata": {"mask_encoding": "one_bit", "propagation_direction": "forward"}
            })
        );
    }

    #[test]
    fn the_frame_price_has_the_fifty_frame_minimum() {
        let sam = SamVideoTracker::new(SamConfig::new("k")).unwrap();
        let d = VideoAnalyzer::<ReleasedImage>::descriptor(&sam);
        let price = d.price.as_ref().unwrap();
        assert_eq!(price.unit(), Some(UsageUnit::Frames));
        assert_eq!(price.request_cost(1), Some(10_000));
        assert_eq!(price.request_cost(1_000), Some(200_000));
        assert_eq!(d.requests_for(3, false), 3);
    }
}
