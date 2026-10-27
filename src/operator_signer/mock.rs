//! A software model of a PIV hardware key, for tests and dev builds
//! (feature `mock-hardware-key`). **Not a security device**: its keys are in
//! this process's memory.
//!
//! It models what the operator API's hardware tiers depend on:
//! - **slots** ([`PivSlot`]), each with a key generated on the device and a
//!   [`KeyPolicy`] fixed at generation; no method returns a private key;
//! - **touch**: [`MockPivDevice::press`] is one touch, taken by the next
//!   signature that needs one. Withheld, a touch-always key fails with
//!   [`SignerError::TouchRequired`] at once (a real key waits ~15 s first).
//!   Cached touch lasts [`CACHED_TOUCH`];
//! - **PIN**: once per session or before every signature, three tries, then
//!   locked;
//! - **removal**: [`MockPivDevice::remove`] makes every call fail with
//!   [`SignerError::DeviceAbsent`]; re-inserting ends the PIN session, as
//!   power-cycling a real key does;
//! - **firmware**: Ed25519 only from [`YUBIKEY_ED25519_MIN_FIRMWARE`];
//! - **attestation**: real X.509, Yubico-shaped (an `f9`-style device
//!   certificate, a CA with `pathlen:0`, issued by a [`MockRootCa`], and a
//!   slot certificate with
//!   Yubico's serial, firmware and policy extensions), read by the same
//!   [`piv`] path as a real one. Every name in it says MOCK, it
//!   travels as its own type ([`MockPivAttestation`]), and only
//!   [`MockAttestationVerifier`] takes it.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ed25519_dalek::Signer as _;
use p256::ecdsa::signature::Signer as _;
use x509_cert::certificate::{Certificate, TbsCertificate};
use x509_cert::der::asn1::{
    BitString, GeneralizedTime, Null, OctetString, UtcTime,
};
use x509_cert::der::{Any, Decode, Encode};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
use x509_cert::time::{Time, Validity};
use zeroize::Zeroizing;

use super::piv::{self, MOCK_DEVICE_SUBJECT, MOCK_ROOT_SUBJECT, TrustedRoots};
use super::{
    AttestationBundle, AttestationError, AttestationVerifier, AttestedKey,
    DerCertificate, DeviceSerial, EcdsaP256PublicKey, EcdsaP256Signature,
    FirmwareVersion, HardwareKey, KeyPolicy, MockPivAttestation,
    OperatorPublicKey, OperatorSignature, OperatorSigner, Pin, PinPolicy,
    PivSerial, PivSlot, SignatureAlgorithm, SignerError, SignerKind,
    TouchPolicy, YUBIKEY_ED25519_MIN_FIRMWARE,
};

/// How long a cached touch covers further signatures (YubiKey: 15 s).
pub const CACHED_TOUCH: Duration = Duration::from_secs(15);

/// PIN tries before the PIN locks (PIV default).
pub const PIN_TRIES: u8 = 3;

// ---------------------------------------------------------------------------
// Root CA
// ---------------------------------------------------------------------------

/// A mock PIV attestation CA, standing in for Yubico's: a root (P-256, or
/// RSA as Yubico's legacy PIV root is), or a CA under one
/// ([`intermediate_p384`](Self::intermediate_p384)), as Yubico's newer PIV
/// attestation CAs sit under Root 1. Whichever issues a device's `f9` is
/// the anchor a verifier trusts; a device made under an intermediate also
/// hands over a copy of it, which the verifier ignores.
pub struct MockRootCa {
    key: IssuerKey,
    name: Name,
    certificate: DerCertificate,
    /// The certificates between this CA and the root, this one first;
    /// empty for a root.
    chain_to_root: Vec<DerCertificate>,
}

/// A key that signs certificates.
enum IssuerKey {
    EcdsaP256(p256::ecdsa::SigningKey),
    EcdsaP384(p384::ecdsa::SigningKey),
    Rsa(Box<rsa::pkcs1v15::SigningKey<rsa::sha2::Sha256>>),
}

impl IssuerKey {
    fn algorithm(&self) -> AlgorithmIdentifierOwned {
        match self {
            Self::EcdsaP256(_) => AlgorithmIdentifierOwned {
                oid: piv::OID_ECDSA_SHA256,
                parameters: None,
            },
            Self::EcdsaP384(_) => AlgorithmIdentifierOwned {
                oid: piv::OID_ECDSA_SHA384,
                parameters: None,
            },
            Self::Rsa(_) => AlgorithmIdentifierOwned {
                oid: piv::OID_RSA_SHA256,
                parameters: Some(Any::from(Null)),
            },
        }
    }

    fn sign(&self, tbs: &[u8]) -> Vec<u8> {
        match self {
            Self::EcdsaP256(key) => {
                let signature: p256::ecdsa::Signature = key.sign(tbs);
                piv::ecdsa_to_der(&signature.to_bytes())
            }
            Self::EcdsaP384(key) => {
                use p384::ecdsa::signature::Signer as _;
                let signature: p384::ecdsa::Signature = key.sign(tbs);
                piv::ecdsa_to_der(&signature.to_bytes())
            }
            Self::Rsa(key) => {
                use rsa::signature::SignatureEncoding;
                let signature: rsa::pkcs1v15::Signature = key.sign(tbs);
                signature.to_vec()
            }
        }
    }

    fn spki(&self) -> SubjectPublicKeyInfoOwned {
        match self {
            Self::EcdsaP256(key) => p256_spki(key),
            Self::EcdsaP384(key) => SubjectPublicKeyInfoOwned {
                algorithm: AlgorithmIdentifierOwned {
                    oid: piv::OID_EC_PUBLIC_KEY,
                    parameters: Some(
                        Any::encode_from(&OID_P384).expect("an OID encodes"),
                    ),
                },
                subject_public_key: BitString::from_bytes(
                    key.verifying_key().to_sec1_point(false).as_bytes(),
                )
                .expect("a whole number of bytes"),
            },
            Self::Rsa(key) => {
                use rsa::pkcs1::EncodeRsaPublicKey;
                use rsa::signature::Keypair;
                let public = key
                    .verifying_key()
                    .as_ref()
                    .to_pkcs1_der()
                    .expect("an RSA key encodes");
                SubjectPublicKeyInfoOwned {
                    algorithm: AlgorithmIdentifierOwned {
                        oid: piv::OID_RSA,
                        parameters: Some(Any::from(Null)),
                    },
                    subject_public_key: BitString::from_bytes(
                        public.as_bytes(),
                    )
                    .expect("a whole number of bytes"),
                }
            }
        }
    }
}

/// A fresh P-384 key, drawn as `random_p256` draws.
fn random_p384() -> p384::ecdsa::SigningKey {
    use rand::RngCore;
    loop {
        let mut bytes = Zeroizing::new([0u8; 48]);
        rand::rngs::OsRng.fill_bytes(bytes.as_mut());
        if let Ok(key) = p384::ecdsa::SigningKey::from_slice(bytes.as_ref()) {
            return key;
        }
    }
}

/// `secp384r1`.
const OID_P384: x509_cert::der::asn1::ObjectIdentifier =
    x509_cert::der::asn1::ObjectIdentifier::new_unwrap("1.3.132.0.34");

