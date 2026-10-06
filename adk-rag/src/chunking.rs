//! Document chunking strategies.
//!
//! This module provides the [`Chunker`] trait and three implementations:
//!
//! - [`FixedSizeChunker`] — splits by byte length with configurable overlap
//! - [`RecursiveChunker`] — splits hierarchically by paragraphs, sentences, then words
//! - [`MarkdownChunker`] — splits by markdown headers, preserving header context
//!
//! Sizes are measured in bytes of UTF-8, not characters. Chunks never split a
//! character, so a chunk is at most `chunk_size` bytes unless a single character
//! is wider than `chunk_size`, in which case that character forms a chunk alone.

use tracing::warn;

use crate::document::{Chunk, Document};
use crate::error::{RagError, Result};

/// MSRV-compatible replacement for `str::floor_char_boundary` (stable since 1.91.0).
/// Returns the largest byte index `<= index` that is a valid char boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Returns the byte index just past the character that starts at `index`.
fn next_char_boundary(s: &str, index: usize) -> usize {
    s[index..].chars().next().map_or(s.len(), |c| index + c.len_utf8())
}

/// Checks that `chunk_size` is positive and `chunk_overlap` is smaller than it.
fn validate_sizes(chunk_size: usize, chunk_overlap: usize) -> Result<()> {
    if chunk_size == 0 {
        return Err(RagError::ConfigError("chunk_size must be greater than zero".to_string()));
    }
    if chunk_overlap >= chunk_size {
        return Err(RagError::ConfigError(format!(
            "chunk_overlap ({chunk_overlap}) must be less than chunk_size ({chunk_size})"
        )));
    }
    Ok(())
}

/// Replaces an invalid size pair with the nearest configuration that still advances.
///
/// A zero `chunk_size` becomes one byte and an overlap that is not smaller than
/// `chunk_size` is dropped, so chunking covers the whole document instead of
/// stopping after the first chunk.
fn normalize_sizes(chunker: &str, chunk_size: usize, chunk_overlap: usize) -> (usize, usize) {
    match validate_sizes(chunk_size, chunk_overlap) {
        Ok(()) => (chunk_size, chunk_overlap),
        Err(error) => {
            let size = chunk_size.max(1);
            let overlap = if chunk_overlap < size { chunk_overlap } else { 0 };
            warn!(
                chunker,
                error = %error,
                chunk_size = size,
                chunk_overlap = overlap,
                "invalid chunker sizes; use try_new to reject them"
            );
            (size, overlap)
        }
    }
}

/// A strategy for splitting documents into chunks.
///
/// Implementations produce [`Chunk`]s with text and metadata but no embeddings.
/// Embeddings are attached later by the pipeline.
pub trait Chunker: Send + Sync {
    /// Split a document into chunks.
    ///
    /// Returns an empty `Vec` if the document has empty text.
    /// Each returned chunk has an empty embedding vector.
    fn chunk(&self, document: &Document) -> Vec<Chunk>;
}

/// Splits text into fixed-size chunks by byte length with configurable overlap.
///
/// Each chunk holds at most `chunk_size` bytes of UTF-8 and starts
/// `chunk_size - chunk_overlap` bytes after the previous one, rounded down to a
/// character boundary and always at least one character further on.
///
/// Chunk IDs are generated as `{document_id}_{chunk_index}`. Each chunk inherits
/// the parent document's metadata plus a `chunk_index` field.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rag::FixedSizeChunker;
///
/// let chunker = FixedSizeChunker::try_new(256, 50)?;
/// let chunks = chunker.chunk(&document);
/// ```
#[derive(Debug, Clone)]
pub struct FixedSizeChunker {
    chunk_size: usize,
    chunk_overlap: usize,
}

impl FixedSizeChunker {
    /// Create a new `FixedSizeChunker`.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// An invalid pair is normalised rather than rejected: a zero `chunk_size`
    /// becomes 1 and a `chunk_overlap` that is not smaller than `chunk_size` is
    /// ignored, with a warning logged. Use [`try_new`](Self::try_new) to reject it.
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        let (chunk_size, chunk_overlap) =
            normalize_sizes("FixedSizeChunker", chunk_size, chunk_overlap);
        Self { chunk_size, chunk_overlap }
    }

    /// Create a new `FixedSizeChunker`, rejecting an invalid size pair.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] when `chunk_size` is zero or
    /// `chunk_overlap` is not smaller than `chunk_size`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::FixedSizeChunker;
    ///
    /// assert!(FixedSizeChunker::try_new(512, 100).is_ok());
    /// assert!(FixedSizeChunker::try_new(100, 100).is_err());
    /// ```
    pub fn try_new(chunk_size: usize, chunk_overlap: usize) -> Result<Self> {
        validate_sizes(chunk_size, chunk_overlap)?;
        Ok(Self { chunk_size, chunk_overlap })
    }
}

