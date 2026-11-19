//! Scripted backends for the runtime and conformance tests.
//!
//! The colour detectors are real, deterministic detectors: they find solid-colour boxes by
//! scanning the pixels they are handed, so every resize, crop and frame conversion the
//! runtime applies is exercised end to end.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{UnitRate, UsageUnit};
use adk_vision::{
    AxisOrder, BBox, BackendDescriptor, Block, BlockKind, CoordFrame, DecodedImage,
    DescribeRequest, Describer, Description, DetectQuery, Detector, DocumentPage, DocumentReader,
    Embedder, EmbeddingSpace, ImageRegion, InputLimits, Letterbox, Locality, Mask, Metric,
    PriceRef, RawRegion, ReadOptions, RedactionClass, Redactor, ReleasedImage, Result,
    SegmentPrompt, Segmenter, SentImage, Shape, SourceImage, VisionCx, VisionError, VisionInput,
    Word,
};

/// The colours the fixtures use, by label.
pub fn palette() -> BTreeMap<&'static str, [u8; 3]> {
    [("red", [255, 0, 0]), ("blue", [0, 0, 255]), ("green", [0, 160, 0])].into()
}

/// A white image with solid boxes of the named colours.
pub fn boxes_image(width: u32, height: u32, boxes: &[(&str, BBox)]) -> SourceImage {
    let colours = palette();
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    for (label, b) in boxes {
        let c = colours[label];
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = (y as usize * width as usize + x as usize) * 4;
                rgba[i..i + 3].copy_from_slice(&c);
            }
        }
    }
    SourceImage::from_pixels(DecodedImage::from_rgba(width, height, rgba).unwrap()).unwrap()
}

/// The bounding box of the pixels close to `colour`, in the image's pixels.
pub fn find_colour(image: &DecodedImage, colour: [u8; 3]) -> Option<BBox> {
    let (w, h) = image.size();
    let rgba = image.rgba();
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize * w as usize + x as usize) * 4;
            let close = (0..3).all(|c| (i32::from(rgba[i + c]) - i32::from(colour[c])).abs() < 64);
            if close {
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
    }
    bounds.map(|(x0, y0, x1, y1)| BBox::new(x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32))
}

fn labels(query: &DetectQuery) -> Vec<String> {
    match query {
        DetectQuery::Classes(v) | DetectQuery::Concepts(v) => v.clone(),
        _ => Vec::new(),
    }
}

/// Finds the queried colours, scoring each label as configured (0.9 by default), and
/// answers in its descriptor's frame.
pub struct ColourDetector {
    pub descriptor: BackendDescriptor,
    pub scores: BTreeMap<String, f32>,
    pub calls: AtomicUsize,
    pub delay: Option<Duration>,
    pub rate_limited: bool,
    pub only: Option<Vec<String>>,
}

impl ColourDetector {
    pub fn local(id: &str) -> Self {
        Self::with(BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels))
    }

    pub fn remote(id: &str) -> Self {
        let rate = UnitRate::new(UsageUnit::Images, 2_500_000, 1_000);
        Self::with(
            BackendDescriptor::new(
                id,
                Locality::Remote { hosts: vec!["vision.example".into()] },
                CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx },
            )
            .with_price(PriceRef::PerUnit {
                vendor: "meta".into(),
                model: "sam-3.1".into(),
                rate,
                min_units_per_request: 0,
            })
            .with_limits(InputLimits::long_edge(1568)),
        )
    }

    pub fn with(descriptor: BackendDescriptor) -> Self {
        Self {
            descriptor,
            scores: BTreeMap::new(),
            calls: AtomicUsize::new(0),
            delay: None,
            rate_limited: false,
            only: None,
        }
    }

    pub fn scoring(mut self, label: &str, score: f32) -> Self {
        self.scores.insert(label.into(), score);
        self
    }

    pub fn slow(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn rate_limited(mut self) -> Self {
        self.rate_limited = true;
        self
    }

    /// Finds only these labels, whatever the query asks.
    pub fn seeing(mut self, labels: &[&str]) -> Self {
        self.only = Some(labels.iter().map(|l| l.to_string()).collect());
        self
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn find(&self, image: &SentImage, query: &DetectQuery) -> Result<Vec<RawRegion>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.rate_limited {
            return Err(VisionError::RateLimited {
                backend: self.descriptor.id.to_string(),
                retry_after: Some(Duration::from_secs(2)),
            });
        }
        let colours = palette();
        let sent = image.size();
        let mut out = Vec::new();
        for label in labels(query) {
            if self.only.as_ref().is_some_and(|only| !only.contains(&label)) {
                continue;
            }
            let Some(colour) = colours.get(label.as_str()) else { continue };
            let Some(found) = find_colour(image.pixels(), *colour) else { continue };
            let a = self
                .descriptor
                .frame
                .point_from_sent([f64::from(found.x0), f64::from(found.y0)], sent);
            let b = self
                .descriptor
                .frame
                .point_from_sent([f64::from(found.x1), f64::from(found.y1)], sent);
            let in_frame = BBox::new(a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32);
            let score = self.scores.get(&label).copied().unwrap_or(0.9);
            out.push(RawRegion::boxed(in_frame, label, score));
        }
        Ok(out)
    }

    async fn pause(&self) {
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
    }
}

