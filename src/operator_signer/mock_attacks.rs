//! Attacks on the PIV chain checks, from the adversarial re-review of #235,
//! kept as regression tests.

use super::*;
use crate::operator_signer::piv::YubicoAttestationVerifier;

const PIN: &str = "975310";
const SERIAL: PivSerial = PivSerial::new(12_345_678);
const RSA_ROOT: &[u8] = include_bytes!(
    "../../tests/fixtures/operator_signer/mock_rsa_root_pkcs1.der"
);

fn mock_att(bundle: AttestationBundle) -> MockPivAttestation {
    match bundle {
        AttestationBundle::MockPiv(a) => a,
        _ => panic!(),
    }
}

fn p256_policy_never() -> KeyPolicy {
    KeyPolicy {
        pin: PinPolicy::Never,
        touch: TouchPolicy::Never,
    }
}

async fn setup(
    root: &MockRootCa,
) -> (MockPivDevice, MockHardwareKey, MockPivAttestation) {
    let device = MockPivDevice::manufacture(
        root,
        SERIAL,
        MockPivDevice::FIRMWARE_P256_ONLY,
        Pin::new(PIN),
    );
    let slot = device
        .generate_key(
            PivSlot::Authentication,
            SignatureAlgorithm::EcdsaP256,
            p256_policy_never(),
        )
        .unwrap();
    let real = mock_att(slot.attestation().await.unwrap());
    (device, slot, real)
}

fn subject_of(der: &DerCertificate) -> Name {
    Certificate::from_der(der.as_der())
        .unwrap()
        .tbs_certificate()
        .subject()
        .clone()
}

fn slot_exts(serial: Vec<u8>, policy: Vec<u8>) -> Vec<Extension> {
    vec![
        extension(piv::OID_YUBICO_FIRMWARE, vec![5, 7, 2]),
        extension(piv::OID_YUBICO_SERIAL, serial),
        extension(piv::OID_YUBICO_POLICY, policy),
    ]
}

/// The slot key signs a "leaf" for `claimed` with `issuer` as issuer.
async fn slot_signs(
    slot: &MockHardwareKey,
    issuer: &Name,
    claimed: &OperatorPublicKey,
    extensions: Vec<Extension>,
) -> DerCertificate {
    let algorithm = AlgorithmIdentifierOwned {
        oid: piv::OID_ECDSA_SHA256,
        parameters: None,
    };
    let tbs = tbs_certificate(
        7,
        &algorithm,
        issuer,
        &Name::from_str("CN=forged").unwrap(),
        piv::spki(claimed),
        extensions,
    );
    let OperatorSignature::EcdsaP256(signature) =
        slot.sign(&tbs).await.unwrap()
    else {
        panic!()
    };
    assemble(tbs, &algorithm, &piv::ecdsa_to_der(&signature.to_bytes()))
}

fn ed_claim() -> OperatorPublicKey {
    let (_, vk) = crate::crypto::generate_keypair();
    OperatorPublicKey::Ed25519(vk)
}

// ---------------------------------------------------------------------------
// 1. The original attack and its chain-shape variants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a1_original_attack_variants() {
    let root = MockRootCa::generate();
    let (_device, slot, real) = setup(&root).await;
    let verifier = MockAttestationVerifier::new(&root);
    let claimed = ed_claim();
    let slot_name = subject_of(&real.slot_certificate);
    let forged = slot_signs(
        &slot,
        &slot_name,
        &claimed,
        slot_exts(99_999_999u32.to_der().unwrap(), vec![3, 2]),
    )
    .await;
    let f9 = real.device_chain[0].clone();
    let root_cert = root.certificate().clone();

    let cases: Vec<(&str, Vec<DerCertificate>)> = vec![
        (
            "[slot, f9]",
            vec![real.slot_certificate.clone(), f9.clone()],
        ),
        ("[slot]", vec![real.slot_certificate.clone()]),
        (
            "[slot, f9, root]",
            vec![real.slot_certificate.clone(), f9.clone(), root_cert.clone()],
        ),
        (
            "[slot, root]",
            vec![real.slot_certificate.clone(), root_cert.clone()],
        ),
        ("[]", vec![]),
        (
            "[f9, slot]",
            vec![f9.clone(), real.slot_certificate.clone()],
        ),
        (
            "[forged, slot]",
            vec![forged.clone(), real.slot_certificate.clone()],
        ),
        ("[root]", vec![root_cert.clone()]),
    ];
    for (name, chain) in cases {
        let r = verifier.verify(
            &MockPivAttestation {
                slot_certificate: forged.clone(),
                device_chain: chain,
            },
            &claimed,
        );
        assert!(r.is_err(), "{name} accepted");
    }

    // The real slot cert with the leaf duplicated as its own issuer.
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: real.slot_certificate.clone(),
            device_chain: vec![real.slot_certificate.clone()],
        },
        &slot.public_key(),
    );
    assert!(r.is_err());

    // Root presented as part of the chain: [f9, root]. Benign if accepted.
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: real.slot_certificate.clone(),
            device_chain: vec![f9.clone(), root_cert.clone()],
        },
        &slot.public_key(),
    );
    r.unwrap();
}

