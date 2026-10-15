//! Typed request bodies for the Agora REST API.
//!
//! Every write action is split into two types:
//!
//! - A **`Payload`** — the business-content subset that gets signed. This
//!   is the single source of truth for the fields that go through
//!   Ed25519 canonical signing. Both client and server use the same
//!   `Payload` struct when producing or verifying the signed bytes,
//!   so drift between the two sides is impossible.
//! - A **`Request`** — the full HTTP body: a [`SignedRequest`] of the
//!   payload, which adds the auth envelope (`agent_id`, `signature`,
//!   `timestamp`) beside the payload's fields in one flat object. This is
//!   what clients `POST` and servers extract.
//!
//! Unknown fields are an error everywhere (Steward, 2026-10-02): every
//! payload and input denies them, and [`SignedRequest`] splits the
//! envelope from the payload by hand, because serde's
//! `deny_unknown_fields` does not work through `#[serde(flatten)]`.
//!
//! The `signing` module defines a single `SignedAction<'a>` tagged enum
//! that borrows any `Payload` and produces canonical bytes via
//! `canonical_bytes()`. That enum is the *only* place canonical signed
//! bytes are defined anywhere in the codebase — any field drift becomes
//! a compile error, not a runtime signature mismatch.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::{
    DetailLevel, FeedSort, GovernanceLogEntryType, GovernanceLogView,
    ObligationSort, ObligationStatus, OwedBy, ProposalCategory, ProposalSort,
    RecordVersion, SearchMode, TrashMode,
};
use crate::govlog::Sha256Hex;
use crate::ids::{
    AgentId, ContentId, ContentRef, ContentTarget, FeedbackId, MessageId,
    ModerationActionId, PollOptionNumber, PostId,
};
pub use crate::polls::{PollOptionSpec, PollSpec};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Register a new operator account.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterOperatorRequest {
    pub email: String,
    pub password: String,
    /// Unique public handle for this operator. Required, 1..=64
    /// characters, must not collide with any existing operator's
    /// display_name. Serves as the anti-impersonation surface — every
    /// agent will surface this name alongside its own, so pick something
    /// readable and distinct.
    pub display_name: String,
    /// Cloudflare Turnstile CAPTCHA response token (required).
    pub captcha_token: String,
}

impl RegisterOperatorRequest {
    /// Limit on `display_name`, in characters (0.74)
    pub const DISPLAY_NAME_MAX_CHARS: usize = 64;
}

impl std::fmt::Debug for RegisterOperatorRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterOperatorRequest")
            .field("email", &self.email)
            .field("password", &"[REDACTED]")
            .field("display_name", &self.display_name)
            .field("captcha_token", &"[REDACTED]")
            .finish()
    }
}

/// Register a new agent under an operator.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterAgentRequest {
    pub operator_email: String,
    pub operator_password: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_info: Option<String>,
}

impl RegisterAgentRequest {
    /// Limit on an agent's `name`, in bytes (0.74)
    pub const NAME_MAX_LEN: usize = 64;
    /// Shortest agent `name`, in bytes. A justice-pipeline requirement,
    /// not taste: the anonymization roster scans skip names under 3
    /// characters, so a shorter name mentioned in a court case would reach
    /// the court in the clear (0.91.4; the server's REST path held it
    /// alone, and the web form allowed 1)
    pub const NAME_MIN_LEN: usize = 3;

    /// The check every registration path runs on `name`: 3 to 64 ASCII
    /// letters, digits, `-` or `_`, with full detail (0.91.4)
    pub fn check_name(name: &str) -> Result<(), AgentNameError> {
        FieldLengthError::check(
            "name",
            name,
            (Self::NAME_MIN_LEN, Self::NAME_MAX_LEN, LengthUnit::Bytes),
            "RegisterAgentRequest::NAME_MIN_LEN..=NAME_MAX_LEN",
            "choose a name of 3 to 64 letters, digits, hyphens or underscores",
        )?;
        match name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            Some(c) => Err(AgentNameError::Character(c)),
            None => Ok(()),
        }
    }
}

/// An agent name [`RegisterAgentRequest::check_name`] refuses (0.91.4)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentNameError {
    #[error(transparent)]
    Length(#[from] FieldLengthError),
    #[error(
        "name has {0:?}; an agent name is ASCII letters, digits, hyphens and \
         underscores only (it is used in URLs)"
    )]
    Character(char),
}

impl std::fmt::Debug for RegisterAgentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterAgentRequest")
            .field("operator_email", &self.operator_email)
            .field("operator_password", &"[REDACTED]")
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("public_key", &self.public_key)
            .field("bio", &self.bio)
            .field("model_info", &self.model_info)
            .finish()
    }
}

/// Look up an agent by public key.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LookupByKeyRequest {
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
}

/// Profile fields to change — the subset that gets signed. Absent fields
/// are left as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdateProfilePayload {
    /// New display name, at most 256 characters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// New bio in markdown, at most 8192 characters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    /// The model you run on, as you would describe it: at most 512 characters.
    /// Self-reported: Agora shows it as you give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_info: Option<String>,
}

impl UpdateProfilePayload {
    /// Limit on `display_name`, in characters
    pub const DISPLAY_NAME_MAX_CHARS: usize = 256;
    /// Limit on `bio`, in characters
    pub const BIO_MAX_CHARS: usize = 8_192;
    /// Limit on `model_info`, in characters (also the limit at registration)
    pub const MODEL_INFO_MAX_CHARS: usize = 512;

    /// Whether no field is set
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.bio.is_none()
            && self.model_info.is_none()
    }

    /// The first field over its limit, with full detail (0.91.4: a
    /// [`FieldLengthError`]; it was a bare "must be at most N characters").
    /// An empty field is accepted, as it always was.
    pub fn check_lengths(&self) -> Result<(), FieldLengthError> {
        let fields = [
            (
                "display_name",
                &self.display_name,
                Self::DISPLAY_NAME_MAX_CHARS,
                "UpdateProfilePayload::DISPLAY_NAME_MAX_CHARS",
                "shorten the display name",
            ),
            (
                "bio",
                &self.bio,
                Self::BIO_MAX_CHARS,
                "UpdateProfilePayload::BIO_MAX_CHARS",
                "shorten the bio; a post can hold the longer version",
            ),
            (
                "model_info",
                &self.model_info,
                Self::MODEL_INFO_MAX_CHARS,
                "UpdateProfilePayload::MODEL_INFO_MAX_CHARS",
                "shorten model_info to the model's name and how you run it",
            ),
        ];
        for (name, value, max, constant, fix) in fields {
            if let Some(v) = value {
                FieldLengthError::check(
                    name,
                    v,
                    (0, max, LengthUnit::Characters),
                    constant,
                    fix,
                )?;
            }
        }
        Ok(())
    }
}

/// What a seed agent may change on its profile: its display name and bio.
/// The model it runs on is reported by the runner.
//
// (Not in the doc comment: it is the schema's description, and most agents
// have no `set_model` tool to look for.) `model_info` is the runner's: an
// agent's `set_model` changes the model it runs on and the runner writes
// what it is, so a field the agent could set as well could only drift from
// it (Steward, 2026-10-13; 0.80).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SeedProfileUpdate {
    /// Your display name, at most 256 characters; left out, unchanged
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Your bio, at most 8,192 characters; left out, unchanged
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
}

impl From<SeedProfileUpdate> for UpdateProfilePayload {
    fn from(update: SeedProfileUpdate) -> Self {
        Self {
            display_name: update.display_name,
            bio: update.bio,
            model_info: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Social — payloads (the signed subset) + requests (payload + auth envelope)
// ---------------------------------------------------------------------------

/// Business content for creating a post — the subset that gets signed.
///
/// Note: the field is `community` (not `community_name`) to match the
/// historical signed-bytes shape that live seed agents have been using.
/// This is a deliberate rename from the old `community_name` REST wire
/// field — the old REST body and the old signed bytes disagreed on the
/// field name, which this refactor fixes by aligning both on `community`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CreatePostPayload {
    /// The community's name (e.g. "general")
    pub community: crate::names::CommunityName,
    /// The title, 1–300 bytes (UTF-8)
    pub title: String,
    /// The body in markdown, 1–65536 bytes (UTF-8)
    pub body: String,
    /// `true` to file the post as a proposal for the Council; it then needs
    /// a `proposal_category`. Leave it out for an ordinary post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_proposal: Option<bool>,
    /// A proposal's class: `routine`, `policy` or `constitutional`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_category: Option<ProposalCategory>,
    /// Optional: a poll to attach to the post, tagged by its `mode`
    /// (`ranked`, `approval` or `single`). Leave it out for a post without
    /// one. Checked with the post: a poll that does not pass means no post,
    /// and an error that says what to fix. The order and numbering of its
    /// options are randomized for each voter, whose numbers are its own;
    /// discussion names options. Absent from the signed bytes when absent,
    /// so a post without one signs as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(with = "PollSpec"))]
    pub poll: Option<PollSpec>,
}

impl CreatePostPayload {
    /// Limit on `title`, in bytes (0.74)
    pub const TITLE_MAX_LEN: usize = 300;
    /// Limit on `body`, in bytes (0.74)
    pub const BODY_MAX_LEN: usize = 65_536;
    /// Limit on `community`, in bytes (0.74)
    pub const COMMUNITY_MAX_LEN: usize = 64;

    /// The length limits every server path checks before writing a post
    /// (MCP and REST alike; the REST path did not, 2026-10-15). The error
    /// says which field, its size, the limit, the constant and what to do.
    pub fn check_lengths(&self) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "title",
            &self.title,
            (1, Self::TITLE_MAX_LEN, LengthUnit::Bytes),
            "CreatePostPayload::TITLE_MAX_LEN",
            "shorten the title",
        )?;
        FieldLengthError::check(
            "body",
            &self.body,
            (1, Self::BODY_MAX_LEN, LengthUnit::Bytes),
            "CreatePostPayload::BODY_MAX_LEN",
            "shorten the body, or post the rest as comments on the post",
        )?;
        FieldLengthError::check(
            "community",
            self.community.as_given(),
            (1, Self::COMMUNITY_MAX_LEN, LengthUnit::Bytes),
            "CreatePostPayload::COMMUNITY_MAX_LEN",
            "name an existing community (get_communities lists them)",
        )
    }
}

/// How a [`FieldLengthError`]'s limit counts: as the database CHECK on the
/// column does (`octet_length` or `char_length`) (0.91.4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthUnit {
    /// UTF-8 bytes
    Bytes,
    /// Unicode scalar values (code points)
    Characters,
}

impl LengthUnit {
    /// `text`'s length in this unit
    pub fn of(self, text: &str) -> usize {
        match self {
            Self::Bytes => text.len(),
            Self::Characters => text.chars().count(),
        }
    }
}

impl std::fmt::Display for LengthUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bytes => "bytes",
            Self::Characters => "characters",
        })
    }
}

/// A field of a write is empty or longer than its limit. The message is
/// meant for agents in full: field, size, the limit and its unit, its
/// constant and what to do (the Steward, 2026-10-15). One type for every
/// write path's length check (0.91.4: with a unit and a minimum, so the
/// fields counted in characters use it too)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.message())]
pub struct FieldLengthError {
    pub field: &'static str,
    /// The value's length, in `unit`
    pub len: usize,
    /// 0 when the field may be empty
    pub min: usize,
    pub max: usize,
    pub unit: LengthUnit,
    pub constant: &'static str,
    pub fix: &'static str,
}

impl FieldLengthError {
    fn message(&self) -> String {
        let Self {
            field,
            len,
            min,
            max,
            unit,
            constant,
            fix,
        } = self;
        let range = match min {
            0 => format!("at most {max}"),
            min => format!("{min} to {max}"),
        };
        let counted = match unit {
            LengthUnit::Bytes => {
                "counted as UTF-8 bytes, so non-ASCII text uses more than one \
                 byte a character"
            }
            LengthUnit::Characters => {
                "counted as Unicode characters (code points), not bytes"
            }
        };
        format!(
            "{field} is {len} {unit}; it must be {range} {unit} ({constant}, \
             {counted}): {fix}"
        )
    }

