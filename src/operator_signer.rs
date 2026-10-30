//! Signing as an operator credential: software keys, hardware keys, and
//! hardware-key attestation.
//!
//! An operator credential (design: `docs/design-operator-api.md`,
//! "Authentication") is a `software_key` (an Ed25519 key in a file) or a
//! hardware key (today a YubiKey PIV slot) whose key was generated on the
//! device and can't leave it. Both sign the same bytes the same way, so the
//! code that builds and sends an operator request takes a
//! `&dyn` [`OperatorSigner`] and doesn't care which it has.
//!
//! - [`OperatorSigner`]: sign bytes; say the public key, algorithm and
//!   [`SignerKind`].
//! - [`HardwareKey`]: what only a hardware key has: its [`KeyPolicy`] (PIN
//!   and touch), its device serial and firmware, a PIN to present, and an
//!   [`AttestationBundle`] proving the key was made on the device.
//! - [`AttestationVerifier`]: the server side. Checks a bundle against its
//!   trusted roots and says what it proves, as an [`AttestedKey`].
//! - [`SoftwareKeySigner`]: an Ed25519 key loaded from a secret file.
//! - `mock` (feature `mock-hardware-key`): a software model of a PIV
//!   device, with a mock root CA, for tests and dev builds.
//! - `piv` (feature `x509`): parsing a PIV attestation certificate and
//!   checking its chain, shared by the mock verifier and
//!   `piv::YubicoAttestationVerifier`.
//! - `fido` (feature `x509`): a passkey's FIDO attestation, against its own
//!   pinned trust store, never mixed with `piv`'s.
//! - `yubikey` (feature `yubikey`): `yubikey::YubikeySigner`, a key in a
//!   YubiKey's PIV slot over PC/SC, and the provisioning calls an operator
//!   CLI needs (generate, attest, reset PIV). Only a client enables it: it
//!   links the system's PC/SC library.
//!
//! # Sync or async
//!
//! Signing is `async` ([`async_trait`], so the traits stay usable as
//! `dyn`). A hardware signature can wait seconds: a YubiKey with touch
//! policy *always* holds the call until it is touched or ~15 s pass, and
//! other vendors' keys (a cloud HSM, a remote signing service) are network
//! calls. A synchronous trait would leave every caller on an async runtime
//! to remember `spawn_blocking`, and the one that forgets stalls its
//! executor for the length of a tap. The blocking PC/SC call belongs inside
//! the implementation, which knows it blocks. Facts a signer learned when it
//! was opened (public key, policy, serial) are plain getters.
//!
//! # What gets signed
//!
//! [`OperatorSigner::sign`] signs exactly the bytes it is handed:
//! - Ed25519: pure Ed25519 over the message.
//! - ECDSA P-256: ECDSA with SHA-256 over the message (the signer hashes;
//!   a PIV implementation hashes and sends the digest to the card).
//!
//! An operator request signs [`crypto::signing_digest`] of its canonical
//! bytes and timestamp, as every signed body does; [`sign_request`] and
//! [`verify_request`] do that, so an Ed25519 operator signature also
//! verifies with [`crypto::verify`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::crypto::{self, Signature, VerifyingKey};
use crate::enums::OperatorCredentialKind;
use crate::ids::OperatorId;
use crate::secrets::Secret;

#[cfg(feature = "x509")]
pub mod fido;
#[cfg(feature = "mock-hardware-key")]
pub mod mock;
#[cfg(feature = "x509")]
pub mod piv;
mod software;
#[cfg(feature = "yubikey")]
pub mod yubikey;

pub use software::SoftwareKeySigner;

// ---------------------------------------------------------------------------
// Algorithms, keys, signatures
// ---------------------------------------------------------------------------

/// A signature algorithm an operator credential may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureAlgorithm {
    /// Ed25519. Software keys, and YubiKeys from firmware
    /// [`YUBIKEY_ED25519_MIN_FIRMWARE`].
    Ed25519,
    /// ECDSA over NIST P-256 with SHA-256. Older YubiKeys. Signing and
    /// verifying need the `p256` feature.
    EcdsaP256,
}

impl SignatureAlgorithm {
    /// Every algorithm, in declaration order.
    pub const ALL: [Self; 2] = [Self::Ed25519, Self::EcdsaP256];
}

impl fmt::Display for SignatureAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ed25519 => "Ed25519",
            Self::EcdsaP256 => "ECDSA P-256",
        })
    }
}

/// The first YubiKey firmware with Ed25519 (and X25519) in PIV. Older keys
/// sign with [`SignatureAlgorithm::EcdsaP256`].
pub const YUBIKEY_ED25519_MIN_FIRMWARE: FirmwareVersion =
    FirmwareVersion::new(5, 7, 0);

/// A P-256 public key: its SEC1 uncompressed encoding, `0x04 ‖ X ‖ Y`.
///
/// With the `p256` feature, [`from_sec1`](Self::from_sec1) also checks the
/// point is on the curve; without it, only the shape is checked, and the
/// key can be carried but not used to verify.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EcdsaP256PublicKey([u8; 65]);

impl EcdsaP256PublicKey {
    /// From SEC1 bytes, uncompressed (65 bytes) or, with the `p256`
    /// feature, compressed (33 bytes).
    pub fn from_sec1(bytes: &[u8]) -> Result<Self, KeyError> {
        #[cfg(feature = "p256")]
        {
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(bytes)
                .map_err(|_| KeyError::InvalidP256Point)?;
            Ok(Self::from_p256(&key))
        }
        #[cfg(not(feature = "p256"))]
        {
            let array: [u8; 65] =
                bytes.try_into().map_err(|_| KeyError::Length {
                    expected: 65,
                    got: bytes.len(),
                })?;
            if array[0] != 0x04 {
                return Err(KeyError::InvalidP256Point);
            }
            Ok(Self(array))
        }
    }

    /// The SEC1 uncompressed encoding.
    pub fn as_sec1(&self) -> &[u8; 65] {
        &self.0
    }