/// The slot key issues a CA "intermediate" in place of f9.
#[tokio::test]
async fn a2_slot_key_issues_a_ca_in_place_of_f9() {
    // Newer hierarchy: root -> intermediate -> f9 -> slot.
    let root = MockRootCa::generate();
    let inter = root.intermediate_p384();
    let (_device, slot, real) = setup(&inter).await;
    let verifier = MockAttestationVerifier::new(&root);
    let claimed = ed_claim();
    let slot_name = subject_of(&real.slot_certificate);
    // A forged CA cert X issued by the slot key (CA, no policy), whose key
    // is attacker's.
    let fake_key = random_p256();
    let algorithm = AlgorithmIdentifierOwned {
        oid: piv::OID_ECDSA_SHA256,
        parameters: None,
    };
    let x_name = Name::from_str("CN=X").unwrap();
    let tbs = tbs_certificate(
        9,
        &algorithm,
        &slot_name,
        &x_name,
        p256_spki(&fake_key),
        ca_extensions(),
    );
    let OperatorSignature::EcdsaP256(sig) = slot.sign(&tbs).await.unwrap()
    else {
        panic!()
    };
    let x = assemble(tbs, &algorithm, &piv::ecdsa_to_der(&sig.to_bytes()));
    let leaf = issue(
        1,
        &x_name,
        &Name::from_str("CN=leaf").unwrap(),
        piv::spki(&claimed),
        slot_exts(1u32.to_der().unwrap(), vec![3, 2]),
        &IssuerKey::EcdsaP256(fake_key),
    );
    for chain in [
        vec![
            x.clone(),
            real.slot_certificate.clone(),
            real.device_chain[0].clone(),
            real.device_chain[1].clone(),
        ],
        vec![
            x.clone(),
            real.slot_certificate.clone(),
            real.device_chain[0].clone(),
        ],
        vec![x.clone(), real.slot_certificate.clone()],
    ] {
        let r = verifier.verify(
            &MockPivAttestation {
                slot_certificate: leaf.clone(),
                device_chain: chain,
            },
            &claimed,
        );
        assert!(r.is_err());
    }
}

