//! The derived challenge that binds a passkey tap to one action (agora
//! `docs/design-operator-api.md`, *Sudo: taps*):
//!
//! ```text
//! challenge = SHA-256("agora/portal-approval/v1\n" ‖ nonce16 ‖ canonical_bytes)
//! canonical_bytes = kind tag (u8) ‖ the kind's fields, fixed width, big-endian
//! ```

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::ids::{
    AgentId, OAuthClientId, OperatorActionId, OperatorCredentialId,
    OperatorEmailAddressId, OperatorEnrolmentId, OperatorId, OperatorPasskeyId,
    OperatorPendingChangeId, OperatorRecoveryCaseId,
};

/// What every derived challenge's preimage begins with
pub const CONTEXT: &[u8] = b"agora/portal-approval/v1\n";

/// Kind tags never produced again: 0x05 was the two-tap passkey add, whose rows
/// stay in the action log, and 0x07 was retired before any
pub const RETIRED_TAGS: [u8; 2] = [0x05, 0x07];

/// What a passkey tap approves: its canonical bytes' first byte
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum TapKind {
    Send = 0x01,
    OAuthConsent = 0x02,
    EnrolmentApproval = 0x03,
    Suspend = 0x04,
    /// Retired: only stored assertions have it, and no event accepts it
    RetiredPasskeyEnrolment = 0x05,
    PendingChange = 0x06,
    // 0x07 retired, never reused
    Unfreeze = 0x08,
    Contest = 0x09,
    AgentDeletion = 0x0A,
    LogPrivate = 0x0B,
    CancelPending = 0x0C,
    FirstContact = 0x0D,
    ReArm = 0x0E,
    CompromiseNotice = 0x0F,
    /// A counter-claim against a decided recovery case (0.140)
    CounterClaim = 0x10,
    /// Moving a send: earlier, or later when a YubiKey tap made it
    MoveSend = 0x11,
    /// Opening a passkey add's grant
    PasskeyAdd = 0x12,
}

impl TapKind {
    /// Every kind still produced, by tag
    pub const ALL: [Self; 16] = [
        Self::Send,
        Self::OAuthConsent,
        Self::EnrolmentApproval,
        Self::Suspend,
        Self::PendingChange,
        Self::Unfreeze,
        Self::Contest,
        Self::AgentDeletion,
        Self::LogPrivate,
        Self::CancelPending,
        Self::FirstContact,
        Self::ReArm,
        Self::CompromiseNotice,
        Self::CounterClaim,
        Self::MoveSend,
        Self::PasskeyAdd,
    ];

    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The kind with this tag, 0x05 included, for reading stored assertions;
    /// `None` for 0x07 and unknown ones
    pub fn from_tag(tag: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::RetiredPasskeyEnrolment])
            .find(|k| k.tag() == tag)
    }

    pub const fn is_retired(self) -> bool {
        matches!(self, Self::RetiredPasskeyEnrolment)
    }
}

/// What a suspension names (0x04's target kind byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuspendTarget {
    Credential(OperatorCredentialId),
    Passkey(OperatorPasskeyId),
}

