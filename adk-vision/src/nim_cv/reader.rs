//! [`NimDocumentReader`]: a page read by NVIDIA's OCR, page-element, table-structure and
//! chart-element endpoints, composed into one [`DocumentPage`].

use async_trait::async_trait;
use base64::Engine;

use super::client::CvClient;
use super::config::NimCvConfig;
use super::wire::{self, NormBox, NormPoint, TextDetection};
use crate::backend::{BackendDescriptor, InputLimits, Locality};
use crate::cx::VisionCx;
use crate::edit::ops;
use crate::egress::ReleasedImage;
use crate::error::{Result, VisionError};
use crate::geometry::{AxisOrder, BBox, CoordFrame, Polygon, RawRegion, Shape};
use crate::reading::{self, Piece};
use crate::traits::{Block, BlockKind, DocumentPage, DocumentReader, ReadOptions, TableCell};

/// The page-element model.
pub const PAGE_ELEMENTS_MODEL: &str = "nvidia/nemotron-page-elements-v3";
/// The table-structure model, called on each table's crop.
pub const TABLE_STRUCTURE_MODEL: &str = "nvidia/nemotron-table-structure-v1";
/// The chart-element model, called on each chart's crop.
pub const GRAPHIC_ELEMENTS_MODEL: &str = "nvidia/nemotron-graphic-elements-v1";

/// The OCR model a [`NimDocumentReader`] reads text with.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NimOcrModel {
    /// `nvidia/nemotron-ocr-v1`, asked for words, which the reader groups into lines.
    NemotronOcrV1,
    /// `nvidia/nemoretriever-ocr`, the same model under its retriever name.
    NemoretrieverOcr,
    /// `baidu/paddleocr`, which answers in lines.
    PaddleOcr,
}

impl NimOcrModel {
    /// The model's `org/name`.
    pub fn id(self) -> &'static str {
        match self {
            Self::NemotronOcrV1 => "nvidia/nemotron-ocr-v1",
            Self::NemoretrieverOcr => "nvidia/nemoretriever-ocr",
            Self::PaddleOcr => "baidu/paddleocr",
        }
    }

    fn merge_levels(self) -> Option<&'static [&'static str]> {
        match self {
            Self::NemotronOcrV1 | Self::NemoretrieverOcr => Some(&["word"]),
            Self::PaddleOcr => None,
        }
    }
}

/// A [`DocumentReader`] over NVIDIA's CV endpoints: OCR for text, page elements for blocks,
/// table structure for cells and chart elements for chart labels.
///
/// Remote: it implements the trait for [`ReleasedImage`] only, so a page reaches it only
/// through the egress gate. It sends the released PNG inline; table and chart crops are cut
/// from the released pixels, so nothing outside the released image leaves. Answers are in
/// 0–1 of the image sent (`CoordFrame::Normalized { scale: 1, order: Xy }`), and the runtime
/// maps them to source pixels. NVIDIA publishes no per-call price, so the descriptor
/// carries none and calls are capped by units (images and pages).
///
/// | Step | Endpoint | Calls per page |
/// |------|----------|----------------|
/// | Text | the [`NimOcrModel`] | 1 |
/// | Blocks | [`PAGE_ELEMENTS_MODEL`] | 1, when layout is on |
/// | Table cells | [`TABLE_STRUCTURE_MODEL`] | 1 per table, when tables are on |
/// | Chart labels | [`GRAPHIC_ELEMENTS_MODEL`] | 1 per chart, when charts are on |
///
/// # Example
///
/// ```
/// use adk_vision::{CoordFrame, NimCvConfig, NimDocumentReader, NimOcrModel, DocumentReader, ReleasedImage};
///
/// let reader = NimDocumentReader::new(
///     NimCvConfig::new("https://ai.api.nvidia.com/v1/cv").with_api_key_from_env(),
/// )
/// .unwrap()
/// .with_ocr(NimOcrModel::NemotronOcrV1)
/// .with_charts(false);
/// let descriptor = DocumentReader::<ReleasedImage>::descriptor(&reader);
/// assert!(descriptor.locality.is_remote());
/// assert!(descriptor.price.is_none());
/// assert!(matches!(descriptor.frame, CoordFrame::Normalized { scale: 1, .. }));
/// ```
#[derive(Debug, Clone)]
pub struct NimDocumentReader {
    descriptor: BackendDescriptor,
    client: CvClient,
    ocr: NimOcrModel,
    layout: bool,
    tables: bool,
    charts: bool,
}

