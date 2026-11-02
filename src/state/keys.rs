//! The signed key records: [`StateKeyRecord`] and [`KeyRotation`]

use chrono::{DateTime, Utc};
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};

use super::chain::HeadRef;
use crate::crypto::{Signature, SigningKey, VerifyingKey};
use crate::enums::KeyRotationAuthority;
use crate::envelope::WRAPPED_KEY_LEN;
use crate::govlog::{HexLengthError, PublicKeyHex, SignatureHex};
use crate::ids::AgentId;

/// What a [`StateKeyRecord`]'s signature covers first
pub const STATE_KEY_CONTEXT: &[u8] = b"agora/state-key/v1";

/// What both signatures of a [`KeyRotation`] cover first
pub const KEY_ROTATION_CONTEXT: &[u8] = b"agora/key-rotation/v1";

crate::govlog::hex_bytes!(
    /// A state key wrapped to an X25519 key, hex on the wire:
    /// `version ‖ ephemeral_pub ‖ ct`, as `envelope`'s wrapped key
    WrappedStateKey,
    WRAPPED_KEY_LEN
);

/// An agent's state key, wrapped to its X25519 key and signed by its
/// Ed25519 key, so a server cannot plant a key it knows
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct StateKeyRecord {
    pub agent_id: AgentId,
    /// The state key's generation
    pub key_gen: u32,
    /// Which wrap of this generation: a new X25519 key re-wraps it
    pub wrap_seq: u32,
    /// The X25519 public key it is wrapped to
    pub wrapped_to_pub: PublicKeyHex,
    pub wrapped: WrappedStateKey,
    /// By the agent's Ed25519 key over [`signing_bytes`](Self::signing_bytes)
    pub signature: SignatureHex,
}

impl StateKeyRecord {
    /// `STATE_KEY_CONTEXT ‖ agent_id ‖ key_gen ‖ wrap_seq ‖ wrapped_to_pub
    /// ‖ wrapped`, integers little-endian
    pub fn signing_bytes(
        agent_id: AgentId,
        key_gen: u32,
        wrap_seq: u32,
        wrapped_to_pub: &PublicKeyHex,
        wrapped: &WrappedStateKey,
    ) -> Vec<u8> {
        let mut out = STATE_KEY_CONTEXT.to_vec();
        out.extend_from_slice(agent_id.as_uuid().as_bytes());
        out.extend_from_slice(&key_gen.to_le_bytes());
        out.extend_from_slice(&wrap_seq.to_le_bytes());
        out.extend_from_slice(wrapped_to_pub.as_bytes());
        out.extend_from_slice(wrapped.as_bytes());
        out
    }

    /// Sign the fields with the agent's key
    pub fn new(
        key: &SigningKey,
        agent_id: AgentId,
        key_gen: u32,
        wrap_seq: u32,
        wrapped_to_pub: PublicKeyHex,
        wrapped: WrappedStateKey,
    ) -> Self {
        let signature = key.sign(&Self::signing_bytes(
            agent_id,
            key_gen,
            wrap_seq,
            &wrapped_to_pub,
            &wrapped,
        ));
        Self {
            agent_id,
            key_gen,
            wrap_seq,
            wrapped_to_pub,
            wrapped,
            signature: SignatureHex::from(signature.to_bytes()),
        }
    }

    /// Whether the agent's `key` signed it
    pub fn verify(&self, key: &VerifyingKey) -> bool {
        let bytes = Self::signing_bytes(
            self.agent_id,
            self.key_gen,
            self.wrap_seq,
            &self.wrapped_to_pub,
            &self.wrapped,
        );
        key.verify_strict(
            &bytes,
            &Signature::from_bytes(self.signature.as_bytes()),
        )
        .is_ok()
    }
}

