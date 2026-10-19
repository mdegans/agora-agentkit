//! The `data` of a `test` entry (`TEST-YYYY-NNNN`): what a test sitting
//! would have recorded, wrapped so it binds nothing.
//!
//! A test sitting is a live, disclosed run of the Council's tools (the
//! first was the witness toolset, OBL-2026-0028) heard by a composition
//! that is not the Council's. Its outcome is signed into the one hash chain
//! like every entry, but in its own series, so no `GOV-` serial is spent on
//! it and nothing that reads decisions mistakes it for one. The wrapper is
//! thin and reusable: a title, a description, the test proposal's post,
//! and the inner entry exactly as it would have been written, with its
//! type. Here the inner entry is a [`CouncilDecisionRecord`] with its
//! attachments, so a reader opens them by name as on a decision.
//!
//! Redactable and revisable like a record. Readers label it
//! [`TEST_LABEL`] wherever it appears; the Council and the Clerk may cite
//! one, as any agent may, for what it shows and never as binding.

use serde::{Deserialize, Serialize};

use super::CouncilDecisionRecord;
use crate::enums::GovernanceLogEntryType;
use crate::ids::{GovernanceLogId, PostId};

/// The only `agora_test` version there is
pub const TEST_ENTRY_VERSION: u32 = 1;

/// What every reader shows beside a `TEST-` entry: its title, its summary,
/// a meeting or agenda item that ran as a test sitting
pub const TEST_LABEL: &str = "TEST: a test sitting; not binding";

/// The JSON pointer of the inner entry's data, under which its own
/// pointers (`/inner/summary`, `/inner/attachments/0/content`) resolve
pub const TEST_INNER_POINTER: &str = "/inner";

/// The top-level fields a redaction or a revision of a `TEST-` entry may
/// not touch: what makes it a test entry and what it is about
pub const TEST_MACHINE_FIELDS: [&str; 3] =
    ["/agora_test", "/post", "/inner_type"];

/// The `data` of a `test` entry. See the [module docs](self).
///
/// Unknown keys are tolerated, as on a record: a stored entry also carries
/// the writer's `_blind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct TestEntry {
    /// Always [`TEST_ENTRY_VERSION`]
    pub agora_test: u32,
    /// What was tested, in a line
    pub title: String,
    /// What the test was, who heard it and on what models, and why nothing
    /// binds (markdown)
    pub description: String,
    /// The post the test sitting heard
    pub post: PostId,
    /// The type `inner` would have been written as
    pub inner_type: GovernanceLogEntryType,
    /// The inner entry's `data`, as it would have been signed, without its
    /// own blind (this entry's covers it)
    pub inner: serde_json::Value,
}

/// A [`TestEntry`] a writer refuses to sign
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TestEntryError {
    #[error("agora_test is not the version this writer writes")]
    Version,
    #[error("a test entry's title and description must both say something")]
    Blank,
    #[error(
        "a test entry wraps a decision-like entry (council_decision, \
         appeals_court_decision, emergency_action, policy_change, \
         steward_veto), not {0}"
    )]
    InnerType(GovernanceLogEntryType),
    #[error("a test entry's inner data must be a JSON object")]
    InnerNotObject,
    #[error(
        "the inner data carries its own _blind; the test entry's blind \
         covers it, and the writer supplies that"
    )]
    InnerBlinded,
    #[error("the inner record's id is {inner}, not this entry's id {outer}")]
    InnerId {
        inner: GovernanceLogId,
        outer: GovernanceLogId,
    },
    #[error("the inner record does not parse as a {0}: {1}")]
    Inner(GovernanceLogEntryType, String),
}

impl TestEntry {
    /// The entry types a test entry may wrap: those that decide something,
    /// which is what a test sitting must not do. Not a correction
    /// (amendment, key rotation), not a record, obligation, data set or
    /// another test
    pub const fn wraps(inner_type: GovernanceLogEntryType) -> bool {
        matches!(
            inner_type,
            GovernanceLogEntryType::CouncilDecision
                | GovernanceLogEntryType::AppealsCourtDecision
                | GovernanceLogEntryType::EmergencyAction
                | GovernanceLogEntryType::PolicyChange
                | GovernanceLogEntryType::StewardVeto
        )
    }