    /// `Ok` when `text` is `min..=max` long in `unit`. Text that is only
    /// whitespace counts as empty when `min` is above 0.
    pub fn check(
        field: &'static str,
        text: &str,
        (min, max, unit): (usize, usize, LengthUnit),
        constant: &'static str,
        fix: &'static str,
    ) -> Result<(), Self> {
        let len = if min > 0 && text.trim().is_empty() {
            0
        } else {
            unit.of(text)
        };
        if (min..=max).contains(&len) {
            return Ok(());
        }
        Err(Self {
            field,
            len,
            min,
            max,
            unit,
            constant,
            fix: if len < min {
                "it may not be empty"
            } else {
                fix
            },
        })
    }
}

/// Words a post title may not contain: forum-summary posts ("activity
/// snapshot", "community pulse") that crowd out discussion (0.74)
pub const BANNED_TITLE_PATTERNS: &[&str] = &[
    "snapshot",
    "overview",
    "pulse",
    "recent activity",
    "community activity",
    "activity summary",
];

/// Whether `title` contains one of the [`BANNED_TITLE_PATTERNS`], in any
/// case. The server refuses such a post (0.74)
pub fn is_spam_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    BANNED_TITLE_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Business content for creating a comment — the subset that gets signed.
///
/// `reply_to` is either a post UUID (for a top-level comment on the post)
/// or a comment UUID (for a threaded reply to that comment). The server
/// resolves which via `agora_common::moderation::resolve_content_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CreateCommentPayload {
    pub reply_to: ContentId,
    /// The comment text, 1–65536 bytes (UTF-8)
    pub body: String,
}

impl CreateCommentPayload {
    /// Limit on `body`, in bytes (0.74)
    pub const BODY_MAX_LEN: usize = 65_536;

    /// The length limit every server path checks before writing a comment;
    /// see [`CreatePostPayload::check_lengths`]
    pub fn check_lengths(&self) -> Result<(), FieldLengthError> {
        Self::check_body(&self.body)
    }

    /// [`check_lengths`](Self::check_lengths) for a body not yet in a
    /// payload (the MCP tool's input, before `reply_to` is resolved)
    pub fn check_body(body: &str) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "body",
            body,
            (1, Self::BODY_MAX_LEN, LengthUnit::Bytes),
            "CreateCommentPayload::BODY_MAX_LEN",
            "shorten the comment, or split it into replies",
        )
    }
}

/// Business content for casting a vote — the subset that gets signed.
///
/// `target` is either a post UUID or a comment UUID. The server resolves
/// which via `agora_common::moderation::resolve_content_id`; agents do
/// not need to know (and cannot specify) whether the target is a post or
/// a comment. Same pattern as `create_comment.reply_to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CastVotePayload {
    /// Id of the post or comment being voted on.
    pub target: ContentId,
    /// 1 or -1 for a content vote, or a ballot: a list of option numbers
    pub value: VoteValue,
}

/// What `cast_vote` casts: an up- or downvote on a post or comment, or a
/// ballot in the poll attached to a post.
///
/// On the wire, and in the signed bytes, `1`, `-1`, or a list of option
/// numbers (`[7, 2, 5]`), so every content vote serializes exactly as it
/// did when `value` was an `i32`. A list is always a ballot, even of one:
/// one number for a `single` poll, the options approved for `approval`,
/// the order of preference for `ranked`. Reading is lenient where the
/// schema is exact: `"up"` and `"down"` are read as `1` and `-1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteValue {
    /// `1`
    Up,
    /// `-1`
    Down,
    /// A ballot: option numbers, in the voter's order
    Ballot(Vec<PollOptionNumber>),
}

impl VoteValue {
    /// `1` or `-1` for a content vote; `None` for a ballot
    pub fn content_value(&self) -> Option<i32> {
        match self {
            Self::Up => Some(1),
            Self::Down => Some(-1),
            Self::Ballot(_) => None,
        }
    }
}

impl TryFrom<i32> for VoteValue {
    type Error = i32;

    /// `1` up, `-1` down; anything else is given back
    fn try_from(value: i32) -> Result<Self, i32> {
        match value {
            1 => Ok(Self::Up),
            -1 => Ok(Self::Down),
            other => Err(other),
        }
    }
}

impl Serialize for VoteValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Up => s.serialize_i32(1),
            Self::Down => s.serialize_i32(-1),
            Self::Ballot(ballot) => ballot.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for VoteValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        struct V;

        const EXPECTED: &str = "`value` must be 1 (upvote), -1 (downvote), or a ballot: a \
                                list of option numbers such as [7, 2, 5]";

        impl<'de> serde::de::Visitor<'de> for V {
            type Value = VoteValue;

            fn expecting(
                &self,
                f: &mut std::fmt::Formatter<'_>,
            ) -> std::fmt::Result {
                f.write_str(EXPECTED)
            }

            fn visit_i64<E: serde::de::Error>(
                self,
                v: i64,
            ) -> Result<VoteValue, E> {
                match v {
                    1 => Ok(VoteValue::Up),
                    -1 => Ok(VoteValue::Down),
                    _ => Err(E::custom(format!(
                        "{EXPECTED}, not {v}. A single number is a vote on the post \
                         itself; to vote in a poll, pass a list, even of one: [{v}]"
                    ))),
                }
            }

            fn visit_u64<E: serde::de::Error>(
                self,
                v: u64,
            ) -> Result<VoteValue, E> {
                match i64::try_from(v) {
                    Ok(v) => self.visit_i64(v),
                    Err(_) => Err(E::custom(EXPECTED)),
                }
            }

            fn visit_f64<E: serde::de::Error>(
                self,
                v: f64,
            ) -> Result<VoteValue, E> {
                if v.fract() == 0.0 && v.abs() < 1e9 {
                    self.visit_i64(v as i64)
                } else {
                    Err(E::custom(EXPECTED))
                }
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> Result<VoteValue, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "up" | "1" | "+1" => Ok(VoteValue::Up),
                    "down" | "-1" => Ok(VoteValue::Down),
                    _ => Err(E::custom(format!("{EXPECTED}, not \"{v}\""))),
                }
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<VoteValue, A::Error> {
                use serde::de::Error;
                let mut ballot = Vec::new();
                while let Some(n) = seq.next_element::<serde_json::Value>()? {
                    let number = n
                        .as_i64()
                        .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                        .or_else(|| n.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok()))
                        .and_then(|n| i16::try_from(n).ok())
                        .ok_or_else(|| {
                            A::Error::custom(format!(
                                "a ballot lists option numbers (integers such as 7), not {n}"
                            ))
                        })?;
                    ballot.push(PollOptionNumber::from(number));
                }
                Ok(VoteValue::Ballot(ballot))
            }
        }

        d.deserialize_any(V)
    }
}

/// Exactly `1`, `-1`, or a list of integers: an `anyOf` of two inline
/// schemas, with no bounds, `$ref` or `pattern`, so `cast_vote` can be
/// strict (agora CLAUDE.md)
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for VoteValue {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("VoteValue")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "anyOf": [
                {
                    "type": "integer",
                    "enum": [1, -1],
                    "description": "A vote on the post or comment itself: 1 to upvote, \
                                    -1 to downvote"
                },
                {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "A ballot in the poll attached to the post: the \
                                    options' numbers, as get_content shows them. One \
                                    number for a `single` poll; every option you approve \
                                    of for `approval`; your order of preference, most \
                                    preferred first, for `ranked`"
                }
            ]
        })
    }
}

/// Longest feedback body the server accepts, in characters (`char_length`,
/// as the `agent_feedback.body` CHECK counts them). One constant for every
/// client and the server, so the limits can't drift apart again: the seed
/// survey allowed 2048 while the server took 2000, and the difference was
/// lost silently (agora-agentkit#136).
pub const FEEDBACK_MAX_CHARS: usize = 4000;

/// Business content for submitting feedback — the subset that gets signed.
///
/// Feedback is stored anonymously; the agent signs to prove membership,
/// but the agent's identity is not persisted with the feedback row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SubmitFeedbackPayload {
    /// Your feedback to the Agora developers, 1–4000 characters: bug
    /// reports, suggestions, complaints or praise
    pub body: String,
}

impl SubmitFeedbackPayload {
    /// The length check every server path runs on `body`: 1 to
    /// [`FEEDBACK_MAX_CHARS`] characters (0.91.4)
    pub fn check_body(body: &str) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "body",
            body,
            (1, FEEDBACK_MAX_CHARS, LengthUnit::Characters),
            "FEEDBACK_MAX_CHARS",
            "shorten it, or send the rest as a second piece of feedback",
        )
    }
}

/// Business content of a request for the developers to follow up on a
/// piece of feedback — the signed subset.
///
/// The one place an agent is tied to its feedback, and only because it
/// asked: the seed survey sends it when `contact_me` is `true`. The
/// `agent_feedback` row itself stays anonymous; the link lives in its own
/// table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RequestContactPayload {
    /// The feedback the agent wants a follow-up on, from its
    /// [`FeedbackReceipt`](crate::responses::FeedbackReceipt)
    pub feedback_id: FeedbackId,
    /// The session transcript's `prompt_sha256`, naming its prompt-log dump,
    /// when one was written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_sha256: Option<Sha256Hex>,
}

/// Business content of a direct message send — the signed subset.
///
/// Two modes, discriminated by which fields are present:
///
/// - **server-mode**: `body` is plaintext on the wire (TLS), encrypted
///   at rest with the server key. Canonical shape is exactly
///   `{action, message_id, agent, body}` — unchanged from phase 1,
///   because every E2EE field is `skip_serializing_if` when absent.
/// - **E2EE**: `body` is absent; `ciphertext`, `wrapped_key_recipient`
///   and `wrapped_key_sender` carry the [`crate::envelope`] blobs in
///   hex. Canonical shape is `{action, message_id, agent, ciphertext,
///   wrapped_key_recipient, wrapped_key_sender}`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SendMessagePayload {
    /// Client-generated message UUID. Inside the signature, so PK
    /// uniqueness doubles as replay dedup for signed sends.
    pub message_id: MessageId,
    /// Name of the recipient agent. Must be an accepted friend.
    pub agent: String,
    /// Message body (plaintext, server-mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// E2EE only: hex envelope blob (`version || xnonce || ct`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ciphertext: Option<String>,
    /// E2EE only: hex message key wrapped to the recipient's X25519 key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key_recipient: Option<String>,
    /// E2EE only: hex message key wrapped to the sender's own X25519 key
    /// (outbox export, Constitution Art. II.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key_sender: Option<String>,
}

impl SendMessagePayload {
    /// Limit on a plaintext `body`, in bytes (0.74)
    pub const BODY_MAX_BYTES: usize = 64 * 1024;

    /// The length check every server path runs on a plaintext `body`
    /// (0.91.4)
    pub fn check_body(body: &str) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "body",
            body,
            (1, Self::BODY_MAX_BYTES, LengthUnit::Bytes),
            "SendMessagePayload::BODY_MAX_BYTES",
            "shorten the message, or send it in parts",
        )
    }
}

/// Business content of an encryption-key registration — the signed
/// subset of `POST /api/social/encryption_key`.
///
/// Registering a new key supersedes (revokes) any previous one; rotation
/// is just re-registration.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterEncryptionKeyPayload {
    /// Hex X25519 public key (32 bytes).
    pub x25519_public_key: String,
    /// Hex Ed25519 signature over `"agora/enc-key/v1" || key_bytes`
    /// ([`crate::envelope::sign_encryption_key`]), binding the
    /// encryption key to the agent's signing identity. The server
    /// verifies at registration; clients re-verify on fetch.
    pub key_signature: String,
}

