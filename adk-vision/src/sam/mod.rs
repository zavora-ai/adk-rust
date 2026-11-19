//! The Meta `sam-3.1` client (spec 3.7 §7.7), feature `sam`.
//!
//! [`SamSegmenter`] is a remote [`Segmenter`] and [`Detector`]: each concept phrase is one
//! `POST /v1/responses` request with the released image, streamed with `stream: true`
//! through `response.completed`, and [`SamStreamParser`] reads the special-token answer —
//! one line per frame, each object a box in source pixels and a mask covering that box.
//! Box and point prompts go in one request in Meta's object-prompt grammar.
//!
//! | Item | Behaviour |
//! |------|-----------|
//! | Credentials | The Meta Model API key Muse uses (`META_API_KEY`, else `MODEL_API_KEY`), and Muse's base URL |
//! | Frame | Pixels of the image as sent ([`CoordFrame::SentPixels`]); the runtime maps them to source pixels |
//! | Price | $2.50 per 1,000 images from the `adk-model` catalog: one image unit, 2,500 µUSD, per request; a five-concept query is five requests |
//! | Rate limits | On 429 the client backs off exponentially with jitter, honouring `Retry-After`, then returns `RateLimited` with the hint |
//! | Masks | Placed into a source-sized mask when a [`SamMaskDecoder`] is installed; otherwise each object is its box |
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use adk_vision::{SamConfig, SamSegmenter, VisionRuntime};
//!
//! let sam = SamSegmenter::new(SamConfig::from_env()?)?;
//! let runtime = VisionRuntime::builder()
//!     .remote_segmenter(Arc::new(sam))
//!     .build()?;
//! # Ok::<(), adk_vision::VisionError>(())
//! ```

mod decode;
mod parse;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, UsageUnit};
use adk_model::pricing::PricingCatalog;
use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use serde_json::{Value, json};

pub use decode::{NodeMaskDecoder, SamMaskDecoder};
pub use parse::{SamMask, SamMaskEncoding, SamRecord, SamStreamParser};

use crate::backend::{BackendDescriptor, Locality, PriceRef};
use crate::cx::VisionCx;
use crate::egress::ReleasedImage;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Mask, RawRegion, Shape};
use crate::traits::{DetectQuery, Detector, SegmentPrompt, Segmenter};
use parse::{SamEvent, SseBuffer, event};

/// The spend-key vendor of the Meta Model API, which Muse and `sam-3.1` share.
const VENDOR: &str = "muse";
/// The most objects one object-prompt request may name.
const MAX_OBJECTS: usize = 16;

/// How to reach `sam-3.1`.
///
/// The key and base URL are Muse's (spec 3.7 Q5: one Meta Model API key authorises both,
/// confirmed live on 2026-11-19).
///
/// # Example
///
/// ```
/// use adk_vision::SamConfig;
///
/// let config = SamConfig::new("meta-key").with_confidence(true);
/// assert_eq!(config.host().unwrap(), "api.meta.ai");
/// assert!(!format!("{config:?}").contains("meta-key"));
/// ```
#[derive(Clone)]
pub struct SamConfig {
    api_key: String,
    base_url: String,
    model: String,
    max_retries: u32,
    include_confidence: bool,
    one_bit: bool,
    timeout: Duration,
}

