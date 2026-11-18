//! Document fixtures with ground truth, and the scores the document-reader tests report:
//! character accuracy over the page's words, and edge error in source pixels for each
//! ground-truth text line a backend found.

#![allow(dead_code)]

use std::path::PathBuf;

use adk_vision::conformance::{Expected, Fixture};
use adk_vision::{BBox, DecodeLimits, DocumentPage, SourceImage};
use serde::Deserialize;

pub const DOC_PAGE: &str = "doc-page-1000x1300";
pub const INVOICE: &str = "invoice-personal-1000x1300";

pub fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/documents")
}

pub fn page(name: &str) -> SourceImage {
    let bytes = std::fs::read(dir().join(format!("{name}.png"))).expect("fixture image");
    SourceImage::decode(&bytes, &DecodeLimits::default()).expect("fixture decodes")
}

#[derive(Clone, Debug, Deserialize)]
pub struct Object {
    pub label: String,
    pub text: Option<String>,
    pub bbox: [f32; 4],
    #[serde(default)]
    pub personal: bool,
}

impl Object {
    pub fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

#[derive(Debug, Deserialize)]
struct Truth {
    objects: Vec<Object>,
}

pub fn truth(name: &str) -> Vec<Object> {
    let text = std::fs::read_to_string(dir().join(format!("{name}.groundtruth.json")))
        .expect("ground truth");
    serde_json::from_str::<Truth>(&text).expect("ground truth parses").objects
}

/// The ground-truth objects that are one line of text.
pub fn text_lines(name: &str) -> Vec<Object> {
    truth(name)
        .into_iter()
        .filter(|o| o.text.as_deref().is_some_and(|t| !t.trim().is_empty()))
        .collect()
}

/// The conformance fixture for document readers: the report page, with its title and its
/// footer line as the objects every reader must place within `tolerance_px`.
pub fn conformance_fixture(tolerance_px: f32) -> Fixture {
    let objects = truth(DOC_PAGE);
    let find = |label: &str| {
        let object = objects.iter().find(|o| o.label == label).expect("labelled object");
        Expected { label: None, bbox: object.bbox() }
    };
    let mut fixture = Fixture::shapes();
    fixture.image = page(DOC_PAGE);
    fixture.objects = vec![find("title"), find("footer")];
    fixture.tolerance_px = tolerance_px;
    fixture
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (prev[j] + 1).min(row[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = row;
    }
    prev[b.len()]
}

/// Every word the page holds: its block texts (and cell texts), split at whitespace.
pub fn page_words(page: &DocumentPage) -> Vec<String> {
    let mut words = Vec::new();
    for block in &page.blocks {
        words.extend(block.text.split_whitespace().map(str::to_string));
    }
    words
}

/// Character accuracy, `1 - errors / characters`, of the page's words against the ground
/// truth's: exact matches first, then each remaining truth word against its nearest
/// remaining page word by edit distance. Order-independent, so reading order and block
/// granularity do not count against a backend.
pub fn char_accuracy(page: &DocumentPage, truth: &[Object]) -> f64 {
    let mut found = page_words(page);
    let wanted: Vec<String> = truth
        .iter()
        .filter_map(|o| o.text.as_deref())
        .flat_map(|t| t.split_whitespace().map(str::to_string))
        .collect();
    let total: usize = wanted.iter().map(|w| w.chars().count()).sum();
    let mut missing = Vec::new();
    for word in wanted {
        match found.iter().position(|f| *f == word) {
            Some(i) => {
                found.remove(i);
            }
            None => missing.push(word),
        }
    }
    let mut errors = 0usize;
    for word in missing {
        let best = found
            .iter()
            .enumerate()
            .map(|(i, f)| (i, levenshtein(&word, f)))
            .min_by_key(|(_, d)| *d);
        match best {
            Some((i, d)) if d < word.chars().count() => {
                errors += d;
                found.remove(i);
            }
            _ => errors += word.chars().count(),
        }
    }
    1.0 - errors as f64 / total.max(1) as f64
}

fn iou(a: &BBox, b: &BBox) -> f32 {
    a.iou(b)
}

/// The page's line boxes: its words grouped into lines where it has words, else its block
/// boxes; and its table cells.
pub fn line_boxes(page: &DocumentPage) -> Vec<BBox> {
    let mut out = Vec::new();
    for block in &page.blocks {
        if block.words.is_empty() {
            out.extend(block.region.shape.bbox());
        } else {
            let mut words: Vec<BBox> =
                block.words.iter().filter_map(|w| w.region.shape.bbox()).collect();
            words.sort_by(|a, b| a.y0.total_cmp(&b.y0));
            let mut lines: Vec<BBox> = Vec::new();
            for w in words {
                let joined = lines.iter_mut().find(|l| {
                    let shared = l.y1.min(w.y1) - l.y0.max(w.y0);
                    shared >= 0.5 * w.height().min(l.height())
                });
                match joined {
                    Some(l) => {
                        *l = BBox::new(
                            l.x0.min(w.x0),
                            l.y0.min(w.y0),
                            l.x1.max(w.x1),
                            l.y1.max(w.y1),
                        )
                    }
                    None => lines.push(w),
                }
            }
            out.extend(lines);
        }
        out.extend(block.cells.iter().filter_map(|c| c.region.shape.bbox()));
    }
    out
}

/// Edge error for the ground-truth lines a backend found.
#[derive(Debug)]
pub struct BoxScore {
    /// Lines matched: a found box holds the line's centre and overlaps it.
    pub matched: usize,
    /// Lines in the ground truth.
    pub total: usize,
    /// The median of each matched line's largest edge error, in source pixels.
    pub median_px: f32,
    /// The largest edge error over the matched lines.
    pub max_px: f32,
}

pub fn box_error(page: &DocumentPage, truth: &[Object]) -> BoxScore {
    let candidates = line_boxes(page);
    let mut errors = Vec::new();
    for object in truth {
        let want = object.bbox();
        let best = candidates
            .iter()
            .filter(|b| b.contains(want.center()) && want.contains(b.center()))
            .max_by(|a, b| iou(a, &want).total_cmp(&iou(b, &want)));
        if let Some(found) = best {
            let e = [
                (found.x0 - want.x0).abs(),
                (found.y0 - want.y0).abs(),
                (found.x1 - want.x1).abs(),
                (found.y1 - want.y1).abs(),
            ]
            .into_iter()
            .fold(0.0f32, f32::max);
            errors.push(e);
        }
    }
    errors.sort_by(f32::total_cmp);
    BoxScore {
        matched: errors.len(),
        total: truth.len(),
        median_px: errors.get(errors.len() / 2).copied().unwrap_or(f32::NAN),
        max_px: errors.last().copied().unwrap_or(f32::NAN),
    }
}
