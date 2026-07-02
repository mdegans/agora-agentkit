//! [`Keyring`] — per-process signing-key provider for [`SeedContext`].
//!
//! Keys ride [`Agent::Context`], never [`Agent::State`] — the state
//! serialization plane goes to [`Storage`], and backups of it must not carry
//! secrets.
//!
//! [`Agent::Context`]: crate::reactor::Agent::Context
//! [`Agent::State`]: crate::reactor::Agent::State
//! [`SeedContext`]: super::SeedContext
//! [`Storage`]: crate::reactor::Storage

use crate::crypto::SigningKey;
use crate::ids::AgentId;

/// Resolves an [`AgentId`] to its Ed25519 [`SigningKey`]
pub trait Keyring: Send + Sync {
    /// The signing key for `id`, or `None` if this ring doesn't hold one
    fn signing_key(&self, id: AgentId) -> Option<SigningKey>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::generate_keypair;

    use std::collections::HashMap;

    /// The in-memory case: any map loaded at process start.
    impl Keyring for HashMap<AgentId, SigningKey> {
        fn signing_key(&self, id: AgentId) -> Option<SigningKey> {
            self.get(&id).cloned()
        }
    }

    #[test]
    fn hashmap_ring_resolves() {
        let id = AgentId::new();
        let (key, _) = generate_keypair();
        let ring: HashMap<AgentId, SigningKey> =
            [(id, key.clone())].into_iter().collect();

        assert_eq!(
            ring.signing_key(id).map(|k| k.to_bytes()),
            Some(key.to_bytes())
        );
        assert!(ring.signing_key(AgentId::new()).is_none());
    }
}
