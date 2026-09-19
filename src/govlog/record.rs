//! The `data` of a `steward_record` entry (`REC-YYYY-NNNN`).
//!
//! A Steward's record says what was *done* — a key ceremony, a restore from
//! backup, the narrative behind a compromise declaration — and decides
//! nothing. No verifier reads it: to the chain it is content like a Council
//! decision's, covered by `data_hash` and nothing more.
//!
//! It exists because such a record names people, and the entries that carry
//! the act itself cannot: a `key_rotation` can never be redacted, so it holds
//! keys and hashes only. The narrative goes here, in a
//! [redactable](super::is_redactable) entry, where Art. II § 7 can reach it.
//!
//! **Every field that might hold personal data is a plain string**, because
//! a redaction replaces a value with a [string marker](super::redaction_marker):
//! a record with a participant's name removed still reads as a record.

use serde::{Deserialize, Serialize};

use crate::ids::GovernanceLogId;

/// The only `agora_steward_record` version there is
pub const STEWARD_RECORD_VERSION: u32 = 1;

/// The `data` of a `steward_record` entry. See the [module docs](self).
///
/// Unknown keys are tolerated on purpose: a stored record also carries the
/// writer's `_blind`, and a record is prose for readers, not a rule for
/// verifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct StewardRecord {
    /// Always [`STEWARD_RECORD_VERSION`]
    pub agora_steward_record: u32,
    /// What kind of act this records, as a short machine label:
    /// `"key_ceremony"`, `"restore"`, `"incident"`. A label and not an
    /// enum, so that a new kind of record is not a new wire format.
    pub kind: String,
    pub title: String,
    /// What happened, in prose (markdown)
    pub body: String,
    /// The entries this record is about — a ceremony's `KEY-` entry, say
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concerns: Vec<GovernanceLogId>,
    /// Who took part, and what each of them can vouch for
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<RecordParticipant>,
    /// Supporting material, inline and as text: a certificate in PEM, a
    /// command's output. Inline because a link rots and a hash of something
    /// nobody can fetch proves nothing to a reader.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<RecordAttachment>,
}

impl StewardRecord {
    /// A record with no participants or attachments yet
    pub fn new(
        kind: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            agora_steward_record: STEWARD_RECORD_VERSION,
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            concerns: Vec::new(),
            participants: Vec::new(),
            attachments: Vec::new(),
        }
    }
}

/// Someone who took part in a recorded act
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct RecordParticipant {
    pub name: String,
    /// `"Steward"`, `"scribe"`, `"delegate"`
    pub role: String,
    /// What this participant can vouch for, in their own terms — and so,
    /// by omission, what they cannot
    pub attests: String,
}

/// A piece of supporting material carried inside a record
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct RecordAttachment {
    pub name: String,
    /// What it is and how to check it
    pub note: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::super::{Blind, blind_data, non_integer_number, redact_data};
    use super::*;

    fn ceremony() -> StewardRecord {
        let mut record = StewardRecord::new(
            "key_ceremony",
            "The first key ceremony",
            "The root certified a fresh online key.",
        );
        record.concerns = vec!["KEY-2026-0001".parse().unwrap()];
        record.participants = vec![RecordParticipant {
            name: "A. Steward".into(),
            role: "Steward".into(),
            attests: "held the key".into(),
        }];
        record.attachments = vec![RecordAttachment {
            name: "attestation.pem".into(),
            note: "chains to the vendor's root".into(),
            content: "-----BEGIN CERTIFICATE-----\n…".into(),
        }];
        record
    }

    #[test]
    fn a_record_round_trips_and_holds_no_fractions() {
        let record = ceremony();
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["agora_steward_record"], 1);
        assert_eq!(non_integer_number(&value), None);
        let back: StewardRecord = serde_json::from_value(value).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn an_empty_list_is_not_written() {
        let value =
            serde_json::to_value(StewardRecord::new("restore", "t", "b"))
                .unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["agora_steward_record", "body", "kind", "title"]);
    }

    /// The point of the type: it is stored blinded, a name can be taken
    /// out of it, and what is left is still a record.
    #[test]
    fn a_stored_and_redacted_record_still_reads() {
        let stored = blind_data(
            &serde_json::to_value(ceremony()).unwrap(),
            Blind::from([7; 32]),
        )
        .unwrap();
        let read: StewardRecord =
            serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(read, ceremony());

        let amendment: GovernanceLogId = "AMD-2026-0009".parse().unwrap();
        let redacted = redact_data(
            &stored,
            &["/participants/0/name".to_string()],
            &amendment,
            Blind::from([8; 32]),
        )
        .unwrap();
        let read: StewardRecord = serde_json::from_value(redacted).unwrap();
        assert_eq!(read.participants[0].name, "[redacted by AMD-2026-0009]");
        assert_eq!(read.participants[0].role, "Steward");
    }
}