    /// From a `p256` verifying key.
    #[cfg(feature = "p256")]
    pub fn from_p256(key: &p256::ecdsa::VerifyingKey) -> Self {
        let point = key.to_sec1_point(false);
        let mut out = [0u8; 65];
        out.copy_from_slice(point.as_bytes());
        Self(out)
    }

    /// As a `p256` verifying key.
    #[cfg(feature = "p256")]
    pub fn to_p256(&self) -> p256::ecdsa::VerifyingKey {
        // Every constructor checked the point.
        p256::ecdsa::VerifyingKey::from_sec1_bytes(&self.0)
            .expect("checked on construction")
    }
}

impl fmt::Debug for EcdsaP256PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EcdsaP256PublicKey({})", hex::encode(self.0))
    }
}

/// A P-256 ECDSA signature, fixed size: `r ‖ s`, 32 bytes each, big
/// endian, `s` normalized low. (PIV returns DER; an implementation
/// converts.)
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EcdsaP256Signature([u8; 64]);

/// Half the order of P-256, big endian: the largest canonical (low) `s`.
const P256_HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0x80, 0x00, 0x00, 0x00, 0x7f, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xde, 0x73, 0x7d, 0x56, 0xd3, 0x8b, 0xcf, 0x42,
    0x79, 0xdc, 0xe5, 0x61, 0x7e, 0x31, 0x92, 0xa8,
];

impl EcdsaP256Signature {
    /// From `r ‖ s`. A high `s` (above half the group order) or a zero `s`
    /// is refused: `(r, n − s)` verifies wherever `(r, s)` does, and only
    /// one of the pair is canonical.
    pub fn from_bytes(bytes: [u8; 64]) -> Result<Self, KeyError> {
        let s: &[u8] = &bytes[32..];
        if s > P256_HALF_ORDER.as_slice() || s.iter().all(|b| *b == 0) {
            return Err(KeyError::NonCanonicalSignature);
        }
        Ok(Self(bytes))
    }

    /// As `r ‖ s`.
    pub const fn to_bytes(&self) -> [u8; 64] {
        self.0
    }

    /// From a `p256` signature, with `s` normalized low.
    #[cfg(feature = "p256")]
    pub fn from_p256(signature: &p256::ecdsa::Signature) -> Self {
        let signature = signature.normalize_s();
        let mut out = [0u8; 64];
        out.copy_from_slice(&signature.to_bytes());
        Self(out)
    }
}

impl fmt::Debug for EcdsaP256Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EcdsaP256Signature({})", hex::encode(self.0))
    }
}

/// An operator credential's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorPublicKey {
    /// Ed25519.
    Ed25519(VerifyingKey),
    /// ECDSA P-256.
    EcdsaP256(EcdsaP256PublicKey),
}

impl OperatorPublicKey {
    /// The key's raw bytes as hex: Ed25519's 32, or P-256's 65-byte
    /// uncompressed SEC1
    pub fn to_hex(&self) -> String {
        match self {
            Self::Ed25519(key) => hex::encode(key.as_bytes()),
            Self::EcdsaP256(key) => hex::encode(key.as_sec1()),
        }
    }

    /// The key's raw bytes: Ed25519's 32, or P-256's 65-byte uncompressed
    /// SEC1 (what [`to_hex`](Self::to_hex) encodes, and what the server
    /// stores)
    pub fn to_raw(&self) -> Vec<u8> {
        match self {
            Self::Ed25519(key) => key.as_bytes().to_vec(),
            Self::EcdsaP256(key) => key.as_sec1().to_vec(),
        }
    }

    /// From [`to_raw`](Self::to_raw)'s bytes. The lengths differ, so they
    /// name the algorithm: 32 bytes are Ed25519, 65 are P-256 (uncompressed
    /// SEC1, checked on the curve with the `p256` feature). Anything else,
    /// or an Ed25519 encoding that is no point, is refused.
    pub fn from_raw(bytes: &[u8]) -> Result<Self, KeyError> {
        match bytes.len() {
            32 => {
                let array: &[u8; 32] = bytes.try_into().expect("32 bytes");
                VerifyingKey::from_bytes(array)
                    .map(Self::Ed25519)
                    .map_err(|_| KeyError::InvalidEd25519Point)
            }
            65 => EcdsaP256PublicKey::from_sec1(bytes).map(Self::EcdsaP256),
            got => Err(KeyError::KeyLength { got }),
        }
    }

    /// The key's algorithm.
    pub const fn algorithm(&self) -> SignatureAlgorithm {
        match self {
            Self::Ed25519(_) => SignatureAlgorithm::Ed25519,
            Self::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        }
    }

    /// Check `signature` over exactly `message` (see the module docs for
    /// what each algorithm signs). Ed25519 is checked strictly, as
    /// [`crypto::verify`] does.
    pub fn verify(
        &self,
        message: &[u8],
        signature: &OperatorSignature,
    ) -> Result<(), VerifyError> {
        match (self, signature) {
            (Self::Ed25519(key), OperatorSignature::Ed25519(signature)) => key
                .verify_strict(message, signature)
                .map_err(|_| VerifyError::BadSignature),
            (Self::EcdsaP256(key), OperatorSignature::EcdsaP256(signature)) => {
                verify_p256(key, message, signature)
            }
            (key, signature) => Err(VerifyError::AlgorithmMismatch {
                key: key.algorithm(),
                signature: signature.algorithm(),
            }),
        }
    }
}

/// Hex of [`OperatorPublicKey::to_raw`] on the wire (0.140)
impl Serialize for OperatorPublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for OperatorPublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        let bytes =
            hex::decode(text.trim()).map_err(serde::de::Error::custom)?;
        Self::from_raw(&bytes).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for OperatorPublicKey {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "OperatorPublicKey".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "hex: Ed25519's 32 bytes, or P-256's 65-byte uncompressed SEC1",
        })
    }
}

#[cfg(feature = "p256")]
fn verify_p256(
    key: &EcdsaP256PublicKey,
    message: &[u8],
    signature: &EcdsaP256Signature,
) -> Result<(), VerifyError> {
    use p256::ecdsa::signature::Verifier;
    let signature = p256::ecdsa::Signature::from_slice(&signature.0)
        .map_err(|_| VerifyError::BadSignature)?;
    // Every constructor refuses a high `s`; check again at the boundary.
    if signature.normalize_s() != signature {
        return Err(VerifyError::BadSignature);
    }
    key.to_p256()
        .verify(message, &signature)
        .map_err(|_| VerifyError::BadSignature)
}