#[async_trait::async_trait]
impl Detector for ColourDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        self.pause().await;
        self.find(image, query)
    }
}

#[async_trait::async_trait]
impl Detector<ReleasedImage> for ColourDetector {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        _cx: &VisionCx,
        image: &ReleasedImage,
        query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        self.pause().await;
        assert_eq!(adk_vision::Digest::of(image.bytes()), image.receipt().egress);
        self.find(image.sent(), query)
    }
}

/// Segments the colour named by each concept prompt into a mask in its model-input frame,
/// and records the prompts it was handed.
pub struct ColourSegmenter {
    pub descriptor: BackendDescriptor,
    pub seen: Mutex<Vec<SegmentPrompt>>,
}

impl ColourSegmenter {
    pub fn local() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "colour-seg",
                Locality::InProcess,
                CoordFrame::ModelInput { width: 320, height: 320, letterbox: Letterbox::Center },
            ),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl Segmenter for ColourSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn segment(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
        self.seen.lock().unwrap().extend(prompts.iter().cloned());
        let CoordFrame::ModelInput { width, height, .. } = self.descriptor.frame else {
            unreachable!()
        };
        let colours = palette();
        let sent = image.size();
        let mut out = Vec::new();
        for prompt in prompts {
            let SegmentPrompt::Concept { text } = prompt else { continue };
            let Some(colour) = colours.get(text.as_str()) else { continue };
            let Some(found) = find_colour(image.pixels(), *colour) else { continue };
            let a = self
                .descriptor
                .frame
                .point_from_sent([f64::from(found.x0), f64::from(found.y0)], sent);
            let b = self
                .descriptor
                .frame
                .point_from_sent([f64::from(found.x1), f64::from(found.y1)], sent);
            let in_model = BBox::new(a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32);
            let mut raw = RawRegion::shape(Shape::Mask(Mask::from_box(width, height, &in_model)));
            raw.label = Some(text.clone());
            raw.score = Some(0.9);
            out.push(raw);
        }
        Ok(out)
    }
}

/// Reads each coloured box as a block of text naming its colour; blue blocks score low.
pub struct ColourReader {
    pub descriptor: BackendDescriptor,
    pub low: f32,
}

impl ColourReader {
    pub fn local(id: &str, low: f32) -> Self {
        Self {
            descriptor: BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels),
            low,
        }
    }

    fn page(&self, image: &SentImage) -> DocumentPage<RawRegion> {
        let mut blocks = Vec::new();
        for (label, colour) in palette() {
            if let Some(found) = find_colour(image.pixels(), colour) {
                let confidence = if label == "blue" { self.low } else { 0.95 };
                blocks.push(Block {
                    kind: BlockKind::Text,
                    region: RawRegion::shape(Shape::Box(found)),
                    text: format!("{label} text from {}", self.descriptor.id),
                    confidence: Some(confidence),
                    words: vec![Word {
                        region: RawRegion::shape(Shape::Box(found)),
                        text: label.to_string(),
                        confidence: Some(confidence),
                    }],
                    cells: Vec::new(),
                });
            }
        }
        let reading_order = (0..blocks.len()).collect();
        DocumentPage { blocks, reading_order, markdown: None }
    }
}

#[async_trait::async_trait]
impl DocumentReader for ColourReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &SentImage,
        _options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        Ok(self.page(page))
    }
}

/// A remote reader in sent pixels that reads every box with full confidence.
pub struct RemoteReader {
    pub inner: ColourReader,
}

impl RemoteReader {
    pub fn new() -> Self {
        let mut inner = ColourReader::local("cloud-reader", 0.99);
        inner.descriptor.locality = Locality::Remote { hosts: vec!["ocr.example".into()] };
        Self { inner }
    }
}

#[async_trait::async_trait]
impl DocumentReader<ReleasedImage> for RemoteReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.inner.descriptor
    }

    async fn read(
        &self,
        _cx: &VisionCx,
        page: &ReleasedImage,
        _options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        Ok(self.inner.page(page.sent()))
    }
}