/// Defence in depth: if Yubico's f9 certificates ARE CAs (cA=TRUE), and some
/// root-chained certificate lacked the policy extension, does anything else
/// stop the forged leaf? This simulates a slot cert without 3.8.
#[tokio::test]
async fn a3_only_the_policy_extension_stands_when_f9_is_a_ca() {
    let root = MockRootCa::generate();
    let (device, slot, _) = setup(&root).await;
    {
        let mut st = device.lock();
        // Re-issue f9 as a CA (cA=TRUE, keyCertSign).
        st.attestation_certificate = issue(
            u64::from(SERIAL.get()),
            &root.name,
            &device_name(),
            st.attestation_key.spki(),
            ca_extensions(),
            &root.key,
        );
    }
    let verifier = MockAttestationVerifier::new(&root);
    // Sanity: real attestation with CA-f9 still verifies.
    let real = mock_att(slot.attestation().await.unwrap());
    verifier.verify(&real, &slot.public_key()).unwrap();

    // A slot cert like the real one, but without the policy extension
    // (signed by the real f9 key).
    let slot_name = Name::from_str("CN=slot-no-policy").unwrap();
    let (no_policy_slot, f9) = {
        let st = device.lock();
        let no_policy_slot = issue(
            0x9a,
            &device_name(),
            &slot_name,
            piv::spki(&slot.public_key()),
            vec![
                extension(piv::OID_YUBICO_FIRMWARE, vec![5, 4, 3]),
                extension(
                    piv::OID_YUBICO_SERIAL,
                    SERIAL.get().to_der().unwrap(),
                ),
            ],
            &st.attestation_key,
        );
        (no_policy_slot, st.attestation_certificate.clone())
    };
    let claimed = ed_claim();
    let forged = slot_signs(
        &slot,
        &slot_name,
        &claimed,
        slot_exts(99_999_999u32.to_der().unwrap(), vec![3, 2]),
    )
    .await;
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: forged,
            device_chain: vec![no_policy_slot, f9],
        },
        &claimed,
    );
    assert!(
        matches!(r, Err(AttestationError::ChainLength { len: 2 })),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. CA rules
// ---------------------------------------------------------------------------

fn custom_root(name: &str, exts: Vec<Extension>) -> MockRootCa {
    let key = IssuerKey::EcdsaP256(random_p256());
    let name = Name::from_str(name).unwrap();
    let certificate = issue(1, &name, &name, key.spki(), exts, &key);
    MockRootCa {
        key,
        name,
        certificate,
        chain_to_root: vec![],
    }
}

fn bc(ca: bool, path_len_constraint: Option<u8>) -> Extension {
    critical_extension(
        &BasicConstraints {
            ca,
            path_len_constraint,
        },
        OID_BASIC_CONSTRAINTS,
    )
}

/// Anchor rules; `f9`'s are `mock::tests::f9_must_be_a_ca`.
#[tokio::test]
async fn b1_ca_rules() {
    let ku =
        |u: KeyUsages| critical_extension(&KeyUsage(u.into()), OID_KEY_USAGE);
    // An anchor that isn't a CA is refused on construction.
    let bad = custom_root("CN=root not ca", vec![]);
    assert!(
        MockAttestationVerifier::from_certificates(std::slice::from_ref(
            bad.certificate()
        ))
        .is_err()
    );
    let bad = custom_root(
        "CN=root ku",
        vec![bc(true, None), ku(KeyUsages::DigitalSignature)],
    );
    assert!(
        MockAttestationVerifier::from_certificates(std::slice::from_ref(
            bad.certificate()
        ))
        .is_err()
    );

    // An anchor with pathLen=0 can't issue `f9`, which is a CA.
    let r0 = custom_root("CN=root plen0", vec![bc(true, Some(0))]);
    let (_, slot, real) = setup(&r0).await;
    let r = MockAttestationVerifier::new(&r0).verify(&real, &slot.public_key());
    assert!(
        matches!(r, Err(AttestationError::PathLength { index: 2 })),
        "{r:?}"
    );
    // pathLen=1 can.
    let r1 = custom_root("CN=root plen1", vec![bc(true, Some(1))]);
    let (_, slot, real) = setup(&r1).await;
    MockAttestationVerifier::new(&r1)
        .verify(&real, &slot.public_key())
        .unwrap();

    // The slot certificate may not be a CA.
    let root = MockRootCa::generate();
    let (device, slot, real) = setup(&root).await;
    let st = device.lock();
    let leaf = issue(
        0x9a,
        &device_name(),
        &Name::from_str("CN=slot").unwrap(),
        piv::spki(&slot.public_key()),
        [
            vec![bc(true, None)],
            slot_exts(SERIAL.get().to_der().unwrap(), vec![1, 1]),
        ]
        .concat(),
        &st.attestation_key,
    );
    drop(st);
    let r = MockAttestationVerifier::new(&root).verify(
        &MockPivAttestation {
            slot_certificate: leaf,
            device_chain: real.device_chain.clone(),
        },
        &slot.public_key(),
    );
    assert!(
        matches!(r, Err(AttestationError::UnexpectedCa { index: 0 })),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Algorithm confusion
// ---------------------------------------------------------------------------

/// Re-wrap `cert` with `alg` as both inner and outer algorithm, with
/// `signature` (signed by `signer` over the new TBS if given).
fn rewrap(
    cert: &DerCertificate,
    alg: AlgorithmIdentifierOwned,
    sign: impl Fn(&[u8]) -> Vec<u8>,
) -> DerCertificate {
    let c = Certificate::from_der(cert.as_der()).unwrap();
    let t = c.tbs_certificate();
    let exts = t.extensions().map(|e| e.to_vec()).unwrap_or_default();
    let tbs = tbs_certificate(
        5,
        &alg,
        t.issuer(),
        t.subject(),
        t.subject_public_key_info().clone(),
        exts,
    );
    let sig = sign(&tbs);
    assemble(tbs, &alg, &sig)
}

#[tokio::test]
async fn c1_algorithm_confusion() {
    let root = MockRootCa::generate();
    let (device, slot, real) = setup(&root).await;
    let verifier = MockAttestationVerifier::new(&root);
    let st = device.lock();
    let IssuerKey::EcdsaP256(f9key) = &st.attestation_key else {
        panic!()
    };
    let f9key = f9key.clone();
    drop(st);

    // ECDSA-SHA384 by a P-256 key (hash/curve mismatch), validly signed.
    let sha384 = AlgorithmIdentifierOwned {
        oid: piv::OID_ECDSA_SHA384,
        parameters: None,
    };
    let leaf = rewrap(&real.slot_certificate, sha384, |tbs| {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        use sha2::Digest;
        let d = sha2::Sha384::digest(tbs);
        let s: p256::ecdsa::Signature = f9key.sign_prehash(&d).unwrap();
        piv::ecdsa_to_der(&s.to_bytes())
    });
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: leaf,
            device_chain: real.device_chain.clone(),
        },
        &slot.public_key(),
    );
    assert!(r.is_err(), "P-256 key with SHA-384: {r:?}");

    // ecdsa-with-SHA256 with NULL parameters, validly signed.
    let with_null = AlgorithmIdentifierOwned {
        oid: piv::OID_ECDSA_SHA256,
        parameters: Some(Any::from(Null)),
    };
    let leaf = rewrap(&real.slot_certificate, with_null, |tbs| {
        let s: p256::ecdsa::Signature = f9key.sign(tbs);
        piv::ecdsa_to_der(&s.to_bytes())
    });
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: leaf,
            device_chain: real.device_chain.clone(),
        },
        &slot.public_key(),
    );
    assert!(
        matches!(r, Err(AttestationError::AlgorithmMismatch { index: 0 })),
        "{r:?}"
    );

    // RSA-SHA256 outer over a P-256 issuer key.
    let rsa_alg = AlgorithmIdentifierOwned {
        oid: piv::OID_RSA_SHA256,
        parameters: Some(Any::from(Null)),
    };
    let leaf = rewrap(&real.slot_certificate, rsa_alg, |_| vec![1u8; 256]);
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: leaf,
            device_chain: real.device_chain.clone(),
        },
        &slot.public_key(),
    );
    assert!(r.is_err(), "RSA algorithm, P-256 key: {r:?}");

    // Control: same re-wrap with the right algorithm verifies.
    let ok_alg = AlgorithmIdentifierOwned {
        oid: piv::OID_ECDSA_SHA256,
        parameters: None,
    };
    let leaf = rewrap(&real.slot_certificate, ok_alg, |tbs| {
        let s: p256::ecdsa::Signature = f9key.sign(tbs);
        piv::ecdsa_to_der(&s.to_bytes())
    });
    verifier
        .verify(
            &MockPivAttestation {
                slot_certificate: leaf,
                device_chain: real.device_chain.clone(),
            },
            &slot.public_key(),
        )
        .unwrap();

    // High-s on a certificate link is accepted by design: it is still the
    // issuer's signature, and nothing keys on a certificate's signature.
    let leaf = rewrap(
        &real.slot_certificate,
        AlgorithmIdentifierOwned {
            oid: piv::OID_ECDSA_SHA256,
            parameters: None,
        },
        |tbs| {
            let s: p256::ecdsa::Signature = f9key.sign(tbs);
            let (r, s) = s.split_scalars();
            let high = p256::ecdsa::Signature::from_scalars(r, -s).unwrap();
            piv::ecdsa_to_der(&high.to_bytes())
        },
    );
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: leaf,
            device_chain: real.device_chain.clone(),
        },
        &slot.public_key(),
    );
    r.unwrap();
}