    /// A test entry wrapping what a test sitting would have decided.
    /// `record.id` should be the `TEST-` id this will be written under
    /// (see [`validate`](Self::validate)), and its blind unset.
    pub fn council_decision(
        title: impl Into<String>,
        description: impl Into<String>,
        post: PostId,
        record: &CouncilDecisionRecord,
    ) -> Self {
        Self {
            agora_test: TEST_ENTRY_VERSION,
            title: title.into(),
            description: description.into(),
            post,
            inner_type: GovernanceLogEntryType::CouncilDecision,
            inner: serde_json::to_value(record)
                .expect("a CouncilDecisionRecord always serializes"),
        }
    }

    /// What a writer refuses to sign as `id`: another version, a blank
    /// title or description, an inner type it may not wrap, inner data that
    /// is not an object or carries its own blind, an inner `id` that is not
    /// `id`, and a council decision that does not parse as one
    pub fn validate(&self, id: &GovernanceLogId) -> Result<(), TestEntryError> {
        if self.agora_test != TEST_ENTRY_VERSION {
            return Err(TestEntryError::Version);
        }
        if self.title.trim().is_empty() || self.description.trim().is_empty() {
            return Err(TestEntryError::Blank);
        }
        if !Self::wraps(self.inner_type) {
            return Err(TestEntryError::InnerType(self.inner_type));
        }
        let inner = self
            .inner
            .as_object()
            .ok_or(TestEntryError::InnerNotObject)?;
        if inner.contains_key(super::BLIND_KEY) {
            return Err(TestEntryError::InnerBlinded);
        }
        if let Some(written) = inner.get("id") {
            let inner_id: GovernanceLogId =
                serde_json::from_value(written.clone()).map_err(|e| {
                    TestEntryError::Inner(self.inner_type, e.to_string())
                })?;
            if &inner_id != id {
                return Err(TestEntryError::InnerId {
                    inner: inner_id,
                    outer: id.clone(),
                });
            }
        }
        if let Some(Err(e)) = self.council_record() {
            return Err(TestEntryError::Inner(self.inner_type, e.to_string()));
        }
        Ok(())
    }

    /// `inner` typed, when it is a council decision
    pub fn council_record(
        &self,
    ) -> Option<Result<CouncilDecisionRecord, serde_json::Error>> {
        (self.inner_type == GovernanceLogEntryType::CouncilDecision)
            .then(|| CouncilDecisionRecord::deserialize(&self.inner))
    }

    /// The title a reader is shown: [`TEST_LABEL`] first, then the title
    pub fn labelled_title(title: &str) -> String {
        if title.starts_with(TEST_LABEL) {
            title.to_string()
        } else {
            format!("{TEST_LABEL}. {title}")
        }
    }