#[cfg(not(feature = "p256"))]
fn verify_p256(
    _: &EcdsaP256PublicKey,
    _: &[u8],
    _: &EcdsaP256Signature,
) -> Result<(), VerifyError> {
    Err(VerifyError::NotBuiltIn {
        algorithm: SignatureAlgorithm::EcdsaP256,
        feature: "p256",
    })
}

/// A signature's 64 bytes, read as an algorithm's once the key is known
/// ([`OperatorSignature::to_bytes`]); hex on the wire (0.140)
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct SignatureBytes(
    #[serde(with = "hex_array_64")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub [u8; 64],
);

impl SignatureBytes {
    /// As `algorithm`'s signature ([`OperatorSignature::from_bytes`])
    pub fn read_as(
        &self,
        algorithm: SignatureAlgorithm,
    ) -> Result<OperatorSignature, KeyError> {
        OperatorSignature::from_bytes(algorithm, &self.0)
    }
}

impl From<OperatorSignature> for SignatureBytes {
    fn from(signature: OperatorSignature) -> Self {
        Self(signature.to_bytes())
    }
}

impl fmt::Debug for SignatureBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SignatureBytes({})", hex::encode(self.0))
    }
}

/// 64 bytes as a hex string
mod hex_array_64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        bytes: &[u8; 64],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[u8; 64], D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        let bytes =
            hex::decode(text.as_ref()).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("a signature is 64 bytes"))
    }
}

/// A signature by an operator credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorSignature {
    /// Ed25519.
    Ed25519(Signature),
    /// ECDSA P-256.
    EcdsaP256(EcdsaP256Signature),
}

impl OperatorSignature {
    /// The signature's 64 bytes: Ed25519's, or P-256's `r ‖ s` (what the
    /// signature header carries, as hex)
    pub fn to_bytes(&self) -> [u8; 64] {
        match self {
            Self::Ed25519(signature) => signature.to_bytes(),
            Self::EcdsaP256(signature) => signature.to_bytes(),
        }
    }

    /// [`to_bytes`](Self::to_bytes) as lowercase hex.
    pub fn to_hex(&self) -> String {
        hex::encode(self.to_bytes())
    }

    /// From [`to_bytes`](Self::to_bytes)'s bytes, read as `algorithm`'s
    /// (the key's: both algorithms' signatures are 64 bytes). A P-256
    /// signature with a high or zero `s` is refused.
    pub fn from_bytes(
        algorithm: SignatureAlgorithm,
        bytes: &[u8],
    ) -> Result<Self, KeyError> {
        let array: [u8; 64] =
            bytes.try_into().map_err(|_| KeyError::Length {
                expected: 64,
                got: bytes.len(),
            })?;
        match algorithm {
            SignatureAlgorithm::Ed25519 => {
                Ok(Self::Ed25519(Signature::from_bytes(&array)))
            }
            SignatureAlgorithm::EcdsaP256 => {
                EcdsaP256Signature::from_bytes(array).map(Self::EcdsaP256)
            }
        }
    }

    /// From [`to_hex`](Self::to_hex)'s text (surrounding whitespace
    /// ignored), read as `algorithm`'s.
    pub fn from_hex(
        algorithm: SignatureAlgorithm,
        text: &str,
    ) -> Result<Self, KeyError> {
        let bytes = hex::decode(text.trim()).map_err(|_| KeyError::NotHex)?;
        Self::from_bytes(algorithm, &bytes)
    }

    /// The signature's algorithm.
    pub const fn algorithm(&self) -> SignatureAlgorithm {
        match self {
            Self::Ed25519(_) => SignatureAlgorithm::Ed25519,
            Self::EcdsaP256(_) => SignatureAlgorithm::EcdsaP256,
        }
    }
}

// ---------------------------------------------------------------------------
// Hardware policy and identity
// ---------------------------------------------------------------------------

pub use crate::enums::{PinPolicy, TouchPolicy};

/// A hardware key's PIN and touch policies, fixed when the key was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyPolicy {
    /// When it asks for the PIN.
    pub pin: PinPolicy,
    /// When it needs a touch.
    pub touch: TouchPolicy,
}

impl KeyPolicy {
    /// Unattended signing (the > 5 agents tier): PIN once, touch never.
    pub const UNATTENDED: Self = Self {
        pin: PinPolicy::Once,
        touch: TouchPolicy::Never,
    };
    /// The sudo slot: PIN once, a touch for every signature.
    pub const SUDO: Self = Self {
        pin: PinPolicy::Once,
        touch: TouchPolicy::Always,
    };
    /// The sudo slot with the PIN required too: PIN and touch every time.
    pub const SUDO_WITH_PIN: Self = Self {
        pin: PinPolicy::Always,
        touch: TouchPolicy::Always,
    };
}

/// A PIV key slot (NIST SP 800-73). PIV-specific: a hardware key from a
/// vendor without slots never names one, which is why no trait method takes
/// a slot.
///
/// The operator setup (design, "Hardware tiers"): the unattended key in
/// [`Authentication`](Self::Authentication) (9a) with touch never, the
/// sudo key in [`Signature`](Self::Signature) (9c) with touch always.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PivSlot {
    /// 9a, PIV Authentication.
    Authentication,
    /// 9c, Digital Signature.
    Signature,
    /// 9d, Key Management.
    KeyManagement,
    /// 9e, Card Authentication.
    CardAuthentication,
}

impl PivSlot {
    /// Every slot, in declaration order.
    pub const ALL: [Self; 4] = [
        Self::Authentication,
        Self::Signature,
        Self::KeyManagement,
        Self::CardAuthentication,
    ];

    /// The slot's number (`0x9a` …).
    pub const fn id(self) -> u8 {
        match self {
            Self::Authentication => 0x9a,
            Self::Signature => 0x9c,
            Self::KeyManagement => 0x9d,
            Self::CardAuthentication => 0x9e,
        }
    }
}