// ---------------------------------------------------------------------------
// 4. Serial and policy extensions (signed by the real f9 key)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn d1_serial_and_policy_encodings() {
    let root = MockRootCa::generate();
    let (device, slot, real) = setup(&root).await;
    let verifier = MockAttestationVerifier::new(&root);
    let fw = || extension(piv::OID_YUBICO_FIRMWARE, vec![5, 4, 3]);
    let ser = |v: Vec<u8>| extension(piv::OID_YUBICO_SERIAL, v);
    let pol = || extension(piv::OID_YUBICO_POLICY, vec![1, 1]);
    let crit = |e: Extension| Extension {
        critical: true,
        ..e
    };
    // Some(serial) where the extensions are accepted.
    let cases: Vec<(&str, Vec<Extension>, Option<u32>)> = vec![
        (
            "canonical 12345678",
            vec![fw(), ser(vec![2, 4, 0x00, 0xbc, 0x61, 0x4e]), pol()],
            Some(12_345_678),
        ),
        (
            "canonical 12345678 (der)",
            vec![fw(), ser(12_345_678u32.to_der().unwrap()), pol()],
            Some(12_345_678),
        ),
        (
            "leading zero 12345678",
            vec![fw(), ser(vec![2, 5, 0, 0x00, 0xbc, 0x61, 0x4e]), pol()],
            None,
        ),
        (
            "serial 0x80000000 canonical",
            vec![fw(), ser(vec![2, 5, 0, 0x80, 0, 0, 0]), pol()],
            Some(0x8000_0000),
        ),
        (
            "serial 0x80000000 negative",
            vec![fw(), ser(vec![2, 4, 0x80, 0, 0, 0]), pol()],
            None,
        ),
        (
            "negative -1",
            vec![fw(), ser(vec![2, 1, 0xff]), pol()],
            None,
        ),
        (
            "too big 2^32",
            vec![fw(), ser(vec![2, 5, 1, 0, 0, 0, 0]), pol()],
            None,
        ),
        (
            "trailing byte",
            vec![fw(), ser(vec![2, 1, 5, 0]), pol()],
            None,
        ),
        (
            "BER long length",
            vec![fw(), ser(vec![2, 0x81, 1, 5]), pol()],
            None,
        ),
        (
            "OCTET STRING-wrapped",
            vec![fw(), ser(vec![4, 3, 2, 1, 5]), pol()],
            None,
        ),
        ("empty int", vec![fw(), ser(vec![2, 0]), pol()], None),
        (
            "serial twice",
            vec![fw(), ser(vec![2, 1, 5]), ser(vec![2, 1, 6]), pol()],
            None,
        ),
        (
            "serial critical",
            vec![fw(), crit(ser(vec![2, 1, 5])), pol()],
            None,
        ),
        (
            "policy critical",
            vec![fw(), ser(vec![2, 1, 5]), crit(pol())],
            None,
        ),
        (
            "policy 3 bytes",
            vec![
                fw(),
                ser(vec![2, 1, 5]),
                extension(piv::OID_YUBICO_POLICY, vec![1, 1, 1]),
            ],
            None,
        ),
        (
            "policy 0,0",
            vec![
                fw(),
                ser(vec![2, 1, 5]),
                extension(piv::OID_YUBICO_POLICY, vec![0, 0]),
            ],
            None,
        ),
        ("policy missing", vec![fw(), ser(vec![2, 1, 5])], None),
        ("firmware missing", vec![ser(vec![2, 1, 5]), pol()], None),
    ];
    let st = device.lock();
    for (name, exts, expected) in cases {
        let leaf = issue(
            0x9a,
            &device_name(),
            &Name::from_str("CN=slot").unwrap(),
            piv::spki(&slot.public_key()),
            exts,
            &st.attestation_key,
        );
        let r = verifier.verify(
            &MockPivAttestation {
                slot_certificate: leaf,
                device_chain: real.device_chain.clone(),
            },
            &slot.public_key(),
        );
        let serial = r.as_ref().ok().map(|k| k.serial());
        assert_eq!(
            serial,
            expected.map(|n| DeviceSerial::Mock(PivSerial::new(n))),
            "{name}: {r:?}"
        );
    }
    // Policy extension in f9.
    let f9 = issue(
        u64::from(SERIAL.get()),
        &root.name,
        &device_name(),
        st.attestation_key.spki(),
        vec![pol()],
        &root.key,
    );
    drop(st);
    let r = verifier.verify(
        &MockPivAttestation {
            slot_certificate: real.slot_certificate.clone(),
            device_chain: vec![f9],
        },
        &slot.public_key(),
    );
    assert!(matches!(
        r,
        Err(AttestationError::PolicyAboveLeaf { index: 1 })
    ));
}

