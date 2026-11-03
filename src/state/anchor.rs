//! `ANC-` entries: a daily Merkle root over every agent's sealed-state head

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::govlog::Sha256Hex;
use crate::ids::GovernanceLogId;

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

/// One of an agent's heads in an `ANC-` entry, with the path from its leaf
/// to the entry's root (0.148). `GET /api/state/proofs` lists them.
///
/// Check it with [`AnchorProof::check`] against the [`AnchorData`] the
/// governance log itself gives for `anc_id`, never against `root` alone:
/// `root` is the server's copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AnchorProof {
    /// The `ANC-` entry
    pub anc_id: GovernanceLogId,
    /// Its [`AnchorData::date`]
    pub date: NaiveDate,
    /// Its [`AnchorData::root`], as the server has it
    pub root: Sha256Hex,
    /// The agent's head that day: a chain entry's hash
    pub entry_hash: Sha256Hex,
    /// That entry's seq
    pub seq: u64,
    /// The leaf's place among the sorted leaves
    pub leaf_index: u32,
    /// How many leaves: [`AnchorData::agents`]
    pub leaf_count: u32,
    /// Leaf first
    pub proof: InclusionProof,
}

#[cfg(feature = "sealed-state-types")]
impl AnchorProof {
    /// Whether this proof puts `agent_id`'s `entry_hash` under `anc`, the
    /// [`AnchorData`] the governance log gives for [`Self::anc_id`]: the
    /// dates and roots agree, and the path from
    /// [`anchor_leaf`]`(agent_id, entry_hash)` reaches `anc.root`
    pub fn check(
        &self,
        agent_id: crate::ids::AgentId,
        anc: &AnchorData,
    ) -> bool {
        anc.date == self.date
            && anc.root == self.root
            && verify_inclusion(
                &anchor_leaf(agent_id, &self.entry_hash),
                &self.proof,
                &anc.root,
            )
    }
}

#[cfg(feature = "sealed-state-types")]
pub use merkle::*;

#[cfg(feature = "sealed-state-types")]
mod merkle {
    use sha2::{Digest, Sha256};

    #[cfg(test)]
    use super::{AnchorData, AnchorProof};
    use super::{InclusionProof, ProofStep, Side};
    use crate::govlog::Sha256Hex;
    use crate::ids::AgentId;
    #[cfg(test)]
    use chrono::NaiveDate;

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

    /// [`inclusion_proof`] for every leaf, in order, building the tree
    /// once: what the server stores for a day's `ANC-` entry
    pub fn inclusion_proofs(leaves: &[Sha256Hex]) -> Vec<InclusionProof> {
        let mut proofs = vec![InclusionProof::new(); leaves.len()];
        let mut level = leaves.to_vec();
        // Which node of `level` each leaf is under
        let mut at: Vec<usize> = (0..leaves.len()).collect();
        while level.len() > 1 {
            for (proof, index) in proofs.iter_mut().zip(at.iter_mut()) {
                let sibling = *index ^ 1;
                if let Some(s) = level.get(sibling) {
                    proof.push(ProofStep {
                        sibling: *s,
                        side: if sibling < *index {
                            Side::Left
                        } else {
                            Side::Right
                        },
                    });
                }
                *index /= 2;
            }
            level = level_up(&level);
        }
        proofs
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

        fn agent(i: usize) -> AgentId {
            AgentId::from(uuid::Uuid::from_u128(i as u128 + 1))
        }

        fn entry(i: usize) -> Sha256Hex {
            Sha256Hex::from([i as u8 + 1; 32])
        }

        /// A day of `n` agents: its [`AnchorData`] and every proof
        fn day(n: usize, date: NaiveDate) -> (AnchorData, Vec<AnchorProof>) {
            let leaves: Vec<_> =
                (0..n).map(|i| anchor_leaf(agent(i), &entry(i))).collect();
            let anc = AnchorData {
                date,
                root: merkle_root(&leaves),
                agents: n as u32,
            };
            let proofs = inclusion_proofs(&leaves)
                .into_iter()
                .enumerate()
                .map(|(i, proof)| AnchorProof {
                    anc_id: "ANC-2026-0001".parse().unwrap(),
                    date,
                    root: anc.root,
                    entry_hash: entry(i),
                    seq: i as u64,
                    leaf_index: i as u32,
                    leaf_count: n as u32,
                    proof,
                })
                .collect();
            (anc, proofs)
        }

        #[test]
        fn inclusion_proofs_match_inclusion_proof() {
            for n in 0..=17usize {
                let leaves: Vec<_> =
                    (0..n).map(|i| anchor_leaf(agent(i), &entry(i))).collect();
                let all = inclusion_proofs(&leaves);
                assert_eq!(all.len(), n);
                for (i, proof) in all.iter().enumerate() {
                    assert_eq!(
                        Some(proof),
                        inclusion_proof(&leaves, i).as_ref(),
                        "{n}/{i}"
                    );
                }
            }
        }

        #[test]
        fn anchor_proofs_check_and_forgeries_fail() {
            let date = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
            let other_date = date.succ_opt().unwrap();
            for n in 1..=17usize {
                let (anc, proofs) = day(n, date);
                // Another day's entry, over different heads
                let (other, _) = day(n + 1, other_date);
                for (i, p) in proofs.iter().enumerate() {
                    assert!(p.check(agent(i), &anc), "{n}/{i}");
                    // Wrong agent
                    assert!(!p.check(agent(i + 100), &anc), "{n}/{i}");
                    // Wrong entry
                    let mut wrong = p.clone();
                    wrong.entry_hash = entry(i + 100);
                    assert!(!wrong.check(agent(i), &anc), "{n}/{i}");
                    // A tampered step
                    if let Some(step) = p.proof.first() {
                        let mut t = p.clone();
                        t.proof[0].sibling = Sha256Hex::from([0xee; 32]);
                        assert!(!t.check(agent(i), &anc), "{n}/{i}");
                        let mut t = p.clone();
                        t.proof[0].side = match step.side {
                            Side::Left => Side::Right,
                            Side::Right => Side::Left,
                        };
                        assert!(!t.check(agent(i), &anc), "{n}/{i}");
                    }
                    // A different ANC's root and date
                    assert!(!p.check(agent(i), &other), "{n}/{i}");
                    // The server's `root` and date rewritten to match it
                    let mut moved = p.clone();
                    moved.root = other.root;
                    moved.date = other.date;
                    assert!(!moved.check(agent(i), &other), "{n}/{i}");
                    // Same day, different root
                    let forged = AnchorData {
                        root: Sha256Hex::from([0xab; 32]),
                        ..anc.clone()
                    };
                    let mut lying = p.clone();
                    lying.root = forged.root;
                    assert!(!lying.check(agent(i), &forged), "{n}/{i}");
                    // The proof's root disagreeing with the log's
                    assert!(!lying.check(agent(i), &anc), "{n}/{i}");
                }
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
