//! RFC 9162 Merkle tree hashing, inclusion proofs, and consistency proofs.
//!
//! Leaves hash as `SHA-256(0x00 ‖ data)` and interior nodes as
//! `SHA-256(0x01 ‖ left ‖ right)`. A tree of `n` leaves splits at the largest power of
//! two below `n`, so every left subtree is complete and its hash can be stored once.

use std::ops::Range;

use adk_core::audit_log::{AuditHash, AuditTreeNode};
use sha2::{Digest, Sha256};

/// A proof that does not verify.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProofError {
    /// The positions or sizes the proof is for are impossible.
    #[error("invalid proof request: {0}")]
    InvalidRequest(String),
    /// The proof has the wrong number of hashes for its positions.
    #[error("proof has the wrong length for its tree sizes")]
    WrongLength,
    /// The proof does not lead to the expected root.
    #[error("proof does not match the root hash")]
    RootMismatch,
}

/// Hashes a leaf: `SHA-256(0x00 ‖ data)`.
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::merkle::leaf_hash;
///
/// assert_eq!(
///     leaf_hash(b"").to_string(),
///     "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
/// );
/// ```
pub fn leaf_hash(data: &[u8]) -> AuditHash {
    let mut hasher = Sha256::new();
    hasher.update([0x00]);
    hasher.update(data);
    AuditHash(hasher.finalize().into())
}

/// Hashes an interior node: `SHA-256(0x01 ‖ left ‖ right)`.
pub fn node_hash(left: &AuditHash, right: &AuditHash) -> AuditHash {
    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(left.0);
    hasher.update(right.0);
    AuditHash(hasher.finalize().into())
}

/// The root of the empty tree: `SHA-256("")`.
pub fn empty_root() -> AuditHash {
    AuditHash(Sha256::digest(b"").into())
}

/// The largest power of two strictly below `n` (`n` ≥ 2).
fn split_point(n: u64) -> u64 {
    1 << (63 - (n - 1).leading_zeros())
}

/// The complete subtrees `(level, index)` that make up `range`, left to right.
///
/// `range.start` must be a multiple of the largest power of two not exceeding the
/// range's length, which every range in an RFC 9162 split satisfies.
pub(crate) fn range_nodes(range: Range<u64>) -> Vec<(u8, u64)> {
    let mut nodes = Vec::new();
    let mut start = range.start;
    while start < range.end {
        let remaining = range.end - start;
        let mut level = 63 - remaining.leading_zeros();
        if start != 0 {
            level = level.min(start.trailing_zeros());
        }
        nodes.push((level as u8, start >> level));
        start += 1 << level;
    }
    nodes
}

/// Folds the hashes of [`range_nodes`] into the range's RFC 9162 subtree hash.
pub(crate) fn fold_range(hashes: &[AuditHash]) -> AuditHash {
    let mut iter = hashes.iter().rev();
    let Some(last) = iter.next() else {
        return empty_root();
    };
    iter.fold(*last, |right, left| node_hash(left, &right))
}