/// The default backend id.
pub const NIM_READER_ID: &str = "nim-document-reader";

impl NimDocumentReader {
    /// A reader with `nemotron-ocr-v1`, page elements, table structure and chart elements.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when a configured URL does not parse or names no host,
    /// or the HTTP client cannot start.
    pub fn new(config: NimCvConfig) -> Result<Self> {
        let client = CvClient::new(config)?;
        let mut reader = Self {
            descriptor: BackendDescriptor::new(
                NIM_READER_ID,
                Locality::Remote { hosts: Vec::new() },
                CoordFrame::Normalized { scale: 1, order: AxisOrder::Xy },
            )
            // The CV endpoints answered 1920×1080 inline in the probe; larger pages are
            // downscaled and the resize recorded.
            .with_limits(InputLimits::long_edge(2048)),
            client,
            ocr: NimOcrModel::NemotronOcrV1,
            layout: true,
            tables: true,
            charts: true,
        };
        // Every model the reader may call is checked now, so later builder calls cannot fail.
        for model in [
            NimOcrModel::NemotronOcrV1.id(),
            NimOcrModel::NemoretrieverOcr.id(),
            NimOcrModel::PaddleOcr.id(),
            PAGE_ELEMENTS_MODEL,
            TABLE_STRUCTURE_MODEL,
            GRAPHIC_ELEMENTS_MODEL,
        ] {
            let url = reader.client.config().url_for(model);
            let parsed = reqwest::Url::parse(&url).map_err(|e| {
                VisionError::InvalidInput(format!("NIM URL `{url}` for `{model}` is invalid: {e}"))
            })?;
            if parsed.host_str().is_none_or(str::is_empty) {
                return Err(VisionError::InvalidInput(format!(
                    "NIM URL `{url}` for `{model}` names no host"
                )));
            }
        }
        reader.refresh_hosts();
        Ok(reader)
    }

    /// Sets the backend id the runtime registers it under.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.descriptor.id = crate::backend::BackendId::new(id);
        self
    }

    /// Sets the input limits.
    pub fn with_limits(mut self, limits: InputLimits) -> Self {
        self.descriptor.limits = limits;
        self
    }

    /// Reads text with `model`.
    pub fn with_ocr(mut self, model: NimOcrModel) -> Self {
        self.ocr = model;
        self.refresh_hosts();
        self
    }

    /// Detects blocks with the page-element model; off, every line is its own block.
    pub fn with_layout(mut self, on: bool) -> Self {
        self.layout = on;
        self.refresh_hosts();
        self
    }

    /// Reads table cells from each table's crop.
    pub fn with_tables(mut self, on: bool) -> Self {
        self.tables = on;
        self.refresh_hosts();
        self
    }

    /// Labels chart text from each chart's crop.
    pub fn with_charts(mut self, on: bool) -> Self {
        self.charts = on;
        self.refresh_hosts();
        self
    }

    fn models(&self) -> Vec<&'static str> {
        let mut models = vec![self.ocr.id()];
        if self.layout {
            models.push(PAGE_ELEMENTS_MODEL);
            if self.tables {
                models.push(TABLE_STRUCTURE_MODEL);
            }
            if self.charts {
                models.push(GRAPHIC_ELEMENTS_MODEL);
            }
        }
        models
    }

    /// The descriptor's hosts are those of every URL the reader calls, so the egress
    /// policy decides each one. `new` checked that every URL parses and names a host.
    fn refresh_hosts(&mut self) {
        let mut hosts: Vec<String> = Vec::new();
        for model in self.models() {
            let url = self.client.config().url_for(model);
            let host = reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        self.descriptor.locality = Locality::Remote { hosts };
    }

    fn data_uri(&self, png: &[u8]) -> Result<String> {
        let max = self.client.config().max_inline_bytes();
        if png.len() > max {
            return Err(VisionError::InputTooLarge {
                backend: self.descriptor.id.to_string(),
                limit: format!("{max} bytes inline"),
                actual: format!("{} bytes", png.len()),
            });
        }
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ))
    }

    async fn call(
        &self,
        model: &str,
        data_uri: &str,
        merge: Option<&[&str]>,
    ) -> Result<serde_json::Value> {
        let body = wire::body(data_uri, merge);
        self.client.infer(self.descriptor.id.as_str(), model, &body).await
    }

    /// Calls `model` on the crop of `released` at `crop` (sent pixels) and returns its boxes
    /// in 0–1 of the whole sent image, or nothing when the crop is degenerate.
    async fn on_crop(
        &self,
        model: &str,
        released: &ReleasedImage,
        crop: BBox,
    ) -> Result<Vec<(String, NormBox)>> {
        let (w, h) = released.image().size();
        let (x, y, cw, ch) = crop.pixel_bounds(w, h);
        if cw < 8 || ch < 8 {
            return Ok(Vec::new());
        }
        let pixels = ops::crop(released.pixels(), x, y, cw, ch);
        let uri = self.data_uri(&pixels.encode_png()?)?;
        let value = self.call(model, &uri, None).await?;
        let found = wire::elements(self.descriptor.id.as_str(), model, value)?;
        let (fw, fh, fx, fy) = (w as f32, h as f32, x as f32, y as f32);
        let (cw, ch) = (cw as f32, ch as f32);
        Ok(found
            .into_iter()
            .map(|(class, b)| {
                let to_page = NormBox {
                    x_min: (fx + b.x_min * cw) / fw,
                    y_min: (fy + b.y_min * ch) / fh,
                    x_max: (fx + b.x_max * cw) / fw,
                    y_max: (fy + b.y_max * ch) / fh,
                    confidence: b.confidence,
                };
                (class, to_page)
            })
            .collect())
    }
}

