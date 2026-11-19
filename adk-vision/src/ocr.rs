//! PP-OCRv6 text lines and words through `oar-ocr` on ONNX Runtime (spec 3.7 §7.6),
//! feature `ocr`.
//!
//! The models are PaddlePaddle's own ONNX exports on Hugging Face, pinned in the built-in
//! manifest (`pp-ocrv6-{tiny,small,medium}-{det,rec}`); the files are byte-identical to the
//! ones `oar-ocr`'s ModelScope registry lists. The recognition model's character
//! dictionary is read from the `inference.yml` beside it, so one verified source serves
//! both. `oar-ocr`'s own `auto-download` stays off.

use std::sync::Arc;

use async_trait::async_trait;
use oar_ocr::domain::tasks::text_detection::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCR, OAROCRBuilder};
use serde::Deserialize;

use crate::backend::{BackendDescriptor, InputLimits, Locality};
use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Polygon, RawRegion, Shape};
use crate::manifest::VerifiedModel;
use crate::reading;
use crate::sent::SentImage;
use crate::traits::{BlockKind, DocumentPage, DocumentReader, ReadOptions, Word};

/// A PP-OCRv6 model size.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PpOcrV6Size {
    /// The fastest pair, over a 6,904-entry dictionary.
    Tiny,
    /// The balanced pair, over the full 18,708-entry dictionary.
    Small,
    /// The most accurate pair, over the full dictionary.
    Medium,
}

impl PpOcrV6Size {
    fn name(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
        }
    }

    /// The built-in manifest id of the detection model, such as `pp-ocrv6-tiny-det`.
    pub fn detection_manifest(self) -> String {
        format!("pp-ocrv6-{}-det", self.name())
    }

    /// The built-in manifest id of the recognition model, such as `pp-ocrv6-tiny-rec`.
    pub fn recognition_manifest(self) -> String {
        format!("pp-ocrv6-{}-rec", self.name())
    }

    /// PaddleOCR's published detection defaults for PP-OCRv6: `thresh` 0.2, `box_thresh`
    /// 0.45 (0.4 for tiny), `unclip_ratio` 1.4, up to 3,000 candidates.
    fn detection_config(self) -> TextDetectionConfig {
        TextDetectionConfig {
            score_threshold: 0.2,
            box_threshold: if self == Self::Tiny { 0.4 } else { 0.45 },
            unclip_ratio: 1.4,
            max_candidates: 3000,
            ..TextDetectionConfig::default()
        }
    }
}

/// The ONNX file of a PP-OCRv6 model in its manifest.
const ONNX_FILE: &str = "inference.onnx";
/// PaddlePaddle's pipeline description, which holds the recognition dictionary.
const PIPELINE_FILE: &str = "inference.yml";

#[derive(Deserialize)]
struct Pipeline {
    #[serde(rename = "PostProcess")]
    post_process: PostProcess,
}

#[derive(Deserialize)]
struct PostProcess {
    character_dict: Vec<String>,
}

/// Reads the character dictionary from a PP-OCR `inference.yml`.
pub(crate) fn dictionary(yaml: &str) -> Result<Vec<String>> {
    let pipeline: Pipeline = serde_saphyr::from_str(yaml).map_err(|e| VisionError::Parse {
        backend: "pp-ocrv6".into(),
        message: format!("the recognition model's inference.yml has no character dictionary: {e}"),
    })?;
    if pipeline.post_process.character_dict.is_empty() {
        return Err(VisionError::Parse {
            backend: "pp-ocrv6".into(),
            message: "the recognition model's character dictionary is empty".into(),
        });
    }
    Ok(pipeline.post_process.character_dict)
}

/// Reads a verified file and checks the bytes in hand against its digest, so what loads is
/// what was verified (V-I10).
fn verified_bytes(model: &VerifiedModel, path: &str) -> Result<Vec<u8>> {
    let id = &model.manifest().id;
    let file = model.file(path).ok_or_else(|| VisionError::Manifest {
        id: id.clone(),
        reason: format!("the manifest pins no `{path}`"),
    })?;
    let bytes = std::fs::read(file.path())
        .map_err(|e| VisionError::io("could not read", file.path(), e))?;
    let actual = Digest::of(&bytes);
    if actual != file.digest() {
        return Err(VisionError::DigestMismatch {
            path: file.path().to_path_buf(),
            expected: file.digest(),
            actual,
        });
    }
    Ok(bytes)
}

