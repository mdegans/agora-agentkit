//! Free text an [`Amendment`](super::Amendment) commits to without
//! containing.
//!
//! An amendment can never be redacted: a verifier acts on its `data`, and
//! could not tell a redaction of the rationale from a rewrite of the
//! `kind`. So from version 2 the signed `data` holds, for each free-text
//! field, only
//!
//! ```text
//! commitment = SHA-256( salt || text )        -- salt: 32 random bytes
//! ```
//!
//! and the text and its salt travel beside the entry, outside everything
//! hashed. Erasing a text is deleting it *and its salt*: `data`, its hash
//! and the chain never change, and what is left cannot be used to confirm
//! a guess at what was there.

use super::{Sha256Hex, TextSalt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What [`AmendmentNotice`](super::AmendmentNotice) shows for a text that
/// is no longer beside its entry
pub const WITHHELD_TEXT: &str = "[text withheld]";

/// What an amendment's signed `data` holds in place of a text
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct TextCommitment {
    pub commitment: Sha256Hex,
}

impl TextCommitment {
    pub fn of(salt: &TextSalt, text: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(text.as_bytes());
        Self {
            commitment: Sha256Hex::from(<[u8; 32]>::from(hasher.finalize())),
        }
    }

    pub fn matches(&self, text: &CommittedText) -> bool {
        *self == Self::of(&text.salt, &text.text)
    }
}

/// A text and the salt its [`TextCommitment`] was made with: beside the
/// entry, never in its hashed `data`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct CommittedText {
    pub salt: TextSalt,
    pub text: String,
}

impl CommittedText {
    /// `text` under a [random](TextSalt::random) salt
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_salt(TextSalt::random(), text)
    }

    /// `salt` must be [random](TextSalt::random) outside tests
    pub fn with_salt(salt: TextSalt, text: impl Into<String>) -> Self {
        Self {
            salt,
            text: text.into(),
        }
    }

    pub fn commitment(&self) -> TextCommitment {
        TextCommitment::of(&self.salt, &self.text)
    }
}

impl From<&str> for CommittedText {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for CommittedText {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// A free-text field of an [`Amendment`](super::Amendment): the text
/// itself in version 1, a commitment to it from version 2
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(untagged)]
pub enum AmendmentText {
    Plain(String),
    Committed(TextCommitment),
}

impl AmendmentText {
    pub fn is_plain(&self) -> bool {
        matches!(self, Self::Plain(_))
    }

    /// What a reader is shown: the text, or [`WITHHELD_TEXT`] when it is
    /// not `beside` the entry — or is, and is not the text committed to
    pub fn resolve<'a>(&'a self, beside: Option<&'a CommittedText>) -> &'a str {
        match (self, beside) {
            (Self::Plain(text), _) => text,
            (Self::Committed(c), Some(t)) if c.matches(t) => &t.text,
            (Self::Committed(_), _) => WITHHELD_TEXT,
        }
    }

    /// `None` for a version 1 text, which is in the signed `data` and has
    /// no status to report
    pub fn status(&self, beside: Option<&CommittedText>) -> Option<TextStatus> {
        match (self, beside) {
            (Self::Plain(_), _) => None,
            (Self::Committed(_), None) => Some(TextStatus::Withheld),
            (Self::Committed(c), Some(t)) if c.matches(t) => {
                Some(TextStatus::Present)
            }
            (Self::Committed(_), Some(_)) => Some(TextStatus::Mismatch),
        }
    }
}

/// The texts beside one amendment entry; each is there or withheld on its
/// own, so erasing a rationale does not take the `note` with it
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct AmendmentTexts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<CommittedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<CommittedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<CommittedText>,
}

impl AmendmentTexts {
    pub fn is_empty(&self) -> bool {
        self.basis.is_none() && self.note.is_none() && self.rationale.is_none()
    }
}

/// Where a committed text stands
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum TextStatus {
    // Beside the entry, and the text the entry committed to.
    Present,
    // Not beside the entry. Lawful: this is what erasure looks like.
    Withheld,
    // Beside the entry and not what it committed to. Never lawful.
    Mismatch,
}

/// [`TextStatus`] of each text of a version 2 amendment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AmendmentTextStatus {
    pub basis: TextStatus,
    pub note: TextStatus,
    /// `None` when the amendment commits to no rationale
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<TextStatus>,
}