/// A block of the page-element model, in 0–1 and in sent pixels.
struct Element {
    class: String,
    norm: BBox,
    px: BBox,
    confidence: Option<f32>,
}

fn kind_of(class: &str) -> BlockKind {
    match class {
        "title" => BlockKind::Title,
        "table" => BlockKind::Table,
        "chart" | "infographic" => BlockKind::Figure,
        _ => BlockKind::Text,
    }
}

fn norm_box(b: &NormBox) -> BBox {
    BBox::new(b.x_min, b.y_min, b.x_max, b.y_max)
}

fn to_px(b: &BBox, (w, h): (u32, u32)) -> BBox {
    BBox::new(b.x0 * w as f32, b.y0 * h as f32, b.x1 * w as f32, b.y1 * h as f32)
}

fn piece(detection: TextDetection, size: (u32, u32)) -> Piece {
    let points: Vec<[f32; 2]> = detection.points.iter().map(|p: &NormPoint| [p.x, p.y]).collect();
    let norm = BBox::covering(points.iter().copied()).unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));
    Piece {
        raw: RawRegion {
            shape: Shape::Polygon(Polygon { points }),
            label: None,
            score: detection.confidence,
            track: None,
        },
        px: to_px(&norm, size),
        text: detection.text,
        confidence: detection.confidence,
    }
}

fn boxed(norm: BBox, label: Option<String>, score: Option<f32>) -> RawRegion {
    RawRegion { shape: Shape::Box(norm), label, score, track: None }
}

/// The index of the smallest element whose box holds `point`.
fn holder(elements: &[Element], point: [f32; 2]) -> Option<usize> {
    elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.px.contains(point))
        .min_by(|(_, a), (_, b)| a.px.area().total_cmp(&b.px.area()))
        .map(|(i, _)| i)
}

/// Rows and columns of a table: each cell's index range among the sorted rows (columns)
/// it overlaps by at least half its own height (width).
fn spans(cell: &BBox, bands: &[BBox], vertical: bool) -> Option<(u32, u32)> {
    let hits: Vec<u32> = bands
        .iter()
        .enumerate()
        .filter(|(_, band)| {
            let (a0, a1, b0, b1, len) = if vertical {
                (cell.y0, cell.y1, band.y0, band.y1, cell.height())
            } else {
                (cell.x0, cell.x1, band.x0, band.x1, cell.width())
            };
            (a1.min(b1) - a0.max(b0)) >= 0.5 * len.max(f32::EPSILON)
        })
        .map(|(i, _)| i as u32)
        .collect();
    let first = *hits.first()?;
    Some((first, hits.len() as u32))
}

