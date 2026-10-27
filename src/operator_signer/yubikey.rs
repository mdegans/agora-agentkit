//! A key in a YubiKey's PIV slot, as an operator credential (feature
//! `yubikey`): [`YubikeySigner`], and the device calls an operator's CLI
//! needs to set one up ([`generate`], [`device_info`], [`reset_piv`]).
//!
//! Over PC/SC (`pcscd` on Linux) through the `yubikey` crate. Every device
//! call blocks, for up to the ~15 s a touch-always key waits for its touch,
//! so each runs in [`tokio::task::spawn_blocking`]; nothing here blocks an
//! executor thread.
//!
//! **What gets signed** (as [`OperatorSigner::sign`] promises):
//! - P-256: the message is hashed with SHA-256 here, the card signs the
//!   digest (PIV's ECDSA takes a digest), and its DER signature becomes
//!   `r ‖ s` with `s` normalized low;
//! - Ed25519 (firmware 5.7 and later only): the card signs the message
//!   itself, pure Ed25519.
//!
//! Every signature is checked against the slot's public key before it is
//! returned, so a slot regenerated behind this signer's back reads as
//! [`SignerError::KeyChanged`] rather than as a signature the server
//! refuses.
//!
//! **The PIN** comes from a [`Pin`], which the caller reads from a file
//! ([`Pin::from_secret`] over [`crate::secrets::Secret::from_file`]), never
//! from the environment or the command line. A signer opened with one
//! ([`YubikeySigner::open_with_pin`]) keeps it, zeroized on drop, so that
//! when the card forgets its PIN session (it was reset, or another program
//! selected another applet) it can present it again once, and so that a
//! PIN-always key gets it immediately before each signature. A wrong PIN is
//! never retried.
//!
//! **Errors** are mapped onto [`SignerError`] by what they mean to the
//! caller: no PC/SC service ([`SignerError::PcscUnavailable`]), no device
//! ([`SignerError::DeviceAbsent`]), PIN states, touch, and the rest as
//! [`SignerError::Transport`] with the driver's own words. The driver's
//! error type is `#[non_exhaustive]`, so its future variants land there
//! too; every variant it has today is named below.

use std::sync::{Arc, Mutex, MutexGuard};

use ::yubikey::piv::{self as ykpiv, AlgorithmId, ManagementSlotId, SlotId};
use ::yubikey::{MgmKey, Serial, YubiKey};
use sha2::{Digest, Sha256};
use x509_cert::der::Encode;

use super::{
    AttestationBundle, DerCertificate, DeviceSerial, EcdsaP256Signature,
    FirmwareVersion, HardwareKey, KeyPolicy, OperatorPublicKey,
    OperatorSignature, OperatorSigner, Pin, PinPolicy, PivAttestation,
    PivSerial, PivSlot, SignatureAlgorithm, SignerError, SignerKind,
    TouchPolicy, YUBIKEY_ED25519_MIN_FIRMWARE, piv,
};
use crate::secrets::Secret;

/// The first firmware with PIV's metadata command (slot algorithm, origin
/// and policy, and whether the PIN is still the default).
const METADATA_MIN_FIRMWARE: FirmwareVersion = FirmwareVersion::new(5, 3, 0);

/// How many wrong PINs or PUKs [`reset_piv`] spends at most to block each:
/// the device's counters are at most 255, and a fresh device's are 3.
const MAX_BLOCKING_TRIES: usize = 256;

// ---------------------------------------------------------------------------
// The signer
// ---------------------------------------------------------------------------

/// A key in one PIV slot of one YubiKey: an [`OperatorSigner`] and a
/// [`HardwareKey`].
///
/// Opening reads the slot's attestation certificate for its public key,
/// policies, serial and firmware: the device's own claims, for display and
/// preflight. The server trusts only the attestation's chain
/// ([`piv::YubicoAttestationVerifier`]).
pub struct YubikeySigner {
    device: Arc<Mutex<YubiKey>>,
    slot: PivSlot,
    public_key: OperatorPublicKey,
    policy: KeyPolicy,
    serial: PivSerial,
    firmware: FirmwareVersion,
    pin: Option<Pin>,
}