// ---------------------------------------------------------------------------
// 5. Names and roots
// ---------------------------------------------------------------------------

/// Re-encode every UTF8String (0x0c) in a Name as PrintableString (0x13).
fn printable(name: &Name) -> Name {
    let mut der = name.to_der().unwrap();
    // Name is SEQ{ SET{ SEQ{ OID, value } } } for single-CN names: find the
    // value tag after the CN OID 55 04 03.
    let pos = der
        .windows(3)
        .position(|w| w == [0x55, 0x04, 0x03])
        .unwrap()
        + 3;
    assert_eq!(der[pos], 0x0c);
    der[pos] = 0x13;
    Name::from_der(&der).unwrap()
}

#[tokio::test]
async fn e1_names() {
    // Two trusted roots with the same subject and different keys: both
    // must work (every same-named root tried).
    let r1 = custom_root(
        "CN=Shared Root",
        vec![critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: None,
            },
            OID_BASIC_CONSTRAINTS,
        )],
    );
    let r2 = custom_root(
        "CN=Shared Root",
        vec![critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: None,
            },
            OID_BASIC_CONSTRAINTS,
        )],
    );
    let both = MockAttestationVerifier::from_certificates(&[
        r1.certificate().clone(),
        r2.certificate().clone(),
    ])
    .unwrap();
    let only1 =
        MockAttestationVerifier::from_certificates(&[r1.certificate().clone()])
            .unwrap();
    let (_, slot, real) = setup(&r2).await;
    both.verify(&real, &slot.public_key()).unwrap();
    let r = only1.verify(&real, &slot.public_key());
    assert!(matches!(r, Err(AttestationError::UntrustedRoot)), "{r:?}");

    // f9's issuer encoded PrintableString while the root's subject is UTF8.
    let root = MockRootCa::generate();
    let (device, slot, real) = setup(&root).await;
    let st = device.lock();
    let f9 = issue(
        1,
        &printable(&root.name),
        &device_name(),
        st.attestation_key.spki(),
        f9_extensions(),
        &root.key,
    );
    drop(st);
    let r = MockAttestationVerifier::new(&root).verify(
        &MockPivAttestation {
            slot_certificate: real.slot_certificate.clone(),
            device_chain: vec![f9],
        },
        &slot.public_key(),
    );
    // Names are compared as encoded: a re-encoded issuer is another name.
    assert!(matches!(r, Err(AttestationError::UntrustedRoot)), "{r:?}");

    // Yubico verifier vs the mock: a mock root re-encoded PrintableString.
    let key = IssuerKey::EcdsaP256(random_p256());
    let pname = printable(&Name::from_str(MOCK_ROOT_SUBJECT).unwrap());
    let cert = issue(1, &pname, &pname, key.spki(), ca_extensions(), &key);
    // The Yubico verifier refuses any anchor whose key isn't pinned.
    let r = YubicoAttestationVerifier::from_anchors(&[cert]);
    assert!(
        matches!(r, Err(AttestationError::RootRefused { .. })),
        "{r:?}"
    );

    let root = MockRootCa::generate();
    let inter = root.intermediate_p384();
    let r = YubicoAttestationVerifier::from_anchors(std::slice::from_ref(
        inter.certificate(),
    ));
    assert!(
        matches!(r, Err(AttestationError::RootRefused { .. })),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. Ed25519 strictness and P-256 low-s at the operator boundary
// ---------------------------------------------------------------------------

#[test]
fn f1_ed25519_strict() {
    // Small-order key (identity) with the all-identity signature: plain
    // `verify` accepts it for any message; `verify_strict` must not.
    let mut id = [0u8; 32];
    id[0] = 1;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&id).unwrap();
    let mut sig = [0u8; 64];
    sig[0] = 1;
    let sig = ed25519_dalek::Signature::from_bytes(&sig);
    let r = OperatorPublicKey::Ed25519(vk)
        .verify(b"anything", &OperatorSignature::Ed25519(sig));
    assert!(r.is_err());

    // EcdsaP256Signature: s = n/2 boundary and s = n/2+1.
    let mut b = [1u8; 64];
    b[32..].copy_from_slice(&[
        0x7f, 0xff, 0xff, 0xff, 0x80, 0x00, 0x00, 0x00, 0x7f, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xde, 0x73, 0x7d, 0x56, 0xd3, 0x8b, 0xcf, 0x42,
        0x79, 0xdc, 0xe5, 0x61, 0x7e, 0x31, 0x92, 0xa8,
    ]);
    assert!(EcdsaP256Signature::from_bytes(b).is_ok());
    b[63] = 0xa9;
    assert!(EcdsaP256Signature::from_bytes(b).is_err());
    // r = 0 is not refused at construction (only s): verify must fail.
    let mut z = [0u8; 64];
    z[63] = 1;
    if let Ok(s) = EcdsaP256Signature::from_bytes(z) {
        let key = OperatorPublicKey::EcdsaP256(EcdsaP256PublicKey::from_p256(
            random_p256().verifying_key(),
        ));
        assert!(key.verify(b"m", &OperatorSignature::EcdsaP256(s)).is_err());
    }
}