/// Cells of one table from the structure model's `cell`, `row` and `column` boxes, with the
/// text of the words inside each cell.
fn cells(
    found: Vec<(String, NormBox)>,
    words: &[Piece],
    size: (u32, u32),
) -> Vec<TableCell<RawRegion>> {
    let mut rows: Vec<BBox> = Vec::new();
    let mut cols: Vec<BBox> = Vec::new();
    let mut raw_cells: Vec<(BBox, Option<f32>)> = Vec::new();
    for (class, b) in found {
        let px = to_px(&norm_box(&b), size);
        match class.as_str() {
            "row" => rows.push(px),
            "column" => cols.push(px),
            "cell" => raw_cells.push((norm_box(&b), b.confidence)),
            _ => {}
        }
    }
    rows.sort_by(|a, b| a.center()[1].total_cmp(&b.center()[1]));
    cols.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
    let mut out: Vec<(TableCell<RawRegion>, Option<f32>)> = Vec::new();
    for (norm, confidence) in raw_cells {
        let px = to_px(&norm, size);
        let (Some((row, row_span)), Some((col, col_span))) =
            (spans(&px, &rows, true), spans(&px, &cols, false))
        else {
            continue;
        };
        let inside: Vec<Piece> =
            words.iter().filter(|w| px.contains(w.px.center())).cloned().collect();
        let text = reading::lines(inside)
            .iter()
            .map(|line| reading::line_text(line))
            .collect::<Vec<_>>()
            .join(" ");
        let cell = TableCell {
            region: boxed(norm, Some("cell".into()), confidence),
            row,
            col,
            row_span,
            col_span,
            text,
        };
        // Two boxes for one grid position keep the more confident.
        match out.iter_mut().find(|(c, _)| c.row == row && c.col == col) {
            Some(existing) if existing.1.unwrap_or(0.0) >= confidence.unwrap_or(0.0) => {}
            Some(existing) => *existing = (cell, confidence),
            None => out.push((cell, confidence)),
        }
    }
    let mut cells: Vec<TableCell<RawRegion>> = out.into_iter().map(|(c, _)| c).collect();
    cells.sort_by_key(|c| (c.row, c.col));
    cells
}

