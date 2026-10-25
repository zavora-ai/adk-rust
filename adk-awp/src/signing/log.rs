//! The site log: an RFC 9162 Merkle tree of `{seq, manifest}` entries, published as C2SP tiles
//! with a signed-note checkpoint (spec 3.5 §7.7; AWP 2.0 §14.5, C17).
//!
//! The entry of release `n` is the JCS form of `{"seq": n, "manifest": <digest>}` at leaf
//! `n − 1`. Tiles hold 256 hashes of one tree level (`tile/<L>/<N>`, partial tiles
//! `tile/<L>/<N>.p/<W>`); entry bundles hold 256 entries (`tile/entries/<N>`), each prefixed by
//! its length as a big-endian `u16`. The checkpoint is signed by a delegated `release` key
//! whose note key name is the origin `{domain}/.well-known/awp/log`.

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

/// Every tile and entry bundle of the log over `entries`, by path relative to the log.
///
/// # Errors
///
/// As [`entry_bytes`]; [`SigningError::Canonicalize`] for an entry over 65,535 bytes.
pub fn tiles(entries: &[LogEntry]) -> Result<BTreeMap<String, Vec<u8>>, SigningError> {
    let mut files = BTreeMap::new();
    let mut leaves = Vec::with_capacity(entries.len());
    let mut bundles: Vec<Vec<u8>> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let bytes = entry_bytes(entry)?;
        let length = u16::try_from(bytes.len()).map_err(|_| {
            SigningError::Canonicalize("a log entry exceeds 65,535 bytes".to_owned())
        })?;
        if (index as u64).is_multiple_of(TILE_WIDTH) {
            bundles.push(Vec::new());
        }
        if let Some(bundle) = bundles.last_mut() {
            bundle.extend_from_slice(&length.to_be_bytes());
            bundle.extend_from_slice(&bytes);
        }
        leaves.push(leaf_hash(&bytes));
    }
    let size = leaves.len() as u64;
    for (index, bundle) in bundles.into_iter().enumerate() {
        let width = (size - index as u64 * TILE_WIDTH).min(TILE_WIDTH);
        files.insert(entry_bundle_path(index as u64, width), bundle);
    }
    // Level L holds the roots of the complete subtrees of 256^L leaves.
    let mut level = 0u32;
    let mut span = 1u64;
    while span <= size {
        let nodes: Vec<AuditHash> = (0..size / span)
            .map(|node| {
                let start = (node * span) as usize;
                root(&leaves[start..start + span as usize])
            })
            .collect();
        for (index, chunk) in nodes.chunks(TILE_WIDTH as usize).enumerate() {
            let bytes: Vec<u8> = chunk.iter().flat_map(|hash| hash.0).collect();
            files.insert(hash_tile_path(level, index as u64, chunk.len() as u64), bytes);
        }
        level += 1;
        span = span.saturating_mul(TILE_WIDTH);
        if span == u64::MAX {
            break;
        }
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