// ---------------------------------------------------------------------------
// 7. Panics / DoS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g1_mutation_fuzz_no_panics() {
    let root = MockRootCa::generate();
    let inter = root.intermediate_p384();
    let (_, _slot, real) = setup(&inter).await;
    let roots = TrustedRoots::new(
        std::slice::from_ref(inter.certificate()),
        MOCK_DEVICE_SUBJECT,
    )
    .unwrap();
    let rsa = MockRootCa::rsa_from_pkcs1_der(RSA_ROOT).unwrap();
    let (_, _rslot, rreal) = setup(&rsa).await;
    let rroots = TrustedRoots::new(
        std::slice::from_ref(rsa.certificate()),
        MOCK_DEVICE_SUBJECT,
    )
    .unwrap();
    let mut panics = 0usize;
    let mut accepted_mutants = 0usize;
    let mut runs = 0usize;
    let mut run = |slot_c: &DerCertificate,
                   chain: &[DerCertificate],
                   roots: &TrustedRoots,
                   base: &MockPivAttestation| {
        runs += 1;
        let res =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                piv::verify_attestation(slot_c, chain, roots)
            }));
        match res {
            Err(_) => panics += 1,
            Ok(Ok(_))
                if slot_c != &base.slot_certificate
                    || chain != base.device_chain.as_slice() =>
            {
                accepted_mutants += 1;
            }
            _ => {}
        }
    };
    for (base, roots) in [(&real, &roots), (&rreal, &rroots)] {
        let all: Vec<DerCertificate> =
            std::iter::once(base.slot_certificate.clone())
                .chain(base.device_chain.iter().cloned())
                .collect();
        for which in 0..all.len() {
            let orig = all[which].as_der().to_vec();
            let mut variants: Vec<Vec<u8>> = Vec::new();
            for i in 0..orig.len() {
                for v in [0x00u8, 0xff, 0x80, 0x7f, 0x01] {
                    let mut m = orig.clone();
                    m[i] = v;
                    variants.push(m);
                }
                let mut m = orig.clone();
                m[i] ^= 0x01;
                variants.push(m);
                variants.push(orig[..i].to_vec());
            }
            for m in variants {
                let mut c = all.clone();
                c[which] = DerCertificate::from_der(m);
                run(&c[0], &c[1..], roots, base);
            }
        }
    }
    assert!(runs > 0);
    assert_eq!(panics, 0);
    assert_eq!(accepted_mutants, 0);
}

