//! [`verify_chain`] and [`head_status`]: what the runner checks itself,
//! never on the server's word

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::chain::{
    BlobEncoding, STATE_ENTRY_VERSION, SignedStateEntry, StateEntry,
};
use super::keys::{KeyRotation, RotationError};
use crate::crypto::VerifyingKey;
use crate::enums::StateEntryKind;
use crate::govlog::{PublicKeyHex, Sha256Hex};
use crate::ids::AgentId;

/// The signing keys a runner trusts for one agent: its first key, and each
/// rotation in turn that the key current at that point endorsed. A key is
/// trusted for entries made before it was rotated away, and never after
#[derive(Debug, Clone)]
pub struct TrustedKeys {
    /// In order; each key with the last entry it may sign
    epochs: Vec<(VerifyingKey, Until)>,
    /// Keys known but not trusted: an operator's replacement, a refused
    /// rotation's, a key the server reports. An entry they signed is
    /// [`ChainProblem::UntrustedKey`]
    untrusted: Vec<VerifyingKey>,
    notices: Vec<KeyNotice>,
}

/// How far down the main line a key is trusted
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Until {
    /// The current key
    Current,
    /// Rotated away after this entry; `None` if it signed none
    Through(Option<HeadRef>),
}

/// Something about the keys the agent and operator should hear
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "notice", rename_all = "snake_case")]
pub enum KeyNotice {
    /// The operator replaced a key: not the old key's word
    OperatorRotation { new_key: PublicKeyHex },
    /// A rotation that does not verify, is out of sequence, or was
    /// endorsed by a key that was not current at that point
    RotationRefused {
        new_key: PublicKeyHex,
        reason: String,
    },
    /// The server reports a key the runner doesn't trust
    ServerKeyUnknown { key: PublicKeyHex },
}

