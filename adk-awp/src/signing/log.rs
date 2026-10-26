//! The site log: an RFC 9162 Merkle tree of `{seq, manifest}` entries, published as C2SP tiles
//! with a signed-note checkpoint (spec 3.5 §7.7; AWP 2.0 §14.5, C17).
//!
//! The entry of release `n` is the JCS form of `{"seq": n, "manifest": <digest>}` at leaf
//! `n − 1`. Tiles hold 256 hashes of one tree level (`tile/<L>/<N>`, partial tiles
//! `tile/<L>/<N>.p/<W>`); entry bundles hold 256 entries (`tile/entries/<N>`), each prefixed by
//! its length as a big-endian `u16`. The checkpoint is signed by a delegated `release` key
//! whose note key name is the origin `{domain}/.well-known/awp/log`.
//!
//! This module is the one C2SP tile writer of the workspace: [`tiles`] writes a whole log for
//! a static bundle or a test vector, and `adk-site`'s site log writes the tiles each release
//! changes with [`changed_tiles`], [`encode_bundle`] and [`hash_tile`].

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_auth::signed_audit::merkle::{leaf_hash, root};
use adk_auth::signed_audit::{
    AuditSigner, CheckpointBody, NoteVerifier, SignatureAlgorithm, canonicalize, sign_note,
};
use adk_core::audit_log::AuditHash;
use awp_types::LogEntry;

use super::SigningError;

/// Tree levels per tile.
pub const TILE_HEIGHT: u32 = 8;
/// Hashes or entries in a full tile.
pub const TILE_WIDTH: u64 = 1 << TILE_HEIGHT;
/// The log's path under a site's domain.
pub const LOG_PATH: &str = "/.well-known/awp/log/";

/// The checkpoint origin of a site: `{domain}/.well-known/awp/log`.
pub fn log_origin(domain: &str) -> String {
    format!("{domain}/.well-known/awp/log")
}

/// The bytes of a log entry: its JCS form.
///
/// # Errors
///
/// [`SigningError::Canonicalize`] when the entry is not I-JSON.
pub fn entry_bytes(entry: &LogEntry) -> Result<Vec<u8>, SigningError> {
    let value =
        serde_json::to_value(entry).map_err(|e| SigningError::Canonicalize(e.to_string()))?;
    Ok(canonicalize(&value).map_err(|e| SigningError::Canonicalize(e.to_string()))?.into_bytes())
}

/// The leaf hash of a log entry: `SHA-256(0x00 ‖ entry)`.
///
/// # Errors
///
/// As [`entry_bytes`].
pub fn entry_leaf(entry: &LogEntry) -> Result<AuditHash, SigningError> {
    Ok(leaf_hash(&entry_bytes(entry)?))
}

/// Encodes a tile index as C2SP path elements: `1234067` is `x001/x234/067`.
///
/// # Example
///
/// ```
/// use adk_awp::signing::log::tile_index_path;
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

/// The path, relative to the log, of hash tile `index` at `level` holding `width` hashes.
pub fn hash_tile_path(level: u32, index: u64, width: u64) -> String {
    let base = format!("tile/{level}/{}", tile_index_path(index));
    if width == TILE_WIDTH { base } else { format!("{base}.p/{width}") }
}

/// The path, relative to the log, of entry bundle `index` holding `width` entries.
pub fn entry_bundle_path(index: u64, width: u64) -> String {
    let base = format!("tile/entries/{}", tile_index_path(index));
    if width == TILE_WIDTH { base } else { format!("{base}.p/{width}") }
}

/// The level-0 tiles a tree of `size` leaves is read from, as `(index, width)`.
pub fn level0_tiles(size: u64) -> Vec<(u64, u64)> {
    (0..size.div_ceil(TILE_WIDTH))
        .map(|index| (index, (size - index * TILE_WIDTH).min(TILE_WIDTH)))
        .collect()
}

/// One tile of a log: an entry bundle (`level` `None`) or a hash tile of tile level `level`,
/// holding `width` entries or hashes from position `index × 256`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileRef {
    /// The tile level; `None` for an entry bundle.
    pub level: Option<u32>,
    /// The tile's index at its level.
    pub index: u64,
    /// Entries or hashes it holds, 1 to 256.
    pub width: u64,
}