/// A huge RSA key: refused before it is used, wherever it appears.
#[tokio::test]
async fn g2_huge_rsa_key() {
    use x509_cert::der::asn1::Uint;
    let root = MockRootCa::generate();
    let (_, slot, real) = setup(&root).await;
    let roots = TrustedRoots::new(
        std::slice::from_ref(root.certificate()),
        MOCK_DEVICE_SUBJECT,
    )
    .unwrap();
    for bits in [8192usize, 65536] {
        let mut n = vec![0xffu8; bits / 8];
        n[bits / 8 - 1] = 0xfb;
        let e = vec![0x01u8, 0x00, 0x01];
        let pkcs1 = der(&vec![Uint::new(&n).unwrap(), Uint::new(&e).unwrap()]);
        let spki = SubjectPublicKeyInfoOwned {
            algorithm: AlgorithmIdentifierOwned {
                oid: piv::OID_RSA,
                parameters: Some(Any::from(Null)),
            },
            subject_public_key: BitString::from_bytes(&pkcs1).unwrap(),
        };
        let fake_key = IssuerKey::EcdsaP256(random_p256());
        let big = Name::from_str("CN=big").unwrap();
        let issuer_cert = issue(1, &big, &big, spki.clone(), vec![], &fake_key);
        let rsa_alg = AlgorithmIdentifierOwned {
            oid: piv::OID_RSA_SHA256,
            parameters: Some(Any::from(Null)),
        };
        let tbs = tbs_certificate(
            1,
            &rsa_alg,
            &big,
            &Name::from_str("CN=leaf").unwrap(),
            piv::spki(&slot.public_key()),
            vec![],
        );
        let leaf = assemble(tbs, &rsa_alg, &vec![0x01u8; bits / 8]);
        // As `f9`: not the device certificate, so its key is never used.
        let r = piv::verify_attestation(&leaf, &[issuer_cert], &roots);
        assert!(r.is_err());
        // Used directly, the cap refuses it.
        let cert = Certificate::from_der(leaf.as_der()).unwrap();
        let r = piv::check_signature(0, &cert, &spki);
        if bits == 8192 {
            assert!(
                matches!(r, Err(AttestationError::KeyTooLarge { index: 0 })),
                "{r:?}"
            );
        }
        // Past what the rsa crate parses at all, it is unsupported.
        assert!(
            matches!(
                r,
                Err(AttestationError::KeyTooLarge { index: 0 }
                    | AttestationError::UnsupportedAlgorithm { index: 0, .. })
            ),
            "{bits}: {r:?}"
        );
    }
    let _ = real;
}

#[tokio::test]
async fn g3_huge_inputs() {
    let root = MockRootCa::generate();
    let (_, slot, real) = setup(&root).await;
    let roots = TrustedRoots::new(
        std::slice::from_ref(root.certificate()),
        MOCK_DEVICE_SUBJECT,
    )
    .unwrap();
    // 10,000-cert chain: refused before parsing?
    let big: Vec<DerCertificate> =
        (0..10_000).map(|_| real.device_chain[0].clone()).collect();
    let t = Instant::now();
    let r = piv::verify_attestation(&real.slot_certificate, &big, &roots);
    assert!(matches!(
        r,
        Err(AttestationError::ChainLength { len: 10_000 })
    ));
    assert!(t.elapsed().as_secs() < 1);
    // A 16 MiB "certificate" of nested SEQUENCEs (indefinite depth).
    let mut nested = Vec::new();
    for _ in 0..100_000 {
        nested.extend([0x30, 0x80]);
    }
    let t = Instant::now();
    let r = piv::verify_attestation(
        &DerCertificate::from_der(nested),
        &real.device_chain,
        &roots,
    );
    assert!(r.is_err());
    assert!(t.elapsed().as_secs() < 5);
    // Huge length prefix.
    let r = piv::verify_attestation(
        &DerCertificate::from_der(vec![0x30, 0x84, 0xff, 0xff, 0xff, 0xff]),
        &real.device_chain,
        &roots,
    );
    assert!(r.is_err());
    let r = piv::verify_attestation(
        &DerCertificate::from_der(vec![
            0x30, 0x88, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]),
        &real.device_chain,
        &roots,
    );
    assert!(r.is_err());
    // A 4 MiB cert (huge extension).
    let st_ext = extension(
        x509_cert::der::asn1::ObjectIdentifier::new_unwrap("1.2.3"),
        vec![0u8; 4 << 20],
    );
    let fake = IssuerKey::EcdsaP256(random_p256());
    let huge = issue(
        1,
        &Name::from_str("CN=a").unwrap(),
        &Name::from_str("CN=b").unwrap(),
        piv::spki(&slot.public_key()),
        vec![st_ext],
        &fake,
    );
    let t = Instant::now();
    let r = piv::verify_attestation(&huge, &real.device_chain, &roots);
    assert!(r.is_err());
    assert!(t.elapsed().as_secs() < 5);
}