impl fmt::Display for PivSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.id())
    }
}

/// A device's firmware version.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
)]
pub struct FirmwareVersion {
    /// Major.
    pub major: u8,
    /// Minor.
    pub minor: u8,
    /// Patch.
    pub patch: u8,
}

impl FirmwareVersion {
    /// `major.minor.patch`.
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A PIV device's serial number, as Yubico's serial extension
/// (`1.3.6.1.4.1.41482.3.7`) carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PivSerial(u32);

impl PivSerial {
    /// From the device's number.
    pub const fn new(serial: u32) -> Self {
        Self(serial)
    }

    /// The number.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PivSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A hardware device's serial number, namespaced by who vouches for it.
///
/// The server keeps one physical key per operator account by an HMAC of
/// this (design, "Hardware tiers"); [`hmac_input`](Self::hmac_input) is
/// what to feed it, so equal numbers from different vendors don't collide.
/// Only a verifier's [`AttestedKey`] carries one the server may trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceSerial {
    /// A YubiKey's serial.
    Yubico(PivSerial),
    /// A mock device's serial (`mock`). The variant exists in every build,
    /// so enabling the mock feature anywhere in a workspace changes no
    /// type; only the mock verifier makes one.
    Mock(PivSerial),
}

impl DeviceSerial {
    /// The bytes to HMAC: the vendor's name, a newline, the decimal serial.
    pub fn hmac_input(&self) -> Vec<u8> {
        match self {
            Self::Yubico(n) => format!("yubico\n{n}").into_bytes(),
            Self::Mock(n) => format!("mock\n{n}").into_bytes(),
        }
    }
}

impl fmt::Display for DeviceSerial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yubico(n) => write!(f, "YubiKey {n}"),
            Self::Mock(n) => write!(f, "MOCK device {n}"),
        }
    }
}

/// Whether this build has the mock hardware key (feature
/// `mock-hardware-key`). **A release server must assert this is `false`**
/// at startup: cargo unifies features across a workspace, so a test or dev
/// crate enabling the mock can compile it into the server binary.
pub const MOCK_HARDWARE_KEY: bool = cfg!(feature = "mock-hardware-key");

/// A hardware key's PIN: a [`Secret`], so zeroized on drop and never
/// printed.
#[derive(Clone)]
pub struct Pin(Secret);

impl Pin {
    /// From the PIN's text.
    pub fn new(pin: impl Into<String>) -> Self {
        Self(Secret::new(pin.into()))
    }

    /// From a secret file's value (`Secret::from_file`).
    pub fn from_secret(secret: Secret) -> Self {
        Self(secret)
    }

    /// The PIN's text, for the device call. Use sparingly.
    pub fn expose(&self) -> &str {
        self.0.expose()
    }
}

impl fmt::Debug for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Bytes as a hex string.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        bytes: &[u8],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<u8>, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        hex::decode(text.as_ref()).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Attestation evidence
// ---------------------------------------------------------------------------

/// A DER-encoded X.509 certificate, as bytes. Not parsed or checked until a
/// verifier does (feature `x509`, `piv`). Hex on the wire.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DerCertificate(
    #[serde(with = "hex_bytes")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    Vec<u8>,
);

impl DerCertificate {
    /// From DER bytes.
    pub fn from_der(der: Vec<u8>) -> Self {
        Self(der)
    }

    /// The DER bytes.
    pub fn as_der(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for DerCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DerCertificate({} bytes)", self.0.len())
    }
}

/// A PIV slot's attestation, as a device returns it.
///
/// On a YubiKey: the slot's attestation certificate (made on request,
/// signed by the `f9` key) and the certificates above it the device holds
/// (`f9`'s own, and on newer devices an intermediate). The verifier
/// supplies the roots; it must never trust `f9` itself, which users can
/// overwrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct PivAttestation {
    /// The slot's attestation certificate: its subject key is the slot's
    /// key, and its extensions carry serial, firmware and policy.
    pub slot_certificate: DerCertificate,
    /// The device's certificates above it, nearest first (`f9` first).
    pub device_chain: Vec<DerCertificate>,
}

/// A mock device's attestation: the same shape as a [`PivAttestation`],
/// and deliberately not the same type. Only the mock verifier (feature
/// `mock-hardware-key`) reads it; every other verifier refuses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct MockPivAttestation {
    /// The slot's attestation certificate.
    pub slot_certificate: DerCertificate,
    /// The device's certificate (`f9`-style).
    pub device_chain: Vec<DerCertificate>,
}

/// What a hardware key offers as proof of where its key lives.
///
/// Closed by format, and the same in every build: a mock attestation is its
/// own variant and type, which every verifier but the mock one refuses
/// ([`AttestationError::WrongFormat`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum AttestationBundle {
    /// Yubico PIV attestation (X.509), checked against Yubico's roots.
    YubicoPiv(PivAttestation),
    /// A mock device's attestation, checked against a mock root CA.
    MockPiv(MockPivAttestation),
}

impl AttestationBundle {
    /// The format's name, as tagged on the wire.
    pub const fn format(&self) -> &'static str {
        match self {
            Self::YubicoPiv(_) => "yubico_piv",
            Self::MockPiv(_) => "mock_piv",
        }
    }
}

/// Checks attestation evidence against trusted roots (server side).
///
/// Each verifier takes its own evidence type, and
/// [`verify_bundle`](Self::verify_bundle) refuses every other format, so a
/// mock attestation can't reach a real verifier. `piv::
/// YubicoAttestationVerifier` takes [`PivAttestation`] and the Yubico PIV
/// attestation CAs it pins; `mock::MockAttestationVerifier` takes
/// [`MockPivAttestation`] and a mock root CA. Only this crate can make an
/// [`AttestedKey`], so only this crate's verifiers can say anything.
pub trait AttestationVerifier {
    /// The evidence this verifier reads.
    type Evidence;

    /// This verifier's evidence in `bundle`, or
    /// [`AttestationError::WrongFormat`].
    fn evidence(
        bundle: &AttestationBundle,
    ) -> Result<&Self::Evidence, AttestationError>;