/// A change of an agent's Ed25519 signing key
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct KeyRotation {
    pub agent_id: AgentId,
    /// This agent's rotations so far, from 0
    pub seq: u32,
    pub old_public_key: PublicKeyHex,
    pub new_public_key: PublicKeyHex,
    pub authority: KeyRotationAuthority,
    /// The last main-line entry the old key may sign; `None` if it signed
    /// none. Trust is bound to this position, never to time
    pub old_key_last: Option<HeadRef>,
    /// By the old key over [`signing_bytes`](Self::signing_bytes); `None`
    /// for an operator's replacement of a lost key
    pub endorsement: Option<SignatureHex>,
    /// By the new key over the same bytes: possession
    pub proof: SignatureHex,
    pub rotated_at: DateTime<Utc>,
}

/// A [`KeyRotation`] that does not verify
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RotationError {
    #[error("a public key in the rotation is not an Ed25519 key")]
    BadKey,
    #[error("the new key's proof of possession does not verify")]
    BadProof,
    #[error("the old key's endorsement is missing or does not verify")]
    BadEndorsement,
    #[error("the rotation is the operator's word, not the old key's")]
    OperatorAuthority,
}

impl KeyRotation {
    /// `KEY_ROTATION_CONTEXT ‖ agent_id ‖ seq ‖ rotated_at ‖ authority ‖
    /// old_key_last ‖ old_pub ‖ new_pub`: `seq` u32, `rotated_at` i64
    /// microseconds, both little-endian; `authority` one byte (0 agent
    /// signed, 1 operator); `old_key_last` 0, or 1 ‖ seq u64 LE ‖ hash
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = KEY_ROTATION_CONTEXT.to_vec();
        out.extend_from_slice(self.agent_id.as_uuid().as_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(
            &self.rotated_at.timestamp_micros().to_le_bytes(),
        );
        out.push(match self.authority {
            KeyRotationAuthority::AgentSigned => 0,
            KeyRotationAuthority::Operator => 1,
        });
        match &self.old_key_last {
            None => out.push(0),
            Some(last) => {
                out.push(1);
                out.extend_from_slice(&last.seq.to_le_bytes());
                out.extend_from_slice(last.entry_hash.as_bytes());
            }
        }
        out.extend_from_slice(self.old_public_key.as_bytes());
        out.extend_from_slice(self.new_public_key.as_bytes());
        out
    }

    /// The agent's `seq`th rotation, from `old` to `new`, after `old`
    /// signed `old_key_last`, signed by both
    pub fn agent_signed(
        agent_id: AgentId,
        seq: u32,
        old: &SigningKey,
        new: &SigningKey,
        old_key_last: Option<HeadRef>,
        rotated_at: DateTime<Utc>,
    ) -> Self {
        // Microseconds: what the bytes carry, so a round trip verifies
        let rotated_at =
            DateTime::from_timestamp_micros(rotated_at.timestamp_micros())
                .expect("in range");
        let mut rotation = Self {
            agent_id,
            seq,
            old_public_key: PublicKeyHex::from(&old.verifying_key()),
            new_public_key: PublicKeyHex::from(&new.verifying_key()),
            authority: KeyRotationAuthority::AgentSigned,
            old_key_last,
            endorsement: None,
            proof: SignatureHex::from([0; 64]),
            rotated_at,
        };
        let bytes = rotation.signing_bytes();
        rotation.endorsement =
            Some(SignatureHex::from(old.sign(&bytes).to_bytes()));
        rotation.proof = SignatureHex::from(new.sign(&bytes).to_bytes());
        rotation
    }

    /// An operator's replacement of a lost `old` key: proved by `new`, with
    /// no endorsement. Never trusted on its own
    pub fn operator(
        agent_id: AgentId,
        seq: u32,
        old: PublicKeyHex,
        new: &SigningKey,
        old_key_last: Option<HeadRef>,
        rotated_at: DateTime<Utc>,
    ) -> Self {
        let rotated_at =
            DateTime::from_timestamp_micros(rotated_at.timestamp_micros())
                .expect("in range");
        let mut rotation = Self {
            agent_id,
            seq,
            old_public_key: old,
            new_public_key: PublicKeyHex::from(&new.verifying_key()),
            authority: KeyRotationAuthority::Operator,
            old_key_last,
            endorsement: None,
            proof: SignatureHex::from([0; 64]),
            rotated_at,
        };
        rotation.proof =
            SignatureHex::from(new.sign(&rotation.signing_bytes()).to_bytes());
        rotation
    }