impl TileRef {
    /// The tile's path relative to the log.
    pub fn path(&self) -> String {
        match self.level {
            None => entry_bundle_path(self.index, self.width),
            Some(level) => hash_tile_path(level, self.index, self.width),
        }
    }

    /// The positions it covers at its level: entries for a bundle, tree nodes of height
    /// `8 × level` for a hash tile.
    pub fn positions(&self) -> std::ops::Range<u64> {
        let start = self.index * TILE_WIDTH;
        start..start + self.width
    }
}

/// The tiles that change when a log grows from `old` to `new` entries, entry bundles first,
/// then hash tiles by level; `changed_tiles(0, n)` is every tile of a log of `n` entries.
///
/// # Example
///
/// ```
/// use adk_awp::signing::log::{TileRef, changed_tiles};
///
/// assert_eq!(
///     changed_tiles(0, 1),
///     [TileRef { level: None, index: 0, width: 1 }, TileRef { level: Some(0), index: 0, width: 1 }]
/// );
/// assert!(changed_tiles(5, 5).is_empty());
/// ```
pub fn changed_tiles(old: u64, new: u64) -> Vec<TileRef> {
    let mut tiles = Vec::new();
    if new <= old {
        return tiles;
    }
    let first = old / TILE_WIDTH;
    let last = (new - 1) / TILE_WIDTH;
    for index in first..=last {
        tiles.push(TileRef {
            level: None,
            index,
            width: (new - index * TILE_WIDTH).min(TILE_WIDTH),
        });
    }
    let mut level = 0;
    loop {
        let shift = level * TILE_HEIGHT;
        if shift >= u64::BITS {
            break;
        }
        let (old_count, new_count) = (old >> shift, new >> shift);
        if new_count == 0 {
            break;
        }
        if new_count > old_count {
            let first = old_count / TILE_WIDTH;
            let last = (new_count - 1) / TILE_WIDTH;
            for index in first..=last {
                let width = (new_count - index * TILE_WIDTH).min(TILE_WIDTH);
                tiles.push(TileRef { level: Some(level), index, width });
            }
        }
        level += 1;
    }
    tiles
}

/// Encodes log entries as an entry bundle: each prefixed by its length as a big-endian `u16`.
///
/// # Errors
///
/// [`SigningError::Canonicalize`] for an entry over 65,535 bytes.
pub fn encode_bundle(entries: &[Vec<u8>]) -> Result<Vec<u8>, SigningError> {
    let mut out = Vec::new();
    for entry in entries {
        let length = u16::try_from(entry.len()).map_err(|_| {
            SigningError::Canonicalize("a log entry exceeds 65,535 bytes".to_owned())
        })?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(entry);
    }
    Ok(out)
}

/// Splits an entry bundle into its entries.
///
/// # Errors
///
/// [`SigningError::Tile`] when the bundle is truncated.
///
/// # Example
///
/// ```
/// use adk_awp::signing::log::{decode_bundle, encode_bundle};
///
/// let entries = vec![b"one".to_vec(), Vec::new()];
/// assert_eq!(decode_bundle(&encode_bundle(&entries).unwrap()).unwrap(), entries);
/// assert!(decode_bundle(&[0, 5, 1]).is_err());
/// ```
pub fn decode_bundle(bundle: &[u8]) -> Result<Vec<Vec<u8>>, SigningError> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bundle.len() {
        let Some(prefix) = bundle.get(at..at + 2) else {
            return Err(SigningError::Tile("the entry bundle is truncated".to_owned()));
        };
        let length = usize::from(u16::from_be_bytes([prefix[0], prefix[1]]));
        at += 2;
        let Some(entry) = bundle.get(at..at + length) else {
            return Err(SigningError::Tile("the entry bundle is truncated".to_owned()));
        };
        entries.push(entry.to_vec());
        at += length;
    }
    Ok(entries)
}

