//! The shared verification vectors in `vectors/govlog`.
//!
//! Each file is one chain and the verdict [`verify_chain`] must return for
//! it, so a second implementation — `tools/verify_governance_log.py`, which
//! reads the same files — can be held to the same answers. A disagreement
//! is the point: it means one of the two is wrong about a rule.
//!
//! Keys are fixed seeds and timestamps fixed constants, so the files are
//! byte-stable. Regenerate with `just vectors`.

use super::tests::{
    Chain, at, certify, chain, gov, key_id, link, rec, resalted, root, roots,
    v1,
};
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
    /// The verifier's [`RootSet`] — throwaway keys, never [`ROOT_KEYS`]
    root_keys: Vec<PublicKeyHex>,
    root_threshold: usize,
    #[serde(deserialize_with = "strict_links")]
    links: Vec<GovernanceChainLink>,
    /// Entry `data` read separately, as a client that fetched an entry in
    /// full would hand to
    /// [`check_content`](GovernanceVerification::check_content)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    contents: BTreeMap<GovernanceLogId, Value>,
    expect: Expect,
}

/// [`links_from_json`], which is how a client should read a chain
fn strict_links<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<GovernanceChainLink>, D::Error> {
    links_from_json(&Value::deserialize(deserializer)?)
        .map_err(serde::de::Error::custom)
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
    /// The signing key history, oldest first
    keys: Vec<GovernanceKeyRecord>,
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
    /// The three below are absent when empty, so the vectors that predate
    /// them are unchanged
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    revisions: Vec<GovernanceLogId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_data_hash: Option<Sha256Hex>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded_revisions: Vec<GovernanceLogId>,
    repudiated: bool,
    amended_by: Vec<GovernanceLogId>,
    /// Where a version 2 amendment's texts stand; `null` for anything else
    texts: Option<AmendmentTextStatus>,
    /// Whether a problem is reported at all — never its text
    problem: bool,
}

/// The verdict for `links`, as a vector file records it
fn observe(
    links: &[GovernanceChainLink],
    genesis: &VerifyingKey,
    anchor: &[PublicKeyHex],
    roots: &RootSet,
    contents: &BTreeMap<GovernanceLogId, Value>,
) -> Expect {
    let anchor: KeyAnchor = anchor.iter().copied().collect();
    let mut report = verify_chain(links, genesis, &anchor, roots);
    for (id, data) in contents {
        // A fuzzed chain may have lost the entry; a committed vector's
        // verdict would then not be the one it records.
        if let Some(link) = links.iter().find(|l| &l.id == id) {
            report.check_content(link, data);
        }
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
        keys: report.keys.clone(),
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
                revisions: e.revisions.clone(),
                latest_data_hash: e.latest_data_hash,
                superseded_revisions: e.superseded_revisions.clone(),
                repudiated: e.repudiated,
                amended_by: e.amended_by.clone(),
                texts: e.texts,
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
    root_threshold: usize,
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
            root_threshold: 1,
            links,
            contents: BTreeMap::new(),
        }
    }

    /// Verified against a root set that needs `n` signers
    fn threshold(mut self, n: usize) -> Self {
        self.root_threshold = n;
        self
    }

    fn roots(&self) -> RootSet {
        RootSet::new(roots().keys().copied(), self.root_threshold)
    }

    /// Sorted, so the file does not depend on a `HashSet`'s order
    fn root_keys(&self) -> Vec<PublicKeyHex> {
        let mut keys: Vec<_> = self.roots().keys().copied().collect();
        keys.sort_by_key(|k| k.to_string());
        keys
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
            root_keys: self.root_keys(),
            root_threshold: self.root_threshold,
            links: self.links.clone(),
            contents: self.contents.clone(),
            expect: observe(
                &self.links,
                &genesis,
                &self.anchor,
                &self.roots(),
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
        texts: None,
    }
}

/// What a version 1 proof of possession covered, for the one vector that
/// shows a v1 rotation is refused
#[derive(Serialize)]
struct RotationStatementV1 {
    agora_governance_key_rotation: u32,
    reason: RotationReason,
    old_key: PublicKeyHex,
    new_key: PublicKeyHex,
    prev_hash: Option<Sha256Hex>,
}

