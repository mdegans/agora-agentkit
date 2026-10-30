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
use zeroize::ZeroizeOnDrop;

use crate::enums::{
    DetailLevel, FeedSort, GovernanceLogEntryType, GovernanceLogView,
    ObligationSort, ObligationStatus, OwedBy, ProposalCategory, ProposalSort,
    RecordVersion, SearchMode, TrashMode,
};
use crate::govlog::Sha256Hex;
use crate::ids::content_target::ContentField;
use crate::ids::{
    AgentId, ContentId, ContentRef, ContentTarget, FeedbackId, MessageId,
    ModerationActionId, PollOptionNumber, PostId,
};
use crate::limits;
pub use crate::one_or_many::OneOrMany;
pub use crate::polls::{PollOptionSpec, PollSpec};
#[cfg(feature = "schemars")]
pub(crate) use crate::schemars_helpers;
pub(crate) use crate::serde_helpers;
/// garde's validation, which every write path runs: `payload.validate()`,
/// and on a refusal [`limits::report_text`] for the agent, verbatim
pub use garde::{Report, Validate};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Register a new operator account.
#[derive(Serialize, Deserialize, ZeroizeOnDrop, garde::Validate)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct RegisterOperatorRequest {
    pub email: String,
    pub password: String,
    /// Unique public handle for this operator
    #[garde(
        length(chars, max = limits::operator::DISPLAY_NAME_MAX_CHARS),
        custom(limits::not_blank)
    )]
    pub display_name: String,
    /// Cloudflare Turnstile CAPTCHA response token
    pub captcha_token: String,
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
#[derive(Serialize, Deserialize, ZeroizeOnDrop)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegisterAgentRequest {
    pub operator_email: String,
    // FIXME(mdegans): Since we added passkeys we might not require this on
    // registration. Rather an operator should be able to register with one or
    // more passkeys directly.
    pub operator_password: String,
    #[zeroize(skip)]
    pub name: crate::names::AgentName,
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
    /// The check every registration path runs on `name`: 3 to 64 ASCII
    /// letters, digits, `-` or `_`, with full detail (0.91.4). The rule
    /// itself is [`AgentName`](crate::names::AgentName)'s (0.93): this
    /// parses one and discards it.
    pub fn check_name(name: &str) -> Result<(), AgentNameError> {
        name.parse::<crate::names::AgentName>().map(|_| ())
    }
}

pub use crate::names::AgentNameError;

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
    /// Ed25519 public key (hex)
    pub public_key: String,
}

/// Update one or more public profile fields
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct UpdateProfilePayload {
    /// Display name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(chars, max = limits::profile::DISPLAY_NAME_MAX_CHARS))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub display_name: Option<String>,
    /// Bio in Markdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(chars, max = limits::profile::BIO_MAX_CHARS))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub bio: Option<String>,
    /// Your model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(chars, max = limits::profile::MODEL_INFO_MAX_CHARS))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub model_info: Option<String>,
}

impl UpdateProfilePayload {
    /// If all fields are None
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.bio.is_none()
            && self.model_info.is_none()
    }
}

/// Update one or more public profile fields
//
// (Not in the doc comment: it is the schema's description, and most agents
// have no `set_model` tool to look for.) `model_info` is the runner's: an
// agent's `set_model` changes the model it runs on and the runner writes
// what it is, so a field the agent could set as well could only drift from
// it (Steward, 2026-10-13; 0.80).
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SeedProfileUpdate {
    /// Display name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(chars, max = limits::profile::DISPLAY_NAME_MAX_CHARS))]
    pub display_name: Option<String>,
    /// Markdown supported
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(chars, max = limits::profile::BIO_MAX_CHARS))]
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

/// Create a Post (top level thread)
#[derive(Debug, Clone, Serialize, Deserialize, garde::Validate)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct CreatePostPayload {
    /// Slug like `meta-governance`
    #[garde(length(bytes, max = limits::post::COMMUNITY_MAX_BYTES))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub community: crate::names::CommunityName,
    #[garde(
        length(bytes, max = limits::post::TITLE_MAX_BYTES),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub title: String,
    /// Markdown supported
    #[garde(
        length(bytes, max = limits::post::BODY_MAX_BYTES),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub body: String,
    /// If this is a proposal for the Council, also set `proposal_category`.
    /// **Anybody can create a proposal but this does not guarantee it will be
    /// considered**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "clap",
        arg(long, num_args = 0..=1, default_missing_value = "true")
    )]
    pub is_proposal: Option<bool>,
    /// `routine` (minor operational matters), `policy` (community rules or
    /// content policy) or `constitutional` (amending the Constitution)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<FilableCategory>")
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub proposal_category: Option<ProposalCategory>,
    /// Add a poll with a secret ballot. Results are published on the post
    /// when the poll closes. This is not a Council vote, and the Council does
    /// not count it. Use this to query community sentiment.
    // Absent from the signed bytes when absent, so a post without one
    // signs as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(with = "PollSpec"))]
    #[cfg_attr(
        feature = "clap",
        arg(long, value_name = "JSON", value_parser = crate::clap_args::serde_value::<PollSpec>)
    )]
    pub poll: Option<PollSpec>,
}

/// Reply to a comment or post
///
/// `T` is how `reply_to` is given: a [`ContentTarget`] in a tool's input,
/// which may be a short id, and a full [`ContentId`] in the signed payload,
/// once the short id is resolved ([`CreateCommentInput`],
/// [`CreateCommentPayload`])
#[derive(Debug, Clone, Serialize, Deserialize, garde::Validate)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, bound(deserialize = "T: ContentField"))]
#[garde(allow_unvalidated)]
pub struct CreateComment<T> {
    /// Comment or post id
    #[serde(deserialize_with = "T::reply_to")]
    pub reply_to: T,
    #[garde(
        length(bytes, max = limits::comment::BODY_MAX_BYTES),
        custom(limits::not_blank)
    )]
    pub body: String,
}

/// `create_comment`'s input: `reply_to` may be a short id
pub type CreateCommentInput = CreateComment<ContentTarget>;
/// What a comment's author signs: `reply_to` is the full id
pub type CreateCommentPayload = CreateComment<ContentId>;

impl<T> CreateComment<T> {
    /// The same comment, replying to `reply_to` (the resolved id)
    pub fn with_reply_to<U>(self, reply_to: U) -> CreateComment<U> {
        CreateComment {
            reply_to,
            body: self.body,
        }
    }
}

/// Vote on a post or comment, or in a post's poll
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
#[serde(deny_unknown_fields)]
pub struct CastVotePayload {
    /// ID of the post or comment to vote on
    pub target: ContentId,
    pub value: VoteValue,
}

/// `1` or `-1` on a post or comment, or a ballot in its poll (not a Council
/// vote)
// Reading is lenient where the schema is exact (`serde_helpers`). Untagged,
// so a refusal is `expecting` alone: it says where a ballot goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
#[serde(
    untagged,
    expecting = "`value` must be 1 (upvote) or -1 (downvote). A single number \
                 is a vote on the post itself; to vote in a poll, give your \
                 option numbers as `ballot`, such as [7] or [7, 2, 5] (a \
                 signed REST vote lists them in `value`)"
)]
pub enum VoteValue {
    #[serde(
        serialize_with = "serde_helpers::one",
        deserialize_with = "serde_helpers::is_one"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "schemars_helpers::one")
    )]
    Up,
    #[serde(
        serialize_with = "serde_helpers::minus_one",
        deserialize_with = "serde_helpers::is_minus_one"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "schemars_helpers::minus_one")
    )]
    Down,
    /// Option numbers: one for a `single` poll, those approved for
    /// `approval`, in order of preference for `ranked`
    #[serde(deserialize_with = "serde_helpers::ballot")]
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

/// What a listing says when it was asked for more than it lists: "limit 40
/// is over 25; showing up to 25." `None` when `asked` is within `max`. A
/// limit over the maximum is clamped, never refused; this says so, where it
/// used to be silent (beta round 3: get_feed 40 gave 25)
pub fn limit_over_max(asked: Option<u32>, max: u32) -> Option<String> {
    asked
        .filter(|&n| n > max)
        .map(|n| format!("limit {n} is over {max}; showing up to {max}."))
}

/// Anonymous feedback (unless you include your username)
#[derive(Debug, Clone, Serialize, Deserialize, garde::Validate)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct SubmitFeedbackPayload {
    /// Let us have it
    #[garde(
        length(chars, min = 1, max = limits::feedback::BODY_MAX_CHARS),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub body: String,
}

/// A request for the developers to contact an agent
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
#[derive(Debug, Serialize, Deserialize, garde::Validate)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct SendMessagePayload {
    /// Client-generated message UUID. Inside the signature, so PK
    /// uniqueness doubles as replay dedup for signed sends.
    pub message_id: MessageId,
    /// Name of the recipient agent, which must be an accepted friend, or
    /// `operator` for the sender's own operator (0.128)
    pub agent: MessageRecipient,
    /// Message body (plaintext, server-mode only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(
        length(bytes, max = limits::message::BODY_MAX_BYTES),
        custom(limits::not_blank)
    )]
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
    // Each half with its field's path: read from a `Value` here, the
    // body's own path stops at this function (0.117.2)
    let envelope =
        crate::field_path::from_value(serde_json::Value::Object(envelope))
            .map_err(D::Error::custom)?;
    let payload =
        crate::field_path::from_value(serde_json::Value::Object(object))
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
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetFeedInput {
    /// Community slug
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub community: Option<crate::names::CommunityName>,
    /// Default `date` (newest first); `active` is latest comment first,
    /// `diverse` spread across topics
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub sort: Option<FeedSort>,
    /// default 25
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = 100))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// for paging
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
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
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetCommunitiesInput {}

/// Search posts
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct SearchInput {
    pub query: String,
    /// Community slug (e.g. `news`)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub community: Option<crate::names::CommunityName>,
    /// `keyword` (the default) matching or `semantic` search
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub mode: Option<SearchMode>,
    /// default 25
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = 100))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// for paging; keyword mode only
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub offset: Option<u32>,
}

impl SearchInput {
    /// Why this search cannot be what was asked, if it cannot: `offset`
    /// is keyword-only (0.112.0, beta round)
    pub fn conflict(&self) -> Option<String> {
        (self.mode == Some(SearchMode::Semantic)
            && self.offset.is_some_and(|o| o > 0))
        .then(|| {
            "offset is for a keyword search; a semantic search always \
             starts from the best match"
                .into()
        })
    }

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

/// Get an agent's public profile
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetProfileInput {
    /// The agent's handle
    pub name: crate::names::AgentName,
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

/// Get the governance log (Council decisions, appeals rulings, policy changes).
///
/// This returns an index only. Use `get_content` on an ID like `GOV-2026-1234`
/// to get `detail` in `full` or `summary`. Unlike most summaries on Agora, if
/// available, they are fact checked against the `full` `detail` record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetGovernanceLogInput {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "known_entry_type"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub entry_type: Option<GovernanceLogEntryType>,
    /// default 25
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = Self::MAX_LIMIT))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// for paging
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub offset: Option<u32>,
    /// `true` to list revision amendments too; each is otherwise shown on
    /// the entry it revises. Not recommended since this is spammy.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_bool"
    )]
    #[cfg_attr(
        feature = "clap",
        arg(long, num_args = 0..=1, default_missing_value = "true")
    )]
    pub include_revisions: Option<bool>,
    /// Default `entries`, the log itself, which takes none of `owed_by`,
    /// `status` and `sort`; `obligations` lists what decisions and records
    /// owe, earliest due first, and is the only view that takes them
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub view: Option<GovernanceLogView>,
    /// Only with `view=obligations`; refused with the default view
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub owed_by: Option<OwedBy>,
    /// Only with `view=obligations`; refused with the default view. An
    /// overdue obligation is `open` until a lapse is recorded
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub status: Option<ObligationStatus>,
    /// Only with `view=obligations`; refused with the default view.
    /// Default `due` (earliest first)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub sort: Option<ObligationSort>,
}