impl std::fmt::Debug for YubikeySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YubikeySigner")
            .field("serial", &self.serial)
            .field("slot", &self.slot)
            .field("public_key", &self.public_key)
            .field("policy", &self.policy)
            .field("firmware", &self.firmware)
            .field("pin", &self.pin.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl YubikeySigner {
    /// Open the key in `slot` of the YubiKey with `serial`, or of the only
    /// one connected. No PIN is presented: a key whose policy wants one
    /// answers [`SignerError::PinRequired`] until
    /// [`verify_pin`](HardwareKey::verify_pin).
    pub async fn open(
        serial: Option<PivSerial>,
        slot: PivSlot,
    ) -> Result<Self, SignerError> {
        blocking(move || {
            let device = open_device(serial)?;
            Self::from_device(device, slot)
        })
        .await
    }

    /// [`open`](Self::open), then present `pin` if the key's policy asks
    /// for one, and keep it for presenting again (module docs).
    pub async fn open_with_pin(
        serial: Option<PivSerial>,
        slot: PivSlot,
        pin: Pin,
    ) -> Result<Self, SignerError> {
        let mut signer = Self::open(serial, slot).await?;
        if signer.policy.pin != PinPolicy::Never {
            signer.verify_pin(&pin).await?;
        }
        signer.pin = Some(pin);
        Ok(signer)
    }

    /// Read the slot's key and claims from an open device.
    fn from_device(
        mut device: YubiKey,
        slot: PivSlot,
    ) -> Result<Self, SignerError> {
        let firmware = firmware_of(&device);
        let serial = PivSerial::new(device.serial().0);
        let metadata_key = slot_metadata(&mut device, slot, firmware)?;
        let slot_certificate = attest(&mut device, slot)?;
        let facts = piv::unverified_slot_facts(&slot_certificate).map_err(
            |e| SignerError::Transport {
                error: format!(
                    "the device's attestation of slot {slot} is unreadable: \
                         {e}"
                ),
            },
        )?;
        if facts.serial != serial || facts.firmware != firmware {
            return Err(SignerError::Transport {
                error: format!(
                    "the attestation of slot {slot} names device {} on firmware \
                     {}, but this is device {serial} on {firmware}",
                    facts.serial, facts.firmware
                ),
            });
        }
        if metadata_key.is_some_and(|key| key != facts.public_key) {
            return Err(SignerError::KeyChanged);
        }
        if facts.public_key.algorithm() == SignatureAlgorithm::Ed25519
            && firmware < YUBIKEY_ED25519_MIN_FIRMWARE
        {
            return Err(SignerError::UnsupportedAlgorithm {
                algorithm: SignatureAlgorithm::Ed25519,
                reason: "Ed25519 in PIV needs YubiKey firmware 5.7 or later",
            });
        }
        Ok(Self {
            device: Arc::new(Mutex::new(device)),
            slot,
            public_key: facts.public_key,
            policy: facts.policy,
            serial,
            firmware,
            pin: None,
        })
    }

    /// The slot.
    pub fn slot(&self) -> PivSlot {
        self.slot
    }

    /// The device's serial number (its own claim; see the type's docs).
    pub fn device_serial(&self) -> PivSerial {
        self.serial
    }
}

#[async_trait::async_trait]
impl OperatorSigner for YubikeySigner {
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
        let device = Arc::clone(&self.device);
        let slot = self.slot;
        let public_key = self.public_key;
        let policy = self.policy;
        let pin = self.pin.clone();
        let message = message.to_vec();
        blocking(move || {
            let mut device = lock(&device);
            // A PIN-always key takes the PIN immediately before each
            // signature
            if policy.pin == PinPolicy::Always
                && let Some(pin) = &pin
            {
                verify_pin_on(&mut device, pin)?;
            }
            match sign_on(&mut device, slot, &public_key, policy, &message) {
                // The card forgot its PIN session: present the PIN once
                // more, and only once
                Err(SignerError::PinRequired) if pin.is_some() => {
                    let pin = pin.as_ref().expect("checked");
                    verify_pin_on(&mut device, pin)?;
                    sign_on(&mut device, slot, &public_key, policy, &message)
                }
                other => other,
            }
        })
        .await
    }
}