impl Chunker for FixedSizeChunker {
    fn chunk(&self, document: &Document) -> Vec<Chunk> {
        split_by_size(&document.text, self.chunk_size, self.chunk_overlap)
            .into_iter()
            .enumerate()
            .map(|(chunk_index, text)| {
                let mut metadata = document.metadata.clone();
                metadata.insert("chunk_index".to_string(), chunk_index.to_string());
                Chunk {
                    id: format!("{}_{chunk_index}", document.id),
                    text,
                    embedding: Vec::new(),
                    metadata,
                    document_id: document.id.clone(),
                }
            })
            .collect()
    }
}

/// Splits text hierarchically: paragraphs → sentences → words.
///
/// First splits by paragraph separators (`\n\n`). If a paragraph exceeds
/// `chunk_size` bytes, splits by sentence boundaries (`. `, `! `, `? `). If a
/// sentence still exceeds `chunk_size`, splits by word boundaries, and a single
/// word longer than `chunk_size` is split by byte length with overlap.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rag::RecursiveChunker;
///
/// let chunker = RecursiveChunker::new(512, 100);
/// let chunks = chunker.chunk(&document);
/// ```
#[derive(Debug, Clone)]
pub struct RecursiveChunker {
    chunk_size: usize,
    chunk_overlap: usize,
}

impl RecursiveChunker {
    /// Create a new `RecursiveChunker`.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// An invalid pair is normalised rather than rejected: a zero `chunk_size`
    /// becomes 1 and a `chunk_overlap` that is not smaller than `chunk_size` is
    /// ignored, with a warning logged. Use [`try_new`](Self::try_new) to reject it.
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        let (chunk_size, chunk_overlap) =
            normalize_sizes("RecursiveChunker", chunk_size, chunk_overlap);
        Self { chunk_size, chunk_overlap }
    }

    /// Create a new `RecursiveChunker`, rejecting an invalid size pair.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] when `chunk_size` is zero or
    /// `chunk_overlap` is not smaller than `chunk_size`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::RecursiveChunker;
    ///
    /// assert!(RecursiveChunker::try_new(512, 100).is_ok());
    /// assert!(RecursiveChunker::try_new(100, 100).is_err());
    /// ```
    pub fn try_new(chunk_size: usize, chunk_overlap: usize) -> Result<Self> {
        validate_sizes(chunk_size, chunk_overlap)?;
        Ok(Self { chunk_size, chunk_overlap })
    }
}

/// Split text by a separator, then merge segments into chunks that respect
/// `chunk_size`. If a segment exceeds `chunk_size`, it is split further
/// using the next-level separator.
fn split_and_merge(
    text: &str,
    chunk_size: usize,
    chunk_overlap: usize,
    separators: &[&str],
) -> Vec<String> {
    if text.len() <= chunk_size || separators.is_empty() {
        return split_by_size(text, chunk_size, chunk_overlap);
    }

    let separator = separators[0];
    let remaining_separators = &separators[1..];

    let segments: Vec<&str> = if separator == " " {
        text.split(' ').collect()
    } else {
        split_keeping_separator(text, separator)
    };

    let mut chunks = Vec::new();
    let mut current = String::new();

    for segment in segments {
        if current.is_empty() {
            current = segment.to_string();
        } else if current.len() + segment.len() <= chunk_size {
            current.push_str(segment);
        } else {
            // Current chunk is full — process it
            if current.len() > chunk_size {
                chunks.extend(split_and_merge(
                    &current,
                    chunk_size,
                    chunk_overlap,
                    remaining_separators,
                ));
            } else {
                chunks.push(current);
            }
            // Start new chunk with overlap
            current = segment.to_string();
        }
    }

    if !current.is_empty() {
        if current.len() > chunk_size {
            chunks.extend(split_and_merge(
                &current,
                chunk_size,
                chunk_overlap,
                remaining_separators,
            ));
        } else {
            chunks.push(current);
        }
    }

    chunks
}

/// Split text at a separator while keeping the separator attached to the preceding segment.
fn split_keeping_separator<'a>(text: &'a str, separator: &str) -> Vec<&'a str> {
    let mut result = Vec::new();
    let mut start = 0;

    while let Some(pos) = text[start..].find(separator) {
        let end = start + pos + separator.len();
        result.push(&text[start..end]);
        start = end;
    }

    if start < text.len() {
        result.push(&text[start..]);
    }

    result
}