/// SHA-256, the digest the encoding uses for every variable-length field
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// One approval, with the fields its kind binds
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// 0x01
    Send {
        action: OperatorActionId,
        /// The digest the compose preview carries; never a body hash
        recipients_digest: [u8; 32],
        deliver_at: DateTime<Utc>,
        immediate: bool,
    },
    /// 0x02: the client id's UTF-8 enters hashed
    OAuthConsent {
        client: OAuthClientId,
        agent: AgentId,
    },
    /// 0x03
    EnrolmentApproval {
        enrolment: OperatorEnrolmentId,
        /// SHA-256 of the public key's raw bytes
        public_key_sha256: [u8; 32],
    },
    /// 0x04
    Suspend { target: SuspendTarget },
    /// 0x06: covers a change that waits and one a YubiKey tap makes at once
    PendingChange {
        change: OperatorPendingChangeId,
        /// SHA-256 of [`change_canonical_form`]
        canonical_sha256: [u8; 32],
    },
    /// 0x08
    Unfreeze { freeze_entry: OperatorActionId },
    /// 0x09
    Contest {
        /// The target's `credential_suspend` entry
        suspend_entry: OperatorActionId,
        passkey: OperatorPasskeyId,
    },
    /// 0x0A
    AgentDeletion { agent: AgentId },
    /// 0x0B
    LogPrivate { operator: OperatorId },
    /// 0x0C
    CancelPending { item: OperatorPendingChangeId },
    /// 0x0D
    FirstContact {
        operator: OperatorId,
        /// SHA-256 of the normalised address
        address_sha256: [u8; 32],
    },
    /// 0x0E
    ReArm {
        operator: OperatorId,
        address: OperatorEmailAddressId,
    },
    /// 0x0F
    CompromiseNotice {
        operator: OperatorId,
        freeze_entry: OperatorActionId,
    },
    /// 0x10
    CounterClaim {
        operator: OperatorId,
        case: OperatorRecoveryCaseId,
    },
    /// 0x11
    MoveSend {
        action: OperatorActionId,
        deliver_at: DateTime<Utc>,
    },
    /// 0x12: names no key, which doesn't exist yet
    PasskeyAdd { operator: OperatorId },
}

fn uuid_bytes(id: impl Into<uuid::Uuid>) -> [u8; 16] {
    *id.into().as_bytes()
}

impl Approval {
    pub fn kind(&self) -> TapKind {
        match self {
            Self::Send { .. } => TapKind::Send,
            Self::OAuthConsent { .. } => TapKind::OAuthConsent,
            Self::EnrolmentApproval { .. } => TapKind::EnrolmentApproval,
            Self::Suspend { .. } => TapKind::Suspend,
            Self::PendingChange { .. } => TapKind::PendingChange,
            Self::Unfreeze { .. } => TapKind::Unfreeze,
            Self::Contest { .. } => TapKind::Contest,
            Self::AgentDeletion { .. } => TapKind::AgentDeletion,
            Self::LogPrivate { .. } => TapKind::LogPrivate,
            Self::CancelPending { .. } => TapKind::CancelPending,
            Self::FirstContact { .. } => TapKind::FirstContact,
            Self::ReArm { .. } => TapKind::ReArm,
            Self::CompromiseNotice { .. } => TapKind::CompromiseNotice,
            Self::CounterClaim { .. } => TapKind::CounterClaim,
            Self::MoveSend { .. } => TapKind::MoveSend,
            Self::PasskeyAdd { .. } => TapKind::PasskeyAdd,
        }
    }

