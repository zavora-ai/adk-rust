//! Page assembly shared by the document readers: lines from recognised words, reading
//! order, table grids and Markdown.
//!
//! Layout decisions are made on boxes in pixels of the image the backend saw, so they do
//! not depend on the frame a backend answers in; the regions a page carries stay in that
//! frame for the runtime to map.

#[cfg(any(feature = "ocr", feature = "nim"))]
use crate::geometry::BBox;
use crate::geometry::RawRegion;
#[cfg(feature = "nim")]
use crate::traits::Word;
use crate::traits::{Block, BlockKind};
#[cfg(any(feature = "ocr", feature = "nim"))]
use crate::traits::{DocumentPage, TableCell};

#[cfg(feature = "nim")]
/// A recognised word or line: its region in the backend's frame, its box in sent pixels
/// for layout decisions, its text and confidence.
#[derive(Clone, Debug)]
pub(crate) struct Piece {
    pub(crate) raw: RawRegion,
    pub(crate) px: BBox,
    pub(crate) text: String,
    pub(crate) confidence: Option<f32>,
}

#[cfg(feature = "nim")]
impl Piece {
    pub(crate) fn word(&self) -> Word<RawRegion> {
        Word { region: self.raw.clone(), text: self.text.clone(), confidence: self.confidence }
    }
}

#[cfg(feature = "nim")]
fn overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

#[cfg(feature = "nim")]
fn gap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (b0 - a1).max(a0 - b1).max(0.0)
}

#[cfg(feature = "nim")]
/// Groups pieces into lines: a piece joins a line it overlaps vertically by at least half
/// the smaller height and sits within two line heights of horizontally. Lines are ordered
/// top to bottom, pieces within a line left to right.
pub(crate) fn lines(mut pieces: Vec<Piece>) -> Vec<Vec<Piece>> {
    pieces.sort_by(|a, b| a.px.center()[1].total_cmp(&b.px.center()[1]));
    let mut lines: Vec<Vec<Piece>> = Vec::new();
    for piece in pieces {
        let h = piece.px.height().max(1.0);
        let joins = lines.iter().rposition(|line| {
            line.iter().any(|q| {
                let shared = overlap(piece.px.y0, piece.px.y1, q.px.y0, q.px.y1);
                let height = h.min(q.px.height().max(1.0));
                shared >= 0.5 * height
                    && gap(piece.px.x0, piece.px.x1, q.px.x0, q.px.x1) <= 2.0 * h.max(q.px.height())
            })
        });
        match joins {
            Some(i) => lines[i].push(piece),
            None => lines.push(vec![piece]),
        }
    }
    for line in &mut lines {
        line.sort_by(|a, b| a.px.x0.total_cmp(&b.px.x0));
    }
    lines.sort_by(|a, b| span(a).y0.total_cmp(&span(b).y0));
    lines
}

#[cfg(feature = "nim")]
/// The box covering every piece.
pub(crate) fn span(pieces: &[Piece]) -> BBox {
    BBox::covering(pieces.iter().flat_map(|p| [[p.px.x0, p.px.y0], [p.px.x1, p.px.y1]]))
        .unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0))
}

#[cfg(feature = "nim")]
/// The text of a line: its pieces joined by spaces.
pub(crate) fn line_text(line: &[Piece]) -> String {
    line.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ")
}

#[cfg(feature = "nim")]
/// The lowest confidence among the pieces, the one an escalation trigger should see.
pub(crate) fn min_confidence<'a>(pieces: impl IntoIterator<Item = &'a Piece>) -> Option<f32> {
    pieces.into_iter().filter_map(|p| p.confidence).reduce(f32::min)
}

#[cfg(any(feature = "ocr", feature = "nim"))]
/// Reading order for blocks with boxes in sent pixels: top to bottom, and left to right
/// among blocks that start on the same band (within half the smaller height).
pub(crate) fn reading_order(boxes: &[BBox]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| boxes[a].y0.total_cmp(&boxes[b].y0));
    let mut out = Vec::with_capacity(order.len());
    let mut band: Vec<usize> = Vec::new();
    for i in order {
        let starts_band = band.first().is_some_and(|&first| {
            let height = boxes[first].height().min(boxes[i].height()).max(1.0);
            boxes[i].y0 - boxes[first].y0 > 0.5 * height
        });
        if starts_band {
            band.sort_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
            out.append(&mut band);
        }
        band.push(i);
    }
    band.sort_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
    out.append(&mut band);
    out
}

#[cfg(feature = "nim")]
/// A table's text: one row per line, cells separated by tabs.
pub(crate) fn table_text<R>(cells: &[TableCell<R>]) -> String {
    let rows = cells.iter().map(|c| c.row + 1).max().unwrap_or(0);
    let cols = cells.iter().map(|c| c.col + 1).max().unwrap_or(0);
    let mut grid = vec![vec![String::new(); cols as usize]; rows as usize];
    for cell in cells {
        grid[cell.row as usize][cell.col as usize].clone_from(&cell.text);
    }
    grid.iter().map(|row| row.join("\t")).collect::<Vec<_>>().join("\n")
}

