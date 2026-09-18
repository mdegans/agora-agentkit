//! The shared verification vectors in `vectors/govlog`.
//!
//! Each file is one chain and the verdict [`verify_chain`] must return for
//! it, so a second implementation — `tools/verify_governance_log.py`, which
//! reads the same files — can be held to the same answers. A disagreement
//! is the point: it means one of the two is wrong about a rule.
//!
//! Keys are fixed seeds and timestamps fixed constants, so the files are
//! byte-stable. Regenerate with `just vectors`.

use super::tests::{Chain, at, chain, gov, key_id, link};
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// The file shape
// ---------------------------------------------------------------------------

/// One vector file
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Vector {
    description: String,
    /// The key the chain started under, which is not in the chain
    genesis_key: PublicKeyHex,
    /// The verifier's out-of-band [`KeyAnchor`]
    anchor: Vec<PublicKeyHex>,
    links: Vec<GovernanceChainLink>,
    /// Entry `data` read separately, as a client that fetched an entry in
    /// full would hand to
    /// [`check_content`](GovernanceVerification::check_content)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    contents: BTreeMap<GovernanceLogId, Value>,
    expect: Expect,
}

/// The part of the verdict both implementations must agree on
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Expect {
    ok: bool,
    head: Option<GovernanceLogId>,
    /// The key in force for the next entry
    public_key: PublicKeyHex,
    repudiated: Vec<GovernanceLogId>,
    unanchored_keys: Vec<PublicKeyHex>,
    entries: Vec<ExpectEntry>,
    /// [`standing`] of each amended entry — the one derived value that is
    /// not on [`EntryVerdict`]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    standing: BTreeMap<GovernanceLogId, Standing>,
}

/// A [`EntryVerdict`] minus the fields whose wording is a verifier's own
/// business
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ExpectEntry {
    id: GovernanceLogId,
    signature_valid: bool,
    link_valid: bool,
    content_matches: Option<bool>,
    /// Recomputed from the timestamps, never copied from the wire
    retroactive: bool,
    out_of_order: bool,
    redacted: bool,
    repudiated: bool,
    amended_by: Vec<GovernanceLogId>,
    /// Whether a problem is reported at all — never its text
    problem: bool,
}

