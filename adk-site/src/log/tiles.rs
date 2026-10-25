//! C2SP tlog-tiles layout for the site log (spec 3.5 §7.7, §7.8.3 "the log's tiles are built
//! for millions of entries").
//!
//! A verifier reads the checkpoint, then only the tiles a proof needs: hash tiles hold 256
//! hashes of one tree level each (`tile/<L>/<N>`, partial tiles `tile/<L>/<N>.p/<W>`), entry
//! bundles hold 256 log records (`tile/entries/<N>`), each prefixed by its length as a
//! big-endian `u16`.

use std::ops::Range;

use adk_auth::signed_audit::merkle::{empty_root, node_hash};
use adk_core::audit_log::AuditHash;
use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{Result, SiteError};

/// Tree levels per tile.
pub const TILE_HEIGHT: u32 = 8;
/// Hashes or entries in a full tile.
pub const TILE_WIDTH: u64 = 1 << TILE_HEIGHT;

/// Encodes a tile index as C2SP path elements: `1234067` gives `x001/x234/067`.
///
/// # Example
///
/// ```rust
/// use adk_site::log::tile_index_path;
///
/// assert_eq!(tile_index_path(5), "005");
/// assert_eq!(tile_index_path(1_234_067), "x001/x234/067");
/// ```
pub fn tile_index_path(index: u64) -> String {
    let mut groups = vec![format!("{:03}", index % 1000)];
    let mut rest = index / 1000;
    while rest > 0 {
        groups.push(format!("x{:03}", rest % 1000));
        rest /= 1000;
    }
    groups.reverse();
    groups.join("/")
}

/// The path of hash tile `index` at tile level `level` holding `width` hashes, relative to
/// the log prefix.
pub fn hash_tile_path(level: u32, index: u64, width: u64) -> String {
    let base = format!("tile/{level}/{}", tile_index_path(index));
    if width == TILE_WIDTH { base } else { format!("{base}.p/{width}") }
}

/// The path of entry bundle `index` holding `width` entries, relative to the log prefix.
pub fn entry_bundle_path(index: u64, width: u64) -> String {
    let base = format!("tile/entries/{}", tile_index_path(index));
    if width == TILE_WIDTH { base } else { format!("{base}.p/{width}") }
}

/// The tiles that change when a log grows from `old` to `new` entries, as
/// `(level, index, width)`; level `None` is the entry bundle.
pub(crate) fn changed_tiles(old: u64, new: u64) -> Vec<(Option<u32>, u64, u64)> {
    let mut tiles = Vec::new();
    if new <= old {
        return tiles;
    }
    let first = old / TILE_WIDTH;
    let last = (new - 1) / TILE_WIDTH;
    for index in first..=last {
        tiles.push((None, index, (new - index * TILE_WIDTH).min(TILE_WIDTH)));
    }
    let mut level = 0;
    loop {
        let shift = level * TILE_HEIGHT;
        let (old_count, new_count) = (old >> shift, new >> shift);
        if new_count == 0 {
            break;
        }
        if new_count > old_count {
            let first = old_count / TILE_WIDTH;
            let last = (new_count - 1) / TILE_WIDTH;
            for index in first..=last {
                tiles.push((Some(level), index, (new_count - index * TILE_WIDTH).min(TILE_WIDTH)));
            }
        }
        level += 1;
    }
    tiles
}

/// Encodes log entries as an entry bundle.
pub(crate) fn encode_bundle(entries: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for entry in entries {
        let len = u16::try_from(entry.len()).map_err(|_| {
            SiteError::Log(format!("log entry of {} bytes exceeds 65535", entry.len()))
        })?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(entry);
    }
    Ok(out)
}

/// Splits an entry bundle into its entries.
///
/// # Errors
///
/// [`SiteError::Log`] when the bundle is truncated.
pub fn decode_bundle(bundle: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bundle.len() {
        if at + 2 > bundle.len() {
            return Err(SiteError::Log("entry bundle is truncated".into()));
        }
        let len = usize::from(u16::from_be_bytes([bundle[at], bundle[at + 1]]));
        at += 2;
        let end = at + len;
        if end > bundle.len() {
            return Err(SiteError::Log("entry bundle is truncated".into()));
        }
        entries.push(bundle[at..end].to_vec());
        at = end;
    }
    Ok(entries)
}

/// Where a verifier reads published tiles from: a host, an HTTP client, a directory.
#[async_trait]
pub trait TileFetch: Send + Sync {
    /// Returns the file at `path` (relative to the log prefix), or `None` when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the source fails.
    async fn fetch_tile(&self, path: &str) -> Result<Option<Bytes>>;
}