/// PP-OCRv6 as a [`DocumentReader`]: one block per text line, in reading order, with its
/// quad, recognition confidence and words.
///
/// On-device: it implements the trait for [`SentImage`] and answers in the pixels of the
/// image it was handed (`CoordFrame::SentPixels`). Words are the recogniser's character
/// boxes merged at spaces, so their horizontal extent is the CTC alignment's estimate.
/// Layout, tables and fields come from `PaddleOcrVlReader` (feature `ocr-vl`) or a
/// cloud reader.
///
/// # Example
///
/// ```no_run
/// # async fn run() -> adk_vision::Result<()> {
/// use std::sync::Arc;
/// use adk_vision::{
///     FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher, PpOcrReader,
///     PpOcrV6Size, ReadOptions, SourceImage, DecodeLimits, VisionCx, VisionRuntime,
/// };
///
/// let fetcher = ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let manifests = ManifestSet::builtin()?;
/// let size = PpOcrV6Size::Tiny;
/// let det = fetcher.fetch(manifests.get(&size.detection_manifest()).unwrap()).await?;
/// let rec = fetcher.fetch(manifests.get(&size.recognition_manifest()).unwrap()).await?;
/// let reader = PpOcrReader::from_models(size, &det, &rec)?;
///
/// let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build()?;
/// let page = SourceImage::decode(&std::fs::read("invoice.png").unwrap(), &DecodeLimits::default())?;
/// let read = runtime.read(&VisionCx::new(), &page, &ReadOptions::default()).await?;
/// for i in &read.value.reading_order {
///     println!("{}", read.value.blocks[*i].text);
/// }
/// # Ok(())
/// # }
/// ```
pub struct PpOcrReader {
    descriptor: BackendDescriptor,
    engine: Arc<OAROCR>,
}

impl std::fmt::Debug for PpOcrReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PpOcrReader").field("descriptor", &self.descriptor).finish_non_exhaustive()
    }
}

impl PpOcrReader {
    /// Loads a detection and a recognition model of `size`, each verified against its
    /// manifest. The ONNX bytes are hashed again as they are read, and the dictionary
    /// comes from the recognition model's `inference.yml`.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when a manifest pins no `inference.onnx` (or the
    /// recognition model no `inference.yml`), [`VisionError::DigestMismatch`] when a file
    /// changed since it was verified, [`VisionError::Parse`] for a dictionary that does not
    /// parse, and [`VisionError::Backend`] when ONNX Runtime refuses a model.
    pub fn from_models(
        size: PpOcrV6Size,
        detection: &VerifiedModel,
        recognition: &VerifiedModel,
    ) -> Result<Self> {
        let det = verified_bytes(detection, ONNX_FILE)?;
        let rec = verified_bytes(recognition, ONNX_FILE)?;
        let yaml = verified_bytes(recognition, PIPELINE_FILE)?;
        let yaml = String::from_utf8(yaml).map_err(|e| VisionError::Parse {
            backend: "pp-ocrv6".into(),
            message: format!("inference.yml is not UTF-8: {e}"),
        })?;
        let dict = dictionary(&yaml)?;
        let id = format!("pp-ocrv6-{}", size.name());
        let engine = OAROCRBuilder::new(det, rec, "")
            .character_dict_content(dict.join("\n"))
            .text_detection_config(size.detection_config())
            .return_word_box(true)
            .build()
            .map_err(|e| VisionError::Backend {
                backend: id.clone(),
                message: format!("ONNX Runtime could not load PP-OCRv6 {}: {e}", size.name()),
            })?;
        tracing::info!(
            vision.backend = %id,
            model.detection = %detection.manifest().id,
            model.recognition = %recognition.manifest().id,
            model.dictionary = dict.len(),
            "pp-ocrv6 loaded"
        );
        let descriptor = BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels)
            .with_manifest(recognition.manifest().id.clone())
            // Detection resizes internally; a page past 4,096 px is downscaled first so
            // its pixels fit the decode budget.
            .with_limits(InputLimits::long_edge(4096));
        Ok(Self { descriptor, engine: Arc::new(engine) })
    }

    /// Loads the built-in `size` models through `fetcher`, verified before load.
    ///
    /// # Errors
    ///
    /// The fetcher's errors, then [`from_models`](Self::from_models)'.
    #[cfg(feature = "fetch")]
    pub async fn fetch(fetcher: &crate::ModelFetcher, size: PpOcrV6Size) -> Result<Self> {
        let manifests = crate::ManifestSet::builtin()?;
        let lookup = |id: String| {
            manifests.get(&id).cloned().ok_or_else(|| VisionError::Manifest {
                id,
                reason: "not in the built-in manifest".into(),
            })
        };
        let det = fetcher.fetch(&lookup(size.detection_manifest())?).await?;
        let rec = fetcher.fetch(&lookup(size.recognition_manifest())?).await?;
        Self::from_models(size, &det, &rec)
    }

    /// Sets the backend id.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }
}