/// Byte-length splitting with overlap that never splits a character.
///
/// Both the chunk end and the next start are rounded down to a character
/// boundary, then pushed one character forward if rounding left them at
/// `start`. Without that, a step narrower than a multibyte character would
/// leave `start` in place forever.
fn split_by_size(text: &str, chunk_size: usize, chunk_overlap: usize) -> Vec<String> {
    let step = chunk_size.saturating_sub(chunk_overlap);
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let mut end = floor_char_boundary(text, start.saturating_add(chunk_size));
        if end <= start {
            end = next_char_boundary(text, start);
        }
        chunks.push(text[start..end].to_string());

        let next = floor_char_boundary(text, start.saturating_add(step));
        start = if next > start { next } else { next_char_boundary(text, start) };
    }

    chunks
}

impl Chunker for RecursiveChunker {
    fn chunk(&self, document: &Document) -> Vec<Chunk> {
        if document.text.is_empty() {
            return Vec::new();
        }

        let separators = ["\n\n", ". ", "! ", "? ", " "];
        let raw_chunks =
            split_and_merge(&document.text, self.chunk_size, self.chunk_overlap, &separators);

        raw_chunks
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let mut metadata = document.metadata.clone();
                metadata.insert("chunk_index".to_string(), i.to_string());
                Chunk {
                    id: format!("{}_{i}", document.id),
                    text,
                    embedding: Vec::new(),
                    metadata,
                    document_id: document.id.clone(),
                }
            })
            .collect()
    }
}

/// Splits text by markdown headers, keeping each section as a chunk.
///
/// Each section is prefixed with its header hierarchy. Sections exceeding
/// `chunk_size` bytes are further split using [`RecursiveChunker`] logic.
/// The `header_path` metadata field records the header hierarchy for each chunk.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rag::MarkdownChunker;
///
/// let chunker = MarkdownChunker::new(512, 100);
/// let chunks = chunker.chunk(&document);
/// ```
#[derive(Debug, Clone)]
pub struct MarkdownChunker {
    chunk_size: usize,
    chunk_overlap: usize,
}

impl MarkdownChunker {
    /// Create a new `MarkdownChunker`.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// An invalid pair is normalised rather than rejected: a zero `chunk_size`
    /// becomes 1 and a `chunk_overlap` that is not smaller than `chunk_size` is
    /// ignored, with a warning logged. Use [`try_new`](Self::try_new) to reject it.
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        let (chunk_size, chunk_overlap) =
            normalize_sizes("MarkdownChunker", chunk_size, chunk_overlap);
        Self { chunk_size, chunk_overlap }
    }

    /// Create a new `MarkdownChunker`, rejecting an invalid size pair.
    ///
    /// # Arguments
    ///
    /// * `chunk_size` — maximum number of bytes per chunk
    /// * `chunk_overlap` — number of bytes shared by consecutive chunks
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] when `chunk_size` is zero or
    /// `chunk_overlap` is not smaller than `chunk_size`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::MarkdownChunker;
    ///
    /// assert!(MarkdownChunker::try_new(512, 100).is_ok());
    /// assert!(MarkdownChunker::try_new(100, 100).is_err());
    /// ```
    pub fn try_new(chunk_size: usize, chunk_overlap: usize) -> Result<Self> {
        validate_sizes(chunk_size, chunk_overlap)?;
        Ok(Self { chunk_size, chunk_overlap })
    }
}

/// A markdown section with its header hierarchy and body text.
struct MarkdownSection {
    header_path: String,
    text: String,
}

/// Parse markdown text into sections split by headers.
fn parse_markdown_sections(text: &str) -> Vec<MarkdownSection> {
    let mut sections = Vec::new();
    let mut headers: Vec<String> = Vec::new();
    let mut current_body = String::new();
    let mut current_header_path = String::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            // Save previous section
            if !current_body.is_empty() || !current_header_path.is_empty() {
                sections.push(MarkdownSection {
                    header_path: current_header_path.clone(),
                    text: current_body.trim().to_string(),
                });
                current_body = String::new();
            }

            // Determine header level
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let header_text = trimmed[level..].trim().to_string();

            // Update header stack
            headers.truncate(level.saturating_sub(1));
            headers.push(header_text);
            current_header_path = headers.join(" > ");
        } else {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        }
    }

    // Save final section
    if !current_body.is_empty() || !current_header_path.is_empty() {
        sections.push(MarkdownSection {
            header_path: current_header_path,
            text: current_body.trim().to_string(),
        });
    }

    sections
}