#[async_trait::async_trait]
impl HardwareKey for YubikeySigner {
    fn policy(&self) -> KeyPolicy {
        self.policy
    }

    fn serial(&self) -> DeviceSerial {
        DeviceSerial::Yubico(self.serial)
    }

    fn firmware(&self) -> FirmwareVersion {
        self.firmware
    }

    async fn verify_pin(&self, pin: &Pin) -> Result<(), SignerError> {
        let device = Arc::clone(&self.device);
        let pin = pin.clone();
        blocking(move || verify_pin_on(&mut lock(&device), &pin)).await
    }

    async fn attestation(&self) -> Result<AttestationBundle, SignerError> {
        let device = Arc::clone(&self.device);
        let slot = self.slot;
        let public_key = self.public_key;
        blocking(move || {
            let mut device = lock(&device);
            let slot_certificate = attest(&mut device, slot)?;
            // The slot must still hold this signer's key
            let facts =
                piv::unverified_slot_facts(&slot_certificate).map_err(|e| {
                    SignerError::Transport {
                        error: format!("the attestation is unreadable: {e}"),
                    }
                })?;
            if facts.public_key != public_key {
                return Err(SignerError::KeyChanged);
            }
            let f9 = ::yubikey::certificate::Certificate::read(
                &mut device,
                SlotId::Attestation,
            )
            .map_err(card_error)?;
            let f9 = f9.cert.to_der().map_err(|e| SignerError::Transport {
                error: format!("the device certificate (f9) won't encode: {e}"),
            })?;
            Ok(AttestationBundle::YubicoPiv(PivAttestation {
                slot_certificate,
                device_chain: vec![DerCertificate::from_der(f9)],
            }))
        })
        .await
    }
}

/// Sign `message` with the key in `slot` (module docs: what is signed),
/// and check the signature against `public_key`.
fn sign_on(
    device: &mut YubiKey,
    slot: PivSlot,
    public_key: &OperatorPublicKey,
    policy: KeyPolicy,
    message: &[u8],
) -> Result<OperatorSignature, SignerError> {
    let sign_error = |e| match e {
        // The card answers a touch that never came with a bare failure
        // (conditions not satisfied), which the driver reports as generic
        ::yubikey::Error::GenericError
            if policy.touch != TouchPolicy::Never =>
        {
            SignerError::TouchRequired
        }
        e => card_error(e),
    };
    let signature = match public_key {
        OperatorPublicKey::EcdsaP256(_) => {
            let digest = Sha256::digest(message);
            let der = ykpiv::sign_data(
                device,
                &digest,
                AlgorithmId::EccP256,
                slot_id(slot),
            )
            .map_err(sign_error)?;
            let r_s = piv::p256_signature_from_der(&der).ok_or_else(|| {
                SignerError::Transport {
                    error: "the card's P-256 signature is not DER".into(),
                }
            })?;
            low_s_p256(r_s)?
        }
        OperatorPublicKey::Ed25519(_) => {
            let raw = ykpiv::sign_data(
                device,
                message,
                AlgorithmId::Ed25519,
                slot_id(slot),
            )
            .map_err(sign_error)?;
            let raw: [u8; 64] = raw.as_slice().try_into().map_err(|_| {
                SignerError::Transport {
                    error: format!(
                        "the card's Ed25519 signature is {} bytes, not 64",
                        raw.len()
                    ),
                }
            })?;
            OperatorSignature::Ed25519(crate::crypto::Signature::from_bytes(
                &raw,
            ))
        }
    };
    public_key
        .verify(message, &signature)
        .map_err(|_| SignerError::KeyChanged)?;
    Ok(signature)
}

