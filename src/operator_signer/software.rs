//! [`SoftwareKeySigner`]: an Ed25519 operator key from a secret file.

use std::fmt;
use std::path::Path;

use ed25519_dalek::Signer;

use super::{
    OperatorPublicKey, OperatorSignature, OperatorSigner, SignerError,
    SignerKind,
};
use crate::crypto::{self, SigningKey};
use crate::secrets::Secret;

/// The `software_key` credential: an Ed25519 key held in memory.
///
/// Loaded the way every other Agora signing key is: hex, from a file, via
/// [`Secret::from_file`] and [`crypto::signing_key_from_hex`]. The key
/// zeroizes itself on drop, and neither `Debug` nor `Display` prints it.
pub struct SoftwareKeySigner {
    key: SigningKey,
}

impl SoftwareKeySigner {
    /// From a key already in memory.
    pub fn new(key: SigningKey) -> Self {
        Self { key }
    }

    /// From a file holding the key's 32 bytes as hex (surrounding
    /// whitespace ignored). Reads synchronously: open signers at startup.
    pub fn from_file(path: &Path) -> Result<Self, SignerError> {
        let secret =
            Secret::from_file(path).map_err(|source| SignerError::KeyFile {
                path: path.to_owned(),
                source,
            })?;
        crypto::signing_key_from_hex(secret.expose())
            .map(Self::new)
            .map_err(|_| SignerError::KeyFormat)
    }
}

impl fmt::Debug for SoftwareKeySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SoftwareKeySigner([REDACTED])")
    }
}

impl fmt::Display for SoftwareKeySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[async_trait::async_trait]
impl OperatorSigner for SoftwareKeySigner {
    fn public_key(&self) -> OperatorPublicKey {
        OperatorPublicKey::Ed25519(self.key.verifying_key())
    }

    fn kind(&self) -> SignerKind<'_> {
        SignerKind::Software
    }

    async fn sign(
        &self,
        message: &[u8],
    ) -> Result<OperatorSignature, SignerError> {
        Ok(OperatorSignature::Ed25519(self.key.sign(message)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator_signer::{
        SignatureAlgorithm, sign_request, verify_request,
    };
    use std::io::Write;

    #[tokio::test]
    async fn file_key_signs_requests_that_crypto_verify_accepts() {
        let (key, vk) = crypto::generate_keypair();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", crypto::signing_key_to_hex(&key)).unwrap();

        let signer = SoftwareKeySigner::from_file(file.path()).unwrap();
        assert_eq!(signer.public_key(), OperatorPublicKey::Ed25519(vk));
        assert_eq!(signer.algorithm(), SignatureAlgorithm::Ed25519);
        assert!(matches!(signer.kind(), SignerKind::Software));
        assert_eq!(
            signer.credential_kind(),
            crate::enums::OperatorCredentialKind::SoftwareKey
        );

        let signature = sign_request(&signer, b"payload", 1_700_000_000)
            .await
            .unwrap();
        verify_request(
            &signer.public_key(),
            b"payload",
            1_700_000_000,
            &signature,
        )
        .unwrap();
        let OperatorSignature::Ed25519(raw) = signature else {
            panic!("Ed25519 key, Ed25519 signature");
        };
        assert!(crypto::verify(&vk, b"payload", 1_700_000_000, &raw));
        assert!(
            verify_request(
                &signer.public_key(),
                b"payload",
                1_700_000_001,
                &signature
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn works_as_a_trait_object() {
        let signer: Box<dyn OperatorSigner> =
            Box::new(SoftwareKeySigner::new(crypto::generate_keypair().0));
        let signature = signer.sign(b"m").await.unwrap();
        signer.public_key().verify(b"m", &signature).unwrap();
    }

    #[test]
    fn missing_and_malformed_files_are_errors() {
        assert!(matches!(
            SoftwareKeySigner::from_file(Path::new("/nonexistent/key")),
            Err(SignerError::KeyFile { .. })
        ));
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "abcd").unwrap();
        assert!(matches!(
            SoftwareKeySigner::from_file(file.path()),
            Err(SignerError::KeyFormat)
        ));
    }

    #[test]
    fn key_format_errors_do_not_echo_the_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "Sk9xQ...base64secret").unwrap();
        let error = SoftwareKeySigner::from_file(file.path()).unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(!text.contains("Sk9") && !text.contains("base64"), "{text}");
        assert!(!text.contains("position") && !text.contains("'"), "{text}");
    }

    #[test]
    fn debug_and_display_are_redacted() {
        let key = crypto::generate_keypair().0;
        let hex = crypto::signing_key_to_hex(&key);
        let signer = SoftwareKeySigner::new(key);
        let debug = format!("{signer:?}");
        let display = format!("{signer}");
        assert!(debug.contains("[REDACTED]") && !debug.contains(&hex));
        assert_eq!(display, "[REDACTED]");
    }

    #[tokio::test]
    async fn a_software_key_proves_possession() {
        use crate::ids::OperatorId;
        use crate::operator_signer::{sign_possession, verify_possession};
        let (key, _) = crypto::generate_keypair();
        let signer = SoftwareKeySigner::new(key);
        let operator = OperatorId::new();
        let proof = sign_possession(&signer, operator).await.unwrap();
        verify_possession(&signer.public_key(), operator, &proof).unwrap();
        let other = SoftwareKeySigner::new(crypto::generate_keypair().0);
        assert!(
            verify_possession(&other.public_key(), operator, &proof).is_err()
        );
        assert!(
            verify_possession(&signer.public_key(), OperatorId::new(), &proof)
                .is_err()
        );
    }
}