/// The bytes of a hash tile: its hashes, 32 bytes each, in order.
pub fn hash_tile(hashes: &[AuditHash]) -> Vec<u8> {
    hashes.iter().flat_map(|hash| hash.0).collect()
}

/// Every tile and entry bundle of the log over `entries`, by path relative to the log.
///
/// # Errors
///
/// As [`entry_bytes`]; [`SigningError::Canonicalize`] for an entry over 65,535 bytes.
pub fn tiles(entries: &[LogEntry]) -> Result<BTreeMap<String, Vec<u8>>, SigningError> {
    let bytes = entries.iter().map(entry_bytes).collect::<Result<Vec<_>, _>>()?;
    let leaves: Vec<AuditHash> = bytes.iter().map(|entry| leaf_hash(entry)).collect();
    let mut files = BTreeMap::new();
    for tile in changed_tiles(0, leaves.len() as u64) {
        let positions = tile.positions();
        let content = match tile.level {
            None => encode_bundle(&bytes[positions.start as usize..positions.end as usize])?,
            // Level L holds the roots of the complete subtrees of 256^L leaves.
            Some(level) => {
                let span = 1usize << (level * TILE_HEIGHT);
                let nodes: Vec<AuditHash> = positions
                    .map(|node| {
                        let start = node as usize * span;
                        root(&leaves[start..start + span])
                    })
                    .collect();
                hash_tile(&nodes)
            }
        };
        files.insert(tile.path(), content);
    }
    Ok(files)
}

/// The checkpoint body of the log over `entries` for `domain`.
///
/// # Errors
///
/// As [`entry_leaf`].
pub fn checkpoint_body(domain: &str, entries: &[LogEntry]) -> Result<CheckpointBody, SigningError> {
    let leaves = entries.iter().map(entry_leaf).collect::<Result<Vec<_>, _>>()?;
    Ok(CheckpointBody {
        origin: log_origin(domain),
        size: leaves.len() as u64,
        root_hash: root(&leaves),
    })
}

/// Signs the checkpoint of the log over `entries` with `signer`, a note signer whose key name
/// is the log origin.
///
/// # Errors
///
/// [`SigningError::Key`] when the signer's name is not the origin or it cannot sign.
pub async fn signed_checkpoint(
    domain: &str,
    entries: &[LogEntry],
    signer: Arc<dyn AuditSigner>,
) -> Result<String, SigningError> {
    let body = checkpoint_body(domain, entries)?;
    if signer.key_name() != body.origin {
        return Err(SigningError::Key(format!(
            "the checkpoint key name must be the log origin `{}`",
            body.origin
        )));
    }
    sign_note(&body.to_text(), &[signer])
        .await
        .map_err(|error| SigningError::Key(error.to_string()))
}

/// The note verifier of a `release` key for `domain`'s checkpoints.
///
/// # Errors
///
/// [`SigningError::Key`] when the key is not 32 bytes.
pub fn checkpoint_verifier(domain: &str, public_key: &[u8]) -> Result<NoteVerifier, SigningError> {
    NoteVerifier::from_public_key(&log_origin(domain), SignatureAlgorithm::Ed25519, public_key)
        .map_err(|error| SigningError::Key(error.to_string()))
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
        let tile = |level, index, width| TileRef { level, index, width };
        let tiles = changed_tiles(255, 257);
        assert!(tiles.contains(&tile(None, 0, 256)));
        assert!(tiles.contains(&tile(None, 1, 1)));
        assert!(tiles.contains(&tile(Some(0), 1, 1)));
        assert!(tiles.contains(&tile(Some(1), 0, 1)));
        assert_eq!(tile(Some(1), 0, 1).positions(), 0..1);
    }

    #[test]
    fn bundles_round_trip() {
        let entries = vec![b"one".to_vec(), Vec::new(), vec![7; 300]];
        assert_eq!(decode_bundle(&encode_bundle(&entries).unwrap()).unwrap(), entries);
        assert!(decode_bundle(&[0, 5, 1]).is_err());
        assert!(decode_bundle(&[0]).is_err());
        assert!(encode_bundle(&[vec![0; 70_000]]).is_err());
    }
}