/// `r ‖ s` with `s` normalized low: the card's ECDSA may return either of
/// the pair, and only the low one is canonical.
fn low_s_p256(r_s: [u8; 64]) -> Result<OperatorSignature, SignerError> {
    let signature = p256::ecdsa::Signature::from_slice(&r_s).map_err(|_| {
        SignerError::Transport {
            error: "the card's P-256 signature is out of range".into(),
        }
    })?;
    Ok(OperatorSignature::EcdsaP256(EcdsaP256Signature::from_p256(
        &signature,
    )))
}

fn verify_pin_on(device: &mut YubiKey, pin: &Pin) -> Result<(), SignerError> {
    device
        .verify_pin(pin.expose().as_bytes())
        .map_err(card_error)
}

// ---------------------------------------------------------------------------
// Provisioning
// ---------------------------------------------------------------------------

/// The management key that authorizes generating a key (PIV's admin
/// credential, distinct from the PIN).
pub enum ManagementKey {
    /// The factory default, as a new or reset device has.
    Default,
    /// This key, as hex (ykman's format), from a secret file.
    Hex(Secret),
    /// The PIN-protected one (ykman's `--protect`), unlocked by the PIN.
    Protected(Pin),
}

impl std::fmt::Debug for ManagementKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Default => "ManagementKey::Default",
            Self::Hex(_) => "ManagementKey::Hex([REDACTED])",
            Self::Protected(_) => "ManagementKey::Protected([REDACTED])",
        })
    }
}

/// Generate a new key in `slot` of the YubiKey with `serial` (or the only
/// one connected), with `policy` fixed into it, and open a signer on it.
///
/// **Destroys any key already in the slot**, irrecoverably; a credential
/// registered with it stops working. The caller confirms that first.
/// Ed25519 needs firmware 5.7 or later.
pub async fn generate(
    serial: Option<PivSerial>,
    slot: PivSlot,
    algorithm: SignatureAlgorithm,
    policy: KeyPolicy,
    management_key: ManagementKey,
) -> Result<YubikeySigner, SignerError> {
    blocking(move || {
        let mut device = open_device(serial)?;
        let firmware = firmware_of(&device);
        let algorithm_id = match algorithm {
            SignatureAlgorithm::EcdsaP256 => AlgorithmId::EccP256,
            SignatureAlgorithm::Ed25519 => {
                if firmware < YUBIKEY_ED25519_MIN_FIRMWARE {
                    return Err(SignerError::UnsupportedAlgorithm {
                        algorithm,
                        reason: "Ed25519 in PIV needs YubiKey firmware 5.7 or \
                                 later",
                    });
                }
                AlgorithmId::Ed25519
            }
        };
        let key = match management_key {
            ManagementKey::Default => MgmKey::get_default(&device),
            ManagementKey::Hex(secret) => {
                let bytes = zeroize::Zeroizing::new(
                    hex::decode(secret.expose().trim())
                        .map_err(|_| SignerError::ManagementKeyRefused)?,
                );
                MgmKey::from_bytes(bytes.as_slice(), None)
            }
            ManagementKey::Protected(pin) => {
                verify_pin_on(&mut device, &pin)?;
                MgmKey::get_protected(&mut device)
            }
        }
        .map_err(|_| SignerError::ManagementKeyRefused)?;
        device.authenticate(&key).map_err(|e| match e {
            ::yubikey::Error::AuthenticationError => {
                SignerError::ManagementKeyRefused
            }
            e => card_error(e),
        })?;
        ykpiv::generate(
            &mut device,
            slot_id(slot),
            algorithm_id,
            pin_policy(policy.pin),
            touch_policy(policy.touch),
        )
        .map_err(|e| match e {
            ::yubikey::Error::AuthenticationError => {
                SignerError::ManagementKeyRefused
            }
            ::yubikey::Error::AlgorithmError => {
                SignerError::UnsupportedAlgorithm {
                    algorithm,
                    reason: "the device refused the algorithm or a policy",
                }
            }
            e => card_error(e),
        })?;
        let signer = YubikeySigner::from_device(device, slot)?;
        if signer.policy != policy {
            return Err(SignerError::Transport {
                error: format!(
                    "the new key's attested policy is {:?}, not the {:?} asked \
                     for",
                    signer.policy, policy
                ),
            });
        }
        Ok(signer)
    })
    .await
}