    /// Check `evidence` and that it attests `claimed` (the credential's
    /// registered key), and say what it proves. An attestation of any other
    /// key is [`AttestationError::KeyMismatch`].
    fn verify(
        &self,
        evidence: &Self::Evidence,
        claimed: &OperatorPublicKey,
    ) -> Result<AttestedKey, AttestationError>;

    /// [`verify`](Self::verify) a bundle as it came off the wire.
    fn verify_bundle(
        &self,
        bundle: &AttestationBundle,
        claimed: &OperatorPublicKey,
    ) -> Result<AttestedKey, AttestationError> {
        self.verify(Self::evidence(bundle)?, claimed)
    }
}

/// What a checked attestation proves about a key. Only this crate's
/// verifiers make one, and it is never read from the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedKey {
    public_key: OperatorPublicKey,
    serial: DeviceSerial,
    firmware: FirmwareVersion,
    policy: KeyPolicy,
}

impl AttestedKey {
    #[cfg(feature = "x509")]
    pub(crate) fn new(
        public_key: OperatorPublicKey,
        serial: DeviceSerial,
        firmware: FirmwareVersion,
        policy: KeyPolicy,
    ) -> Self {
        Self {
            public_key,
            serial,
            firmware,
            policy,
        }
    }

    /// The key, generated on the device and not exportable.
    pub fn public_key(&self) -> &OperatorPublicKey {
        &self.public_key
    }

    /// The device's serial.
    pub fn serial(&self) -> DeviceSerial {
        self.serial
    }

    /// The device's firmware.
    pub fn firmware(&self) -> FirmwareVersion {
        self.firmware
    }

    /// The key's PIN and touch policies.
    pub fn policy(&self) -> KeyPolicy {
        self.policy
    }
}

/// Why attestation evidence was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttestationError {
    /// A certificate isn't valid DER X.509.
    #[error("certificate {index} in the chain is not valid X.509: {message}")]
    Malformed {
        /// Position: 0 is the slot certificate.
        index: usize,
        /// The parser's words.
        message: String,
    },
    /// The bundle is in a format this verifier doesn't read (a mock
    /// attestation sent to a real verifier, or the reverse).
    #[error("this verifier does not accept {format} attestations")]
    WrongFormat {
        /// The bundle's format.
        format: &'static str,
    },
    /// The device chain isn't `[f9]`, or `[f9, anchor]` with the anchor the
    /// same certificate as a trusted one (equal as parsed DER).
    #[error(
        "the device chain has {len} certificates; expected the device \
         certificate, optionally followed by its trusted issuer"
    )]
    ChainLength {
        /// The device chain's length.
        len: usize,
    },
    /// The device certificate (index 1) isn't the vendor's PIV attestation
    /// certificate: its subject, compared as DER, is not the expected one
    /// (on a YubiKey, `CN=Yubico PIV Attestation` as a UTF8String).
    #[error("certificate {index} is not a PIV device attestation certificate")]
    NotADeviceCertificate {
        /// Position of the certificate.
        index: usize,
    },
    /// A certificate that issues others is not a CA: no `basicConstraints
    /// cA=TRUE`, or a `keyUsage` without `keyCertSign`.
    #[error("certificate {index} issues certificates but is not a CA")]
    NotACa {
        /// Position of the certificate.
        index: usize,
    },
    /// The slot certificate claims to be a CA.
    #[error("certificate {index} is a slot certificate but claims to be a CA")]
    UnexpectedCa {
        /// Position of the certificate.
        index: usize,
    },
    /// A CA's `pathLenConstraint` forbids the CA certificates below it.
    /// Index 2 is the trust anchor.
    #[error("certificate {index}'s path length constraint is exceeded")]
    PathLength {
        /// Position of the certificate.
        index: usize,
    },
    /// An issuer's RSA key is over 4096 bits.
    #[error("certificate {index}'s issuer key is too large")]
    KeyTooLarge {
        /// Position of the certificate whose issuer's key it is.
        index: usize,
    },
    /// A certificate above the slot certificate carries a slot policy.
    #[error("certificate {index} is above the slot but carries a policy")]
    PolicyAboveLeaf {
        /// Position of the certificate.
        index: usize,
    },
    /// A critical extension this verifier doesn't process.
    #[error("certificate {index} has an unknown critical extension {oid}")]
    UnknownCriticalExtension {
        /// Position of the certificate.
        index: usize,
        /// The extension's OID.
        oid: String,
    },
    /// The same extension twice.
    #[error("certificate {index} has extension {oid} more than once")]
    DuplicateExtension {
        /// Position of the certificate.
        index: usize,
        /// The extension's OID.
        oid: String,
    },
    /// The signature algorithm inside the signed part differs from the one
    /// outside it, or an algorithm's parameters are wrong.
    #[error("certificate {index}'s algorithm identifiers are inconsistent")]
    AlgorithmMismatch {
        /// Position of the certificate.
        index: usize,
    },
    /// A trust anchor was refused: not a CA, or (for the Yubico verifier)
    /// not one of the pinned Yubico PIV CAs.
    #[error("refused root: {reason}")]
    RootRefused {
        /// Why.
        reason: &'static str,
    },
    /// A certificate's issuer isn't the next one's subject.
    #[error("certificate {index} was not issued by the certificate after it")]
    IssuerMismatch {
        /// Position of the certificate whose issuer didn't match.
        index: usize,
    },
    /// A certificate's signature doesn't verify under its issuer's key.
    #[error("certificate {index}'s signature does not verify")]
    BadSignature {
        /// Position of the certificate.
        index: usize,
    },
    /// No trusted root issued the top of the chain.
    #[error("the chain does not end at a trusted root")]
    UntrustedRoot,
    /// A signature or key algorithm this build can't check.
    #[error("unsupported algorithm {oid} in certificate {index}")]
    UnsupportedAlgorithm {
        /// Position of the certificate.
        index: usize,
        /// The algorithm's OID.
        oid: String,
    },
    /// A required attestation extension is absent.
    #[error("the slot certificate has no {name} extension")]
    MissingExtension {
        /// Which (`serial`, `firmware`, `policy`).
        name: &'static str,
    },
    /// An attestation extension has a value this build doesn't know.
    #[error("the slot certificate's {name} extension is malformed")]
    BadExtension {
        /// Which (`serial`, `firmware`, `policy`).
        name: &'static str,
    },
    /// The attested key isn't the credential's key.
    #[error("the attestation is for a different key")]
    KeyMismatch,
}