/// Yubico's real hierarchy: Root 1 -> Intermediate B 1 -> {PIV, FIDO, OPGP,
/// HSM, SD} Attestation B 1 -> device cert -> ... A PIV slot cert needs 3
/// device certs under Root 1, over the cap of 2, so an operator anchors at
/// Intermediate B 1. Then a sibling applet's f9-depth, non-CA key (a FIDO
/// batch attestation key, shared by many devices) fits the cap and nothing
/// says "this is not PIV".
#[tokio::test]
async fn i1_sibling_applet_under_shared_anchor() {
    let bc = |plen: Option<u8>| {
        critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: plen,
            },
            OID_BASIC_CONSTRAINTS,
        )
    };
    let root =
        custom_root("CN=Yubico Attestation Root 1 (mock)", vec![bc(Some(3))]);
    let int_key = IssuerKey::EcdsaP256(random_p256());
    let int_name =
        Name::from_str("CN=Yubico Attestation Intermediate B 1 (mock)")
            .unwrap();
    let int_cert = issue(
        2,
        &root.name,
        &int_name,
        int_key.spki(),
        vec![bc(Some(2))],
        &root.key,
    );
    let fido_key = IssuerKey::EcdsaP256(random_p256());
    let fido_name =
        Name::from_str("CN=Yubico FIDO Attestation B 1 (mock)").unwrap();
    let fido_ca = issue(
        3,
        &int_name,
        &fido_name,
        fido_key.spki(),
        vec![bc(Some(0))],
        &int_key,
    );
    // A FIDO batch attestation cert: non-CA, no Yubico PIV extensions.
    let batch_key = random_p256();
    let batch_name =
        Name::from_str("CN=Yubico U2F EE Serial 123 (mock)").unwrap();
    let batch = issue(
        4,
        &fido_name,
        &batch_name,
        p256_spki(&batch_key),
        vec![extension(
            x509_cert::der::asn1::ObjectIdentifier::new_unwrap(
                "1.3.6.1.4.1.45724.1.1.4",
            ),
            vec![4, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        )],
        &fido_key,
    );
    let claimed = ed_claim();
    let forged = issue(
        5,
        &batch_name,
        &Name::from_str("CN=forged").unwrap(),
        piv::spki(&claimed),
        slot_exts(77_777_777u32.to_der().unwrap(), vec![3, 2]),
        &IssuerKey::EcdsaP256(batch_key),
    );

    let anchored_at_int = MockAttestationVerifier::from_certificates(
        std::slice::from_ref(&int_cert),
    )
    .unwrap();
    let r = anchored_at_int.verify(
        &MockPivAttestation {
            slot_certificate: forged.clone(),
            device_chain: vec![batch.clone(), fido_ca.clone()],
        },
        &claimed,
    );
    assert!(
        matches!(r, Err(AttestationError::ChainLength { len: 2 })),
        "{r:?}"
    );
    // Even with the FIDO CA as the anchor, the batch certificate is not a
    // PIV device certificate.
    let anchored_at_fido = MockAttestationVerifier::from_certificates(
        std::slice::from_ref(&fido_ca),
    )
    .unwrap();
    let r = anchored_at_fido.verify(
        &MockPivAttestation {
            slot_certificate: forged.clone(),
            device_chain: vec![batch.clone()],
        },
        &claimed,
    );
    assert!(
        matches!(r, Err(AttestationError::NotADeviceCertificate { index: 1 })),
        "{r:?}"
    );
    let anchored_at_root = MockAttestationVerifier::from_certificates(&[root
        .certificate()
        .clone()])
    .unwrap();
    let r = anchored_at_root.verify(
        &MockPivAttestation {
            slot_certificate: forged,
            device_chain: vec![batch, fido_ca, int_cert],
        },
        &claimed,
    );
    assert!(r.is_err(), "{r:?}");
}

/// Yubico's published bundle (yubico-ca-certs.txt) includes the U2F and
/// FIDO roots (pathlen 0), which sign batch attestation EE certs directly.
#[tokio::test]
async fn i2_u2f_root_batch_key_issues_leaf() {
    let bc = critical_extension(
        &BasicConstraints {
            ca: true,
            path_len_constraint: Some(0),
        },
        OID_BASIC_CONSTRAINTS,
    );
    let u2f_root =
        custom_root("CN=Yubico U2F Root CA Serial 457200631 (mock)", vec![bc]);
    let piv_root = custom_root(
        "CN=Yubico PIV Root CA Serial 263751 (mock)",
        vec![critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: Some(1),
            },
            OID_BASIC_CONSTRAINTS,
        )],
    );
    let batch_key = random_p256();
    let batch_name =
        Name::from_str("CN=Yubico U2F EE Serial 776137165 (mock)").unwrap();
    let batch = issue(
        4,
        &u2f_root.name,
        &batch_name,
        p256_spki(&batch_key),
        vec![],
        &u2f_root.key,
    );
    let claimed = ed_claim();
    let forged = issue(
        5,
        &batch_name,
        &Name::from_str("CN=forged").unwrap(),
        piv::spki(&claimed),
        slot_exts(1u32.to_der().unwrap(), vec![3, 2]),
        &IssuerKey::EcdsaP256(batch_key),
    );
    let v = MockAttestationVerifier::from_certificates(&[
        piv_root.certificate().clone(),
        u2f_root.certificate().clone(),
    ])
    .unwrap();
    let r = v.verify(
        &MockPivAttestation {
            slot_certificate: forged,
            device_chain: vec![batch],
        },
        &claimed,
    );
    assert!(
        matches!(r, Err(AttestationError::NotADeviceCertificate { index: 1 })),
        "{r:?}"
    );
}