/// What a slot holds, as the device reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInfo {
    /// The slot.
    pub slot: PivSlot,
    /// Its key, if any, with what its attestation claims.
    pub key: Option<piv::SlotFacts>,
    /// Whether the key was generated on the device (`None` if the
    /// firmware can't say, before 5.3).
    pub generated: Option<bool>,
}

/// A YubiKey's PIV state, as it reports it (no PIN needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Its serial number.
    pub serial: PivSerial,
    /// Its firmware.
    pub firmware: FirmwareVersion,
    /// PIN tries left.
    pub pin_tries_left: u8,
    /// Whether the PIN, PUK and management key are still the factory
    /// defaults (`None` before firmware 5.3, which can't say).
    pub pin_is_default: Option<bool>,
    /// Whether the PUK is the factory default.
    pub puk_is_default: Option<bool>,
    /// Whether the management key is the factory default.
    pub management_key_is_default: Option<bool>,
    /// The four key slots.
    pub slots: Vec<SlotInfo>,
}

/// Read the YubiKey with `serial` (or the only one connected): serial,
/// firmware, PIN state, and each key slot's key and policies.
pub async fn device_info(
    serial: Option<PivSerial>,
) -> Result<DeviceInfo, SignerError> {
    blocking(move || {
        let mut device = open_device(serial)?;
        let firmware = firmware_of(&device);
        let pin_tries_left = device.get_pin_retries().map_err(card_error)?;
        let is_default = |device: &mut YubiKey, slot: ManagementSlotId| {
            if firmware < METADATA_MIN_FIRMWARE {
                return Ok(None);
            }
            ykpiv::metadata(device, SlotId::Management(slot))
                .map(|m| m.default)
                .map_err(card_error)
        };
        let pin_is_default = is_default(&mut device, ManagementSlotId::Pin)?;
        let puk_is_default = is_default(&mut device, ManagementSlotId::Puk)?;
        let management_key_is_default =
            is_default(&mut device, ManagementSlotId::Management)?;
        let mut slots = Vec::new();
        for slot in PivSlot::ALL {
            let generated = if firmware < METADATA_MIN_FIRMWARE {
                None
            } else {
                match ykpiv::metadata(&mut device, slot_id(slot)) {
                    Ok(m) => m.origin.map(|o| o == ykpiv::Origin::Generated),
                    Err(::yubikey::Error::NotFound) => {
                        slots.push(SlotInfo {
                            slot,
                            key: None,
                            generated: None,
                        });
                        continue;
                    }
                    Err(e) => return Err(card_error(e)),
                }
            };
            // An imported key can't be attested; an empty slot (old
            // firmware) answers the attestation with an error
            let key = match attest(&mut device, slot) {
                Ok(certificate) => {
                    piv::unverified_slot_facts(&certificate).ok()
                }
                Err(_) => None,
            };
            slots.push(SlotInfo {
                slot,
                key,
                generated,
            });
        }
        Ok(DeviceInfo {
            serial: PivSerial::new(device.serial().0),
            firmware,
            pin_tries_left,
            pin_is_default,
            puk_is_default,
            management_key_is_default,
            slots,
        })
    })
    .await
}