impl std::fmt::Debug for SamConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamConfig")
            .field("api_key", &"[REDACTED]")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("max_retries", &self.max_retries)
            .field("include_confidence", &self.include_confidence)
            .field("one_bit", &self.one_bit)
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl SamConfig {
    /// `sam-3.1` at Muse's base URL with `api_key`.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: adk_model::muse::MUSE_API_BASE.to_string(),
            model: "sam-3.1".into(),
            max_retries: 3,
            include_confidence: false,
            one_bit: false,
            timeout: Duration::from_secs(300),
        }
    }

    /// The key and base URL of a Muse configuration.
    pub fn from_muse(muse: &adk_model::MuseConfig) -> Self {
        Self::new(muse.api_key.clone()).with_base_url(muse.effective_base_url())
    }

    /// The key from `META_API_KEY`, else `MODEL_API_KEY`, as Muse reads it.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when neither is set.
    pub fn from_env() -> Result<Self> {
        ["META_API_KEY", "MODEL_API_KEY"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|k| !k.is_empty()))
            .map(Self::new)
            .ok_or_else(|| {
                VisionError::InvalidInput(
                    "set META_API_KEY (or MODEL_API_KEY) to the Meta Model API key".into(),
                )
            })
    }

    /// Calls `base_url` (ending in `/v1`) instead of Meta's.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Retries a rate-limited request up to `retries` times.
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Asks for each object's confidence, returned as the region's score.
    pub fn with_confidence(mut self, include: bool) -> Self {
        self.include_confidence = include;
        self
    }

    /// Asks for `one_bit` masks, several times smaller than the default `lossless` ones.
    pub fn with_one_bit_masks(mut self, one_bit: bool) -> Self {
        self.one_bit = one_bit;
        self
    }

    /// Ends a request that takes longer than `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The host requests go to.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a base URL with no host.
    pub fn host(&self) -> Result<String> {
        let rest = self.base_url.split_once("://").map_or(self.base_url.as_str(), |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.split(':').next().unwrap_or_default(),
        };
        if host.is_empty() {
            return Err(VisionError::InvalidInput(format!(
                "the sam-3.1 base URL `{}` names no host",
                self.base_url
            )));
        }
        Ok(host.to_ascii_lowercase())
    }
}

/// One `sam-3.1` request: the text part and the label its regions carry.
struct Job {
    text: String,
    label: Option<String>,
}

/// What one request answered.
struct Answer {
    records: Vec<SamRecord>,
}

/// Meta's `sam-3.1` as a remote [`Segmenter`] and [`Detector`].
///
/// Regions are in pixels of the released image ([`CoordFrame::SentPixels`]). Each object
/// is a [`Shape::Mask`] the size of the released image when a [`SamMaskDecoder`] is
/// installed ([`with_mask_decoder`](Self::with_mask_decoder)), else a [`Shape::Box`].
///
/// # Example
///
/// ```
/// use adk_vision::{CoordFrame, Locality, SamConfig, SamSegmenter, Segmenter, ReleasedImage};
///
/// let sam = SamSegmenter::new(SamConfig::new("meta-key")).unwrap();
/// let d = Segmenter::<ReleasedImage>::descriptor(&sam);
/// assert_eq!(d.frame, CoordFrame::SentPixels);
/// assert_eq!(d.locality, Locality::Remote { hosts: vec!["api.meta.ai".into()] });
/// assert_eq!(d.price.as_ref().unwrap().request_cost(1), Some(2_500));
/// assert_eq!(d.concepts_per_request, Some(1));
/// ```
pub struct SamSegmenter {
    config: SamConfig,
    http: reqwest::Client,
    descriptor: BackendDescriptor,
    decoder: Option<Arc<dyn SamMaskDecoder>>,
}

impl std::fmt::Debug for SamSegmenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamSegmenter")
            .field("config", &self.config)
            .field("descriptor", &self.descriptor)
            .field("mask_decoder", &self.decoder.is_some())
            .finish()
    }
}

impl SamSegmenter {
    /// A client for `config`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a base URL with no host, or when the `adk-model`
    /// catalog has no image price for the model; [`VisionError::Backend`] when the HTTP
    /// client cannot be built.
    pub fn new(config: SamConfig) -> Result<Self> {
        let host = config.host()?;
        let rate = PricingCatalog::standard()
            .unit_price(Some(VENDOR), &config.model, UsageUnit::Images)
            .map(|p| p.rate)
            .ok_or_else(|| {
                VisionError::InvalidInput(format!(
                    "the pricing catalog has no image price for `{}`",
                    config.model
                ))
            })?;
        let descriptor = BackendDescriptor::new(
            config.model.clone(),
            Locality::Remote { hosts: vec![host] },
            CoordFrame::SentPixels,
        )
        .with_price(PriceRef::PerUnit {
            vendor: VENDOR.into(),
            model: config.model.clone(),
            rate,
            min_units_per_request: 0,
        })
        .with_concepts_per_request(1);
        let http = reqwest::Client::builder().timeout(config.timeout).build().map_err(|e| {
            VisionError::Backend { backend: config.model.clone(), message: e.to_string() }
        })?;
        Ok(Self { config, http, descriptor, decoder: None })
    }