fn split_point(n: u64) -> u64 {
    1 << (63 - (n - 1).leading_zeros())
}

/// The complete subtrees `(height, index)` that make up `range`, left to right.
pub(crate) fn range_nodes(range: Range<u64>) -> Vec<(u32, u64)> {
    let mut nodes = Vec::new();
    let mut start = range.start;
    while start < range.end {
        let remaining = range.end - start;
        let mut level = 63 - remaining.leading_zeros();
        if start != 0 {
            level = level.min(start.trailing_zeros());
        }
        nodes.push((level, start >> level));
        start += 1 << level;
    }
    nodes
}

pub(crate) fn inclusion_ranges(index: u64, size: u64) -> Vec<Range<u64>> {
    fn path(index: u64, range: Range<u64>, out: &mut Vec<Range<u64>>) {
        let n = range.end - range.start;
        if n <= 1 {
            return;
        }
        let mid = range.start + split_point(n);
        if index < mid {
            path(index, range.start..mid, out);
            out.push(mid..range.end);
        } else {
            path(index, mid..range.end, out);
            out.push(range.start..mid);
        }
    }
    let mut out = Vec::new();
    path(index, 0..size, &mut out);
    out
}

pub(crate) fn consistency_ranges(old: u64, new: u64) -> Vec<Range<u64>> {
    fn subproof(m: u64, range: Range<u64>, complete: bool, out: &mut Vec<Range<u64>>) {
        let n = range.end - range.start;
        if m == n {
            if !complete {
                out.push(range);
            }
            return;
        }
        let k = split_point(n);
        let mid = range.start + k;
        if m <= k {
            subproof(m, range.start..mid, complete, out);
            out.push(mid..range.end);
        } else {
            subproof(m - k, mid..range.end, false, out);
            out.push(range.start..mid);
        }
    }
    let mut out = Vec::new();
    if old > 0 && old < new {
        subproof(old, 0..new, true, &mut out);
    }
    out
}

/// Computes proofs for a tree of `size` entries from published tiles alone, as a remote
/// verifier does (spec 3.5 §7.7 `awp verify`).
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::log::{TileFetch, TileProofs};
///
/// # async fn demo(source: &dyn TileFetch) -> adk_site::Result<()> {
/// let proofs = TileProofs::new(source, 42);
/// let proof = proofs.inclusion_proof(41).await?;
/// let record = proofs.entry(41).await?;
/// # let _ = (proof, record);
/// # Ok(())
/// # }
/// ```
pub struct TileProofs<'a> {
    source: &'a dyn TileFetch,
    size: u64,
}

impl<'a> TileProofs<'a> {
    /// Reads tiles from `source` for the tree of `size` entries.
    pub fn new(source: &'a dyn TileFetch, size: u64) -> Self {
        Self { source, size }
    }

    async fn tile(&self, level: Option<u32>, index: u64) -> Result<Bytes> {
        let count = match level {
            None => self.size,
            Some(level) => self.size >> (level * TILE_HEIGHT),
        };
        let width = count.saturating_sub(index * TILE_WIDTH).min(TILE_WIDTH);
        if width == 0 {
            return Err(SiteError::Log(format!("tile {index} is outside a tree of {}", self.size)));
        }
        let path = match level {
            None => entry_bundle_path(index, width),
            Some(level) => hash_tile_path(level, index, width),
        };
        self.source
            .fetch_tile(&path)
            .await?
            .ok_or_else(|| SiteError::Log(format!("tile '{path}' is not published")))
    }

    /// The hash of the complete subtree of `height` at `index`.
    async fn complete_subtree(&self, height: u32, index: u64) -> Result<AuditHash> {
        let level = height / TILE_HEIGHT;
        let span = 1u64 << (height % TILE_HEIGHT);
        let first = index * span;
        let mut hashes = Vec::with_capacity(span as usize);
        let mut current: Option<(u64, Bytes)> = None;
        for position in first..first + span {
            let tile_index = position / TILE_WIDTH;
            if current.as_ref().is_none_or(|(at, _)| *at != tile_index) {
                current = Some((tile_index, self.tile(Some(level), tile_index).await?));
            }
            let (_, tile) = current.as_ref().expect("tile loaded above");
            let offset = ((position % TILE_WIDTH) * 32) as usize;
            let bytes: [u8; 32] = tile
                .get(offset..offset + 32)
                .and_then(|slice| slice.try_into().ok())
                .ok_or_else(|| SiteError::Log(format!("hash tile at level {level} is short")))?;
            hashes.push(AuditHash(bytes));
        }
        while hashes.len() > 1 {
            hashes = hashes.chunks(2).map(|pair| node_hash(&pair[0], &pair[1])).collect();
        }
        Ok(hashes.pop().unwrap_or_else(empty_root))
    }

