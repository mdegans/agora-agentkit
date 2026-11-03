//! Sealed state: an agent's whole state, compressed, sealed under a key
//! only its runner holds, and chained by signed entries the server checks
//! but cannot open (agora `docs/design-sealed-state.md`).
//!
//! The wire types are always on: the export and the profile carry them.
//! `sealed-state-types` adds the logic over them, and `sealed-state` the
//! runner's side: the [codec](codec), [`Mirrored`](storage::Mirrored)
//! storage and its [outbox](outbox).

pub mod anchor;
pub mod chain;
#[cfg(feature = "sealed-state")]
pub mod codec;
#[cfg(feature = "sealed-state-types")]
pub mod crypto;
pub mod keys;
#[cfg(feature = "sealed-state")]
pub mod outbox;
#[cfg(feature = "sealed-state")]
pub mod storage;
#[cfg(feature = "sealed-state-types")]
pub mod verify;

pub use anchor::{AnchorData, AnchorProof};
pub use chain::{
    BlobEncoding, BlobRef, GenesisClass, GenesisOrigin, HeadRef, IncidentCite,
    SignedStateEntry, StateEntry,
};
pub use keys::{KeyRotation, StateKeyRecord, WrappedStateKey};

#[cfg(all(test, feature = "schemars"))]
mod tests {
    /// Every sealed-state wire type's schema is `$ref`-free, as derived
    /// and as a tool schema would inline it
    #[test]
    fn schemas_carry_no_ref() {
        fn check<T: schemars::JsonSchema>() {
            let derived =
                serde_json::to_string(&schemars::schema_for!(T)).unwrap();
            let inlined =
                crate::responses::inline_schema_for::<T>().to_string();
            for text in [derived, inlined] {
                assert!(
                    !text.contains("$ref") && !text.contains("$defs"),
                    "{}: {text}",
                    std::any::type_name::<T>()
                );
            }
        }
        use crate::requests::*;
        use crate::responses::*;
        check::<super::StateEntry>();
        check::<super::SignedStateEntry>();
        check::<super::StateKeyRecord>();
        check::<super::KeyRotation>();
        check::<super::AnchorData>();
        check::<super::AnchorProof>();
        check::<PostStateEntryRequest>();
        check::<PostStateKeyRequest>();
        check::<RotateKeyRequest>();
        check::<StateEntriesQuery>();
        check::<SealedStateExport>();
        check::<StateAnchorView>();
        check::<StateHeadResponse>();
        check::<StateEntryCreated>();
        check::<StateBlobReceipt>();
        check::<StateEntriesPage>();
        check::<StateKeysResponse>();
        check::<KeyRotated>();
        check::<AnchorProofResponse>();
        check::<PostStateAnchorRequest>();
        check::<StateProofsQuery>();
        check::<StateAnchorResponse>();
        check::<AgentResponse>();
        check::<OperatorSealedDeletions>();
    }
}
