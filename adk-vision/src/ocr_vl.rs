//! PaddleOCR-VL-1.6 with PP-DocLayoutV3 through `oar-ocr-vl` on Candle (spec 3.7 §7.6),
//! feature `ocr-vl`.
//!
//! PP-DocLayoutV3 finds the page's blocks and their reading order; PaddleOCR-VL-1.6 reads
//! each block's crop as text, a table (HTML), a formula (LaTeX) or a chart. Both models are
//! Apache-2.0 safetensors on Hugging Face, pinned in the built-in manifest
//! (`paddleocr-vl-1.6`, `pp-doclayoutv3`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use oar_ocr_vl::{
    LayoutElementType, LayoutPageParser, LayoutPageParserOptions, PaddleOcrVl, PpDocLayout,
    StructureResult,
};

use crate::backend::{BackendDescriptor, InputLimits, Locality};
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, RawRegion, Shape};
use crate::manifest::VerifiedModel;
use crate::reading;
use crate::sent::SentImage;
use crate::traits::{BlockKind, DocumentPage, DocumentReader, ReadOptions};

/// The built-in manifest id of PaddleOCR-VL-1.6.
pub const PADDLEOCR_VL_MANIFEST: &str = "paddleocr-vl-1.6";
/// The built-in manifest id of PP-DocLayoutV3.
pub const PP_DOCLAYOUT_MANIFEST: &str = "pp-doclayoutv3";

/// Where Candle runs the models.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VlDevice {
    /// The CPU.
    #[default]
    Cpu,
    /// The first Apple GPU, with feature `ocr-vl-metal`.
    Metal,
}

impl VlDevice {
    fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Metal => "metal",
        }
    }
}

type Parser = LayoutPageParser<PpDocLayout, PaddleOcrVl>;

/// PaddleOCR-VL-1.6 as a [`DocumentReader`]: layout blocks in the layout model's reading
/// order, each with its kind, text, layout confidence, and for tables the rows.
///
/// On-device: it implements the trait for [`SentImage`] and answers in the pixels of the
/// image it was handed. PaddleOCR-VL reads a table as HTML without cell geometry, so a
/// table block's text holds its rows (cells separated by tabs) and its `cells` stay empty;
/// the page's Markdown renders the table. Generation runs on a blocking thread, one page
/// at a time per reader.
///
/// # Example
///
/// ```no_run
/// # async fn run() -> adk_vision::Result<()> {
/// use std::sync::Arc;
/// use adk_vision::{
///     FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher, PaddleOcrVlReader, VlDevice,
/// };
///
/// let fetcher = ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let manifests = ManifestSet::builtin()?;
/// let vl = fetcher.fetch(manifests.get("paddleocr-vl-1.6").unwrap()).await?;
/// let layout = fetcher.fetch(manifests.get("pp-doclayoutv3").unwrap()).await?;
/// let reader = PaddleOcrVlReader::from_models(&vl, &layout, VlDevice::Cpu)?;
/// # let _ = reader;
/// # Ok(())
/// # }
/// ```
pub struct PaddleOcrVlReader {
    descriptor: BackendDescriptor,
    parser: Arc<Mutex<Parser>>,
}

impl std::fmt::Debug for PaddleOcrVlReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaddleOcrVlReader")
            .field("descriptor", &self.descriptor)
            .finish_non_exhaustive()
    }
}

/// The directory a verified model's top-level files sit in, after checking that the
/// loader will find no weights there the manifest does not pin.
fn model_dir(model: &VerifiedModel, required: &[&str]) -> Result<PathBuf> {
    let id = &model.manifest().id;
    let invalid = |reason: String| VisionError::Manifest { id: id.clone(), reason };
    let mut dir: Option<PathBuf> = None;
    for path in required {
        let file = model.file(path).ok_or_else(|| {
            invalid(format!("the manifest pins no `{path}`, which the loader reads"))
        })?;
        let parent = file.path().parent().map(Path::to_path_buf).unwrap_or_default();
        match &dir {
            None => dir = Some(parent),
            Some(d) if *d != parent => {
                return Err(invalid(format!("`{path}` is not beside the model's other files")));
            }
            Some(_) => {}
        }
    }
    let dir = dir.ok_or_else(|| invalid("no file to load".into()))?;
    // The loaders collect every `*.safetensors` in the directory; each must be pinned.
    let entries =
        std::fs::read_dir(&dir).map_err(|e| VisionError::io("could not read", &dir, e))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".safetensors") && model.file(&name).is_none() {
            return Err(invalid(format!(
                "`{name}` in {} is not in the manifest and would load unverified; remove it",
                dir.display()
            )));
        }
    }
    Ok(dir)
}