// ---------------------------------------------------------------------------
// Signers
// ---------------------------------------------------------------------------

/// Software or hardware, and for hardware, what only hardware has. The
/// credential kind it registers as is
/// [`credential_kind`](Self::credential_kind).
#[derive(Clone, Copy)]
pub enum SignerKind<'a> {
    /// A key held in memory, loaded from a file
    /// ([`OperatorCredentialKind::SoftwareKey`]).
    Software,
    /// A key on a device that won't export it
    /// ([`OperatorCredentialKind::HardwareKey`]).
    Hardware(&'a dyn HardwareKey),
}

impl SignerKind<'_> {
    /// The credential kind this signer registers as.
    pub const fn credential_kind(&self) -> OperatorCredentialKind {
        match self {
            Self::Software => OperatorCredentialKind::SoftwareKey,
            Self::Hardware(_) => OperatorCredentialKind::HardwareKey,
        }
    }
}

impl fmt::Debug for SignerKind<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Software => f.write_str("Software"),
            Self::Hardware(key) => f
                .debug_struct("Hardware")
                .field("serial", &key.serial())
                .field("policy", &key.policy())
                .finish(),
        }
    }
}

/// Something that signs as an operator credential.
///
/// Implemented by [`SoftwareKeySigner`], `mock::MockHardwareKey`,
/// `yubikey::YubikeySigner` and, later, other vendors' devices. No method
/// returns a private key.
#[async_trait::async_trait]
pub trait OperatorSigner: Send + Sync {
    /// The credential's public key, as registered.
    fn public_key(&self) -> OperatorPublicKey;

    /// The signature algorithm.
    fn algorithm(&self) -> SignatureAlgorithm {
        self.public_key().algorithm()
    }

    /// Software or hardware.
    fn kind(&self) -> SignerKind<'_>;

    /// The credential kind this signer registers as.
    fn credential_kind(&self) -> OperatorCredentialKind {
        self.kind().credential_kind()
    }

    /// Sign exactly `message` (module docs: what each algorithm signs).
    ///
    /// A hardware key may wait here for a touch, and fails with
    /// [`SignerError::TouchRequired`] if none comes,
    /// [`SignerError::PinRequired`] if its PIN policy wants a PIN not yet
    /// presented, or [`SignerError::DeviceAbsent`].
    async fn sign(
        &self,
        message: &[u8],
    ) -> Result<OperatorSignature, SignerError>;
}

/// What a hardware key has beyond signing.
///
/// The getters report what the device said when it was opened. They are
/// the device's own claims, for display and preflight; the server trusts
/// only an [`AttestedKey`].
#[async_trait::async_trait]
pub trait HardwareKey: Send + Sync {
    /// The key's PIN and touch policies.
    fn policy(&self) -> KeyPolicy;

    /// The device's serial.
    fn serial(&self) -> DeviceSerial;

    /// The device's firmware.
    fn firmware(&self) -> FirmwareVersion;

    /// Present the PIN. Under [`PinPolicy::Once`] it holds until the device
    /// is removed; under [`PinPolicy::Always`] it covers the next
    /// signature only, so present it just before signing.
    async fn verify_pin(&self, pin: &Pin) -> Result<(), SignerError>;

    /// The device's attestation of this key. Needs neither PIN nor touch.
    async fn attestation(&self) -> Result<AttestationBundle, SignerError>;
}

/// Why a signer couldn't sign (or open, or attest).
#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    /// The device isn't connected (unplugged, or not found).
    #[error("the hardware key is not connected")]
    DeviceAbsent,
    /// The key's PIN policy needs the PIN and it hasn't been presented
    /// (for [`PinPolicy::Always`], immediately before this signature).
    #[error("the hardware key needs its PIN first")]
    PinRequired,
    /// The PIN was wrong.
    #[error("wrong PIN ({retries_left} tries left before the PIN locks)")]
    WrongPin {
        /// Tries left before the PIN locks.
        retries_left: u8,
    },
    /// Too many wrong PINs: the PIN is locked until reset with the PUK.
    #[error("the hardware key's PIN is locked")]
    PinBlocked,
    /// The key's touch policy needs a touch and none came in time.
    #[error("the hardware key needs a touch, and none came")]
    TouchRequired,
    /// The slot holds a different key now (it was regenerated or
    /// overwritten); this signer's credential can no longer sign.
    #[error("the key in this slot has changed since it was opened")]
    KeyChanged,
    /// The device, or this build, can't use the algorithm.
    #[error("{algorithm} is not supported here: {reason}")]
    UnsupportedAlgorithm {
        /// The algorithm.
        algorithm: SignatureAlgorithm,
        /// Why.
        reason: &'static str,
    },
    /// A software key's file couldn't be read.
    #[error("could not read the key file {path}: {source}")]
    KeyFile {
        /// The file.
        path: std::path::PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// A software key's file isn't a key. Says nothing of the file's
    /// contents: a parser's "invalid character 'x' at position 7" would
    /// leak the key into logs.
    #[error("the key file is not an Ed25519 key in hex (64 hex digits)")]
    KeyFormat,
    /// The smart-card service (PC/SC: `pcscd` on Linux) can't be reached,
    /// or refused this user. Nothing was sent to a device.
    #[error(
        "the smart-card service (pcscd) is not reachable or refused this user: \
         {reason}. Is pcscd running, and may this user use it (polkit's \
         org.debian.pcsc-lite.access_pcsc and access_card)?"
    )]
    PcscUnavailable {
        /// The PC/SC error.
        reason: String,
    },
    /// The device has no key in the slot.
    #[error("the hardware key has no key in slot {slot}")]
    SlotEmpty {
        /// The slot.
        slot: PivSlot,
    },
    /// The key in the slot was imported, not generated on the device, so
    /// nothing can attest it: it may exist elsewhere.
    #[error("the key in slot {slot} was imported, not generated on the device")]
    ImportedKey {
        /// The slot.
        slot: PivSlot,
    },
    /// The device refused the management key (needed to generate a key or
    /// change the device's PIV settings).
    #[error("the hardware key refused the management key")]
    ManagementKeyRefused,
    /// More than one device is connected and none was named.
    #[error("more than one hardware key is connected: name one by its serial")]
    SeveralDevices,
    /// The device, or the transport to it, failed in a way none of the
    /// variants above names (the driver's code and words).
    #[error("the hardware key failed: {error}")]
    Transport {
        /// What the driver said.
        error: String,
    },
}