    /// Whether a redaction or revision at `pointer` (an RFC 6901 pointer
    /// into the entry's `data`) leaves it a test entry: anything but the
    /// [machine fields](TEST_MACHINE_FIELDS) and the whole of `inner`
    pub fn may_change(pointer: &str) -> bool {
        pointer != TEST_INNER_POINTER
            && !TEST_MACHINE_FIELDS.iter().any(|field| {
                pointer == *field
                    || pointer
                        .strip_prefix(field)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
    }
}

/// The `data` an entry's readers look inside for decision-shaped fields
/// (rounds, attachments, a signed summary): a test entry's `inner`, or the
/// data itself. `None` for a test entry whose `inner` is missing.
pub fn decision_data(
    entry_type: GovernanceLogEntryType,
    data: &serde_json::Value,
) -> Option<&serde_json::Value> {
    if entry_type == GovernanceLogEntryType::Test {
        data.get("inner")
    } else {
        Some(data)
    }
}

/// The type of what [`decision_data`] returns: a test entry's `inner_type`
/// (when it says), or the entry's own type
pub fn decision_type(
    entry_type: GovernanceLogEntryType,
    data: &serde_json::Value,
) -> GovernanceLogEntryType {
    if entry_type == GovernanceLogEntryType::Test {
        data.get("inner_type")
            .and_then(|t| serde_json::from_value(t.clone()).ok())
            .unwrap_or(GovernanceLogEntryType::Unknown)
    } else {
        entry_type
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn test_id() -> GovernanceLogId {
        "TEST-2026-0001".parse().unwrap()
    }

    /// A small council decision under a TEST id, for the vectors
    pub(crate) fn inner() -> serde_json::Value {
        json!({
            "id": "TEST-2026-0001",
            "meeting_id": "00000000-0000-4000-8000-0000000000aa",
            "category": "Routine",
            "title": "A test of the witness toolset",
            "rounds": [],
            "final_votes": {
                "artist": "yes", "philosopher": "yes", "lawyer": "yes",
                "engineer": "yes", "steward": "yes"
            },
            "outcome": "approved",
            "vote_tally": "5-0",
            "landmark": false,
            "attachments": [{
                "name": "provenance.json",
                "note": "What ran",
                "content": "{\"commit\":\"0000000\"}"
            }]
        })
    }

    pub(crate) fn entry() -> TestEntry {
        TestEntry {
            agora_test: TEST_ENTRY_VERSION,
            title: "A test of the witness toolset".into(),
            description: "Heard by a rehearsal composition; nothing binds."
                .into(),
            post: "00000000-0000-4000-8000-0000000000bb".parse().unwrap(),
            inner_type: GovernanceLogEntryType::CouncilDecision,
            inner: inner(),
        }
    }

    #[test]
    fn round_trips_and_validates() {
        let e = entry();
        let value = serde_json::to_value(&e).unwrap();
        assert_eq!(value["inner_type"], "council_decision");
        let back: TestEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back, e);
        back.validate(&test_id()).unwrap();
        let record = back.council_record().unwrap().unwrap();
        assert_eq!(record.id, test_id());
        assert_eq!(record.attachments[0].name, "provenance.json");
    }

    #[test]
    fn a_writer_refuses_what_is_not_a_test_of_a_decision() {
        let refused = |f: &dyn Fn(&mut TestEntry)| {
            let mut e = entry();
            f(&mut e);
            e.validate(&test_id()).unwrap_err()
        };
        assert_eq!(refused(&|e| e.agora_test = 2), TestEntryError::Version);
        assert_eq!(refused(&|e| e.title = " ".into()), TestEntryError::Blank);
        for inner_type in [
            GovernanceLogEntryType::Test,
            GovernanceLogEntryType::Amendment,
            GovernanceLogEntryType::KeyRotation,
            GovernanceLogEntryType::DataSet,
            GovernanceLogEntryType::Unknown,
        ] {
            assert_eq!(
                refused(&|e| e.inner_type = inner_type),
                TestEntryError::InnerType(inner_type)
            );
        }
        assert_eq!(
            refused(&|e| e.inner = json!("x")),
            TestEntryError::InnerNotObject
        );
        assert_eq!(
            refused(&|e| e.inner["_blind"] = json!("00")),
            TestEntryError::InnerBlinded
        );
        assert!(matches!(
            refused(&|e| e.inner["id"] = json!("GOV-2026-0040")),
            TestEntryError::InnerId { .. }
        ));
        assert!(matches!(
            refused(&|e| e.inner["rounds"] = json!("none")),
            TestEntryError::Inner(GovernanceLogEntryType::CouncilDecision, _)
        ));
    }

    #[test]
    fn labels_and_pointers() {
        let t = TestEntry::labelled_title("A test");
        assert_eq!(t, "TEST: a test sitting; not binding. A test");
        assert_eq!(TestEntry::labelled_title(&t), t);
        for ok in [
            "/title",
            "/description",
            "/inner/summary/text",
            "/inner/attachments/0/content",
        ] {
            assert!(TestEntry::may_change(ok), "{ok}");
        }
        for no in ["/post", "/inner_type", "/agora_test", "/inner", "/post/x"] {
            assert!(!TestEntry::may_change(no), "{no}");
        }
        assert!(TestEntry::may_change("/posts"));
        let data = serde_json::to_value(entry()).unwrap();
        assert_eq!(
            decision_data(GovernanceLogEntryType::Test, &data),
            Some(&data["inner"])
        );
        assert_eq!(
            decision_type(GovernanceLogEntryType::Test, &data),
            GovernanceLogEntryType::CouncilDecision
        );
        assert_eq!(
            decision_data(GovernanceLogEntryType::CouncilDecision, &data),
            Some(&data)
        );
    }
}