/// The verdict for `links`, as a vector file records it
fn observe(
    links: &[GovernanceChainLink],
    genesis: &VerifyingKey,
    anchor: &[PublicKeyHex],
    contents: &BTreeMap<GovernanceLogId, Value>,
) -> Expect {
    let anchor: KeyAnchor = anchor.iter().copied().collect();
    let mut report = verify_chain(links, genesis, &anchor);
    for (id, data) in contents {
        let link = links
            .iter()
            .find(|l| &l.id == id)
            .expect("`contents` names an entry of the chain");
        report.check_content(link, data);
    }
    let report = report.settle();

    let kinds: BTreeMap<GovernanceLogId, AmendmentKind> = links
        .iter()
        .filter(|l| l.entry_type == GovernanceLogEntryType::Amendment)
        .filter_map(|l| {
            let data = l.data.clone()?;
            let amendment: Amendment = serde_json::from_value(data).ok()?;
            Some((l.id.clone(), amendment.kind))
        })
        .collect();

    Expect {
        ok: report.ok,
        head: report.head.clone(),
        public_key: report.public_key,
        repudiated: report.repudiated.clone(),
        unanchored_keys: report.unanchored_keys.clone(),
        standing: report
            .entries
            .iter()
            .filter(|e| !e.amended_by.is_empty())
            .map(|e| {
                let kinds =
                    e.amended_by.iter().filter_map(|id| kinds.get(id).copied());
                (e.id.clone(), standing(kinds))
            })
            .collect(),
        entries: report
            .entries
            .iter()
            .map(|e| ExpectEntry {
                id: e.id.clone(),
                signature_valid: e.signature_valid,
                link_valid: e.link_valid,
                content_matches: e.content_matches,
                retroactive: e.retroactive,
                out_of_order: e.out_of_order,
                redacted: e.redacted,
                repudiated: e.repudiated,
                amended_by: e.amended_by.clone(),
                problem: e.problem.is_some(),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// A chain to write out, before its verdict is observed
struct Case {
    name: &'static str,
    description: &'static str,
    genesis: PublicKeyHex,
    anchor: Vec<PublicKeyHex>,
    links: Vec<GovernanceChainLink>,
    contents: BTreeMap<GovernanceLogId, Value>,
}

impl Case {
    fn new(
        name: &'static str,
        description: &'static str,
        genesis: &VerifyingKey,
        anchor: Vec<PublicKeyHex>,
        links: Vec<GovernanceChainLink>,
    ) -> Self {
        Self {
            name,
            description,
            genesis: genesis.into(),
            anchor,
            links,
            contents: BTreeMap::new(),
        }
    }

    /// The `data` a client read for `id`, hashed by the content check
    fn content(mut self, id: GovernanceLogId, data: Value) -> Self {
        self.contents.insert(id, data);
        self
    }

    fn vector(&self) -> Vector {
        let genesis = self
            .genesis
            .to_verifying_key()
            .expect("a case's genesis key is a curve point");
        Vector {
            description: self.description.to_string(),
            genesis_key: self.genesis,
            anchor: self.anchor.clone(),
            links: self.links.clone(),
            contents: self.contents.clone(),
            expect: observe(
                &self.links,
                &genesis,
                &self.anchor,
                &self.contents,
            ),
        }
    }
}

/// The signing key for `seed`. Fixed, not generated: a vector is only
/// useful if it is the same bytes tomorrow.
fn signer(seed: u8) -> (SigningKey, VerifyingKey) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let public = key.verifying_key();
    (key, public)
}

/// A link built field by field, so a vector can put one of them out of
/// place and leave the rest well-formed
fn forge(
    key: &SigningKey,
    id: GovernanceLogId,
    entry_type: GovernanceLogEntryType,
    seq: u64,
    created_at: DateTime<Utc>,
    prev_hash: Option<Sha256Hex>,
    data: &Value,
) -> GovernanceChainLink {
    let created_at = truncate_to_micros(created_at);
    let envelope = Envelope::new(
        id.clone(),
        entry_type,
        created_at,
        prev_hash,
        data_hash(data),
    );
    let attestation = attest(
        key,
        &envelope,
        seq,
        created_at + chrono::Duration::seconds(1),
    );
    GovernanceChainLink {
        id,
        entry_type,
        created_at,
        attestation,
        data: None,
    }
}

fn cases() -> Vec<Case> {
    use GovernanceLogEntryType::{
        Amendment as AmendmentEntry, CouncilDecision,
    };

    let (steward, steward_pk) = signer(1);
    let (successor, successor_pk) = signer(2);
    let (thief, _thief_pk) = signer(3);
    let (recovery, recovery_pk) = signer(4);
    let (forger, _) = signer(5);
    let pinned = vec![PublicKeyHex::from(&steward_pk)];
    let both = vec![
        PublicKeyHex::from(&steward_pk),
        PublicKeyHex::from(&successor_pk),
    ];
    let recovered = vec![
        PublicKeyHex::from(&steward_pk),
        PublicKeyHex::from(&recovery_pk),
    ];
    let mut out = Vec::new();

    // -- the chain itself --

    out.push(Case::new(
        "clean_chain",
        "Three council decisions, each signed as it was recorded.",
        &steward_pk,
        pinned.clone(),
        chain(&steward, 3),
    ));

    let mut links: Vec<GovernanceChainLink> = Vec::new();
    for i in 1..=3u32 {
        let data = json!({"title": format!("Decision {i}")});
        let l = link(&steward, i, links.last(), &data, at(100_000 + i as i64));
        links.push(l);
    }
    out.push(Case::new(
        "retroactive",
        "A chain attested long after it was recorded: valid, and every \
         entry flagged retroactive.",
        &steward_pk,
        pinned.clone(),
        links,
    ));

    let mut links = chain(&steward, 2);
    links[0].attestation.data_hash =
        data_hash(&json!({"title": "Decision 1", "outcome": "REJECTED"}));
    out.push(Case::new(
        "tampered_data_hash",
        "The first entry's attested data_hash was edited, so its \
         entry_hash no longer recomputes.",
        &steward_pk,
        pinned.clone(),
        links,
    ));

    let mut links = chain(&steward, 3);
    let mut signature = *links[1].attestation.signature.as_bytes();
    signature[0] ^= 0x01;
    links[1].attestation.signature = signature.into();
    out.push(Case::new(
        "bad_signature",
        "One bit flipped in the second entry's signature. The hashes still \
         recompute; the signature does not verify.",
        &steward_pk,
        pinned.clone(),
        links,
    ));

    let mut links = chain(&steward, 3);
    links[2] = forge(
        &steward,
        gov(3),
        CouncilDecision,
        3,
        at(30),
        Some(links[0].attestation.entry_hash),
        &json!({"title": "Decision 3", "outcome": "approved"}),
    );
    out.push(Case::new(
        "broken_prev_hash",
        "The third entry is signed and self-consistent but names the first \
         entry as its predecessor.",
        &steward_pk,
        pinned.clone(),
        links,
    ));

    let mut links = chain(&steward, 3);
    links[2].attestation.chain_seq = 5;
    out.push(Case::new(
        "chain_seq_gap",
        "A gap in chain_seq. It is an index, not part of the envelope, so \
         the signature is untouched and only the position is wrong.",
        &steward_pk,
        pinned.clone(),
        links,
    ));

    let first = forge(
        &steward,
        gov(1),
        CouncilDecision,
        1,
        at(100),
        None,
        &json!({"title": "Decision 1"}),
    );
    let second = forge(
        &steward,
        gov(2),
        CouncilDecision,
        2,
        at(50),
        Some(first.attestation.entry_hash),
        &json!({"title": "Decision 2"}),
    );
    out.push(Case::new(
        "out_of_order",
        "The second entry was recorded before the first by the clock. Chain \
         order is what is attested, so this is flagged, not broken.",
        &steward_pk,
        pinned.clone(),
        vec![first, second],
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.push(
        &steward,
        gov(1),
        CouncilDecision,
        json!({"title": "Decision 1, again"}),
        false,
    );
    out.push(Case::new(
        "duplicate_id",
        "The same id appears twice.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.push(
        &steward,
        key_id(1),
        CouncilDecision,
        json!({"title": "a decision wearing a KEY- id"}),
        false,
    );
    let amendment = Amendment::new(
        key_id(1),
        c.hash_at(1),
        AmendmentKind::Correction,
        "clerical",
        "citation corrected",
    )
    .unwrap();
    c.push(
        &steward,
        gov(7),
        AmendmentEntry,
        serde_json::to_value(&amendment).unwrap(),
        true,
    );
    out.push(Case::new(
        "id_series_mismatch",
        "A council decision in the reserved KEY- series, and an amendment \
         outside the AMD- series.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- amendments --

    let mut c = Chain::new();
    let target = c.decision(&steward);
    c.decision(&steward);
    let amendment = Amendment::new(
        target,
        c.hash_at(1),
        AmendmentKind::NonPrecedential,
        "§1 (Red Team Cases Recharacterized)",
        "diagnostic finding — not citable as moderation precedent",
    )
    .unwrap()
    .with_authority(gov(5))
    .with_rationale("§5 leaves the ruling itself standing");
    c.amend(&steward, &amendment);
    out.push(Case::new(
        "amendment_non_precedential",
        "An amendment that strips precedential force from an earlier \
         decision without disturbing it.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let amendment = Amendment::new(
        target,
        data_hash(&json!("some other entry")),
        AmendmentKind::Overruled,
        "Art. VI § 2",
        "overruled",
    )
    .unwrap();
    c.amend(&steward, &amendment);
    out.push(Case::new(
        "amendment_wrong_target_hash",
        "Right id, wrong entry: target_entry_hash is not the target's.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let amendment = Amendment::new(
        gov(2),
        c.hash_at(1),
        AmendmentKind::Overruled,
        "Art. VI § 2",
        "overruled",
    )
    .unwrap();
    c.amend(&steward, &amendment);
    c.decision(&steward);
    out.push(Case::new(
        "amendment_forward_reference",
        "An amendment naming an entry that does not exist yet.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let amendment = Amendment::new(
        target,
        c.hash_at(1),
        AmendmentKind::Correction,
        "clerical",
        "typo in the citation",
    )
    .unwrap();
    c.amend(&steward, &amendment);
    c.links[1].data = None;
    out.push(Case::new(
        "amendment_missing_data",
        "An amendment served without its `data`, which is the whole record \
         of what it does.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let amendment = Amendment::new(
        target,
        c.hash_at(1),
        AmendmentKind::Overruled,
        "Art. VI § 2",
        "overruled by a later decision",
    )
    .unwrap();
    c.amend(&steward, &amendment);
    c.links[1].data.as_mut().unwrap()["note"] = json!("reinstated, actually");
    out.push(Case::new(
        "amendment_data_tampered",
        "The amendment's `data` was edited after signing: the envelope is \
         intact, the content no longer hashes to it, and the amendment has \
         no effect.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- redaction --

    let data = json!({
        "finding": "upheld",
        "subject": {"handle": "someone", "detail": "personal"},
    });
    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let amendment_id = c.next_amd();
    let (amendment, redacted) = Amendment::redaction(
        &amendment_id,
        target.clone(),
        c.hash_at(1),
        "GDPR Art. 17(1)(a)",
        "personal data removed on request",
        vec!["/subject/handle".into(), "/subject/detail".into()],
        &data,
        // Fixed, like every key in this file: vectors are reproducible.
        Blind::from([0x5a; 32]),
    )
    .unwrap();
    c.amend(&steward, &amendment);
    out.push(
        Case::new(
            "redaction_accepted",
            "A lawful redaction: the target's data has changed since it was \
             attested, and hashes to the amendment's resulting_data_hash.",
            &steward_pk,
            pinned.clone(),
            c.links.clone(),
        )
        .content(target.clone(), redacted.clone()),
    );
    let mut tampered = redacted.clone();
    tampered["finding"] = json!("overturned");
    out.push(
        Case::new(
            "redaction_tampered",
            "The same redaction, with the redacted entry edited a second \
             time. Neither hash matches.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, tampered),
    );

    // -- forgery --

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let fake = Amendment::new(
        target,
        c.hash_at(1),
        AmendmentKind::Overruled,
        "none",
        "overruled, says nobody with the key",
    )
    .unwrap();
    c.amend(&forger, &fake);
    let grab = KeyRotation::routine(
        (&steward_pk).into(),
        &forger,
        c.prev_hash(),
        at(35),
        "",
    );
    c.rotate(&forger, &grab);
    out.push(Case::new(
        "forged_entries_no_effects",
        "An amendment and a rotation signed by a key the chain never named. \
         Neither amends nor moves anything.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- key rotation, v1 --

    let mut c = Chain::new();
    c.decision(&steward);
    let rotation = KeyRotation::routine(
        (&steward_pk).into(),
        &successor,
        c.prev_hash(),
        at(25),
        "scheduled rotation",
    );
    c.rotate(&steward, &rotation);
    c.decision(&successor);
    let routine = c.links;
    out.push(Case::new(
        "rotation_v1_routine",
        "A scheduled rotation, signed by the old key, to a key the anchor \
         vouches for.",
        &steward_pk,
        both.clone(),
        routine.clone(),
    ));
    out.push(Case::new(
        "rotation_v1_routine_unanchored",
        "The same rotation, to a key this verifier has never heard of: \
         followed, and reported.",
        &steward_pk,
        pinned.clone(),
        routine,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let out_key = KeyRotation::routine(
        (&steward_pk).into(),
        &successor,
        c.prev_hash(),
        at(15),
        "scheduled",
    );
    c.rotate(&steward, &out_key);
    let back = KeyRotation::routine(
        (&successor_pk).into(),
        &steward,
        c.prev_hash(),
        at(25),
        "back to the old key",
    );
    c.rotate(&successor, &back);
    out.push(Case::new(
        "rotation_v1_reused_key",
        "A rotation back to a key that has already held the chain.",
        &steward_pk,
        both.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let mut rotation = KeyRotation::routine(
        (&steward_pk).into(),
        &thief,
        c.prev_hash(),
        at(25),
        "scheduled",
    );
    rotation.new_key = (&successor_pk).into();
    let statement = rotation.statement(c.prev_hash());
    rotation.proof = crypto::sign(
        &steward,
        statement.hash().as_bytes(),
        rotation.proof_signed_at,
    )
    .into();
    c.rotate(&steward, &rotation);
    out.push(Case::new(
        "rotation_v1_forged_proof",
        "A rotation to a key nobody holds: the proof of possession is \
         signed by the old key instead of the new one.",
        &steward_pk,
        both.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.decision(&steward);
    let reattested = c.decision(&steward);
    let declaration = KeyRotation::compromise(
        (&steward_pk).into(),
        &successor,
        TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        },
        c.prev_hash(),
        at(45),
        "signing key exfiltrated",
    );
    c.rotate(&successor, &declaration);
    let vouch = Amendment::new(
        reattested,
        c.hash_at(3),
        AmendmentKind::Reattested,
        "Art. VII",
        "independently verified; the Steward vouches for it",
    )
    .unwrap();
    c.amend(&successor, &vouch);
    let compromise = c.links;
    out.push(Case::new(
        "rotation_v1_compromise_anchored",
        "A compromise declaration signed by an anchored new key: the window \
         after the last trusted entry is repudiated, and a reattestation \
         restores one entry of it.",
        &steward_pk,
        both.clone(),
        compromise,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.decision(&steward);
    let declaration = KeyRotation::compromise(
        (&steward_pk).into(),
        &successor,
        TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        },
        c.prev_hash(),
        at(45),
        "trust me",
    );
    c.rotate(&successor, &declaration);
    out.push(Case::new(
        "rotation_v1_compromise_unanchored",
        "The same declaration, from a key outside the anchor: it \
         authenticates nothing, and nothing is repudiated on its say-so.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let stolen = KeyRotation::routine(
        (&steward_pk).into(),
        &thief,
        c.prev_hash(),
        at(25),
        "routine",
    );
    c.rotate(&steward, &stolen);
    c.decision(&thief);
    let declaration = KeyRotation::compromise(
        (&steward_pk).into(),
        &recovery,
        TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        },
        c.prev_hash(),
        at(55),
        "key stolen; everything after GOV-2026-0001 is disclaimed",
    );
    c.rotate(&recovery, &declaration);
    out.push(Case::new(
        "rotation_v1_thief_rotation_repudiated",
        "A thief rotates the chain onto their own key; the Steward answers \
         with a compromise declaration from before the theft.",
        &steward_pk,
        recovered,
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward); // 1 — the last entry the first declaration trusts
    c.decision(&steward); // 2 — inside the first window
    let first = KeyRotation::compromise(
        (&steward_pk).into(),
        &successor,
        TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        },
        c.prev_hash(),
        at(35),
        "signing key exfiltrated",
    );
    c.rotate(&successor, &first);
    let second = KeyRotation::compromise(
        (&steward_pk).into(),
        &recovery,
        TrustedHead {
            id: gov(2),
            chain_seq: 2,
            entry_hash: c.hash_at(2),
        },
        c.prev_hash(),
        at(45),
        "and trust the old key one entry further, actually",
    );
    c.rotate(&recovery, &second);
    out.push(Case::new(
        "rotation_v1_second_compromise_inside_the_window",
        "A second compromise declaration naming, as its last trusted entry, \
         one the first declaration repudiated. Trust cannot be anchored \
         inside a window nobody trusts.",
        &steward_pk,
        vec![
            PublicKeyHex::from(&steward_pk),
            PublicKeyHex::from(&successor_pk),
            PublicKeyHex::from(&recovery_pk),
        ],
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let real = KeyRotation::compromise(
        (&steward_pk).into(),
        &successor,
        TrustedHead {
            id: gov(1),
            chain_seq: 1,
            entry_hash: c.hash_at(1),
        },
        c.prev_hash(),
        at(25),
        "signing key exfiltrated",
    );
    c.rotate(&successor, &real);
    c.decision(&successor);
    let hijack = KeyRotation::compromise(
        (&successor_pk).into(),
        &steward,
        TrustedHead {
            id: gov(2),
            chain_seq: 3,
            entry_hash: c.hash_at(3),
        },
        c.prev_hash(),
        at(45),
        "the Steward's key is the compromised one, trust me",
    );
    c.rotate(&steward, &hijack);
    c.decision(&steward);
    out.push(Case::new(
        "rotation_v1_stolen_key_not_restored",
        "The thief, still holding the compromised key, declares the \
         Steward's replacement compromised and names the stolen key as the \
         new one.",
        &steward_pk,
        both,
        c.links,
    ));

    out
}

// ---------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/govlog")
}

fn render(case: &Case) -> String {
    let mut text = serde_json::to_string_pretty(&case.vector())
        .expect("a Vector always serializes");
    text.push('\n');
    text
}

/// The committed vector files, sorted by name
fn vector_files() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .expect("vectors/govlog exists; `just vectors` writes it")
        .map(|e| e.expect("a readable directory entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
}

/// Rewrite `vectors/govlog`. Not part of the suite: it is the generator,
/// and what CI checks is that what it would write is what is committed.
#[test]
#[ignore = "writes vectors/govlog; run it with `just vectors`"]
fn regenerate_the_vectors() {
    let dir = vectors_dir();
    std::fs::create_dir_all(&dir).expect("vectors/govlog is creatable");
    for case in cases() {
        std::fs::write(dir.join(format!("{}.json", case.name)), render(&case))
            .expect("a vector file is writable");
    }
}

/// The drift guard: every committed vector still gets the verdict it
/// records, from this verifier, today.
#[test]
fn every_vector_matches_the_verifier() {
    let files = vector_files();
    assert!(
        files.len() >= cases().len(),
        "a case has no file: {files:#?}"
    );
    for path in files {
        let text = std::fs::read_to_string(&path).expect("a readable vector");
        let vector: Vector = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let genesis = vector
            .genesis_key
            .to_verifying_key()
            .expect("a vector's genesis key is a curve point");
        let actual =
            observe(&vector.links, &genesis, &vector.anchor, &vector.contents);
        assert_eq!(actual, vector.expect, "{}", path.display());
    }
}

/// And the generator still writes exactly those bytes, so editing a case
/// without regenerating is a failure rather than a silent divergence.
#[test]
fn the_vectors_are_what_the_generator_writes() {
    for case in cases() {
        let path = vectors_dir().join(format!("{}.json", case.name));
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            render(&case),
            committed,
            "{} is stale; run `just vectors`",
            path.display()
        );
    }
}

/// The Python verifier ships its own copy of [`PUBLISHED_KEYS`] — it is one
/// stdlib-only file on purpose — so the two lists are checked against each
/// other here rather than trusted to stay equal.
#[test]
fn the_python_verifier_publishes_the_same_keys() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tools/verify_governance_log.py");
    let text = std::fs::read_to_string(&path).expect("the Python verifier");
    let (_, rest) = text
        .split_once("PUBLISHED_KEYS = [")
        .expect("the script declares PUBLISHED_KEYS");
    let (list, _) = rest.split_once(']').expect("the declaration ends");
    let keys: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
    assert_eq!(keys, PUBLISHED_KEYS, "{}", path.display());
}