/// A fresh P-256 key. (p256 0.14 takes a `rand_core` 0.10 RNG; this crate's
/// is `rand` 0.8, so draw bytes and retry the rare out-of-range scalar.)
fn random_p256() -> p256::ecdsa::SigningKey {
    use rand::RngCore;
    loop {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(bytes.as_mut());
        if let Ok(key) = p256::ecdsa::SigningKey::from_slice(bytes.as_ref()) {
            return key;
        }
    }
}

impl MockRootCa {
    /// A new P-256 root with a fresh key and a self-signed certificate.
    pub fn generate() -> Self {
        Self::with_key(IssuerKey::EcdsaP256(random_p256()))
    }

    /// An RSA root (PKCS#1 v1.5, SHA-256), as Yubico's older root is, from
    /// a PKCS#1 DER private key. (This build has no RSA key generation:
    /// tests bring a fixture key.)
    pub fn rsa_from_pkcs1_der(der: &[u8]) -> Result<Self, AttestationError> {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        let key = rsa::RsaPrivateKey::from_pkcs1_der(der).map_err(|e| {
            AttestationError::Malformed {
                index: 0,
                message: e.to_string(),
            }
        })?;
        Ok(Self::with_key(IssuerKey::Rsa(Box::new(
            rsa::pkcs1v15::SigningKey::new(key),
        ))))
    }

    fn with_key(key: IssuerKey) -> Self {
        let name = Name::from_str(MOCK_ROOT_SUBJECT).expect("a valid name");
        let certificate =
            issue(1, &name, &name, key.spki(), ca_extensions(), &key);
        Self {
            key,
            name,
            certificate,
            chain_to_root: Vec::new(),
        }
    }

    /// A P-384 intermediate CA issued by this one, as in Yubico's newer
    /// hierarchy (root → intermediate → `f9` → slot).
    pub fn intermediate_p384(&self) -> Self {
        let key = IssuerKey::EcdsaP384(random_p384());
        let name =
            Name::from_str(MOCK_INTERMEDIATE_SUBJECT).expect("a valid name");
        let certificate =
            issue(2, &self.name, &name, key.spki(), ca_extensions(), &self.key);
        let mut chain_to_root = vec![certificate.clone()];
        chain_to_root.extend(self.chain_to_root.iter().cloned());
        Self {
            key,
            name,
            certificate,
            chain_to_root,
        }
    }

    /// This CA's certificate: for a root, the one a verifier trusts.
    pub fn certificate(&self) -> &DerCertificate {
        &self.certificate
    }
}

/// A mock intermediate CA's subject.
const MOCK_INTERMEDIATE_SUBJECT: &str =
    "CN=Agora MOCK PIV Intermediate CA - NOT A REAL DEVICE";

/// `basicConstraints cA=TRUE` and `keyUsage keyCertSign`, as a CA needs.
fn ca_extensions() -> Vec<Extension> {
    vec![
        critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: None,
            },
            OID_BASIC_CONSTRAINTS,
        ),
        critical_extension(
            &KeyUsage(KeyUsages::KeyCertSign | KeyUsages::CRLSign),
            OID_KEY_USAGE,
        ),
    ]
}

/// A device certificate's: a CA that issues only end certificates, as a
/// YubiKey's `f9` is (`CA:TRUE, pathlen:0`).
fn f9_extensions() -> Vec<Extension> {
    vec![
        critical_extension(
            &BasicConstraints {
                ca: true,
                path_len_constraint: Some(0),
            },
            OID_BASIC_CONSTRAINTS,
        ),
        critical_extension(
            &KeyUsage(KeyUsages::KeyCertSign.into()),
            OID_KEY_USAGE,
        ),
    ]
}

impl fmt::Debug for MockRootCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MockRootCa([REDACTED])")
    }
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

enum SlotPrivateKey {
    Ed25519(ed25519_dalek::SigningKey),
    EcdsaP256(p256::ecdsa::SigningKey),
}

struct SlotKey {
    key: SlotPrivateKey,
    public_key: OperatorPublicKey,
    policy: KeyPolicy,
    generation: u64,
}

struct DeviceState {
    present: bool,
    pin: Pin,
    pin_tries_left: u8,
    /// The PIN was verified this session ([`PinPolicy::Once`]).
    pin_session: bool,
    /// The PIN was verified and no PIN-always signature has been made
    /// since ([`PinPolicy::Always`]).
    pin_fresh: bool,
    touch_pending: bool,
    last_touch: Option<Instant>,
    attestation_key: IssuerKey,
    attestation_certificate: DerCertificate,
    /// The CA certificates above `f9` the device holds (an intermediate).
    issuer_chain: Vec<DerCertificate>,
    slots: HashMap<PivSlot, SlotKey>,
    generations: u64,
}

/// A mock PIV device. Cloning gives another handle to the same device, so a
/// test can [`press`](Self::press) it while a signer waits.
#[derive(Clone)]
pub struct MockPivDevice {
    state: Arc<Mutex<DeviceState>>,
    serial: PivSerial,
    firmware: FirmwareVersion,
}

impl fmt::Debug for MockPivDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockPivDevice")
            .field("serial", &self.serial)
            .field("firmware", &self.firmware)
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

impl MockPivDevice {
    /// A firmware with Ed25519.
    pub const FIRMWARE_ED25519: FirmwareVersion = FirmwareVersion::new(5, 7, 2);
    /// A firmware without: P-256 only.
    pub const FIRMWARE_P256_ONLY: FirmwareVersion =
        FirmwareVersion::new(5, 4, 3);

    /// "Manufacture" a device: give it an attestation key whose certificate
    /// `root` issues, a serial, a firmware and a PIN. It starts inserted.
    pub fn manufacture(
        root: &MockRootCa,
        serial: PivSerial,
        firmware: FirmwareVersion,
        pin: Pin,
    ) -> Self {
        let attestation_key = IssuerKey::EcdsaP256(random_p256());
        let attestation_certificate = issue(
            u64::from(serial.get()),
            &root.name,
            &device_name(),
            attestation_key.spki(),
            f9_extensions(),
            &root.key,
        );
        Self {
            state: Arc::new(Mutex::new(DeviceState {
                present: true,
                pin,
                pin_tries_left: PIN_TRIES,
                pin_session: false,
                pin_fresh: false,
                touch_pending: false,
                last_touch: None,
                attestation_key,
                attestation_certificate,
                issuer_chain: root.chain_to_root.clone(),
                slots: HashMap::new(),
                generations: 0,
            })),
            serial,
            firmware,
        }
    }

    fn lock(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn present(&self) -> Result<MutexGuard<'_, DeviceState>, SignerError> {
        let state = self.lock();
        if state.present {
            Ok(state)
        } else {
            Err(SignerError::DeviceAbsent)
        }
    }