impl Chunker for MarkdownChunker {
    fn chunk(&self, document: &Document) -> Vec<Chunk> {
        if document.text.is_empty() {
            return Vec::new();
        }

        let sections = parse_markdown_sections(&document.text);
        let mut chunks = Vec::new();
        let mut chunk_index = 0;

        for section in sections {
            // Build section text with header prefix
            let section_text = if section.header_path.is_empty() {
                section.text.clone()
            } else if section.text.is_empty() {
                section.header_path.clone()
            } else {
                format!("{}\n{}", section.header_path, section.text)
            };

            if section_text.is_empty() {
                continue;
            }

            let sub_chunks = if section_text.len() > self.chunk_size {
                // Further split using recursive logic
                let separators = ["\n\n", ". ", "! ", "? ", " "];
                split_and_merge(&section_text, self.chunk_size, self.chunk_overlap, &separators)
            } else {
                vec![section_text]
            };

            for text in sub_chunks {
                let mut metadata = document.metadata.clone();
                metadata.insert("chunk_index".to_string(), chunk_index.to_string());
                metadata.insert("header_path".to_string(), section.header_path.clone());

                chunks.push(Chunk {
                    id: format!("{}_{chunk_index}", document.id),
                    text,
                    embedding: Vec::new(),
                    metadata,
                    document_id: document.id.clone(),
                });
                chunk_index += 1;
            }
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn doc(text: &str) -> Document {
        Document {
            id: "test".to_string(),
            text: text.to_string(),
            metadata: HashMap::new(),
            source_uri: None,
        }
    }

    #[test]
    fn fixed_chunker_utf8_multibyte() {
        // Chinese characters are 3 bytes each in UTF-8.
        // "你好世界" = 4 chars, 12 bytes.
        // With chunk_size=5 (bytes), naive slicing would panic mid-character.
        let chunker = FixedSizeChunker::new(5, 0);
        let chunks = chunker.chunk(&doc("你好世界测试文本"));
        // Should not panic and all chunks should be valid UTF-8
        for chunk in &chunks {
            assert!(chunk.text.is_char_boundary(0));
            // Verify it's valid UTF-8 by iterating chars
            let _ = chunk.text.chars().count();
        }
        // Should produce multiple chunks
        assert!(chunks.len() > 1);
    }

    #[test]
    fn fixed_chunker_utf8_emoji() {
        // Emoji are 4 bytes each. "🦀🚀🎉" = 3 chars, 12 bytes.
        let chunker = FixedSizeChunker::new(6, 0);
        let chunks = chunker.chunk(&doc("🦀🚀🎉✨🌟💫"));
        for chunk in &chunks {
            let _ = chunk.text.chars().count();
        }
        assert!(chunks.len() > 1);
    }

    #[test]
    fn fixed_chunker_utf8_mixed() {
        // Mix of ASCII (1 byte), accented (2 bytes), CJK (3 bytes), emoji (4 bytes)
        let text = "Hello café 你好 🦀";
        let chunker = FixedSizeChunker::new(7, 2);
        let chunks = chunker.chunk(&doc(text));
        for chunk in &chunks {
            let _ = chunk.text.chars().count();
        }
        assert!(!chunks.is_empty());
    }

    #[test]
    fn split_by_size_utf8() {
        let text = "日本語のテスト文字列です";
        let chunks = split_by_size(text, 10, 3);
        for chunk in &chunks {
            let _ = chunk.chars().count();
        }
        assert!(chunks.len() > 1);
    }

    #[test]
    fn recursive_chunker_utf8() {
        let text = "第一段落。这是中文文本。\n\n第二段落。更多中文内容在这里。";
        let chunker = RecursiveChunker::new(15, 3);
        let chunks = chunker.chunk(&doc(text));
        for chunk in &chunks {
            let _ = chunk.text.chars().count();
        }
        assert!(!chunks.is_empty());
    }

    fn texts(chunks: &[Chunk]) -> Vec<&str> {
        chunks.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn fixed_chunker_advances_when_step_is_narrower_than_a_character() {
        // Step 2 bytes, characters 3 bytes: rounding down used to leave `start` in place.
        let chunks = FixedSizeChunker::new(5, 3).chunk(&doc("你好世界"));
        assert_eq!(texts(&chunks), ["你", "好", "世", "界"]);

        // Step 1 byte, characters 4 bytes.
        let chunks = FixedSizeChunker::new(6, 5).chunk(&doc("🦀🚀"));
        assert_eq!(texts(&chunks), ["🦀", "🚀"]);
    }

    #[test]
    fn fixed_chunker_emits_characters_wider_than_chunk_size_whole() {
        let chunks = FixedSizeChunker::new(2, 0).chunk(&doc("a你b"));
        assert_eq!(texts(&chunks), ["a", "你", "b"]);
    }

    #[test]
    fn split_by_size_advances_when_step_is_narrower_than_a_character() {
        assert_eq!(split_by_size("日本語", 4, 3), ["日", "本", "語"]);
    }

    #[test]
    fn recursive_and_markdown_chunkers_terminate_on_narrow_steps() {
        let text = "# 标题\n\n第一段落很长很长很长。第二句也很长很长。\n\n🦀🦀🦀🦀🦀🦀";
        let recursive = RecursiveChunker::new(5, 4).chunk(&doc(text));
        let markdown = MarkdownChunker::new(5, 4).chunk(&doc(text));
        assert!(!recursive.is_empty());
        assert!(!markdown.is_empty());
    }

    #[test]
    fn try_new_rejects_invalid_sizes() {
        for (size, overlap) in [(0, 0), (10, 10), (10, 11)] {
            assert!(
                matches!(FixedSizeChunker::try_new(size, overlap), Err(RagError::ConfigError(_))),
                "FixedSizeChunker accepted ({size}, {overlap})"
            );
            assert!(
                matches!(RecursiveChunker::try_new(size, overlap), Err(RagError::ConfigError(_))),
                "RecursiveChunker accepted ({size}, {overlap})"
            );
            assert!(
                matches!(MarkdownChunker::try_new(size, overlap), Err(RagError::ConfigError(_))),
                "MarkdownChunker accepted ({size}, {overlap})"
            );
        }
        assert!(FixedSizeChunker::try_new(10, 9).is_ok());
    }

    #[test]
    fn new_normalizes_invalid_sizes_instead_of_truncating() {
        // An overlap equal to chunk_size used to stop after the first chunk.
        let chunks = FixedSizeChunker::new(4, 4).chunk(&doc("abcdefghij"));
        assert_eq!(texts(&chunks), ["abcd", "efgh", "ij"]);

        let chunks = FixedSizeChunker::new(0, 0).chunk(&doc("abc"));
        assert_eq!(texts(&chunks), ["a", "b", "c"]);
    }

    fn arb_text() -> impl Strategy<Value = String> {
        prop_oneof![
            "\\PC{0,120}",
            "[a 你好世界🦀é\\n.!?#]{0,120}",
            proptest::collection::vec(any::<char>(), 0..60)
                .prop_map(|chars| chars.into_iter().collect::<String>()),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Chunking terminates for any UTF-8 text and any size pair, and every
        /// chunk is a non-empty slice that advances through the text.
        #[test]
        fn prop_chunkers_terminate_for_any_text_and_sizes(
            text in arb_text(),
            chunk_size in 0usize..24,
            chunk_overlap in 0usize..32,
        ) {
            let document = doc(&text);

            let fixed = FixedSizeChunker::new(chunk_size, chunk_overlap).chunk(&document);
            prop_assert!(fixed.len() <= text.len());
            prop_assert!(fixed.iter().all(|c| !c.text.is_empty()));
            if let (Some(first), Some(last)) = (fixed.first(), fixed.last()) {
                prop_assert!(text.starts_with(&first.text));
                prop_assert!(text.ends_with(&last.text));
            }

            let recursive = RecursiveChunker::new(chunk_size, chunk_overlap).chunk(&document);
            prop_assert!(recursive.iter().all(|c| !c.text.is_empty()));

            let markdown = MarkdownChunker::new(chunk_size, chunk_overlap).chunk(&document);
            prop_assert!(markdown.iter().all(|c| !c.text.is_empty()));
        }

        /// With no overlap the chunks partition the text, and none exceeds
        /// `chunk_size` unless it is a single wider character.
        #[test]
        fn prop_fixed_chunks_without_overlap_partition_the_text(
            text in arb_text(),
            chunk_size in 1usize..24,
        ) {
            let chunks = FixedSizeChunker::try_new(chunk_size, 0)
                .expect("valid sizes")
                .chunk(&doc(&text));
            prop_assert_eq!(texts(&chunks).concat(), text.clone());
            for chunk in &chunks {
                prop_assert!(
                    chunk.text.len() <= chunk_size || chunk.text.chars().count() == 1,
                    "chunk {:?} exceeds {} bytes", chunk.text, chunk_size
                );
            }
        }
    }
}