    /// Names the hosts the egress policy checks instead of the base URL's, for a gateway
    /// that forwards to Meta.
    pub fn with_hosts<H: Into<String>>(mut self, hosts: impl IntoIterator<Item = H>) -> Self {
        self.descriptor.locality =
            Locality::Remote { hosts: hosts.into_iter().map(Into::into).collect() };
        self
    }

    /// Answers with masks, decoded by `decoder`.
    pub fn with_mask_decoder(mut self, decoder: Arc<dyn SamMaskDecoder>) -> Self {
        self.decoder = Some(decoder);
        self
    }

    /// The request body for one text part and the released image.
    pub(crate) fn body(&self, text: &str, image: &ReleasedImage) -> Value {
        let data = base64::engine::general_purpose::STANDARD.encode(image.bytes());
        let mut body = json!({
            "model": self.config.model,
            "stream": true,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": text},
                    {"type": "input_image", "image_url": format!("data:{};base64,{data}", image.mime_type())}
                ]
            }]
        });
        let mut metadata = serde_json::Map::new();
        if self.config.include_confidence {
            metadata.insert("include_confidence".into(), json!("true"));
        }
        if self.config.one_bit {
            metadata.insert("mask_encoding".into(), json!("one_bit"));
        }
        if !metadata.is_empty() {
            body["metadata"] = Value::Object(metadata);
        }
        body
    }

    async fn segment_jobs(
        &self,
        cx: &VisionCx,
        image: &ReleasedImage,
        jobs: Vec<Job>,
    ) -> Result<Vec<RawRegion>> {
        if !image.is_for(&self.descriptor.id) {
            return Err(VisionError::NotReleased { backend: self.descriptor.id.to_string() });
        }
        let mut regions = Vec::new();
        for job in jobs {
            cx.check()?;
            let answer = self.request(cx, &job.text, image).await?;
            regions.extend(self.regions(image, answer, job.label).await?);
        }
        Ok(regions)
    }

    async fn regions(
        &self,
        image: &ReleasedImage,
        answer: Answer,
        label: Option<String>,
    ) -> Result<Vec<RawRegion>> {
        let sent = image.image().size();
        let parse = |message: String| VisionError::Parse {
            backend: self.descriptor.id.to_string(),
            message,
        };
        for r in &answer.records {
            if r.source != sent {
                return Err(parse(format!(
                    "a record is in a {}x{} frame, but the image sent is {}x{}",
                    r.source.0, r.source.1, sent.0, sent.1
                )));
            }
        }
        let rasters = match &self.decoder {
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
        let mut out = Vec::with_capacity(answer.records.len());
        for (i, record) in answer.records.iter().enumerate() {
            let [x0, y0, x1, y1] = record.bounds;
            let shape = match rasters.as_ref().map(|r| &r[i]) {
                Some(bits) => Shape::Mask(place(bits, record, sent).map_err(parse)?),
                None => Shape::Box(BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)),
            };
            out.push(RawRegion {
                shape,
                label: label.clone().or_else(|| Some(format!("object {}", record.object))),
                score: record.confidence,
                track: None,
            });
        }
        Ok(out)
    }

    /// Sends one request, retrying rate limits, and reads its stream to the end.
    async fn request(&self, cx: &VisionCx, text: &str, image: &ReleasedImage) -> Result<Answer> {
        let backend = self.descriptor.id.to_string();
        let url = format!("{}/responses", self.config.base_url);
        let body = self.body(text, image);
        let mut attempt = 0u32;
        loop {
            cx.check()?;
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.config.api_key)
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(err) if err.is_timeout() => return Err(VisionError::Timeout { backend }),
                Err(err) => {
                    return Err(VisionError::Unavailable {
                        backend,
                        message: redact(&err.to_string()),
                    });
                }
            };
            let status = response.status();
            if status.as_u16() == 429 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .map(Duration::from_secs_f64);
                if attempt >= self.config.max_retries {
                    return Err(VisionError::RateLimited { backend, retry_after });
                }
                let wait = retry_after.unwrap_or_else(|| backoff(attempt));
                attempt += 1;
                tracing::debug!(vision.backend = %backend, retry.attempt = attempt, retry.wait_ms = wait.as_millis() as u64, "sam-3.1 rate limited; backing off");
                tokio::select! {
                    () = cx.cancellation().cancelled() => return Err(VisionError::Cancelled),
                    () = tokio::time::sleep(wait) => continue,
                }
            }
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(status_error(&backend, status.as_u16(), &text));
            }
            return self.read_stream(&backend, response).await;
        }
    }

    async fn read_stream(&self, backend: &str, response: reqwest::Response) -> Result<Answer> {
        let parse = |message: String| VisionError::Parse { backend: backend.to_string(), message };
        let mut sse = SseBuffer::default();
        let mut parser = SamStreamParser::new();
        let mut records = Vec::new();
        let mut done_text: Option<String> = None;
        let mut completed = false;
        let mut bytes = response.bytes_stream();
        let mut pending: Vec<u8> = Vec::new();
        let mut handle = |data: String,
                          records: &mut Vec<SamRecord>,
                          parser: &mut SamStreamParser|
         -> Result<()> {
            match event(&data).map_err(parse)? {
                SamEvent::Delta(delta) => records.extend(parser.push(&delta).map_err(parse)?),
                SamEvent::Done(text) => done_text = Some(text),
                SamEvent::Completed { .. } => completed = true,
                SamEvent::Failed(message) => {
                    return Err(VisionError::Backend { backend: backend.to_string(), message });
                }
                SamEvent::Other => {}
            }
            Ok(())
        };
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| VisionError::Unavailable {
                backend: backend.to_string(),
                message: redact(&e.to_string()),
            })?;
            pending.extend_from_slice(&chunk);
            // Keep an incomplete UTF-8 sequence for the next chunk.
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            for data in sse.push(&text) {
                handle(data, &mut records, &mut parser)?;
            }
        }
        for data in sse.finish() {
            handle(data, &mut records, &mut parser)?;
        }
        records.extend(parser.finish().map_err(parse)?);
        match done_text {
            // No deltas: the final text is the whole lane.
            Some(text) if parser.text().is_empty() => {
                records.extend(parser.push(&text).map_err(parse)?);
                records.extend(parser.finish().map_err(parse)?);
            }
            Some(text) if text != parser.text() => {
                return Err(parse("the final text differs from the streamed deltas".into()));
            }
            _ => {}
        }
        if !completed {
            return Err(VisionError::Unavailable {
                backend: backend.to_string(),
                message: "the stream ended before `response.completed`".into(),
            });
        }
        Ok(Answer { records })
    }
}