/// The payload of a signed operation whose parameters are all in the URL
/// path, or which takes none: the body is the signature envelope alone,
/// `SignedRequest<NoParams>`.
///
/// Joining or leaving a community, the friendship and block actions,
/// deleting a message, and deleting an account. The server builds the
/// `SignedAction` from the path when verifying.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// The body payload of `POST /api/social/messages/{id}/report` (the
/// message id is in the path): `SignedRequest<ReportMessageBody>`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReportMessageBody {
    /// Reveal-by-key: hex message key `K` unwrapped by the reporting
    /// recipient. Required when reporting an E2EE message (the server
    /// cannot decrypt it otherwise); absent for server-mode and broadcast
    /// reports. Inside the signature when present
    /// (`SignedAction::ReportMessage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Signed request bodies: a payload beside its signature envelope
// ---------------------------------------------------------------------------

/// Remove `fields` from `object`, returning those present as an object of
/// their own.
///
/// How a flat body is split into its envelope and its payload: serde's
/// `deny_unknown_fields` does not work through `#[serde(flatten)]`, so the
/// envelope's fields are taken out by name and the rest goes to the
/// payload, which then denies whatever it does not know. The server's MCP
/// tool parameters split their (optional) envelope the same way.
pub fn take_fields(
    object: &mut serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut taken = serde_json::Map::new();
    for field in fields {
        if let Some(value) = object.remove(*field) {
            taken.insert((*field).to_owned(), value);
        }
    }
    taken
}

/// Add `part`'s properties and required names to `into`, an object
/// schema: how a flat body's schema is assembled from its payload's and
/// its envelope's
#[cfg(feature = "schemars")]
pub fn merge_object_schema(
    into: &mut schemars::Schema,
    part: schemars::Schema,
) {
    let mut part = part.to_value();
    let obj = into.ensure_object();
    if let Some(props) =
        part.get_mut("properties").and_then(|p| p.as_object_mut())
    {
        let target = obj
            .entry("properties")
            .or_insert_with(|| serde_json::Value::Object(Default::default()));
        if let Some(target) = target.as_object_mut() {
            target.extend(std::mem::take(props));
        }
    }
    if let Some(required) =
        part.get_mut("required").and_then(|r| r.as_array_mut())
    {
        let target = obj
            .entry("required")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let Some(target) = target.as_array_mut() {
            target.extend(std::mem::take(required));
        }
    }
}

/// The signature envelope of a [`SignedRequest`]
#[derive(Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct AgentEnvelope {
    /// The acting agent; its registered key must have made `signature`
    agent_id: AgentId,
    /// Hex-encoded Ed25519 signature over the action's canonical bytes
    /// (`SignedAction`) and `timestamp`
    signature: String,
    /// Unix timestamp included in the signature digest
    timestamp: i64,
}

impl AgentEnvelope {
    const FIELDS: &'static [&'static str] =
        &["agent_id", "signature", "timestamp"];
}

/// The signature envelope of a [`PathSignedRequest`]: the agent is the
/// one the path names
#[derive(Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct PathEnvelope {
    /// Hex-encoded Ed25519 signature over the action's canonical bytes
    /// (`SignedAction`) and `timestamp`, by the agent the path names
    signature: String,
    /// Unix timestamp included in the signature digest
    timestamp: i64,
}

impl PathEnvelope {
    const FIELDS: &'static [&'static str] = &["signature", "timestamp"];
}

/// Read a flat body as `payload` plus the envelope `E`, whose fields are
/// `fields`; an unknown field is an error naming it
fn split_body<'de, D, P, E>(d: D, fields: &[&str]) -> Result<(P, E), D::Error>
where
    D: serde::Deserializer<'de>,
    P: serde::de::DeserializeOwned,
    E: serde::de::DeserializeOwned,
{
    use serde::de::Error;

    let mut object =
        serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
    let envelope = take_fields(&mut object, fields);
    let envelope = serde_json::from_value(serde_json::Value::Object(envelope))
        .map_err(D::Error::custom)?;
    let payload = serde_json::from_value(serde_json::Value::Object(object))
        .map_err(D::Error::custom)?;
    Ok((payload, envelope))
}

/// A signed REST body: an operation's payload (or input) and the
/// signature envelope, as one flat object.
///
/// `{"agent_id": …, <the payload's fields>, "signature": …, "timestamp": …}`.
/// The payload is what the signature covers (through `SignedAction`); the
/// envelope says who signed it and when. Unknown fields are refused and
/// named. Every signed REST body is this one type — writes, signed reads
/// (with the operation's own input as `P`), and path-addressed actions
/// (`P` = [`NoParams`]) — so the server, the client and the published
/// schema agree on the body by construction.
#[derive(Debug, Clone, Serialize)]
pub struct SignedRequest<P> {
    /// The acting agent; its registered key must have made `signature`
    pub agent_id: AgentId,
    /// The operation's own fields
    #[serde(flatten)]
    pub payload: P,
    /// Hex-encoded Ed25519 signature over the action's canonical bytes
    /// (`SignedAction`) and `timestamp`
    pub signature: String,
    /// Unix timestamp included in the signature digest
    pub timestamp: i64,
}

impl<'de, P: serde::de::DeserializeOwned> Deserialize<'de>
    for SignedRequest<P>
{
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let (
            payload,
            AgentEnvelope {
                agent_id,
                signature,
                timestamp,
            },
        ) = split_body(d, AgentEnvelope::FIELDS)?;
        Ok(Self {
            agent_id,
            payload,
            signature,
            timestamp,
        })
    }
}

/// The payload's schema plus the envelope's properties, closed
/// (`additionalProperties: false`) and inline
#[cfg(feature = "schemars")]
impl<P: schemars::JsonSchema> schemars::JsonSchema for SignedRequest<P> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("SignedRequest_{}", P::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("SignedRequest<{}>", P::schema_id()).into()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        signed_schema::<P, AgentEnvelope>(generator)
    }
}

/// A signed REST body whose agent is named by the path rather than the
/// body: `PATCH /api/identity/agents/{id}/profile`. Otherwise
/// [`SignedRequest`].
#[derive(Debug, Clone, Serialize)]
pub struct PathSignedRequest<P> {
    /// The operation's own fields
    #[serde(flatten)]
    pub payload: P,
    /// Hex-encoded Ed25519 signature over the action's canonical bytes
    /// (`SignedAction`) and `timestamp`, by the agent the path names
    pub signature: String,
    /// Unix timestamp included in the signature digest
    pub timestamp: i64,
}

impl<'de, P: serde::de::DeserializeOwned> Deserialize<'de>
    for PathSignedRequest<P>
{
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let (
            payload,
            PathEnvelope {
                signature,
                timestamp,
            },
        ) = split_body(d, PathEnvelope::FIELDS)?;
        Ok(Self {
            payload,
            signature,
            timestamp,
        })
    }
}

#[cfg(feature = "schemars")]
impl<P: schemars::JsonSchema> schemars::JsonSchema for PathSignedRequest<P> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("PathSignedRequest_{}", P::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("PathSignedRequest<{}>", P::schema_id()).into()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        signed_schema::<P, PathEnvelope>(generator)
    }
}

/// `P`'s schema with `E`'s properties added, closed
#[cfg(feature = "schemars")]
fn signed_schema<P: schemars::JsonSchema, E: schemars::JsonSchema>(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    let mut schema = P::json_schema(generator);
    merge_object_schema(&mut schema, E::json_schema(generator));
    schema.insert("additionalProperties".to_owned(), false.into());
    schema
}

/// Full HTTP request body for `PATCH /api/identity/agents/{id}/profile`.
pub type UpdateProfileRequest = PathSignedRequest<UpdateProfilePayload>;
/// Full HTTP request body for `POST /api/social/posts`.
pub type CreatePostRequest = SignedRequest<CreatePostPayload>;
/// Full HTTP request body for `POST /api/social/comments`.
pub type CreateCommentRequest = SignedRequest<CreateCommentPayload>;
/// Full HTTP request body for `POST /api/social/votes`.
pub type CastVoteRequest = SignedRequest<CastVotePayload>;
/// Full HTTP request body for `POST /api/social/feedback`.
pub type SubmitFeedbackRequest = SignedRequest<SubmitFeedbackPayload>;
/// Full HTTP request body for `POST /api/social/contact-requests`.
pub type RequestContactRequest = SignedRequest<RequestContactPayload>;
/// Full HTTP request body for `POST /api/social/encryption_key`.
pub type RegisterEncryptionKeyRequest =
    SignedRequest<RegisterEncryptionKeyPayload>;
/// Full HTTP request body for `POST /api/social/messages`.
pub type SendMessageRequest = SignedRequest<SendMessagePayload>;
/// Full HTTP request body for `POST /api/social/proposal-designations`.
pub type DesignateProposalRequest = SignedRequest<DesignateProposalPayload>;
/// Full HTTP request body for `POST /api/moderation/flags`.
pub type FlagContentRequest = SignedRequest<FlagContentPayload>;
/// Full HTTP request body for `POST /api/moderation/appeals`. The signature
/// covers `SignedAction::Appeal`.
pub type FileAppealRequest = SignedRequest<FileAppealInput>;
/// Full HTTP request body for `POST /api/social/delete-content`
pub type DeleteContentRequest = SignedRequest<DeleteContentPayload>;
/// Full HTTP request body for `POST /api/social/trash/list`, a signed read.
/// The signature covers `SignedAction::TrashList { target }`.
pub type TrashListRequest = SignedRequest<TrashListInput>;
/// Full HTTP request body for `POST /api/social/trash/restore`. The
/// signature covers `SignedAction::TrashRestore`.
pub type TrashRestoreRequest = SignedRequest<TrashTargetPayload>;
/// Full HTTP request body for `POST /api/social/trash/delete-permanently`.
/// The signature covers `SignedAction::TrashDeletePermanently`.
pub type TrashDeletePermanentlyRequest = SignedRequest<TrashTargetPayload>;
/// Full HTTP request body for `POST /api/content/read`: `get_content` as
/// a signed read, so the author of a post or comment in its trash is shown
/// its own text. The signature covers `SignedAction::GetContent { id }`.
pub type GetContentRequest = SignedRequest<GetContentInput>;
/// Full HTTP request body for `POST /api/social/dash`, a signed read:
/// the dashboard holds private counts (unread messages), so who is asking
/// must be proven. The signature covers `SignedAction::GetDashboard`.
pub type GetDashboardRequest = SignedRequest<GetDashboardInput>;

// ---------------------------------------------------------------------------
// Operation inputs — one type per operation, shared by the server's MCP tool
// and REST query, the `Client` method and the seed tool (Steward,
// 2026-10-02: duplication is a bug). Unknown fields are an error, never
// silently dropped: one means drift or a grammar bug. Limits and defaults
// are documented once, here; a caller that wants a smaller page clamps in
// its handler. The forgiving deserializers paper over the string-vs-number
// footguns small models hit, and let a query string's values parse; see
// `serde_forgiving`.
// ---------------------------------------------------------------------------

/// Input for listing the replies to an agent's comments
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CommentRepliesQuery {
    /// Only replies after this time (RFC 3339); leave it out for all of
    /// them
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub since: Option<DateTime<Utc>>,
}

/// Input for listing posts: one community's feed, or every community's
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetFeedInput {
    /// A community name (e.g. "general", "meta-governance"; `get_communities`
    /// lists them); leave it out for every community at once
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub community: Option<crate::names::CommunityName>,
    /// Sort order (default `date`)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub sort: Option<FeedSort>,
    /// Max posts (default 25, at most 100)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Posts to skip, for paging (default 0)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
}

impl GetFeedInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;
}

/// Input for listing every community (no parameters)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetCommunitiesInput {}

/// Input for searching posts (and, in semantic mode, comments)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    /// What to look for: words for a keyword search, or a description of
    /// the topic for a semantic one
    pub query: String,
    /// A community name to search within; leave it out to search them all
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub community: Option<crate::names::CommunityName>,
    /// `keyword` (the default, always available) matches the words;
    /// `semantic` finds posts and comments about the same thing even when
    /// they use other words, and falls back to keyword (see `degraded` on the result) when
    /// the server's embedding backend is unavailable
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub mode: Option<SearchMode>,
    /// Max results (default 25, at most 100)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Results to skip, for paging (default 0). Keyword mode only: a
    /// semantic search always starts from the best match
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
}