/// Reset the PIV application of the YubiKey with `serial`, and nothing
/// else on it (FIDO, OpenPGP and OTP are untouched): every PIV key and
/// certificate is destroyed, and the PIN, PUK and management key return to
/// their factory defaults (123456, 12345678, and Yubico's default key).
///
/// PIV allows a reset only once the PIN and the PUK are both blocked, so
/// this blocks them first by presenting wrong ones. **Irreversible**; a
/// serial is required, never "the one connected", and the caller confirms
/// first.
pub async fn reset_piv(serial: PivSerial) -> Result<(), SignerError> {
    blocking(move || {
        let mut device = open_device(Some(serial))?;
        // Block the PIN: wrong PINs until none are left. A candidate that
        // happens to be the PIN resets the counter; the next one is used.
        let mut blocked = false;
        for attempt in 0..MAX_BLOCKING_TRIES {
            let wrong = format!("{:08}", 99_999_999 - attempt);
            match device.verify_pin(wrong.as_bytes()) {
                Ok(()) => continue,
                Err(::yubikey::Error::WrongPin { tries: 0 })
                | Err(::yubikey::Error::PinLocked) => {
                    blocked = true;
                    break;
                }
                Err(::yubikey::Error::WrongPin { .. }) => continue,
                Err(e) => return Err(card_error(e)),
            }
        }
        if !blocked {
            return Err(SignerError::Transport {
                error: "the PIN did not block".into(),
            });
        }
        // Block the PUK the same way, through PIN unblocking
        let mut blocked = false;
        for attempt in 0..MAX_BLOCKING_TRIES {
            let wrong = format!("{:08}", 99_999_999 - attempt);
            match device.unblock_pin(wrong.as_bytes(), b"99999999") {
                Ok(()) => {
                    // That was the PUK, and the PIN is now 99999999: block
                    // the PIN again, then carry on with the PUK
                    for _ in 0..MAX_BLOCKING_TRIES {
                        match device.verify_pin(b"00000000") {
                            Err(::yubikey::Error::WrongPin { tries: 0 })
                            | Err(::yubikey::Error::PinLocked) => break,
                            Ok(()) | Err(::yubikey::Error::WrongPin { .. }) => {
                            }
                            Err(e) => return Err(card_error(e)),
                        }
                    }
                }
                Err(::yubikey::Error::WrongPin { tries: 0 })
                | Err(::yubikey::Error::PinLocked) => {
                    blocked = true;
                    break;
                }
                Err(::yubikey::Error::WrongPin { .. }) => continue,
                Err(e) => return Err(card_error(e)),
            }
        }
        if !blocked {
            return Err(SignerError::Transport {
                error: "the PUK did not block".into(),
            });
        }
        device.reset_device().map_err(card_error)
    })
    .await
}

// ---------------------------------------------------------------------------
// Device plumbing
// ---------------------------------------------------------------------------

/// Run a blocking device call off the async executor.
async fn blocking<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, SignerError> + Send + 'static,
) -> Result<T, SignerError> {
    tokio::task::spawn_blocking(call).await.map_err(|e| {
        SignerError::Transport {
            error: format!("the device call did not finish: {e}"),
        }
    })?
}

fn lock(device: &Mutex<YubiKey>) -> MutexGuard<'_, YubiKey> {
    device.lock().unwrap_or_else(|e| e.into_inner())
}

/// The YubiKey with `serial`, or the only one connected.
fn open_device(serial: Option<PivSerial>) -> Result<YubiKey, SignerError> {
    let opened = match serial {
        Some(serial) => YubiKey::open_by_serial(Serial(serial.get())),
        None => YubiKey::open(),
    };
    opened.map_err(|e| match e {
        // `open` says "several" this way, and "none" as not found
        ::yubikey::Error::PcscError { inner: None } if serial.is_none() => {
            SignerError::SeveralDevices
        }
        ::yubikey::Error::NotFound => SignerError::DeviceAbsent,
        e => card_error(e),
    })
}