/// Stretches a `height × width` raster over its box inside a source-sized mask: each source
/// pixel in the box takes the raster cell under its centre.
fn place(bits: &[bool], record: &SamRecord, sent: (u32, u32)) -> std::result::Result<Mask, String> {
    let (w, h) = (record.mask.width as usize, record.mask.height as usize);
    if bits.len() != w * h {
        return Err(format!("a raster of {} bits for a {w}x{h} mask", bits.len()));
    }
    let (sw, sh) = (sent.0 as usize, sent.1 as usize);
    let [x0, y0, x1, y1] = record.bounds.map(|v| v as usize);
    if x1 > sw || y1 > sh {
        return Err(format!("box ({x0}, {y0})-({x1}, {y1}) lies outside {sw}x{sh}"));
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut full = vec![false; sw * sh];
    for y in y0..y1 {
        let ry = (((y - y0) as f64 + 0.5) * h as f64 / bh as f64) as usize;
        let row = &bits[ry.min(h - 1) * w..(ry.min(h - 1) + 1) * w];
        for x in x0..x1 {
            let rx = (((x - x0) as f64 + 0.5) * w as f64 / bw as f64) as usize;
            full[y * sw + x] = row[rx.min(w - 1)];
        }
    }
    Ok(Mask::encode(sent.0, sent.1, &full))
}

/// Exponential backoff with jitter: 0.5 s, 1 s, 2 s, ... plus up to half again.
fn backoff(attempt: u32) -> Duration {
    let base = 500u64.saturating_mul(1 << attempt.min(6));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    Duration::from_millis(base + nanos % (base / 2 + 1))
}

fn status_error(backend: &str, status: u16, body: &str) -> VisionError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect());
    let message = redact(&message);
    match status {
        400 | 413 | 422 => {
            VisionError::InvalidInput(format!("{backend} refused the request: {message}"))
        }
        401 | 403 => VisionError::Core(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Unauthorized,
            "vision.sam.unauthorized",
            format!(
                "{backend} refused the key ({status}): {message}; check META_API_KEY and that \
                 the Meta Model API account has sam-3.1"
            ),
        )),
        404 => VisionError::BackendNotFound(format!("{backend} at this base URL: {message}")),
        500..=599 => VisionError::Unavailable {
            backend: backend.to_string(),
            message: format!("{status}: {message}"),
        },
        _ => VisionError::Backend {
            backend: backend.to_string(),
            message: format!("{status}: {message}"),
        },
    }
}