impl RotationStatementV1 {
    fn preimage(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("it always serializes")
    }
}

fn cases() -> Vec<Case> {
    use GovernanceLogEntryType::{
        Amendment as AmendmentEntry, CouncilDecision,
        StewardRecord as StewardRecordEntry,
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
    let amendment = AmendmentDraft::new(
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
        serde_json::to_value(resalted(&amendment, 2).amendment).unwrap(),
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

    // -- a Steward's record --

    let record = serde_json::to_value(StewardRecord::new(
        "key_ceremony",
        "The first key ceremony",
        "The root certified a fresh online key.",
    ))
    .unwrap();

    let mut c = Chain::new();
    c.decision(&steward);
    c.push(&steward, rec(1), StewardRecordEntry, record.clone(), false);
    c.decision(&steward);
    out.push(Case::new(
        "steward_record",
        "A Steward's record between two decisions. No verifier reads what \
         it says: it is content, in the REC- series.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.push(&steward, gov(2), StewardRecordEntry, record.clone(), false);
    c.push(
        &steward,
        rec(1),
        CouncilDecision,
        json!({"title": "a decision wearing a REC- id"}),
        false,
    );
    out.push(Case::new(
        "record_series_mismatch",
        "A Steward's record outside the REC- series, and a council decision \
         inside it.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- amendments --

    let mut c = Chain::new();
    let target = c.decision(&steward);
    c.decision(&steward);
    let amendment = AmendmentDraft::new(
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
    let amendment = AmendmentDraft::new(
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
    let amendment = AmendmentDraft::new(
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
    let amendment = AmendmentDraft::new(
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
    let amendment = AmendmentDraft::new(
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

    // -- amendment texts: committed to, not contained --

    // `amendment_non_precedential` again, at each thing that can happen to
    // the texts beside it.
    let texts_case =
        |name: &'static str,
         description: &'static str,
         edit: &dyn Fn(&mut GovernanceChainLink)| {
            let mut c = Chain::new();
            let target = c.decision(&steward);
            c.decision(&steward);
            let amendment = AmendmentDraft::new(
                target,
                c.hash_at(1),
                AmendmentKind::NonPrecedential,
                "§1 (Red Team Cases Recharacterized)",
                "diagnostic finding — not citable as moderation precedent",
            )
            .unwrap()
            .with_authority(gov(5))
            .with_rationale(
                "at the request of the operator of the agent named",
            );
            c.amend(&steward, &amendment);
            edit(c.links.last_mut().unwrap());
            Case::new(name, description, &steward_pk, pinned.clone(), c.links)
        };
    out.push(texts_case(
        "amendment_v2_rationale_withheld",
        "The rationale and its salt have been erased; the label agents see \
         is still there. Nothing signed has changed, and nothing is wrong.",
        &|link| link.texts.as_mut().unwrap().rationale = None,
    ));
    out.push(texts_case(
        "amendment_v2_all_texts_withheld",
        "No texts beside the entry at all, as a chain served without them \
         looks. The amendment still takes effect.",
        &|link| link.texts = None,
    ));
    out.push(texts_case(
        "amendment_v2_text_substituted",
        "Different words under the original salt, where the rationale was. \
         The entry never committed to them.",
        &|link| {
            let rationale =
                link.texts.as_mut().unwrap().rationale.as_mut().unwrap();
            rationale.text = "at nobody's request".into();
        },
    ));
    out.push(texts_case(
        "amendment_v2_texts_swapped",
        "The basis and the note, each genuine, in each other's place.",
        &|link| {
            let texts = link.texts.as_mut().unwrap();
            std::mem::swap(&mut texts.basis, &mut texts.note);
        },
    ));

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let amendment = AmendmentDraft::new(
        target,
        c.hash_at(1),
        AmendmentKind::Correction,
        "clerical",
        "citation corrected",
    )
    .unwrap();
    c.amend(&steward, &amendment);
    c.links[1].texts.as_mut().unwrap().rationale =
        Some(CommittedText::with_salt(
            TextSalt::from([7; 32]),
            "a rationale nobody signed",
        ));
    out.push(Case::new(
        "amendment_v2_uncommitted_rationale",
        "A rationale beside an amendment that commits to none.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let mut amendment = AmendmentDraft::new(
        target,
        c.hash_at(1),
        AmendmentKind::Correction,
        "clerical",
        "citation corrected",
    )
    .unwrap();
    amendment = resalted(&amendment, 2);
    amendment.amendment.note =
        AmendmentText::Plain("citation corrected".into());
    c.amend_v1(&steward, &amendment.amendment);
    out.push(Case::new(
        "amendment_v2_plain_text",
        "A version 2 amendment with its note in the signed data after all. \
         Permanent free text is what version 2 exists to end: malformed.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let legacy = |c: &mut Chain| {
        let target = c.decision(&steward);
        let amendment = AmendmentDraft::new(
            target,
            c.hash_at(1),
            AmendmentKind::NonPrecedential,
            "§1 (Red Team Cases Recharacterized)",
            "diagnostic finding — not citable as moderation precedent",
        )
        .unwrap()
        .with_authority(gov(5))
        .with_rationale("§5 leaves the ruling itself standing");
        let amendment = resalted(&amendment, 2);
        c.amend_v1(&steward, &v1(amendment.clone()));
        amendment
    };
    let mut c = Chain::new();
    legacy(&mut c);
    out.push(Case::new(
        "amendment_v1",
        "A version 1 amendment, texts in the signed data, as the \
         platform's first three are. Still valid, and always will be.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let amendment = legacy(&mut c);
    c.links[1].texts = Some(amendment.texts);
    out.push(Case::new(
        "amendment_v1_texts_beside",
        "The same, with texts served beside it. A version 1 amendment \
         commits to none, so whatever they say, nobody signed it.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.links[0].texts = Some(AmendmentTexts {
        note: Some(CommittedText::with_salt(
            TextSalt::from([7; 32]),
            "the Council did not really mean this",
        )),
        ..Default::default()
    });
    out.push(Case::new(
        "texts_beside_a_decision",
        "Texts beside an entry that is not an amendment.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- numbers --

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let amendment = AmendmentDraft::new(
        target,
        c.hash_at(1),
        AmendmentKind::Correction,
        "clerical",
        "citation corrected",
    )
    .unwrap();
    let mut data =
        serde_json::to_value(resalted(&amendment, 2).amendment).unwrap();
    data["weight"] = json!(0.5);
    c.push(&steward, super::tests::amd(1), AmendmentEntry, data, true);
    out.push(Case::new(
        "data_non_integer_number",
        "An amendment, validly signed, with a field holding 0.5. Governance \
         data never contains a number that is not a 64-bit integer: no two \
         JSON libraries write a float the same way, so it is reported and \
         never hashed.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.entry(&steward, json!({"title": "Decision", "tally": 3}));
    out.push(
        Case::new(
            "content_non_integer_number",
            "A decision read in full whose content holds 3.0 where 3 was \
             attested. Not hashed, so not a match, whatever a library \
             would have printed for it.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, json!({"title": "Decision", "tally": 3.0})),
    );

    // -- redaction --

    let data = json!({
        "finding": "upheld",
        "subject": {"handle": "someone", "detail": "personal"},
    });
    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let amendment_id = c.next_amd();
    let (amendment, redacted) = AmendmentDraft::redaction(
        &amendment_id,
        target.clone(),
        c.hash_at(1),
        "GDPR Art. 17(1)(a)",
        "personal data removed on request",
        vec!["/subject/handle".into(), "/subject/detail".into()],
        &data,
        // Fixed, like every key in this file: vectors are reproducible.
        Blind::from([0x5a; 32]),
        &[],
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

    // -- revision --

    let data = json!({
        "title": "A motion",
        "responses": [
            {"role": "lawyer", "rationale": "Because.", "raw_text": "Because."},
            {"role": "artist", "rationale": "Why not.", "raw_text": "Why not?"},
        ],
        "subject": {"handle": "someone"},
        BLIND_KEY: Blind::from([0x5b; 32]),
    });
    let dedup = |data: &Value| {
        Revision::remove_duplicates(
            data,
            &[("/responses/0/raw_text", "/responses/0/rationale")],
        )
        .unwrap()
    };
    let moved: json_patch::Patch = serde_json::from_value(json!([
        {"op": "move", "from": "/title", "path": "/motion"}
    ]))
    .unwrap();
    let revise =
        |c: &Chain, target: &GovernanceLogId, latest: &Value, edit: Edit| {
            AmendmentDraft::revision(
                target.clone(),
                CouncilDecision,
                c.hash_at(1),
                "Steward's record",
                "raw_text identical to the rationale removed",
                latest,
                edit,
            )
            .unwrap()
        };
    let redact = |c: &Chain,
                  target: &GovernanceLogId,
                  field: &str,
                  data: &Value,
                  revisions: &[&Revision]| {
        AmendmentDraft::redaction(
            &c.next_amd(),
            target.clone(),
            c.hash_at(1),
            "GDPR Art. 17(1)(a)",
            "removed on request",
            vec![field.to_string()],
            data,
            Blind::from([0x5c; 32]),
            revisions,
        )
        .unwrap()
    };

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (first, v1) = revise(&c, &target, &data, dedup(&data));
    c.amend(&steward, &first);
    let (second, v2) = revise(&c, &target, &v1, moved.clone().into());
    c.amend(&steward, &second);
    out.push(
        Case::new(
            "revision_accepted",
            "Two revisions: one removes a key whose value is byte-identical \
             to another's, one moves a key. The stored data is untouched and \
             hashes to the attested data_hash; applying each RFC 6902 patch \
             in chain order produces each resulting_data_hash.",
            &steward_pk,
            pinned.clone(),
            c.links.clone(),
        )
        .content(target.clone(), data.clone()),
    );
    out.push(
        Case::new(
            "revision_latest_served",
            "The same chain, content read as the latest version: it hashes \
             to the last revision's resulting_data_hash.",
            &steward_pk,
            pinned.clone(),
            c.links.clone(),
        )
        .content(target.clone(), v2),
    );

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (mut lying, _) = revise(&c, &target, &data, dedup(&data));
    lying
        .amendment
        .revision
        .as_mut()
        .unwrap()
        .resulting_data_hash = data_hash(&json!({"title": "Something else"}));
    c.amend(&steward, &lying);
    out.push(
        Case::new(
            "revision_wrong_hash",
            "A revision whose patch does not produce the resulting_data_hash \
             it claims.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, data.clone()),
    );

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (mut broken, _) = revise(&c, &target, &data, dedup(&data));
    let broken_revision = broken.amendment.revision.as_mut().unwrap();
    broken_revision.patch =
        serde_json::from_value(json!([{"op": "remove", "path": "/nowhere"}]))
            .unwrap();
    broken_revision.duplicates.clear();
    c.amend(&steward, &broken);
    out.push(
        Case::new(
            "revision_does_not_apply",
            "A well-formed revision whose patch does not apply to the \
             stored data.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, data.clone()),
    );

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (mut empty, _) = revise(&c, &target, &data, moved.clone().into());
    empty.amendment.revision.as_mut().unwrap().patch =
        json_patch::Patch(vec![]);
    c.amend(&steward, &empty);
    out.push(Case::new(
        "revision_empty_patch",
        "A revision with an empty patch: malformed.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (revision, _) = revise(&c, &target, &data, dedup(&data));
    c.amend(&steward, &revision);
    let rev = revision.amendment.revision.clone().unwrap();
    let (redaction, redacted) =
        redact(&c, &target, "/responses/0/rationale", &data, &[&rev]);
    c.amend(&steward, &redaction);
    out.push(
        Case::new(
            "revision_then_redaction",
            "A revision removes a duplicate; a redaction then erases the \
             value it duplicated, and the stored copy with it. The revision \
             is rebased over the redacted data: its own resulting hash is \
             superseded, and the redaction's resulting_latest_hash holds.",
            &steward_pk,
            pinned.clone(),
            c.links.clone(),
        )
        .content(target.clone(), redacted.clone()),
    );
    let mut lie = redaction.clone();
    lie.amendment
        .redaction
        .as_mut()
        .unwrap()
        .resulting_latest_hash = Some(data_hash(&json!({})));
    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    c.amend(&steward, &revision);
    c.amend(&steward, &lie);
    out.push(
        Case::new(
            "redaction_wrong_latest_hash",
            "The same redaction, claiming a resulting_latest_hash the \
             rebased revisions do not produce.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, redacted),
    );

    let mut c = Chain::new();
    let target = c.entry(&steward, data.clone());
    let (redaction, redacted) =
        redact(&c, &target, "/subject/handle", &data, &[]);
    c.amend(&steward, &redaction);
    let (revision, _) = revise(&c, &target, &redacted, dedup(&redacted));
    c.amend(&steward, &revision);
    out.push(
        Case::new(
            "redaction_then_revision",
            "A redaction, then a revision of what it left: the patch is \
             checked against the redacted data.",
            &steward_pk,
            pinned.clone(),
            c.links,
        )
        .content(target, redacted),
    );

    // -- forgery --

    let mut c = Chain::new();
    let target = c.decision(&steward);
    let fake = AmendmentDraft::new(
        target,
        c.hash_at(1),
        AmendmentKind::Overruled,
        "none",
        "overruled, says nobody with the key",
    )
    .unwrap();
    c.amend(&forger, &fake);
    let grab = c.routine(&steward_pk, &forger);
    c.rotate(&forger, &grab);
    out.push(Case::new(
        "forged_entries_no_effects",
        "An amendment and a rotation signed by a key the chain never named. \
         Neither amends nor moves anything — the rotation is even \
         root-certified, which is no licence to skip the old key's signature.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    // -- key rotation, v2: the root certifies, nobody else --

    let mut c = Chain::new();
    c.decision(&steward);
    let rotation = c.routine(&steward_pk, &successor);
    c.rotate(&steward, &rotation);
    c.decision(&successor);
    let routine = c.links;
    out.push(Case::new(
        "rotation_v2_routine",
        "A scheduled rotation, signed by the old key and certified by a \
         root. The anchor has never heard of the new key and does not \
         need to.",
        &steward_pk,
        pinned.clone(),
        routine.clone(),
    ));
    out.push(Case::new(
        "rotation_v2_genesis_certified",
        "The same chain to a verifier with no anchor at all: the first \
         rotation carries the genesis key's retroactive certificate, so \
         nothing is unanchored.",
        &steward_pk,
        Vec::new(),
        routine,
    ));
    out.push(Case::new(
        "genesis_unanchored",
        "No rotation, no anchor: the genesis key is reported, not rejected.",
        &steward_pk,
        Vec::new(),
        chain(&steward, 2),
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let out_key = c.routine(&steward_pk, &successor);
    c.rotate(&steward, &out_key);
    let back = c.routine(&successor_pk, &steward);
    c.rotate(&successor, &back);
    out.push(Case::new(
        "rotation_v2_reused_key",
        "A certified rotation back to a key that has already held the \
         chain. Not even the root brings a key back.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let mut rotation = c.routine(&steward_pk, &successor);
    let statement = rotation.statement(c.prev_hash());
    rotation.proof = crypto::sign(
        &steward,
        statement.hash().as_bytes(),
        rotation.proof_signed_at,
    )
    .into();
    c.rotate(&steward, &rotation);
    out.push(Case::new(
        "rotation_v2_forged_proof",
        "A certified rotation whose proof of possession is signed by the \
         old key instead of the new one: a key nobody has shown they hold.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.decision(&steward);
    let reattested = c.decision(&steward);
    let declaration = c.compromise(&steward_pk, &successor, 1);
    c.rotate(&successor, &declaration);
    let vouch = AmendmentDraft::new(
        reattested,
        c.hash_at(3),
        AmendmentKind::Reattested,
        "Art. VII",
        "independently verified; the Steward vouches for it",
    )
    .unwrap();
    c.amend(&successor, &vouch);
    out.push(Case::new(
        "rotation_v2_compromise",
        "A compromise declaration signed by the certified new key: the \
         window after the last trusted entry is repudiated, and a \
         reattestation restores one entry of it.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.decision(&steward);
    let mut declaration = c.compromise(&steward_pk, &thief, 1);
    let statement = declaration.certificate.statement.clone();
    declaration.certificate = certify(&[&thief], statement);
    c.rotate(&thief, &declaration);
    out.push(Case::new(
        "rotation_v2_compromise_uncertified",
        "The same declaration certified by its own new key instead of a \
         root — in the anchor, even. It authenticates nothing, and nothing \
         is repudiated on its say-so.",
        &steward_pk,
        vec![
            PublicKeyHex::from(&steward_pk),
            PublicKeyHex::from(&thief.verifying_key()),
        ],
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    c.decision(&steward); // the thief's, as it turns out
    let routine = c.routine(&steward_pk, &successor);
    c.rotate(&steward, &routine);
    c.decision(&successor);
    let declaration = c.compromise(&steward_pk, &recovery, 1);
    c.rotate(&recovery, &declaration);
    out.push(Case::new(
        "rotation_v2_rotation_inside_the_window",
        "The key was stolen before anyone knew. The Steward rotates \
         routinely, then learns of it and names a head from before the \
         rotation: the rotation is void with the rest of the window, and \
         the genesis certificate it carried is the root's word all the same.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward); // 1 — the last entry the first declaration trusts
    c.decision(&steward); // 2 — inside the first window
    let first = c.compromise(&steward_pk, &successor, 1);
    c.rotate(&successor, &first);
    let second = c.compromise(&steward_pk, &recovery, 2);
    c.rotate(&recovery, &second);
    out.push(Case::new(
        "rotation_v2_second_compromise_inside_the_window",
        "A second, certified compromise declaration naming, as its last \
         trusted entry, one the first declaration repudiated. Trust cannot \
         be anchored inside a window nobody trusts.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let real = c.compromise(&steward_pk, &successor, 1);
    c.rotate(&successor, &real);
    c.decision(&successor);
    let hijack = c.compromise(&successor_pk, &steward, 3);
    c.rotate(&steward, &hijack);
    c.decision(&steward);
    out.push(Case::new(
        "rotation_v2_stolen_key_not_restored",
        "The thief, still holding the compromised key, declares the \
         Steward's replacement compromised and names the stolen key as the \
         new one.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let rotation = c.routine(&steward_pk, &successor);
    let mut data = serde_json::to_value(&rotation).unwrap();
    data["note"] = json!("at the request of the Steward");
    c.push(
        &steward,
        key_id(1),
        GovernanceLogEntryType::KeyRotation,
        data,
        true,
    );
    out.push(Case::new(
        "rotation_v2_free_text",
        "A well-formed, certified rotation with one extra field. A rotation \
         can never be redacted, so it carries no free text: malformed.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let v1 = RotationStatementV1 {
        agora_governance_key_rotation: 1,
        reason: RotationReason::Routine,
        old_key: (&steward_pk).into(),
        new_key: (&successor_pk).into(),
        prev_hash: c.prev_hash(),
    };
    let proof =
        crypto::sign(&successor, &Sha256::digest(v1.preimage()), 1_700_000_025);
    c.push(
        &steward,
        key_id(1),
        GovernanceLogEntryType::KeyRotation,
        json!({
            "agora_governance_key_rotation": 1,
            "reason": "routine",
            "old_key": PublicKeyHex::from(&steward_pk),
            "new_key": PublicKeyHex::from(&successor_pk),
            "proof": SignatureHex::from(proof),
            "proof_signed_at": 1_700_000_025,
            "note": "scheduled rotation",
        }),
        true,
    );
    c.decision(&successor);
    out.push(Case::new(
        "rotation_v1_refused",
        "A version 1 rotation, exactly as 0.26 accepted it: old key's \
         signature, valid proof of possession, no certificate. It never \
         appeared on the platform and moves nothing now.",
        &steward_pk,
        both.clone(),
        c.links,
    ));

    // -- certificates --

    // Each case is `rotation_v2_routine` with one thing wrong with who
    // signed what.
    let tamper = |name: &'static str,
                  description: &'static str,
                  threshold: usize,
                  edit: &dyn Fn(&Chain, &mut KeyRotation)| {
        let mut c = Chain::new();
        c.decision(&steward);
        let mut rotation = c.routine(&steward_pk, &successor);
        if threshold > 1 {
            rotation.outgoing_certificate = Some(certify(
                &[&root(1), &root(2)],
                KeyCertStatement::genesis((&steward_pk).into()),
            ));
        }
        edit(&c, &mut rotation);
        c.rotate(&steward, &rotation);
        c.decision(&successor);
        Case::new(name, description, &steward_pk, pinned.clone(), c.links)
            .threshold(threshold)
    };
    let resign = |rotation: &mut KeyRotation, signers: &[&SigningKey]| {
        let statement = rotation.certificate.statement.clone();
        rotation.certificate = certify(signers, statement);
    };

    out.push(tamper(
        "certificate_online_key",
        "What a thief holding the online key can produce: a rotation to \
         their own key, certified by the key they stole. The online key is \
         not a root.",
        1,
        &|_, r| resign(r, &[&steward]),
    ));
    out.push(tamper(
        "certificate_unknown_root",
        "Certified by a key that is not in the verifier's root set.",
        1,
        &|_, r| resign(r, &[&forger]),
    ));
    out.push(tamper(
        "certificate_unknown_root_beside_a_known_one",
        "A signature from outside the root set next to a sufficient one: \
         ignored, not an error — a later root set may overlap this one.",
        1,
        &|_, r| resign(r, &[&forger, &root(2)]),
    ));
    out.push(tamper(
        "certificate_wrong_prefix",
        "A root's valid signature over the statement's canonical JSON \
         without the domain prefix.",
        1,
        &|_, r| {
            use ed25519_dalek::Signer;
            let statement = r.certificate.statement.clone();
            let bare =
                canonical_json(&serde_json::to_value(&statement).unwrap());
            r.certificate =
                KeyCertificate::unsigned(statement).with(RootSignature {
                    root_key: (&root(1).verifying_key()).into(),
                    signature: root(1).sign(&bare).into(),
                });
        },
    ));
    out.push(tamper(
        "certificate_wrong_position",
        "A genuine certificate for this key, issued for the position one \
         entry later.",
        1,
        &|c, r| {
            r.certificate = certify(
                &[&root(1)],
                KeyCertStatement::routine(
                    r.new_key,
                    c.next_seq() + 1,
                    c.prev_hash(),
                ),
            );
        },
    ));
    out.push(tamper(
        "certificate_wrong_prev_hash",
        "A genuine certificate for this key and seq, bound to a different \
         predecessor: another chain, or this one before it was rewritten.",
        1,
        &|c, r| {
            r.certificate = certify(
                &[&root(1)],
                KeyCertStatement::routine(
                    r.new_key,
                    c.next_seq(),
                    Some(data_hash(&json!("another chain"))),
                ),
            );
        },
    ));
    out.push(tamper(
        "certificate_altered_statement",
        "A genuine certificate for another key, its statement edited \
         afterwards to name this one.",
        1,
        &|c, r| {
            r.certificate = certify(
                &[&root(1)],
                KeyCertStatement::routine(
                    (&recovery_pk).into(),
                    c.next_seq(),
                    c.prev_hash(),
                ),
            );
            r.certificate.statement.key = r.new_key;
        },
    ));
    out.push(tamper(
        "certificate_wrong_purpose",
        "A compromise certificate presented for a routine rotation.",
        1,
        &|c, r| {
            r.certificate = certify(
                &[&root(1)],
                KeyCertStatement::compromise(
                    r.new_key,
                    c.next_seq(),
                    c.prev_hash(),
                    c.head(1),
                ),
            );
        },
    ));
    out.push(tamper(
        "certificate_two_of_two",
        "A root set that needs two signers, and gets them.",
        2,
        &|_, r| resign(r, &[&root(1), &root(2)]),
    ));
    out.push(tamper(
        "certificate_below_threshold",
        "A root set that needs two signers, and gets one.",
        2,
        &|_, r| resign(r, &[&root(1)]),
    ));
    out.push(tamper(
        "certificate_duplicate_signer",
        "A root set that needs two signers, and gets the same one twice.",
        2,
        &|_, r| resign(r, &[&root(1), &root(1)]),
    ));
    out.push(tamper(
        "certificate_missing_genesis",
        "The chain's first rotation without the genesis key's certificate.",
        1,
        &|_, r| r.outgoing_certificate = None,
    ));
    out.push(tamper(
        "certificate_genesis_for_another_key",
        "The chain's first rotation carrying a genuine genesis certificate \
         — for a key that is not the one the chain started under.",
        1,
        &|_, r| {
            r.outgoing_certificate = Some(certify(
                &[&root(1)],
                KeyCertStatement::genesis((&recovery_pk).into()),
            ));
        },
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let first = c.routine(&steward_pk, &successor);
    let genesis = first.outgoing_certificate.clone().unwrap();
    c.rotate(&steward, &first);
    let second = c.routine(&successor_pk, &recovery).with_outgoing(genesis);
    c.rotate(&successor, &second);
    out.push(Case::new(
        "certificate_second_genesis",
        "A later rotation carrying the genesis certificate again. It \
         belongs on the first rotation and nowhere else.",
        &steward_pk,
        pinned.clone(),
        c.links,
    ));

    let mut c = Chain::new();
    c.decision(&steward);
    let rotation = c.routine(&steward_pk, &successor);
    let mut data = serde_json::to_value(&rotation).unwrap();
    data["certificate"]["statement"]["comment"] = json!("signed at home");
    c.push(
        &steward,
        key_id(1),
        GovernanceLogEntryType::KeyRotation,
        data,
        true,
    );
    out.push(Case::new(
        "certificate_unknown_statement_field",
        "A statement with a field the verifier does not know. What the \
         root signed is exactly what the verifier rebuilds, so there is no \
         room for one: malformed.",
        &steward_pk,
        pinned.clone(),
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
        let actual = observe(
            &vector.links,
            &genesis,
            &vector.anchor,
            &RootSet::new(
                vector.root_keys.iter().copied(),
                vector.root_threshold,
            ),
            &vector.contents,
        );
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

/// The Rust half of `just fuzz`: a verdict, or a refusal, for every mutated
/// vector `tools/fuzz_verifiers.py` left in `AGORA_FUZZ_DIR`, written next
/// to it for the Python half to compare with its own.
#[test]
#[ignore = "driven by tools/fuzz_verifiers.py; run it with `just fuzz`"]
fn observe_the_fuzz_corpus() {
    let dir = PathBuf::from(
        std::env::var("AGORA_FUZZ_DIR")
            .expect("AGORA_FUZZ_DIR names the corpus directory"),
    );
    for entry in std::fs::read_dir(&dir).expect("a readable corpus") {
        let path = entry.expect("a readable directory entry").path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable mutant");
        // A link that does not parse is not a chain: a refusal, not a
        // verdict, in both implementations.
        let verdict = match serde_json::from_str::<Vector>(&text) {
            Ok(vector) => {
                let genesis = vector
                    .genesis_key
                    .to_verifying_key()
                    .expect("the fuzzer leaves the genesis key alone");
                let roots = RootSet::new(
                    vector.root_keys.iter().copied(),
                    vector.root_threshold,
                );
                serde_json::to_value(observe(
                    &vector.links,
                    &genesis,
                    &vector.anchor,
                    &roots,
                    &vector.contents,
                ))
                .expect("an Expect always serializes")
            }
            Err(_) => json!("refused"),
        };
        std::fs::write(path.with_extension("rust"), verdict.to_string())
            .expect("a writable corpus");
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

    let (_, rest) = text
        .split_once("ROOT_KEYS = [")
        .expect("the script declares ROOT_KEYS");
    let (list, rest) = rest.split_once(']').expect("the declaration ends");
    let keys: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
    assert_eq!(keys, ROOT_KEYS, "{}", path.display());
    assert!(
        rest.contains(&format!("ROOT_THRESHOLD = {ROOT_THRESHOLD}\n")),
        "{}",
        path.display()
    );
}