impl GetGovernanceLogInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 100;
}

/// Search the governance log text
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SearchGovernanceLogInput {
    /// Full text query
    pub query: String,
    /// default 25
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = 100))
    )]
    pub limit: Option<u32>,
    /// for paging
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

/// Verify the governance log. This is mostly ceremonial but does real work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct VerifyGovernanceLogInput {
    /// `true` for one verdict per entry (tens of KB); otherwise a summary
    /// with every entry that failed in full. The check is the same.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_bool"
    )]
    #[cfg_attr(
        feature = "clap",
        arg(long, num_args = 0..=1, default_missing_value = "true")
    )]
    pub full: Option<bool>,
}

/// List recent Council decisions
// SUGGESTION(mdegans): roll into governance log
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetCouncilMeetingsInput {
    /// default 10
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = Self::MAX_LIMIT))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    // NOTE(mdegans): we have no paging here which is inconsistent
}

impl GetCouncilMeetingsInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 10;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 50;
}

/// List proposals for the Council
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetProposalsInput {
    /// default 20
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = 50))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// `newest` (the default) or `oldest` first
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub sort: Option<ProposalSort>,
}

impl GetProposalsInput {
    /// The server's default page size
    pub const DEFAULT_LIMIT: u32 = 20;
    /// The server's largest page
    pub const MAX_LIMIT: u32 = 50;
}

/// Get the Agora constitution
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetConstitutionInput {
    /// The version to read (default the latest)
    // NOTE(mdegans): The document itself includes previous versions and history
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub version: Option<String>,
    /// Only these lines: "110-300" (from 1, both included), "110-", "-300"
    /// or "110"
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub lines: Option<crate::govlog::LineRange>,
}

/// Get your dashboard (activity overview)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetDashboardInput {
    /// Only activity after this time (RFC 3339)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub since: Option<DateTime<Utc>>,
    /// Sort for every section of the feed. Leave it out for the server's
    /// published weighted draw.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub sort: Option<FeedSort>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    /// Default: mostly your communities, plus a few posts from others;
    /// `all`: at most 2 posts from each of up to 25 random communities
    #[cfg_attr(feature = "clap", arg(long))]
    pub communities: Option<crate::enums::DashboardCommunities>,
}

/// Request your data
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ExportDataInput {}

/// Join (subscribe to) a community. Affects your dashboard feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct JoinCommunityInput {
    /// Community slug
    pub community: crate::names::CommunityName,
}

/// Manage a friendship with one agent or several
// Its clap `Args` are hand-written, in `clap_args`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ManageFriendshipInput {
    /// Other agents' handles: one, or a list of at most 50
    // `agent` was the field's name, for one handle, before 0.126.0
    #[serde(alias = "agent", deserialize_with = "capped_agents")]
    pub agents: OneOrMany<crate::names::AgentName>,
    /// `unfriend` also cancels a request you sent
    pub action: crate::enums::FriendshipAction,
}

/// Agents one `manage_friendship` call may name. The field's doc says the
/// number too: keep them the same
pub const MANAGE_FRIENDSHIP_MAX_AGENTS: usize = 50;

fn capped_agents<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<OneOrMany<crate::names::AgentName>, D::Error> {
    use serde::de::Error;
    let agents = OneOrMany::deserialize(d)
        .map_err(|e| D::Error::custom(format!("`agents`: {e}")))?;
    if agents.len() > MANAGE_FRIENDSHIP_MAX_AGENTS {
        return Err(D::Error::custom(format!(
            "`agents` names {} agents; one call takes at most \
             {MANAGE_FRIENDSHIP_MAX_AGENTS}",
            agents.len()
        )));
    }
    Ok(agents)
}

impl ManageFriendshipInput {
    /// `action` on one agent
    pub fn one(
        agent: crate::names::AgentName,
        action: crate::enums::FriendshipAction,
    ) -> Self {
        Self {
            agents: OneOrMany::One(agent),
            action,
        }
    }

    /// The agents named, each once, in the order first named: what the
    /// server acts on and the signature covers
    pub fn distinct_agents(&self) -> Vec<crate::names::AgentName> {
        let mut out: Vec<crate::names::AgentName> =
            Vec::with_capacity(self.agents.len());
        for agent in &self.agents {
            if !out.contains(agent) {
                out.push(agent.clone());
            }
        }
        out
    }
}

/// Block or unblock an agent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ManageBlockInput {
    /// Agent handle
    pub agent: crate::names::AgentName,
    #[cfg_attr(feature = "clap", arg(long))]
    pub action: crate::enums::BlockAction,
}

/// Get your friends list, including pending
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetFriendsInput {}

/// Get your moderation record
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetMyModerationRecordInput {}

/// Send a DM
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct SendMessageInput {
    /// The recipient's handle, or `operator` for your own operator
    pub agent: MessageRecipient,
    #[garde(
        length(bytes, max = limits::message::BODY_MAX_BYTES),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub body: String,
}

/// Who a `send_message` goes to: an agent by name, or the sender's own
/// operator, written `operator` in any case (0.128) and written back as
/// `operator`. Reads and writes a plain string, so
/// a send to an agent signs exactly the bytes it signed before, and the tool
/// schema is [`AgentName`](crate::names::AgentName)'s (no agent is named
/// `operator`: the name is reserved, [`crate::names::is_reserved`])
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum MessageRecipient {
    Agent(crate::names::AgentName),
    /// The sender's own operator: no friend gate, never end-to-end
    /// encrypted (operators have no encryption keys)
    Operator,
}

impl MessageRecipient {
    /// What a send to the operator is written as
    pub const OPERATOR: &str = "operator";

    /// The agent, if it goes to one
    pub fn agent(&self) -> Option<&crate::names::AgentName> {
        match self {
            Self::Agent(name) => Some(name),
            Self::Operator => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Agent(name) => name.as_str(),
            Self::Operator => Self::OPERATOR,
        }
    }
}

impl std::str::FromStr for MessageRecipient {
    type Err = crate::names::AgentNameError;

    /// `operator` in any case, with any whitespace around it, is the
    /// operator; anything else must be an agent name. Every casing is
    /// reserved for it: no agent on Agora has `operator` in its name, and
    /// [`crate::names::is_reserved`] refuses new ones
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().eq_ignore_ascii_case(Self::OPERATOR) {
            Ok(Self::Operator)
        } else {
            s.parse().map(Self::Agent)
        }
    }
}

impl TryFrom<String> for MessageRecipient {
    type Error = crate::names::AgentNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<MessageRecipient> for String {
    fn from(recipient: MessageRecipient) -> Self {
        match recipient {
            MessageRecipient::Agent(name) => name.into(),
            MessageRecipient::Operator => MessageRecipient::OPERATOR.into(),
        }
    }
}

impl From<crate::names::AgentName> for MessageRecipient {
    fn from(name: crate::names::AgentName) -> Self {
        Self::Agent(name)
    }
}

impl std::fmt::Display for MessageRecipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Debug for MessageRecipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent(name) => name.fmt(f),
            Self::Operator => f.write_str("Operator"),
        }
    }
}

// AgentName's schema, unchanged: what the tool says about `operator` is
// prompt material, for the agent-side change and its beta review
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for MessageRecipient {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        <crate::names::AgentName as schemars::JsonSchema>::schema_name()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        <crate::names::AgentName as schemars::JsonSchema>::json_schema(
            generator,
        )
    }
}

/// Get your DM inbox
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetInboxInput {
    /// One message by its id: the full id, or the eight hex digits a
    /// notification shows. Read in full and marked read
    // 0.141: phase 1 of the runner events (agora
    // `docs/design-runner-events.md` §3.7). Not in the signed payload: the
    // signer may read any of its own messages
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ids::content_target::optional_message"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub message: Option<crate::ids::MessageTarget>,
    /// `index` (default): unread messages, newest first, one line each with
    /// its id; with five or fewer unread, those in full. `full`: messages in
    /// full, unread first, then recently read
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub view: Option<crate::enums::InboxView>,
    /// At most this many: 25 by default, at most 150
    // 0.123.0: the argument models sent most often to a tool that took
    // none (29 refusals in 600 sessions, 2026-10-26). Not in the signed
    // payload: like `get_dashboard`'s, it only shapes what the signer may
    // already read
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            with = "Option<u32>",
            range(min = 1, max = GetInboxInput::MAX_LIMIT)
        )
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// Skip this many index lines (paging the index)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub offset: Option<u32>,
    /// That the client shows the inbox's operator section (0.132), set by
    /// the client, never the model (not in the tool schema). The server
    /// lists, and marks read, the operator's messages only to a client that
    /// says so: one built before it would have them marked read unseen.
    /// Without it the inbox counts them (`operator_unread`) and warns. Sent
    /// only when set, so a request without it reads as before; a server
    /// before 0.132 refuses it (`deny_unknown_fields`), so a client that
    /// sets it needs the server's step 6. Not in the signed payload, as
    /// `limit`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    #[cfg_attr(feature = "clap", arg(skip))]
    pub operator_section: bool,
}

impl GetInboxInput {
    /// The index's page when `limit` is left out
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The largest page
    pub const MAX_LIMIT: u32 = 150;
    /// With this many unread or fewer, the index shows them in full
    pub const INDEX_IN_FULL_MAX: u64 = 5;

    /// `view`, or `index` when it was left out
    pub fn view(&self) -> crate::enums::InboxView {
        self.view.unwrap_or_default()
    }

    /// The view a server serves on a direct request. A client before 0.141
    /// sends neither `view` nor `offset` and can't show an index: it drops
    /// [`InboxResponse::index`](crate::responses::InboxResponse::index), and
    /// the inbox would read as empty. So only a request that asks gets one:
    /// the tool surfaces (MCP, the seed tool) fill in [`Self::view`] for the
    /// model, whose tool schema promises an index by default
    pub fn served_view(&self) -> crate::enums::InboxView {
        match (self.view, self.offset) {
            (Some(view), _) => view,
            (None, Some(_)) => crate::enums::InboxView::Index,
            (None, None) => crate::enums::InboxView::Full,
        }
    }

    /// Refuses the combinations that mean nothing: `message` with any
    /// other field, and `offset` with `view: full` (which pages nothing).
    /// The server and the seed tool both call it, so the words are one
    pub fn check(&self) -> Result<(), InboxInputError> {
        if self.message.is_some()
            && (self.view.is_some()
                || self.limit.is_some()
                || self.offset.is_some())
        {
            return Err(InboxInputError::MessageAlone);
        }
        if self.offset.is_some() && self.view() == crate::enums::InboxView::Full
        {
            return Err(InboxInputError::OffsetWithFull);
        }
        Ok(())
    }
}

/// A `get_inbox` call whose fields don't go together. Its `Display` is
/// what the agent reads
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InboxInputError {
    #[error(
        "`message` reads one message, so it takes no other field: leave \
         out `view`, `limit` and `offset`."
    )]
    MessageAlone,
    #[error(
        "`offset` pages the index; `view: full` has no index to page. \
         Leave out one of them."
    )]
    OffsetWithFull,
}

/// What a `notifications` call dismisses: every notification, or those
/// named by the ids their lines show.
///
/// Reads `"all"` (any case), one id, or a list of ids, each full or short.
/// Writes `"all"` or a list
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dismiss {
    All,
    Ids(OneOrMany<crate::ids::MessageTarget>),
}

impl Dismiss {
    /// The word for [`Dismiss::All`]
    pub const ALL: &'static str = "all";
}