fn firmware_of(device: &YubiKey) -> FirmwareVersion {
    let v = device.version();
    FirmwareVersion::new(v.major, v.minor, v.patch)
}

/// The slot's public key from the metadata command, if the firmware has it.
/// An empty slot is [`SignerError::SlotEmpty`] and an imported key
/// [`SignerError::ImportedKey`].
fn slot_metadata(
    device: &mut YubiKey,
    slot: PivSlot,
    firmware: FirmwareVersion,
) -> Result<Option<OperatorPublicKey>, SignerError> {
    if firmware < METADATA_MIN_FIRMWARE {
        return Ok(None);
    }
    let metadata =
        ykpiv::metadata(device, slot_id(slot)).map_err(|e| match e {
            ::yubikey::Error::NotFound => SignerError::SlotEmpty { slot },
            e => card_error(e),
        })?;
    if metadata.origin == Some(ykpiv::Origin::Imported) {
        return Err(SignerError::ImportedKey { slot });
    }
    Ok(metadata
        .public
        .as_ref()
        .and_then(|spki| piv::operator_key(spki).ok()))
}

/// The slot's attestation certificate, made by the device on request.
fn attest(
    device: &mut YubiKey,
    slot: PivSlot,
) -> Result<DerCertificate, SignerError> {
    ykpiv::attest(device, slot_id(slot))
        .map(|der| DerCertificate::from_der(der.to_vec()))
        .map_err(|e| match e {
            ::yubikey::Error::NotSupported => SignerError::Transport {
                error: "this device can't attest keys (PIV attestation needs \
                        firmware 4.3 or later)"
                    .into(),
            },
            e => card_error(e),
        })
}

fn slot_id(slot: PivSlot) -> SlotId {
    match slot {
        PivSlot::Authentication => SlotId::Authentication,
        PivSlot::Signature => SlotId::Signature,
        PivSlot::KeyManagement => SlotId::KeyManagement,
        PivSlot::CardAuthentication => SlotId::CardAuthentication,
    }
}

fn pin_policy(policy: PinPolicy) -> ::yubikey::PinPolicy {
    match policy {
        PinPolicy::Never => ::yubikey::PinPolicy::Never,
        PinPolicy::Once => ::yubikey::PinPolicy::Once,
        PinPolicy::Always => ::yubikey::PinPolicy::Always,
    }
}

fn touch_policy(policy: TouchPolicy) -> ::yubikey::TouchPolicy {
    match policy {
        TouchPolicy::Never => ::yubikey::TouchPolicy::Never,
        TouchPolicy::Always => ::yubikey::TouchPolicy::Always,
        TouchPolicy::Cached => ::yubikey::TouchPolicy::Cached,
    }
}

/// A driver error as a [`SignerError`], where the call site has nothing
/// more specific to say (module docs).
fn card_error(e: ::yubikey::Error) -> SignerError {
    use ::yubikey::Error as E;
    let transport = |e: E| SignerError::Transport {
        error: match e.name() {
            Some(name) => format!("{e} ({name})"),
            None => e.to_string(),
        },
    };
    match e {
        E::WrongPin { tries: 0 } | E::PinLocked => SignerError::PinBlocked,
        E::WrongPin { tries } => SignerError::WrongPin {
            retries_left: tries,
        },
        // "Security status not satisfied": the key wants its PIN
        E::AuthenticationError => SignerError::PinRequired,
        E::PcscError { inner: Some(pcsc) } => pcsc_error(pcsc),
        E::NotFound => SignerError::DeviceAbsent,
        E::PcscError { inner: None }
        | E::AlgorithmError
        | E::AppletError
        | E::AppletNotFound { .. }
        | E::ArgumentError
        | E::CertificateBuilder
        | E::GenericError
        | E::InvalidObject
        | E::KeyError
        | E::MemoryError
        | E::NotSupported
        | E::ParseError
        | E::RangeError
        | E::SizeError => transport(e),
        // `yubikey::Error` is non-exhaustive: a variant added after
        // 0.9.0-rc.0 (exact pin) is reported in its own words
        _ => transport(e),
    }
}