    /// Generate a key in `slot`, replacing any key there (whose signers
    /// then fail with [`SignerError::KeyChanged`]), and open a signer on it.
    pub fn generate_key(
        &self,
        slot: PivSlot,
        algorithm: SignatureAlgorithm,
        policy: KeyPolicy,
    ) -> Result<MockHardwareKey, SignerError> {
        let mut state = self.present()?;
        let (key, public_key) = match algorithm {
            SignatureAlgorithm::Ed25519 => {
                if self.firmware < YUBIKEY_ED25519_MIN_FIRMWARE {
                    return Err(SignerError::UnsupportedAlgorithm {
                        algorithm,
                        reason: "Ed25519 needs firmware 5.7 or later",
                    });
                }
                let key =
                    ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
                let public_key =
                    OperatorPublicKey::Ed25519(key.verifying_key());
                (SlotPrivateKey::Ed25519(key), public_key)
            }
            SignatureAlgorithm::EcdsaP256 => {
                let key = random_p256();
                let public_key = OperatorPublicKey::EcdsaP256(
                    EcdsaP256PublicKey::from_p256(key.verifying_key()),
                );
                (SlotPrivateKey::EcdsaP256(key), public_key)
            }
        };
        state.generations += 1;
        let generation = state.generations;
        state.slots.insert(
            slot,
            SlotKey {
                key,
                public_key,
                policy,
                generation,
            },
        );
        Ok(MockHardwareKey {
            device: self.clone(),
            slot,
            generation,
            public_key,
            policy,
        })
    }

    /// Touch the device once. The touch is taken by the next signature that
    /// needs one; it does nothing while the device is removed.
    pub fn press(&self) {
        let mut state = self.lock();
        if state.present {
            state.touch_pending = true;
        }
    }

    /// Unplug the device.
    pub fn remove(&self) {
        let mut state = self.lock();
        state.present = false;
        state.pin_session = false;
        state.pin_fresh = false;
        state.touch_pending = false;
        state.last_touch = None;
    }

    /// Plug it back in. The PIN session starts over.
    pub fn insert(&self) {
        self.lock().present = true;
    }

    /// Overwrite the attestation key and certificate with a self-signed
    /// pair, as a user can on a real YubiKey. The device's attestations then
    /// chain to no trusted root.
    pub fn overwrite_attestation_key(&self) {
        let mut state = self.lock();
        let key = IssuerKey::EcdsaP256(random_p256());
        state.attestation_certificate = issue(
            u64::from(self.serial.get()),
            &device_name(),
            &device_name(),
            key.spki(),
            f9_extensions(),
            &key,
        );
        state.attestation_key = key;
    }
}

/// A key in a [`MockPivDevice`] slot: an [`OperatorSigner`] and
/// [`HardwareKey`].
pub struct MockHardwareKey {
    device: MockPivDevice,
    slot: PivSlot,
    generation: u64,
    public_key: OperatorPublicKey,
    policy: KeyPolicy,
}

impl fmt::Debug for MockHardwareKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockHardwareKey")
            .field("serial", &self.device.serial)
            .field("slot", &self.slot)
            .field("public_key", &self.public_key)
            .field("policy", &self.policy)
            .finish()
    }
}

impl MockHardwareKey {
    /// The device this key is on.
    pub fn device(&self) -> &MockPivDevice {
        &self.device
    }

    /// Its slot.
    pub fn slot(&self) -> PivSlot {
        self.slot
    }

    fn slot_key<'a>(
        &self,
        state: &'a DeviceState,
    ) -> Result<&'a SlotKey, SignerError> {
        state
            .slots
            .get(&self.slot)
            .filter(|key| key.generation == self.generation)
            .ok_or(SignerError::KeyChanged)
    }
}

#[async_trait::async_trait]
impl OperatorSigner for MockHardwareKey {
    fn public_key(&self) -> OperatorPublicKey {
        self.public_key
    }

    fn kind(&self) -> SignerKind<'_> {
        SignerKind::Hardware(self)
    }

    async fn sign(
        &self,
        message: &[u8],
    ) -> Result<OperatorSignature, SignerError> {
        let mut state = self.device.present()?;
        self.slot_key(&state)?;

        // PIN, then touch, as a PIV card checks them.
        let pin_ok = match self.policy.pin {
            PinPolicy::Never => true,
            PinPolicy::Once => state.pin_session,
            PinPolicy::Always => state.pin_fresh,
        };
        if !pin_ok {
            return Err(if state.pin_tries_left == 0 {
                SignerError::PinBlocked
            } else {
                SignerError::PinRequired
            });
        }
        match self.policy.touch {
            TouchPolicy::Never => {}
            TouchPolicy::Always => {
                if !std::mem::take(&mut state.touch_pending) {
                    return Err(SignerError::TouchRequired);
                }
            }
            TouchPolicy::Cached => {
                let now = Instant::now();
                if std::mem::take(&mut state.touch_pending) {
                    state.last_touch = Some(now);
                } else if !state
                    .last_touch
                    .is_some_and(|at| now.duration_since(at) < CACHED_TOUCH)
                {
                    return Err(SignerError::TouchRequired);
                }
            }
        }

        // A PIN-always verification covers one signature that succeeds,
        // and only on a PIN-always key.
        if self.policy.pin == PinPolicy::Always {
            state.pin_fresh = false;
        }
        Ok(match &self.slot_key(&state)?.key {
            SlotPrivateKey::Ed25519(key) => {
                OperatorSignature::Ed25519(key.sign(message))
            }
            SlotPrivateKey::EcdsaP256(key) => {
                let signature: p256::ecdsa::Signature = key.sign(message);
                OperatorSignature::EcdsaP256(EcdsaP256Signature::from_p256(
                    &signature,
                ))
            }
        })
    }
}

#[async_trait::async_trait]
impl HardwareKey for MockHardwareKey {
    fn policy(&self) -> KeyPolicy {
        self.policy
    }

    fn serial(&self) -> DeviceSerial {
        DeviceSerial::Mock(self.device.serial)
    }

    fn firmware(&self) -> FirmwareVersion {
        self.device.firmware
    }

    async fn verify_pin(&self, pin: &Pin) -> Result<(), SignerError> {
        let mut state = self.device.present()?;
        if state.pin_tries_left == 0 {
            return Err(SignerError::PinBlocked);
        }
        if !constant_time_eq(
            pin.expose().as_bytes(),
            state.pin.expose().as_bytes(),
        ) {
            state.pin_tries_left -= 1;
            state.pin_session = false;
            state.pin_fresh = false;
            return Err(match state.pin_tries_left {
                0 => SignerError::PinBlocked,
                retries_left => SignerError::WrongPin { retries_left },
            });
        }
        state.pin_tries_left = PIN_TRIES;
        state.pin_session = true;
        state.pin_fresh = true;
        Ok(())
    }

