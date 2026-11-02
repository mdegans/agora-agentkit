//! `ANC-` entries: a daily Merkle root over every agent's sealed-state head

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::govlog::Sha256Hex;

/// The `data` of an `anchor` governance log entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AnchorData {
    /// The UTC date the heads are as of, at its end
    pub date: NaiveDate,
    /// [`merkle_root`] over every head's [`anchor_leaf`], sorted by agent
    pub root: Sha256Hex,
    /// How many leaves
    pub agents: u32,
}

/// Which side of the path a [`ProofStep`]'s sibling is on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

/// One level of an [`InclusionProof`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct ProofStep {
    pub sibling: Sha256Hex,
    pub side: Side,
}

/// The path from a leaf to an [`AnchorData::root`], leaf first. A level
/// where the node had no sibling is skipped
pub type InclusionProof = Vec<ProofStep>;

#[cfg(feature = "sealed-state-types")]
pub use merkle::*;

#[cfg(feature = "sealed-state-types")]
mod merkle {
    use sha2::{Digest, Sha256};

    use super::{InclusionProof, ProofStep, Side};
    use crate::govlog::Sha256Hex;
    use crate::ids::AgentId;

    const LEAF: &[u8] = b"agora/anc-leaf/v1";
    const NODE: &[u8] = b"agora/anc-node/v1";
    const EMPTY: &[u8] = b"agora/anc-empty/v1";

    fn digest(parts: &[&[u8]]) -> Sha256Hex {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        Sha256Hex::from(<[u8; 32]>::from(h.finalize()))
    }

    /// `SHA-256("agora/anc-leaf/v1" ‖ agent_id ‖ head_hash)`
    pub fn anchor_leaf(agent_id: AgentId, head_hash: &Sha256Hex) -> Sha256Hex {
        digest(&[LEAF, agent_id.as_uuid().as_bytes(), head_hash.as_bytes()])
    }

    fn node(left: &Sha256Hex, right: &Sha256Hex) -> Sha256Hex {
        digest(&[NODE, left.as_bytes(), right.as_bytes()])
    }

    /// The next level up: pairs hashed, an odd last node carried
    fn level_up(level: &[Sha256Hex]) -> Vec<Sha256Hex> {
        level
            .chunks(2)
            .map(|pair| match pair {
                [l, r] => node(l, r),
                [one] => *one,
                _ => unreachable!("chunks(2)"),
            })
            .collect()
    }

    /// The root over `leaves`, in the order given
    pub fn merkle_root(leaves: &[Sha256Hex]) -> Sha256Hex {
        if leaves.is_empty() {
            return digest(&[EMPTY]);
        }
        let mut level = leaves.to_vec();
        while level.len() > 1 {
            level = level_up(&level);
        }
        level[0]
    }

    /// The path from `leaves[index]` to the root; `None` out of range
    pub fn inclusion_proof(
        leaves: &[Sha256Hex],
        mut index: usize,
    ) -> Option<InclusionProof> {
        if index >= leaves.len() {
            return None;
        }
        let mut proof = Vec::new();
        let mut level = leaves.to_vec();
        while level.len() > 1 {
            let sibling = index ^ 1;
            if let Some(s) = level.get(sibling) {
                proof.push(ProofStep {
                    sibling: *s,
                    side: if sibling < index {
                        Side::Left
                    } else {
                        Side::Right
                    },
                });
            }
            level = level_up(&level);
            index /= 2;
        }
        Some(proof)
    }

    /// Whether `proof` takes `leaf` to `root`
    pub fn verify_inclusion(
        leaf: &Sha256Hex,
        proof: &[ProofStep],
        root: &Sha256Hex,
    ) -> bool {
        let got = proof.iter().fold(*leaf, |acc, step| match step.side {
            Side::Left => node(&step.sibling, &acc),
            Side::Right => node(&acc, &step.sibling),
        });
        got == *root
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_leaf_proves_into_the_root_for_every_size() {
            for n in 1..=17usize {
                let leaves: Vec<Sha256Hex> = (0..n)
                    .map(|i| {
                        anchor_leaf(
                            AgentId::from(uuid::Uuid::from_u128(i as u128)),
                            &Sha256Hex::from([i as u8; 32]),
                        )
                    })
                    .collect();
                let root = merkle_root(&leaves);
                for (i, leaf) in leaves.iter().enumerate() {
                    let proof = inclusion_proof(&leaves, i).unwrap();
                    assert!(verify_inclusion(leaf, &proof, &root), "{n}/{i}");
                    let other = leaves[(i + 1) % n];
                    if n > 1 {
                        assert!(!verify_inclusion(&other, &proof, &root));
                    }
                }
                assert!(inclusion_proof(&leaves, n).is_none());
            }
        }

        #[test]
        fn the_leaf_binds_agent_and_head() {
            let a = AgentId::from(uuid::Uuid::from_u128(1));
            let b = AgentId::from(uuid::Uuid::from_u128(2));
            let h = Sha256Hex::from([9; 32]);
            assert_ne!(anchor_leaf(a, &h), anchor_leaf(b, &h));
            assert_ne!(
                anchor_leaf(a, &h),
                anchor_leaf(a, &Sha256Hex::from([8; 32]))
            );
            assert_ne!(merkle_root(&[]), merkle_root(&[anchor_leaf(a, &h)]));
        }
    }
}