    /// The RFC 9162 hash of the leaves in `range`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Log`] when a tile is missing or short.
    pub async fn range_hash(&self, range: Range<u64>) -> Result<AuditHash> {
        let mut hashes = Vec::new();
        for (height, index) in range_nodes(range) {
            hashes.push(self.complete_subtree(height, index).await?);
        }
        let mut iter = hashes.iter().rev();
        let Some(last) = iter.next() else {
            return Ok(empty_root());
        };
        Ok(iter.fold(*last, |right, left| node_hash(left, &right)))
    }

    /// The root hash of the tree.
    ///
    /// # Errors
    ///
    /// As [`range_hash`](Self::range_hash).
    pub async fn root(&self) -> Result<AuditHash> {
        self.range_hash(0..self.size).await
    }

    /// The inclusion proof for entry `index`.
    ///
    /// # Errors
    ///
    /// As [`range_hash`](Self::range_hash), or when `index` is outside the tree.
    pub async fn inclusion_proof(&self, index: u64) -> Result<Vec<AuditHash>> {
        if index >= self.size {
            return Err(SiteError::Log(format!(
                "entry {index} is outside a tree of {}",
                self.size
            )));
        }
        let mut proof = Vec::new();
        for range in inclusion_ranges(index, self.size) {
            proof.push(self.range_hash(range).await?);
        }
        Ok(proof)
    }

    /// The consistency proof from the tree of `old` entries to this tree.
    ///
    /// # Errors
    ///
    /// As [`range_hash`](Self::range_hash), or when `old` exceeds the tree.
    pub async fn consistency_proof(&self, old: u64) -> Result<Vec<AuditHash>> {
        if old > self.size {
            return Err(SiteError::Log(format!("size {old} exceeds the tree's {}", self.size)));
        }
        let mut proof = Vec::new();
        for range in consistency_ranges(old, self.size) {
            proof.push(self.range_hash(range).await?);
        }
        Ok(proof)
    }

    /// The raw record of entry `index`, from its entry bundle.
    ///
    /// # Errors
    ///
    /// As [`range_hash`](Self::range_hash).
    pub async fn entry(&self, index: u64) -> Result<Vec<u8>> {
        if index >= self.size {
            return Err(SiteError::Log(format!(
                "entry {index} is outside a tree of {}",
                self.size
            )));
        }
        let bundle = self.tile(None, index / TILE_WIDTH).await?;
        let mut entries = decode_bundle(&bundle)?;
        let offset = (index % TILE_WIDTH) as usize;
        if offset >= entries.len() {
            return Err(SiteError::Log(format!("entry bundle is missing entry {index}")));
        }
        Ok(entries.swap_remove(offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_paths_follow_c2sp() {
        assert_eq!(tile_index_path(0), "000");
        assert_eq!(tile_index_path(999), "999");
        assert_eq!(tile_index_path(1000), "x001/000");
        assert_eq!(hash_tile_path(0, 0, 256), "tile/0/000");
        assert_eq!(hash_tile_path(1, 3, 7), "tile/1/003.p/7");
        assert_eq!(entry_bundle_path(1_234_067, 1), "tile/entries/x001/x234/067.p/1");
    }

    #[test]
    fn changed_tiles_cover_growth() {
        assert_eq!(changed_tiles(0, 1), vec![(None, 0, 1), (Some(0), 0, 1)]);
        let tiles = changed_tiles(255, 257);
        assert!(tiles.contains(&(None, 0, 256)));
        assert!(tiles.contains(&(None, 1, 1)));
        assert!(tiles.contains(&(Some(0), 1, 1)));
        assert!(tiles.contains(&(Some(1), 0, 1)));
        assert!(changed_tiles(5, 5).is_empty());
    }

    #[test]
    fn bundles_round_trip() {
        let entries = vec![b"one".to_vec(), Vec::new(), vec![7; 300]];
        assert_eq!(decode_bundle(&encode_bundle(&entries).unwrap()).unwrap(), entries);
        assert!(decode_bundle(&[0, 5, 1]).is_err());
        assert!(encode_bundle(&[vec![0; 70_000]]).is_err());
    }
}