/// Why a signature didn't verify.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerifyError {
    /// The signature doesn't verify.
    #[error("the signature does not verify")]
    BadSignature,
    /// The key and the signature are of different algorithms.
    #[error("a {signature} signature can't be checked with a {key} key")]
    AlgorithmMismatch {
        /// The key's.
        key: SignatureAlgorithm,
        /// The signature's.
        signature: SignatureAlgorithm,
    },
    /// This build can't verify the algorithm.
    #[error("{algorithm} verification needs the `{feature}` feature")]
    NotBuiltIn {
        /// The algorithm.
        algorithm: SignatureAlgorithm,
        /// The feature that adds it.
        feature: &'static str,
    },
}

/// Why a public key was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    /// The wrong number of bytes.
    #[error("expected {expected} bytes, got {got}")]
    Length {
        /// Expected.
        expected: usize,
        /// Got.
        got: usize,
    },
    /// Not a point on P-256.
    #[error("not a valid P-256 public key")]
    InvalidP256Point,
    /// Not an Ed25519 public key.
    #[error("not a valid Ed25519 public key")]
    InvalidEd25519Point,
    /// Not hexadecimal.
    #[error("not hexadecimal")]
    NotHex,
    /// A raw public key of neither algorithm's length.
    #[error(
        "a public key is 32 bytes (Ed25519) or 65 (P-256, uncompressed), \
         not {got}"
    )]
    KeyLength {
        /// Its length.
        got: usize,
    },
    /// An ECDSA signature with a high (or zero) `s`.
    #[error("not a canonical (low-s) P-256 signature")]
    NonCanonicalSignature,
}

/// Sign a request's canonical bytes at `timestamp`, as every signed body is
/// signed: the signer signs [`crypto::signing_digest`]`(payload,
/// timestamp)`. For an operator API request, `payload` is
/// [`operator_request_bytes`](crate::signing::operator_request::operator_request_bytes).
pub async fn sign_request(
    signer: &(impl OperatorSigner + ?Sized),
    payload: &[u8],
    timestamp: i64,
) -> Result<OperatorSignature, SignerError> {
    signer
        .sign(&crypto::signing_digest(payload, timestamp))
        .await
}

/// Check a [`sign_request`] signature. (For Ed25519 this is
/// [`crypto::verify`].)
pub fn verify_request(
    key: &OperatorPublicKey,
    payload: &[u8],
    timestamp: i64,
    signature: &OperatorSignature,
) -> Result<(), VerifyError> {
    key.verify(&crypto::signing_digest(payload, timestamp), signature)
}

/// Prove that `signer` holds the key it is registering as `operator`'s
/// credential: a signature over exactly
/// [`credential_possession_bytes`](crate::signing::operator_request::credential_possession_bytes),
/// by software and hardware keys alike
pub async fn sign_possession(
    signer: &(impl OperatorSigner + ?Sized),
    operator: OperatorId,
) -> Result<OperatorSignature, SignerError> {
    let bytes = crate::signing::operator_request::credential_possession_bytes(
        operator,
        &signer.public_key(),
    );
    signer.sign(&bytes).await
}

/// Check a [`sign_possession`] proof for `key`, registered as `operator`'s
pub fn verify_possession(
    key: &OperatorPublicKey,
    operator: OperatorId,
    signature: &OperatorSignature,
) -> Result<(), VerifyError> {
    key.verify(
        &crate::signing::operator_request::credential_possession_bytes(
            operator, key,
        ),
        signature,
    )
}

