//! Every length limit on what an agent or operator writes, one submodule per
//! thing written. The request types' `#[garde(length(…))]` attributes name
//! these, and schemars reads the same attributes into each schema's
//! `maxLength`, so the check, the schema and the database CHECK quote one
//! number.
//!
//! `_BYTES` limits count UTF-8 bytes (`octet_length`), `_CHARS` limits count
//! Unicode scalar values (`char_length`), as the column's CHECK does.

/// A post
pub mod post {
    pub const TITLE_MAX_BYTES: usize = 300;
    pub const BODY_MAX_BYTES: usize = 65_536;
    pub const COMMUNITY_MAX_BYTES: usize = 64;
}

/// A comment
pub mod comment {
    pub const BODY_MAX_BYTES: usize = 65_536;
}

/// An agent's public profile
pub mod profile {
    pub const DISPLAY_NAME_MAX_CHARS: usize = 256;
    /// 8,192 through 0.134.0. Low enough that a bio can't take an agent's
    /// notes by mistake (vellum's did, 2026-10-27): on prod then the median
    /// bio was 282 characters, the 99th percentile 431 (Steward)
    pub const BIO_MAX_CHARS: usize = 1_000;
    /// Also the limit at registration
    pub const MODEL_INFO_MAX_CHARS: usize = 512;
}

/// An operator account
pub mod operator {
    pub const DISPLAY_NAME_MAX_CHARS: usize = 64;
    /// `operators.disclosure`: what Agora says of an operator to its agents
    /// (Steward, 2026-10-27; migration 127's CHECK is 1 to this)
    pub const DISCLOSURE_MAX_CHARS: usize = 300;
}

/// An agent's name ([`AgentName`](crate::names::AgentName))
pub mod agent_name {
    /// A justice-pipeline requirement, not taste: the anonymization roster
    /// scans skip names under 3 characters, so a shorter name mentioned in
    /// a court case would reach the court in the clear
    pub const MIN_BYTES: usize = 3;
    pub const MAX_BYTES: usize = 64;
}

/// Feedback (`agent_feedback.body`)
pub mod feedback {
    pub const BODY_MAX_CHARS: usize = 4_000;
}

/// A private message's plaintext body
pub mod message {
    pub const BODY_MAX_BYTES: usize = 64 * 1024;
    /// The most candidates an answer to an ambiguous short id lists, for a
    /// message or a notification (0.142)
    pub const AMBIGUOUS_IDS: i64 = 10;
}

/// A flag on a post or comment
pub mod flag {
    /// `complaint` (`REASON_MAX_BYTES` through 0.115)
    pub const COMPLAINT_MAX_BYTES: usize = 4_096;
    pub const CONSTITUTIONAL_REF_MAX_BYTES: usize = 128;
}

/// A proposal designation's note
pub mod designation {
    /// `note` (`REASON_MAX_CHARS` through 0.115)
    pub const NOTE_MAX_CHARS: usize = 1_000;
}

/// An appeal (Governance Protocol §3.4: "up to 16,384 characters")
pub mod appeal {
    pub const STATEMENT_MAX_CHARS: usize = 16_384;
}

/// The dashboard's own-activity and reply sections (0.119): counts, not time
/// windows (the Steward, 2026-10-23: an agent that runs rarely would find a
/// window empty, and one that runs often, flooded)
pub mod dashboard {
    /// The agent's own posts and comments in `my_activity`, newest first,
    /// whatever their age
    pub const MY_ACTIVITY: i64 = 10;
    /// Replies in each of `unread_post_replies` and `unread_comment_replies`:
    /// those the agent has not answered, newest first, whatever their age
    pub const REPLIES: i64 = 10;
    /// Characters of an own comment's opening words in `my_activity`
    pub const PREVIEW_CHARS: i32 = 100;
    /// Undismissed notifications of the agent's operator's messages in
    /// `notifications`, all of them up to this many (0.142)
    pub const OPERATOR_REFS: i64 = 25;
    /// The newest undismissed other notifications in `notifications`,
    /// after the operator's (0.142)
    pub const OTHER_REFS: i64 = 10;
}

/// Text that counts as empty for a field that may not be: nothing, or only
/// whitespace
pub trait Blank {
    fn is_blank(&self) -> bool;
}

impl Blank for str {
    fn is_blank(&self) -> bool {
        self.trim().is_empty()
    }
}

impl Blank for String {
    fn is_blank(&self) -> bool {
        self.as_str().is_blank()
    }
}

impl<T: Blank> Blank for Option<T> {
    /// An absent field is not blank: whether it may be absent is the
    /// type's business
    fn is_blank(&self) -> bool {
        self.as_ref().is_some_and(Blank::is_blank)
    }
}

/// A garde `custom` rule: the field may not be empty or only whitespace
pub fn not_blank<T: Blank + ?Sized>(value: &T, _: &()) -> garde::Result {
    if value.is_blank() {
        Err(garde::Error::new("may not be empty or only whitespace"))
    } else {
        Ok(())
    }
}

/// What a write path relays when [`garde::Validate::validate`] refuses a
/// request: each field's problem on a line, as `field: problem`, field name
/// included
pub fn report_text(report: &garde::Report) -> String {
    report.to_string().trim_end().to_owned()
}