/// A PC/SC error: the service unreachable, the device gone, or a fault.
fn pcsc_error(e: pcsc::Error) -> SignerError {
    use pcsc::Error as P;
    match e {
        P::NoService
        | P::ServiceStopped
        | P::NoAccess
        | P::SecurityViolation
        | P::Shutdown
        | P::ServerTooBusy => SignerError::PcscUnavailable {
            reason: e.to_string(),
        },
        P::NoReadersAvailable
        | P::ReaderUnavailable
        | P::UnknownReader
        | P::NoSmartcard
        | P::RemovedCard
        | P::UnpoweredCard
        | P::UnresponsiveCard => SignerError::DeviceAbsent,
        _ => SignerError::Transport {
            error: format!("PC/SC: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_policies_map_one_to_one() {
        for slot in PivSlot::ALL {
            assert_eq!(u8::from(slot_id(slot)), slot.id());
        }
        // Yubico's codes, which the attestation's policy extension uses too
        for (policy, code) in PinPolicy::ALL.into_iter().zip([1u8, 2, 3]) {
            assert_eq!(u8::from(pin_policy(policy)), code);
        }
        for (policy, code) in TouchPolicy::ALL.into_iter().zip([1u8, 2, 3]) {
            assert_eq!(u8::from(touch_policy(policy)), code);
        }
    }

    #[test]
    fn card_errors_say_what_they_mean() {
        use ::yubikey::Error as E;
        assert!(matches!(
            card_error(E::WrongPin { tries: 2 }),
            SignerError::WrongPin { retries_left: 2 }
        ));
        assert!(matches!(
            card_error(E::WrongPin { tries: 0 }),
            SignerError::PinBlocked
        ));
        assert!(matches!(card_error(E::PinLocked), SignerError::PinBlocked));
        assert!(matches!(
            card_error(E::AuthenticationError),
            SignerError::PinRequired
        ));
        assert!(matches!(
            card_error(E::PcscError {
                inner: Some(pcsc::Error::NoService)
            }),
            SignerError::PcscUnavailable { .. }
        ));
        assert!(matches!(
            card_error(E::PcscError {
                inner: Some(pcsc::Error::RemovedCard)
            }),
            SignerError::DeviceAbsent
        ));
        let SignerError::Transport { error } = card_error(E::GenericError)
        else {
            panic!("a generic failure is a transport error");
        };
        assert!(error.contains("YKPIV_GENERIC_ERROR"), "{error}");
    }

    #[test]
    fn the_pcsc_refusal_says_how_to_fix_it() {
        let text = pcsc_error(pcsc::Error::NoService).to_string();
        assert!(text.contains("pcscd"), "{text}");
        assert!(text.contains("polkit"), "{text}");
    }

    /// A card's high-s signature comes out low, and still verifies
    #[test]
    fn a_high_s_from_the_card_is_normalized() {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        let key = p256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
        let public = OperatorPublicKey::EcdsaP256(
            super::super::EcdsaP256PublicKey::from_p256(key.verifying_key()),
        );
        let message = b"agora";
        let digest = Sha256::digest(message);
        let signature: p256::ecdsa::Signature =
            key.sign_prehash(&digest).unwrap();
        // Force the high half of the pair
        let low = signature.normalize_s();
        let n_minus_s = -*low.s();
        let high =
            p256::ecdsa::Signature::from_scalars(low.r(), n_minus_s).unwrap();
        assert_ne!(high, low);
        let mut r_s = [0u8; 64];
        r_s.copy_from_slice(&high.to_bytes());
        let out = low_s_p256(r_s).unwrap();
        assert!(public.verify(message, &out).is_ok());
        assert_eq!(out.to_bytes()[..], low.to_bytes()[..]);
    }
}
