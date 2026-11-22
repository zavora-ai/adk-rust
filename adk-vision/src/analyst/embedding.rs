//! [`WordHashEmbedding`]: a text embedding that needs no model.

use adk_rag::EmbeddingProvider;
use async_trait::async_trait;

/// Words too common in captions and questions to tell chunks apart.
const STOP_WORDS: &[&str] = &[
    "a", "about", "an", "and", "any", "are", "as", "at", "be", "been", "by", "can", "do", "does",
    "did", "during", "for", "from", "has", "have", "here", "in", "into", "is", "it", "its", "of",
    "on", "one", "or", "over", "part", "seen", "so", "some", "that", "the", "their", "them",
    "then", "there", "these", "this", "those", "to", "up", "was", "were", "what", "when",
    "where", "which", "while", "who", "with", "image", "images", "frame", "frames", "video",
    "scene", "show", "shows", "shown", "visible",
];

/// A text embedding with no model and no network: each word, lower-cased and cut to a crude
/// stem, is hashed into one of `dimensions` buckets, and the counts are scaled to unit
/// length, so the cosine similarity of two texts measures the words they share.
///
/// It is the offline default for retrieving captions when no text embedding model is
/// configured, and a deterministic one for tests. A sentence embedding model — `adk-rag`'s
/// `OnnxEmbeddingProvider` on this machine, or a hosted one — also matches captions that
/// say the same thing in other words.
///
/// # Example
///
/// ```
/// use adk_rag::EmbeddingProvider;
/// use adk_vision::analyst::WordHashEmbedding;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let words = WordHashEmbedding::default();
/// let caption = words.embed("A forklift carries a pallet into aisle 2.").await.unwrap();
/// let question = words.embed("When do forklifts carry pallets?").await.unwrap();
/// let other = words.embed("A truck backs up to the dock door.").await.unwrap();
/// let cosine = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
/// assert!(cosine(&caption, &question) > cosine(&other, &question));
/// # });
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WordHashEmbedding {
    dimensions: usize,
}

impl Default for WordHashEmbedding {
    /// 1,024 buckets.
    fn default() -> Self {
        Self::new(1_024)
    }
}

impl WordHashEmbedding {
    /// An embedding into `dimensions` buckets, at least 16.
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions: dimensions.max(16) }
    }

    /// The vector of `text`: unit length, or the first axis for a text with no word.
    pub fn vector(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0f32; self.dimensions];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .map(str::to_lowercase)
            .filter(|w| !w.is_empty() && !STOP_WORDS.contains(&w.as_str()))
        {
            // FNV-1a: stable across platforms and releases, unlike the std hasher.
            let hash = stem(&word)
                .bytes()
                .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
            vector[(hash % self.dimensions as u64) as usize] += 1.0;
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm == 0.0 {
            vector[0] = 1.0;
        } else {
            vector.iter_mut().for_each(|v| *v /= norm);
        }
        vector
    }
}

/// `word` without a plural, `-ing` or `-ed` ending, so "falls" and "falling" are one word;
/// irregular forms such as "fell" stay as they are.
fn stem(word: &str) -> std::borrow::Cow<'_, str> {
    let n = word.len();
    if n > 4 && word.ends_with("ies") {
        return format!("{}y", &word[..n - 3]).into();
    }
    let cut = |len: usize| std::borrow::Cow::Borrowed(&word[..len]);
    if n > 5 && word.ends_with("ing") {
        return cut(n - 3);
    }
    if n > 4 && word.ends_with("ied") {
        return format!("{}y", &word[..n - 3]).into();
    }
    if n > 4 && word.ends_with("ed") {
        return cut(n - 2);
    }
    if n > 4 && (word.ends_with("ches") || word.ends_with("shes") || word.ends_with("xes")) {
        return cut(n - 2);
    }
    if n > 3 && word.ends_with('s') && !word.ends_with("ss") {
        return cut(n - 1);
    }
    cut(n)
}

#[async_trait]
impl EmbeddingProvider for WordHashEmbedding {
    async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
        Ok(self.vector(text))
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_join_forms_of_one_word() {
        for (a, b) in [
            ("falls", "falling"),
            ("boxes", "box"),
            ("climbs", "climbed"),
            ("ladders", "ladder"),
            ("carries", "carry"),
            ("carried", "carry"),
        ] {
            assert_eq!(stem(a), stem(b), "{a} / {b}");
        }
        assert_eq!(stem("glass"), "glass");
        assert_eq!(stem("fell"), "fell");
    }

    #[test]
    fn vectors_are_unit_length_and_stable() {
        let words = WordHashEmbedding::new(64);
        let v = words.vector("The truck backs into dock 1");
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(v, words.vector("the TRUCK backs into Dock 1."));
        assert_eq!(words.vector("the of a")[0], 1.0);
    }
}