    async fn attestation(&self) -> Result<AttestationBundle, SignerError> {
        let state = self.device.present()?;
        let slot_key = self.slot_key(&state)?;
        let subject = Name::from_str(&format!(
            "CN=Agora MOCK PIV Attestation {} - NOT A REAL DEVICE",
            self.slot
        ))
        .expect("a valid name");
        let extensions = vec![
            extension(
                piv::OID_YUBICO_FIRMWARE,
                vec![
                    self.device.firmware.major,
                    self.device.firmware.minor,
                    self.device.firmware.patch,
                ],
            ),
            extension(
                piv::OID_YUBICO_SERIAL,
                self.device
                    .serial
                    .get()
                    .to_der()
                    .expect("an integer encodes"),
            ),
            extension(
                piv::OID_YUBICO_POLICY,
                piv::policy_bytes(slot_key.policy).to_vec(),
            ),
        ];
        let slot_certificate = issue(
            u64::from(self.slot.id()),
            &device_name(),
            &subject,
            piv::spki(&slot_key.public_key),
            extensions,
            &state.attestation_key,
        );
        Ok(AttestationBundle::MockPiv(MockPivAttestation {
            slot_certificate,
            device_chain: std::iter::once(&state.attestation_certificate)
                .chain(&state.issuer_chain)
                .cloned()
                .collect(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

/// Checks [`MockPivAttestation`]s against mock CAs, through the same chain
/// checks [`piv::YubicoAttestationVerifier`] uses, with the mock device
/// subject in place of Yubico's.
#[derive(Debug, Clone)]
pub struct MockAttestationVerifier {
    roots: TrustedRoots,
}

impl MockAttestationVerifier {
    /// Trust `root`.
    pub fn new(root: &MockRootCa) -> Self {
        Self::from_certificates(std::slice::from_ref(root.certificate()))
            .expect("a mock root's certificate parses")
    }

    /// Trust these root certificates.
    pub fn from_certificates(
        roots: &[DerCertificate],
    ) -> Result<Self, AttestationError> {
        Ok(Self {
            roots: TrustedRoots::new(roots, MOCK_DEVICE_SUBJECT)?,
        })
    }
}

impl AttestationVerifier for MockAttestationVerifier {
    type Evidence = MockPivAttestation;

    fn evidence(
        bundle: &AttestationBundle,
    ) -> Result<&MockPivAttestation, AttestationError> {
        match bundle {
            AttestationBundle::MockPiv(evidence) => Ok(evidence),
            AttestationBundle::YubicoPiv(_) => {
                Err(AttestationError::WrongFormat {
                    format: bundle.format(),
                })
            }
        }
    }

    fn verify(
        &self,
        evidence: &MockPivAttestation,
        claimed: &OperatorPublicKey,
    ) -> Result<AttestedKey, AttestationError> {
        let facts = piv::verify_attestation(
            &evidence.slot_certificate,
            &evidence.device_chain,
            &self.roots,
        )?;
        piv::attested(facts, claimed, DeviceSerial::Mock)
    }
}

/// Compare in constant time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

// ---------------------------------------------------------------------------
// Certificates
// ---------------------------------------------------------------------------

fn device_name() -> Name {
    Name::from_str(MOCK_DEVICE_SUBJECT).expect("a valid name")
}

fn p256_spki(key: &p256::ecdsa::SigningKey) -> SubjectPublicKeyInfoOwned {
    piv::spki(&OperatorPublicKey::EcdsaP256(
        EcdsaP256PublicKey::from_p256(key.verifying_key()),
    ))
}

fn extension(
    oid: x509_cert::der::asn1::ObjectIdentifier,
    value: Vec<u8>,
) -> Extension {
    Extension {
        extn_id: oid,
        critical: false,
        extn_value: OctetString::new(value).expect("a short value"),
    }
}

/// `basicConstraints`.
const OID_BASIC_CONSTRAINTS: x509_cert::der::asn1::ObjectIdentifier =
    x509_cert::der::asn1::ObjectIdentifier::new_unwrap("2.5.29.19");
/// `keyUsage`.
const OID_KEY_USAGE: x509_cert::der::asn1::ObjectIdentifier =
    x509_cert::der::asn1::ObjectIdentifier::new_unwrap("2.5.29.15");

fn critical_extension(
    value: &impl Encode,
    oid: x509_cert::der::asn1::ObjectIdentifier,
) -> Extension {
    Extension {
        critical: true,
        ..extension(oid, der(value))
    }
}

/// Issue a certificate signed with `issuer_key`, valid from 2026-01-01 to
/// the end of 9999 (as Yubico's slot certificates are).
fn issue(
    serial: u64,
    issuer: &Name,
    subject: &Name,
    subject_public_key_info: SubjectPublicKeyInfoOwned,
    extensions: Vec<Extension>,
    issuer_key: &IssuerKey,
) -> DerCertificate {
    let algorithm = issuer_key.algorithm();
    let tbs = tbs_certificate(
        serial,
        &algorithm,
        issuer,
        subject,
        subject_public_key_info,
        extensions,
    );
    let signature = issuer_key.sign(&tbs);
    assemble(tbs, &algorithm, &signature)
}

/// A TBS certificate's DER, valid from 2026-01-01 to the end of 9999.
fn tbs_certificate(
    serial: u64,
    algorithm: &AlgorithmIdentifierOwned,
    issuer: &Name,
    subject: &Name,
    subject_public_key_info: SubjectPublicKeyInfoOwned,
    extensions: Vec<Extension>,
) -> Vec<u8> {
    let validity = Validity::<x509_cert::certificate::Rfc5280>::new(
        Time::UtcTime(
            UtcTime::from_unix_duration(Duration::from_secs(1_767_225_600))
                .expect("in UTCTime's range"),
        ),
        Time::GeneralTime(GeneralizedTime::from_date_time(
            x509_cert::der::DateTime::new(9999, 12, 31, 23, 59, 59)
                .expect("a valid date"),
        )),
    );

    // x509-cert 0.3 parses certificates but builds them only through its
    // `builder` feature, so the TBS is assembled here from encoded parts
    // and parsed back to check it.
    let mut tbs = tlv(0xa0, &der(&2u8)); // [0] EXPLICIT version: v3
    tbs.extend(der(&SerialNumber::<x509_cert::certificate::Rfc5280>::new(
        &serial.to_be_bytes(),
    )
    .expect("a valid serial")));
    tbs.extend(der(algorithm));
    tbs.extend(der(issuer));
    tbs.extend(der(&validity));
    tbs.extend(der(subject));
    tbs.extend(der(&subject_public_key_info));
    if !extensions.is_empty() {
        tbs.extend(tlv(0xa3, &der(&extensions))); // [3] EXPLICIT
    }
    let tbs = tlv(0x30, &tbs);
    TbsCertificate::from_der(&tbs).expect("a well-formed TBS certificate");
    tbs
}

/// A certificate from its TBS DER, outer algorithm and signature.
fn assemble(
    tbs: Vec<u8>,
    algorithm: &AlgorithmIdentifierOwned,
    signature: &[u8],
) -> DerCertificate {
    let mut certificate = tbs;
    certificate.extend(der(algorithm));
    certificate.extend(der(
        &BitString::from_bytes(signature).expect("a whole number of bytes")
    ));
    let certificate = tlv(0x30, &certificate);
    Certificate::from_der(&certificate).expect("a well-formed certificate");
    DerCertificate::from_der(certificate)
}

fn der(value: &impl Encode) -> Vec<u8> {
    value.to_der().expect("encodes")
}

/// One DER tag-length-value.
fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = value.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend(&bytes[skip..]);
    }
    out.extend(value);
    out
}

#[cfg(test)]
#[path = "mock_attacks.rs"]
mod attacks;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use crate::operator_signer::{sign_request, verify_request};

    const PIN: &str = "975310";
    const SERIAL: PivSerial = PivSerial::new(12_345_678);

    fn device(firmware: FirmwareVersion) -> (MockRootCa, MockPivDevice) {
        let root = MockRootCa::generate();
        let device =
            MockPivDevice::manufacture(&root, SERIAL, firmware, Pin::new(PIN));
        (root, device)
    }

    fn mock_attestation(bundle: AttestationBundle) -> MockPivAttestation {
        match bundle {
            AttestationBundle::MockPiv(attestation) => attestation,
            AttestationBundle::YubicoPiv(_) => {
                panic!("a mock device attests as a mock")
            }
        }
    }

    #[tokio::test]
    async fn unattended_slot_signs_after_the_pin_with_no_touch() {
        for (firmware, algorithm) in [
            (MockPivDevice::FIRMWARE_ED25519, SignatureAlgorithm::Ed25519),
            (
                MockPivDevice::FIRMWARE_ED25519,
                SignatureAlgorithm::EcdsaP256,
            ),
            (
                MockPivDevice::FIRMWARE_P256_ONLY,
                SignatureAlgorithm::EcdsaP256,
            ),
        ] {
            let (_, device) = device(firmware);
            let key = device
                .generate_key(
                    PivSlot::Authentication,
                    algorithm,
                    KeyPolicy::UNATTENDED,
                )
                .unwrap();
            assert_eq!(key.algorithm(), algorithm);
            assert!(matches!(
                sign_request(&key, b"payload", 7).await,
                Err(SignerError::PinRequired)
            ));
            key.verify_pin(&Pin::new(PIN)).await.unwrap();
            // PIN once: it holds for every later signature.
            for _ in 0..3 {
                let signature =
                    sign_request(&key, b"payload", 7).await.unwrap();
                assert_eq!(signature.algorithm(), algorithm);
                verify_request(&key.public_key(), b"payload", 7, &signature)
                    .unwrap();
                assert!(
                    verify_request(&key.public_key(), b"other", 7, &signature)
                        .is_err()
                );
            }
        }
    }

    /// A hardware key proves possession as a software key does, under
    /// either algorithm, and the proof binds operator and key
    #[tokio::test]
    async fn a_hardware_key_proves_possession() {
        use crate::ids::OperatorId;
        use crate::operator_signer::{sign_possession, verify_possession};
        for (firmware, algorithm) in [
            (MockPivDevice::FIRMWARE_ED25519, SignatureAlgorithm::Ed25519),
            (
                MockPivDevice::FIRMWARE_P256_ONLY,
                SignatureAlgorithm::EcdsaP256,
            ),
        ] {
            let (_, device) = device(firmware);
            let key = device
                .generate_key(
                    PivSlot::Authentication,
                    algorithm,
                    KeyPolicy {
                        pin: PinPolicy::Never,
                        touch: TouchPolicy::Never,
                    },
                )
                .unwrap();
            let operator = OperatorId::new();
            let proof = sign_possession(&key, operator).await.unwrap();
            verify_possession(&key.public_key(), operator, &proof).unwrap();
            assert!(
                verify_possession(&key.public_key(), OperatorId::new(), &proof)
                    .is_err()
            );
            // Not a request signature over the same bytes
            let bytes =
                crate::signing::operator_request::credential_possession_bytes(
                    operator,
                    &key.public_key(),
                );
            assert!(
                verify_request(&key.public_key(), &bytes, 0, &proof).is_err()
            );
        }
    }

    #[tokio::test]
    async fn ed25519_mock_signatures_verify_with_crypto_verify() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy {
                    pin: PinPolicy::Never,
                    touch: TouchPolicy::Never,
                },
            )
            .unwrap();
        let (
            OperatorPublicKey::Ed25519(vk),
            OperatorSignature::Ed25519(signature),
        ) = (
            key.public_key(),
            sign_request(&key, b"body", 99).await.unwrap(),
        )
        else {
            panic!("Ed25519");
        };
        assert!(crypto::verify(&vk, b"body", 99, &signature));
    }

    #[tokio::test]
    async fn old_firmware_refuses_ed25519() {
        let (_, device) = device(MockPivDevice::FIRMWARE_P256_ONLY);
        assert!(matches!(
            device.generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED
            ),
            Err(SignerError::UnsupportedAlgorithm {
                algorithm: SignatureAlgorithm::Ed25519,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn sudo_slot_needs_a_touch_for_every_signature() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let sudo = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO,
            )
            .unwrap();
        sudo.verify_pin(&Pin::new(PIN)).await.unwrap();

        assert!(matches!(
            sudo.sign(b"m").await,
            Err(SignerError::TouchRequired)
        ));
        device.press();
        let signature = sudo.sign(b"m").await.unwrap();
        sudo.public_key().verify(b"m", &signature).unwrap();
        // The touch was used up.
        assert!(matches!(
            sudo.sign(b"m").await,
            Err(SignerError::TouchRequired)
        ));
    }

    #[tokio::test]
    async fn cached_touch_covers_the_next_signatures() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy {
                    pin: PinPolicy::Never,
                    touch: TouchPolicy::Cached,
                },
            )
            .unwrap();
        assert!(matches!(
            key.sign(b"m").await,
            Err(SignerError::TouchRequired)
        ));
        device.press();
        key.sign(b"m").await.unwrap();
        key.sign(b"m").await.unwrap();
    }

    #[tokio::test]
    async fn pin_always_covers_one_signature() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO_WITH_PIN,
            )
            .unwrap();
        device.press();
        assert!(matches!(
            key.sign(b"m").await,
            Err(SignerError::PinRequired)
        ));
        key.verify_pin(&Pin::new(PIN)).await.unwrap();
        key.sign(b"m").await.unwrap();
        device.press();
        assert!(matches!(
            key.sign(b"m").await,
            Err(SignerError::PinRequired)
        ));
    }

    #[tokio::test]
    async fn wrong_pins_count_down_and_lock() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let wrong = Pin::new("000000");
        assert!(matches!(
            key.verify_pin(&wrong).await,
            Err(SignerError::WrongPin { retries_left: 2 })
        ));
        // A right PIN resets the count.
        key.verify_pin(&Pin::new(PIN)).await.unwrap();
        for retries_left in [2, 1] {
            assert!(matches!(
                key.verify_pin(&wrong).await,
                Err(SignerError::WrongPin { retries_left: n }) if n == retries_left
            ));
        }
        assert!(matches!(
            key.verify_pin(&wrong).await,
            Err(SignerError::PinBlocked)
        ));
        assert!(matches!(
            key.verify_pin(&Pin::new(PIN)).await,
            Err(SignerError::PinBlocked)
        ));
        assert!(matches!(key.sign(b"m").await, Err(SignerError::PinBlocked)));
    }

    #[tokio::test]
    async fn a_pin_always_verification_is_spent_only_by_its_own_success() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let unattended = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let sudo = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO_WITH_PIN,
            )
            .unwrap();
        sudo.verify_pin(&Pin::new(PIN)).await.unwrap();
        // A PIN-once key's signature doesn't spend it.
        unattended.sign(b"m").await.unwrap();
        // Nor does a PIN-always signature that fails for want of a touch.
        assert!(matches!(
            sudo.sign(b"m").await,
            Err(SignerError::TouchRequired)
        ));
        device.press();
        sudo.sign(b"m").await.unwrap();
        // A successful one does.
        device.press();
        assert!(matches!(
            sudo.sign(b"m").await,
            Err(SignerError::PinRequired)
        ));
    }

    #[tokio::test]
    async fn removal_makes_the_device_absent_and_ends_the_pin_session() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        key.verify_pin(&Pin::new(PIN)).await.unwrap();
        key.sign(b"m").await.unwrap();

        device.remove();
        assert!(matches!(
            key.sign(b"m").await,
            Err(SignerError::DeviceAbsent)
        ));
        assert!(matches!(
            key.attestation().await,
            Err(SignerError::DeviceAbsent)
        ));
        assert!(matches!(
            key.verify_pin(&Pin::new(PIN)).await,
            Err(SignerError::DeviceAbsent)
        ));

        device.insert();
        assert!(matches!(
            key.sign(b"m").await,
            Err(SignerError::PinRequired)
        ));
        key.verify_pin(&Pin::new(PIN)).await.unwrap();
        key.sign(b"m").await.unwrap();
    }

    #[tokio::test]
    async fn regenerating_a_slot_retires_the_old_signer() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let policy = KeyPolicy {
            pin: PinPolicy::Never,
            touch: TouchPolicy::Never,
        };
        let old = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                policy,
            )
            .unwrap();
        let new = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                policy,
            )
            .unwrap();
        assert!(matches!(old.sign(b"m").await, Err(SignerError::KeyChanged)));
        new.sign(b"m").await.unwrap();
    }

    #[tokio::test]
    async fn attestation_verifies_with_the_mock_root_and_says_what_it_should() {
        for (firmware, algorithm, policy) in [
            (
                MockPivDevice::FIRMWARE_ED25519,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            ),
            (
                MockPivDevice::FIRMWARE_ED25519,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO_WITH_PIN,
            ),
            (
                MockPivDevice::FIRMWARE_P256_ONLY,
                SignatureAlgorithm::EcdsaP256,
                KeyPolicy::SUDO,
            ),
        ] {
            let (root, device) = device(firmware);
            let key = device
                .generate_key(PivSlot::Signature, algorithm, policy)
                .unwrap();
            // Attestation needs neither PIN nor touch.
            let attestation =
                mock_attestation(key.attestation().await.unwrap());

            let attested = MockAttestationVerifier::new(&root)
                .verify(&attestation, &key.public_key())
                .unwrap();
            assert_eq!(*attested.public_key(), key.public_key());
            assert_eq!(attested.serial(), DeviceSerial::Mock(SERIAL));
            assert_eq!(attested.firmware(), firmware);
            assert_eq!(attested.policy(), policy);
        }
    }

    const RSA_ROOT: &[u8] = include_bytes!(
        "../../tests/fixtures/operator_signer/mock_rsa_root_pkcs1.der"
    );

    #[tokio::test]
    async fn attestation_chains_to_an_rsa_root() {
        let root = MockRootCa::rsa_from_pkcs1_der(RSA_ROOT).unwrap();
        let device = MockPivDevice::manufacture(
            &root,
            PivSerial::new(7),
            MockPivDevice::FIRMWARE_ED25519,
            Pin::new(PIN),
        );
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let mut attestation =
            mock_attestation(key.attestation().await.unwrap());
        let verifier = MockAttestationVerifier::new(&root);
        let attested =
            verifier.verify(&attestation, &key.public_key()).unwrap();
        assert_eq!(attested.serial(), DeviceSerial::Mock(PivSerial::new(7)));

        // A P-256 root of the same name doesn't verify the RSA signature.
        assert!(matches!(
            MockAttestationVerifier::new(&MockRootCa::generate())
                .verify(&attestation, &key.public_key()),
            Err(AttestationError::UntrustedRoot)
        ));

        // Flip a byte of the device certificate's RSA signature.
        let mut der = attestation.device_chain[0].as_der().to_vec();
        let last = der.len() - 1;
        der[last] ^= 1;
        attestation.device_chain[0] = DerCertificate::from_der(der);
        assert!(matches!(
            verifier.verify(&attestation, &key.public_key()),
            Err(AttestationError::UntrustedRoot)
        ));
    }

    #[tokio::test]
    async fn attestation_fails_with_another_root() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let attestation = mock_attestation(key.attestation().await.unwrap());
        let other = MockRootCa::generate();
        assert!(matches!(
            MockAttestationVerifier::new(&other)
                .verify(&attestation, &key.public_key()),
            Err(AttestationError::UntrustedRoot)
        ));
    }

    #[tokio::test]
    async fn attestation_fails_after_the_attestation_key_is_overwritten() {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        device.overwrite_attestation_key();
        let attestation = mock_attestation(key.attestation().await.unwrap());
        assert!(matches!(
            MockAttestationVerifier::new(&root)
                .verify(&attestation, &key.public_key()),
            Err(AttestationError::UntrustedRoot)
        ));
    }

    #[tokio::test]
    async fn attestation_fails_with_a_tampered_policy() {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO,
            )
            .unwrap();
        let mut attestation =
            mock_attestation(key.attestation().await.unwrap());

        // Rewrite touch "always" (2) as "never" (1) in the policy extension.
        let mut der = attestation.slot_certificate.as_der().to_vec();
        let policy_oid = piv::OID_YUBICO_POLICY.as_bytes();
        let at = der
            .windows(policy_oid.len())
            .position(|w| w == policy_oid)
            .expect("the policy extension is there");
        // OID, then OCTET STRING (04 02) holding [pin, touch].
        let touch = at + policy_oid.len() + 3;
        assert_eq!(der[touch], 2);
        der[touch] = 1;
        attestation.slot_certificate = DerCertificate::from_der(der);

        assert!(matches!(
            MockAttestationVerifier::new(&root)
                .verify(&attestation, &key.public_key()),
            Err(AttestationError::BadSignature { index: 0 })
        ));
    }

    #[tokio::test]
    async fn attestation_of_another_key_is_a_mismatch() {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let a = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let b = device
            .generate_key(
                PivSlot::Signature,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::SUDO,
            )
            .unwrap();
        let attestation = mock_attestation(a.attestation().await.unwrap());
        assert!(matches!(
            MockAttestationVerifier::new(&root)
                .verify(&attestation, &b.public_key()),
            Err(AttestationError::KeyMismatch)
        ));
    }

    #[tokio::test]
    async fn mock_bundle_is_tagged_as_a_mock_and_round_trips() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let bundle = key.attestation().await.unwrap();
        let json = serde_json::to_value(&bundle).unwrap();
        assert_eq!(json["format"], "mock_piv");
        assert_eq!(
            serde_json::from_value::<AttestationBundle>(json).unwrap(),
            bundle
        );
    }

    #[tokio::test]
    async fn hardware_kind_reports_device_facts() {
        let (_, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key: Box<dyn OperatorSigner> = Box::new(
            device
                .generate_key(
                    PivSlot::Signature,
                    SignatureAlgorithm::Ed25519,
                    KeyPolicy::SUDO,
                )
                .unwrap(),
        );
        let SignerKind::Hardware(hardware) = key.kind() else {
            panic!("a mock key is hardware");
        };
        assert_eq!(hardware.policy(), KeyPolicy::SUDO);
        assert_eq!(hardware.serial(), DeviceSerial::Mock(SERIAL));
        assert_eq!(hardware.firmware(), MockPivDevice::FIRMWARE_ED25519);
        assert_eq!(
            key.credential_kind(),
            crate::enums::OperatorCredentialKind::HardwareKey
        );
    }

    #[test]
    fn debug_is_redacted() {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        assert_eq!(format!("{root:?}"), "MockRootCa([REDACTED])");
        let debug = format!("{device:?}");
        assert!(debug.contains("[REDACTED]") && !debug.contains(PIN));
    }

    // ---- Regression tests from the review of #235 ----

    /// A legitimately attested P-256 slot key, the attacker's own.
    async fn attacker_slot() -> (MockRootCa, MockHardwareKey, MockPivAttestation)
    {
        let (root, device) = device(MockPivDevice::FIRMWARE_P256_ONLY);
        let slot = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::EcdsaP256,
                KeyPolicy {
                    pin: PinPolicy::Never,
                    touch: TouchPolicy::Never,
                },
            )
            .unwrap();
        let real = mock_attestation(slot.attestation().await.unwrap());
        (root, slot, real)
    }

    /// A "slot certificate" for `claimed`, signed by `slot` with itself as
    /// issuer, claiming PIN and touch always and another serial.
    async fn forge(
        slot: &MockHardwareKey,
        real: &MockPivAttestation,
        claimed: &OperatorPublicKey,
        critical_unknown: bool,
    ) -> DerCertificate {
        let issuer = Certificate::from_der(real.slot_certificate.as_der())
            .unwrap()
            .tbs_certificate()
            .subject()
            .clone();
        let mut extensions = vec![
            extension(piv::OID_YUBICO_FIRMWARE, vec![5, 7, 2]),
            extension(piv::OID_YUBICO_SERIAL, 99_999_999u32.to_der().unwrap()),
            extension(piv::OID_YUBICO_POLICY, vec![3, 2]),
        ];
        if critical_unknown {
            extensions.push(Extension {
                extn_id: x509_cert::der::asn1::ObjectIdentifier::new_unwrap(
                    "1.2.3.4.5",
                ),
                critical: true,
                extn_value: OctetString::new(vec![0]).unwrap(),
            });
        }
        let algorithm = AlgorithmIdentifierOwned {
            oid: piv::OID_ECDSA_SHA256,
            parameters: None,
        };
        let tbs = tbs_certificate(
            1,
            &algorithm,
            &issuer,
            &Name::from_str("CN=forged").unwrap(),
            piv::spki(claimed),
            extensions,
        );
        // The slot key signs whatever it is handed: ECDSA-SHA256 over it.
        let OperatorSignature::EcdsaP256(signature) =
            slot.sign(&tbs).await.unwrap()
        else {
            panic!("a P-256 slot");
        };
        let signature =
            p256::ecdsa::Signature::from_slice(&signature.to_bytes()).unwrap();
        assemble(tbs, &algorithm, &piv::ecdsa_to_der(&signature.to_bytes()))
    }

    #[tokio::test]
    async fn a_slot_key_cannot_issue_an_attested_leaf() {
        let (root, slot, real) = attacker_slot().await;
        let (_, vk) = crypto::generate_keypair();
        let claimed = OperatorPublicKey::Ed25519(vk);
        let verifier = MockAttestationVerifier::new(&root);

        for critical_unknown in [true, false] {
            let evidence = MockPivAttestation {
                slot_certificate: forge(
                    &slot,
                    &real,
                    &claimed,
                    critical_unknown,
                )
                .await,
                device_chain: vec![
                    real.slot_certificate.clone(),
                    real.device_chain[0].clone(),
                ],
            };
            let result = verifier.verify(&evidence, &claimed);
            assert!(result.is_err(), "forged leaf accepted: {result:?}");
        }

        // Each rule refuses it on its own.
        let evidence = |slot_certificate| MockPivAttestation {
            slot_certificate,
            device_chain: vec![real.slot_certificate.clone()],
        };
        assert!(matches!(
            verifier.verify(
                &evidence(forge(&slot, &real, &claimed, true).await),
                &claimed
            ),
            Err(AttestationError::UnknownCriticalExtension { index: 0, .. })
        ));
        assert!(matches!(
            verifier.verify(
                &evidence(forge(&slot, &real, &claimed, false).await),
                &claimed
            ),
            Err(AttestationError::PolicyAboveLeaf { index: 1 })
        ));
    }

    #[tokio::test]
    async fn the_device_chain_is_f9_and_at_most_a_copy_of_its_anchor() {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let real = mock_attestation(key.attestation().await.unwrap());
        let f9 = real.device_chain[0].clone();
        let verifier = MockAttestationVerifier::new(&root);
        let with = |device_chain| MockPivAttestation {
            slot_certificate: real.slot_certificate.clone(),
            device_chain,
        };
        verifier
            .verify(
                &with(vec![f9.clone(), root.certificate().clone()]),
                &key.public_key(),
            )
            .unwrap();
        assert!(matches!(
            verifier
                .verify(&with(vec![f9.clone(), f9.clone()]), &key.public_key()),
            Err(AttestationError::ChainLength { len: 2 })
        ));
        assert!(matches!(
            verifier.verify(
                &with(vec![f9.clone(), f9.clone(), f9]),
                &key.public_key()
            ),
            Err(AttestationError::ChainLength { len: 3 })
        ));
        assert!(matches!(
            verifier.verify(&with(vec![]), &key.public_key()),
            Err(AttestationError::ChainLength { len: 0 })
        ));
    }

    /// `f9` reissued by the root with `extensions`, for the real slot key.
    async fn with_f9_extensions(
        extensions: Vec<Extension>,
    ) -> Result<AttestedKey, AttestationError> {
        let (root, device) = device(MockPivDevice::FIRMWARE_ED25519);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::Ed25519,
                KeyPolicy::UNATTENDED,
            )
            .unwrap();
        let real = mock_attestation(key.attestation().await.unwrap());
        let f9 = {
            let state = device.lock();
            issue(
                u64::from(SERIAL.get()),
                &root.name,
                &device_name(),
                state.attestation_key.spki(),
                extensions,
                &root.key,
            )
        };
        MockAttestationVerifier::new(&root).verify(
            &MockPivAttestation {
                slot_certificate: real.slot_certificate,
                device_chain: vec![f9],
            },
            &key.public_key(),
        )
    }

    #[tokio::test]
    async fn f9_must_be_a_ca() {
        let bc = |ca: bool, path_len_constraint: Option<u8>| {
            critical_extension(
                &BasicConstraints {
                    ca,
                    path_len_constraint,
                },
                OID_BASIC_CONSTRAINTS,
            )
        };
        let ku = |usage: KeyUsages| {
            critical_extension(&KeyUsage(usage.into()), OID_KEY_USAGE)
        };
        let cases: Vec<(&str, Vec<Extension>, bool)> = vec![
            ("no extensions", vec![], false),
            ("cA=FALSE", vec![bc(false, None)], false),
            (
                "cA=TRUE, keyUsage digitalSignature only",
                vec![bc(true, None), ku(KeyUsages::DigitalSignature)],
                false,
            ),
            (
                "keyUsage keyCertSign, no basicConstraints",
                vec![ku(KeyUsages::KeyCertSign)],
                false,
            ),
            ("cA=TRUE, no keyUsage", vec![bc(true, None)], true),
            ("cA=TRUE, pathlen:0", vec![bc(true, Some(0))], true),
        ];
        for (name, extensions, accepted) in cases {
            let result = with_f9_extensions(extensions).await;
            if accepted {
                assert!(result.is_ok(), "{name}: {result:?}");
            } else {
                assert!(
                    matches!(
                        result,
                        Err(AttestationError::NotACa { index: 1 })
                    ),
                    "{name}: {result:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn inner_and_outer_algorithms_must_agree() {
        let (root, slot, real) = attacker_slot().await;
        let algorithm = AlgorithmIdentifierOwned {
            oid: piv::OID_ECDSA_SHA256,
            parameters: None,
        };
        let other = AlgorithmIdentifierOwned {
            oid: piv::OID_RSA_SHA256,
            parameters: Some(Any::from(Null)),
        };
        // Re-wrap the real slot certificate's TBS under another outer
        // algorithm, keeping its signature.
        let cert =
            Certificate::from_der(real.slot_certificate.as_der()).unwrap();
        let tbs = cert.tbs_certificate().to_der().unwrap();
        assert_eq!(*cert.signature_algorithm(), algorithm);
        let signature = cert.signature().as_bytes().unwrap().to_vec();
        let evidence = MockPivAttestation {
            slot_certificate: assemble(tbs, &other, &signature),
            device_chain: real.device_chain.clone(),
        };
        assert!(matches!(
            MockAttestationVerifier::new(&root)
                .verify(&evidence, &slot.public_key()),
            Err(AttestationError::AlgorithmMismatch { index: 0 })
        ));
    }

    #[tokio::test]
    async fn duplicate_extensions_are_refused() {
        let (root, slot, real) = attacker_slot().await;
        let device = slot.device();
        let state = device.lock();
        let slot_key = slot.slot_key(&state).unwrap();
        let policy = extension(
            piv::OID_YUBICO_POLICY,
            piv::policy_bytes(slot_key.policy).to_vec(),
        );
        let issuer = device_name();
        let tbs = tbs_certificate(
            2,
            &state.attestation_key.algorithm(),
            &issuer,
            &Name::from_str("CN=dup").unwrap(),
            piv::spki(&slot_key.public_key),
            vec![
                extension(piv::OID_YUBICO_FIRMWARE, vec![5, 4, 3]),
                extension(
                    piv::OID_YUBICO_SERIAL,
                    SERIAL.get().to_der().unwrap(),
                ),
                policy.clone(),
                policy,
            ],
        );
        let signature = state.attestation_key.sign(&tbs);
        let evidence = MockPivAttestation {
            slot_certificate: assemble(
                tbs,
                &state.attestation_key.algorithm(),
                &signature,
            ),
            device_chain: real.device_chain.clone(),
        };
        drop(state);
        assert!(matches!(
            MockAttestationVerifier::new(&root)
                .verify(&evidence, &slot.public_key()),
            Err(AttestationError::DuplicateExtension { index: 0, .. })
        ));
    }

    #[tokio::test]
    async fn verifiers_refuse_each_others_bundles() {
        let (root, slot, real) = attacker_slot().await;
        let mock = MockAttestationVerifier::new(&root);
        // The mock root is refused as a Yubico root.
        assert!(matches!(
            piv::YubicoAttestationVerifier::from_anchors(std::slice::from_ref(
                root.certificate()
            )),
            Err(AttestationError::RootRefused { .. })
        ));
        // A mock attestation relabelled as Yubico's reaches the mock
        // verifier only as the wrong format.
        let relabelled =
            AttestationBundle::YubicoPiv(super::super::PivAttestation {
                slot_certificate: real.slot_certificate.clone(),
                device_chain: real.device_chain.clone(),
            });
        assert!(matches!(
            mock.verify_bundle(&relabelled, &slot.public_key()),
            Err(AttestationError::WrongFormat {
                format: "yubico_piv"
            })
        ));
        mock.verify_bundle(
            &AttestationBundle::MockPiv(real),
            &slot.public_key(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn high_s_signatures_are_refused() {
        let (_, device) = device(MockPivDevice::FIRMWARE_P256_ONLY);
        let key = device
            .generate_key(
                PivSlot::Authentication,
                SignatureAlgorithm::EcdsaP256,
                KeyPolicy {
                    pin: PinPolicy::Never,
                    touch: TouchPolicy::Never,
                },
            )
            .unwrap();
        let OperatorSignature::EcdsaP256(signature) =
            sign_request(&key, b"p", 1).await.unwrap()
        else {
            panic!("a P-256 key");
        };
        let low =
            p256::ecdsa::Signature::from_slice(&signature.to_bytes()).unwrap();
        let (r, s) = low.split_scalars();
        let high = p256::ecdsa::Signature::from_scalars(r, -s).unwrap();
        assert_ne!(high.to_bytes(), low.to_bytes());
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(&high.to_bytes());
        assert!(EcdsaP256Signature::from_bytes(bytes).is_err());
        verify_request(
            &key.public_key(),
            b"p",
            1,
            &OperatorSignature::EcdsaP256(signature),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn attestation_chains_through_a_p384_intermediate() {
        for root in [
            MockRootCa::generate(),
            MockRootCa::rsa_from_pkcs1_der(RSA_ROOT).unwrap(),
        ] {
            let intermediate = root.intermediate_p384();
            let device = MockPivDevice::manufacture(
                &intermediate,
                SERIAL,
                MockPivDevice::FIRMWARE_ED25519,
                Pin::new(PIN),
            );
            let key = device
                .generate_key(
                    PivSlot::Authentication,
                    SignatureAlgorithm::Ed25519,
                    KeyPolicy::UNATTENDED,
                )
                .unwrap();
            let mut attestation =
                mock_attestation(key.attestation().await.unwrap());
            // `[f9, intermediate]`: the intermediate is the anchor, as
            // Yubico's PIV Attestation B 1 is.
            assert_eq!(attestation.device_chain.len(), 2);
            let verifier = MockAttestationVerifier::new(&intermediate);
            let attested =
                verifier.verify(&attestation, &key.public_key()).unwrap();
            assert_eq!(attested.serial(), DeviceSerial::Mock(SERIAL));

            // The root above it isn't an anchor for `f9`.
            assert!(matches!(
                MockAttestationVerifier::new(&root)
                    .verify(&attestation, &key.public_key()),
                Err(AttestationError::ChainLength { len: 2 })
            ));
            attestation.device_chain.truncate(1);
            assert!(matches!(
                MockAttestationVerifier::new(&root)
                    .verify(&attestation, &key.public_key()),
                Err(AttestationError::UntrustedRoot)
            ));

            // Flip a byte of the P-384 signature on `f9`'s certificate.
            let mut der = attestation.device_chain[0].as_der().to_vec();
            let last = der.len() - 1;
            der[last] ^= 1;
            attestation.device_chain[0] = DerCertificate::from_der(der);
            assert!(matches!(
                verifier.verify(&attestation, &key.public_key()),
                Err(AttestationError::UntrustedRoot)
            ));
        }
    }
}