#[async_trait]
impl DocumentReader<ReleasedImage> for NimDocumentReader {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn read(
        &self,
        cx: &VisionCx,
        page: &ReleasedImage,
        options: &ReadOptions,
    ) -> Result<DocumentPage<RawRegion>> {
        cx.check()?;
        if !page.is_for(&self.descriptor.id) {
            return Err(VisionError::NotReleased { backend: self.descriptor.id.to_string() });
        }
        let size = page.image().size();
        let backend = self.descriptor.id.as_str();
        let uri = self.data_uri(page.bytes())?;
        let ocr = self.call(self.ocr.id(), &uri, self.ocr.merge_levels());
        let layout = async {
            if self.layout {
                self.call(PAGE_ELEMENTS_MODEL, &uri, None).await.map(Some)
            } else {
                Ok(None)
            }
        };
        let (ocr, layout) = tokio::try_join!(ocr, layout)?;
        let pieces: Vec<Piece> = wire::text_detections(backend, self.ocr.id(), ocr)?
            .into_iter()
            .filter(|d| !d.text.trim().is_empty())
            .map(|d| piece(d, size))
            .collect();
        let elements: Vec<Element> = match layout {
            Some(value) => wire::elements(backend, PAGE_ELEMENTS_MODEL, value)?
                .into_iter()
                .map(|(class, b)| {
                    let norm = norm_box(&b);
                    Element { px: to_px(&norm, size), norm, class, confidence: b.confidence }
                })
                .filter(|e| !e.px.is_empty())
                .collect(),
            None => Vec::new(),
        };

        // Each piece belongs to the smallest element holding its centre, or to none.
        let mut owned: Vec<Vec<Piece>> = vec![Vec::new(); elements.len()];
        let mut free: Vec<Piece> = Vec::new();
        for piece in pieces {
            match holder(&elements, piece.px.center()) {
                Some(i) => owned[i].push(piece),
                None => free.push(piece),
            }
        }

        let mut blocks: Vec<Block<RawRegion>> = Vec::new();
        let mut boxes: Vec<BBox> = Vec::new();
        for (element, words) in elements.iter().zip(owned) {
            let kind = kind_of(&element.class);
            let lines = reading::lines(words);
            let text = lines.iter().map(|l| reading::line_text(l)).collect::<Vec<_>>().join("\n");
            let flat: Vec<Piece> = lines.into_iter().flatten().collect();
            let confidence = reading::min_confidence(&flat).or(element.confidence);
            let mut block = reading::block(
                kind,
                boxed(element.norm, Some(element.class.clone()), element.confidence),
                text,
                confidence,
            );
            match kind {
                BlockKind::Table if self.tables => {
                    let found =
                        self.on_crop(TABLE_STRUCTURE_MODEL, page, element.px.expanded(4.0)).await?;
                    block.cells = cells(found, &flat, size);
                    if !block.cells.is_empty() {
                        block.text = reading::table_text(&block.cells);
                    }
                }
                BlockKind::Figure if self.charts && element.class == "chart" => {
                    let found = self
                        .on_crop(GRAPHIC_ELEMENTS_MODEL, page, element.px.expanded(4.0))
                        .await?;
                    let labels: Vec<Element> = found
                        .into_iter()
                        .map(|(class, b)| {
                            let norm = norm_box(&b);
                            Element {
                                px: to_px(&norm, size),
                                norm,
                                class,
                                confidence: b.confidence,
                            }
                        })
                        .collect();
                    block.words = flat
                        .iter()
                        .map(|p| {
                            let mut word = p.word();
                            word.region.label =
                                holder(&labels, p.px.center()).map(|i| labels[i].class.clone());
                            word
                        })
                        .collect();
                }
                _ => {}
            }
            if block.words.is_empty() {
                block.words = flat.iter().map(Piece::word).collect();
            }
            boxes.push(element.px);
            blocks.push(block);
        }
        // Text outside every element: one block per line.
        for line in reading::lines(free) {
            let norm = BBox::covering(
                line.iter()
                    .filter_map(|p| p.raw.shape.bbox())
                    .flat_map(|b| [[b.x0, b.y0], [b.x1, b.y1]]),
            )
            .unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));
            let confidence = reading::min_confidence(&line);
            let mut block = reading::block(
                BlockKind::Text,
                boxed(norm, None, confidence),
                reading::line_text(&line),
                confidence,
            );
            block.words = line.iter().map(Piece::word).collect();
            boxes.push(reading::span(&line));
            blocks.push(block);
        }
        let reading_order = reading::reading_order(&boxes);
        let mut out = DocumentPage { blocks, reading_order, markdown: None };
        if options.markdown {
            out.markdown = Some(reading::markdown(&out));
        }
        tracing::debug!(
            vision.backend = backend,
            vision.blocks = out.blocks.len(),
            "nim page read"
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nb(x0: f32, y0: f32, x1: f32, y1: f32) -> NormBox {
        NormBox { x_min: x0, y_min: y0, x_max: x1, y_max: y1, confidence: Some(0.9) }
    }

    #[test]
    fn structure_boxes_become_a_cell_grid_with_text() {
        let size = (100, 100);
        let found = vec![
            ("row".to_string(), nb(0.0, 0.0, 1.0, 0.5)),
            ("row".to_string(), nb(0.0, 0.5, 1.0, 1.0)),
            ("column".to_string(), nb(0.0, 0.0, 0.5, 1.0)),
            ("column".to_string(), nb(0.5, 0.0, 1.0, 1.0)),
            ("cell".to_string(), nb(0.5, 0.5, 1.0, 1.0)),
            ("cell".to_string(), nb(0.0, 0.0, 0.5, 0.5)),
            // A header spanning both columns.
            ("cell".to_string(), nb(0.0, 0.0, 1.0, 0.5)),
        ];
        let word = |text: &str, x0: f32, y0: f32| {
            let px = BBox::new(x0, y0, x0 + 10.0, y0 + 10.0);
            Piece {
                raw: RawRegion::shape(Shape::Box(px)),
                px,
                text: text.into(),
                confidence: Some(0.9),
            }
        };
        let words = vec![word("4.2", 70.0, 70.0), word("Part", 10.0, 10.0)];
        let cells = cells(found, &words, size);
        let grid: Vec<(u32, u32, u32, u32, &str)> =
            cells.iter().map(|c| (c.row, c.col, c.row_span, c.col_span, c.text.as_str())).collect();
        // The two cells at (0, 0) keep the first seen at equal confidence.
        assert_eq!(grid, vec![(0, 0, 1, 1, "Part"), (1, 1, 1, 1, "4.2")]);
    }

    #[test]
    fn pieces_belong_to_the_smallest_holding_element() {
        let element =
            |class: &str, px: BBox| Element { class: class.into(), norm: px, px, confidence: None };
        let elements = vec![
            element("chart", BBox::new(0.0, 0.0, 100.0, 100.0)),
            element("title", BBox::new(10.0, 10.0, 50.0, 20.0)),
        ];
        assert_eq!(holder(&elements, [20.0, 15.0]), Some(1));
        assert_eq!(holder(&elements, [80.0, 80.0]), Some(0));
        assert_eq!(holder(&elements, [180.0, 80.0]), None);
    }
}