impl TrustedKeys {
    /// The agent's first key, as the runner recorded it
    pub fn new(first: VerifyingKey) -> Self {
        Self {
            epochs: vec![(first, Until::Current)],
            untrusted: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Follow `rotations` in `seq` order. Each must be the next in
    /// sequence, later than the last, from the key current at that point,
    /// and endorsed by it. The first that isn't stops the walk: every
    /// rotation after it is refused too
    pub fn with_rotations(mut self, rotations: &[KeyRotation]) -> Self {
        let mut sorted: Vec<&KeyRotation> = rotations.iter().collect();
        sorted.sort_by_key(|r| r.seq);
        let mut broken = false;
        for r in sorted {
            let refuse = |this: &mut Self, reason: String| {
                this.notices.push(KeyNotice::RotationRefused {
                    new_key: r.new_public_key,
                    reason,
                });
            };
            let next = (self.epochs.len() - 1) as u32;
            let (current, _) = *self.epochs.last().expect("never empty");
            let reason = match r.verify_agent_signed() {
                Err(RotationError::OperatorAuthority) => {
                    self.notices.push(KeyNotice::OperatorRotation {
                        new_key: r.new_public_key,
                    });
                    None
                }
                Err(e) => Some(e.to_string()),
                Ok(()) if broken => {
                    Some("an earlier rotation was refused".into())
                }
                Ok(()) if r.seq != next => Some(format!(
                    "rotation {} out of sequence; {next} is next",
                    r.seq
                )),
                Ok(()) if r.old_public_key != PublicKeyHex::from(&current) => {
                    Some("endorsed by a key that was not current".into())
                }
                Ok(()) if self.before_last_rotation(r.old_key_last) => {
                    Some("placed before the rotation before it".into())
                }
                Ok(()) => {
                    match r.new_public_key.to_verifying_key() {
                        Ok(new) => {
                            self.epochs.last_mut().expect("never empty").1 =
                                Until::Through(r.old_key_last);
                            self.epochs.push((new, Until::Current));
                        }
                        Err(_) => broken = true,
                    }
                    continue;
                }
            };
            broken = true;
            if let Some(reason) = reason {
                refuse(&mut self, reason);
            }
            if let Ok(new) = r.new_public_key.to_verifying_key()
                && !self.trusts(&new)
            {
                self.untrusted.push(new);
            }
        }
        self
    }

    /// Whether `last` is earlier on the line than the previous rotation's
    fn before_last_rotation(&self, last: Option<HeadRef>) -> bool {
        let previous = self.epochs.iter().rev().find_map(|(_, u)| match u {
            Until::Through(Some(h)) => Some(h.seq),
            _ => None,
        });
        match (previous, last) {
            (Some(p), Some(l)) => l.seq < p,
            (Some(_), None) | (None, _) => false,
        }
    }

    /// The key the server reports: advisory, a notice if not current
    pub fn with_server_key(mut self, key: VerifyingKey) -> Self {
        if key != self.current() {
            self.notices.push(KeyNotice::ServerKeyUnknown {
                key: PublicKeyHex::from(&key),
            });
            if !self.trusts(&key) {
                self.untrusted.push(key);
            }
        }
        self
    }

    /// The key at the end of the followed rotations
    pub fn current(&self) -> VerifyingKey {
        self.epochs.last().expect("never empty").0
    }

    /// Whether `key` is trusted for any entry
    pub fn trusts(&self, key: &VerifyingKey) -> bool {
        self.epochs.iter().any(|(k, _)| k == key)
    }

    /// What the keys alone raised
    pub fn notices(&self) -> &[KeyNotice] {
        &self.notices
    }
}

pub use super::chain::HeadRef;

/// What is wrong with a chain
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum ChainProblem {
    /// The first entry is not a genesis at seq 0
    MissingGenesis,
    /// A genesis anywhere but first
    GenesisNotFirst {
        seq: u64,
    },
    /// An entry for another agent
    WrongAgent {
        seq: u64,
    },
    /// A domain tag this build doesn't know
    UnknownVersion {
        seq: u64,
        version: u32,
    },
    SeqGap {
        expected: u64,
        found: u64,
    },
    /// Two entries at one seq
    Fork {
        seq: u64,
    },
    /// `prev_hash` is not the previous entry's hash
    BrokenLink {
        seq: u64,
    },
    /// No known key made the signature
    BadSignature {
        seq: u64,
    },
    /// A known but untrusted key made it
    UntrustedKey {
        seq: u64,
    },
    /// A key made it after it was rotated away, or after a later key
    /// had signed
    RetiredKey {
        seq: u64,
    },
    /// A kind v1 doesn't write
    UnknownKind {
        seq: u64,
        kind: StateEntryKind,
    },
    /// A delta whose base is not an earlier keyframe of this chain
    DeltaWithoutBase {
        seq: u64,
    },
    /// A snapshot or archived copy without a blob
    MissingBlob {
        seq: u64,
    },
    /// `merged_from` set, which v1 doesn't allow
    MergeNotAllowed {
        seq: u64,
    },
    /// `created_at` earlier than the previous entry's
    TimeWentBack {
        seq: u64,
    },
}

/// The verdict of [`verify_chain`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainReport {
    /// No problems
    pub intact: bool,
    pub count: usize,
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    pub head: Option<HeadRef>,
    pub problems: Vec<ChainProblem>,
    /// From the keys, whether or not an entry is affected
    pub notices: Vec<KeyNotice>,
}

/// Check `entries` (in seq order) as one agent's main line: links, seqs,
/// signatures under `trusted`, kinds and delta bases
pub fn verify_chain(
    agent_id: AgentId,
    entries: &[SignedStateEntry],
    trusted: &TrustedKeys,
) -> ChainReport {
    let mut problems = Vec::new();
    let mut keyframes: HashSet<Sha256Hex> = HashSet::new();
    let mut prev: Option<(&StateEntry, Sha256Hex)> = None;
    // The latest key epoch an entry was signed in: no going back
    let mut epoch_seen = 0usize;

    match entries.first() {
        Some(first)
            if first.entry.seq == 0
                && first.entry.kind == StateEntryKind::Genesis
                && first.entry.prev_hash.is_none() => {}
        Some(_) => problems.push(ChainProblem::MissingGenesis),
        None => {}
    }

    for signed in entries {
        let e = &signed.entry;
        let seq = e.seq;
        let hash = e.entry_hash();
        if e.agent_id != agent_id {
            problems.push(ChainProblem::WrongAgent { seq });
        }
        if e.agora_state_entry != STATE_ENTRY_VERSION {
            problems.push(ChainProblem::UnknownVersion {
                seq,
                version: e.agora_state_entry,
            });
        }
        if !e.kind.is_v1() {
            problems.push(ChainProblem::UnknownKind { seq, kind: e.kind });
        }
        if e.kind == StateEntryKind::Genesis && prev.is_some() {
            problems.push(ChainProblem::GenesisNotFirst { seq });
        }
        if !e.merged_from.is_empty() {
            problems.push(ChainProblem::MergeNotAllowed { seq });
        }
        if let Some((p, p_hash)) = prev {
            if seq == p.seq {
                problems.push(ChainProblem::Fork { seq });
            } else if seq != p.seq + 1 {
                problems.push(ChainProblem::SeqGap {
                    expected: p.seq + 1,
                    found: seq,
                });
            }
            if e.prev_hash != Some(p_hash) {
                problems.push(ChainProblem::BrokenLink { seq });
            }
            if e.created_at < p.created_at {
                problems.push(ChainProblem::TimeWentBack { seq });
            }
        }
        match &e.blob {
            None if matches!(
                e.kind,
                StateEntryKind::Snapshot | StateEntryKind::ArchivedPrior
            ) =>
            {
                problems.push(ChainProblem::MissingBlob { seq })
            }
            Some(blob) => match blob.encoding {
                BlobEncoding::Keyframe => {
                    keyframes.insert(hash);
                }
                BlobEncoding::Delta { base } => {
                    if !keyframes.contains(&base) {
                        problems.push(ChainProblem::DeltaWithoutBase { seq });
                    }
                }
            },
            None => {}
        }
        match trusted.epochs.iter().position(|(k, _)| signed.verify(k)) {
            Some(epoch) => {
                let past = match trusted.epochs[epoch].1 {
                    Until::Current => false,
                    Until::Through(None) => true,
                    Until::Through(Some(last)) => {
                        seq > last.seq
                            || (seq == last.seq && hash != last.entry_hash)
                    }
                };
                if past || epoch < epoch_seen {
                    problems.push(ChainProblem::RetiredKey { seq });
                }
                epoch_seen = epoch_seen.max(epoch);
            }
            None => problems.push(
                if trusted.untrusted.iter().any(|k| signed.verify(k)) {
                    ChainProblem::UntrustedKey { seq }
                } else {
                    ChainProblem::BadSignature { seq }
                },
            ),
        }
        prev = Some((e, hash));
    }

    let time = |e: &SignedStateEntry| {
        DateTime::from_timestamp_micros(e.entry.created_at)
    };
    ChainReport {
        intact: problems.is_empty(),
        count: entries.len(),
        first_at: entries.first().and_then(time),
        last_at: entries.last().and_then(time),
        head: prev.map(|(e, entry_hash)| HeadRef {
            entry_hash,
            seq: e.seq,
        }),
        problems,
        notices: trusted.notices.clone(),
    }
}

/// Where the local head stands against the server's chain
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadStatus {
    /// The server's head is the local head
    Same,
    /// The server has the local head and more after it
    ServerAhead,
    /// Local writes wait in the outbox on a base the server has moved on
    /// from
    Diverged,
    /// The local head, or the outbox's base, is not on the server at all
    LocalMissing,
    /// Local writes wait in the outbox on the server's head
    Pending,
}

/// [`HeadStatus`] from the local head, the local outbox (oldest first) and
/// the server's chain hashes (in seq order). `Pending` is decided from the
/// outbox only, never from a head missing on the server: otherwise a
/// truncation by the server would pass for an upload not yet made
pub fn head_status(
    local_head: Option<&Sha256Hex>,
    outbox: &[StateEntry],
    server: &[Sha256Hex],
) -> HeadStatus {
    let server_head = server.last();
    let on_server: HashMap<&Sha256Hex, usize> =
        server.iter().enumerate().map(|(i, h)| (h, i)).collect();
    if !outbox.is_empty() {
        // An item the server already took (an upload acknowledged but not
        // yet committed locally) is skipped.
        let waiting: Vec<&StateEntry> = outbox
            .iter()
            .filter(|e| !on_server.contains_key(&e.entry_hash()))
            .collect();
        let Some(first) = waiting.first() else {
            return if local_head == server_head {
                HeadStatus::Same
            } else {
                HeadStatus::ServerAhead
            };
        };
        return match (&first.prev_hash, server_head) {
            (None, None) => HeadStatus::Pending,
            (Some(base), Some(head)) if base == head => HeadStatus::Pending,
            (Some(base), _) if on_server.contains_key(base) => {
                HeadStatus::Diverged
            }
            (None, Some(_)) => HeadStatus::Diverged,
            (Some(_), _) => HeadStatus::LocalMissing,
        };
    }
    match local_head {
        h if h == server_head => HeadStatus::Same,
        None => HeadStatus::ServerAhead,
        Some(h) if on_server.contains_key(h) => HeadStatus::ServerAhead,
        Some(_) => HeadStatus::LocalMissing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{SigningKey, generate_keypair};
    use crate::state::chain::BlobRef;

    fn agent() -> AgentId {
        AgentId::from(uuid::Uuid::from_u128(42))
    }

    fn entry(
        seq: u64,
        prev: Option<Sha256Hex>,
        kind: StateEntryKind,
        encoding: Option<BlobEncoding>,
    ) -> StateEntry {
        StateEntry {
            agora_state_entry: STATE_ENTRY_VERSION,
            agent_id: agent(),
            seq,
            created_at: 1_762_000_000_000_000 + seq as i64 * 1_000_000,
            state_at: 1_762_000_000_000_000,
            kind,
            prev_hash: prev,
            merged_from: vec![],
            blob: encoding.map(|encoding| BlobRef {
                sha256: Sha256Hex::from([seq as u8; 32]),
                size_bytes: 1,
                encoding,
            }),
            target: None,
            key_gen: 0,
        }
    }

    /// genesis, keyframe, then deltas against it
    fn chain(key: &SigningKey, n: u64) -> Vec<SignedStateEntry> {
        let mut out: Vec<SignedStateEntry> = Vec::new();
        let mut keyframe = None;
        for seq in 0..n {
            let prev = out.last().map(|e| e.entry_hash());
            let e = match seq {
                0 => entry(
                    0,
                    None,
                    StateEntryKind::Genesis,
                    Some(BlobEncoding::Keyframe),
                ),
                1 => entry(
                    1,
                    prev,
                    StateEntryKind::Snapshot,
                    Some(BlobEncoding::Keyframe),
                ),
                _ => entry(
                    seq,
                    prev,
                    StateEntryKind::Snapshot,
                    Some(BlobEncoding::Delta {
                        base: keyframe.unwrap(),
                    }),
                ),
            };
            let signed = e.sign(key);
            if seq == 1 {
                keyframe = Some(signed.entry_hash());
            }
            out.push(signed);
        }
        out
    }

    #[test]
    fn an_intact_chain() {
        let (sk, vk) = generate_keypair();
        let entries = chain(&sk, 5);
        let report = verify_chain(agent(), &entries, &TrustedKeys::new(vk));
        assert!(report.intact, "{:?}", report.problems);
        assert_eq!(report.count, 5);
        assert_eq!(report.head.unwrap().entry_hash, entries[4].entry_hash());
    }

    #[test]
    fn crafted_chains_are_caught() {
        let (sk, vk) = generate_keypair();
        let trusted = TrustedKeys::new(vk);
        let problems = |entries: &[SignedStateEntry]| {
            verify_chain(agent(), entries, &trusted).problems
        };

        // A gap
        let mut gap = chain(&sk, 5);
        gap.remove(2);
        let p = problems(&gap);
        assert!(p.contains(&ChainProblem::SeqGap {
            expected: 2,
            found: 3
        }));
        assert!(p.contains(&ChainProblem::BrokenLink { seq: 3 }));

        // A fork: two entries at seq 2
        let mut fork = chain(&sk, 3);
        let mut sibling = fork[2].entry.clone();
        sibling.state_at += 1;
        fork.push(sibling.sign(&sk));
        assert!(problems(&fork).contains(&ChainProblem::Fork { seq: 2 }));

        // A bad signature
        let mut bad = chain(&sk, 3);
        let (other, _) = generate_keypair();
        bad[1] = bad[1].entry.clone().sign(&other);
        assert!(
            problems(&bad).contains(&ChainProblem::BadSignature { seq: 1 })
        );

        // A rewritten entry breaks the next link
        let mut rewritten = chain(&sk, 3);
        let mut e = rewritten[1].entry.clone();
        e.state_at += 5;
        rewritten[1] = e.sign(&sk);
        assert!(
            problems(&rewritten).contains(&ChainProblem::BrokenLink { seq: 2 })
        );

        // A delta against a base that isn't a keyframe of this chain
        let mut stray = chain(&sk, 2);
        stray.push(
            entry(
                2,
                Some(stray[1].entry_hash()),
                StateEntryKind::Snapshot,
                Some(BlobEncoding::Delta {
                    base: Sha256Hex::from([9; 32]),
                }),
            )
            .sign(&sk),
        );
        assert!(
            problems(&stray)
                .contains(&ChainProblem::DeltaWithoutBase { seq: 2 })
        );

        // Reserved kinds and merges
        let mut reserved = chain(&sk, 2);
        let mut m = entry(
            2,
            Some(reserved[1].entry_hash()),
            StateEntryKind::Merge,
            Some(BlobEncoding::Keyframe),
        );
        m.merged_from.push(Sha256Hex::from([1; 32]));
        reserved.push(m.sign(&sk));
        let p = problems(&reserved);
        assert!(p.contains(&ChainProblem::UnknownKind {
            seq: 2,
            kind: StateEntryKind::Merge
        }));
        assert!(p.contains(&ChainProblem::MergeNotAllowed { seq: 2 }));

        // No genesis
        let headless = chain(&sk, 3);
        assert!(
            problems(&headless[1..]).contains(&ChainProblem::MissingGenesis)
        );

        // Someone else's entry
        let report = verify_chain(AgentId::new(), &chain(&sk, 1), &trusted);
        assert!(
            report
                .problems
                .contains(&ChainProblem::WrongAgent { seq: 0 })
        );
    }

    #[test]
    fn rotations_are_trusted_only_on_the_old_keys_word() {
        let (old, old_vk) = generate_keypair();
        let (new, new_vk) = generate_keypair();
        let mut entries = chain(&old, 2);
        let last = HeadRef {
            entry_hash: entries[1].entry_hash(),
            seq: 1,
        };
        let next = entry(
            2,
            Some(last.entry_hash),
            StateEntryKind::Snapshot,
            Some(BlobEncoding::Keyframe),
        );
        entries.push(next.sign(&new));
        let rotation = KeyRotation::agent_signed(
            agent(),
            0,
            &old,
            &new,
            Some(last),
            Utc::now(),
        );

        let rotated = TrustedKeys::new(old_vk)
            .with_rotations(std::slice::from_ref(&rotation));
        assert!(rotated.trusts(&new_vk));
        assert_eq!(rotated.current(), new_vk);
        assert!(verify_chain(agent(), &entries, &rotated).intact);

        // Without the rotation, the new key's entry is unverified
        let report = verify_chain(agent(), &entries, &TrustedKeys::new(old_vk));
        assert!(
            report
                .problems
                .contains(&ChainProblem::BadSignature { seq: 2 })
        );

        // An operator's replacement is a notice, and its entries untrusted
        let operator = KeyRotation::operator(
            agent(),
            0,
            PublicKeyHex::from(&old_vk),
            &new,
            Some(last),
            Utc::now(),
        );
        let replaced = TrustedKeys::new(old_vk).with_rotations(&[operator]);
        assert!(!replaced.trusts(&new_vk));
        let report = verify_chain(agent(), &entries, &replaced);
        assert!(
            report
                .problems
                .contains(&ChainProblem::UntrustedKey { seq: 2 })
        );
        assert!(matches!(
            report.notices[..],
            [KeyNotice::OperatorRotation { .. }]
        ));

        // A key the server reports is advisory
        let advised = TrustedKeys::new(old_vk).with_server_key(new_vk);
        assert!(!advised.trusts(&new_vk));
        assert!(matches!(
            advised.notices()[..],
            [KeyNotice::ServerKeyUnknown { .. }]
        ));
    }

    /// A retired key can neither rotate again nor sign past its rotation's
    /// position, however it dates the entry
    #[test]
    fn a_retired_key_is_powerless() {
        let (old, old_vk) = generate_keypair();
        let (new, new_vk) = generate_keypair();
        let (attacker, attacker_vk) = generate_keypair();
        let entries = chain(&old, 2);
        let last = HeadRef {
            entry_hash: entries[1].entry_hash(),
            seq: 1,
        };
        let r0 = KeyRotation::agent_signed(
            agent(),
            0,
            &old,
            &new,
            Some(last),
            Utc::now(),
        );
        // The stolen old key endorses old -> attacker, as the next rotation
        let bad = KeyRotation::agent_signed(
            agent(),
            1,
            &old,
            &attacker,
            Some(last),
            Utc::now(),
        );
        let keys = TrustedKeys::new(old_vk).with_rotations(&[r0.clone(), bad]);
        assert_eq!(keys.current(), new_vk);
        assert!(!keys.trusts(&attacker_vk));
        assert!(matches!(
            keys.notices()[..],
            [KeyNotice::RotationRefused { .. }]
        ));
        // Or as a rival rotation 0
        let rival = KeyRotation::agent_signed(
            agent(),
            0,
            &old,
            &attacker,
            Some(last),
            Utc::now(),
        );
        let keys2 = TrustedKeys::new(old_vk).with_rotations(&[r0, rival]);
        assert!(!keys2.trusts(&attacker_vk));

        // The old key's own entries, through its last, are fine
        assert!(verify_chain(agent(), &entries, &keys).intact);
        // A backdated entry past its last is not
        let mut forged = entry(
            2,
            Some(last.entry_hash),
            StateEntryKind::Snapshot,
            Some(BlobEncoding::Keyframe),
        );
        forged.created_at = entries[0].entry.created_at;
        let mut with_forged = entries.clone();
        with_forged.push(forged.clone().sign(&old));
        let report = verify_chain(agent(), &with_forged, &keys);
        assert!(
            report
                .problems
                .contains(&ChainProblem::RetiredKey { seq: 2 })
        );
        // Nor a rewrite of its last entry
        let mut rewritten = entries[1].entry.clone();
        rewritten.state_at += 1;
        let swapped = vec![entries[0].clone(), rewritten.sign(&old)];
        let report = verify_chain(agent(), &swapped, &keys);
        assert!(
            report
                .problems
                .contains(&ChainProblem::RetiredKey { seq: 1 })
        );
        // An old-key entry after a new-key one is refused too
        let second = forged.clone().sign(&new);
        let third = entry(
            3,
            Some(second.entry_hash()),
            StateEntryKind::Snapshot,
            Some(BlobEncoding::Keyframe),
        );
        let mut mixed = entries.clone();
        mixed.push(second);
        mixed.push(third.sign(&old));
        let report = verify_chain(agent(), &mixed, &keys);
        assert!(
            report
                .problems
                .contains(&ChainProblem::RetiredKey { seq: 3 })
        );
        // Clock skew is no problem: trust is by position
        let mut skewed = entries.clone();
        let mut late = forged;
        late.created_at = entries[1].entry.created_at + 1;
        skewed.push(late.sign(&new));
        assert!(verify_chain(agent(), &skewed, &keys).intact);
    }

    #[test]
    fn head_status_from_head_outbox_and_server() {
        let (sk, _) = generate_keypair();
        let entries = chain(&sk, 4);
        let hashes: Vec<Sha256Hex> =
            entries.iter().map(|e| e.entry_hash()).collect();
        let pending = |base: usize| {
            entry(
                9,
                Some(hashes[base]),
                StateEntryKind::Snapshot,
                Some(BlobEncoding::Keyframe),
            )
        };

        assert_eq!(
            head_status(Some(&hashes[3]), &[], &hashes),
            HeadStatus::Same
        );
        assert_eq!(
            head_status(Some(&hashes[2]), &[], &hashes),
            HeadStatus::ServerAhead
        );
        assert_eq!(head_status(None, &[], &hashes), HeadStatus::ServerAhead);
        assert_eq!(head_status(None, &[], &[]), HeadStatus::Same);
        // A rollback: the server lost the local head
        assert_eq!(
            head_status(Some(&hashes[3]), &[], &hashes[..2]),
            HeadStatus::LocalMissing
        );

        let p = pending(3);
        let h = p.entry_hash();
        assert_eq!(
            head_status(Some(&h), std::slice::from_ref(&p), &hashes),
            HeadStatus::Pending
        );
        // The server moved on from the outbox's base
        let p = pending(2);
        assert_eq!(
            head_status(Some(&p.entry_hash()), &[p], &hashes),
            HeadStatus::Diverged
        );
        // A truncated server is never Pending
        let p = pending(3);
        assert_eq!(
            head_status(Some(&p.entry_hash()), &[p], &hashes[..2]),
            HeadStatus::LocalMissing
        );
        // An upload taken but not yet committed locally
        let up = entries[3].entry.clone();
        assert_eq!(
            head_status(Some(&hashes[3]), &[up], &hashes),
            HeadStatus::Same
        );
        // A first upload
        let g = entries[0].entry.clone();
        assert_eq!(
            head_status(Some(&hashes[0]), &[g], &[]),
            HeadStatus::Pending
        );
    }
}