/// Check a device-flow enrolment's proof of possession: `proof`, by `key`
/// itself, over exactly
/// [`enrolment_possession_bytes`](crate::signing::operator_request::enrolment_possession_bytes)
/// (0.140). The ±300 s window and the id's uniqueness are the server's
pub fn verify_enrolment(
    key: &OperatorPublicKey,
    host: &str,
    enrolment: crate::ids::OperatorEnrolmentId,
    timestamp: i64,
    proof: &OperatorSignature,
) -> Result<(), VerifyError> {
    let bytes = crate::signing::operator_request::enrolment_possession_bytes(
        host, enrolment, timestamp,
    )
    .map_err(|_| VerifyError::BadSignature)?;
    key.verify(&bytes, proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_enrolment_proof_verifies_with_its_own_key_only() {
        let (secret, public) = crypto::generate_keypair();
        let key = OperatorPublicKey::Ed25519(public);
        let id = crate::ids::OperatorEnrolmentId::new();
        let bytes =
            crate::signing::operator_request::enrolment_possession_bytes(
                "a.org", id, 7,
            )
            .unwrap();
        use ed25519_dalek::Signer;
        let proof = OperatorSignature::Ed25519(secret.sign(&bytes));
        assert_eq!(verify_enrolment(&key, "a.org", id, 7, &proof), Ok(()));
        assert!(verify_enrolment(&key, "a.org", id, 8, &proof).is_err());
        assert!(verify_enrolment(&key, "b.org", id, 7, &proof).is_err());
        // The wire's forms round-trip
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, format!("\"{}\"", key.to_hex()));
        assert_eq!(
            serde_json::from_str::<OperatorPublicKey>(&json).unwrap(),
            key
        );
        let wire = SignatureBytes::from(proof);
        let back: SignatureBytes =
            serde_json::from_str(&serde_json::to_string(&wire).unwrap())
                .unwrap();
        assert_eq!(back.read_as(SignatureAlgorithm::Ed25519).unwrap(), proof);
        assert!(serde_json::from_str::<OperatorPublicKey>("\"abcd\"").is_err());
    }

    #[test]
    fn pin_is_redacted() {
        let pin = Pin::new("123456");
        assert_eq!(format!("{pin:?}"), "[REDACTED]");
        assert_eq!(format!("{pin}"), "[REDACTED]");
    }

    #[test]
    fn touch_policy_tiers() {
        assert!(TouchPolicy::Never.signs_unattended());
        assert!(!TouchPolicy::Always.signs_unattended());
        assert!(!TouchPolicy::Cached.signs_unattended());
        assert!(TouchPolicy::Always.proves_presence_per_signature());
        assert!(!TouchPolicy::Cached.proves_presence_per_signature());
        assert!(!TouchPolicy::Never.proves_presence_per_signature());
    }

    #[test]
    fn firmware_orders_and_gates_ed25519() {
        assert!(FirmwareVersion::new(5, 4, 3) < YUBIKEY_ED25519_MIN_FIRMWARE);
        assert!(FirmwareVersion::new(5, 7, 1) >= YUBIKEY_ED25519_MIN_FIRMWARE);
        assert_eq!(YUBIKEY_ED25519_MIN_FIRMWARE.to_string(), "5.7.0");
    }

    #[test]
    fn mismatched_algorithms_refuse() {
        let (_, vk) = crypto::generate_keypair();
        let key = OperatorPublicKey::Ed25519(vk);
        let mut bytes = [0; 64];
        bytes[63] = 1;
        let sig = OperatorSignature::EcdsaP256(
            EcdsaP256Signature::from_bytes(bytes).unwrap(),
        );
        assert_eq!(
            key.verify(b"x", &sig),
            Err(VerifyError::AlgorithmMismatch {
                key: SignatureAlgorithm::Ed25519,
                signature: SignatureAlgorithm::EcdsaP256,
            })
        );
    }

    #[test]
    fn bundles_round_trip_with_their_format_tags() {
        let piv = PivAttestation {
            slot_certificate: DerCertificate::from_der(vec![0x30, 0x00]),
            device_chain: vec![DerCertificate::from_der(vec![0x30, 0x01])],
        };
        let yubico = AttestationBundle::YubicoPiv(piv.clone());
        let json = serde_json::to_value(&yubico).unwrap();
        assert_eq!(json["format"], "yubico_piv");
        assert_eq!(json["slot_certificate"], "3000");
        assert_eq!(
            serde_json::from_value::<AttestationBundle>(json).unwrap(),
            yubico
        );

        let mock = AttestationBundle::MockPiv(MockPivAttestation {
            slot_certificate: piv.slot_certificate,
            device_chain: piv.device_chain,
        });
        let json = serde_json::to_value(&mock).unwrap();
        assert_eq!(json["format"], "mock_piv");
        assert_eq!(mock.format(), "mock_piv");
        assert_eq!(
            serde_json::from_value::<AttestationBundle>(json).unwrap(),
            mock
        );
    }

    #[test]
    fn high_or_zero_s_is_refused() {
        let mut bytes = [0u8; 64];
        bytes[0] = 1;
        assert_eq!(
            EcdsaP256Signature::from_bytes(bytes),
            Err(KeyError::NonCanonicalSignature)
        );
        bytes[32..].copy_from_slice(&P256_HALF_ORDER);
        assert!(EcdsaP256Signature::from_bytes(bytes).is_ok());
        bytes[63] += 1;
        assert_eq!(
            EcdsaP256Signature::from_bytes(bytes),
            Err(KeyError::NonCanonicalSignature)
        );
    }

    #[test]
    fn raw_keys_name_their_algorithm_by_length() {
        let (_, vk) = crypto::generate_keypair();
        let ed = OperatorPublicKey::Ed25519(vk);
        assert_eq!(OperatorPublicKey::from_raw(&ed.to_raw()), Ok(ed));
        assert_eq!(hex::encode(ed.to_raw()), ed.to_hex());
        assert_eq!(
            OperatorPublicKey::from_raw(&[0; 33]),
            Err(KeyError::KeyLength { got: 33 })
        );
        assert_eq!(
            OperatorPublicKey::from_raw(&[0x05; 65]),
            Err(KeyError::InvalidP256Point)
        );
        #[cfg(feature = "p256")]
        {
            let key = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
            let p = OperatorPublicKey::EcdsaP256(
                EcdsaP256PublicKey::from_p256(key.verifying_key()),
            );
            assert_eq!(p.to_raw().len(), 65);
            assert_eq!(OperatorPublicKey::from_raw(&p.to_raw()), Ok(p));
        }
    }

    #[test]
    fn signatures_round_trip_through_hex_and_refuse_high_s() {
        let (key, vk) = crypto::generate_keypair();
        let signature = OperatorSignature::Ed25519(crypto::sign(&key, b"m", 1));
        assert_eq!(
            OperatorSignature::from_hex(
                SignatureAlgorithm::Ed25519,
                &format!(" {} ", signature.to_hex())
            ),
            Ok(signature)
        );
        assert!(
            OperatorPublicKey::Ed25519(vk)
                .verify(&crypto::signing_digest(b"m", 1), &signature)
                .is_ok()
        );
        let mut high = [0xffu8; 64];
        high[0] = 1;
        assert_eq!(
            OperatorSignature::from_bytes(SignatureAlgorithm::EcdsaP256, &high),
            Err(KeyError::NonCanonicalSignature)
        );
        assert_eq!(
            OperatorSignature::from_hex(SignatureAlgorithm::Ed25519, "zz"),
            Err(KeyError::NotHex)
        );
        assert_eq!(
            OperatorSignature::from_bytes(
                SignatureAlgorithm::Ed25519,
                &[0; 63]
            ),
            Err(KeyError::Length {
                expected: 64,
                got: 63
            })
        );
    }

    #[test]
    fn mock_flag_matches_the_feature() {
        assert_eq!(MOCK_HARDWARE_KEY, cfg!(feature = "mock-hardware-key"));
    }

    #[test]
    fn serial_hmac_inputs_are_namespaced() {
        assert_eq!(
            DeviceSerial::Yubico(PivSerial::new(12345678)).hmac_input(),
            b"yubico\n12345678"
        );
    }
}