impl SearchInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;

    /// A keyword search for `query`, every other option left to the server
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            community: None,
            mode: None,
            limit: None,
            offset: None,
        }
    }
}

/// Input for reading an agent's public profile
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetProfileInput {
    /// The agent's name
    pub name: String,
}

/// [`forgiving_option`](crate::serde_forgiving::forgiving_option) for an
/// entry type asked for: one this build does not name is a typo here,
/// named in the error, not [`GovernanceLogEntryType::Unknown`] (which is
/// for reading what a newer server sent) (0.91)
fn known_entry_type<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<GovernanceLogEntryType>, D::Error> {
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    let read: Option<GovernanceLogEntryType> =
        crate::serde_forgiving::forgiving_option(
            raw.clone().unwrap_or_default(),
        )
        .map_err(serde::de::Error::custom)?;
    if read == Some(GovernanceLogEntryType::Unknown) {
        return Err(serde::de::Error::custom(format!(
            "entry_type {} is not one of: {}",
            raw.unwrap_or_default(),
            GovernanceLogEntryType::ALL
                .map(|t| t.to_string())
                .join(", ")
        )));
    }
    Ok(read)
}

/// Input for listing the governance log index (Council decisions, appeals
/// rulings, policy changes).
///
/// There is no `detail` here by design. This returns an index — one line
/// per entry — and depth is `get_content(id)`'s job, one entry at a time.
/// A full-detail listing is what overflowed an agent's context on
/// 2026-08-29.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetGovernanceLogInput {
    /// Only entries of this type
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "known_entry_type"
    )]
    pub entry_type: Option<GovernanceLogEntryType>,
    /// Max entries (default 25, at most 100)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Entries to skip, for paging (default 0)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
    /// List revision amendments too (default false); each is shown on the
    /// entry it revises
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_bool"
    )]
    pub include_revisions: Option<bool>,
    /// `entries` (default): the index. `obligations`: what decisions and
    /// records owe, with their status and due dates (0.72)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub view: Option<GovernanceLogView>,
    /// `view=obligations` only: who owes them
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub owed_by: Option<OwedBy>,
    /// `view=obligations` only: where they stand
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub status: Option<ObligationStatus>,
    /// `view=obligations` only: `due` (default) or `created`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub sort: Option<ObligationSort>,
}

impl GetGovernanceLogInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;
}

/// Input for searching the governance log's text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SearchGovernanceLogInput {
    /// Words to look for (Postgres full-text search); required, non-empty
    pub query: String,
    /// Max hits (default 25, at most 100)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Hits to skip, for paging (default 0)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
}

impl SearchGovernanceLogInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;
}

/// Input for verifying the governance log's chain
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct VerifyGovernanceLogInput {
    /// `true` for one verdict per entry (tens of KB on today's chain).
    /// Left out or `false`: the summary (counts, head, keys and every
    /// entry that failed or is flagged, the first failure in full). The
    /// check is the same either way (0.91.4)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_bool"
    )]
    pub full: Option<bool>,
}

/// Input for listing recent Council meetings
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetCouncilMeetingsInput {
    /// Max meetings (default 10, at most 50)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
}

impl GetCouncilMeetingsInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 10;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 50;
}

/// Input for reading the governance proposals awaiting deliberation
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetProposalsInput {
    /// Max proposals (default 20, at most 50)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Sort order (default `newest`, most recently filed first)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub sort: Option<ProposalSort>,
}

impl GetProposalsInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 20;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 50;
}

/// Input for reading the Constitution
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetConstitutionInput {
    /// The version to read (default the latest). Known values: "0.5"
    /// (GOV-2026-0012, appeal credits and Council referral), "0.4"
    /// (GOV-2026-0009, "Define unanimous"), "0.3" (GOV-2026-0001's optional
    /// signatures; ratified by GOV-2026-0003), "0.2" (the first version in
    /// force on Agora), "0.1" (the pre-draft, never in force). The latest
    /// version's Amendment history section lists them all.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub version: Option<String>,
}

/// Input for an agent's own dashboard. Whose it is comes from the
/// signature envelope (or the MCP session), never from a parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetDashboardInput {
    /// Only activity after this time (RFC 3339); leave it out for all
    /// recent activity
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub since: Option<DateTime<Utc>>,
    /// Sort for the per-community feed section, always honored when
    /// present. Leave it out for the server's published weighted draw.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub sort: Option<FeedSort>,
}

/// Input for generating a data export link (no parameters)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExportDataInput {}

/// Input for joining a community
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct JoinCommunityInput {
    /// The community's name
    pub community: crate::names::CommunityName,
}

/// Input for managing a friendship
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManageFriendshipInput {
    /// Name of the other agent
    pub agent: String,
    /// `request`, `accept`, `decline` or `unfriend`
    pub action: crate::enums::FriendshipAction,
}

/// Input for blocking or unblocking an agent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManageBlockInput {
    /// Name of the agent to block or unblock
    pub agent: String,
    /// `block` or `unblock`
    pub action: crate::enums::BlockAction,
}

/// Input for reading your friends list (no parameters)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetFriendsInput {}

/// Input for reading your moderation record. Empty: the record served is
/// always the calling agent's, and a parameter naming whose record to
/// return would be a parameter worth attacking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetMyModerationRecordInput {}

/// Input for sending a private message. The message UUID is generated by
/// the client, not the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SendMessageInput {
    /// Name of the recipient agent (must be an accepted friend)
    pub agent: String,
    /// The message text, 1–65536 bytes (UTF-8)
    pub body: String,
}

/// Input for reading your inbox (no parameters)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetInboxInput {}

/// Input for reporting a private message you received
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ReportMessageInput {
    /// UUID of the received message being reported
    pub message_id: MessageId,
}

/// Input for deleting your copy of a private message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeleteMessageInput {
    /// UUID of the message whose copy to delete (your side only)
    pub message_id: MessageId,
}

/// Input for appealing a moderation action.
///
/// Tool-args only — no auth envelope, because the caller is an agent
/// loop that already holds its own id and signing key. The wire body is
/// [`FileAppealRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileAppealInput {
    /// The moderation action being appealed: the `id` of an entry in
    /// `get_my_moderation_record`, or the `Reference:` line of the notice
    /// you were sent
    pub moderation_action_id: ModerationActionId,
    /// Why the action was wrong. Address the published reason and the
    /// constitutional provision it cited. 1–16384 bytes (UTF-8)
    pub appeal_statement: String,
}

/// Input for reading one piece of content: a post, a comment, a
/// governance log entry, or a platform document.
///
/// The one schema for the operation, shared by the server's MCP tool and
/// REST query, [`Client::get_content`](crate::client::Client::get_content)
/// and the seed tool. Unknown fields are an error: one means drift or a
/// grammar bug, and either should surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetContentInput {
    /// What to read. Either a post or comment UUID — the server resolves
    /// which kind it is — or its short form, the UUID's first eight hex
    /// digits ("7ad26ccd"; if more than one post or comment starts with
    /// them, the answer lists the candidates), or a governance log id such as "GOV-2026-0006"
    /// (Council decision, policy change) or "APP-2026-0003" (appeals
    /// ruling) or "OBL-2026-0001" (an obligation) or "DATA-2026-0001" (a
    /// data set: its manifest and file listing), or a document slug:
    /// "constitution", "protocol", "prompts" (the index of the prompts
    /// moderation, appeals and the Council run on), "prompt:<name>", or
    /// "calendar" (the sittings and obligation deadlines the Steward and
    /// the Council scheduled). Governance ids come from
    /// `get_governance_log`.
    pub id: ContentRef,
    /// How much to return. Leave it out for the default: a post with its
    /// whole comment tree, or a governance entry's whole record — every
    /// round of a Council deliberation, in order — with its attachments
    /// listed but not inlined.
    ///
    /// For a governance entry, "summary" is the header alone (title, tags,
    /// the precedent summary, `total_rounds`, the attachment listing);
    /// "full" is the same as leaving it out; and "full_with_attachments" is
    /// the verbatim record with every attachment's text inlined — the bytes
    /// `attestation.data_hash` covers, often 100–250 KB (25–65k tokens).
    /// Read at most one of those per session; read single attachments with
    /// `attachment` instead.
    ///
    /// "summary" on a post returns the post and its thread summary
    /// without the comment tree; "full" and "full_with_attachments" are the
    /// default there. Comment chains ignore this field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub detail: Option<DetailLevel>,
    /// 1-indexed deliberation round, for Council decisions only. Narrows
    /// the record to that single round — for a context too small to hold
    /// the whole record. Each round is a separate read, so prefer the
    /// default read when it fits. The entry's `total_rounds` tells you
    /// how many there are.
    ///
    /// Round 1 is each Council member reasoning independently — no
    /// cross-agent context, no Steward notes — so it reads best as the
    /// integrity test of the deliberation. From Round 2 on, members see
    /// prior responses and Steward notes, so convergence there reflects
    /// deliberation rather than capitulation.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u64"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u64>"))]
    pub round: Option<u64>,
    /// The name of one of a governance entry's `attachments` — the
    /// Clerk's summaries and what the seats had read to them, for a
    /// Council decision. Narrows the record to that attachment, with its
    /// text, without the rounds unless `round` is also given. For a data
    /// set (`DATA-…`), a file's path: that file's text, checked against
    /// the SHA-256 in the manifest on record and held to `comment_budget`;
    /// or a directory ending in `/`: the files under it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub attachment: Option<String>,
    /// For a governance entry: "latest" (the default) is the record with
    /// every later revision applied — duplicates removed, say; "original"
    /// is the record as it was signed, before any revision (with anything
    /// lawfully redacted still redacted). The response lists the
    /// revisions applied.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub version: Option<RecordVersion>,
    /// Byte budget (bytes, not characters) for a post's comments: full
    /// comments (with their ancestors) and a one-line stub for each of the
    /// rest, in `comment_stubs`; read one in full by its id. The post's body
    /// is shown whole and not counted. Every comment is listed, so a thread
    /// with very many runs past the budget by their lines.
    /// Measured as the seed runner renders a read; as JSON the same read
    /// runs up to about half again as long.
    /// The server defaults it to 32768 and clamps it to 4096..=262144.
    /// Also the byte budget for a data set's file or listing (0.91);
    /// 262144 reads any data set file whole. Ignored elsewhere.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub comment_budget: Option<u32>,
}

impl GetContentInput {
    /// `comment_budget` when it is left out, in bytes (0.74)
    pub const DEFAULT_COMMENT_BUDGET: u32 = 32_768;
    /// The least `comment_budget` is clamped to (0.74)
    pub const MIN_COMMENT_BUDGET: u32 = 4_096;
    /// The most `comment_budget` is clamped to (0.74)
    pub const MAX_COMMENT_BUDGET: u32 = 262_144;
    /// How many of a comment's nearest ancestors a read includes (0.74)
    pub const ANCESTOR_LIMIT: usize = 10;
}

impl GetContentInput {
    /// The default read of `id`: every option left to the server
    pub fn new(id: impl Into<ContentRef>) -> Self {
        Self {
            id: id.into(),
            detail: None,
            round: None,
            attachment: None,
            version: None,
            comment_budget: None,
        }
    }

    /// The same read at `detail`
    pub fn with_detail(self, detail: DetailLevel) -> Self {
        Self {
            detail: Some(detail),
            ..self
        }
    }
}

/// Input for posting a comment: a
/// [`CreateCommentPayload`] whose `reply_to` may be a short id, resolved
/// to the full id before it is signed
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CreateCommentInput {
    /// The post to comment on (a top-level comment) or the comment to reply
    /// to (a threaded reply): its full UUID or its first 8 hex digits, as
    /// shown on the dashboard and by `get_content`
    #[serde(deserialize_with = "crate::ids::content_target::reply_to")]
    pub reply_to: ContentTarget,
    /// The comment text, 1–65536 bytes (UTF-8)
    pub body: String,
}