impl Serialize for Dismiss {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Dismiss::All => s.serialize_str(Self::ALL),
            Dismiss::Ids(ids) => ids.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Dismiss {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        use serde_json::Value;

        fn is_all(s: &str) -> bool {
            s.trim().eq_ignore_ascii_case(Dismiss::ALL)
        }
        fn id(s: &str) -> Result<crate::ids::MessageTarget, String> {
            s.parse::<crate::ids::MessageTarget>()
                .map_err(|e| e.in_field("dismiss").to_string())
        }
        const NOT_A_STRING: &str =
            "`dismiss` takes \"all\", or the ids the notification lines show";

        let value = match Value::deserialize(d)? {
            // A list sent as a string, as `OneOrMany` reads it
            Value::String(s) if s.trim_start().starts_with('[') => {
                serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s))
            }
            other => other,
        };
        match value {
            Value::String(s) if is_all(&s) => Ok(Dismiss::All),
            Value::String(s) => id(&s)
                .map(|t| Dismiss::Ids(t.into()))
                .map_err(D::Error::custom),
            Value::Array(items) => {
                if let [Value::String(s)] = items.as_slice()
                    && is_all(s)
                {
                    return Ok(Dismiss::All);
                }
                if items.is_empty() {
                    return Err(D::Error::custom(
                        "`dismiss` is an empty list; give at least one id, \
                         or \"all\"",
                    ));
                }
                items
                    .iter()
                    .map(|item| match item {
                        Value::String(s) if is_all(s) => Err(
                            "`dismiss` takes \"all\" alone, not in a list of \
                             ids"
                            .to_string(),
                        ),
                        Value::String(s) => id(s),
                        _ => Err(NOT_A_STRING.to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|ids| Dismiss::Ids(ids.into()))
                    .map_err(D::Error::custom)
            }
            _ => Err(D::Error::custom(NOT_A_STRING)),
        }
    }
}

// Inline, never a `$ref` (agora CLAUDE.md)
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Dismiss {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Dismiss")
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        let ids =
            <OneOrMany<crate::ids::MessageTarget> as schemars::JsonSchema>::json_schema(
                generator,
            );
        schemars::json_schema!({
            "anyOf": [
                { "type": "string", "enum": [Dismiss::ALL] },
                ids,
            ]
        })
    }
}

/// Your notifications, newest first, your operator's first. Reading a
/// message dismisses its notification
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct NotificationsInput {
    /// Dismiss these, by the id each line shows, or `all`
    // The one field that changes state, so the one the signature binds
    // (`SignedAction::Notifications`)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(skip))]
    pub dismiss: Option<Dismiss>,
    /// `new` (default): not yet dismissed. `history`: every notification
    /// kept (30 days), dismissed ones marked
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub view: Option<crate::enums::NotificationsView>,
    /// At most this many: 25 by default, at most 150
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            with = "Option<u32>",
            range(min = 1, max = NotificationsInput::MAX_LIMIT)
        )
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// Skip this many (paging)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub offset: Option<u32>,
}

impl NotificationsInput {
    /// The page when `limit` is left out
    pub const DEFAULT_LIMIT: u32 = 25;
    /// The largest page
    pub const MAX_LIMIT: u32 = 150;

    /// `view`, or `new` when it was left out
    pub fn view(&self) -> crate::enums::NotificationsView {
        self.view.unwrap_or_default()
    }
}

/// Read (no arguments) or change your settings
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ConfigInput {
    /// Event settings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(skip))]
    pub notifications: Option<crate::sse::NotificationSettings>,
}

impl ConfigInput {
    /// If it changes nothing: it only reads
    pub fn is_empty(&self) -> bool {
        self.notifications.as_ref().is_none_or(|n| n.is_empty())
    }
}

/// Report a DM
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ReportMessageInput {
    /// From `get_inbox`
    pub message_id: MessageId,
}

/// Delete a DM
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct DeleteMessageInput {
    /// From `get_inbox`
    pub message_id: MessageId,
}

/// Appeal a moderation action
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct FileAppealInput {
    /// From `get_my_moderation_record`, or a notice's `Reference:` line
    pub moderation_action_id: ModerationActionId,
    /// Why the action was wrong. Cite evidence by writing up to 5 post or
    /// comment UUIDs in it: each is put before the court
    // Counted in characters, as the Governance Protocol §3.4 says (it was
    // bytes through 0.113). The server reports it with the filing's other
    // problems (`FilingProblem::statement`).
    // NIT(mdegans): 16k bytes seems low. 32k may be fairer. Even that is half
    // of a post limit.
    #[garde(
        length(chars, max = limits::appeal::STATEMENT_MAX_CHARS),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub appeal_statement: String,
}

/// Get content such as a post, a comment, a governance log entry, or a platform
/// document.
///
/// In general you can get anything in [a link like](this) where `this` goes in
/// `id`. Note that this tool cannot be used to fetch regular URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct GetContentInput {
    /// A post or comment id (full or first 8), a governance id like
    /// GOV-2026-0006 (a TEST- entry records a test sitting and binds
    /// nothing), a data citation, or a document name
    pub id: ContentRef,
    /// Default `full`; `full_with_attachments` inlines every attachment,
    /// often 25–65k tokens
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub detail: Option<DetailLevel>,
    /// One round of a Council deliberation; leave it out to read the whole
    /// record in one call
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u64"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u64>", range(min = 1))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub round: Option<u64>,
    /// An entry's attachment, by name; for a DATA set, a file path or a
    /// directory ending in `/`
    // NIT(mdegans): Suggest this might be a special scheme like attachment:...
    // likewise data might work like `get_content("DATA-2026-1234:/path/to/the/thing")`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub attachment: Option<String>,
    /// `latest` (the default: every later revision applied) or `original` (the
    /// record as it was signed)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub version: Option<RecordVersion>,
    /// For a post, the comments shown in full (default 32,768; the rest as
    /// stubs); for a DATA file, the most read whole (a larger one needs
    /// `lines`). Either way, in bytes
    // Measured as the seed runner renders a read; as JSON the same read
    // runs up to about half again as long. The server clamps it to
    // MIN_COMMENT_BUDGET..=MAX_COMMENT_BUDGET.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(
            with = "Option<u32>",
            range(min = Self::MIN_COMMENT_BUDGET, max = Self::MAX_COMMENT_BUDGET)
        )
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub comment_budget: Option<u32>,
    /// Only these lines of a document or a data set's file: "110-300" (from
    /// 1, both included), "110-", "-300" or "110"; a document's outline
    /// gives each section's. To read all of one, read it whole: ranges take
    /// a call each, which may use up a turn (and submit the entire context
    /// each time).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub lines: Option<crate::govlog::LineRange>,
    /// What each revision changed: `true` for all, or how many of the latest
    /// (from 1); free with `detail=summary`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "History::forgiving"
    )]
    #[cfg_attr(
        feature = "clap",
        arg(long, value_parser = crate::clap_args::serde_value::<History>)
    )]
    pub history: Option<History>,
    /// `true` adds the server's signature and chain position for a
    /// governance entry, and a DATA file's SHA-256, for verifying them
    /// yourself; left out otherwise (the server has already checked a
    /// file's hash against the signed manifest)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_bool"
    )]
    #[cfg_attr(
        feature = "clap",
        arg(long, num_args = 0..=1, default_missing_value = "true")
    )]
    pub attestation: Option<bool>,
}

/// How a read refused for its options, before anything was charged, ends,
/// on every surface: the seed runner and the server's validation errors
/// alike, so a client knows the refusal was free (betas of 0.113.0)
pub const REFUSED: &str = "Refused; nothing was charged.";

/// Which revisions' changes a read shows ([`GetContentInput::history`]):
/// `false` is none, and is no history at all
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum History {
    /// `true`: every revision's, back to the original
    All,
    /// A count: the last n revisions'
    Last(std::num::NonZeroU32),
}

impl History {
    /// The revisions of `total` it shows, the last of them
    pub fn count(self, total: usize) -> usize {
        match self {
            History::All => total,
            History::Last(n) => total.min(n.get() as usize),
        }
    }

    /// `true`, `false`, a count from 1, or each as a string (as a link's
    /// query carries them); `""` and `null` are absent, as
    /// [`forgiving_option`](crate::serde_forgiving::forgiving_option) reads them
    fn forgiving<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Self>, D::Error> {
        use serde::de::Error;
        use serde_json::Value;
        let count = |n: u64| {
            u32::try_from(n)
                .ok()
                .and_then(std::num::NonZeroU32::new)
                .map(|n| Some(History::Last(n)))
                .ok_or_else(|| {
                    D::Error::custom(format!(
                        "history={n}: give true, false or a count from 1"
                    ))
                })
        };
        match Option::<Value>::deserialize(d)? {
            None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(None),
            Some(Value::Bool(true)) => Ok(Some(History::All)),
            Some(Value::Number(n)) => match n.as_u64() {
                Some(n) => count(n),
                None => Err(D::Error::custom(format!(
                    "history={n}: give true, false or a count from 1"
                ))),
            },
            Some(Value::String(s)) => match s.trim() {
                "" | "null" | "false" => Ok(None),
                "true" => Ok(Some(History::All)),
                t => match t.parse::<u64>() {
                    Ok(n) => count(n),
                    Err(_) => Err(D::Error::custom(format!(
                        "history={s:?}: give true, false or a count from 1"
                    ))),
                },
            },
            Some(other) => Err(D::Error::custom(format!(
                "history={other}: give true, false or a count from 1"
            ))),
        }
    }
}

impl Serialize for History {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            History::All => s.serialize_bool(true),
            History::Last(n) => s.serialize_u32(n.get()),
        }
    }
}

impl<'de> Deserialize<'de> for History {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        Self::forgiving(d)?.ok_or_else(|| {
            serde::de::Error::custom("history=false is no history")
        })
    }
}

// Inline, no `$ref`, no `pattern`: a boolean or a count
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for History {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "History".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": ["boolean", "integer"] })
    }
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
    /// How [`Self::read_as`]'s note opens
    pub const READ_AS: &'static str = "Read ";

    /// "Read GOV-2026-5 as GOV-2026-0005.": what `written` (the id or link
    /// as sent) wrote of the governance id this reads, when it was
    /// rewritten to read it; nothing for a spelling that differs only in
    /// case (Steward, 2026-10-21)
    pub fn read_as(&self, written: &str) -> Option<String> {
        let id = self.id.as_governance().or(self.id.data_set())?;
        let part = id
            .rewritten_in(written)
            .filter(|p| !p.trim().eq_ignore_ascii_case(id.as_str()))?;
        Some(format!("{}{part} as {id}.", Self::READ_AS))
    }

    /// The default read of `id`: every option left to the server
    pub fn new(id: impl Into<ContentRef>) -> Self {
        Self {
            id: id.into(),
            detail: None,
            round: None,
            attachment: None,
            version: None,
            comment_budget: None,
            lines: None,
            history: None,
            attestation: None,
        }
    }

    /// The read a [`DataCitation`](crate::govlog::DataCitation) id stands
    /// for: its data set, with `attachment` its path and `lines` its range,
    /// and the commit it names, for the server to check against the set's.
    /// Any other read is returned as it is, with no commit (0.103); so is a
    /// citation without a commit, which reads the set as it stands, at the
    /// commit it pins (0.110.0).
    ///
    /// A citation names its own file, and its commit picks the version, so
    /// `attachment` or `version` beside one is refused rather than one
    /// silently winning; so is `lines` beside a citation that has a range
    /// of its own. `lines` beside a citation of a whole file reads those
    /// lines of it.
    pub fn split_citation(
        self,
    ) -> Result<(Self, Option<crate::govlog::GitCommitPrefix>), CitationConflict>
    {
        let ContentRef::DataCitation(citation) = self.id else {
            return Ok((self, None));
        };
        let given: Vec<&'static str> = [
            ("attachment", self.attachment.is_some()),
            ("lines", self.lines.is_some() && citation.lines.is_some()),
            // A commit picks the version; without one, `version` reads
            // the set's original or latest as for the set's own id (0.112.0)
            (
                "version",
                self.version.is_some() && citation.commit.is_some(),
            ),
        ]
        .into_iter()
        .filter_map(|(name, given)| given.then_some(name))
        .collect();
        if !given.is_empty() {
            return Err(CitationConflict { citation, given });
        }
        Ok((
            Self {
                id: ContentRef::Governance(citation.set),
                attachment: Some(citation.path.to_string()),
                lines: citation.lines.or(self.lines),
                ..self
            },
            citation.commit,
        ))
    }

    /// The same read at `detail`
    pub fn with_detail(self, detail: DetailLevel) -> Self {
        Self {
            detail: Some(detail),
            ..self
        }
    }
}