    /// The kind tag, then the kind's fields, fixed width, big-endian
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![self.kind().tag()];
        match self {
            Self::Send {
                action,
                recipients_digest,
                deliver_at,
                immediate,
            } => {
                out.extend(uuid_bytes(*action));
                out.extend(recipients_digest);
                out.extend(deliver_at.timestamp_micros().to_be_bytes());
                out.push(u8::from(*immediate));
            }
            Self::OAuthConsent { client, agent } => {
                out.extend(sha256(client.as_str().as_bytes()));
                out.extend(uuid_bytes(*agent));
            }
            Self::EnrolmentApproval {
                enrolment,
                public_key_sha256,
            } => {
                out.extend(uuid_bytes(*enrolment));
                out.extend(public_key_sha256);
            }
            Self::Suspend { target } => match target {
                SuspendTarget::Credential(id) => {
                    out.extend(uuid_bytes(*id));
                    out.push(0);
                }
                SuspendTarget::Passkey(id) => {
                    out.extend(uuid_bytes(*id));
                    out.push(1);
                }
            },
            Self::PendingChange {
                change,
                canonical_sha256,
            } => {
                out.extend(uuid_bytes(*change));
                out.extend(canonical_sha256);
            }
            Self::Unfreeze { freeze_entry } => {
                out.extend(uuid_bytes(*freeze_entry));
            }
            Self::Contest {
                suspend_entry,
                passkey,
            } => {
                out.extend(uuid_bytes(*suspend_entry));
                out.extend(uuid_bytes(*passkey));
            }
            Self::AgentDeletion { agent } => out.extend(uuid_bytes(*agent)),
            Self::LogPrivate { operator } => out.extend(uuid_bytes(*operator)),
            Self::CancelPending { item } => out.extend(uuid_bytes(*item)),
            Self::FirstContact {
                operator,
                address_sha256,
            } => {
                out.extend(uuid_bytes(*operator));
                out.extend(address_sha256);
            }
            Self::ReArm { operator, address } => {
                out.extend(uuid_bytes(*operator));
                out.extend(uuid_bytes(*address));
            }
            Self::CompromiseNotice {
                operator,
                freeze_entry,
            } => {
                out.extend(uuid_bytes(*operator));
                out.extend(uuid_bytes(*freeze_entry));
            }
            Self::CounterClaim { operator, case } => {
                out.extend(uuid_bytes(*operator));
                out.extend(uuid_bytes(*case));
            }
            Self::MoveSend { action, deliver_at } => {
                out.extend(uuid_bytes(*action));
                out.extend(deliver_at.timestamp_micros().to_be_bytes());
            }
            Self::PasskeyAdd { operator } => out.extend(uuid_bytes(*operator)),
        }
        out
    }

    /// [`derived_challenge`] over this approval's canonical bytes
    pub fn challenge(&self, nonce: &[u8; 16]) -> [u8; 16] {
        derived_challenge(nonce, &self.canonical_bytes())
    }
}