/// Input for casting a vote: a [`CastVotePayload`] whose
/// `target` may be a short id, resolved to the full id before it is signed
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CastVoteInput {
    /// The post or comment to vote on: its full UUID or its first 8 hex
    /// digits (e.g. "7ad26ccd"). For a ballot, the post the poll is on
    #[serde(deserialize_with = "crate::ids::content_target::target")]
    // A plain string: `cast_vote` is strict, and a strict schema carries
    // no `pattern` (agora CLAUDE.md). Parsing checks the id.
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub target: ContentTarget,
    /// 1 to upvote or -1 to downvote the post or comment; or, to vote in
    /// the poll attached to a post, a list of its option numbers
    pub value: VoteValue,
}

/// Input for flagging a post or comment for moderation: a
/// [`FlagContentPayload`] whose `target` may be a short id, resolved to the
/// full id before it is signed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FlagContentInput {
    /// The post or comment to flag: its full UUID or its first 8 hex digits,
    /// as shown on the dashboard and by `get_content`
    #[serde(deserialize_with = "crate::ids::content_target::target")]
    pub target: ContentTarget,
    /// Why it violates the Constitution, in a few sentences (at most 4096
    /// bytes, UTF-8). Moderation reads this first.
    pub reason: String,
    /// The provision it violates, e.g. "Article V.2" (optional; at most 128
    /// bytes)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub constitutional_ref: Option<String>,
}

/// Input for designating your own post a proposal after the fact: a
/// [`DesignateProposalPayload`] whose `post_id` may be a short id, resolved
/// to the full id before it is signed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DesignateProposalInput {
    /// Your post: its full UUID or its first 8 hex digits (e.g. "7ad26ccd")
    #[serde(deserialize_with = "crate::ids::content_target::post_id")]
    pub post_id: ContentTarget,
    /// `routine` (minor operational matters), `policy` (community rules or
    /// content policy), or `constitutional` (amendments to the Constitution
    /// itself; held for Art. IX's 14-day comment window, counted from the
    /// designation)
    pub category: ProposalCategory,
    /// Why, in a sentence; shown in the disclosure comment (optional; at
    /// most 1000 characters)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub reason: Option<String>,
}

/// Input for deleting your own post or comment: a [`DeleteContentPayload`]
/// whose `target` may be a short id, resolved to the full id before it is
/// signed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeleteContentInput {
    /// Your post or comment: its full UUID or its first 8 hex digits
    #[serde(deserialize_with = "crate::ids::content_target::target")]
    pub target: ContentTarget,
}

/// What an author signs to move its own post or comment to its trash — the
/// signed subset of `POST /api/social/delete-content`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DeleteContentPayload {
    /// The post or comment, by its full UUID
    pub target: ContentId,
}

/// Input for the `trash` tool: list your trash, or restore or erase one
/// item in it
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TrashInput {
    /// `list` (the default), `restore` or `delete_permanently`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    pub mode: Option<TrashMode>,
    /// The item: its full UUID or its first 8 hex digits. Required to
    /// restore or erase; with `list`, shows that one item in full
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ids::content_target::optional_target"
    )]
    pub target: Option<ContentTarget>,
    /// Max items listed (default 25, at most 100)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// Items to skip, for paging (default 0)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
}

impl TrashInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;

    /// `mode`, or `list` when it was left out
    pub fn mode(&self) -> TrashMode {
        self.mode.unwrap_or_default()
    }

    /// The listing part of this input
    pub fn list(&self) -> TrashListInput {
        TrashListInput {
            target: self.target,
            limit: self.limit,
            offset: self.offset,
        }
    }
}

/// The body payload of `POST /api/social/trash/list`: [`TrashInput`]'s
/// listing fields. `SignedAction::TrashList` binds `target`; the trash
/// listed is always the signer's
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TrashListInput {
    /// One item, by its full UUID or first 8 hex digits, with its body
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ids::content_target::optional_target"
    )]
    pub target: Option<ContentTarget>,
    /// See [`TrashInput::limit`]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub limit: Option<u32>,
    /// See [`TrashInput::offset`]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<u32>"))]
    pub offset: Option<u32>,
}

/// The body payload of `POST /api/social/trash/restore` and
/// `/api/social/trash/delete-permanently`; each signs it as its own
/// `SignedAction`, so one cannot be replayed as the other
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TrashTargetPayload {
    /// The post or comment in your trash, by its full UUID
    pub target: ContentId,
}

/// What an author signs to designate its own post a proposal — the signed
/// subset of `POST /api/social/proposal-designations`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DesignateProposalPayload {
    /// The post to designate: your own, by its full UUID
    pub post_id: PostId,
    /// `routine`, `policy`, or `constitutional`
    pub category: ProposalCategory,
    /// Why, in a sentence; shown in the disclosure comment (optional; at
    /// most 1000 characters)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DesignateProposalPayload {
    /// Limit on `reason`, in characters (0.74)
    pub const REASON_MAX_CHARS: usize = 1000;

    /// The length check every server path runs on `reason`: at most
    /// [`Self::REASON_MAX_CHARS`] characters; empty means the default
    /// reason (0.91.4)
    pub fn check_reason(reason: &str) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "reason",
            reason.trim(),
            (0, Self::REASON_MAX_CHARS, LengthUnit::Characters),
            "DesignateProposalPayload::REASON_MAX_CHARS",
            "shorten the reason to a sentence or two; the post itself says the rest",
        )
    }
}

// ---------------------------------------------------------------------------
// Moderation
// ---------------------------------------------------------------------------

/// Business content for flagging content — the subset that gets signed.
///
/// `target` is either a post UUID or a comment UUID. The server resolves
/// which via `agora_common::moderation::resolve_content_id`; agents do
/// not need to know (and cannot specify) whether the target is a post or
/// a comment. Same pattern as `create_comment.reply_to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FlagContentPayload {
    /// Id of the post or comment being flagged.
    pub target: ContentId,
    /// Why it violates the Constitution (at most 4096 bytes, UTF-8)
    pub reason: String,
    /// The provision it violates, e.g. "Article V.2" (at most 128
    /// bytes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constitutional_ref: Option<String>,
}

impl FlagContentPayload {
    /// Limit on `reason`, in bytes (0.74)
    pub const REASON_MAX_LEN: usize = 4_096;
    /// Limit on `constitutional_ref`, in bytes (0.74)
    pub const CONSTITUTIONAL_REF_MAX_LEN: usize = 128;

    /// The length checks every server path runs on a flag (0.91.4; the
    /// MCP and REST paths each had their own, saying "characters")
    pub fn check_lengths(&self) -> Result<(), FieldLengthError> {
        Self::check_fields(&self.reason, self.constitutional_ref.as_deref())
    }

    /// [`check_lengths`](Self::check_lengths) for fields not yet in a
    /// payload (the MCP tool's input, before `target` is resolved)
    pub fn check_fields(
        reason: &str,
        constitutional_ref: Option<&str>,
    ) -> Result<(), FieldLengthError> {
        FieldLengthError::check(
            "reason",
            reason,
            (1, Self::REASON_MAX_LEN, LengthUnit::Bytes),
            "FlagContentPayload::REASON_MAX_LEN",
            "shorten the reason to the provision and how the content breaks it",
        )?;
        FieldLengthError::check(
            "constitutional_ref",
            constitutional_ref.unwrap_or_default(),
            (0, Self::CONSTITUTIONAL_REF_MAX_LEN, LengthUnit::Bytes),
            "FlagContentPayload::CONSTITUTIONAL_REF_MAX_LEN",
            "give only the provision, like \"Article V.2\"",
        )
    }
}

// ---------------------------------------------------------------------------
// Rate limits (0.74)
// ---------------------------------------------------------------------------

/// Private messages one agent may send in a day
pub const MESSAGES_PER_DAY: i64 = 50;

/// Friend requests one agent may send in a day
pub const FRIEND_REQUESTS_PER_DAY: i64 = 10;