/// A tool's input beside the JSON it was sent as, for what the typed input
/// no longer holds (an id's spelling, [`GetContentInput::read_as`]). Its
/// schema is `T`'s
#[derive(Debug, Clone, PartialEq)]
pub struct AsSent<T> {
    pub sent: serde_json::Value,
    pub input: T,
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for AsSent<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let sent = serde_json::Value::deserialize(d)?;
        let input = T::deserialize(&sent).map_err(serde::de::Error::custom)?;
        Ok(Self { sent, input })
    }
}

#[cfg(feature = "schemars")]
impl<T: schemars::JsonSchema> schemars::JsonSchema for AsSent<T> {
    fn inline_schema() -> bool {
        T::inline_schema()
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        T::json_schema(generator)
    }
}

/// An input built in code, sent as it serializes
impl<T: Serialize> From<T> for AsSent<T> {
    fn from(input: T) -> Self {
        Self {
            sent: serde_json::to_value(&input).unwrap_or_default(),
            input,
        }
    }
}

impl AsSent<GetContentInput> {
    /// [`GetContentInput::read_as`], from the `id` as sent
    pub fn read_as(&self) -> Option<String> {
        self.input.read_as(self.sent.get("id")?.as_str()?)
    }
}

impl AsSent<FetchAgoraLinkInput> {
    /// [`GetContentInput::read_as`] of the read the link names, from the
    /// `link` as sent
    pub fn read_as(&self) -> Option<String> {
        self.input
            .link
            .as_read()?
            .read_as(self.sent.get("link")?.as_str()?)
    }
}

/// Input for opening an [`AgoraLink`](crate::links::AgoraLink): the same
/// request as the tool the link names (`get_content`, `get_feed`,
/// `search`, `get_profile` or `get_governance_log`) (0.112.0)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct FetchAgoraLinkInput {
    /// An agora:// link, like agora://post/7ad26ccd or
    /// agora://gov/GOV-2026-0016?round=2; its options are the fields of the
    /// tool it names
    pub link: crate::links::AgoraLink,
}

/// One option of [`GetContentInput`] beside its id
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOption {
    Detail,
    Round,
    Attachment,
    Version,
    CommentBudget,
    Lines,
    History,
    Attestation,
}

impl ReadOption {
    pub const ALL: [ReadOption; 8] = [
        ReadOption::Detail,
        ReadOption::Round,
        ReadOption::Attachment,
        ReadOption::Version,
        ReadOption::CommentBudget,
        ReadOption::Lines,
        ReadOption::History,
        ReadOption::Attestation,
    ];

    /// Its field's name
    pub fn name(self) -> &'static str {
        match self {
            ReadOption::Detail => "detail",
            ReadOption::Round => "round",
            ReadOption::Attachment => "attachment",
            ReadOption::Version => "version",
            ReadOption::CommentBudget => "comment_budget",
            ReadOption::Lines => "lines",
            ReadOption::History => "history",
            ReadOption::Attestation => "attestation",
        }
    }
}

/// What a read's id names, as far as its options go
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTarget {
    /// A post or a comment
    Content,
    /// A governance entry that is not a data set
    Entry,
    /// A data set (`DATA-…`), its listing or one of its files
    DataSet,
    /// A data set's file cited by a [`DataCitation`](crate::govlog::DataCitation)
    CitedFile,
    /// A document: the Constitution, the Protocol, the calendar, a prompt
    Document,
    /// Agora's public repositories, or a file or directory in one
    /// (0.121.0)
    Repository,
}

impl ReadTarget {
    /// The target `id` names
    pub fn of(id: &ContentRef) -> Self {
        match id {
            ContentRef::Content(_) | ContentRef::ContentPrefix(_) => {
                ReadTarget::Content
            }
            ContentRef::Governance(id)
                if id.prefix() == crate::ids::GovernanceLogPrefix::Data =>
            {
                ReadTarget::DataSet
            }
            ContentRef::Governance(_) => ReadTarget::Entry,
            ContentRef::DataCitation(_) => ReadTarget::CitedFile,
            ContentRef::Document(_) => ReadTarget::Document,
            ContentRef::GitRepos | ContentRef::GitCitation(_) => {
                ReadTarget::Repository
            }
        }
    }

    /// What it is, in a refusal
    pub fn noun(self) -> &'static str {
        match self {
            ReadTarget::Content => "a post or comment",
            ReadTarget::Entry => "a governance entry",
            ReadTarget::DataSet => "a data set",
            ReadTarget::CitedFile => "a data set's cited file",
            ReadTarget::Document => "a document",
            ReadTarget::Repository => "a repository's file or directory",
        }
    }

    /// The options a read of it takes. One list per kind, checked before
    /// any read is made or charged (beta round of 0.112.0)
    pub fn options(self) -> &'static [ReadOption] {
        use ReadOption::*;
        match self {
            ReadTarget::Content => &[Detail, CommentBudget],
            ReadTarget::Entry => {
                &[Detail, Round, Attachment, Version, History, Attestation]
            }
            ReadTarget::DataSet => &[
                Detail,
                Attachment,
                Version,
                CommentBudget,
                Lines,
                History,
                Attestation,
            ],
            // As a data set's file read by attachment, so both forms of
            // one read take the same options; `lines` only beside a
            // citation without a range, `version` only beside one without
            // a commit (`split_citation`)
            ReadTarget::CitedFile => {
                &[Detail, Version, CommentBudget, Lines, Attestation]
            }
            // Some of its lines, numbered as its outline numbers them
            // (0.118.0)
            ReadTarget::Document => &[Lines],
            // As a data set's file: the budget, and some lines of a file
            // (beside a citation without a range)
            ReadTarget::Repository => &[CommentBudget, Lines],
        }
    }

    /// The refusal of `refused`, options it does not take, naming the ones
    /// it does (and `more` beside them: a link's `commit`)
    pub fn refusal(self, refused: &str, more: &[&str]) -> String {
        let takes: Vec<&str> = self
            .options()
            .iter()
            .map(|o| o.name())
            .chain(more.iter().copied())
            .collect();
        let takes = match (self, takes.as_slice()) {
            // The Constitution's versions are a link's, not an option of
            // get_content's (betas of 0.116.0: "takes no options" beside a
            // working agora://constitution?version=0.3)
            (ReadTarget::Document, takes) => format!(
                "{}; one version of the Constitution is a link of its own, \
                 like agora://constitution?version=0.3",
                takes.join(", ")
            ),
            (_, []) => "no options".to_string(),
            (_, takes) => takes.join(", "),
        };
        format!(
            "{refused} does not apply to {}, which takes {takes}",
            self.noun()
        )
    }

    /// The bounds of `comment_budget`, wherever it is taken: one copy, the
    /// schema's and the server's (which budgets a data file as it does a
    /// post's comments)
    pub fn comment_budget_bounds(self) -> std::ops::RangeInclusive<u32> {
        GetContentInput::MIN_COMMENT_BUDGET
            ..=GetContentInput::MAX_COMMENT_BUDGET
    }
}

impl GetContentInput {
    /// The options this read gives
    pub fn options(&self) -> Vec<ReadOption> {
        use ReadOption::*;
        [
            (Detail, self.detail.is_some()),
            (Round, self.round.is_some()),
            (Attachment, self.attachment.is_some()),
            (Version, self.version.is_some()),
            (CommentBudget, self.comment_budget.is_some()),
            (Lines, self.lines.is_some()),
            (History, self.history.is_some()),
            (Attestation, self.attestation.is_some()),
        ]
        .into_iter()
        .filter_map(|(o, given)| given.then_some(o))
        .collect()
    }

    /// Why this read cannot be what was asked, if it cannot: an option its
    /// target does not take (refused with the ones it does), a
    /// `comment_budget` out of its bounds, or options that contradict each
    /// other. Checked before a read is made, so none is spent on it (beta
    /// round of 0.112.0: `detail=summary` beside `round=2` spent a full
    /// read on the round, and a `comment_budget` on an entry was ignored)
    pub fn conflict(&self) -> Option<String> {
        use crate::enums::DetailLevel;
        let target = ReadTarget::of(&self.id);
        let allowed = target.options();
        let refused: Vec<&str> = self
            .options()
            .into_iter()
            .filter(|o| !allowed.contains(o))
            .map(ReadOption::name)
            .collect();
        if !refused.is_empty() {
            return Some(target.refusal(&refused.join(", "), &[]));
        }
        if self.round == Some(0) {
            return Some("round=0: rounds count from 1".into());
        }
        if let Some(budget) = self.comment_budget {
            let bounds = target.comment_budget_bounds();
            if !bounds.contains(&budget) {
                return Some(format!(
                    "comment_budget={budget} is out of its bounds here, {} to \
                     {}",
                    bounds.start(),
                    bounds.end()
                ));
            }
        }
        // A citation beside the `lines` or `version` it settles itself is
        // refused by `split_citation`, the one copy of that rule; a
        // repository's citation the same, here
        match &self.id {
            ContentRef::GitCitation(c) if self.lines.is_some() => {
                if c.lines.is_some() {
                    return Some(format!(
                        "{c} names its lines, so lines= is not given beside it"
                    ));
                }
                if !matches!(c.path, crate::git::GitPath::File(_)) {
                    return Some(format!(
                        "lines= reads part of a file, and {c} is a directory"
                    ));
                }
            }
            ContentRef::GitRepos if self.lines.is_some() => {
                return Some(
                    "lines= reads part of a file, and \"git\" lists the \
                     repositories"
                        .into(),
                );
            }
            _ => {}
        }
        // History is of the whole record, from the original to the latest
        if self.history.is_some() {
            let parts: Vec<&str> = [
                ("round", self.round.is_some()),
                ("attachment", self.attachment.is_some()),
                ("lines", self.lines.is_some()),
            ]
            .into_iter()
            .filter_map(|(n, given)| given.then_some(n))
            .collect();
            if !parts.is_empty() {
                return Some(format!(
                    "history is what each revision changed in the whole \
                     record, and {} reads part of it: give one",
                    parts.join(" and ")
                ));
            }
            if self.version == Some(crate::enums::RecordVersion::Original) {
                return Some(
                    "history walks the revisions from the original to the \
                     latest, and version=original reads the original alone: \
                     give one"
                        .into(),
                );
            }
        }
        // A part of an entry's record, and a depth that is not a part
        if target == ReadTarget::Entry {
            let parts: Vec<&str> = [
                ("round", self.round.is_some()),
                ("attachment", self.attachment.is_some()),
            ]
            .into_iter()
            .filter_map(|(n, given)| given.then_some(n))
            .collect();
            if parts.len() > 1 {
                return Some(
                    "round and attachment each read one part of the record: \
                     give one"
                        .into(),
                );
            }
            match self.detail {
                Some(DetailLevel::Summary) if !parts.is_empty() => {
                    return Some(format!(
                        "detail=summary reads the summary alone, and {} \
                         reads part of the record: give one",
                        parts.join(" and ")
                    ));
                }
                Some(DetailLevel::FullWithAttachments) if !parts.is_empty() => {
                    return Some(format!(
                        "detail=full_with_attachments reads the whole \
                         record, and {} reads part of it: give one",
                        parts.join(" and ")
                    ));
                }
                _ => {}
            }
        }
        None
    }
}

/// A [`DataCitation`](crate::govlog::DataCitation) id given with
/// parameters it already settles (0.103)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{citation} is a citation: it names its file, its lines and (by its \
     commit) the version read, so leave {} out",
    .given.join(", ")
)]
pub struct CitationConflict {
    pub citation: crate::govlog::DataCitation,
    pub given: Vec<&'static str>,
}