impl PaddleOcrVlReader {
    /// Loads PaddleOCR-VL-1.6 and PP-DocLayoutV3 from models verified against their
    /// manifests.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when a manifest does not pin a file the loader reads, or
    /// the model directory holds weights it does not pin; [`VisionError::Unsupported`] for
    /// [`VlDevice::Metal`] without feature `ocr-vl-metal`; [`VisionError::Backend`] when
    /// Candle refuses a model.
    pub fn from_models(
        vl: &VerifiedModel,
        layout: &VerifiedModel,
        device: VlDevice,
    ) -> Result<Self> {
        let vl_dir = model_dir(
            vl,
            &["config.json", "preprocessor_config.json", "tokenizer.json", "model.safetensors"],
        )?;
        let layout_dir = model_dir(layout, &["config.json", "model.safetensors"])?;
        let id = "paddleocr-vl-1.6".to_string();
        let backend = |message: String| VisionError::Backend { backend: id.clone(), message };
        let candle = oar_ocr_vl::utils::parse_device(device.name()).map_err(|e| {
            VisionError::Unsupported(format!(
                "Candle device `{}` is not available: {e}; build with feature `ocr-vl-metal` for Metal",
                device.name()
            ))
        })?;
        let layout_model = PpDocLayout::from_dir(&layout_dir, candle.clone())
            .map_err(|e| backend(format!("PP-DocLayoutV3 did not load: {e}")))?;
        let vl_model = PaddleOcrVl::from_dir(&vl_dir, candle)
            .map_err(|e| backend(format!("PaddleOCR-VL-1.6 did not load: {e}")))?;
        tracing::info!(
            vision.backend = %id,
            model.recognition = %vl.manifest().id,
            model.layout = %layout.manifest().id,
            device = device.name(),
            "paddleocr-vl loaded"
        );
        let descriptor = BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels)
            .with_manifest(vl.manifest().id.clone())
            .with_limits(InputLimits::long_edge(4096));
        Ok(Self {
            descriptor,
            parser: Arc::new(Mutex::new(LayoutPageParser::new(layout_model, vl_model))),
        })
    }

    /// Loads the built-in models through `fetcher`, verified before load.
    ///
    /// # Errors
    ///
    /// The fetcher's errors, then [`from_models`](Self::from_models)'.
    #[cfg(feature = "fetch")]
    pub async fn fetch(fetcher: &crate::ModelFetcher, device: VlDevice) -> Result<Self> {
        let manifests = crate::ManifestSet::builtin()?;
        let lookup = |id: &str| {
            manifests.get(id).cloned().ok_or_else(|| VisionError::Manifest {
                id: id.to_string(),
                reason: "not in the built-in manifest".into(),
            })
        };
        let vl = fetcher.fetch(&lookup(PADDLEOCR_VL_MANIFEST)?).await?;
        let layout = fetcher.fetch(&lookup(PP_DOCLAYOUT_MANIFEST)?).await?;
        Self::from_models(&vl, &layout, device)
    }

    /// Sets the backend id.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }
}

fn kind_of(element: LayoutElementType) -> BlockKind {
    use LayoutElementType as T;
    match element {
        T::DocTitle | T::ParagraphTitle => BlockKind::Title,
        T::Table => BlockKind::Table,
        T::Formula => BlockKind::Formula,
        T::Image | T::HeaderImage | T::FooterImage | T::Chart => BlockKind::Figure,
        T::Seal => BlockKind::Seal,
        T::Text
        | T::Content
        | T::Abstract
        | T::FigureTitle
        | T::TableTitle
        | T::ChartTitle
        | T::FigureTableChartTitle
        | T::Header
        | T::Footer
        | T::Footnote
        | T::Number
        | T::Reference
        | T::ReferenceContent
        | T::Algorithm
        | T::FormulaNumber
        | T::AsideText
        | T::List
        | T::Region
        | T::Other => BlockKind::Text,
    }
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(out.split_whitespace().collect::<Vec<_>>().join(" ").as_str())
}