/// A line's words: its character boxes merged between spaces.
fn words(text: &str, chars: &[BBox], confidence: Option<f32>) -> Vec<Word<RawRegion>> {
    let letters: Vec<char> = text.chars().collect();
    if letters.len() != chars.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current: Option<(String, BBox)> = None;
    for (c, b) in letters.iter().zip(chars) {
        if c.is_whitespace() {
            out.extend(current.take());
            continue;
        }
        current = Some(match current.take() {
            Some((mut word, bbox)) => {
                word.push(*c);
                (
                    word,
                    BBox::new(
                        bbox.x0.min(b.x0),
                        bbox.y0.min(b.y0),
                        bbox.x1.max(b.x1),
                        bbox.y1.max(b.y1),
                    ),
                )
            }
            None => (c.to_string(), *b),
        });
    }
    out.extend(current);
    out.into_iter()
        .map(|(text, bbox)| Word {
            region: RawRegion {
                shape: Shape::Box(bbox),
                label: None,
                score: confidence,
                track: None,
            },
            text,
            confidence,
        })
        .collect()
}

#[async_trait]
impl DocumentReader for PpOcrReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        cx: &VisionCx,
        page: &SentImage,
        options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        cx.check()?;
        let (w, h) = page.size();
        // Too thin to hold a line of text; detection would only reject it.
        if w < 8 || h < 8 {
            return Ok(DocumentPage::default());
        }
        let rgb = image::DynamicImage::ImageRgba8(page.pixels().inner().clone()).to_rgb8();
        let engine = Arc::clone(&self.engine);
        let backend = self.descriptor.id.to_string();
        let results = tokio::task::spawn_blocking(move || engine.predict(vec![rgb]))
            .await
            .map_err(|e| VisionError::Backend {
                backend: backend.clone(),
                message: format!("the OCR task failed: {e}"),
            })?
            .map_err(|e| VisionError::Backend {
                backend: backend.clone(),
                message: e.to_string(),
            })?;
        let Some(result) = results.into_iter().next() else {
            return Ok(DocumentPage::default());
        };
        let mut blocks = Vec::new();
        let mut boxes = Vec::new();
        for region in result.text_regions {
            let Some(text) = region.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
                continue;
            };
            let points: Vec<[f32; 2]> =
                region.bounding_box.points.iter().map(|p| [p.x, p.y]).collect();
            let Some(bbox) = BBox::covering(points.iter().copied()) else { continue };
            let chars: Vec<BBox> = region
                .word_boxes
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|b| BBox::covering(b.points.iter().map(|p| [p.x, p.y])))
                .collect();
            let full = region.text.as_deref().unwrap_or_default();
            let mut block = reading::block(
                BlockKind::Text,
                RawRegion {
                    shape: Shape::Polygon(Polygon { points }),
                    label: None,
                    score: region.confidence,
                    track: None,
                },
                text.to_string(),
                region.confidence,
            );
            block.words = words(full, &chars, region.confidence);
            boxes.push(bbox);
            blocks.push(block);
        }
        let reading_order = reading::reading_order(&boxes);
        let mut out = DocumentPage { blocks, reading_order, markdown: None };
        if options.markdown {
            out.markdown = Some(reading::markdown(&out));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dictionary_comes_from_the_pipeline_yaml() {
        let yaml = "Global:\n  model_name: PP-OCRv6_tiny_rec\nHpi:\n  a: &id001\n  - 1\n  b: *id001\nPostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - '!'\n  - '\"'\n  - ''''\n  - y\n  - n\n  - '0'\n  - ～\n";
        assert_eq!(dictionary(yaml).unwrap(), vec!["!", "\"", "'", "y", "n", "0", "～"]);
    }

    #[test]
    fn a_pipeline_without_a_dictionary_is_refused() {
        assert!(matches!(dictionary("Global: {}\n"), Err(VisionError::Parse { .. })));
    }

    #[test]
    fn character_boxes_merge_into_words_at_spaces() {
        let chars: Vec<BBox> =
            (0..7).map(|i| BBox::new(i as f32 * 10.0, 0.0, i as f32 * 10.0 + 10.0, 20.0)).collect();
        let found = words("ab cd e", &chars, Some(0.9));
        let texts: Vec<(&str, Option<BBox>)> =
            found.iter().map(|w| (w.text.as_str(), w.region.shape.bbox())).collect();
        assert_eq!(
            texts,
            vec![
                ("ab", Some(BBox::new(0.0, 0.0, 20.0, 20.0))),
                ("cd", Some(BBox::new(30.0, 0.0, 50.0, 20.0))),
                ("e", Some(BBox::new(60.0, 0.0, 70.0, 20.0))),
            ]
        );
        // A count mismatch gives no words rather than misplaced ones.
        assert!(words("abc", &chars, None).is_empty());
    }
}