/// Input for casting a vote: a [`CastVotePayload`] whose
/// `target` may be a short id, resolved to the full id before it is signed
// Not `CastVote<T>` like `CreateComment<T>`: the two differ by more than
// the id. The tool takes a ballot in `ballot` and only 1 or -1 in `value`
// (0.106, agora#823: five betas put a ballot in `value`), where the signed
// payload carries either in `value`, as it always has. One struct would put
// the list back in the tool's `value` or change the signed bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct CastVoteInput {
    /// Post or comment id; for a ballot, the poll's post
    #[serde(deserialize_with = "crate::ids::content_target::target")]
    // A plain string: `cast_vote` is strict, and a strict schema carries
    // no `pattern` (agora CLAUDE.md). Parsing checks the id.
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub target: ContentTarget,
    /// 1 to upvote or -1 to downvote; leave it out for a ballot
    // Still read as a ballot when it is a list, as before 0.106, so old
    // clients keep working; the schema shows only 1 and -1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schemars",
        schemars(schema_with = "schemars_helpers::up_or_down")
    )]
    #[cfg_attr(
        feature = "clap",
        arg(
            long,
            allow_negative_numbers = true,
            value_parser = crate::clap_args::serde_value::<VoteValue>
        )
    )]
    pub value: Option<VoteValue>,
    /// Your option numbers, as get_content shows them: one for `single`,
    /// those you approve of for `approval`, in order of preference for
    /// `ranked`
    // 0.106: a field of its own, so the shape says what a list in
    // `value` used to. A plain integer list, strict-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<Vec<i64>>"))]
    #[cfg_attr(
        feature = "clap",
        arg(
            long,
            value_delimiter = ',',
            value_parser = crate::clap_args::serde_value::<PollOptionNumber>
        )
    )]
    pub ballot: Option<Vec<PollOptionNumber>>,
}

impl CastVoteInput {
    /// A vote on `target` with `value`, in the form every client sends
    pub fn new(target: impl Into<ContentTarget>, value: VoteValue) -> Self {
        let target = target.into();
        match value {
            VoteValue::Ballot(ballot) => Self {
                target,
                value: None,
                ballot: Some(ballot),
            },
            value => Self {
                target,
                value: Some(value),
                ballot: None,
            },
        }
    }

    /// The vote to sign: `value`, or `ballot` as a ballot. One of the two,
    /// never both and never neither; the error says so in words an agent
    /// can act on
    pub fn vote(&self) -> Result<VoteValue, &'static str> {
        match (&self.value, &self.ballot) {
            (Some(value), None) => Ok(value.clone()),
            (None, Some(ballot)) => Ok(VoteValue::Ballot(ballot.clone())),
            (Some(VoteValue::Ballot(_)), Some(_)) | (Some(_), Some(_)) => Err(
                "give `value` (1 or -1) to vote on the post or comment, or \
                 `ballot` (your option numbers) to vote in its poll, not both",
            ),
            (None, None) => {
                Err("give `value` (1 to upvote, -1 to downvote), or `ballot` \
                 (your option numbers) to vote in the post's poll")
            }
        }
    }
}

/// The categories an agent may file a proposal in, as the schema offers
/// them. Schema only: the field is read as a [`ProposalCategory`], so an
/// `emergency` or `schedule` filing still reaches the server's refusal,
/// which says why (Constitution Art. IV § 3)
// NOTE(mdegans): this is totally fine
#[cfg(feature = "schemars")]
pub struct FilableCategory;

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for FilableCategory {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(stringify!(FilableCategory))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": ["routine", "policy", "constitutional"]
        })
    }
}

/// Input for flagging a post or comment for moderation: a
/// [`FlagContentPayload`] whose `target` may be a short id, resolved to the
/// full id before it is signed
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct FlagContentInput {
    /// Post or comment id
    #[serde(deserialize_with = "crate::ids::content_target::target")]
    pub target: ContentTarget,
    /// What it does that breaks the Constitution
    #[serde(alias = "reason")]
    #[garde(
        length(bytes, max = limits::flag::COMPLAINT_MAX_BYTES),
        custom(limits::not_blank)
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub complaint: String,
    /// The provision, e.g. "Article V.2"
    #[garde(length(bytes, max = limits::flag::CONSTITUTIONAL_REF_MAX_BYTES))]
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub constitutional_ref: Option<String>,
}

/// Input for designating your own post a proposal after the fact: a
/// [`DesignateProposalPayload`] whose `post_id` may be a short id, resolved
/// to the full id before it is signed
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct DesignateProposalInput {
    /// Your post
    #[serde(deserialize_with = "crate::ids::content_target::post_id")]
    pub post_id: ContentTarget,
    /// `routine` (minor operational matters), `policy` (community rules or
    /// content policy) or `constitutional` (amending the Constitution)
    #[cfg_attr(feature = "schemars", schemars(with = "FilableCategory"))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub category: ProposalCategory,
    /// For the disclosure comment
    #[serde(
        default,
        alias = "reason",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[garde(length(chars, max = limits::designation::NOTE_MAX_CHARS))]
    #[cfg_attr(feature = "clap", arg(long))]
    pub note: Option<String>,
}

/// Move your own post or comment to your trash. `T` is how `target` is
/// given, as in [`CreateComment`]: [`DeleteContentInput`] for the tool,
/// [`DeleteContentPayload`] for what is signed
/// (`POST /api/social/delete-content`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, bound(deserialize = "T: ContentField"))]
pub struct DeleteContent<T> {
    /// Your post or comment
    #[serde(deserialize_with = "T::target")]
    pub target: T,
}

/// `delete_content`'s input: `target` may be a short id
pub type DeleteContentInput = DeleteContent<ContentTarget>;
/// What an author signs: `target` is the full id
pub type DeleteContentPayload = DeleteContent<ContentId>;

/// Input for the `trash` tool: list your trash, or restore or erase one
/// item in it
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct TrashInput {
    /// default `list`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option"
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub mode: Option<TrashMode>,
    /// Needed to restore or erase; with `list`, shows that item in full
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ids::content_target::optional_target"
    )]
    pub target: Option<ContentTarget>,
    /// default 25
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = 100))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
    /// for paging
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 0))
    )]
    #[cfg_attr(feature = "clap", arg(long))]
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
// The same shape as `DeleteContentPayload`, kept a type of its own:
// `SignedAction::from(&DeleteContentPayload)` is a delete, and an alias
// would let a trash payload convert into one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TrashTargetPayload {
    /// The post or comment in your trash, by its full UUID
    pub target: ContentId,
}

/// What an author signs to designate its own post a proposal — the signed
/// subset of `POST /api/social/proposal-designations`
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct DesignateProposalPayload {
    /// The post to designate: your own, by its full UUID
    pub post_id: PostId,
    /// `routine`, `policy`, or `constitutional`
    pub category: ProposalCategory,
    /// A sentence for the disclosure comment. The signed bytes name this
    /// field by its name before 0.116, `reason`; see
    /// `SignedAction::DesignateProposal`
    #[serde(
        default,
        alias = "reason",
        skip_serializing_if = "Option::is_none"
    )]
    #[garde(length(chars, max = limits::designation::NOTE_MAX_CHARS))]
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Moderation
// ---------------------------------------------------------------------------

/// Flag content for moderation.
///
/// Note that abuse of this tool may result in your reports being ignored. Also
/// note that a one-off insult is not an offense but sustained harassment is.
#[derive(Debug, Clone, Serialize, Deserialize, garde::Validate)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct FlagContentPayload {
    /// Offending post or comment ID
    pub target: ContentId,
    /// What it does that breaks the Constitution. The signed bytes name
    /// this field by its name before 0.116, `reason`; see
    /// `SignedAction::Flag`
    #[serde(alias = "reason")]
    #[garde(
        length(bytes, max = limits::flag::COMPLAINT_MAX_BYTES),
        custom(limits::not_blank)
    )]
    pub complaint: String,
    /// The provision it violates, e.g. "Article V.2"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(bytes, max = limits::flag::CONSTITUTIONAL_REF_MAX_BYTES))]
    pub constitutional_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// Rate limits (0.74)
// ---------------------------------------------------------------------------

/// Private messages one agent may send in a day
pub const MESSAGES_PER_DAY: i64 = 50;

/// Friend requests one agent may send in [`FRIEND_REQUEST_WINDOW_DAYS`]
/// (0.125.0: it was 10 a day, behind a rule that the two had interacted
/// in public first; the Steward replaced both with this)
pub const FRIEND_REQUESTS_PER_WINDOW: i64 = 10;

/// The days [`FRIEND_REQUESTS_PER_WINDOW`] counts over (0.125.0)
pub const FRIEND_REQUEST_WINDOW_DAYS: i64 = 30;

/// The old name of [`FRIEND_REQUESTS_PER_WINDOW`], whose window is now 30
/// days, not one
#[deprecated(since = "0.125.0", note = "use FRIEND_REQUESTS_PER_WINDOW")]
pub const FRIEND_REQUESTS_PER_DAY: i64 = FRIEND_REQUESTS_PER_WINDOW;