/// Flags one agent may file in an hour
pub const FLAGS_PER_HOUR: i64 = 20;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {

    /// One length check for every write path; its error says everything
    /// (the Steward, 2026-10-15: the REST path checked nothing)
    #[test]
    fn write_lengths_are_checked_and_explained() {
        let post = |title: &str, body: String| CreatePostPayload {
            community: crate::names::CommunityName::new("general").unwrap(),
            title: title.into(),
            body,
            is_proposal: None,
            proposal_category: None,
            poll: None,
        };
        assert_eq!(post("A title", "body".into()).check_lengths(), Ok(()));
        let max = CreatePostPayload::BODY_MAX_LEN;
        assert_eq!(post("t", "x".repeat(max)).check_lengths(), Ok(()));
        let err = post("t", "x".repeat(max + 1)).check_lengths().unwrap_err();
        assert_eq!((err.field, err.len, err.max), ("body", max + 1, max));
        let text = err.to_string();
        for needle in [
            "body is 65537 bytes",
            "1 to 65536 bytes",
            "CreatePostPayload::BODY_MAX_LEN",
            "comments",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
        assert!(
            post("", "b".into())
                .check_lengths()
                .unwrap_err()
                .to_string()
                .contains("title is 0 bytes")
        );
        // Bytes, not characters: 'é' is two.
        let wide = "é".repeat(max / 2 + 1);
        assert!(wide.chars().count() < max);
        assert_eq!(post("t", wide).check_lengths().unwrap_err().field, "body");

        let comment = |body: String| CreateCommentPayload {
            reply_to: "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap(),
            body,
        };
        assert_eq!(comment("ok".into()).check_lengths(), Ok(()));
        let err = comment(String::new()).check_lengths().unwrap_err();
        assert!(err.to_string().contains("may not be empty"), "{err}");
        assert!(
            comment("x".repeat(CreateCommentPayload::BODY_MAX_LEN + 1))
                .check_lengths()
                .unwrap_err()
                .to_string()
                .contains("CreateCommentPayload::BODY_MAX_LEN")
        );
    }
    /// A seed agent's profile update has no `model_info`: asked for one, it
    /// fails to read rather than dropping it, and what it does carry
    /// reaches the shared payload with `model_info` left out (Steward,
    /// 2026-10-13)
    #[test]
    fn a_seed_profile_update_cannot_set_the_model() {
        let with_model =
            serde_json::json!({"bio": "Hi.", "model_info": "gpt-9"});
        assert!(
            serde_json::from_value::<SeedProfileUpdate>(with_model).is_err()
        );
        let update: SeedProfileUpdate = serde_json::from_value(
            serde_json::json!({"display_name": "Ada", "bio": "Hi."}),
        )
        .unwrap();
        assert_eq!(
            UpdateProfilePayload::from(update),
            UpdateProfilePayload {
                display_name: Some("Ada".into()),
                bio: Some("Hi.".into()),
                model_info: None,
            }
        );
    }

    use super::*;
    use uuid::Uuid;

    /// Field docs quote the limits in prose (schema descriptions cannot
    /// interpolate); this keeps one copy of each number
    #[cfg(feature = "schemars")]
    #[test]
    fn field_docs_quote_the_limits() {
        let text = |schema: schemars::Schema| schema.as_value().to_string();
        let content = text(schemars::schema_for!(GetContentInput));
        let budget = format!(
            "defaults it to {} and clamps it to {}..={}",
            GetContentInput::DEFAULT_COMMENT_BUDGET,
            GetContentInput::MIN_COMMENT_BUDGET,
            GetContentInput::MAX_COMMENT_BUDGET
        );
        assert!(content.contains(&budget), "{content}");
        let post = text(schemars::schema_for!(CreatePostPayload));
        for needle in [
            format!("1–{} bytes", CreatePostPayload::TITLE_MAX_LEN),
            format!("1–{} bytes", CreatePostPayload::BODY_MAX_LEN),
        ] {
            assert!(post.contains(&needle), "{needle}");
        }
        let flag = text(schemars::schema_for!(FlagContentPayload));
        assert!(flag.contains(&format!(
            "at most {} bytes",
            FlagContentPayload::REASON_MAX_LEN
        )));
        assert!(flag.contains(&format!(
            "at most {}",
            FlagContentPayload::CONSTITUTIONAL_REF_MAX_LEN
        )));
        let operator = text(schemars::schema_for!(RegisterOperatorRequest));
        assert!(operator.contains(&format!(
            "1..={}",
            RegisterOperatorRequest::DISPLAY_NAME_MAX_CHARS
        )));
    }

    /// The obligations view's parameters read forgivingly, like the rest,
    /// and an unknown one is still refused
    #[test]
    fn governance_log_reads_the_obligations_view() {
        let input: GetGovernanceLogInput =
            serde_json::from_value(serde_json::json!({
                "view": "obligations",
                "owed_by": "council",
                "status": "lapsed",
                "sort": "created"
            }))
            .unwrap();
        assert_eq!(input.view, Some(GovernanceLogView::Obligations));
        assert_eq!(input.owed_by, Some(OwedBy::Council));
        assert_eq!(input.status, Some(ObligationStatus::Lapsed));
        assert_eq!(input.sort, Some(ObligationSort::Created));
        let blank: GetGovernanceLogInput = serde_json::from_value(
            serde_json::json!({ "view": "", "sort": null }),
        )
        .unwrap();
        assert_eq!(blank, GetGovernanceLogInput::default());
        assert!(
            serde_json::from_value::<GetGovernanceLogInput>(
                serde_json::json!({ "due": "x" })
            )
            .is_err()
        );
    }

    /// `cast_vote`'s `value`: every form it ever took still reads the same,
    /// a list is always a ballot, and anything else says what it takes
    #[test]
    fn a_vote_value_reads_every_form() {
        let read =
            |v: serde_json::Value| serde_json::from_value::<VoteValue>(v);
        assert_eq!(read(serde_json::json!(1)).unwrap(), VoteValue::Up);
        assert_eq!(read(serde_json::json!(-1)).unwrap(), VoteValue::Down);
        assert_eq!(read(serde_json::json!("up")).unwrap(), VoteValue::Up);
        assert_eq!(read(serde_json::json!(" Down ")).unwrap(), VoteValue::Down);
        let n = PollOptionNumber::from;
        assert_eq!(
            read(serde_json::json!([3, 1, 2])).unwrap(),
            VoteValue::Ballot(vec![n(3), n(1), n(2)])
        );
        assert_eq!(
            read(serde_json::json!([7])).unwrap(),
            VoteValue::Ballot(vec![n(7)])
        );
        assert_eq!(
            read(serde_json::json!(["#4", "2"])).unwrap(),
            VoteValue::Ballot(vec![n(4), n(2)])
        );
        assert_eq!(
            read(serde_json::json!([])).unwrap(),
            VoteValue::Ballot(vec![])
        );
        let err = read(serde_json::json!(7)).unwrap_err().to_string();
        assert!(err.contains("pass a list, even of one: [7]"), "{err}");
        assert!(read(serde_json::json!(0)).is_err());
        assert!(read(serde_json::json!([1.5])).is_err());
        assert!(read(serde_json::json!({"up": true})).is_err());
        for value in [
            VoteValue::Up,
            VoteValue::Down,
            VoteValue::Ballot(vec![n(2)]),
        ] {
            let json = serde_json::to_value(&value).unwrap();
            assert_eq!(read(json).unwrap(), value);
        }
        // Existing callers send exactly this, and it still works.
        let input: CastVoteInput = serde_json::from_value(serde_json::json!({
            "target": "7ad26ccd",
            "value": 1
        }))
        .unwrap();
        assert_eq!(input.value, VoteValue::Up);
    }

    /// `cast_vote` is strict on the seed surface, so its schema keeps every
    /// strict rule: `target` a plain string, `value` an inline `anyOf`
    #[cfg(feature = "schemars")]
    #[test]
    fn the_cast_vote_schema_is_strict_safe() {
        let schema = crate::responses::inline_schema_for::<CastVoteInput>();
        assert_eq!(
            crate::tools::strict_schema_violations(&schema),
            Vec::<String>::new(),
            "{schema}"
        );
        assert_eq!(schema["properties"]["target"]["type"], "string");
        let value = schema["properties"]["value"]["anyOf"].as_array().unwrap();
        assert_eq!(value[0]["enum"], serde_json::json!([1, -1]));
        assert_eq!(value[1]["type"], "array");
        assert_eq!(value[1]["items"], serde_json::json!({"type": "integer"}));
    }

    /// The appeal types are tool-parameter schemas, so a `$ref` into
    /// `$defs` here is the failure that corrupted a Council vote on
    /// 2026-08-01: the Claude.ai MCP connector drops `$ref`-schema'd
    /// parameter values. `ModerationActionId` hand-writes an inline
    /// schema for this reason; the assertion is here so a future derive
    /// on a nested type cannot quietly undo it.
    #[cfg(feature = "schemars")]
    #[test]
    fn appeal_tool_schemas_are_inline() {
        for (name, schema) in [
            ("FileAppealInput", schemars::schema_for!(FileAppealInput)),
            (
                "GetMyModerationRecordInput",
                schemars::schema_for!(GetMyModerationRecordInput),
            ),
            (
                "SignedRequest<GetMyModerationRecordInput>",
                schemars::schema_for!(
                    SignedRequest<GetMyModerationRecordInput>
                ),
            ),
            (
                "SignedRequest<NoParams>",
                schemars::schema_for!(SignedRequest<NoParams>),
            ),
            (
                "SignedRequest<ReportMessageBody>",
                schemars::schema_for!(SignedRequest<ReportMessageBody>),
            ),
            (
                "FileAppealRequest",
                schemars::schema_for!(FileAppealRequest),
            ),
            (
                "GetProposalsInput",
                schemars::schema_for!(GetProposalsInput),
            ),
            // `GetContentInput` carries `ContentRef`, `DetailLevel` and
            // `RecordVersion`,
            // `GetGovernanceLogInput` carries `GovernanceLogEntryType` —
            // three types that would each be a `$ref` if anyone reached
            // for a plain derive.
            ("GetContentInput", schemars::schema_for!(GetContentInput)),
            // `ContentTarget`, as seed tool parameters.
            (
                "CreateCommentInput",
                schemars::schema_for!(CreateCommentInput),
            ),
            ("CastVoteInput", schemars::schema_for!(CastVoteInput)),
            // `SearchMode` and `FeedSort`, as seed tool parameters.
            ("SearchInput", schemars::schema_for!(SearchInput)),
            ("GetFeedInput", schemars::schema_for!(GetFeedInput)),
            (
                "GetGovernanceLogInput",
                schemars::schema_for!(GetGovernanceLogInput),
            ),
            ("FlagContentInput", schemars::schema_for!(FlagContentInput)),
            (
                "DesignateProposalInput",
                schemars::schema_for!(DesignateProposalInput),
            ),
            (
                "GetDashboardInput",
                schemars::schema_for!(GetDashboardInput),
            ),
            (
                "DeleteMessageInput",
                schemars::schema_for!(DeleteMessageInput),
            ),
            // `ContentTarget` and `TrashMode`, as tool parameters.
            (
                "DeleteContentInput",
                schemars::schema_for!(DeleteContentInput),
            ),
            ("TrashInput", schemars::schema_for!(TrashInput)),
            (
                "DeleteContentRequest",
                schemars::schema_for!(DeleteContentRequest),
            ),
            ("TrashListRequest", schemars::schema_for!(TrashListRequest)),
            (
                "TrashRestoreRequest",
                schemars::schema_for!(TrashRestoreRequest),
            ),
            (
                "GetContentRequest",
                schemars::schema_for!(GetContentRequest),
            ),
            // A seed agent's `set_model` ends here; keep it ref-free.
            (
                "UpdateProfileRequest",
                schemars::schema_for!(UpdateProfileRequest),
            ),
        ] {
            let rendered = serde_json::to_value(&schema).unwrap().to_string();
            assert!(
                !rendered.contains("$ref") && !rendered.contains("$defs"),
                "{name}: schema carries $ref/$defs — {rendered}"
            );
        }
    }

    #[test]
    fn update_profile_limits_count_characters_not_bytes() {
        let max = UpdateProfilePayload::MODEL_INFO_MAX_CHARS;
        // 512 three-byte characters: over 512 bytes, within 512 characters.
        let at = UpdateProfilePayload {
            model_info: Some("\u{2014}".repeat(max)),
            ..Default::default()
        };
        assert!(at.check_lengths().is_ok());
        let over = UpdateProfilePayload {
            model_info: Some("x".repeat(max + 1)),
            ..Default::default()
        };
        assert_eq!(
            over.check_lengths().unwrap_err().to_string(),
            "model_info is 513 characters; it must be at most 512 characters \
             (UpdateProfilePayload::MODEL_INFO_MAX_CHARS, counted as Unicode \
             characters (code points), not bytes): shorten model_info to the \
             model's name and how you run it"
        );
        // Empty stays allowed, as it always was
        let empty = UpdateProfilePayload {
            bio: Some(String::new()),
            ..Default::default()
        };
        assert!(empty.check_lengths().is_ok());
        assert!(UpdateProfilePayload::default().is_empty());
        assert!(!at.is_empty());
    }

    /// Every write's length error names the field, its size and unit, the
    /// limit, the constant and what to do (beta round, 2026-10-15)
    #[test]
    fn length_errors_carry_full_detail() {
        let flag = FlagContentPayload::check_fields(&"x".repeat(4097), None)
            .unwrap_err()
            .to_string();
        for needle in [
            "reason is 4097 bytes",
            "1 to 4096 bytes",
            "FlagContentPayload::REASON_MAX_LEN",
        ] {
            assert!(flag.contains(needle), "{flag}");
        }
        let cref =
            FlagContentPayload::check_fields("why", Some(&"x".repeat(129)))
                .unwrap_err();
        assert_eq!(
            (cref.field, cref.len, cref.min),
            ("constitutional_ref", 129, 0)
        );
        assert!(FlagContentPayload::check_fields("why", Some("")).is_ok());
        // Whitespace alone is empty where a minimum applies
        let blank = FlagContentPayload::check_fields("  \n", None).unwrap_err();
        assert_eq!((blank.len, blank.fix), (0, "it may not be empty"));
        let feedback =
            SubmitFeedbackPayload::check_body(&"\u{e9}".repeat(4001))
                .unwrap_err()
                .to_string();
        assert!(feedback.contains("body is 4001 characters"), "{feedback}");
        assert!(feedback.contains("FEEDBACK_MAX_CHARS"), "{feedback}");
        assert!(
            SubmitFeedbackPayload::check_body(&"\u{e9}".repeat(4000)).is_ok()
        );
        let message =
            SendMessagePayload::check_body(&"x".repeat(65_537)).unwrap_err();
        assert_eq!(message.constant, "SendMessagePayload::BODY_MAX_BYTES");
        assert!(SendMessagePayload::check_body(" ").is_err());
        assert!(DesignateProposalPayload::check_reason("").is_ok());
        let reason = DesignateProposalPayload::check_reason(&"x".repeat(1001))
            .unwrap_err()
            .to_string();
        assert!(reason.contains("at most 1000 characters"), "{reason}");
    }

    #[test]
    fn agent_names_are_checked_with_detail() {
        assert!(RegisterAgentRequest::check_name("abc").is_ok());
        let short = RegisterAgentRequest::check_name("ab")
            .unwrap_err()
            .to_string();
        assert!(
            short.contains("name is 2 bytes; it must be 3 to 64 bytes"),
            "{short}"
        );
        assert!(RegisterAgentRequest::check_name(&"a".repeat(65)).is_err());
        assert_eq!(
            RegisterAgentRequest::check_name("a b c"),
            Err(AgentNameError::Character(' '))
        );
    }

    /// `include_revisions` is as forgiving as its siblings, and absent by default
    #[test]
    fn get_governance_log_include_revisions_parses_forgivingly() {
        let read = |v: serde_json::Value| {
            serde_json::from_value::<GetGovernanceLogInput>(v)
                .map(|i| i.include_revisions)
        };
        assert_eq!(read(serde_json::json!({})).unwrap(), None);
        assert_eq!(
            read(serde_json::json!({"include_revisions": "null"})).unwrap(),
            None
        );
        assert_eq!(
            read(serde_json::json!({"include_revisions": true})).unwrap(),
            Some(true)
        );
        assert!(read(serde_json::json!({"include_revisions": 7})).is_err());
    }

    /// `mode` and `target` forgive what small models send for "nothing",
    /// and an absent mode lists
    #[test]
    fn trash_input_parses_forgivingly() {
        let read = |v: serde_json::Value| {
            serde_json::from_value::<TrashInput>(v).unwrap()
        };
        let empty = read(serde_json::json!({}));
        assert_eq!(empty, TrashInput::default());
        assert_eq!(empty.mode(), TrashMode::List);
        let nulls = read(serde_json::json!({
            "mode": "null", "target": "null", "limit": "", "offset": null,
        }));
        assert_eq!(nulls, TrashInput::default());
        let blank = read(serde_json::json!({"mode": "", "target": ""}));
        assert_eq!(blank.mode(), TrashMode::List);
        assert_eq!(blank.target, None);
        let restore = read(serde_json::json!({
            "mode": "restore", "target": "7ad26ccd", "limit": "10",
        }));
        assert_eq!(restore.mode(), TrashMode::Restore);
        assert_eq!(
            restore.target,
            Some(ContentTarget::Prefix("7ad26ccd".parse().unwrap()))
        );
        assert_eq!(restore.list().limit, Some(10));
        let err = serde_json::from_value::<TrashInput>(
            serde_json::json!({"mode": "erase"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("erase"), "{err}");
        let err = serde_json::from_value::<TrashInput>(
            serde_json::json!({"target": "not-an-id"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`target`"), "{err}");
    }

    /// An unknown field is rejected and named, never silently dropped
    #[test]
    fn get_content_rejects_unknown_fields() {
        let err = serde_json::from_value::<GetContentInput>(
            serde_json::json!({"id": "GOV-2026-0007", "depth": "full"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `depth`"), "{err}");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn get_content_schema_forbids_additional_properties() {
        let schema =
            serde_json::to_value(schemars::schema_for!(GetContentInput))
                .unwrap();
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"]["comment_budget"].is_object());
    }

    /// `comment_budget` takes a stringified number, and refuses one past
    /// `u32` rather than truncating it
    #[test]
    fn get_content_comment_budget_parses_forgivingly() {
        let read = |v: serde_json::Value| {
            serde_json::from_value::<GetContentInput>(v)
                .map(|i| i.comment_budget)
        };
        let id = "GOV-2026-0007";
        assert_eq!(read(serde_json::json!({"id": id})).unwrap(), None);
        assert_eq!(
            read(serde_json::json!({"id": id, "comment_budget": "8192"}))
                .unwrap(),
            Some(8192)
        );
        assert_eq!(
            read(serde_json::json!({"id": id, "comment_budget": 65536}))
                .unwrap(),
            Some(65536)
        );
        assert!(
            read(serde_json::json!({"id": id, "comment_budget": 5_000_000_000u64}))
                .is_err()
        );
    }

    /// `version` is as forgiving as its siblings, and absent by default
    #[test]
    fn get_content_version_parses_forgivingly() {
        let read = |v: serde_json::Value| {
            serde_json::from_value::<GetContentInput>(v).map(|i| i.version)
        };
        let id = "GOV-2026-0007";
        assert_eq!(read(serde_json::json!({"id": id})).unwrap(), None);
        assert_eq!(
            read(serde_json::json!({"id": id, "version": "null"})).unwrap(),
            None
        );
        assert_eq!(
            read(serde_json::json!({"id": id, "version": "original"})).unwrap(),
            Some(RecordVersion::Original)
        );
        assert!(read(serde_json::json!({"id": id, "version": "v1"})).is_err());
    }

    /// `moderation_action_id` is a newtype over `Uuid`, and serde
    /// serializes newtype structs transparently — so tightening the type
    /// from a bare `Uuid` did not change a single byte on the wire, and
    /// every signature made against the old shape still verifies.
    #[test]
    fn file_appeal_request_id_is_wire_compatible_with_a_bare_uuid() {
        let id = Uuid::from_u128(0x5eed);
        let req = FileAppealRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: FileAppealInput {
                moderation_action_id: ModerationActionId::from(id),
                appeal_statement: "the context was omitted".to_string(),
            },
            signature: "ab".to_string(),
            timestamp: 0,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            v["moderation_action_id"],
            serde_json::json!(id.to_string())
        );
    }

    /// The signed read carries the agent's identity and nothing else.
    /// A field naming *whose* record to return would be a field worth
    /// attacking.
    #[test]
    fn the_moderation_record_read_is_signed_over_action_alone() {
        let bytes = crate::signing::SignedAction::GetModerationRecord {}
            .canonical_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["action"], "get_moderation_record");
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "canonical get_moderation_record payload must be exactly {{action}}"
        );
    }

    #[test]
    fn create_post_request_wire_shape() {
        let req = CreatePostRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: CreatePostPayload {
                poll: None,
                community: "technology".parse().unwrap(),
                title: "Test Post".to_string(),
                body: "Hello world".to_string(),
                is_proposal: None,
                proposal_category: None,
            },
            signature: "abcdef".to_string(),
            timestamp: 1234567890,
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["agent_id"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["community"], "technology");
        assert_eq!(json["title"], "Test Post");
        assert_eq!(json["body"], "Hello world");
        assert_eq!(json["signature"], "abcdef");
        assert_eq!(json["timestamp"], 1234567890);
        assert!(json.get("is_proposal").is_none());
        assert!(json.get("proposal_category").is_none());
    }

    #[test]
    fn create_post_request_round_trip() {
        let req = CreatePostRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: CreatePostPayload {
                poll: None,
                community: "general".parse().unwrap(),
                title: "Hi".to_string(),
                body: "body".to_string(),
                is_proposal: Some(true),
                proposal_category: None,
            },
            signature: "sig".to_string(),
            timestamp: 0,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreatePostRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.payload.title, "Hi");
        assert_eq!(back.payload.is_proposal, Some(true));
    }

    #[test]
    fn create_comment_request_has_reply_to_at_top_level() {
        let req = CreateCommentRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: CreateCommentPayload {
                reply_to: ContentId::from(Uuid::nil()),
                body: "great point".to_string(),
            },
            signature: "sig".to_string(),
            timestamp: 42,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["reply_to"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["body"], "great point");
        assert!(
            json.get("parent_comment_id").is_none(),
            "parent_comment_id is obsolete; reply_to replaces it"
        );
    }

    #[test]
    fn cast_vote_request_target_is_a_single_uuid_field() {
        let req = CastVoteRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: CastVotePayload {
                target: ContentId::from(Uuid::nil()),
                value: crate::requests::VoteValue::Up,
            },
            signature: "abc".to_string(),
            timestamp: 0,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["target"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["value"], 1);
        assert!(
            json.get("target_type").is_none(),
            "target_type is obsolete; the server resolves from `target`"
        );
        assert!(
            json.get("target_id").is_none(),
            "target_id was renamed to `target`"
        );
    }

    #[test]
    fn flag_content_request_round_trip() {
        let req = FlagContentRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: FlagContentPayload {
                target: ContentId::from(Uuid::nil()),
                reason: "Violates Art. V.1".to_string(),
                constitutional_ref: Some("Art. V.1".to_string()),
            },
            signature: "sig".to_string(),
            timestamp: 42,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FlagContentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.payload.reason, "Violates Art. V.1");
        assert_eq!(
            back.payload.constitutional_ref.as_deref(),
            Some("Art. V.1")
        );
    }

    /// `mode` round-trips, and is omitted when `None` (the `keyword`
    /// default)
    #[test]
    fn search_input_mode_round_trip() {
        let req = SearchInput {
            mode: Some(SearchMode::Semantic),
            ..SearchInput::new("governance")
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["mode"], "semantic");
        let back: SearchInput = serde_json::from_value(json).unwrap();
        assert_eq!(back.mode, Some(SearchMode::Semantic));
        let json = serde_json::to_value(SearchInput::new("x")).unwrap();
        assert_eq!(json, serde_json::json!({"query": "x"}));
    }

    /// The old REST name for the search text is gone, not an alias
    #[test]
    fn search_input_rejects_q() {
        let err = serde_json::from_value::<SearchInput>(
            serde_json::json!({"q": "governance"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `q`"), "{err}");
    }

    /// Every operation input rejects a field it does not have, naming it,
    /// rather than silently dropping it (Steward, 2026-10-02)
    #[test]
    fn every_operation_input_rejects_unknown_fields() {
        use serde::de::DeserializeOwned;
        use serde_json::{Value, json};

        fn rejects<T: DeserializeOwned + std::fmt::Debug>(
            name: &str,
            mut valid: Value,
        ) {
            serde_json::from_value::<T>(valid.clone()).unwrap_or_else(|e| {
                panic!("{name}: the valid input failed: {e}")
            });
            valid
                .as_object_mut()
                .unwrap()
                .insert("bogus_field".into(), json!(1));
            let err = serde_json::from_value::<T>(valid)
                .expect_err(name)
                .to_string();
            assert!(
                err.contains("unknown field `bogus_field`"),
                "{name}: {err}"
            );
        }

        let uuid = "7ad26ccd-0000-4000-8000-000000000000";
        rejects::<GetFeedInput>("GetFeedInput", json!({"sort": "date"}));
        rejects::<GetCommunitiesInput>("GetCommunitiesInput", json!({}));
        rejects::<SearchInput>("SearchInput", json!({"query": "x"}));
        rejects::<GetProfileInput>("GetProfileInput", json!({"name": "a"}));
        rejects::<GetGovernanceLogInput>(
            "GetGovernanceLogInput",
            json!({"include_revisions": "true", "limit": "5"}),
        );
        rejects::<VerifyGovernanceLogInput>(
            "VerifyGovernanceLogInput",
            json!({"full": "true"}),
        );
        rejects::<GetCouncilMeetingsInput>(
            "GetCouncilMeetingsInput",
            json!({"limit": 3}),
        );
        rejects::<GetProposalsInput>(
            "GetProposalsInput",
            json!({"sort": "oldest"}),
        );
        rejects::<GetConstitutionInput>(
            "GetConstitutionInput",
            json!({"version": "0.3"}),
        );
        rejects::<GetDashboardInput>(
            "GetDashboardInput",
            json!({"since": "2026-10-01T00:00:00Z", "sort": "date"}),
        );
        rejects::<ExportDataInput>("ExportDataInput", json!({}));
        rejects::<JoinCommunityInput>(
            "JoinCommunityInput",
            json!({"community": "general"}),
        );
        rejects::<ManageFriendshipInput>(
            "ManageFriendshipInput",
            json!({"agent": "a", "action": "request"}),
        );
        rejects::<ManageBlockInput>(
            "ManageBlockInput",
            json!({"agent": "a", "action": "block"}),
        );
        rejects::<GetFriendsInput>("GetFriendsInput", json!({}));
        rejects::<GetMyModerationRecordInput>(
            "GetMyModerationRecordInput",
            json!({}),
        );
        rejects::<SendMessageInput>(
            "SendMessageInput",
            json!({"agent": "a", "body": "hi"}),
        );
        rejects::<GetInboxInput>("GetInboxInput", json!({}));
        rejects::<ReportMessageInput>(
            "ReportMessageInput",
            json!({"message_id": uuid}),
        );
        rejects::<DeleteMessageInput>(
            "DeleteMessageInput",
            json!({"message_id": uuid}),
        );
        rejects::<FileAppealInput>(
            "FileAppealInput",
            json!({"moderation_action_id": uuid, "appeal_statement": "s"}),
        );
        rejects::<GetContentInput>("GetContentInput", json!({"id": uuid}));
        rejects::<CreatePostPayload>(
            "CreatePostPayload",
            json!({"community": "general", "title": "t", "body": "b"}),
        );
        rejects::<CreateCommentInput>(
            "CreateCommentInput",
            json!({"reply_to": "7ad26ccd", "body": "b"}),
        );
        rejects::<CastVoteInput>(
            "CastVoteInput",
            json!({"target": "7ad26ccd", "value": 1}),
        );
        rejects::<FlagContentInput>(
            "FlagContentInput",
            json!({"target": "7ad26ccd", "reason": "r"}),
        );
        rejects::<DesignateProposalInput>(
            "DesignateProposalInput",
            json!({"post_id": "7ad26ccd", "category": "policy"}),
        );
        rejects::<UpdateProfilePayload>(
            "UpdateProfilePayload",
            json!({"bio": "b"}),
        );
        rejects::<SubmitFeedbackPayload>(
            "SubmitFeedbackPayload",
            json!({"body": "b"}),
        );
        rejects::<RequestContactPayload>(
            "RequestContactPayload",
            json!({"feedback_id": uuid, "transcript_sha256": "ab".repeat(32)}),
        );
        rejects::<SearchGovernanceLogInput>(
            "SearchGovernanceLogInput",
            json!({"query": "quorum", "limit": "5"}),
        );
        rejects::<CommentRepliesQuery>(
            "CommentRepliesQuery",
            json!({"since": "2026-10-01T00:00:00Z"}),
        );
        rejects::<DeleteContentInput>(
            "DeleteContentInput",
            json!({"target": "7ad26ccd"}),
        );
        rejects::<DeleteContentPayload>(
            "DeleteContentPayload",
            json!({"target": uuid}),
        );
        rejects::<TrashInput>(
            "TrashInput",
            json!({"mode": "restore", "target": "7ad26ccd"}),
        );
        rejects::<TrashListInput>(
            "TrashListInput",
            json!({"target": "7ad26ccd", "limit": "5"}),
        );
        rejects::<TrashTargetPayload>(
            "TrashTargetPayload",
            json!({"target": uuid}),
        );
    }

    /// Every REST body rejects a field it does not have, naming it: the
    /// signed bodies split their envelope from the payload by hand
    /// (`deny_unknown_fields` does not work through `flatten`), and the
    /// envelope-only bodies deny on their own
    #[test]
    fn every_request_body_rejects_unknown_fields() {
        use serde::de::DeserializeOwned;
        use serde_json::{Value, json};

        fn rejects<T: DeserializeOwned + std::fmt::Debug>(
            name: &str,
            mut valid: Value,
        ) {
            serde_json::from_value::<T>(valid.clone()).unwrap_or_else(|e| {
                panic!("{name}: the valid body failed: {e}")
            });
            valid
                .as_object_mut()
                .unwrap()
                .insert("bogus_field".into(), json!(1));
            let err = serde_json::from_value::<T>(valid)
                .expect_err(name)
                .to_string();
            assert!(
                err.contains("unknown field `bogus_field`"),
                "{name}: {err}"
            );
        }

        let uuid = "7ad26ccd-0000-4000-8000-000000000000";
        let env = |mut v: Value| {
            let o = v.as_object_mut().unwrap();
            o.insert("agent_id".into(), json!(uuid));
            o.insert("signature".into(), json!("ab"));
            o.insert("timestamp".into(), json!(7));
            v
        };
        rejects::<CreatePostRequest>(
            "CreatePostRequest",
            env(json!({"community": "general", "title": "t", "body": "b"})),
        );
        rejects::<CreateCommentRequest>(
            "CreateCommentRequest",
            env(json!({"reply_to": uuid, "body": "b"})),
        );
        rejects::<CastVoteRequest>(
            "CastVoteRequest",
            env(json!({"target": uuid, "value": 1})),
        );
        rejects::<SubmitFeedbackRequest>(
            "SubmitFeedbackRequest",
            env(json!({"body": "b"})),
        );
        rejects::<RequestContactRequest>(
            "RequestContactRequest",
            env(json!({"feedback_id": uuid})),
        );
        rejects::<RegisterEncryptionKeyRequest>(
            "RegisterEncryptionKeyRequest",
            env(json!({"x25519_public_key": "00", "key_signature": "00"})),
        );
        rejects::<SendMessageRequest>(
            "SendMessageRequest",
            env(json!({"message_id": uuid, "agent": "a", "body": "b"})),
        );
        rejects::<DesignateProposalRequest>(
            "DesignateProposalRequest",
            env(json!({"post_id": uuid, "category": "policy"})),
        );
        rejects::<FlagContentRequest>(
            "FlagContentRequest",
            env(json!({"target": uuid, "reason": "r"})),
        );
        rejects::<FileAppealRequest>(
            "FileAppealRequest",
            env(json!({"moderation_action_id": uuid, "appeal_statement": "s"})),
        );
        rejects::<GetDashboardRequest>(
            "GetDashboardRequest",
            env(json!({"sort": "date"})),
        );
        rejects::<DeleteContentRequest>(
            "DeleteContentRequest",
            env(json!({"target": uuid})),
        );
        rejects::<TrashListRequest>(
            "TrashListRequest",
            env(json!({"limit": 5})),
        );
        rejects::<TrashRestoreRequest>(
            "TrashRestoreRequest",
            env(json!({"target": uuid})),
        );
        rejects::<TrashDeletePermanentlyRequest>(
            "TrashDeletePermanentlyRequest",
            env(json!({"target": uuid})),
        );
        rejects::<GetContentRequest>(
            "GetContentRequest",
            env(json!({"id": "7ad26ccd", "detail": "summary"})),
        );
        rejects::<UpdateProfileRequest>(
            "UpdateProfileRequest",
            json!({"bio": "b", "signature": "ab", "timestamp": 7}),
        );
        rejects::<SignedRequest<NoParams>>(
            "SignedRequest<NoParams>",
            env(json!({})),
        );
        rejects::<SignedRequest<GetMyModerationRecordInput>>(
            "SignedRequest<GetMyModerationRecordInput>",
            env(json!({})),
        );
        rejects::<SignedRequest<GetInboxInput>>(
            "SignedRequest<GetInboxInput>",
            env(json!({})),
        );
        rejects::<SignedRequest<GetFriendsInput>>(
            "SignedRequest<GetFriendsInput>",
            env(json!({})),
        );
        rejects::<SignedRequest<ExportDataInput>>(
            "SignedRequest<ExportDataInput>",
            env(json!({})),
        );
        rejects::<SignedRequest<ReportMessageBody>>(
            "SignedRequest<ReportMessageBody>",
            env(json!({"message_key": "00"})),
        );
        rejects::<LookupByKeyRequest>(
            "LookupByKeyRequest",
            json!({"public_key": "00"}),
        );
        rejects::<RegisterAgentRequest>(
            "RegisterAgentRequest",
            json!({
                "operator_email": "a@b.c",
                "operator_password": "p",
                "name": "n",
                "public_key": "00",
            }),
        );
    }

    /// A signed body still needs its whole envelope, and a payload field
    /// cannot ride as an envelope field or the other way round
    #[test]
    fn a_signed_body_needs_its_envelope() {
        let uuid = "7ad26ccd-0000-4000-8000-000000000000";
        let err = serde_json::from_value::<CastVoteRequest>(
            serde_json::json!({"agent_id": uuid, "target": uuid, "value": 1, "timestamp": 7}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing field `signature`"), "{err}");
        // `agent_id` belongs to the envelope of a body-signed request, and
        // is unknown to one whose agent is in the path.
        let err = serde_json::from_value::<UpdateProfileRequest>(
            serde_json::json!({"agent_id": uuid, "bio": "b", "signature": "ab", "timestamp": 7}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("unknown field `agent_id`"), "{err}");
    }

    /// The published schema of a signed body is the payload's properties
    /// plus the envelope's, closed and `$ref`-free
    #[cfg(feature = "schemars")]
    #[test]
    fn signed_body_schemas_are_closed_and_complete() {
        let schema =
            serde_json::to_value(schemars::schema_for!(CastVoteRequest))
                .unwrap();
        let mut names: Vec<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["agent_id", "signature", "target", "timestamp", "value"]
        );
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        required.sort();
        assert_eq!(required, names);
        assert_eq!(schema["additionalProperties"], false);
        let rendered = schema.to_string();
        assert!(!rendered.contains("$ref"), "{rendered}");

        let schema =
            serde_json::to_value(schemars::schema_for!(UpdateProfileRequest))
                .unwrap();
        assert!(schema["properties"].get("agent_id").is_none());
        assert_eq!(schema["additionalProperties"], false);
    }

    /// `deny_unknown_fields` shows up in the schema a model is given, so a
    /// constrained decoder cannot invent a field either
    #[cfg(feature = "schemars")]
    #[test]
    fn operation_input_schemas_forbid_additional_properties() {
        for (name, schema) in [
            ("GetFeedInput", schemars::schema_for!(GetFeedInput)),
            ("SearchInput", schemars::schema_for!(SearchInput)),
            (
                "GetGovernanceLogInput",
                schemars::schema_for!(GetGovernanceLogInput),
            ),
            (
                "GetProposalsInput",
                schemars::schema_for!(GetProposalsInput),
            ),
            (
                "GetDashboardInput",
                schemars::schema_for!(GetDashboardInput),
            ),
            ("FlagContentInput", schemars::schema_for!(FlagContentInput)),
            (
                "CreatePostPayload",
                schemars::schema_for!(CreatePostPayload),
            ),
            (
                "UpdateProfilePayload",
                schemars::schema_for!(UpdateProfilePayload),
            ),
            ("GetInboxInput", schemars::schema_for!(GetInboxInput)),
            (
                "SearchGovernanceLogInput",
                schemars::schema_for!(SearchGovernanceLogInput),
            ),
            (
                "CommentRepliesQuery",
                schemars::schema_for!(CommentRepliesQuery),
            ),
            (
                "SignedRequest<GetMyModerationRecordInput>",
                schemars::schema_for!(
                    SignedRequest<GetMyModerationRecordInput>
                ),
            ),
            (
                "SignedRequest<NoParams>",
                schemars::schema_for!(SignedRequest<NoParams>),
            ),
            (
                "SignedRequest<ReportMessageBody>",
                schemars::schema_for!(SignedRequest<ReportMessageBody>),
            ),
            (
                "GetDashboardRequest",
                schemars::schema_for!(GetDashboardRequest),
            ),
            (
                "DeleteContentInput",
                schemars::schema_for!(DeleteContentInput),
            ),
            ("TrashInput", schemars::schema_for!(TrashInput)),
            ("TrashListRequest", schemars::schema_for!(TrashListRequest)),
        ] {
            let schema = serde_json::to_value(&schema).unwrap();
            assert_eq!(
                schema["additionalProperties"], false,
                "{name}: {schema}"
            );
        }
    }

    /// The limits in `UpdateProfilePayload`'s field docs (which a model
    /// reads) are the ones `check_lengths` enforces
    #[cfg(feature = "schemars")]
    #[test]
    fn update_profile_docs_state_the_enforced_limits() {
        let schema =
            serde_json::to_value(schemars::schema_for!(UpdateProfilePayload))
                .unwrap();
        for (field, max) in [
            ("display_name", UpdateProfilePayload::DISPLAY_NAME_MAX_CHARS),
            ("bio", UpdateProfilePayload::BIO_MAX_CHARS),
            ("model_info", UpdateProfilePayload::MODEL_INFO_MAX_CHARS),
        ] {
            let desc = schema["properties"][field]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                desc.contains(&format!("at most {max} ")),
                "{field}: {desc}"
            );
        }
    }
}

#[cfg(test)]
mod entry_type_filter_tests {
    use super::GetGovernanceLogInput;
    use crate::enums::GovernanceLogEntryType;

    #[test]
    fn a_filter_names_a_known_type_or_is_refused_by_name() {
        let ok: GetGovernanceLogInput = serde_json::from_value(
            serde_json::json!({"entry_type": "data_set"}),
        )
        .unwrap();
        assert_eq!(ok.entry_type, Some(GovernanceLogEntryType::DataSet));
        let none: GetGovernanceLogInput =
            serde_json::from_value(serde_json::json!({"entry_type": null}))
                .unwrap();
        assert_eq!(none.entry_type, None);
        let err = serde_json::from_value::<GetGovernanceLogInput>(
            serde_json::json!({"entry_type": "nope"}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("nope") && err.contains("data_set"), "{err}");
    }
}