/// Embeds an image as its mean colour and a text as its length, in a 3-dimensional space.
pub struct MeanEmbedder {
    pub descriptor: BackendDescriptor,
    pub space: EmbeddingSpace,
    pub dims: usize,
}

impl MeanEmbedder {
    pub fn local() -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                "mean-embedder",
                Locality::InProcess,
                CoordFrame::SentPixels,
            ),
            space: EmbeddingSpace {
                model: "mean".into(),
                revision: "1".into(),
                dimensions: 3,
                metric: Metric::Cosine,
            },
            dims: 3,
        }
    }
}

#[async_trait::async_trait]
impl Embedder for MeanEmbedder {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_images(&self, _cx: &VisionCx, images: &[SentImage]) -> Result<Vec<Vec<f32>>> {
        Ok(images
            .iter()
            .map(|image| {
                let rgba = image.pixels().rgba();
                let n = (rgba.len() / 4) as f32;
                let mut v = vec![0f32; self.dims];
                for px in rgba.chunks(4) {
                    for c in 0..3.min(self.dims) {
                        v[c] += f32::from(px[c]) / n;
                    }
                }
                v
            })
            .collect())
    }

    async fn embed_texts(&self, _cx: &VisionCx, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| vec![t.len() as f32; self.dims]).collect())
    }
}

/// A remote describer in Gemini's frame that names the colours it sees, with a fixed
/// confidence, and returns their boxes when asked.
pub struct ColourDescriber {
    pub descriptor: BackendDescriptor,
    pub confidence: f32,
}

impl ColourDescriber {
    pub fn remote(id: &str, confidence: f32) -> Self {
        Self {
            descriptor: BackendDescriptor::new(
                id,
                Locality::Remote { hosts: vec!["generativelanguage.example".into()] },
                CoordFrame::Normalized { scale: 1000, order: AxisOrder::Yx },
            )
            .with_price(PriceRef::Tokens {
                vendor: "google".into(),
                model: "gemini-3.8-flash".into(),
            }),
            confidence,
        }
    }

    pub fn local(id: &str, confidence: f32) -> Self {
        let mut d = Self::remote(id, confidence);
        d.descriptor.locality = Locality::InProcess;
        d.descriptor.price = None;
        d
    }

    fn answer<I: VisionInput>(&self, request: &DescribeRequest<I>) -> Description<ImageRegion> {
        let mut names = Vec::new();
        let mut regions = Vec::new();
        for (index, image) in request.images.iter().enumerate() {
            let sent = image.sent();
            for (label, colour) in palette() {
                if let Some(found) = find_colour(sent.pixels(), colour) {
                    names.push(label);
                    if request.want_regions {
                        let f = self.descriptor.frame;
                        let a = f.point_from_sent(
                            [f64::from(found.x0), f64::from(found.y0)],
                            sent.size(),
                        );
                        let b = f.point_from_sent(
                            [f64::from(found.x1), f64::from(found.y1)],
                            sent.size(),
                        );
                        let raw = RawRegion::boxed(
                            BBox::new(a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32),
                            label,
                            0.8,
                        );
                        regions.push(ImageRegion { image: index, region: raw });
                    }
                }
            }
        }
        Description {
            text: Some(format!("{} says: {}", self.descriptor.id, names.join(", "))),
            value: Some(serde_json::json!({ "colours": names })),
            regions,
            confidence: Some(self.confidence),
            escalated_from: None,
        }
    }
}

#[async_trait::async_trait]
impl Describer<ReleasedImage> for ColourDescriber {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        _cx: &VisionCx,
        request: &DescribeRequest<ReleasedImage>,
    ) -> Result<Description<ImageRegion>> {
        Ok(self.answer(request))
    }
}

#[async_trait::async_trait]
impl Describer for ColourDescriber {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn describe(
        &self,
        _cx: &VisionCx,
        request: &DescribeRequest<SentImage>,
    ) -> Result<Description<ImageRegion>> {
        Ok(self.answer(request))
    }
}

/// A redactor that finds one colour's box, as a face detector would find faces.
pub struct ColourRedactor {
    pub class: RedactionClass,
    pub colour: [u8; 3],
}

#[async_trait::async_trait]
impl Redactor for ColourRedactor {
    fn class(&self) -> RedactionClass {
        self.class.clone()
    }

    async fn find(&self, _cx: &VisionCx, image: &SentImage) -> Result<Vec<BBox>> {
        Ok(find_colour(image.pixels(), self.colour).into_iter().collect())
    }
}

pub fn arc<T>(value: T) -> Arc<T> {
    Arc::new(value)
}