/// Removes anything that looks like a bearer token from an error message.
fn redact(message: &str) -> String {
    message
        .split_whitespace()
        .map(|w| {
            if w.len() > 32 && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
                "[REDACTED]"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The object-prompt text for box and point prompts, in pixels of an image of `size`.
fn object_prompt(prompts: &[&SegmentPrompt], (w, h): (u32, u32)) -> Result<String> {
    let clamp_x = |v: f32| (v.floor().max(0.0) as u32).min(w.saturating_sub(1));
    let clamp_y = |v: f32| (v.floor().max(0.0) as u32).min(h.saturating_sub(1));
    let mut objects: Vec<String> = Vec::new();
    for prompt in prompts {
        match prompt {
            SegmentPrompt::Box { bbox } => {
                let x1 = clamp_x(bbox.x0);
                let y1 = clamp_y(bbox.y0);
                let x2 = clamp_x(bbox.x1.ceil() - 1.0).max(x1);
                let y2 = clamp_y(bbox.y1.ceil() - 1.0).max(y1);
                objects.push(format!(
                    "{}<|box;x1={x1};y1={y1};x2={x2};y2={y2};w={w};h={h}|>",
                    objects.len() + 1
                ));
            }
            SegmentPrompt::Point { at, positive } => {
                let token =
                    format!("<|point;x={};y={};w={w};h={h}|>", clamp_x(at[0]), clamp_y(at[1]));
                let token = if *positive { token } else { format!("-{token}") };
                match objects.last_mut() {
                    Some(object) => object.push_str(&token),
                    None if *positive => objects.push(format!("{}{token}", objects.len() + 1)),
                    None => {
                        return Err(VisionError::InvalidInput(
                            "a negative point needs a box or positive point before it, naming the \
                             object it is not part of"
                                .into(),
                        ));
                    }
                }
            }
            SegmentPrompt::Mask { .. } => {
                return Err(VisionError::Unsupported(
                    "sam-3.1 takes no mask prompts; give a box or points".into(),
                ));
            }
            SegmentPrompt::Concept { .. } => {}
        }
    }
    if objects.len() > MAX_OBJECTS {
        return Err(VisionError::InvalidInput(format!(
            "sam-3.1 takes at most {MAX_OBJECTS} objects per request; {} were given",
            objects.len()
        )));
    }
    Ok(format!("<0f>{}", objects.join(",")))
}

#[async_trait]
impl Segmenter<ReleasedImage> for SamSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn segment(
        &self,
        cx: &VisionCx,
        image: &ReleasedImage,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
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
            jobs.push(Job { text: object_prompt(&objects, image.image().size())?, label: None });
        }
        if jobs.is_empty() {
            return Err(VisionError::InvalidInput("segment needs at least one prompt".into()));
        }
        self.segment_jobs(cx, image, jobs).await
    }
}

#[async_trait]
impl Detector<ReleasedImage> for SamSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        cx: &VisionCx,
        image: &ReleasedImage,
        query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        let concepts = match query {
            DetectQuery::Concepts(c) | DetectQuery::Classes(c) => c,
        };
        if concepts.is_empty() {
            return Err(VisionError::InvalidInput("detect needs at least one concept".into()));
        }
        let jobs =
            concepts.iter().map(|c| Job { text: c.clone(), label: Some(c.clone()) }).collect();
        self.segment_jobs(cx, image, jobs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_prompts_follow_the_grammar() {
        let prompts = [
            SegmentPrompt::Box { bbox: BBox::new(10.0, 20.0, 40.0, 60.0) },
            SegmentPrompt::Point { at: [25.5, 30.2], positive: false },
            SegmentPrompt::Point { at: [5.0, 5.0], positive: true },
        ];
        let refs: Vec<&SegmentPrompt> = prompts.iter().collect();
        assert_eq!(
            object_prompt(&refs, (640, 480)).unwrap(),
            "<0f>1<|box;x1=10;y1=20;x2=39;y2=59;w=640;h=480|>-<|point;x=25;y=30;w=640;h=480|><|point;x=5;y=5;w=640;h=480|>"
        );
        let lonely = [SegmentPrompt::Point { at: [1.0, 1.0], positive: false }];
        assert!(object_prompt(&lonely.iter().collect::<Vec<_>>(), (10, 10)).is_err());
        let mask = [SegmentPrompt::Mask { mask: Mask::empty(2, 2) }];
        assert!(matches!(
            object_prompt(&mask.iter().collect::<Vec<_>>(), (10, 10)),
            Err(VisionError::Unsupported(_))
        ));
    }

    #[test]
    fn downscaled_rasters_are_stretched_over_their_box() {
        // A 2x1 raster over a 4x2 box: the left half set.
        let record = parse::parse_line(
            "<0f>0<|box;x1=1;y1=0;x2=4;y2=1;w=6;h=3|><|mask;x=0;y=0;data=1,2,~!|>",
        )
        .unwrap()
        .remove(0);
        let mask = place(&[true, false], &record, (6, 3)).unwrap();
        let bits = mask.decode();
        let set: Vec<usize> =
            bits.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect();
        assert_eq!(set, vec![1, 2, 7, 8]);
    }

    #[test]
    fn rasters_are_placed_at_their_box() {
        let record = parse::parse_line(
            "<0f>0<|box;x1=2;y1=1;x2=3;y2=2;w=5;h=4|><|mask;x=0;y=0;data=2,2,~!|>",
        )
        .unwrap()
        .remove(0);
        let mask = place(&[true, false, true, true], &record, (5, 4)).unwrap();
        let bits = mask.decode();
        let set: Vec<usize> =
            bits.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect();
        // (2,1), (2,2) and (3,2) in a 5-wide raster.
        assert_eq!(set, vec![7, 12, 13]);
    }

    #[test]
    fn statuses_map_to_categories() {
        let category = |status| AdkError::from(status_error("sam-3.1", status, "{}")).category;
        assert_eq!(category(400), ErrorCategory::InvalidInput);
        assert_eq!(category(401), ErrorCategory::Unauthorized);
        assert_eq!(category(503), ErrorCategory::Unavailable);
        let body = r#"{"error": {"message": "metadata[\"mask_encoding\"] must be one of ['lossless', 'one_bit']"}}"#;
        assert!(status_error("sam-3.1", 400, body).to_string().contains("one_bit"));
    }

    #[test]
    fn backoff_grows_and_keys_never_reach_messages() {
        assert!(
            backoff(0) >= Duration::from_millis(500) && backoff(0) <= Duration::from_millis(750)
        );
        assert!(backoff(3) >= Duration::from_millis(4_000));
        assert!(
            !redact("bad key sk-0123456789abcdef0123456789abcdef01")
                .contains("0123456789abcdef0123")
        );
    }
}