    /// The proof of possession alone. What a server checks of an
    /// operator's replacement
    pub fn verify_proof(&self) -> Result<(), RotationError> {
        let new = self
            .new_public_key
            .to_verifying_key()
            .map_err(|_| RotationError::BadKey)?;
        new.verify_strict(
            &self.bytes(),
            &Signature::from_bytes(self.proof.as_bytes()),
        )
        .map_err(|_| RotationError::BadProof)
    }

    /// Whether the old key's word moves trust to the new key: the
    /// proof, and an endorsement by the old key. An operator's replacement
    /// is [`RotationError::OperatorAuthority`], never trusted on its own
    pub fn verify_agent_signed(&self) -> Result<(), RotationError> {
        self.verify_proof()?;
        if self.authority == KeyRotationAuthority::Operator {
            return Err(RotationError::OperatorAuthority);
        }
        let old = self
            .old_public_key
            .to_verifying_key()
            .map_err(|_| RotationError::BadKey)?;
        let endorsement =
            self.endorsement.ok_or(RotationError::BadEndorsement)?;
        old.verify_strict(
            &self.bytes(),
            &Signature::from_bytes(endorsement.as_bytes()),
        )
        .map_err(|_| RotationError::BadEndorsement)
    }

    fn bytes(&self) -> Vec<u8> {
        self.signing_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;

    #[test]
    fn an_agent_signed_rotation_verifies_and_an_operator_one_does_not() {
        let id = AgentId::new();
        let (old, _) = generate_keypair();
        let (new, _) = generate_keypair();
        let last = Some(HeadRef {
            entry_hash: crate::govlog::Sha256Hex::from([4; 32]),
            seq: 9,
        });
        let rotation =
            KeyRotation::agent_signed(id, 0, &old, &new, last, Utc::now());
        assert_eq!(rotation.verify_agent_signed(), Ok(()));

        let operator = KeyRotation::operator(
            id,
            0,
            PublicKeyHex::from(&old.verifying_key()),
            &new,
            last,
            Utc::now(),
        );
        assert_eq!(operator.verify_proof(), Ok(()));
        assert_eq!(
            operator.verify_agent_signed(),
            Err(RotationError::OperatorAuthority)
        );

        let mut forged = rotation.clone();
        let (other, _) = generate_keypair();
        forged.new_public_key = PublicKeyHex::from(&other.verifying_key());
        assert_eq!(forged.verify_agent_signed(), Err(RotationError::BadProof));

        // Its seq and time are signed
        let mut moved = rotation.clone();
        moved.seq = 1;
        assert_eq!(moved.verify_agent_signed(), Err(RotationError::BadProof));
        let mut moved = rotation.clone();
        moved.rotated_at += chrono::Duration::seconds(1);
        assert_eq!(moved.verify_agent_signed(), Err(RotationError::BadProof));
        let mut moved = rotation.clone();
        moved.old_key_last.as_mut().unwrap().seq = 10;
        assert_eq!(moved.verify_agent_signed(), Err(RotationError::BadProof));
        // A server can't flip it to the operator's word
        let mut flipped = rotation.clone();
        flipped.authority = KeyRotationAuthority::Operator;
        assert_eq!(flipped.verify_proof(), Err(RotationError::BadProof));

        let mut unendorsed = rotation;
        unendorsed.endorsement = None;
        assert_eq!(
            unendorsed.verify_agent_signed(),
            Err(RotationError::BadEndorsement)
        );
    }
}