#[cfg(any(feature = "ocr", feature = "nim"))]
fn markdown_table<R>(cells: &[TableCell<R>]) -> String {
    let rows = cells.iter().map(|c| c.row + 1).max().unwrap_or(0) as usize;
    let cols = cells.iter().map(|c| c.col + 1).max().unwrap_or(0) as usize;
    let mut grid = vec![vec![String::new(); cols]; rows];
    for cell in cells {
        grid[cell.row as usize][cell.col as usize] = cell.text.replace('|', "\\|");
    }
    let mut out = Vec::with_capacity(rows + 1);
    for (i, row) in grid.iter().enumerate() {
        out.push(format!("| {} |", row.join(" | ")));
        if i == 0 {
            out.push(format!("|{}", " --- |".repeat(cols)));
        }
    }
    out.join("\n")
}

#[cfg(any(feature = "ocr", feature = "nim"))]
/// The page as Markdown, blocks in reading order: titles as headings, tables as pipe
/// tables when their cells are known, formulas as display maths.
pub(crate) fn markdown<R>(page: &DocumentPage<R>) -> String {
    let order: Vec<usize> = if page.reading_order.is_empty() {
        (0..page.blocks.len()).collect()
    } else {
        page.reading_order.clone()
    };
    let mut parts = Vec::with_capacity(order.len());
    for i in order {
        let Some(block) = page.blocks.get(i) else { continue };
        let text = block.text.trim();
        let part = match block.kind {
            BlockKind::Title => format!("# {}", text.replace('\n', " ")),
            BlockKind::Table if !block.cells.is_empty() => markdown_table(&block.cells),
            BlockKind::Formula => format!("$$\n{text}\n$$"),
            _ => text.to_string(),
        };
        if !part.trim().is_empty() {
            parts.push(part);
        }
    }
    parts.join("\n\n")
}

/// A block with no words or cells.
pub(crate) fn block(
    kind: BlockKind,
    region: RawRegion,
    text: String,
    confidence: Option<f32>,
) -> Block<RawRegion> {
    Block { kind, region, text, confidence, words: Vec::new(), cells: Vec::new() }
}

#[cfg(all(test, any(feature = "ocr", feature = "nim")))]
mod tests {
    use super::*;
    #[cfg(feature = "nim")]
    use crate::geometry::Shape;

    #[cfg(feature = "nim")]
    fn piece(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Piece {
        let px = BBox::new(x0, y0, x1, y1);
        Piece {
            raw: RawRegion::shape(Shape::Box(px)),
            px,
            text: text.into(),
            confidence: Some(0.9),
        }
    }

    #[cfg(feature = "nim")]
    #[test]
    fn words_group_into_lines_in_reading_order() {
        let words = vec![
            piece("world", 60.0, 10.0, 100.0, 30.0),
            piece("second", 10.0, 50.0, 70.0, 70.0),
            piece("hello", 10.0, 12.0, 50.0, 31.0),
            // Same height band, but far to the right: another column.
            piece("aside", 400.0, 10.0, 450.0, 30.0),
        ];
        let lines = lines(words);
        let texts: Vec<String> = lines.iter().map(|l| line_text(l)).collect();
        assert_eq!(texts, vec!["hello world", "aside", "second"]);
    }

    #[cfg(any(feature = "ocr", feature = "nim"))]
    #[test]
    fn blocks_on_one_band_read_left_to_right() {
        let boxes = vec![
            BBox::new(500.0, 100.0, 900.0, 140.0),
            BBox::new(50.0, 102.0, 450.0, 140.0),
            BBox::new(50.0, 10.0, 900.0, 60.0),
        ];
        assert_eq!(reading_order(&boxes), vec![2, 1, 0]);
    }

    #[cfg(feature = "nim")]
    #[test]
    fn markdown_renders_titles_tables_and_text() {
        let region = RawRegion::shape(Shape::Box(BBox::new(0.0, 0.0, 1.0, 1.0)));
        let cell = |row, col, text: &str| TableCell {
            region: region.clone(),
            row,
            col,
            row_span: 1,
            col_span: 1,
            text: text.into(),
        };
        let mut table = block(BlockKind::Table, region.clone(), String::new(), None);
        table.cells =
            vec![cell(0, 0, "Part"), cell(0, 1, "Length"), cell(1, 0, "P-017"), cell(1, 1, "4.2")];
        let page = DocumentPage {
            blocks: vec![
                block(BlockKind::Text, region.clone(), "Body".into(), None),
                block(BlockKind::Title, region.clone(), "Report".into(), None),
                table,
            ],
            reading_order: vec![1, 0, 2],
            markdown: None,
        };
        assert_eq!(
            markdown(&page),
            "# Report\n\nBody\n\n| Part | Length |\n| --- | --- |\n| P-017 | 4.2 |"
        );
        assert_eq!(table_text(&page.blocks[2].cells), "Part\tLength\nP-017\t4.2");
    }
}