/// `SHA-256(CONTEXT ‖ nonce ‖ canonical_bytes)[..16]`. The nonce is 16
/// fresh random bytes the server draws after the canonical bytes are fixed.
/// 16 bytes because webauthn_rp's challenge is a `u128` (the spec's
/// *Challenge size*)
pub fn derived_challenge(nonce: &[u8; 16], canonical_bytes: &[u8]) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(CONTEXT);
    hash.update(nonce);
    hash.update(canonical_bytes);
    let full: [u8; 32] = hash.finalize().into();
    let mut out = [0; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// A pending change's canonical form (0x06): the setting's name, then its
/// new value, each a big-endian `u32` length then UTF-8. The one encoding
/// of each setting's value is
/// [`SettingChange::canonical_form`](super::SettingChange::canonical_form)
pub fn change_canonical_form(
    setting: &str,
    value: &str,
) -> Result<Vec<u8>, PartTooLong> {
    let mut out = Vec::with_capacity(8 + setting.len() + value.len());
    for part in [setting, value] {
        let len = u32::try_from(part.len()).map_err(|_| PartTooLong)?;
        out.extend(len.to_be_bytes());
        out.extend(part.as_bytes());
    }
    Ok(out)
}

/// A canonical form's part longer than a `u32` length can say
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a canonical form's part is longer than 4 GiB")]
pub struct PartTooLong;

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T: From<uuid::Uuid>>(s: &str) -> T {
        T::from(s.parse::<uuid::Uuid>().unwrap())
    }

    const NONCE: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
        0xcc, 0xdd, 0xee, 0xff,
    ];
    const A: &str = "00112233-4455-6677-8899-aabbccddeeff";
    const B: &str = "0b7e2a61-5d3c-4c8e-8f1a-2e9d4b6c7a10";

    /// One approval of every kind, with fixed fields
    fn every_kind() -> Vec<Approval> {
        let digest = sha256(b"digest");
        vec![
            Approval::Send {
                action: id(A),
                recipients_digest: digest,
                deliver_at: DateTime::from_timestamp_micros(
                    1_761_782_400_123_456,
                )
                .unwrap(),
                immediate: true,
            },
            Approval::OAuthConsent {
                client: "claude-ai".parse().unwrap(),
                agent: id(B),
            },
            Approval::EnrolmentApproval {
                enrolment: id(A),
                public_key_sha256: digest,
            },
            Approval::Suspend {
                target: SuspendTarget::Passkey(id(A)),
            },
            Approval::PendingChange {
                change: id(A),
                canonical_sha256: sha256(
                    &change_canonical_form("dm_hold", "60").unwrap(),
                ),
            },
            Approval::Unfreeze {
                freeze_entry: id(A),
            },
            Approval::Contest {
                suspend_entry: id(A),
                passkey: id(B),
            },
            Approval::AgentDeletion { agent: id(A) },
            Approval::LogPrivate { operator: id(A) },
            Approval::CancelPending { item: id(A) },
            Approval::FirstContact {
                operator: id(A),
                address_sha256: digest,
            },
            Approval::ReArm {
                operator: id(A),
                address: id(B),
            },
            Approval::CompromiseNotice {
                operator: id(A),
                freeze_entry: id(B),
            },
            Approval::CounterClaim {
                operator: id(A),
                case: id(B),
            },
            Approval::MoveSend {
                action: id(A),
                deliver_at: DateTime::from_timestamp_micros(
                    1_761_782_400_123_456,
                )
                .unwrap(),
            },
            Approval::PasskeyAdd { operator: id(A) },
        ]
    }

    #[test]
    fn tags_are_the_spec_table_and_0x05_and_0x07_are_retired() {
        let tags: Vec<u8> = TapKind::ALL.iter().map(|k| k.tag()).collect();
        assert_eq!(
            tags,
            [
                1, 2, 3, 4, 6, 8, 9, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10,
                0x11, 0x12
            ]
        );
        assert_eq!(TapKind::from_tag(0x07), None);
        assert_eq!(TapKind::from_tag(0x00), None);
        assert_eq!(TapKind::from_tag(0x13), None);
        // 0x05 still reads, so stored assertions do, and isn't produced
        let retired = TapKind::from_tag(0x05).unwrap();
        assert!(retired.is_retired() && !TapKind::ALL.contains(&retired));
        for k in TapKind::ALL {
            assert_eq!(TapKind::from_tag(k.tag()), Some(k));
            assert!(!k.is_retired());
            assert!(!RETIRED_TAGS.contains(&k.tag()));
        }
        let kinds: Vec<TapKind> =
            every_kind().iter().map(Approval::kind).collect();
        assert_eq!(kinds, TapKind::ALL);
    }

    #[test]
    fn fields_are_fixed_width() {
        let widths: Vec<usize> = every_kind()
            .iter()
            .map(|a| a.canonical_bytes().len())
            .collect();
        assert_eq!(
            widths,
            [
                1 + 16 + 32 + 8 + 1,
                1 + 32 + 16,
                1 + 16 + 32,
                1 + 16 + 1,
                1 + 16 + 32,
                1 + 16,
                1 + 16 + 16,
                1 + 16,
                1 + 16,
                1 + 16,
                1 + 16 + 32,
                1 + 16 + 16,
                1 + 16 + 16,
                1 + 16 + 16,
                1 + 16 + 8,
                1 + 16,
            ]
        );
    }

    #[test]
    fn send_bytes_are_big_endian_micros() {
        let bytes = every_kind()[0].canonical_bytes();
        assert_eq!(bytes[0], 0x01);
        assert_eq!(&bytes[1..17], &uuid_bytes(id::<OperatorActionId>(A)));
        assert_eq!(&bytes[49..57], &1_761_782_400_123_456_i64.to_be_bytes());
        assert_eq!(bytes[57], 1);
        // The suspend target byte: 0 credential, 1 passkey
        let credential = Approval::Suspend {
            target: SuspendTarget::Credential(id(A)),
        };
        assert_eq!(*credential.canonical_bytes().last().unwrap(), 0);
        assert_eq!(*every_kind()[3].canonical_bytes().last().unwrap(), 1);
    }

    #[test]
    fn change_form_is_length_prefixed() {
        assert_eq!(
            change_canonical_form("dm_hold", "60").unwrap(),
            b"\0\0\0\x07dm_hold\0\0\0\x0260".to_vec()
        );
        // The prefix keeps a boundary from moving
        assert_ne!(
            change_canonical_form("ab", "c"),
            change_canonical_form("a", "bc")
        );
    }

    /// Pinned test vectors, one per kind: the full SHA-256 over [`NONCE`]
    /// and the fields in [`every_kind`]; the challenge is its first 16 bytes
    #[test]
    fn challenge_vectors() {
        assert_eq!(VECTORS.len(), TapKind::ALL.len());
        for (approval, want) in every_kind().iter().zip(VECTORS) {
            let mut preimage = CONTEXT.to_vec();
            preimage.extend(NONCE);
            preimage.extend(approval.canonical_bytes());
            assert_eq!(
                hex::encode(sha256(&preimage)),
                want,
                "{:?}",
                approval.kind()
            );
            assert_eq!(
                hex::encode(approval.challenge(&NONCE)),
                want[..32],
                "{:?}",
                approval.kind()
            );
        }
    }

    /// The challenge is SHA-256 over context, nonce and bytes, nothing else
    #[test]
    fn challenge_is_the_spec_preimage() {
        let approval = &every_kind()[5];
        let mut preimage = b"agora/portal-approval/v1\n".to_vec();
        preimage.extend(NONCE);
        preimage.extend(approval.canonical_bytes());
        assert_eq!(approval.challenge(&NONCE), sha256(&preimage)[..16]);
        let mut other = NONCE;
        other[0] ^= 1;
        assert_ne!(approval.challenge(&NONCE), approval.challenge(&other));
    }

    /// A stored 0x05 assertion stays verifiable from its own bytes, which are
    /// all a reader needs: the digest of a kind nothing produces any more
    #[test]
    fn a_stored_0x05_challenge_still_derives() {
        let mut bytes = vec![0x05];
        bytes.extend(uuid_bytes(id::<OperatorPasskeyId>(A)));
        bytes.extend(sha256(b"digest"));
        assert_eq!(
            hex::encode(derived_challenge(&NONCE, &bytes)),
            "cac0483fde1848a473d4a63edb7975be"
        );
    }

    /// Computed independently (Python `hashlib`) from the spec's table
    const VECTORS: [&str; 16] = [
        "e417deb6abe94bb74037918fb0f749e401b58c3b282b206c96963217826c3435",
        "a4304de3920dfbb53406abda4991fa100f08578a65fd37a361de8364bf438940",
        "b55e306e201216cb476830ef7bada806c0e1f97ae49cca4b50aeb8989d0bddde",
        "c5108e5d7fc822bb58bcdcaffdf620167c793de11a408181ce68381943a66f91",
        "36ccbd6e1a321935c842b90a1ce25470df00e721cfb59f80023703a642c51658",
        "38573fdcbd3e51d270a83649e191d42482d6f3b6c58f303b5a62f70b662b9938",
        "2d300dda2a14bd49d8317b955b271623601e812407ca5d7366dc7c99688c0a58",
        "fef9480c24d13e0d621e550853c858a66fbb7c8ddb90a93ceed85fccf8b5181c",
        "b4e48b7d95361d0cb4d1960766f2df1e40e2e29e348e603148fb789b65d5356c",
        "90d4a0f117bb2b59f93a9135bd28b52f5e6c0dafc87cf13d72cb2d1c2eb3a386",
        "3db852cf63852ea9503a9022843ac4852b04750b29b17692964f3ca122878836",
        "f7ffe9ecaaf866cf58fabf8f4cbeb93419b3a158228b8bb2c2b9aea2b01c01de",
        "9f8a8ea4904c1fd8a9d9fcb6097be539dd66b4b58d419623bda57ff4fe0858d4",
        "503e9692af58b3013f1401a73aa707ac7053cf915033358e03b4ac9b444dc700",
        "4a187c9157d8c8d42fb3c38380e2d96869c2697d16347ba716a8b96704114ed4",
        "1b8688cf24d1c7db7b53257c3d9b51ffbde4658f67f57486a148f1ebfad9ec8f",
    ];
}