/// A table's rows from PaddleOCR-VL's HTML, each cell's text with tags removed.
pub(crate) fn html_rows(html: &str) -> Vec<Vec<String>> {
    let lower = html.to_ascii_lowercase();
    let mut rows = Vec::new();
    for (start, _) in lower.match_indices("<tr") {
        let end = lower[start..].find("</tr>").map_or(lower.len(), |e| start + e);
        let row = &html[start..end];
        let row_lower = &lower[start..end];
        let mut cells = Vec::new();
        let mut at = 0;
        while let Some(open) =
            [row_lower[at..].find("<td"), row_lower[at..].find("<th")].into_iter().flatten().min()
        {
            let open = at + open;
            let Some(content) = row_lower[open..].find('>').map(|i| open + i + 1) else { break };
            let close = row_lower[content..].find("</t").map_or(row_lower.len(), |i| content + i);
            cells.push(strip_tags(&row[content..close]));
            at = close.max(content);
            if at >= row_lower.len() {
                break;
            }
            at += 1;
        }
        rows.push(cells);
    }
    rows
}

fn page_from(result: StructureResult, markdown: bool) -> DocumentPage<RawRegion> {
    let rendered = markdown.then(|| result.to_markdown());
    let mut blocks = Vec::new();
    let mut orders: Vec<(Option<u32>, usize)> = Vec::new();
    for element in result.layout_elements {
        let Some(bbox) = BBox::covering(element.bbox.points.iter().map(|p| [p.x, p.y])) else {
            continue;
        };
        let kind = kind_of(element.element_type);
        let label =
            element.label.clone().unwrap_or_else(|| element.element_type.as_str().to_string());
        let raw = element.text.unwrap_or_default();
        let text = match kind {
            BlockKind::Table => {
                html_rows(&raw).iter().map(|row| row.join("\t")).collect::<Vec<_>>().join("\n")
            }
            _ => raw.trim().to_string(),
        };
        let region = RawRegion {
            shape: Shape::Box(bbox),
            label: Some(label),
            score: Some(element.confidence),
            track: None,
        };
        orders.push((element.order_index, blocks.len()));
        blocks.push(reading::block(kind, region, text, Some(element.confidence)));
    }
    // The layout model's order where it gives one; its listing order otherwise.
    orders.sort_by_key(|(order, i)| (order.is_none(), order.unwrap_or(0), *i));
    let reading_order = orders.into_iter().map(|(_, i)| i).collect();
    DocumentPage { blocks, reading_order, markdown: rendered }
}

#[async_trait]
impl DocumentReader for PaddleOcrVlReader {
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
        // Too thin to hold a block; the layout model would only reject it.
        if w < 16 || h < 16 {
            return Ok(DocumentPage::default());
        }
        let rgb = image::DynamicImage::ImageRgba8(page.pixels().inner().clone()).to_rgb8();
        let parser = Arc::clone(&self.parser);
        let backend = self.descriptor.id.to_string();
        let markdown = options.markdown;
        let failed = |message: String| VisionError::Backend { backend: backend.clone(), message };
        let result = tokio::task::spawn_blocking(move || {
            let parser = parser.lock().unwrap_or_else(|e| e.into_inner());
            parser.parse_structure(&rgb, &LayoutPageParserOptions::default())
        })
        .await
        .map_err(|e| failed(format!("the parsing task failed: {e}")))?
        .map_err(|e| failed(e.to_string()))?;
        Ok(page_from(result, markdown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_tables_become_rows() {
        let html = "<table><tr><td>Part</td><td>Length (mm)</td></tr>\
                    <tr><td><b>P-017</b></td><td>4.2</td></tr><tr><th>A &amp; B</th></tr></table>";
        assert_eq!(
            html_rows(html),
            vec![
                vec!["Part".to_string(), "Length (mm)".to_string()],
                vec!["P-017".to_string(), "4.2".to_string()],
                vec!["A & B".to_string()],
            ]
        );
    }
}
