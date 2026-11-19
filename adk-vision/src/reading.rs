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
/// Groups pieces into lines: two pieces share a line when they overlap vertically by at
/// least half the smaller height and sit within two heights of each other horizontally,
/// and a line is a connected group, so a word that bridges two partial lines joins them.
/// Lines are ordered top to bottom, pieces within a line left to right.
pub(crate) fn lines(pieces: Vec<Piece>) -> Vec<Vec<Piece>> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let n = pieces.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for a in 0..n {
        for b in a + 1..n {
            let (p, q) = (&pieces[a].px, &pieces[b].px);
            let shared = overlap(p.y0, p.y1, q.y0, q.y1);
            let low = p.height().min(q.height()).max(1.0);
            let high = p.height().max(q.height()).max(1.0);
            if shared >= 0.5 * low && gap(p.x0, p.x1, q.x0, q.x1) <= 2.0 * high {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: Vec<(usize, Vec<Piece>)> = Vec::new();
    for (i, piece) in pieces.into_iter().enumerate() {
        let r = root(&mut parent, i);
        match groups.iter_mut().find(|(g, _)| *g == r) {
            Some((_, line)) => line.push(piece),
            None => groups.push((r, vec![piece])),
        }
    }
    let mut lines: Vec<Vec<Piece>> = groups.into_iter().map(|(_, line)| line).collect();
    for line in &mut lines {
        line.sort_by(|a, b| a.px.x0.total_cmp(&b.px.x0));
    }
    lines.sort_by(|a, b| {
        let (sa, sb) = (span(a), span(b));
        sa.y0.total_cmp(&sb.y0).then(sa.x0.total_cmp(&sb.x0))
    });
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
/// among blocks that start on the same band (within half the smaller height). A block
/// lying inside a larger one, such as a chart's title inside the chart's box, reads just
/// before its container.
pub(crate) fn reading_order(boxes: &[BBox]) -> Vec<usize> {
    fn emit(i: usize, boxes: &[BBox], container: &[Option<usize>], out: &mut Vec<usize>) {
        let mut nested: Vec<usize> =
            (0..boxes.len()).filter(|&j| container[j] == Some(i)).collect();
        nested.sort_by(|&a, &b| {
            boxes[a].y0.total_cmp(&boxes[b].y0).then(boxes[a].x0.total_cmp(&boxes[b].x0))
        });
        for j in nested {
            emit(j, boxes, container, out);
        }
        out.push(i);
    }
    let inside = |i: usize, j: usize| {
        i != j
            && boxes[j].area() > boxes[i].area()
            && boxes[i].intersection(&boxes[j]).is_some_and(|x| x.area() >= 0.8 * boxes[i].area())
    };
    let container: Vec<Option<usize>> = (0..boxes.len())
        .map(|i| {
            (0..boxes.len())
                .filter(|&j| inside(i, j))
                .min_by(|&a, &b| boxes[a].area().total_cmp(&boxes[b].area()))
        })
        .collect();
    let mut order: Vec<usize> = (0..boxes.len()).filter(|&i| container[i].is_none()).collect();
    order.sort_by(|&a, &b| boxes[a].y0.total_cmp(&boxes[b].y0));
    let mut top = Vec::with_capacity(order.len());
    let mut band: Vec<usize> = Vec::new();
    for i in order {
        let starts_band = band.first().is_some_and(|&first| {
            let height = boxes[first].height().min(boxes[i].height()).max(1.0);
            boxes[i].y0 - boxes[first].y0 > 0.5 * height
        });
        if starts_band {
            band.sort_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
            top.append(&mut band);
        }
        band.push(i);
    }
    band.sort_by(|&a, &b| boxes[a].x0.total_cmp(&boxes[b].x0));
    top.append(&mut band);
    let mut out = Vec::with_capacity(boxes.len());
    for i in top {
        emit(i, boxes, &container, &mut out);
    }
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

    #[cfg(any(feature = "ocr", feature = "nim"))]
    #[test]
    fn a_nested_block_reads_before_its_container() {
        let boxes = vec![
            // A chart whose box also holds its title.
            BBox::new(67.0, 677.0, 917.0, 1164.0),
            BBox::new(77.0, 690.0, 250.0, 720.0),
            BBox::new(80.0, 70.0, 550.0, 120.0),
        ];
        assert_eq!(reading_order(&boxes), vec![2, 1, 0]);
    }

    #[cfg(feature = "nim")]
    #[test]
    fn a_bridging_word_joins_two_partial_lines() {
        // In centre order, "this" and "200" start lines before "inspected" and
        // "conveyor" bridge them: the recorded report page's first line.
        let words = vec![
            piece("The", 79.0, 174.0, 121.0, 192.0),
            piece("conveyor", 128.0, 176.0, 225.0, 197.0),
            piece("inspected", 230.0, 174.0, 333.0, 197.0),
            piece("200", 338.0, 175.0, 378.0, 193.0),
            piece("printed", 385.0, 173.0, 457.0, 198.0),
            piece("parts", 464.0, 175.0, 516.0, 197.0),
            piece("this", 524.0, 174.0, 560.0, 193.0),
            piece("quarter.", 568.0, 174.0, 649.0, 197.0),
        ];
        let lines = lines(words);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "The conveyor inspected 200 printed parts this quarter.");
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
