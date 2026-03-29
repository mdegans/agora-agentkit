//! Secret value management with zeroization and redacted output.
//!
//! Use [`Secret::from_file`] to load secrets from file-mounted paths
//! (e.g., Docker secrets). The inner value is zeroized on drop and never
//! appears in `Debug` or `Display` output.

use std::path::Path;
use zeroize::Zeroizing;

/// A secret value that is zeroized on drop and redacted in debug/display output.
///
/// Use `from_file` to load secrets from file-mounted paths (e.g., Docker secrets).
/// Use `expose` to access the inner value when needed.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Load a secret from a file, trimming trailing whitespace.
    pub fn from_file(path: &Path) -> Result<Self, std::io::Error> {
        let contents = std::fs::read_to_string(path)?;
        Ok(Self(Zeroizing::new(contents.trim().to_owned())))
    }

    /// Create a secret from a string value.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Access the secret value. Use sparingly.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn from_file_and_expose() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "  my-secret-value  \n").unwrap();

        let secret = Secret::from_file(file.path()).unwrap();
        assert_eq!(secret.expose(), "my-secret-value");
    }

    #[test]
    fn debug_is_redacted() {
        let secret = Secret::new("hunter2".to_string());
        assert_eq!(format!("{:?}", secret), "[REDACTED]");
    }

    #[test]
    fn display_is_redacted() {
        let secret = Secret::new("hunter2".to_string());
        assert_eq!(format!("{}", secret), "[REDACTED]");
    }

    #[test]
    fn nonexistent_file_returns_error() {
        let result = Secret::from_file(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn clone_preserves_value() {
        let secret = Secret::new("cloneable".to_string());
        let cloned = secret.clone();
        assert_eq!(cloned.expose(), "cloneable");
    }
}