/// The subtree ranges whose hashes form the inclusion proof for `index` in a tree of
/// `size` leaves, in proof order (RFC 9162 section 2.1.3.1).
pub(crate) fn inclusion_ranges(index: u64, size: u64) -> Vec<Range<u64>> {
    fn path(index: u64, range: Range<u64>, out: &mut Vec<Range<u64>>) {
        let n = range.end - range.start;
        if n <= 1 {
            return;
        }
        let k = split_point(n);
        let mid = range.start + k;
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

/// The subtree ranges whose hashes form the consistency proof from `old` to `new`
/// leaves, in proof order (RFC 9162 section 2.1.4.1).
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

fn subtree_root(leaves: &[AuditHash], range: Range<u64>) -> AuditHash {
    let n = range.end - range.start;
    match n {
        0 => empty_root(),
        1 => leaves[range.start as usize],
        _ => {
            let mid = range.start + split_point(n);
            node_hash(
                &subtree_root(leaves, range.start..mid),
                &subtree_root(leaves, mid..range.end),
            )
        }
    }
}

/// The RFC 9162 root hash over `leaves` (leaf hashes, not data).
///
/// # Example
///
/// ```rust
/// use adk_auth::signed_audit::merkle::{empty_root, root};
///
/// assert_eq!(root(&[]), empty_root());
/// ```
pub fn root(leaves: &[AuditHash]) -> AuditHash {
    subtree_root(leaves, 0..leaves.len() as u64)
}

/// The inclusion proof for leaf `index` in the tree over `leaves`.
///
/// # Errors
///
/// [`ProofError::InvalidRequest`] when `index` is not a leaf.
pub fn inclusion_proof(leaves: &[AuditHash], index: u64) -> Result<Vec<AuditHash>, ProofError> {
    let size = leaves.len() as u64;
    if index >= size {
        return Err(ProofError::InvalidRequest(format!("leaf {index} is not in a tree of {size}")));
    }
    Ok(inclusion_ranges(index, size).into_iter().map(|range| subtree_root(leaves, range)).collect())
}

/// The consistency proof from the tree over the first `old` leaves to the tree over
/// all `leaves`.
///
/// # Errors
///
/// [`ProofError::InvalidRequest`] when `old` exceeds the number of leaves.
pub fn consistency_proof(leaves: &[AuditHash], old: u64) -> Result<Vec<AuditHash>, ProofError> {
    let new = leaves.len() as u64;
    if old > new {
        return Err(ProofError::InvalidRequest(format!("size {old} exceeds the tree's {new}")));
    }
    Ok(consistency_ranges(old, new).into_iter().map(|range| subtree_root(leaves, range)).collect())
}

/// Verifies that `leaf` is at `index` in the tree of `size` leaves with root `root`
/// (RFC 9162 section 2.1.3.2).
///
/// # Errors
///
/// [`ProofError`] describing why the proof does not verify.
pub fn verify_inclusion(
    index: u64,
    size: u64,
    leaf: &AuditHash,
    proof: &[AuditHash],
    root: &AuditHash,
) -> Result<(), ProofError> {
    if index >= size {
        return Err(ProofError::InvalidRequest(format!("leaf {index} is not in a tree of {size}")));
    }
    let mut fn_ = index;
    let mut sn = size - 1;
    let mut r = *leaf;
    for p in proof {
        if sn == 0 {
            return Err(ProofError::WrongLength);
        }
        if fn_ & 1 == 1 || fn_ == sn {
            r = node_hash(p, &r);
            while fn_ & 1 == 0 && fn_ != 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            r = node_hash(&r, p);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    if sn != 0 {
        return Err(ProofError::WrongLength);
    }
    if r != *root {
        return Err(ProofError::RootMismatch);
    }
    Ok(())
}

/// Verifies that the tree of `new_size` leaves with root `new_root` extends the tree of
/// `old_size` leaves with root `old_root` (RFC 9162 section 2.1.4.2).
///
/// # Errors
///
/// [`ProofError`] describing why the proof does not verify.
pub fn verify_consistency(
    old_size: u64,
    new_size: u64,
    old_root: &AuditHash,
    new_root: &AuditHash,
    proof: &[AuditHash],
) -> Result<(), ProofError> {
    if old_size > new_size {
        return Err(ProofError::InvalidRequest(format!(
            "old size {old_size} exceeds new size {new_size}"
        )));
    }
    if old_size == new_size {
        if !proof.is_empty() {
            return Err(ProofError::WrongLength);
        }
        return if old_root == new_root { Ok(()) } else { Err(ProofError::RootMismatch) };
    }
    if old_size == 0 {
        return if proof.is_empty() { Ok(()) } else { Err(ProofError::WrongLength) };
    }
    if proof.is_empty() {
        return Err(ProofError::WrongLength);
    }
    let mut path: Vec<AuditHash> = Vec::with_capacity(proof.len() + 1);
    if old_size.is_power_of_two() {
        path.push(*old_root);
    }
    path.extend_from_slice(proof);
    let mut fn_ = old_size - 1;
    let mut sn = new_size - 1;
    while fn_ & 1 == 1 {
        fn_ >>= 1;
        sn >>= 1;
    }
    let mut fr = path[0];
    let mut sr = path[0];
    for c in &path[1..] {
        if sn == 0 {
            return Err(ProofError::WrongLength);
        }
        if fn_ & 1 == 1 || fn_ == sn {
            fr = node_hash(c, &fr);
            sr = node_hash(c, &sr);
            while fn_ & 1 == 0 && fn_ != 0 {
                fn_ >>= 1;
                sn >>= 1;
            }
        } else {
            sr = node_hash(&sr, c);
        }
        fn_ >>= 1;
        sn >>= 1;
    }
    if sn != 0 {
        return Err(ProofError::WrongLength);
    }
    if fr != *old_root || sr != *new_root {
        return Err(ProofError::RootMismatch);
    }
    Ok(())
}

/// The right edge of a tree: one complete-subtree hash per set bit of its size, largest
/// first. Appending a leaf returns the subtree nodes it completes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Frontier {
    pub(crate) size: u64,
    pub(crate) hashes: Vec<AuditHash>,
}

impl Frontier {
    /// Appends a leaf hash and returns the nodes (level ≥ 1) it completes.
    pub(crate) fn push(&mut self, leaf: AuditHash) -> Vec<AuditTreeNode> {
        let mut completed = Vec::new();
        let mut node = leaf;
        let mut index = self.size;
        let mut level = 0u8;
        while index & 1 == 1 {
            let left = self.hashes.pop().expect("a set bit has a frontier hash");
            node = node_hash(&left, &node);
            level += 1;
            index >>= 1;
            completed.push(AuditTreeNode { level, index, hash: node });
        }
        self.hashes.push(node);
        self.size += 1;
        completed
    }

    pub(crate) fn root(&self) -> AuditHash {
        fold_range(&self.hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> AuditHash {
        text.parse().unwrap()
    }

    /// Leaf inputs from transparency-dev/merkle `testonly.LeafInputs`.
    fn reference_leaves() -> Vec<AuditHash> {
        [
            "",
            "00",
            "10",
            "2021",
            "3031",
            "40414243",
            "5051525354555657",
            "606162636465666768696a6b6c6d6e6f",
        ]
        .iter()
        .map(|text| {
            let bytes: Vec<u8> = (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
                .collect();
            leaf_hash(&bytes)
        })
        .collect()
    }

    /// Roots from transparency-dev/merkle `testonly.RootHashes` (RFC 6962 vectors).
    #[test]
    fn reference_roots() {
        let roots = [
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            "fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125",
            "aeb6bcfe274b70a14fb067a5e5578264db0fa9b51af5e0ba159158f329e06e77",
            "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
            "4e3bbb1f7b478dcfe71fb631631519a3bca12c9aefca1612bfce4c13a86264d4",
            "76e67dadbcdf1e10e1b74ddc608abd2f98dfb16fbce75277b5232a127f2087ef",
            "ddb89be403809e325750d3d263cd78929c2942b7942a34b77e122c9594a74c8c",
            "5dc9da79a70659a9ad559cb701ded9a2ab9d823aad2f4960cfe370eff4604328",
        ];
        let leaves = reference_leaves();
        let mut frontier = Frontier::default();
        assert_eq!(frontier.root(), hex(roots[0]));
        for (size, expected) in roots.iter().enumerate() {
            assert_eq!(root(&leaves[..size]), hex(expected), "size {size}");
            if size > 0 {
                frontier.push(leaves[size - 1]);
                assert_eq!(frontier.root(), hex(expected), "frontier size {size}");
            }
        }
    }

    /// Inclusion proofs from transparency-dev/merkle `TestRefInclusionProof`.
    #[test]
    fn reference_inclusion_proofs() {
        let cases: &[(u64, u64, &[&str])] = &[
            (0, 1, &[]),
            (0, 2, &["96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7"]),
            (1, 2, &["6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d"]),
            (2, 3, &["fac54203e7cc696cf0dfcb42c92a1d9dbaf70ad9e621f4bd8d98662f00e3c125"]),
            (
                1,
                5,
                &[
                    "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
                    "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
                    "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
                ],
            ),
            (
                0,
                8,
                &[
                    "96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
                    "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
                    "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4",
                ],
            ),
            (
                5,
                8,
                &[
                    "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
                    "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
                    "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
                ],
            ),
        ];
        let leaves = reference_leaves();
        for (index, size, expected) in cases {
            let tree = &leaves[..*size as usize];
            let proof = inclusion_proof(tree, *index).unwrap();
            let expected: Vec<AuditHash> = expected.iter().map(|h| hex(h)).collect();
            assert_eq!(proof, expected, "{index}:{size}");
            verify_inclusion(*index, *size, &leaves[*index as usize], &proof, &root(tree)).unwrap();
        }
    }

    /// Consistency proofs from transparency-dev/merkle `TestRefConsistencyProof`.
    #[test]
    fn reference_consistency_proofs() {
        let cases: &[(u64, u64, &[&str])] = &[
            (1, 1, &[]),
            (
                1,
                8,
                &[
                    "96a296d224f285c67bee93c30f8a309157f0daa35dc5b87e410b78630a09cfc7",
                    "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
                    "6b47aaf29ee3c2af9af889bc1fb9254dabd31177f16232dd6aab035ca39bf6e4",
                ],
            ),
            (
                2,
                5,
                &[
                    "5f083f0a1a33ca076a95279832580db3e0ef4584bdff1f54c8a360f50de3031e",
                    "bc1a0643b12e4d2d7c77918f44e0f4f79a838b6cf9ec5b5c283e1f4d88599e6b",
                ],
            ),
            (
                6,
                8,
                &[
                    "0ebc5d3437fbe2db158b9f126a1d118e308181031d0a949f8dededebc558ef6a",
                    "ca854ea128ed050b41b35ffc1b87b8eb2bde461e9e3b5596ece6b9d5975a0ae0",
                    "d37ee418976dd95753c1c73862b9398fa2a2cf9b4ff0fdfe8b30cd95209614b7",
                ],
            ),
        ];
        let leaves = reference_leaves();
        for (old, new, expected) in cases {
            let tree = &leaves[..*new as usize];
            let proof = consistency_proof(tree, *old).unwrap();
            let expected: Vec<AuditHash> = expected.iter().map(|h| hex(h)).collect();
            assert_eq!(proof, expected, "{old}:{new}");
            verify_consistency(*old, *new, &root(&leaves[..*old as usize]), &root(tree), &proof)
                .unwrap();
        }
    }

    #[test]
    fn range_nodes_decompose_into_complete_subtrees() {
        assert_eq!(range_nodes(0..7), vec![(2, 0), (1, 2), (0, 6)]);
        assert_eq!(range_nodes(4..7), vec![(1, 2), (0, 6)]);
        assert_eq!(range_nodes(8..16), vec![(3, 1)]);
        assert_eq!(range_nodes(0..0), vec![]);
    }
}