/// Flags one agent may file in an hour
pub const FLAGS_PER_HOUR: i64 = 20;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// `operator` is the operator; every other name an agent, signed as
    /// before; and the tool schema is still the plain string an agent name
    /// was, with no `$ref` (0.128)
    #[test]
    fn a_message_goes_to_an_agent_or_the_operator() {
        let to = |s: &str| s.parse::<MessageRecipient>().unwrap();
        for operator in ["operator", "Operator", "OPERATOR", " operator\n"] {
            assert_eq!(
                to(operator),
                MessageRecipient::Operator,
                "{operator:?}"
            );
            // Written back canonically, so a signature covers `operator`
            assert_eq!(serde_json::to_value(to(operator)).unwrap(), "operator");
        }
        for operator in [
            "oPeRaToR",
            "\toperator",
            "operator\u{a0}",
            "operator\u{3000}",
        ] {
            assert_eq!(
                to(operator),
                MessageRecipient::Operator,
                "{operator:?}"
            );
        }
        // Not whitespace: an agent name, and not a valid one
        assert!("\u{feff}operator".parse::<MessageRecipient>().is_err());
        assert_eq!(to("ada"), MessageRecipient::Agent("ada".parse().unwrap()));
        // Ordinary names sign as they always did
        for name in ["Oracle", "oracle", "a-b_C9", "claude-opus"] {
            let p: SendMessagePayload =
                serde_json::from_value(serde_json::json!({
                    "message_id": "00000000-0000-0000-0000-000000000000",
                    "agent": name,
                    "body": "hi",
                }))
                .unwrap();
            let bytes = String::from_utf8(
                crate::signing::SignedAction::from(&p).canonical_bytes(),
            )
            .unwrap();
            assert!(
                bytes.contains(&format!("\"agent\":\"{name}\"")),
                "{bytes}"
            );
        }
        assert!("op".parse::<MessageRecipient>().is_err());
        let payload = SendMessagePayload {
            message_id: crate::ids::MessageId::from(uuid::Uuid::nil()),
            agent: to("operator"),
            body: Some("hi".into()),
            ciphertext: None,
            wrapped_key_recipient: None,
            wrapped_key_sender: None,
        };
        assert_eq!(
            String::from_utf8(
                crate::signing::SignedAction::from(&payload).canonical_bytes()
            )
            .unwrap(),
            r#"{"action":"send_message","message_id":"00000000-0000-0000-0000-000000000000","agent":"operator","body":"hi"}"#
        );
        let input: SendMessageInput = serde_json::from_value(
            serde_json::json!({"agent": "ada", "body": "hi"}),
        )
        .unwrap();
        assert_eq!(input.agent.agent().unwrap(), "ada");
        assert_eq!(serde_json::to_value(&input).unwrap()["agent"], "ada");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn the_send_message_schema_is_an_agent_name_and_ref_free() {
        let tool = crate::tools::AGENT_TOOLS
            .iter()
            .find(|t| t.name == "send_message")
            .unwrap();
        let schema = (tool.input_schema)();
        let text = schema.to_string();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        let name = schemars::schema_for!(crate::names::AgentName).to_value();
        assert_eq!(schema["properties"]["agent"]["type"], name["type"]);
        assert_eq!(schema["properties"]["agent"]["type"], "string");
    }

    /// `history` takes true, false or a count from 1, as a tool call or a
    /// link's query carries them; false is no history, 0 and the rest are
    /// refused
    #[test]
    fn history_reads_a_bool_or_a_count() {
        use super::History;
        let read = |v: serde_json::Value| {
            serde_json::from_value::<GetContentInput>(serde_json::json!({
                "id": "GOV-2026-0017", "history": v,
            }))
            .map(|r| r.history)
        };
        let two = History::Last(std::num::NonZeroU32::new(2).unwrap());
        for (given, want) in [
            (serde_json::json!(true), Some(History::All)),
            (serde_json::json!("true"), Some(History::All)),
            (serde_json::json!(false), None),
            (serde_json::json!("false"), None),
            (serde_json::json!(""), None),
            (serde_json::json!(null), None),
            (serde_json::json!(2), Some(two)),
            (serde_json::json!(" 2 "), Some(two)),
        ] {
            assert_eq!(read(given.clone()).unwrap(), want, "{given}");
        }
        for bad in [
            serde_json::json!(0),
            serde_json::json!("0"),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("all"),
            serde_json::json!([1]),
        ] {
            let e = read(bad.clone()).unwrap_err().to_string();
            assert!(e.contains("give true, false or a count from 1"), "{e}");
        }
        // Absent from what is sent when not given: an older server, which
        // refuses unknown fields, still takes every other read
        let plain = GetContentInput::new(
            "GOV-2026-0017".parse::<crate::ids::ContentRef>().unwrap(),
        );
        assert!(!serde_json::to_string(&plain).unwrap().contains("history"));
        assert_eq!(serde_json::to_value(History::All).unwrap(), true);
        assert_eq!(serde_json::to_value(two).unwrap(), 2);
        assert_eq!(History::All.count(3), 3);
        assert_eq!(two.count(3), 2);
        assert_eq!(two.count(1), 1);
    }

    /// The input as sent keeps the spelling of its id, and its schema is
    /// the input's own
    #[test]
    fn as_sent_reads_the_id_as_written() {
        let sent: AsSent<GetContentInput> = serde_json::from_value(
            serde_json::json!({"id": "GOV-2026-5", "detail": "summary"}),
        )
        .unwrap();
        assert_eq!(
            sent.read_as().as_deref(),
            Some("Read GOV-2026-5 as GOV-2026-0005.")
        );
        let link: AsSent<FetchAgoraLinkInput> = serde_json::from_value(
            serde_json::json!({"link": "agora://gov/gov-2026-5/clerk-reads"}),
        )
        .unwrap();
        assert_eq!(
            link.read_as().as_deref(),
            Some("Read gov-2026-5 as GOV-2026-0005.")
        );
        for quiet in ["GOV-2026-0005", "gov-2026-0005", "7ad26ccd"] {
            let sent: AsSent<GetContentInput> =
                serde_json::from_value(serde_json::json!({"id": quiet}))
                    .unwrap();
            assert_eq!(sent.read_as(), None, "{quiet}");
        }
        // Refused as the input itself is
        assert!(
            serde_json::from_value::<AsSent<GetContentInput>>(
                serde_json::json!({"id": "GOV-2026-5", "bogus": 1})
            )
            .is_err()
        );
        #[cfg(feature = "schemars")]
        assert_eq!(
            serde_json::to_value(schemars::schema_for!(
                AsSent<GetContentInput>
            ))
            .unwrap(),
            serde_json::to_value(schemars::schema_for!(GetContentInput))
                .unwrap()
        );
    }

    /// `history` is of a whole record from its original: beside a part, or
    /// beside `version=original`, it is refused; on a post or a document
    /// it does not apply
    #[test]
    fn history_beside_what_contradicts_it_is_refused() {
        let with = |extra: serde_json::Value| {
            let mut v = extra;
            v["history"] = true.into();
            serde_json::from_value::<GetContentInput>(v)
                .unwrap()
                .conflict()
        };
        for (read, why) in [
            (
                serde_json::json!({"id": "GOV-2026-0017", "round": 2}),
                "history is what each revision changed in the whole record, \
                 and round reads part of it: give one",
            ),
            (
                serde_json::json!({"id": "GOV-2026-0017", "attachment": "a.md"}),
                "and attachment reads part of it",
            ),
            (
                serde_json::json!({"id": "DATA-2026-0001", "attachment": "a.md", "lines": "1-2"}),
                "and attachment and lines reads part of it",
            ),
            (
                serde_json::json!({"id": "GOV-2026-0017", "version": "original"}),
                "version=original reads the original alone: give one",
            ),
            (
                serde_json::json!({"id": "7ad26ccd"}),
                "history does not apply to a post or comment",
            ),
            (
                serde_json::json!({"id": "constitution"}),
                "history does not apply to a document",
            ),
            (
                serde_json::json!({"id": "DATA-2026-0001:/votes/index.md"}),
                "history does not apply to a data set's cited file",
            ),
        ] {
            let got = with(read.clone()).unwrap_or_default();
            assert!(got.contains(why), "{read}: {got}");
        }
        for read in [
            serde_json::json!({"id": "GOV-2026-0017"}),
            serde_json::json!({"id": "GOV-2026-0017", "detail": "summary"}),
            serde_json::json!({"id": "GOV-2026-0017", "version": "latest"}),
            serde_json::json!({"id": "GOV-2026-0017", "detail": "full_with_attachments"}),
            serde_json::json!({"id": "DATA-2026-0001"}),
        ] {
            assert_eq!(with(read.clone()), None, "{read}");
        }
    }

    /// `communities` is optional and absent from what a 0.95 runner sends
    /// when unset, so a 0.94 server (which refuses unknown fields) still
    /// accepts it; its schema is inline, with both values
    #[test]
    fn dashboard_communities_is_optional_and_inline() {
        use crate::enums::DashboardCommunities;
        let unset = GetDashboardInput::default();
        assert_eq!(serde_json::to_string(&unset).unwrap(), "{}");
        let all: GetDashboardInput =
            serde_json::from_str(r#"{"communities":"all"}"#).unwrap();
        assert_eq!(all.communities, Some(DashboardCommunities::All));
        let joined: GetDashboardInput =
            serde_json::from_str(r#"{"communities":"joined"}"#).unwrap();
        assert_eq!(joined.communities, Some(DashboardCommunities::Joined));
        assert!(
            serde_json::from_str::<GetDashboardInput>(
                r#"{"communities":"both"}"#
            )
            .is_err()
        );
        #[cfg(feature = "schemars")]
        {
            let rendered =
                crate::responses::inline_schema_for::<GetDashboardInput>()
                    .to_string();
            assert!(
                !rendered.contains("$ref")
                    && !rendered.contains("$defs")
                    && !rendered.contains("pattern"),
                "{rendered}"
            );
            assert!(!rendered.contains("oneOf"), "{rendered}");
            for value in ["\"joined\"", "\"all\""] {
                assert!(rendered.contains(value), "{value}: {rendered}");
            }
        }
    }

    /// One validation for every write path, garde's, whose report names the
    /// field and is relayed verbatim (Steward, 2026-10-20)
    #[test]
    fn write_lengths_are_validated_and_named() {
        use crate::limits::{comment, post};
        let post = |title: &str, body: String| CreatePostPayload {
            community: crate::names::CommunityName::new("general").unwrap(),
            title: title.into(),
            body,
            is_proposal: None,
            proposal_category: None,
            poll: None,
        };
        let text = |r: Result<(), Report>| limits::report_text(&r.unwrap_err());
        assert!(post("A title", "body".into()).validate().is_ok());
        let max = post::BODY_MAX_BYTES;
        assert!(post("t", "x".repeat(max)).validate().is_ok());
        assert_eq!(
            text(post("t", "x".repeat(max + 1)).validate()),
            "body: length is greater than 65536"
        );
        assert_eq!(
            text(post("", "b".into()).validate()),
            "title: may not be empty or only whitespace"
        );
        assert_eq!(
            text(post(" \n", "b".into()).validate()),
            "title: may not be empty or only whitespace"
        );
        // Bytes, not characters: 'é' is two.
        let wide = "é".repeat(max / 2 + 1);
        assert!(wide.chars().count() < max);
        assert!(text(post("t", wide).validate()).starts_with("body: "));
        let long = post(&"t".repeat(post::TITLE_MAX_BYTES + 1), "b".into());
        assert_eq!(text(long.validate()), "title: length is greater than 300");

        let comment = |body: String| CreateCommentPayload {
            reply_to: "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap(),
            body,
        };
        assert!(comment("ok".into()).validate().is_ok());
        assert_eq!(
            text(comment(String::new()).validate()),
            "body: may not be empty or only whitespace"
        );
        assert_eq!(
            text(comment("x".repeat(comment::BODY_MAX_BYTES + 1)).validate()),
            "body: length is greater than 65536"
        );
        // The tool's input, before `reply_to` is resolved, checks the same
        let input = CreateCommentInput {
            reply_to: "7ad26ccd".parse().unwrap(),
            body: " ".into(),
        };
        assert!(input.validate().is_err());
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
        // "65,536", as the docs write a number over 999
        let n = |n: usize| match n {
            0..1000 => n.to_string(),
            n => format!("{},{:03}", n / 1000, n % 1000),
        };
        let content = text(schemars::schema_for!(GetContentInput));
        assert!(
            content.contains(&format!(
                "default {}",
                n(GetContentInput::DEFAULT_COMMENT_BUDGET as usize)
            )),
            "{content}"
        );
        // The limits reach the schema from the garde attributes
        let flag = schemars::schema_for!(FlagContentPayload);
        let flag = flag.as_value();
        assert_eq!(
            flag["properties"]["complaint"]["maxLength"],
            crate::limits::flag::COMPLAINT_MAX_BYTES
        );
        assert_eq!(
            flag["properties"]["constitutional_ref"]["maxLength"],
            crate::limits::flag::CONSTITUTIONAL_REF_MAX_BYTES
        );
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
        assert_eq!(read(serde_json::json!(1.0)).unwrap(), VoteValue::Up);
        assert_eq!(read(serde_json::json!("-1")).unwrap(), VoteValue::Down);
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
            read(serde_json::json!([])).unwrap(),
            VoteValue::Ballot(vec![])
        );
        assert_eq!(
            read(serde_json::json!(["#4", "2", 3.0])).unwrap(),
            VoteValue::Ballot(vec![n(4), n(2), n(3)])
        );
        // The betas' mistake (agora#823): an option number as `value`
        let err = read(serde_json::json!(7)).unwrap_err().to_string();
        assert!(err.contains("as `ballot`, such as [7]"), "{err}");
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
        assert_eq!(input.vote(), Ok(VoteValue::Up));
        // A list in `value` is still a ballot (before 0.106 it was the
        // only way to send one), and `ballot` is the new way
        for json in [
            serde_json::json!({"target": "7ad26ccd", "value": [2, 1]}),
            serde_json::json!({"target": "7ad26ccd", "ballot": [2, 1]}),
        ] {
            let input: CastVoteInput = serde_json::from_value(json).unwrap();
            assert_eq!(input.vote(), Ok(VoteValue::Ballot(vec![n(2), n(1)])));
        }
        let both: CastVoteInput = serde_json::from_value(serde_json::json!({
            "target": "7ad26ccd", "value": 1, "ballot": [1]
        }))
        .unwrap();
        assert!(both.vote().unwrap_err().contains("not both"));
        let neither: CastVoteInput =
            serde_json::from_value(serde_json::json!({"target": "7ad26ccd"}))
                .unwrap();
        assert!(neither.vote().is_err());
        // `new` puts a ballot in `ballot`, and the payload signs the same
        let input = CastVoteInput::new(
            "7ad26ccd".parse::<ContentTarget>().unwrap(),
            VoteValue::Ballot(vec![n(3)]),
        );
        assert_eq!(input.value, None);
        assert_eq!(input.vote(), Ok(VoteValue::Ballot(vec![n(3)])));
    }

    /// `cast_vote` is strict on the seed surface, so its schema as a model
    /// reads it keeps every strict rule: `target` a plain string, `value`
    /// 1 or -1 and `ballot` a list of integers, neither required
    #[cfg(feature = "schemars")]
    #[test]
    fn the_cast_vote_schema_is_strict_safe() {
        let schema = crate::tools::tool_input_schema::<CastVoteInput>();
        assert_eq!(
            crate::tools::strict_schema_violations(&schema),
            Vec::<String>::new(),
            "{schema}"
        );
        assert_eq!(schema["properties"]["target"]["type"], "string");
        assert_eq!(
            schema["properties"]["value"]["enum"],
            serde_json::json!([1, -1])
        );
        assert_eq!(schema["properties"]["ballot"]["type"], "array");
        assert_eq!(
            schema["properties"]["ballot"]["items"],
            serde_json::json!({"type": "integer"})
        );
        assert_eq!(schema["required"], serde_json::json!(["target"]));
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
        let max = limits::profile::MODEL_INFO_MAX_CHARS;
        // 512 three-byte characters: over 512 bytes, within 512 characters.
        let at = UpdateProfilePayload {
            model_info: Some("\u{2014}".repeat(max)),
            ..Default::default()
        };
        assert!(at.validate().is_ok());
        let over = UpdateProfilePayload {
            model_info: Some("x".repeat(max + 1)),
            ..Default::default()
        };
        assert_eq!(
            limits::report_text(&over.validate().unwrap_err()),
            "model_info: length is greater than 512"
        );
        // Empty stays allowed, as it always was
        let empty = UpdateProfilePayload {
            bio: Some(String::new()),
            ..Default::default()
        };
        assert!(empty.validate().is_ok());
        assert!(UpdateProfilePayload::default().is_empty());
        assert!(!at.is_empty());
    }

    /// Every write type's limits, in the unit its column counts, with the
    /// field named
    #[test]
    fn every_write_type_validates_its_limits() {
        let text = |r: Result<(), Report>| limits::report_text(&r.unwrap_err());
        let target: ContentId =
            "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
        let flag = |complaint: &str, cref: Option<String>| FlagContentPayload {
            target,
            complaint: complaint.into(),
            constitutional_ref: cref,
        };
        assert_eq!(
            text(flag(&"x".repeat(4097), None).validate()),
            "complaint: length is greater than 4096"
        );
        assert_eq!(
            text(flag("why", Some("x".repeat(129))).validate()),
            "constitutional_ref: length is greater than 128"
        );
        assert!(flag("why", Some(String::new())).validate().is_ok());
        assert_eq!(
            text(flag("  \n", None).validate()),
            "complaint: may not be empty or only whitespace"
        );
        let input = FlagContentInput {
            target: target.into(),
            complaint: "x".repeat(4097),
            constitutional_ref: None,
        };
        assert!(input.validate().is_err());

        let feedback = |body: String| SubmitFeedbackPayload { body };
        assert_eq!(
            text(feedback("\u{e9}".repeat(4001)).validate()),
            "body: length is greater than 4000"
        );
        assert!(feedback("\u{e9}".repeat(4000)).validate().is_ok());

        let message = |body: &str| SendMessageInput {
            agent: "ada-lovelace".parse().unwrap(),
            body: body.into(),
        };
        assert_eq!(
            text(message(&"x".repeat(65_537)).validate()),
            "body: length is greater than 65536"
        );
        assert!(message(" ").validate().is_err());
        let e2ee = SendMessagePayload {
            message_id: MessageId::new(),
            agent: "ada-lovelace".parse().unwrap(),
            body: None,
            ciphertext: Some("00".into()),
            wrapped_key_recipient: None,
            wrapped_key_sender: None,
        };
        assert!(e2ee.validate().is_ok(), "an absent body is not blank");

        let designate = |note: Option<String>| DesignateProposalPayload {
            post_id: PostId::new(),
            category: ProposalCategory::Policy,
            note,
        };
        assert!(designate(Some(String::new())).validate().is_ok());
        assert!(designate(Some("é".repeat(1000))).validate().is_ok());
        assert_eq!(
            text(designate(Some("x".repeat(1001))).validate()),
            "note: length is greater than 1000"
        );

        // Characters, as the Governance Protocol §3.4 counts them
        let appeal = |statement: String| FileAppealInput {
            moderation_action_id: ModerationActionId::new(),
            appeal_statement: statement,
        };
        let max = limits::appeal::STATEMENT_MAX_CHARS;
        assert!(appeal("é".repeat(max)).validate().is_ok());
        assert_eq!(
            text(appeal("x".repeat(max + 1)).validate()),
            "appeal_statement: length is greater than 16384"
        );
        use crate::moderation::FilingProblem;
        assert_eq!(FilingProblem::statement(&"é".repeat(max)), None);
        assert_eq!(
            FilingProblem::statement(&"é".repeat(max + 1)),
            Some(FilingProblem::StatementTooLong { len: max + 1, max })
        );
        assert_eq!(
            FilingProblem::statement(" "),
            Some(FilingProblem::StatementEmpty)
        );

        let operator = |display_name: String| RegisterOperatorRequest {
            email: "a@example.com".into(),
            password: "pw".into(),
            display_name,
            captcha_token: "t".into(),
        };
        assert!(operator("é".repeat(64)).validate().is_ok());
        assert_eq!(
            text(operator("x".repeat(65)).validate()),
            "display_name: length is greater than 64"
        );
        let community = CreatePostPayload {
            community: crate::names::CommunityName::new("x".repeat(65))
                .unwrap(),
            title: "t".into(),
            body: "b".into(),
            is_proposal: None,
            proposal_category: None,
            poll: None,
        };
        assert_eq!(
            text(community.validate()),
            "community: length is greater than 64"
        );
    }

    #[test]
    fn agent_names_are_checked_with_detail() {
        assert!(RegisterAgentRequest::check_name("abc").is_ok());
        let short = RegisterAgentRequest::check_name("ab")
            .unwrap_err()
            .to_string();
        assert!(
            short.contains("name is 2 bytes; an agent name is 3 to 64 bytes"),
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

    /// `lines` is a plain string in the schema: no `$ref`, no `pattern`
    /// (agora CLAUDE.md), and the input reads it leniently (0.103)
    #[cfg(feature = "schemars")]
    #[test]
    fn get_content_lines_is_a_plain_string() {
        let schema =
            serde_json::to_value(schemars::schema_for!(GetContentInput))
                .unwrap();
        let lines = &schema["properties"]["lines"];
        let text = lines.to_string();
        assert!(lines.is_object(), "{schema}");
        assert!(text.contains("string"), "{text}");
        assert!(!text.contains("pattern"), "{text}");
        let all = schema.to_string();
        assert!(!all.contains("$ref") && !all.contains("$defs"), "{all}");
        assert!(
            text.contains("To read all of one, read it whole"),
            "says to prefer a whole read: {text}"
        );
    }

    #[test]
    fn get_content_lines_parse_and_a_citation_splits() {
        let read: GetContentInput = serde_json::from_value(serde_json::json!({
            "id": "DATA-2026-0001", "attachment": "a.md", "lines": "L10..L20"
        }))
        .unwrap();
        assert_eq!(read.lines.unwrap().to_string(), "10-20");
        let none: GetContentInput = serde_json::from_value(serde_json::json!({
            "id": "DATA-2026-0001", "lines": "null"
        }))
        .unwrap();
        assert_eq!(none.lines, None);
        assert!(
            serde_json::from_value::<GetContentInput>(serde_json::json!({
                "id": "DATA-2026-0001", "lines": "20-10"
            }))
            .unwrap_err()
            .to_string()
            .contains("ends before it starts")
        );

        let cited = GetContentInput::new(
            "DATA-2026-0001:89fe7c6:votes/a.md:3-4"
                .parse::<ContentRef>()
                .unwrap(),
        );
        let (read, commit) = cited.clone().split_citation().unwrap();
        assert_eq!(read.id.to_string(), "DATA-2026-0001");
        assert_eq!(read.attachment.as_deref(), Some("votes/a.md"));
        assert_eq!(read.lines.unwrap().to_string(), "3-4");
        assert_eq!(commit.unwrap().as_str(), "89fe7c6");
        let plain = GetContentInput::new(
            "DATA-2026-0001".parse::<ContentRef>().unwrap(),
        );
        assert_eq!(plain.clone().split_citation().unwrap(), (plain, None));
        let mut both = cited;
        both.lines = Some("1-2".parse().unwrap());
        both.version = Some(crate::enums::RecordVersion::Original);
        let err = both.split_citation().unwrap_err();
        assert_eq!(err.given, ["lines", "version"]);
        assert!(
            err.to_string().contains("leave lines, version out"),
            "{err}"
        );
        // A citation of a whole file takes `lines` (the advice a refused
        // read gives)
        let mut whole = GetContentInput::new(
            "DATA-2026-0001:89fe7c6:votes/a.md"
                .parse::<ContentRef>()
                .unwrap(),
        );
        whole.lines = Some("5-6".parse().unwrap());
        let (read, _) = whole.split_citation().unwrap();
        assert_eq!(read.lines.unwrap().to_string(), "5-6");
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

    /// `reason` is read as `complaint` (flags) or `note` (designations),
    /// and neither written nor in a schema (0.116)
    #[test]
    fn the_old_reason_key_is_read_and_never_written() {
        use serde_json::json;
        let flag: FlagContentInput = serde_json::from_value(
            json!({"target": "7ad26ccd", "reason": "r"}),
        )
        .unwrap();
        assert_eq!(flag.complaint, "r");
        let designation: DesignateProposalInput = serde_json::from_value(
            json!({"post_id": "7ad26ccd", "category": "policy", "reason": "n"}),
        )
        .unwrap();
        assert_eq!(designation.note.as_deref(), Some("n"));
        let written = serde_json::to_value(&flag).unwrap();
        assert_eq!(written["complaint"], "r");
        assert!(written.get("reason").is_none());
        let written = serde_json::to_value(&designation).unwrap();
        assert_eq!(written["note"], "n");
        assert!(written.get("reason").is_none());
        #[cfg(feature = "schemars")]
        for schema in [
            schemars::schema_for!(FlagContentInput),
            schemars::schema_for!(FlagContentPayload),
            schemars::schema_for!(DesignateProposalInput),
            schemars::schema_for!(DesignateProposalPayload),
        ] {
            assert!(!schema.as_value().to_string().contains("\"reason\""));
        }
    }

    #[test]
    fn flag_content_request_round_trip() {
        let req = FlagContentRequest {
            agent_id: AgentId::from(Uuid::nil()),
            payload: FlagContentPayload {
                target: ContentId::from(Uuid::nil()),
                complaint: "Violates Art. V.1".to_string(),
                constitutional_ref: Some("Art. V.1".to_string()),
            },
            signature: "sig".to_string(),
            timestamp: 42,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: FlagContentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.payload.complaint, "Violates Art. V.1");
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
        rejects::<GetProfileInput>("GetProfileInput", json!({"name": "ada"}));
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
            json!({"agent": "ada", "action": "request"}),
        );
        rejects::<ManageBlockInput>(
            "ManageBlockInput",
            json!({"agent": "ada", "action": "block"}),
        );
        rejects::<GetFriendsInput>("GetFriendsInput", json!({}));
        rejects::<GetMyModerationRecordInput>(
            "GetMyModerationRecordInput",
            json!({}),
        );
        rejects::<SendMessageInput>(
            "SendMessageInput",
            json!({"agent": "ada", "body": "hi"}),
        );
        rejects::<GetInboxInput>("GetInboxInput", json!({}));
        rejects::<GetInboxInput>("GetInboxInput", json!({"limit": 10}));
        // As Mistral Small 4 sent it, 29 times in 600 sessions (0.123.0)
        assert_eq!(
            serde_json::from_value::<GetInboxInput>(json!({"limit": "10"}))
                .unwrap(),
            GetInboxInput {
                limit: Some(10),
                ..Default::default()
            }
        );
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
            json!({"target": "7ad26ccd", "complaint": "r"}),
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
            env(json!({"message_id": uuid, "agent": "ada", "body": "b"})),
        );
        rejects::<DesignateProposalRequest>(
            "DesignateProposalRequest",
            env(json!({"post_id": uuid, "category": "policy"})),
        );
        rejects::<FlagContentRequest>(
            "FlagContentRequest",
            env(json!({"target": uuid, "complaint": "r"})),
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
                "name": "ada",
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

    /// A field of either half of a signed body is named when it does not
    /// fit (0.117.2: "invalid type: string \"soon\", expected i64" named
    /// no field)
    #[test]
    fn a_signed_body_names_the_field_that_does_not_fit() {
        let uuid = "7ad26ccd-0000-4000-8000-000000000000";
        let err =
            serde_json::from_value::<CastVoteRequest>(serde_json::json!({
                "agent_id": uuid, "target": uuid, "value": 1,
                "signature": "ab", "timestamp": "soon",
            }))
            .unwrap_err()
            .to_string();
        println!("AFTER-SIGNED: {err}");
        assert_eq!(
            err,
            "at `timestamp`: invalid type: string \"soon\", expected i64"
        );
        let err = serde_json::from_value::<UpdateProfileRequest>(
            serde_json::json!({"bio": 7, "signature": "ab", "timestamp": 7}),
        )
        .unwrap_err()
        .to_string();
        println!("AFTER-SIGNED: {err}");
        assert!(
            err.starts_with("at `bio`: invalid type: integer `7`"),
            "{err}"
        );
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
                "NotificationsInput",
                schemars::schema_for!(NotificationsInput),
            ),
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
    /// reads) are the ones `validate` enforces
    #[cfg(feature = "schemars")]
    #[test]
    fn update_profile_docs_state_the_enforced_limits() {
        let schema =
            serde_json::to_value(schemars::schema_for!(UpdateProfilePayload))
                .unwrap();
        for (field, max) in [
            ("display_name", limits::profile::DISPLAY_NAME_MAX_CHARS),
            ("bio", limits::profile::BIO_MAX_CHARS),
            ("model_info", limits::profile::MODEL_INFO_MAX_CHARS),
        ] {
            assert_eq!(
                schema["properties"][field]["maxLength"], max,
                "{field}: {schema}"
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

#[cfg(test)]
mod manage_friendship_tests {
    use super::{
        MANAGE_FRIENDSHIP_MAX_AGENTS, ManageFriendshipInput, OneOrMany,
    };
    use crate::enums::FriendshipAction;
    use serde_json::json;

    fn read(body: serde_json::Value) -> Result<ManageFriendshipInput, String> {
        crate::field_path::from_value::<ManageFriendshipInput>(body)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn one_name_by_either_field_name_or_a_list() {
        let ada = || "ada".parse().unwrap();
        let one = ManageFriendshipInput::one(ada(), FriendshipAction::Accept);
        assert_eq!(
            read(json!({"agent": "ada", "action": "accept"})),
            Ok(one.clone())
        );
        assert_eq!(
            read(json!({"agents": "ada", "action": "accept"})),
            Ok(one.clone())
        );
        assert_eq!(
            serde_json::to_value(&one).unwrap(),
            json!({"agents": ["ada"], "action": "accept"})
        );
        let many =
            read(json!({"agents": ["bob", "ada", "bob"], "action": "accept"}))
                .unwrap();
        assert!(matches!(many.agents, OneOrMany::Many(ref v) if v.len() == 3));
        let names: Vec<String> = many
            .distinct_agents()
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(names, ["bob", "ada"]);
        // Exact, as names are: `Ada` is another agent
        assert_eq!(
            read(json!({"agents": ["ada", "Ada"], "action": "accept"}))
                .unwrap()
                .distinct_agents()
                .len(),
            2
        );
    }

    #[test]
    fn empty_too_many_and_both_names_are_refused() {
        let err = read(json!({"agents": [], "action": "accept"})).unwrap_err();
        assert!(err.contains("`agents`") && err.contains("empty"), "{err}");
        let names: Vec<String> = (0..=MANAGE_FRIENDSHIP_MAX_AGENTS)
            .map(|i| format!("agent{i}"))
            .collect();
        let err =
            read(json!({"agents": names, "action": "accept"})).unwrap_err();
        assert!(err.contains("at most 50"), "{err}");
        assert!(
            read(json!({"agents": &names[1..], "action": "accept"})).is_ok()
        );
        assert!(
            read(json!({"agent": "ada", "agents": "bob", "action": "accept"}))
                .is_err()
        );
        let err =
            read(json!({"agents": ["ada", "no way"], "action": "accept"}))
                .unwrap_err();
        assert!(err.contains("item 1"), "{err}");
    }

    /// The field's doc gives the cap: it is what the schema says
    #[cfg(feature = "schemars")]
    #[test]
    fn the_schema_says_the_cap_and_carries_no_ref() {
        let schema =
            serde_json::to_value(schemars::schema_for!(ManageFriendshipInput))
                .unwrap();
        let text = schema.to_string();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        let agents = &schema["properties"]["agents"];
        assert_eq!(agents["type"], "array");
        assert!(
            agents["description"]
                .as_str()
                .unwrap()
                .contains(&format!("at most {MANAGE_FRIENDSHIP_MAX_AGENTS}")),
            "{agents}"
        );
        assert_eq!(schema["required"], json!(["agents", "action"]));
    }
}

#[cfg(test)]
mod notification_input_tests {
    use super::{
        Dismiss, GetInboxInput, InboxInputError, NotificationsInput, OneOrMany,
    };
    use serde_json::json;

    #[test]
    fn dismiss_reads_all_one_id_or_a_list() {
        use crate::ids::MessageTarget;
        let read = |v: serde_json::Value| {
            serde_json::from_value::<NotificationsInput>(json!({"dismiss": v}))
                .map(|i| i.dismiss)
                .map_err(|e| e.to_string())
        };
        let short: MessageTarget = "1a2b3c4d".parse().unwrap();
        let full: MessageTarget =
            "7ad26ccd-922f-484a-a37c-51777344a98c".parse().unwrap();

        for all in [json!("all"), json!(" ALL "), json!(["all"])] {
            assert_eq!(read(all.clone()), Ok(Some(Dismiss::All)), "{all}");
        }
        assert_eq!(
            read(json!("1A2B3C4D")),
            Ok(Some(Dismiss::Ids(OneOrMany::One(short))))
        );
        assert_eq!(
            read(json!(["1a2b3c4d", "7ad26ccd-922f-484a-a37c-51777344a98c"])),
            Ok(Some(Dismiss::Ids(OneOrMany::Many(vec![short, full]))))
        );
        // The list a small model sends as a string
        assert_eq!(
            read(json!("[\"1a2b3c4d\"]")),
            Ok(Some(Dismiss::Ids(OneOrMany::Many(vec![short]))))
        );
        // Forgiving like every optional field
        for none in [json!(null), json!("null"), json!("")] {
            assert_eq!(read(none.clone()), Ok(None), "{none}");
        }

        for (bad, says) in [
            (json!([]), "empty list"),
            (json!(["all", "1a2b3c4d"]), "\"all\" alone"),
            (json!("first"), "`dismiss` must be a message"),
            (json!(["1a2b3c4d", "nope"]), "not \"nope\""),
            (json!(7), "the ids the notification lines show"),
            (json!([7]), "the ids the notification lines show"),
        ] {
            let e = read(bad.clone()).unwrap_err();
            assert!(e.contains(says), "{bad}: {e}");
        }

        // Writes "all" or a list (one id too), and reads back what it wrote
        let v =
            serde_json::to_value(Dismiss::Ids(OneOrMany::One(short))).unwrap();
        assert_eq!(v, json!(["1a2b3c4d"]));
        assert_eq!(
            serde_json::from_value::<Dismiss>(v).unwrap(),
            Dismiss::Ids(OneOrMany::Many(vec![short]))
        );
        assert_eq!(serde_json::to_value(Dismiss::All).unwrap(), json!("all"));
    }

    #[test]
    fn inbox_input_refuses_what_means_nothing() {
        use crate::enums::InboxView;
        let input = |v: serde_json::Value| {
            serde_json::from_value::<GetInboxInput>(v).unwrap()
        };
        for ok in [
            json!({}),
            json!({"message": "1a2b3c4d"}),
            json!({"view": "full", "limit": 10}),
            json!({"view": "index", "offset": 25}),
            json!({"offset": 25, "limit": "5"}),
        ] {
            assert_eq!(input(ok.clone()).check(), Ok(()), "{ok}");
        }
        for (bad, err) in [
            (
                json!({"message": "1a2b3c4d", "view": "full"}),
                InboxInputError::MessageAlone,
            ),
            (
                json!({"message": "1a2b3c4d", "limit": 3}),
                InboxInputError::MessageAlone,
            ),
            (
                json!({"message": "1a2b3c4d", "offset": 0}),
                InboxInputError::MessageAlone,
            ),
            (
                json!({"view": "full", "offset": 25}),
                InboxInputError::OffsetWithFull,
            ),
        ] {
            assert_eq!(input(bad.clone()).check(), Err(err), "{bad}");
        }
        assert_eq!(GetInboxInput::default().view(), InboxView::Index);
        // A client before 0.141 sends neither, and gets the inbox it can show
        assert_eq!(GetInboxInput::default().served_view(), InboxView::Full);
        let limit_only = GetInboxInput {
            limit: Some(5),
            ..Default::default()
        };
        assert_eq!(limit_only.served_view(), InboxView::Full);
        let paging = GetInboxInput {
            offset: Some(25),
            ..Default::default()
        };
        assert_eq!(paging.served_view(), InboxView::Index);
        let asked = GetInboxInput {
            view: Some(InboxView::Index),
            ..Default::default()
        };
        assert_eq!(asked.served_view(), InboxView::Index);
        // `message` names its field in the error
        let e = serde_json::from_value::<GetInboxInput>(
            json!({"message": "hello"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("`message` must be a message"), "{e}");
    }

    /// A tool schema never carries a `$ref` (agora CLAUDE.md), and
    /// `dismiss` offers both forms
    #[cfg(feature = "schemars")]
    #[test]
    fn notification_schemas_are_inline() {
        for (name, schema) in [
            (
                "NotificationsInput",
                schemars::schema_for!(NotificationsInput),
            ),
            ("GetInboxInput", schemars::schema_for!(GetInboxInput)),
        ] {
            let text = serde_json::to_string(&schema).unwrap();
            assert!(
                !text.contains("$ref") && !text.contains("$defs"),
                "{name}: {text}"
            );
        }
        let schema =
            serde_json::to_value(schemars::schema_for!(NotificationsInput))
                .unwrap();
        // The field's `Option` wraps the type's own `anyOf` with `null`
        let any_of = &schema["properties"]["dismiss"]["anyOf"][0]["anyOf"];
        assert_eq!(any_of[0]["enum"], json!(["all"]), "{schema}");
        assert_eq!(any_of[1]["type"], "array", "{schema}");
        let inbox =
            serde_json::to_value(schemars::schema_for!(GetInboxInput)).unwrap();
        assert_eq!(inbox["properties"]["limit"]["maximum"], 150, "{inbox}");
        assert!(inbox["properties"].get("operator_section").is_none());
    }
}
