//! A small cache of image embeddings, so a SAM-family encoder runs once per prepared image.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::digest::Digest;
use crate::geometry::TransformChain;
use crate::sent::SentImage;

/// The most recent encoder outputs, keyed by the source image's digest and the transform
/// chain to the pixels the encoder saw: the same source cropped or resized differently is a
/// different key.
pub(crate) struct EmbeddingCache<T> {
    capacity: usize,
    entries: Mutex<VecDeque<(Digest, TransformChain, Arc<T>)>>,
}

impl<T> EmbeddingCache<T> {
    /// A cache holding at most `capacity` embeddings.
    pub fn new(capacity: usize) -> Self {
        Self { capacity, entries: Mutex::new(VecDeque::with_capacity(capacity)) }
    }

    /// The cached embedding of `image`, if any; a hit becomes the most recent entry.
    pub fn get(&self, image: &SentImage) -> Option<Arc<T>> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let index = entries
            .iter()
            .position(|(digest, chain, _)| *digest == image.source() && chain == image.chain())?;
        let entry = entries.remove(index)?;
        let value = Arc::clone(&entry.2);
        entries.push_back(entry);
        Some(value)
    }

    /// Stores the embedding of `image`, evicting the least recent entry when full.
    pub fn put(&self, image: &SentImage, value: Arc<T>) {
        if self.capacity == 0 {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|(digest, chain, _)| !(*digest == image.source() && chain == image.chain()));
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back((image.source(), image.chain().clone(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::DecodedImage;

    fn sent(tag: &[u8], width: u32) -> SentImage {
        let pixels = Arc::new(DecodedImage::filled(width, 4, [0, 0, 0, 255]));
        SentImage::new(Digest::of(tag), pixels, TransformChain::new(width, 4))
    }

    #[test]
    fn a_hit_needs_the_same_source_and_chain_and_the_oldest_is_evicted() {
        let cache = EmbeddingCache::new(2);
        let (a, b, c) = (sent(b"a", 4), sent(b"b", 4), sent(b"c", 4));
        cache.put(&a, Arc::new(1));
        cache.put(&b, Arc::new(2));
        assert_eq!(cache.get(&a).as_deref(), Some(&1));
        // `a` is now the most recent, so `c` evicts `b`.
        cache.put(&c, Arc::new(3));
        assert_eq!(cache.get(&b), None);
        assert_eq!(cache.get(&a).as_deref(), Some(&1));
        assert_eq!(cache.get(&c).as_deref(), Some(&3));
        // The same source seen through another chain is another key.
        assert_eq!(cache.get(&sent(b"a", 8)), None);
    }
}
