//! RFC 9162 inclusion and consistency proofs verify for every position and size, and
//! fail when a proof, leaf, or root is altered.

#![cfg(feature = "signed-audit")]

use adk_auth::signed_audit::merkle::{
    consistency_proof, inclusion_proof, leaf_hash, root, verify_consistency, verify_inclusion,
};
use adk_core::audit_log::AuditHash;
use proptest::prelude::*;

fn leaves(count: usize, salt: u64) -> Vec<AuditHash> {
    (0..count as u64).map(|i| leaf_hash(&(i ^ salt).to_be_bytes())).collect()
}

fn flip(hash: &AuditHash, bit: usize) -> AuditHash {
    let mut bytes = hash.0;
    bytes[(bit / 8) % 32] ^= 1 << (bit % 8);
    AuditHash(bytes)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn inclusion_proofs_verify_and_detect_tampering(
        size in 1usize..300,
        index_seed in any::<u64>(),
        salt in any::<u64>(),
        bit in 0usize..256,
        pick in any::<prop::sample::Index>(),
    ) {
        let tree = leaves(size, salt);
        let index = index_seed % size as u64;
        let tree_root = root(&tree);
        let proof = inclusion_proof(&tree, index).unwrap();
        prop_assert!(verify_inclusion(index, size as u64, &tree[index as usize], &proof, &tree_root).is_ok());

        let wrong_leaf = flip(&tree[index as usize], bit);
        prop_assert!(verify_inclusion(index, size as u64, &wrong_leaf, &proof, &tree_root).is_err());
        prop_assert!(verify_inclusion(index, size as u64, &tree[index as usize], &proof, &flip(&tree_root, bit)).is_err());
        if !proof.is_empty() {
            let mut bad = proof.clone();
            let at = pick.index(bad.len());
            bad[at] = flip(&bad[at], bit);
            prop_assert!(verify_inclusion(index, size as u64, &tree[index as usize], &bad, &tree_root).is_err());
            bad.pop();
            prop_assert!(verify_inclusion(index, size as u64, &tree[index as usize], &bad, &tree_root).is_err());
        }
        let mut long = proof.clone();
        long.push(tree_root);
        prop_assert!(verify_inclusion(index, size as u64, &tree[index as usize], &long, &tree_root).is_err());
        if size > 1 {
            let other = (index + 1) % size as u64;
            if tree[other as usize] != tree[index as usize] {
                prop_assert!(verify_inclusion(other, size as u64, &tree[index as usize], &proof, &tree_root).is_err());
            }
        }
    }

    #[test]
    fn consistency_proofs_verify_and_detect_tampering(
        new in 1usize..300,
        old_seed in any::<u64>(),
        salt in any::<u64>(),
        bit in 0usize..256,
        pick in any::<prop::sample::Index>(),
    ) {
        let tree = leaves(new, salt);
        let old = (old_seed % (new as u64 + 1)) as usize;
        let old_root = root(&tree[..old]);
        let new_root = root(&tree);
        let proof = consistency_proof(&tree, old as u64).unwrap();
        prop_assert!(verify_consistency(old as u64, new as u64, &old_root, &new_root, &proof).is_ok());

        if old > 0 && old < new {
            prop_assert!(verify_consistency(old as u64, new as u64, &flip(&old_root, bit), &new_root, &proof).is_err());
            prop_assert!(verify_consistency(old as u64, new as u64, &old_root, &flip(&new_root, bit), &proof).is_err());
            let mut bad = proof.clone();
            let at = pick.index(bad.len());
            bad[at] = flip(&bad[at], bit);
            prop_assert!(verify_consistency(old as u64, new as u64, &old_root, &new_root, &bad).is_err());
            bad.pop();
            prop_assert!(verify_consistency(old as u64, new as u64, &old_root, &new_root, &bad).is_err());

            // A tree that rewrote any of the first `old` leaves is not consistent.
            let mut forked = tree.clone();
            let at = pick.index(old);
            forked[at] = flip(&forked[at], bit);
            let forked_proof = consistency_proof(&forked, old as u64).unwrap();
            prop_assert!(verify_consistency(old as u64, new as u64, &old_root, &root(&forked), &forked_proof).is_err());
        }
    }
}
