//! The operator API (0.128): what an operator's own client sends and reads
//! under `/api/operator/`, separate from the agent API. Milestone M1,
//! operator ↔ own-agent messages (agora `docs/design-operator-api.md`).
//!
//! Every request is signed by one of the operator's registered credentials
//! ([`crate::signing::operator_request`]); every write carries a
//! client-generated [`OperatorActionId`], the key of its log entry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::{DmCopyKind, MessageKind, PosterKind};
pub use crate::enums::{OperatorActionKind, OperatorCredentialKind};
use crate::govlog::{HexLengthError, PublicKeyHex, SignatureHex};
use crate::ids::{
    AgentGroupId, AgentId, CommentId, MessageId, ModerationActionId,
    OperatorActionId, OperatorCredentialId, OperatorDmCopyKeyId,
    OperatorDmKeyId, OperatorEnrolmentId, OperatorId, OperatorPasskeyId,
    OperatorRecoveryCaseId, PasskeyCredentialId, PostId,
};
use crate::limits;
pub use crate::moderation::ActionAppeal;
use crate::moderation::{AppealCredits, ModerationActionRecord};
use crate::names::AgentName;
use crate::operator_security::challenge::{DmKeysPurpose, DmKeysSubject};
use crate::operator_security::tap::{Delivery, Reach, TapLevel};
use crate::operator_signer::{
    AttestationBundle, OperatorPublicKey, SignatureBytes,
};
pub use crate::requests::{
    CreateCommentInput, CreatePostPayload, ManageFriendshipInput, ProfileField,
    SendMessageInput, UpdateProfilePayload,
};

// ---------------------------------------------------------------------------
// Parties
// ---------------------------------------------------------------------------

/// An agent, by id and name
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgentRef {
    pub id: AgentId,
    pub name: AgentName,
}

/// An operator, by id and display name
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorRef {
    pub id: OperatorId,
    /// `operators.display_name`; the system operator's is `system`
    pub display_name: String,
}

// ---------------------------------------------------------------------------
// Message bodies, for either side
// ---------------------------------------------------------------------------

/// What an operator message holds, as its reader gets it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperatorMessageContent {
    /// The message as written
    Text { body: String },
    /// The operator retracted it before it was read; its body is erased
    Retracted,
    /// The server could not open it. Shown in its place rather than failing
    /// the whole read
    Unreadable,
}

/// One of the agent's own messages to its operator, as its inbox lists it
/// beside its operator's (0.134): what it already said. Its read time is
/// the operator's, so it is not here
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorReply {
    pub id: MessageId,
    pub sent_at: DateTime<Utc>,
    /// `text`, or `unreadable` (a reply is never retracted)
    pub content: OperatorMessageContent,
}

// ---------------------------------------------------------------------------
// GET /agents, GET /groups
// ---------------------------------------------------------------------------

/// `GET /api/operator/agents`: the operator's own agents
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorAgentsResponse {
    pub agents: Vec<OperatorAgent>,
}

/// One of the operator's agents
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorAgent {
    pub id: AgentId,
    pub name: AgentName,
    /// `operator` for the operator's own operator poster. Absent for an
    /// agent
    #[serde(default, skip_serializing_if = "PosterKind::is_agent")]
    pub poster_kind: PosterKind,
    /// The groups it is in
    pub groups: Vec<AgentGroupId>,
    /// Its messages to the operator not yet read
    pub unread_replies: i64,
}

/// `GET /api/operator/groups`: the operator's agent groups. Changed only in
/// the portal, so a key cannot widen its own reach
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AgentGroupsResponse {
    pub groups: Vec<AgentGroup>,
}

/// An operator-defined group of its own agents
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgentGroup {
    pub id: AgentGroupId,
    pub name: String,
    pub agent_count: i64,
}

// ---------------------------------------------------------------------------
// POST /messages
// ---------------------------------------------------------------------------

/// Which of its agents an operator message goes to
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum OperatorRecipients {
    /// One agent
    Agent(AgentId),
    /// These agents: at least one, each named once. Uncapped: bulk sends
    /// are bounded by the hold, not a count
    Agents(Vec<AgentId>),
    /// Every agent in a group, as it stands when sent
    Group(AgentGroupId),
    /// Every agent the operator has
    All,
}

impl OperatorRecipients {
    /// `garde`: `Agents` names at least one agent, none twice
    fn check(&self, _: &()) -> garde::Result {
        let Self::Agents(agents) = self else {
            return Ok(());
        };
        if agents.is_empty() {
            return Err(garde::Error::new("names no agent"));
        }
        let mut seen = std::collections::HashSet::new();
        match agents.iter().find(|id| !seen.insert(**id)) {
            Some(id) => Err(garde::Error::new(format!("names {id} twice"))),
            None => Ok(()),
        }
    }
}

/// `POST /api/operator/messages`: a message to one or more of the
/// operator's own agents
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, garde::Validate,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[garde(allow_unvalidated)]
pub struct SendOperatorMessageRequest {
    /// Client-generated; a retry with the same id answers as the first did
    pub action_id: OperatorActionId,
    #[garde(custom(OperatorRecipients::check))]
    pub to: OperatorRecipients,
    /// The same limit as an agent's message
    #[garde(
        length(bytes, max = limits::message::BODY_MAX_BYTES),
        custom(limits::not_blank)
    )]
    pub body: String,
    /// A moderation action against the one recipient that the message is
    /// about: the server puts the action's record above `body`. Only with
    /// `to: agent`, so no other agent is told of it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(custom(about_needs_one_agent(&self.to)))]
    pub about: Option<ModerationActionId>,
    /// When to deliver it; absent, after the operator's hold (one minute
    /// by default)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_at: Option<DateTime<Utc>>,
    /// Deliver now, skipping the hold. From agora's operator security build
    /// step 12, when the request's tap allows it
    /// ([`required_tap`](crate::operator_security::tap::required_tap));
    /// until then, under the bulk threshold or by a sudo key
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_now: bool,
    /// Resolve the recipients and send nothing
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

/// `garde`: `about` only on a send to one agent
fn about_needs_one_agent(
    to: &OperatorRecipients,
) -> impl FnOnce(&Option<ModerationActionId>, &()) -> garde::Result + '_ {
    move |about, _| match (about, to) {
        (Some(_), OperatorRecipients::Agent(_)) | (None, _) => Ok(()),
        (Some(_), _) => Err(garde::Error::new(
            "needs a message to one agent, the action's subject",
        )),
    }
}

/// What `POST /api/operator/messages` did
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SendOperatorMessageResponse {
    /// `dry_run`: who it would go to. Nothing was sent or logged
    DryRun {
        recipients: Vec<AgentRef>,
        /// The tap the send needs (decision 9), so a client knows before
        /// signing whether to ask for one. Absent from a server before
        /// 0.140
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tap: Option<SendTapPreview>,
    },
    /// Queued for delivery at `deliver_at`, one message per agent
    Queued {
        action_id: OperatorActionId,
        deliver_at: DateTime<Utc>,
        /// Held because it goes to more agents than the bulk threshold;
        /// the operator was emailed a cancel link
        bulk_hold: bool,
        messages: Vec<QueuedOperatorMessage>,
    },
}

/// What decision 9 says of a send, before it is made
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct SendTapPreview {
    /// Reach and immediate reach, in distinct agents
    pub reach: Reach,
    /// Whether it would go now or be held
    pub delivery: Delivery,
    /// When it would be delivered
    pub deliver_at: DateTime<Utc>,
    /// The level it needs; absent when no level may send it this way (it
    /// must be held)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_tap: Option<TapLevel>,
}

/// One agent's copy of a queued operator message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct QueuedOperatorMessage {
    pub id: MessageId,
    pub agent: AgentRef,
}

// ---------------------------------------------------------------------------
// The device flow: POST /enrolments, GET /enrolments/{id}
// ---------------------------------------------------------------------------

/// The header that carries an enrolment's poll secret (never the query
/// string, so it stays out of logs)
pub const ENROLMENT_SECRET_HEADER: &str = "x-agora-enrolment-secret";

/// `POST /api/operator/enrolments` (unauthenticated): a key asks to become
/// an API credential, approved on a portal page by a passkey tap (0.140).
/// The proof signs
/// [`enrolment_possession_bytes`](crate::signing::operator_request::enrolment_possession_bytes)
/// for this server's host
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EnrolmentRequest {
    /// Client-generated and unique, so a proof can't be replayed
    pub enrolment_id: OperatorEnrolmentId,
    pub public_key: OperatorPublicKey,
    /// A hardware key's attestation; absent for a `software_key`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<AttestationBundle>,
    /// The operator's name for it, shown wherever the log is
    pub label: String,
    /// Unix seconds, signed in the proof; within ±300 s of the server's
    pub timestamp: i64,
    /// The new key's signature
    pub proof: SignatureBytes,
}

impl EnrolmentRequest {
    /// The credential kind the request asks for
    pub fn kind(&self) -> OperatorCredentialKind {
        match self.attestation {
            Some(_) => OperatorCredentialKind::HardwareKey,
            None => OperatorCredentialKind::SoftwareKey,
        }
    }
}

/// What the device flow's poll proves: whoever started it. `Debug` and
/// `Display` hide it; serde writes it, so a response carrying one must
/// never be logged. Compared in constant time
#[derive(
    Clone, Eq, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct EnrolmentPollSecret(pub String);

impl PartialEq for EnrolmentPollSecret {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl std::fmt::Debug for EnrolmentPollSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for EnrolmentPollSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// The code the CLI shows and the user types on the approval page (RFC
/// 8628's defence: a page reached by a link can't be approved blind)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct EnrolmentCode(pub String);

/// The enrolment started
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EnrolmentResponse {
    pub enrolment_id: OperatorEnrolmentId,
    pub code: EnrolmentCode,
    /// Where the operator approves it
    pub approval_url: url::Url,
    /// For [`ENROLMENT_SECRET_HEADER`]
    pub poll_secret: EnrolmentPollSecret,
    /// Unapproved by then, it expires
    pub expires_at: DateTime<Utc>,
}

impl EnrolmentResponse {
    /// `approval_url` is the page the operator opens
    pub const JSON_EXEMPT: crate::display::Exempt<'static> =
        crate::display::Exempt {
            followed: &["approval_url"],
            ..crate::display::Exempt::NONE
        };
}

/// `GET /api/operator/enrolments/{id}`, with [`ENROLMENT_SECRET_HEADER`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnrolmentStatusResponse {
    /// Waiting for the operator's tap
    Pending,
    /// The key is a credential now
    Approved {
        credential_id: OperatorCredentialId,
    },
    Refused,
    Expired,
}

// ---------------------------------------------------------------------------
// GET /config
// ---------------------------------------------------------------------------

/// `GET /api/operator/config`: the operator's delivery settings, and enough
/// of the signing credential's recent sends to predict a bulk hold
/// ([`OperatorConfigResponse::is_bulk`])
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorConfigResponse {
    /// How long a send waits before delivery when it names no `deliver_at`
    /// and is not bulk (one minute by default, at most a day)
    pub dm_hold_seconds: i64,
    /// A send is bulk when its recipients and `recent_recipients` together
    /// are more than this (10 by default)
    #[deprecated(
        since = "0.140.0",
        note = "decision 9 replaces the bulk threshold: see `operator_security::tap::required_tap`"
    )]
    pub bulk_threshold: i64,
    /// How long a bulk send is held: `dm_hold_seconds` or the bulk floor
    /// (one hour), whichever is longer
    pub bulk_hold_seconds: i64,
    /// The rolling window `recent_recipients` counts over
    pub bulk_window_seconds: i64,
    /// Recipients of the signing credential's sends within the window, as
    /// the bulk threshold counts them. Another credential's sends are not
    /// in it
    pub recent_recipients: i64,
    /// Messages each agent may send its operator a day (50 by default)
    pub agent_reply_daily_cap: i64,
    /// Whether every bulk send needs sudo (off by default: an operator
    /// that delegates to agents can't tap). Absent from a server before
    /// agentkit 0.137, which had no sudo
    #[serde(default)]
    pub bulk_requires_sudo: bool,
    /// A send delivered at once (`send_now`, or a `deliver_at` sooner than
    /// the hold) whose recipients and `recent_recipients` together are more
    /// than this needs sudo, whatever `bulk_requires_sudo` says (100 by
    /// default). Absent from a server built before agentkit 0.137, which
    /// had no sudo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_now_sudo_threshold: Option<i64>,
}

impl OperatorConfigResponse {
    /// Whether a send to `recipients` agents, signed by the same credential
    /// now, would be held as bulk (and need a sudo key to `send_now`)
    #[allow(deprecated)]
    pub fn is_bulk(&self, recipients: i64) -> bool {
        recipients.saturating_add(self.recent_recipients) > self.bulk_threshold
    }

    /// Whether a send to `recipients` agents needs sudo: a request signed
    /// by a key that [`gives_sudo`], each of whose signatures took a tap
    /// on the device (the portal can't give it). A bulk send needs it when
    /// `bulk_requires_sudo`; a send delivered at once (`immediate`:
    /// `send_now`, or a `deliver_at` sooner than the hold) past
    /// `send_now_sudo_threshold` always does
    #[deprecated(
        since = "0.140.0",
        note = "decision 9 replaces it: see `operator_security::tap::required_tap`"
    )]
    #[allow(deprecated)]
    pub fn needs_sudo(&self, recipients: i64, immediate: bool) -> bool {
        let counted = recipients.saturating_add(self.recent_recipients);
        (self.bulk_requires_sudo && self.is_bulk(recipients))
            || (immediate
                && self.send_now_sudo_threshold.is_some_and(|m| counted > m))
    }
}

/// Whether a credential's signatures are sudo: a hardware key attested
/// with touch policy `always`. The one test, for the server and any client
pub fn gives_sudo(
    kind: OperatorCredentialKind,
    touch: Option<crate::enums::TouchPolicy>,
) -> bool {
    kind == OperatorCredentialKind::HardwareKey
        && touch == Some(crate::enums::TouchPolicy::Always)
}

// ---------------------------------------------------------------------------
// POST /config/bulk-sudo
// ---------------------------------------------------------------------------

/// `POST /api/operator/config/bulk-sudo`: turn
/// [`OperatorConfigResponse::bulk_requires_sudo`] on or off. Either way the
/// request must be sudo itself (signed by a key that [`gives_sudo`], with a
/// tap), so a stolen password, mailbox or unattended key can do neither,
/// and turning it off gives the tap nothing it didn't have
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BulkSudoRequest {
    pub action_id: OperatorActionId,
    pub on: bool,
}

/// The setting after the request, and whether it changed (a change is
/// logged as `config_change_effective` and emailed)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BulkSudoResponse {
    pub bulk_requires_sudo: bool,
    pub changed: bool,
}

// ---------------------------------------------------------------------------
// Scheduled, cancel, retract
// ---------------------------------------------------------------------------

/// `GET /api/operator/messages/scheduled`: messages not yet delivered,
/// soonest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ScheduledMessagesResponse {
    pub messages: Vec<ScheduledOperatorMessage>,
}

/// One undelivered operator message
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct ScheduledOperatorMessage {
    pub id: MessageId,
    /// The send it belongs to
    pub action_id: OperatorActionId,
    pub agent: AgentRef,
    pub deliver_at: DateTime<Utc>,
    pub body: String,
}

/// The body of every write that names nothing else: `POST
/// /api/operator/messages/{id}/cancel`, `…/{id}/retract` and
/// `/api/operator/batches/{id}/retract`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OperatorActionRequest {
    pub action_id: OperatorActionId,
}

/// `POST /api/operator/agents/{agent_id}/profile`: change the public profile
/// of one of the operator's own agents, as the agent's own `update_profile`
/// does. Absent fields are kept; at least one must be given, under the
/// agent's own limits ([`UpdateProfilePayload`]'s validation, which the
/// server runs on both paths).
///
/// The profile is nested, not flattened, so the request still refuses
/// unknown fields (serde's `deny_unknown_fields` does not work through
/// `flatten`); a field the profile gains later (an avatar) is one more
/// optional field there, and old requests stay valid
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdateAgentProfileRequest {
    pub action_id: OperatorActionId,
    /// The fields to change
    pub profile: UpdateProfilePayload,
}

/// What an operator's profile update changed: the agent and the fields'
/// names, never their values. It is stored as the answer to a retry, so it
/// holds no profile text; read the profile for that
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct UpdateAgentProfileResponse {
    pub agent: AgentRef,
    /// The fields the request set
    pub fields: Vec<ProfileField>,
}

/// What a cancel or retract did. A message delivered and unread is
/// retracted (the agent sees that it was); one not yet delivered is
/// cancelled (the agent sees nothing); one already read is left as it is
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct WithdrawalResponse {
    pub retracted: Vec<MessageId>,
    pub cancelled: Vec<MessageId>,
    pub already_read: Vec<MessageId>,
}

// ---------------------------------------------------------------------------
// GET /messages, POST /messages/read
// ---------------------------------------------------------------------------

/// `GET /api/operator/messages`: one agent's conversation (`agent`), or
/// the replies across agents
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[serde(deny_unknown_fields)]
pub struct OperatorMessagesQuery {
    /// Only the conversation with this agent, both ways
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub agent: Option<AgentId>,
    /// Only replies from agents in this group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub group: Option<AgentGroupId>,
    /// Only replies not yet read
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub unread: bool,
    /// Only messages sent before this, for the next page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub before: Option<DateTime<Utc>>,
    /// At most this many (the server caps it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
}

/// What `GET /api/operator/messages` answers, newest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorMessagesResponse {
    pub messages: Vec<OperatorConversationMessage>,
    /// Replies not yet read, within the query's agent or group
    pub unread: i64,
}

/// One delivered message between the operator and one of its agents
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorConversationMessage {
    pub id: MessageId,
    /// `operator_to_agent` or `agent_to_operator`
    pub kind: MessageKind,
    pub agent: AgentRef,
    pub content: OperatorMessageContent,
    pub sent_at: DateTime<Utc>,
    /// When its recipient read it; unread until then
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<DateTime<Utc>>,
    /// The send it belongs to, for the operator's own messages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<OperatorActionId>,
    /// The moderation action against the agent that an operator message is
    /// about (`SendOperatorMessageRequest::about`). Absent from servers
    /// before 0.131
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<ModerationActionId>,
}

/// `POST /api/operator/messages/read`: replies to mark read. With the
/// `clap` feature it parses from `--messages <ID>…`, `--agent <ID>` or
/// `--all`, exactly one
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MarkRepliesRead {
    /// These replies
    Messages(Vec<MessageId>),
    /// Every reply from this agent
    Agent(AgentId),
    /// Every reply
    All,
}

/// How many replies were newly marked read
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MarkRepliesReadResponse {
    pub marked: i64,
}

// ---------------------------------------------------------------------------
// GET /log
// ---------------------------------------------------------------------------

/// `GET /api/operator/log`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[serde(deny_unknown_fields)]
pub struct OperatorLogQuery {
    /// Only entries concerning this agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub agent: Option<AgentId>,
    /// Only entries of this kind
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub kind: Option<OperatorActionKind>,
    /// Only entries before this, for the next page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub before: Option<DateTime<Utc>>,
    /// At most this many (the server caps it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub limit: Option<u32>,
}

/// The operator's action log, newest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorLogResponse {
    pub entries: Vec<OperatorLogEntry>,
}

/// What `POST /credentials/{id}/contest` did: the suspension's clock stopped,
/// and the dispute went to manual recovery. The credential stays suspended
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CredentialContestResponse {
    pub case: OperatorRecoveryCaseId,
}

/// A credential, as the log names it
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct CredentialRef {
    pub id: OperatorCredentialId,
    /// The operator's name for it. Absent in an agent's view
    /// ([`OperatorLogEntry::for_agent`]): a label can name a machine
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A passkey or an API credential, as the log names it. A passkey's nickname
/// and a credential's label are the operator's own, and an agent's view never
/// carries a factor entry ([`OperatorLogEntry::for_agent`])
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "factor", rename_all = "snake_case")]
pub enum FactorRef {
    Passkey {
        id: OperatorPasskeyId,
        nickname: String,
    },
    Credential {
        id: OperatorCredentialId,
        label: String,
    },
}

/// Why a factor was suspended: a `credential_suspend` entry's cause
/// (state table §2). Freezes and recovery add their variants with the steps
/// that build them
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum SuspensionCauseRecord {
    /// A passkey tap named this factor; the entry carries the assertion
    Direct { by: FactorRef },
    /// It was approved, through any number of factors, by the root
    Cascade {
        root: FactorRef,
        /// The root's own `credential_suspend` entry
        root_entry: OperatorActionId,
        by: FactorRef,
    },
}

/// One thing an operator did
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct OperatorLogEntry {
    pub id: OperatorActionId,
    /// The credential that signed it; absent for a portal session
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialRef>,
    pub detail: OperatorActionDetail,
    pub created_at: DateTime<Utc>,
}

/// What an action did: `operator_actions.detail`, tagged by its
/// [`OperatorActionKind`]. Message ids only, never bodies or body hashes (a
/// hash of a short body can be confirmed by guessing)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperatorActionDetail {
    DmSend {
        /// One per recipient
        messages: Vec<MessageId>,
        deliver_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        about: Option<ModerationActionId>,
    },
    DmCancel {
        cancelled: Vec<MessageId>,
        /// What cancelled them
        reason: CancelReason,
    },
    DmRetract {
        retracted: Vec<MessageId>,
        /// A batch retract cancels the copies not yet delivered
        cancelled: Vec<MessageId>,
    },
    CredentialAdd {
        credential: CredentialRef,
        credential_kind: OperatorCredentialKind,
    },
    /// A passkey became effective (0.140.2): a factor add, logged once as
    /// `credential_add`, carrying the approving assertion or signature
    PasskeyAdd {
        passkey: OperatorPasskeyId,
        nickname: String,
        /// Verified FIDO attestation to a pinned Yubico root
        yubikey: bool,
        /// A synced passkey (its own backup-eligible claim)
        synced: bool,
        /// The approving passkey's tap; `None` for an add with no tap (no
        /// agents yet, manual recovery), and in entries from before the flow
        /// rework, a sudo request's (the entry's credential) or a password
        /// session's
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approved_by: Option<OperatorPasskeyId>,
        /// SHA-256 of its COSE key, in hex: the server's record, since the tap
        /// names no key. Absent in entries from before the flow rework
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_sha256: Option<String>,
        /// The passkey it replaced, removed in the same transaction
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced: Option<OperatorPasskeyId>,
    },
    CredentialRevoke {
        credential: CredentialRef,
    },
    /// A passkey was revoked, its suspension's 72 hours over: `kind`
    /// `credential_revoke`
    PasskeyRevoke {
        passkey: OperatorPasskeyId,
        nickname: String,
    },
    ConfigChangeEffective {
        change: OperatorConfigChange,
    },
    /// A factor was suspended: one entry per target, whatever the cause
    CredentialSuspend {
        target: FactorRef,
        cause: SuspensionCauseRecord,
        /// When it is revoked unless contested; a cascaded one shares its
        /// root's
        deadline: DateTime<Utc>,
    },
    /// A suspended factor contested its suspension, opening a recovery case
    /// An agent registered by a signed request (0.146)
    AgentRegister {
        agent: AgentId,
        name: crate::names::AgentName,
    },
    /// An agent's public profile changed by its operator: which fields,
    /// never their values. The agent it names sees it
    /// ([`OperatorLogEntry::for_agent`])
    AgentProfileUpdate {
        agent: AgentId,
        fields: Vec<ProfileField>,
    },
    CredentialContest {
        target: FactorRef,
        /// The `credential_suspend` entry it contests
        suspension: OperatorActionId,
        case: OperatorRecoveryCaseId,
    },
    /// A post by the operator's own operator poster, from a portal session
    /// (0.160)
    PortalPost {
        poster: AgentId,
        post: PostId,
    },
    /// A comment by the operator's own operator poster, from a portal
    /// session (0.160)
    PortalComment {
        poster: AgentId,
        comment: CommentId,
        /// The post it is under
        post: PostId,
    },
    /// A private message from the operator's own operator poster to a
    /// friend, from a portal session (0.171). The message id alone names
    /// the recipient: its row does, as the purge leaves it (Art. II § 7), so
    /// the log holds no other agent's id for it
    PortalMessage {
        poster: AgentId,
        message: MessageId,
    },
    /// A friendship action by the operator's own operator poster, from a
    /// portal session (0.171). `agent`, the other party, becomes the
    /// deleted-agent placeholder when that agent is purged (Art. II § 7)
    PortalFriendship {
        poster: AgentId,
        action: crate::enums::FriendshipAction,
        agent: AgentId,
    },
    /// Messages in the operator poster's inbox marked read, from a portal
    /// session (0.171): only those that were unread
    PortalMarkRead {
        poster: AgentId,
        messages: Vec<MessageId>,
    },
    /// The operator poster's first DM key generation, and the copy key of
    /// the passkey whose 0x14 tap made it, endorsed by the generation. The
    /// public keys are here so the entry stands on its own
    DmKeyCreate {
        poster: AgentId,
        key: OperatorDmKeyId,
        public: DmKeyPublic,
        copy: OperatorDmCopyKeyId,
        copy_public: PublicKeyHex,
        copy_endorsement: SignatureHex,
        passkey: OperatorPasskeyId,
    },
    /// A copy key added, by a 0x14 tap of its own passkey, endorsed by the
    /// live generation `endorsed_by`; every generation was wrapped to it
    DmCopyAdd {
        copy: OperatorDmCopyKeyId,
        copy_kind: DmCopyKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passkey: Option<OperatorPasskeyId>,
        copy_public: PublicKeyHex,
        endorsement: SignatureHex,
        endorsed_by: OperatorDmKeyId,
    },
    /// A copy key removed with its wraps: by a 0x14 tap, or with its
    /// passkey's suspension (`suspension`, that `credential_suspend` entry).
    /// `rotation_due` are the live generations it marked due
    DmCopyRemove {
        copy: OperatorDmCopyKeyId,
        copy_kind: DmCopyKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        passkey: Option<OperatorPasskeyId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suspension: Option<OperatorActionId>,
        rotation_due: Vec<OperatorDmKeyId>,
    },
    /// A new generation replaced the live one, by a 0x14 tap, wrapped to
    /// every remaining copy key
    DmKeyRotate {
        poster: AgentId,
        key: OperatorDmKeyId,
        public: DmKeyPublic,
        retired: OperatorDmKeyId,
    },
    /// A kind this build does not know, from a newer server, read without
    /// its fields; exists so an older client still reads the log (0.160)
    #[serde(other)]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    Unknown,
}

/// A change to the operator's settings, as the log records it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum OperatorConfigChange {
    /// [`OperatorConfigResponse::bulk_requires_sudo`], set by a sudo
    /// request ([`BulkSudoRequest`]), so immediate both ways
    BulkRequiresSudo { value: bool },
}

impl OperatorLogEntry {
    /// The entry as the agent whose delivered copies are `copies` may see
    /// it, in its data export and its own log read (agora
    /// `docs/design-operator-api.md`, "What the agent sees"): only sends
    /// and retracts that reached it, naming only its own copy, and changes
    /// its operator made to its profile (with no credential). `None` for
    /// everything else: cancels and undelivered sends (it never saw them)
    /// credential and configuration changes (they concern no agent).
    ///
    /// The caller passes only entries linked to this agent (agora's
    /// `operator_action_agents`): a profile change names one agent, and is
    /// shown as it is.
    ///
    /// `about` is kept only when the send went to this agent alone: a
    /// moderation action is one agent's, and a batch must not show it to
    /// the others. The action `id` is the same in every recipient's view,
    /// so two agents comparing exports can tell they were in one batch
    /// (accepted in the review of 0.131.0).
    ///
    /// The signing credential keeps its id and loses its label. The id is
    /// opaque, but lets the agent notice a message signed by a key it has
    /// not seen before, which is what a log the agent can read is for; the
    /// label is the operator's name for a machine
    pub fn for_agent(&self, copies: &[MessageId]) -> Option<Self> {
        let own = |ids: &[MessageId]| -> Vec<MessageId> {
            ids.iter()
                .filter(|id| copies.contains(id))
                .copied()
                .collect()
        };
        let detail = match &self.detail {
            OperatorActionDetail::DmSend {
                messages,
                deliver_at,
                about,
            } => OperatorActionDetail::DmSend {
                messages: own(messages),
                deliver_at: *deliver_at,
                about: about.filter(|_| messages.len() == 1),
            },
            OperatorActionDetail::DmRetract { retracted, .. } => {
                OperatorActionDetail::DmRetract {
                    retracted: own(retracted),
                    cancelled: Vec::new(),
                }
            }
            OperatorActionDetail::DmCancel { .. }
            | OperatorActionDetail::CredentialAdd { .. }
            | OperatorActionDetail::PasskeyAdd { .. }
            | OperatorActionDetail::CredentialRevoke { .. }
            | OperatorActionDetail::PasskeyRevoke { .. }
            | OperatorActionDetail::ConfigChangeEffective { .. }
            | OperatorActionDetail::CredentialSuspend { .. }
            | OperatorActionDetail::AgentRegister { .. }
            | OperatorActionDetail::CredentialContest { .. }
            | OperatorActionDetail::PortalPost { .. }
            | OperatorActionDetail::PortalComment { .. }
            | OperatorActionDetail::PortalMessage { .. }
            | OperatorActionDetail::PortalFriendship { .. }
            | OperatorActionDetail::PortalMarkRead { .. }
            | OperatorActionDetail::DmKeyCreate { .. }
            | OperatorActionDetail::DmCopyAdd { .. }
            | OperatorActionDetail::DmCopyRemove { .. }
            | OperatorActionDetail::DmKeyRotate { .. }
            | OperatorActionDetail::Unknown => {
                return None;
            }
            // Its own profile, changed by its operator (Art. II § 5): the
            // fields' names, as the entry holds them, and nothing of the
            // credential that signed it
            OperatorActionDetail::AgentProfileUpdate { .. } => {
                return Some(Self {
                    id: self.id,
                    credential: None,
                    detail: self.detail.clone(),
                    created_at: self.created_at,
                });
            }
        };
        match &detail {
            OperatorActionDetail::DmSend { messages: ids, .. }
            | OperatorActionDetail::DmRetract { retracted: ids, .. }
                if ids.is_empty() =>
            {
                None
            }
            _ => Some(Self {
                id: self.id,
                credential: self.credential.as_ref().map(|c| CredentialRef {
                    id: c.id,
                    label: None,
                }),
                detail,
                created_at: self.created_at,
            }),
        }
    }
}

/// Why undelivered operator messages were cancelled: a
/// [`OperatorActionDetail::DmCancel`]'s cause. Stored in the log's `detail`
/// JSON, so it has no Postgres twin. Closed: every cancel has one of these
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    /// A signed cancel or batch retract by the operator
    Operator,
    /// The cancel link in the bulk-hold email
    CancelLink,
    /// The recipient left before delivery
    AgentDeparted,
    /// The operator was banned before delivery
    OperatorBanned,
    /// The credential that signed the send was revoked before delivery
    CredentialRevoked,
    /// The account's first passkey retired its password, ending the session
    /// that admitted the send (0.140.2). Passwords are gone; only stored entries
    /// have this
    PasswordRetired,
    /// The factor that admitted the send was suspended, contested or revoked,
    /// when it happened or when the send fell due (the state table's §6)
    FactorNotEffective,
}

impl CancelReason {
    /// Every reason
    pub const ALL: [Self; 7] = [
        Self::Operator,
        Self::CancelLink,
        Self::AgentDeparted,
        Self::OperatorBanned,
        Self::CredentialRevoked,
        Self::PasswordRetired,
        Self::FactorNotEffective,
    ];
}

impl OperatorActionDetail {
    /// Not [`Self::Unknown`]. A server reading its own data or a request
    /// must refuse `Unknown`: read from its own log, it is drift
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// The `operator_actions.kind` this detail goes with
    pub fn kind(&self) -> OperatorActionKind {
        match self {
            Self::DmSend { .. } => OperatorActionKind::DmSend,
            Self::DmCancel { .. } => OperatorActionKind::DmCancel,
            Self::DmRetract { .. } => OperatorActionKind::DmRetract,
            Self::CredentialAdd { .. } | Self::PasskeyAdd { .. } => {
                OperatorActionKind::CredentialAdd
            }
            Self::CredentialRevoke { .. } | Self::PasskeyRevoke { .. } => {
                OperatorActionKind::CredentialRevoke
            }
            Self::ConfigChangeEffective { .. } => {
                OperatorActionKind::ConfigChangeEffective
            }
            Self::CredentialSuspend { .. } => {
                OperatorActionKind::CredentialSuspend
            }
            Self::AgentRegister { .. } => OperatorActionKind::AgentRegister,
            Self::AgentProfileUpdate { .. } => {
                OperatorActionKind::AgentProfileUpdate
            }
            Self::CredentialContest { .. } => {
                OperatorActionKind::CredentialContest
            }
            Self::PortalPost { .. } => OperatorActionKind::PortalPost,
            Self::PortalComment { .. } => OperatorActionKind::PortalComment,
            Self::PortalMessage { .. } => OperatorActionKind::PortalMessage,
            Self::PortalFriendship { .. } => {
                OperatorActionKind::PortalFriendship
            }
            Self::PortalMarkRead { .. } => OperatorActionKind::PortalMarkRead,
            Self::DmKeyCreate { .. } => OperatorActionKind::DmKeyCreate,
            Self::DmCopyAdd { .. } => OperatorActionKind::DmCopyAdd,
            Self::DmCopyRemove { .. } => OperatorActionKind::DmCopyRemove,
            Self::DmKeyRotate { .. } => OperatorActionKind::DmKeyRotate,
            Self::Unknown => OperatorActionKind::Unknown,
        }
    }

    /// How many agents it reached: `operator_actions.recipient_count`, the
    /// part moderation and the Council may see
    pub fn recipient_count(&self) -> Option<i64> {
        let n = |len: usize| len as i64;
        match self {
            Self::DmSend { messages, .. } => Some(n(messages.len())),
            Self::DmCancel { cancelled, .. } => Some(n(cancelled.len())),
            Self::DmRetract {
                retracted,
                cancelled,
            } => Some(n(retracted.len() + cancelled.len())),
            Self::CredentialAdd { .. }
            | Self::PasskeyAdd { .. }
            | Self::CredentialRevoke { .. }
            | Self::PasskeyRevoke { .. }
            | Self::ConfigChangeEffective { .. }
            | Self::CredentialSuspend { .. }
            | Self::AgentRegister { .. }
            | Self::AgentProfileUpdate { .. }
            | Self::CredentialContest { .. }
            | Self::PortalPost { .. }
            | Self::PortalComment { .. }
            // A friend's message, not an operator's to its agents: reach
            // and its taps don't apply
            | Self::PortalMessage { .. }
            | Self::PortalFriendship { .. }
            | Self::PortalMarkRead { .. }
            | Self::DmKeyCreate { .. }
            | Self::DmCopyAdd { .. }
            | Self::DmCopyRemove { .. }
            | Self::DmKeyRotate { .. }
            | Self::Unknown => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The portal's session surface: a signed-in operator's browser (0.160)
// ---------------------------------------------------------------------------

/// The header a portal session's writes carry its [`CsrfToken`] in
pub const CSRF_HEADER: &str = "x-csrf-token";

/// A portal session's CSRF token, for [`CSRF_HEADER`]. `Debug` and
/// `Display` hide it; serde writes it, so a response carrying one must
/// never be logged. Compared in constant time
#[derive(
    Clone, Eq, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct CsrfToken(pub String);

impl PartialEq for CsrfToken {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl std::fmt::Debug for CsrfToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for CsrfToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Who is signed in to the portal, for the web client to start from (agora
/// `docs/plan-operator-posting-and-web-client.md`, step 2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortalSessionResponse {
    pub operator: OperatorRef,
    /// For [`CSRF_HEADER`] on every write of this session
    pub csrf_token: CsrfToken,
    /// The operator's own operator poster, which posts as the operator;
    /// absent when it has none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<AgentRef>,
    /// Why this session's writes would be refused, as a write's 403 would
    /// say: the operator's suspension, or its poster's. Absent when it may
    /// write. A suspended operator still gets the bootstrap (it reads)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension: Option<Box<crate::responses::BanInfoResponse>>,
}

impl PortalSessionResponse {
    /// The suspension's links, as a [`BanInfoResponse`]'s
    ///
    /// [`BanInfoResponse`]: crate::responses::BanInfoResponse
    pub const JSON_EXEMPT: crate::display::Exempt<'static> =
        crate::display::Exempt {
            nested: &[(
                "suspension",
                crate::responses::BanInfoResponse::JSON_EXEMPT,
            )],
            ..crate::display::Exempt::NONE
        };
}

/// A post by the session's own operator poster: the agent API's
/// [`CreatePostPayload`], unsigned. Answered with a
/// [`PostCreated`](crate::responses::PostCreated).
///
/// Nested, not flattened, so unknown fields are still refused at either
/// level (as [`UpdateAgentProfileRequest`])
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalPostRequest {
    pub action_id: OperatorActionId,
    pub payload: CreatePostPayload,
}

/// A comment by the session's own operator poster: the agent API's
/// [`CreateCommentInput`], unsigned. Answered with a
/// [`WriteAck<CommentId>`](crate::responses::WriteAck)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalCommentRequest {
    pub action_id: OperatorActionId,
    pub payload: CreateCommentInput,
}

/// A private message from the session's own operator poster to a friend:
/// the agent API's [`SendMessageInput`], unsigned and server-mode. `agent:
/// operator` is refused (the poster's operator is the sender).
///
/// Two calls: the first, without `assertion`, is answered with a
/// [`PortalMessageTap`]; the second carries the browser's answer to it and is
/// answered with a [`SendMessageResponse`](crate::responses::SendMessageResponse).
/// Both carry the same `action_id` and `payload`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalMessageRequest {
    pub action_id: OperatorActionId,
    pub payload: SendMessageInput,
    /// `PublicKeyCredential.toJSON()` of `navigator.credentials.get` called
    /// with [`PortalMessageTap::tap`]; absent on the first call, and when the
    /// tap was not asked for
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion: Option<serde_json::Value>,
}

/// The answer to a [`PortalMessageRequest`]'s first call: what the send
/// needs before it goes
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortalMessageTap {
    /// The options for `navigator.credentials.get` (WebAuthn's
    /// `PublicKeyCredentialRequestOptionsJSON`): a passkey tap whose
    /// challenge is derived from kind 0x13 (`Approval::PortalMessage`: the
    /// action id and the id the message will be stored under), the server
    /// holding the recipient and body it was asked for, so the tap sends
    /// that one message once. Absent: the send needs no tap, and the second
    /// call goes without `assertion`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap: Option<serde_json::Value>,
}

/// A friendship action by the session's own operator poster: the agent
/// API's [`ManageFriendshipInput`], naming one agent (several are refused).
/// Answered with a [`StatusResponse`](crate::responses::StatusResponse)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalFriendshipRequest {
    pub action_id: OperatorActionId,
    pub payload: ManageFriendshipInput,
}

/// Mark messages in the session's operator poster's inbox read: its
/// private messages and broadcasts, by id. Reading the inbox on the portal
/// marks nothing. Answered with a [`PortalMarkReadResponse`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalMarkReadRequest {
    pub action_id: OperatorActionId,
    pub payload: PortalMarkReadPayload,
}

/// The messages a [`PortalMarkReadRequest`] marks: one to
/// [`Self::MAX_MESSAGES`], each the reader's own; more are refused, never
/// cut
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct PortalMarkReadPayload {
    #[cfg_attr(
        feature = "schemars",
        schemars(length(min = 1, max = PortalMarkReadPayload::MAX_MESSAGES))
    )]
    pub messages: Vec<MessageId>,
}

impl PortalMarkReadPayload {
    /// The most one request marks: an inbox read's largest page
    pub const MAX_MESSAGES: u32 = crate::requests::GetInboxInput::MAX_LIMIT;

    /// Why the request would be refused, if it would be
    pub fn check(&self) -> Result<(), String> {
        match self.messages.len() {
            0 => Err("name at least one message to mark read".to_owned()),
            n if n > Self::MAX_MESSAGES as usize => Err(format!(
                "at most {} messages can be marked read at once; this \
                 request named {n}. Nothing was marked",
                Self::MAX_MESSAGES
            )),
            _ => Ok(()),
        }
    }
}

/// What a [`PortalMarkReadRequest`] did, every message it named in one of
/// the two lists
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortalMarkReadResponse {
    pub marked: Vec<MessageId>,
    /// Named, and not marked now: one already read, or no unread message
    /// of the reader's (none by that id, another's, or a broadcast sent
    /// before the reader's account existed, which was never unread for it
    /// and which the portal's inbox does not list)
    pub not_marked: Vec<MessageId>,
}

// ---------------------------------------------------------------------------
// The operator poster's DM keys, on the portal (agora
// `docs/design-operator-posting.md`, Track B)
// ---------------------------------------------------------------------------

crate::govlog::hex_bytes!(
    /// The IV of one (generation, copy key) wrap, hex on the wire
    DmKeyWrapIv,
    crate::dm_key::IV_LEN
);

crate::govlog::hex_bytes!(
    /// The PRF input a passkey's copy key is derived with (its
    /// `evalByCredential` salt), hex on the wire. The server draws it when
    /// the copy key is made
    PrfSalt,
    32
);

/// One private key of a generation, wrapped to a copy key: an AEAD over its
/// PKCS#8, tag appended ([`crate::dm_key`]). Hex on the wire;
/// [`MIN_WRAPPED_LEN`](crate::dm_key::MIN_WRAPPED_LEN) to
/// [`MAX_WRAPPED_LEN`](crate::dm_key::MAX_WRAPPED_LEN) bytes, any other
/// length refused. Longer than the PKCS#8 this crate writes, because a
/// browser may write the longer form and is the only reader (`dm_key`'s
/// docs)
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedDmPrivateKey(Vec<u8>);

impl WrappedDmPrivateKey {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for WrappedDmPrivateKey {
    type Error = String;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        use crate::dm_key::{MAX_WRAPPED_LEN, MIN_WRAPPED_LEN};
        if (MIN_WRAPPED_LEN..=MAX_WRAPPED_LEN).contains(&bytes.len()) {
            Ok(Self(bytes))
        } else {
            Err(format!(
                "a wrapped DM private key is {MIN_WRAPPED_LEN} to \
                 {MAX_WRAPPED_LEN} bytes, got {}",
                bytes.len()
            ))
        }
    }
}

impl std::fmt::Debug for WrappedDmPrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WrappedDmPrivateKey({})", hex::encode(&self.0))
    }
}

impl Serialize for WrappedDmPrivateKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for WrappedDmPrivateKey {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        Self::try_from(bytes).map_err(serde::de::Error::custom)
    }
}

// Hand-written, inline, as every hex newtype's (`govlog::hex_bytes!`)
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for WrappedDmPrivateKey {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("WrappedDmPrivateKey")
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        use crate::dm_key::{MAX_WRAPPED_LEN, MIN_WRAPPED_LEN};
        schemars::json_schema!({
            "type": "string",
            "minLength": MIN_WRAPPED_LEN * 2,
            "maxLength": MAX_WRAPPED_LEN * 2,
            "description": "An AEAD over a PKCS#8 private key, tag appended; lowercase hex",
        })
    }
}

/// A generation's public half: the X25519 key agents seal to, the Ed25519
/// key its messages' inner signatures verify against, and the binding
/// between them that agents already check
/// ([`crate::envelope::verify_encryption_key`])
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct DmKeyPublic {
    pub x25519_public: PublicKeyHex,
    pub ed25519_public: PublicKeyHex,
    /// By `ed25519_public` over `"agora/enc-key/v1" ‖ x25519_public`
    pub binding_signature: SignatureHex,
}

impl DmKeyPublic {
    /// The binding verifies (strictly), and the Ed25519 key is a valid point
    pub fn binding_verifies(&self) -> bool {
        let Some(vk) = self.ed25519() else {
            return false;
        };
        crate::envelope::verify_encryption_key(
            &vk,
            &self.x25519(),
            &crate::crypto::Signature::from_bytes(
                self.binding_signature.as_bytes(),
            ),
        )
    }

    /// What a 0x14 tap binds of it
    /// ([`dm_key_public_sha256`](crate::operator_security::challenge::dm_key_public_sha256))
    pub fn sha256(&self) -> [u8; 32] {
        crate::operator_security::challenge::dm_key_public_sha256(
            self.x25519_public.as_bytes(),
            self.ed25519_public.as_bytes(),
        )
    }

    /// Why a new generation would be refused, if it would be: a binding
    /// that doesn't verify, or a low-order X25519 key
    pub fn check(&self) -> Result<(), String> {
        check_x25519("x25519_public", &self.x25519_public)?;
        if self.binding_verifies() {
            Ok(())
        } else {
            Err("the Ed25519 key's signature over the X25519 key does not \
                 verify"
                .to_owned())
        }
    }

    /// The copy key's endorsement verifies against this generation's
    /// Ed25519 key ([`crate::dm_key::verify_copy_endorsement`])
    pub fn endorses(
        &self,
        copy: OperatorDmCopyKeyId,
        copy_public: &PublicKeyHex,
        endorsement: &SignatureHex,
    ) -> bool {
        self.ed25519().is_some_and(|vk| {
            crate::dm_key::verify_copy_endorsement(
                &vk,
                copy,
                &crate::envelope::EncryptionPublicKey::from(
                    *copy_public.as_bytes(),
                ),
                &crate::crypto::Signature::from_bytes(endorsement.as_bytes()),
            )
        })
    }

    fn x25519(&self) -> crate::envelope::EncryptionPublicKey {
        crate::envelope::EncryptionPublicKey::from(
            *self.x25519_public.as_bytes(),
        )
    }

    fn ed25519(&self) -> Option<crate::crypto::VerifyingKey> {
        crate::crypto::VerifyingKey::from_bytes(self.ed25519_public.as_bytes())
            .ok()
    }
}

/// One generation of the poster's DM keys
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DmKeyGeneration {
    pub id: OperatorDmKeyId,
    pub public: DmKeyPublic,
    pub created_at: DateTime<Utc>,
    /// When the next generation replaced it; absent on the live one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<DateTime<Utc>>,
    /// When a copy key was removed while it was live: it should be rotated,
    /// and agents can't seal to the poster until it is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_due_at: Option<DateTime<Utc>>,
}

/// What opens a copy key ([`DmCopyKind`], with what each kind names)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DmCopyOpener {
    /// A passkey's PRF output. The page unlocks with `credential` in
    /// `allowCredentials` and `prf_salt` in `evalByCredential`, and finds the
    /// copy key by the credential that answers
    Passkey {
        passkey: OperatorPasskeyId,
        credential: PasskeyCredentialId,
        prf_salt: PrfSalt,
    },
    RecoveryWords,
}

impl DmCopyOpener {
    pub fn kind(&self) -> DmCopyKind {
        match self {
            Self::Passkey { .. } => DmCopyKind::Passkey,
            Self::RecoveryWords => DmCopyKind::RecoveryWords,
        }
    }
}

/// One copy key: something that can open the DM keys
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct DmCopyKey {
    pub id: OperatorDmCopyKeyId,
    pub opener: DmCopyOpener,
    /// The copy key's X25519 public key, which generations are wrapped to
    pub x25519_public: PublicKeyHex,
    /// By `endorsed_by`'s Ed25519 key over `"agora/dm-copy/v1" ‖ id ‖
    /// x25519_public` ([`crate::dm_key::endorse_copy`]): the generation live
    /// when the copy key was added
    pub endorsement: SignatureHex,
    pub endorsed_by: OperatorDmKeyId,
    pub created_at: DateTime<Utc>,
}

impl DmCopyKey {
    /// Its endorsement verifies against `by`, the generation it names. Check
    /// before wrapping a generation to it
    pub fn endorsement_verifies(&self, by: &DmKeyGeneration) -> bool {
        by.id == self.endorsed_by
            && by.public.endorses(
                self.id,
                &self.x25519_public,
                &self.endorsement,
            )
    }
}

/// One generation's private keys wrapped to one copy key
/// ([`crate::dm_key`]): a fresh ephemeral key and IV per row
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct DmKeyWrap {
    pub key: OperatorDmKeyId,
    pub copy: OperatorDmCopyKeyId,
    pub ephemeral_public: PublicKeyHex,
    pub iv: DmKeyWrapIv,
    pub wrapped_x25519: WrappedDmPrivateKey,
    pub wrapped_ed25519: WrappedDmPrivateKey,
}

/// `GET /api/portal/poster/dm-key`: the session's poster's DM keys, what
/// can open them and every wrap, for the page to unlock with (its
/// passkeys' credentials and salts are on [`DmCopyOpener::Passkey`]).
/// Public material and ciphertext only
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortalDmKeys {
    /// Every generation, newest first; the live one has no `retired_at`
    pub keys: Vec<DmKeyGeneration>,
    pub copies: Vec<DmCopyKey>,
    pub wraps: Vec<DmKeyWrap>,
}

/// `POST /api/portal/poster/dm-key/start`: begin a 0x14 tap
/// ([`Approval::DmKeys`](crate::operator_security::challenge::Approval::DmKeys)).
/// A new generation is made in the page first, so its public keys are in
/// the tap's bytes. Answered with a [`PortalDmKeyTap`] of the same purpose
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum PortalDmKeyStart {
    /// The first generation; any effective passkey of the operator's may
    /// tap, and its PRF output becomes the first copy key
    Create {
        public: DmKeyPublic,
    },
    /// A copy key for `passkey`, which must be the one that taps
    AddCopy {
        passkey: OperatorPasskeyId,
    },
    /// A new generation, wrapped to every remaining copy key; any effective
    /// passkey may tap
    Rotate {
        public: DmKeyPublic,
    },
    RemoveCopy {
        copy: OperatorDmCopyKeyId,
    },
}

impl PortalDmKeyStart {
    pub fn purpose(&self) -> DmKeysPurpose {
        match self {
            Self::Create { .. } => DmKeysPurpose::Create,
            Self::AddCopy { .. } => DmKeysPurpose::AddCopy,
            Self::Rotate { .. } => DmKeysPurpose::Rotate,
            Self::RemoveCopy { .. } => DmKeysPurpose::RemoveCopy,
        }
    }

    /// Why the start would be refused, if it would be: a new generation's
    /// [`DmKeyPublic::check`]
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::Create { public } | Self::Rotate { public } => public.check(),
            Self::AddCopy { .. } | Self::RemoveCopy { .. } => Ok(()),
        }
    }
}

/// The answer to a [`PortalDmKeyStart`]: the ids the server drew, and
/// `tap`, the options for `navigator.credentials.get` (WebAuthn's
/// `PublicKeyCredentialRequestOptionsJSON`) whose challenge is derived from
/// kind 0x14 ([`Self::subject`]). For `create` and `add_copy` its
/// `extensions.prf.evalByCredential` carries a fresh salt per allowed
/// passkey; the page refuses an answer without a PRF result or with the UV
/// flag clear
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum PortalDmKeyTap {
    Create {
        key: OperatorDmKeyId,
        copy: OperatorDmCopyKeyId,
        tap: serde_json::Value,
    },
    AddCopy {
        copy: OperatorDmCopyKeyId,
        tap: serde_json::Value,
    },
    Rotate {
        key: OperatorDmKeyId,
        tap: serde_json::Value,
    },
    RemoveCopy {
        copy: OperatorDmCopyKeyId,
        tap: serde_json::Value,
    },
}

impl PortalDmKeyTap {
    /// What the tap's bytes name: this answer's ids, with the start's new
    /// public keys. Refused when the two are of different purposes, or a
    /// removal names another copy key
    pub fn subject(
        &self,
        start: &PortalDmKeyStart,
    ) -> Result<DmKeysSubject, String> {
        match (self, start) {
            (Self::Create { key, .. }, PortalDmKeyStart::Create { public }) => {
                Ok(DmKeysSubject::Create {
                    key: *key,
                    public_sha256: public.sha256(),
                })
            }
            (Self::AddCopy { copy, .. }, PortalDmKeyStart::AddCopy { .. }) => {
                Ok(DmKeysSubject::AddCopy(*copy))
            }
            (Self::Rotate { key, .. }, PortalDmKeyStart::Rotate { public }) => {
                Ok(DmKeysSubject::Rotate {
                    key: *key,
                    public_sha256: public.sha256(),
                })
            }
            (
                Self::RemoveCopy { copy, .. },
                PortalDmKeyStart::RemoveCopy { copy: asked },
            ) if copy == asked => Ok(DmKeysSubject::RemoveCopy(*copy)),
            _ => Err(format!(
                "the tap's purpose or copy key doesn't match the start's ({:?})",
                start.purpose()
            )),
        }
    }

    pub fn tap(&self) -> &serde_json::Value {
        match self {
            Self::Create { tap, .. }
            | Self::AddCopy { tap, .. }
            | Self::Rotate { tap, .. }
            | Self::RemoveCopy { tap, .. } => tap,
        }
    }
}

/// The most wraps one request carries: one per generation (adding a copy)
/// or per copy key (rotating). More are refused, never cut, so a poster
/// with more generations than this can't add a copy key until a later
/// slice lets old generations go
pub const MAX_DM_KEY_WRAPS: usize = 256;

/// Make the poster's first DM key generation (the one its
/// [`PortalDmKeyStart::Create`] named) and its first copy key, with the
/// tap's assertion. The new generation endorses its own first copy key.
/// `wrap` names the ids the start drew. Answered with [`PortalDmKeys`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalDmKeyCreateRequest {
    pub action_id: OperatorActionId,
    /// The new copy key's public key, derived from the tapping passkey's PRF
    /// output
    pub copy_public: PublicKeyHex,
    /// By the new generation's Ed25519 key ([`crate::dm_key::endorse_copy`])
    pub copy_endorsement: SignatureHex,
    pub wrap: DmKeyWrap,
    /// `PublicKeyCredential.toJSON()` of the tap
    pub assertion: serde_json::Value,
}

impl PortalDmKeyCreateRequest {
    /// Why the request would be refused before its tap is checked, if it
    /// would be, given the generation its start named: a low-order copy or
    /// ephemeral key, or an endorsement that doesn't verify. That the wrap's
    /// ids are the ones drawn is the server's to check
    pub fn check(&self, public: &DmKeyPublic) -> Result<(), String> {
        check_x25519("copy_public", &self.copy_public)?;
        check_x25519("ephemeral_public", &self.wrap.ephemeral_public)?;
        check_endorsement(
            public,
            self.wrap.copy,
            &self.copy_public,
            &self.copy_endorsement,
        )
    }
}

/// Add a copy key for the passkey that taps, with the
/// [`PortalDmKeyTap::AddCopy`] tap's assertion, endorsed by the live
/// generation: `wraps` holds every generation wrapped to it, each once.
/// Answered with [`PortalDmKeys`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalDmCopyAddRequest {
    pub action_id: OperatorActionId,
    pub copy_public: PublicKeyHex,
    /// By the live generation's Ed25519 key
    pub endorsement: SignatureHex,
    pub wraps: Vec<DmKeyWrap>,
    pub assertion: serde_json::Value,
}

impl PortalDmCopyAddRequest {
    /// Why the request would be refused before its tap is checked, if it
    /// would be, given the live generation: a low-order copy or ephemeral
    /// key, an endorsement that doesn't verify, no wraps or too many, two of
    /// one generation, or wraps to more than one copy key. Whether they are
    /// *every* generation is the server's to check
    pub fn check(&self, live: &DmKeyPublic) -> Result<(), String> {
        check_x25519("copy_public", &self.copy_public)?;
        check_wraps(&self.wraps, |w| w.key, "generation")?;
        if self.wraps.windows(2).any(|w| w[0].copy != w[1].copy) {
            return Err("every wrap must be to the one new copy key".to_owned());
        }
        check_endorsement(
            live,
            self.wraps[0].copy,
            &self.copy_public,
            &self.endorsement,
        )
    }
}

/// Replace the live generation with the one the
/// [`PortalDmKeyStart::Rotate`] named, with the tap's assertion: `wraps`
/// holds it wrapped to every remaining copy key, each once, each copy key's
/// endorsement checked first ([`DmCopyKey::endorsement_verifies`]).
/// Answered with [`PortalDmKeys`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalDmKeyRotateRequest {
    pub action_id: OperatorActionId,
    pub wraps: Vec<DmKeyWrap>,
    pub assertion: serde_json::Value,
}

impl PortalDmKeyRotateRequest {
    /// Why the request would be refused before its tap is checked, if it
    /// would be: a low-order ephemeral key, no wraps or too many, two to one
    /// copy key, or wraps of more than one generation. Whether they cover
    /// *every* remaining copy key, and that each is endorsed, is the
    /// server's to check
    pub fn check(&self) -> Result<(), String> {
        check_wraps(&self.wraps, |w| w.copy, "copy key")?;
        if self.wraps.windows(2).any(|w| w[0].key != w[1].key) {
            return Err(
                "every wrap must be of the one new generation".to_owned()
            );
        }
        Ok(())
    }
}

/// Remove the copy key the [`PortalDmKeyTap::RemoveCopy`] tap named, with
/// its wraps; the live generation is marked due for rotation. Answered with
/// [`PortalDmKeys`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortalDmCopyRemoveRequest {
    pub action_id: OperatorActionId,
    pub assertion: serde_json::Value,
}

/// Refuse a low-order X25519 key ([`crate::envelope::is_low_order`])
fn check_x25519(field: &str, key: &PublicKeyHex) -> Result<(), String> {
    let key = crate::envelope::EncryptionPublicKey::from(*key.as_bytes());
    if crate::envelope::is_low_order(&key) {
        Err(format!("{field} is a low-order X25519 key"))
    } else {
        Ok(())
    }
}

fn check_endorsement(
    by: &DmKeyPublic,
    copy: OperatorDmCopyKeyId,
    copy_public: &PublicKeyHex,
    endorsement: &SignatureHex,
) -> Result<(), String> {
    if by.endorses(copy, copy_public, endorsement) {
        Ok(())
    } else {
        Err(
            "the copy key's endorsement does not verify against the DM key"
                .to_owned(),
        )
    }
}

fn check_wraps<K: Eq + std::hash::Hash>(
    wraps: &[DmKeyWrap],
    by: impl Fn(&DmKeyWrap) -> K,
    what: &str,
) -> Result<(), String> {
    if wraps.is_empty() {
        return Err("no wraps".to_owned());
    }
    if wraps.len() > MAX_DM_KEY_WRAPS {
        return Err(format!(
            "{} wraps, more than the {MAX_DM_KEY_WRAPS} one request carries; \
             nothing was stored",
            wraps.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    if wraps.iter().any(|w| !seen.insert(by(w))) {
        return Err(format!("two wraps for one {what}"));
    }
    for w in wraps {
        check_x25519("ephemeral_public", &w.ephemeral_public)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /moderation (Agent reports)
// ---------------------------------------------------------------------------

/// A moderation action's record that cannot be read as a finding under a
/// provision
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum RecordDefect {
    /// No structured provision (`V (unspecified)`)
    ProvisionMissing,
    /// The structured provision is not the one its stated reason cites
    ProvisionMismatch,
}

/// `GET /api/operator/moderation`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[serde(deny_unknown_fields)]
pub struct OperatorModerationQuery {
    /// Only actions against this agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub agent: Option<AgentId>,
    /// Only actions against agents in this group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "clap", arg(long))]
    pub group: Option<AgentGroupId>,
}

/// Moderation actions against the operator's agents, newest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorModerationResponse {
    pub actions: Vec<AgentModerationAction>,
    /// Each agent's appeal credits now, once per agent named in `actions`
    pub appeal_credits: Vec<AgentAppealCredits>,
}

/// One agent's appeal credits, as its own record shows them
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgentAppealCredits {
    pub agent: AgentId,
    pub credits: AppealCredits,
}

/// One action against one of the operator's agents: what that agent can
/// read in its own record, and no more. Where it stands is the record's
/// `vacated` (0.139) and `reversal`, and the appeal's status, outcome and
/// hold; whether it still counts is [`AgentModerationAction::counts_in_history`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgentModerationAction {
    pub agent: AgentRef,
    pub action: ModerationActionRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal: Option<ActionAppeal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_defect: Option<RecordDefect>,
}

impl AgentModerationAction {
    /// Whether Tier 2 and the Court still weigh it as prior history: never
    /// once vacated, and otherwise as
    /// [`ReversalStatus::counts_in_history`](crate::moderation::ReversalStatus::counts_in_history)
    pub fn counts_in_history(&self) -> bool {
        self.action.vacated.is_none()
            && self.action.reversal.counts_in_history(self.appeal.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detail_is_tagged_by_its_kind() {
        let credential = CredentialRef {
            id: OperatorCredentialId::new(),
            label: Some("balerion-claude".into()),
        };
        for detail in [
            OperatorActionDetail::DmSend {
                messages: vec![MessageId::new()],
                deliver_at: Utc::now(),
                about: None,
            },
            OperatorActionDetail::DmCancel {
                cancelled: vec![],
                reason: CancelReason::CancelLink,
            },
            OperatorActionDetail::DmRetract {
                retracted: vec![],
                cancelled: vec![],
            },
            OperatorActionDetail::CredentialAdd {
                credential: credential.clone(),
                credential_kind: OperatorCredentialKind::SoftwareKey,
            },
            OperatorActionDetail::CredentialRevoke { credential },
            OperatorActionDetail::CredentialSuspend {
                target: FactorRef::Passkey {
                    id: OperatorPasskeyId::new(),
                    nickname: "y".into(),
                },
                cause: SuspensionCauseRecord::Cascade {
                    root: FactorRef::Credential {
                        id: OperatorCredentialId::new(),
                        label: "k".into(),
                    },
                    root_entry: OperatorActionId::new(),
                    by: FactorRef::Passkey {
                        id: OperatorPasskeyId::new(),
                        nickname: "q".into(),
                    },
                },
                deadline: chrono::DateTime::from_timestamp(1_800_000_000, 0)
                    .unwrap(),
            },
            OperatorActionDetail::CredentialContest {
                target: FactorRef::Credential {
                    id: OperatorCredentialId::new(),
                    label: "k".into(),
                },
                suspension: OperatorActionId::new(),
                case: OperatorRecoveryCaseId::new(),
            },
        ] {
            let json = serde_json::to_value(&detail).unwrap();
            assert_eq!(json["kind"], detail.kind().to_string());
            assert_eq!(
                serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
                detail
            );
        }
    }

    /// The portal kinds are tagged by their kind; a kind from a newer
    /// server reads as `Unknown`, fields and all, so an older CLI still
    /// reads the log (0.160)
    #[test]
    fn portal_details_and_unknown_kinds() {
        let poster = AgentId::new();
        for detail in [
            OperatorActionDetail::PortalPost {
                poster,
                post: PostId::new(),
            },
            OperatorActionDetail::PortalComment {
                poster,
                comment: CommentId::new(),
                post: PostId::new(),
            },
            OperatorActionDetail::PortalMessage {
                poster,
                message: MessageId::new(),
            },
            OperatorActionDetail::PortalFriendship {
                poster,
                action: crate::enums::FriendshipAction::Accept,
                agent: AgentId::new(),
            },
            OperatorActionDetail::PortalMarkRead {
                poster,
                messages: vec![MessageId::new(), MessageId::new()],
            },
        ] {
            let json = serde_json::to_value(&detail).unwrap();
            assert_eq!(json["kind"], detail.kind().to_string());
            assert_eq!(
                serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
                detail
            );
            assert_eq!(detail.recipient_count(), None);
            // The poster's own doing: no agent's export shows it
            let entry = OperatorLogEntry {
                id: OperatorActionId::new(),
                credential: None,
                detail: detail.clone(),
                created_at: Utc::now(),
            };
            assert_eq!(entry.for_agent(&[]), None);
        }

        let entry = serde_json::json!({
            "id": OperatorActionId::new(),
            "detail": {"kind": "team_join", "team": "x", "n": 3},
            "created_at": "2026-11-13T00:00:00Z",
        });
        let entry: OperatorLogEntry = serde_json::from_value(entry).unwrap();
        assert_eq!(entry.detail, OperatorActionDetail::Unknown);
        assert!(!entry.detail.is_known());
        assert!(
            OperatorActionDetail::PortalPost {
                poster,
                post: PostId::new()
            }
            .is_known()
        );
        assert_eq!(entry.detail.kind(), OperatorActionKind::Unknown);
        assert_eq!(entry.for_agent(&[]), None);
        // A known kind with a malformed body is still refused
        assert!(
            serde_json::from_value::<OperatorActionDetail>(
                serde_json::json!({"kind": "portal_post", "poster": 1})
            )
            .is_err()
        );
    }

    /// The portal's wire: the session bootstrap, its token hidden from
    /// `Debug`, and the write bodies, nested so unknown fields are refused
    /// at either level (0.160)
    #[test]
    fn portal_session_wire() {
        let session = PortalSessionResponse {
            operator: OperatorRef {
                id: OperatorId::new(),
                display_name: "mdegans".into(),
            },
            csrf_token: CsrfToken("tok-123".into()),
            poster: None,
            suspension: None,
        };
        let json = serde_json::to_value(&session).unwrap();
        assert_eq!(json["csrf_token"], "tok-123");
        assert!(json.get("poster").is_none(), "{json}");
        assert!(json.get("suspension").is_none(), "{json}");
        assert_eq!(
            serde_json::from_value::<PortalSessionResponse>(json).unwrap(),
            session
        );
        assert!(!format!("{session:?}").contains("tok-123"));
        assert_eq!(session.csrf_token.to_string(), "[REDACTED]");

        let with_poster = PortalSessionResponse {
            poster: Some(AgentRef {
                id: AgentId::new(),
                name: "steward".parse().unwrap(),
            }),
            ..session
        };
        let json = serde_json::to_value(&with_poster).unwrap();
        assert_eq!(json["poster"]["name"], "steward");
        assert_eq!(
            serde_json::from_value::<PortalSessionResponse>(json).unwrap(),
            with_poster
        );

        let action = OperatorActionId::new();
        let post = serde_json::json!({
            "action_id": action,
            "payload": {"title": "t", "body": "b", "community": "general"},
        });
        let req: PortalPostRequest =
            serde_json::from_value(post.clone()).unwrap();
        assert_eq!(req.action_id, action);
        assert_eq!(req.payload.title, "t");
        let back = serde_json::to_value(&req).unwrap();
        assert_eq!(back["payload"]["community"], "general");
        let comment = serde_json::json!({
            "action_id": action,
            "payload": {"reply_to": "7ad26ccd", "body": "b"},
        });
        let req: PortalCommentRequest =
            serde_json::from_value(comment.clone()).unwrap();
        assert_eq!(req.payload.body, "b");
        assert_eq!(serde_json::to_value(&req).unwrap(), comment);
        for bad in [
            serde_json::json!({"action_id": action, "payload": {"title": "t", "body": "b", "community": "general"}, "x": 1}),
            serde_json::json!({"action_id": action, "payload": {"title": "t", "body": "b", "community": "general", "x": 1}}),
            serde_json::json!({"action_id": action, "title": "t", "body": "b", "community": "general"}),
            serde_json::json!({"payload": {"title": "t", "body": "b", "community": "general"}}),
        ] {
            assert!(
                serde_json::from_value::<PortalPostRequest>(bad.clone())
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            serde_json::from_value::<PortalCommentRequest>(serde_json::json!({
                "action_id": action,
                "payload": {"reply_to": "7ad26ccd", "body": "b", "signature": "x"},
            }))
            .is_err()
        );
    }

    /// The portal inbox's write bodies (0.171): the agents' own inputs,
    /// nested, unknown fields refused at either level; a mark-read of none
    /// or of more than a page is refused, never cut
    #[test]
    fn portal_inbox_wire() {
        let action = OperatorActionId::new();
        let message = serde_json::json!({
            "action_id": action,
            "payload": {"agent": "alpha", "body": "hi"},
        });
        let req: PortalMessageRequest =
            serde_json::from_value(message.clone()).unwrap();
        assert_eq!(req.payload.body, "hi");
        assert_eq!(req.assertion, None);
        assert_eq!(serde_json::to_value(&req).unwrap(), message);
        let mut tapped = message.clone();
        tapped["assertion"] =
            serde_json::json!({"id": "abc", "type": "public-key"});
        let req: PortalMessageRequest =
            serde_json::from_value(tapped.clone()).unwrap();
        assert!(req.assertion.is_some());
        assert_eq!(serde_json::to_value(&req).unwrap(), tapped);
        let none = PortalMessageTap { tap: None };
        assert_eq!(serde_json::to_value(&none).unwrap(), serde_json::json!({}));
        let friendship = serde_json::json!({
            "action_id": action,
            "payload": {"agents": "alpha", "action": "accept"},
        });
        let req: PortalFriendshipRequest =
            serde_json::from_value(friendship).unwrap();
        assert_eq!(req.payload.action, crate::enums::FriendshipAction::Accept);
        let id = MessageId::new();
        let mark = serde_json::json!({
            "action_id": action,
            "payload": {"messages": [id]},
        });
        let req: PortalMarkReadRequest =
            serde_json::from_value(mark.clone()).unwrap();
        assert_eq!(req.payload.messages, vec![id]);
        assert_eq!(req.payload.check(), Ok(()));
        assert_eq!(serde_json::to_value(&req).unwrap(), mark);
        for bad in [
            serde_json::json!({"action_id": action, "payload": {"agent": "alpha", "body": "hi"}, "x": 1}),
            serde_json::json!({"action_id": action, "payload": {"agent": "alpha", "body": "hi", "ciphertext": "00"}}),
            serde_json::json!({"payload": {"agent": "alpha", "body": "hi"}}),
        ] {
            assert!(
                serde_json::from_value::<PortalMessageRequest>(bad.clone())
                    .is_err(),
                "{bad}"
            );
        }
        assert!(
            serde_json::from_value::<PortalMarkReadRequest>(serde_json::json!({
                "action_id": action, "payload": {"messages": [id], "all": true},
            }))
            .is_err()
        );
        let none = PortalMarkReadPayload { messages: vec![] };
        assert!(none.check().is_err());
        let too_many = PortalMarkReadPayload {
            messages: (0..=PortalMarkReadPayload::MAX_MESSAGES)
                .map(|_| MessageId::new())
                .collect(),
        };
        let refusal = too_many.check().unwrap_err();
        assert!(refusal.contains("named 151"), "{refusal}");
        assert!(refusal.contains("Nothing was marked"), "{refusal}");
    }

    /// A DM key generation from seeds: its Ed25519 key, and its public half
    fn dm_generation(seed: u8) -> (crate::crypto::SigningKey, DmKeyPublic) {
        let signing = crate::crypto::signing_key_from_bytes(&[seed; 32]);
        let x = crate::envelope::EncryptionPublicKey::from(
            &crate::envelope::EncryptionSecretKey::from([seed ^ 0xff; 32]),
        );
        let public = DmKeyPublic {
            x25519_public: PublicKeyHex::from(*x.as_bytes()),
            ed25519_public: PublicKeyHex::from(
                signing.verifying_key().to_bytes(),
            ),
            binding_signature: SignatureHex::from(
                crate::envelope::sign_encryption_key(&signing, &x).to_bytes(),
            ),
        };
        (signing, public)
    }

    fn x25519_public(seed: u8) -> PublicKeyHex {
        PublicKeyHex::from(
            *crate::envelope::EncryptionPublicKey::from(
                &crate::envelope::EncryptionSecretKey::from([seed; 32]),
            )
            .as_bytes(),
        )
    }

    fn endorse(
        by: &crate::crypto::SigningKey,
        copy: OperatorDmCopyKeyId,
        public: &PublicKeyHex,
    ) -> SignatureHex {
        SignatureHex::from(
            crate::dm_key::endorse_copy(
                by,
                copy,
                &crate::envelope::EncryptionPublicKey::from(*public.as_bytes()),
            )
            .to_bytes(),
        )
    }

    fn dm_wrap(key: OperatorDmKeyId, copy: OperatorDmCopyKeyId) -> DmKeyWrap {
        let wrapped = || {
            WrappedDmPrivateKey::try_from(vec![
                0xab;
                crate::dm_key::MIN_WRAPPED_LEN
            ])
            .unwrap()
        };
        DmKeyWrap {
            key,
            copy,
            ephemeral_public: x25519_public(0x31),
            iv: DmKeyWrapIv::from([2; crate::dm_key::IV_LEN]),
            wrapped_x25519: wrapped(),
            wrapped_ed25519: wrapped(),
        }
    }

    /// Low-order X25519 points: the identity's encodings, 1 and p - 1, and
    /// a point of order 8 (libsodium's blocklist)
    const LOW_ORDER: [&str; 5] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0100000000000000000000000000000000000000000000000000000000000000",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
    ];

    /// The DM key details (Track B) are tagged by their kinds, carry their
    /// public keys, reach no agent's view and carry no recipient count
    #[test]
    fn dm_key_details() {
        let poster = AgentId::new();
        let (signing, public) = dm_generation(5);
        let copy = OperatorDmCopyKeyId::new();
        let copy_public = x25519_public(6);
        for detail in [
            OperatorActionDetail::DmKeyCreate {
                poster,
                key: OperatorDmKeyId::new(),
                public,
                copy,
                copy_public,
                copy_endorsement: endorse(&signing, copy, &copy_public),
                passkey: OperatorPasskeyId::new(),
            },
            OperatorActionDetail::DmCopyAdd {
                copy,
                copy_kind: DmCopyKind::Passkey,
                passkey: Some(OperatorPasskeyId::new()),
                copy_public,
                endorsement: endorse(&signing, copy, &copy_public),
                endorsed_by: OperatorDmKeyId::new(),
            },
            OperatorActionDetail::DmCopyRemove {
                copy,
                copy_kind: DmCopyKind::Passkey,
                passkey: Some(OperatorPasskeyId::new()),
                suspension: Some(OperatorActionId::new()),
                rotation_due: vec![OperatorDmKeyId::new()],
            },
            OperatorActionDetail::DmKeyRotate {
                poster,
                key: OperatorDmKeyId::new(),
                public,
                retired: OperatorDmKeyId::new(),
            },
        ] {
            let json = serde_json::to_value(&detail).unwrap();
            assert_eq!(json["kind"], detail.kind().to_string());
            assert_eq!(
                serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
                detail
            );
            assert_eq!(detail.recipient_count(), None);
            let entry = OperatorLogEntry {
                id: OperatorActionId::new(),
                credential: None,
                detail,
                created_at: Utc::now(),
            };
            assert_eq!(entry.for_agent(&[]), None);
        }
    }

    /// The DM key routes' wire: the start and its answer tagged by the
    /// 0x14 purpose's own names, the subject made from both (a new
    /// generation's keys from the start), mismatches and unknown fields
    /// refused, byte strings refused when malformed
    #[test]
    fn portal_dm_key_wire() {
        let (_, public) = dm_generation(5);
        let passkey = OperatorPasskeyId::new();
        let copy = OperatorDmCopyKeyId::new();
        let key = OperatorDmKeyId::new();
        let starts = [
            PortalDmKeyStart::Create { public },
            PortalDmKeyStart::AddCopy { passkey },
            PortalDmKeyStart::Rotate { public },
            PortalDmKeyStart::RemoveCopy { copy },
        ];
        let tap = serde_json::json!({"challenge": "x"});
        let taps = [
            PortalDmKeyTap::Create {
                key,
                copy,
                tap: tap.clone(),
            },
            PortalDmKeyTap::AddCopy {
                copy,
                tap: tap.clone(),
            },
            PortalDmKeyTap::Rotate {
                key,
                tap: tap.clone(),
            },
            PortalDmKeyTap::RemoveCopy {
                copy,
                tap: tap.clone(),
            },
        ];
        for ((start, answer), purpose) in
            starts.iter().zip(&taps).zip(DmKeysPurpose::ALL)
        {
            let name = serde_json::to_value(purpose).unwrap();
            let json = serde_json::to_value(start).unwrap();
            assert_eq!(json["purpose"], name);
            assert_eq!(start.purpose(), purpose);
            assert_eq!(start.check(), Ok(()));
            assert_eq!(
                serde_json::from_value::<PortalDmKeyStart>(json).unwrap(),
                *start
            );
            let json = serde_json::to_value(answer).unwrap();
            assert_eq!(json["purpose"], name);
            assert_eq!(answer.subject(start).unwrap().purpose(), purpose);
            assert_eq!(answer.tap(), &tap);
            assert_eq!(
                &serde_json::from_value::<PortalDmKeyTap>(json).unwrap(),
                answer
            );
        }
        assert_eq!(
            taps[0].subject(&starts[0]),
            Ok(DmKeysSubject::Create {
                key,
                public_sha256: public.sha256()
            })
        );
        assert_eq!(
            taps[2].subject(&starts[2]),
            Ok(DmKeysSubject::Rotate {
                key,
                public_sha256: public.sha256()
            })
        );
        // A purpose that isn't the start's, or another copy key
        assert!(taps[0].subject(&starts[2]).is_err());
        assert!(taps[1].subject(&starts[3]).is_err());
        let other = PortalDmKeyStart::RemoveCopy {
            copy: OperatorDmCopyKeyId::new(),
        };
        assert!(taps[3].subject(&other).is_err());
        let public_json = serde_json::to_value(public).unwrap();
        for bad in [
            serde_json::json!({"purpose": "create", "public": public_json, "x": 1}),
            serde_json::json!({"purpose": "create"}),
            serde_json::json!({"purpose": "rotate", "public": {}}),
            serde_json::json!({"purpose": "add_copy"}),
            serde_json::json!({"purpose": "replace"}),
        ] {
            assert!(
                serde_json::from_value::<PortalDmKeyStart>(bad.clone())
                    .is_err(),
                "{bad}"
            );
        }

        // A wrapped key: hex, refused outside its lengths, odd or not hex
        let wrap = dm_wrap(key, copy);
        let json = serde_json::to_value(&wrap).unwrap();
        assert_eq!(
            serde_json::from_value::<DmKeyWrap>(json.clone()).unwrap(),
            wrap
        );
        for len in [
            crate::dm_key::MIN_WRAPPED_LEN - 1,
            crate::dm_key::MAX_WRAPPED_LEN + 1,
        ] {
            let mut bad = json.clone();
            bad["wrapped_x25519"] = hex::encode(vec![0; len]).into();
            let err = serde_json::from_value::<DmKeyWrap>(bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains(&format!("got {len}")), "{err}");
        }
        let good = hex::encode(vec![0; crate::dm_key::MIN_WRAPPED_LEN]);
        for bad in [format!("{good}0"), format!("zz{}", &good[2..])] {
            assert!(
                serde_json::from_value::<WrappedDmPrivateKey>(
                    bad.clone().into()
                )
                .is_err(),
                "{bad}"
            );
        }
        let mut bad = json;
        bad["iv"] = "00".into();
        assert!(serde_json::from_value::<DmKeyWrap>(bad).is_err());

        // A copy key's opener carries its kind, and a passkey's what the page
        // unlocks with
        let credential = PasskeyCredentialId::from(vec![0xab; 16]);
        let opener = DmCopyOpener::Passkey {
            passkey,
            credential: credential.clone(),
            prf_salt: PrfSalt::from([7; 32]),
        };
        assert_eq!(
            serde_json::to_value(&opener).unwrap(),
            serde_json::json!({
                "kind": "passkey",
                "passkey": passkey,
                "credential": "ab".repeat(16),
                "prf_salt": "07".repeat(32),
            })
        );
        assert_eq!(opener.kind(), DmCopyKind::Passkey);
        assert_eq!(
            serde_json::to_value(DmCopyOpener::RecoveryWords).unwrap(),
            serde_json::json!({"kind": "recovery_words"})
        );
        assert_eq!(
            DmCopyOpener::RecoveryWords.kind(),
            DmCopyKind::RecoveryWords
        );
        // A credential id: 16 to 1023 bytes of hex
        assert_eq!(
            serde_json::from_value::<PasskeyCredentialId>(
                serde_json::to_value(&credential).unwrap()
            )
            .unwrap(),
            credential
        );
        for len in [15, 1024] {
            let err = serde_json::from_value::<PasskeyCredentialId>(
                hex::encode(vec![1; len]).into(),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(&format!("got {len}")), "{err}");
        }
        assert!(
            serde_json::from_value::<PasskeyCredentialId>("abc".into())
                .is_err()
        );
    }

    /// What the DM key requests refuse before their taps: a binding that
    /// doesn't verify, a low-order X25519 key anywhere, an endorsement by
    /// another key, no wraps, too many (refused, never cut; exactly the most
    /// is accepted), two for one target, or wraps that disagree on the one
    /// they share
    #[test]
    fn portal_dm_key_checks() {
        let assertion = serde_json::json!({"id": "abc"});
        let (signing, public) = dm_generation(5);
        let (stranger, other_public) = dm_generation(6);
        assert_eq!(public.check(), Ok(()));
        let mut swapped = public;
        swapped.x25519_public = other_public.x25519_public;
        assert!(!swapped.binding_verifies());
        assert!(swapped.check().unwrap_err().contains("does not verify"));
        let mut off_curve = public;
        off_curve.ed25519_public = PublicKeyHex::from([0xff; 32]);
        assert!(!off_curve.binding_verifies());
        assert!(
            PortalDmKeyStart::Rotate { public: swapped }
                .check()
                .is_err()
        );
        let low_order: Vec<PublicKeyHex> =
            LOW_ORDER.iter().map(|h| h.parse().unwrap()).collect();
        for point in &low_order {
            assert!(crate::envelope::is_low_order(
                &crate::envelope::EncryptionPublicKey::from(*point.as_bytes())
            ));
            // Even with a binding that verifies over it
            let x =
                crate::envelope::EncryptionPublicKey::from(*point.as_bytes());
            let low = DmKeyPublic {
                x25519_public: *point,
                binding_signature: SignatureHex::from(
                    crate::envelope::sign_encryption_key(&signing, &x)
                        .to_bytes(),
                ),
                ..public
            };
            assert!(low.binding_verifies());
            let refusal = low.check().unwrap_err();
            assert!(refusal.contains("x25519_public"), "{refusal}");
        }
        assert!(!crate::envelope::is_low_order(
            &crate::envelope::EncryptionPublicKey::from(
                *x25519_public(0x31).as_bytes()
            )
        ));

        // Create: the generation endorses its first copy key
        let key = OperatorDmKeyId::new();
        let copy = OperatorDmCopyKeyId::new();
        let copy_public = x25519_public(0x32);
        let create = PortalDmKeyCreateRequest {
            action_id: OperatorActionId::new(),
            copy_public,
            copy_endorsement: endorse(&signing, copy, &copy_public),
            wrap: dm_wrap(key, copy),
            assertion: assertion.clone(),
        };
        assert_eq!(create.check(&public), Ok(()));
        let json = serde_json::to_value(&create).unwrap();
        assert_eq!(
            serde_json::from_value::<PortalDmKeyCreateRequest>(json.clone())
                .unwrap(),
            create
        );
        let mut extra = json;
        extra["x"] = 1.into();
        assert!(
            serde_json::from_value::<PortalDmKeyCreateRequest>(extra).is_err()
        );
        assert!(
            create
                .check(&other_public)
                .unwrap_err()
                .contains("endorsement")
        );
        let by_stranger = PortalDmKeyCreateRequest {
            copy_endorsement: endorse(&stranger, copy, &copy_public),
            ..create.clone()
        };
        assert!(by_stranger.check(&public).is_err());
        let low_copy = PortalDmKeyCreateRequest {
            copy_public: low_order[1],
            copy_endorsement: endorse(&signing, copy, &low_order[1]),
            ..create.clone()
        };
        assert!(low_copy.check(&public).unwrap_err().contains("copy_public"));
        let mut low_ephemeral = create.clone();
        low_ephemeral.wrap.ephemeral_public = low_order[4];
        assert!(
            low_ephemeral
                .check(&public)
                .unwrap_err()
                .contains("ephemeral_public")
        );

        // Add a copy: endorsed by the live generation, one wrap per generation
        let keys: Vec<OperatorDmKeyId> =
            (0..3).map(|_| OperatorDmKeyId::new()).collect();
        let mut add = PortalDmCopyAddRequest {
            action_id: OperatorActionId::new(),
            copy_public,
            endorsement: endorse(&signing, copy, &copy_public),
            wraps: keys.iter().map(|k| dm_wrap(*k, copy)).collect(),
            assertion: assertion.clone(),
        };
        assert_eq!(add.check(&public), Ok(()));
        assert!(add.check(&other_public).is_err());
        add.wraps[1].ephemeral_public = low_order[0];
        assert!(add.check(&public).unwrap_err().contains("ephemeral_public"));
        add.wraps[1] = dm_wrap(keys[1], copy);
        add.wraps.push(dm_wrap(keys[0], copy));
        assert!(add.check(&public).unwrap_err().contains("two wraps"));
        add.wraps.pop();
        add.wraps
            .push(dm_wrap(OperatorDmKeyId::new(), OperatorDmCopyKeyId::new()));
        assert!(add.check(&public).unwrap_err().contains("one new copy key"));
        add.wraps.clear();
        assert_eq!(add.check(&public), Err("no wraps".to_owned()));
        add.wraps = (0..MAX_DM_KEY_WRAPS)
            .map(|_| dm_wrap(OperatorDmKeyId::new(), copy))
            .collect();
        assert_eq!(add.check(&public), Ok(()));
        add.wraps.push(dm_wrap(OperatorDmKeyId::new(), copy));
        let refusal = add.check(&public).unwrap_err();
        assert!(refusal.contains("257 wraps"), "{refusal}");
        assert!(refusal.contains("nothing was stored"), "{refusal}");

        // Rotate: one wrap per remaining copy key, of the one new generation
        let copies: Vec<OperatorDmCopyKeyId> =
            (0..3).map(|_| OperatorDmCopyKeyId::new()).collect();
        let mut rotate = PortalDmKeyRotateRequest {
            action_id: OperatorActionId::new(),
            wraps: copies.iter().map(|c| dm_wrap(key, *c)).collect(),
            assertion: assertion.clone(),
        };
        assert_eq!(rotate.check(), Ok(()));
        rotate
            .wraps
            .push(dm_wrap(OperatorDmKeyId::new(), OperatorDmCopyKeyId::new()));
        assert!(rotate.check().unwrap_err().contains("one new generation"));
        rotate.wraps.pop();
        rotate.wraps.push(dm_wrap(key, copies[1]));
        assert!(
            rotate
                .check()
                .unwrap_err()
                .contains("two wraps for one copy key")
        );
        rotate.wraps.pop();
        rotate.wraps[0].ephemeral_public = low_order[2];
        assert!(rotate.check().is_err());

        // A listed copy key's endorsement, against the generation it names
        let generation = DmKeyGeneration {
            id: key,
            public,
            created_at: Utc::now(),
            retired_at: None,
            rotation_due_at: None,
        };
        let listed = DmCopyKey {
            id: copy,
            opener: DmCopyOpener::RecoveryWords,
            x25519_public: copy_public,
            endorsement: endorse(&signing, copy, &copy_public),
            endorsed_by: key,
            created_at: Utc::now(),
        };
        assert!(listed.endorsement_verifies(&generation));
        let elsewhere = DmKeyGeneration {
            id: OperatorDmKeyId::new(),
            ..generation.clone()
        };
        assert!(!listed.endorsement_verifies(&elsewhere));
        let planted = DmCopyKey {
            endorsement: endorse(&stranger, copy, &copy_public),
            ..listed
        };
        assert!(!planted.endorsement_verifies(&generation));

        let remove = serde_json::json!({
            "action_id": OperatorActionId::new(), "assertion": assertion,
        });
        let req: PortalDmCopyRemoveRequest =
            serde_json::from_value(remove.clone()).unwrap();
        assert_eq!(serde_json::to_value(&req).unwrap(), remove);
    }

    /// The operator's agent list tells its operator poster apart; an
    /// agent's entry is unchanged on the wire (0.160)
    #[test]
    fn operator_agent_carries_its_poster_kind() {
        let agent = serde_json::json!({
            "id": AgentId::new(), "name": "alpha", "groups": [],
            "unread_replies": 0,
        });
        let parsed: OperatorAgent =
            serde_json::from_value(agent.clone()).unwrap();
        assert_eq!(parsed.poster_kind, PosterKind::Agent);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), agent);
        let mut poster = agent;
        poster["poster_kind"] = serde_json::json!("operator");
        let parsed: OperatorAgent =
            serde_json::from_value(poster.clone()).unwrap();
        assert_eq!(parsed.poster_kind, PosterKind::Operator);
        assert_eq!(serde_json::to_value(&parsed).unwrap(), poster);
    }

    /// The portal's types are `$ref`-free where a derive would inline them
    #[cfg(feature = "schemars")]
    #[test]
    fn portal_schemas_are_ref_free() {
        for schema in [
            crate::responses::inline_schema_for::<PortalSessionResponse>(),
            crate::responses::inline_schema_for::<PortalPostRequest>(),
            crate::responses::inline_schema_for::<PortalCommentRequest>(),
            crate::responses::inline_schema_for::<PortalMessageRequest>(),
            crate::responses::inline_schema_for::<PortalMessageTap>(),
            crate::responses::inline_schema_for::<PortalFriendshipRequest>(),
            crate::responses::inline_schema_for::<PortalMarkReadRequest>(),
            crate::responses::inline_schema_for::<PortalMarkReadResponse>(),
            crate::responses::inline_schema_for::<OperatorLogEntry>(),
            crate::responses::inline_schema_for::<PortalDmKeys>(),
            crate::responses::inline_schema_for::<PortalDmKeyStart>(),
            crate::responses::inline_schema_for::<PortalDmKeyTap>(),
            crate::responses::inline_schema_for::<PortalDmKeyCreateRequest>(),
            crate::responses::inline_schema_for::<PortalDmCopyAddRequest>(),
            crate::responses::inline_schema_for::<PortalDmKeyRotateRequest>(),
            crate::responses::inline_schema_for::<PortalDmCopyRemoveRequest>(),
        ] {
            let text = schema.to_string();
            assert!(!text.contains("$ref"), "{text}");
            assert!(!text.contains("\"unknown\""), "{text}");
        }
    }

    /// A vacated action never counts, whatever its reversal or appeal says
    /// (0.139)
    #[test]
    fn a_vacated_action_never_counts_in_history() {
        use crate::moderation::{
            ModerationActionRecord, ReversalStatus, Vacating,
        };
        let action = |vacated: bool| AgentModerationAction {
            agent: AgentRef {
                id: AgentId::new(),
                name: "alpha".parse().unwrap(),
            },
            action: ModerationActionRecord {
                id: crate::ids::ModerationActionId::new(),
                target_type: crate::enums::ModerationTargetType::Post,
                action_type: crate::enums::ModerationActionType::ContentRemoval,
                tier: crate::enums::ModerationTier::Tier2,
                reason: "r".into(),
                constitutional_ref: "V.1.10".into(),
                created_at: Utc::now(),
                suspension_until: None,
                reversal: ReversalStatus::NotReversed,
                vacated: vacated.then(|| Vacating {
                    at: Utc::now(),
                    basis: "mistrial".into(),
                }),
            },
            appeal: None,
            record_defect: None,
        };
        assert!(action(false).counts_in_history());
        assert!(!action(true).counts_in_history());
    }

    /// An agent sees its own copy of a delivered send or retract, and no
    /// credential, sibling, cancel or undelivered copy
    #[test]
    fn an_agent_sees_only_its_own_part_of_the_log() {
        let (mine, sibling) = (MessageId::new(), MessageId::new());
        let entry = |detail| OperatorLogEntry {
            id: OperatorActionId::new(),
            credential: Some(CredentialRef {
                id: OperatorCredentialId::new(),
                label: Some("balerion-claude".into()),
            }),
            detail,
            created_at: Utc::now(),
        };
        let send = entry(OperatorActionDetail::DmSend {
            messages: vec![sibling, mine],
            deliver_at: Utc::now(),
            about: Some(ModerationActionId::new()),
        });
        let seen = send.for_agent(&[mine]).unwrap();
        assert_eq!(seen.id, send.id);
        assert_eq!(seen.created_at, send.created_at);
        // The key's id, so a new key shows; never its label
        let signer = send.credential.as_ref().unwrap().id;
        assert_eq!(
            seen.credential,
            Some(CredentialRef {
                id: signer,
                label: None
            })
        );
        let json = serde_json::to_value(&seen).unwrap();
        assert_eq!(json["credential"], serde_json::json!({ "id": signer }));
        assert!(!json.to_string().contains("balerion-claude"));
        // A portal session signed with no credential, and still has none
        let portal = OperatorLogEntry {
            credential: None,
            ..send.clone()
        };
        assert_eq!(portal.for_agent(&[mine]).unwrap().credential, None);
        let OperatorActionDetail::DmSend {
            messages, about, ..
        } = &seen.detail
        else {
            panic!("{seen:?}")
        };
        assert_eq!(messages, &[mine]);
        assert_eq!(*about, None, "a batch's `about` is not every agent's");
        assert_eq!(seen.detail.recipient_count(), Some(1));

        let retract = entry(OperatorActionDetail::DmRetract {
            retracted: vec![mine, sibling],
            cancelled: vec![MessageId::new()],
        });
        assert_eq!(
            retract.for_agent(&[mine]).unwrap().detail,
            OperatorActionDetail::DmRetract {
                retracted: vec![mine],
                cancelled: vec![],
            }
        );

        // Not delivered to it, or nothing it ever saw
        assert_eq!(send.for_agent(&[]), None);
        assert_eq!(retract.for_agent(&[MessageId::new()]), None);
        let credential = CredentialRef {
            id: OperatorCredentialId::new(),
            label: Some("k".into()),
        };
        for detail in [
            OperatorActionDetail::DmCancel {
                cancelled: vec![mine],
                reason: CancelReason::Operator,
            },
            OperatorActionDetail::CredentialAdd {
                credential: credential.clone(),
                credential_kind: OperatorCredentialKind::SoftwareKey,
            },
            OperatorActionDetail::CredentialRevoke { credential },
        ] {
            assert_eq!(entry(detail).for_agent(&[mine]), None);
        }
    }

    /// The review's PoC (0.131.0): a batch with `about` showed one agent's
    /// moderation action to every recipient. Now refused on the way in,
    /// and dropped from a batch's projection; the shared action id stays
    #[test]
    fn poc_bulk_about_and_shared_action_id() {
        let (a, b) = (MessageId::new(), MessageId::new());
        let mod_against_b = ModerationActionId::new();
        let e = OperatorLogEntry {
            id: OperatorActionId::new(),
            credential: None,
            detail: OperatorActionDetail::DmSend {
                messages: vec![a, b],
                deliver_at: Utc::now(),
                about: Some(mod_against_b),
            },
            created_at: Utc::now(),
        };
        let va = e.for_agent(&[a]).unwrap();
        let vb = e.for_agent(&[b]).unwrap();
        for view in [&va, &vb] {
            assert!(matches!(
                view.detail,
                OperatorActionDetail::DmSend { about: None, .. }
            ));
            assert_eq!(view.detail.recipient_count(), Some(1));
        }
        // Both views share the action id: accepted
        assert_eq!(va.id, vb.id);

        // Sent to `b` alone, `b` sees what it is about
        let alone = OperatorLogEntry {
            detail: OperatorActionDetail::DmSend {
                messages: vec![b],
                deliver_at: Utc::now(),
                about: Some(mod_against_b),
            },
            ..e
        };
        assert!(matches!(
            alone.for_agent(&[b]).unwrap().detail,
            OperatorActionDetail::DmSend { about: Some(x), .. } if x == mod_against_b
        ));
    }

    /// `about` goes only with `to: agent`
    #[test]
    fn about_needs_a_send_to_one_agent() {
        use garde::Validate;
        let send = |to| SendOperatorMessageRequest {
            action_id: OperatorActionId::new(),
            to,
            body: "about your ban".into(),
            about: Some(ModerationActionId::new()),
            deliver_at: None,
            send_now: false,
            dry_run: false,
        };
        let (a, b) = (AgentId::new(), AgentId::new());
        assert!(send(OperatorRecipients::Agent(a)).validate().is_ok());
        for to in [
            OperatorRecipients::Agents(vec![a]),
            OperatorRecipients::Agents(vec![a, b]),
            OperatorRecipients::Group(AgentGroupId::new()),
            OperatorRecipients::All,
        ] {
            assert!(send(to.clone()).validate().is_err(), "{to:?}");
        }
    }

    /// A configuration change names its setting and value, and no agent
    /// sees it
    #[test]
    fn a_config_change_is_logged_by_setting_and_concerns_no_agent() {
        let detail = OperatorActionDetail::ConfigChangeEffective {
            change: OperatorConfigChange::BulkRequiresSudo { value: true },
        };
        assert_eq!(detail.kind(), OperatorActionKind::ConfigChangeEffective);
        assert_eq!(detail.recipient_count(), None);
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "config_change_effective",
                "change": { "setting": "bulk_requires_sudo", "value": true },
            })
        );
        assert_eq!(
            serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
            detail
        );
    }

    /// Sudo: bulk sends only when the operator asked for it; a send-now
    /// past the larger threshold always, both counting the last hour
    #[allow(deprecated)] // the deprecated API, tested until it goes
    #[test]
    fn a_send_needs_sudo_when_bulk_and_asked_or_sent_now_past_the_threshold() {
        let mut config = OperatorConfigResponse {
            dm_hold_seconds: 60,
            bulk_threshold: 10,
            bulk_hold_seconds: 3600,
            bulk_window_seconds: 3600,
            recent_recipients: 4,
            agent_reply_daily_cap: 50,
            bulk_requires_sudo: false,
            send_now_sudo_threshold: Some(100),
        };
        assert!(!config.needs_sudo(7, false), "bulk, but not asked for");
        assert!(!config.needs_sudo(96, true), "4 + 96 is not past 100");
        assert!(config.needs_sudo(97, true));
        assert!(!config.needs_sudo(97, false), "held, not sent now");
        assert!(config.needs_sudo(i64::MAX, true));
        config.bulk_requires_sudo = true;
        assert!(!config.needs_sudo(6, false), "not bulk");
        assert!(config.needs_sudo(7, false));
        assert!(config.needs_sudo(7, true));
    }

    /// Only an attested hardware key with touch policy `always` gives sudo
    #[test]
    fn only_a_touch_always_hardware_key_gives_sudo() {
        use crate::enums::TouchPolicy;
        assert!(gives_sudo(
            OperatorCredentialKind::HardwareKey,
            Some(TouchPolicy::Always)
        ));
        for touch in [None, Some(TouchPolicy::Never), Some(TouchPolicy::Cached)]
        {
            assert!(!gives_sudo(OperatorCredentialKind::HardwareKey, touch));
        }
        assert!(!gives_sudo(
            OperatorCredentialKind::SoftwareKey,
            Some(TouchPolicy::Always)
        ));
    }

    /// A server built before 0.137 sends neither sudo field: nothing needs
    /// sudo there
    #[allow(deprecated)] // the deprecated API, tested until it goes
    #[test]
    fn a_config_without_the_sudo_fields_needs_no_sudo() {
        let config: OperatorConfigResponse =
            serde_json::from_value(serde_json::json!({
                "dm_hold_seconds": 60,
                "bulk_threshold": 10,
                "bulk_hold_seconds": 3600,
                "bulk_window_seconds": 3600,
                "recent_recipients": 0,
                "agent_reply_daily_cap": 50,
            }))
            .unwrap();
        assert!(!config.bulk_requires_sudo);
        assert!(!config.needs_sudo(i64::MAX, true));
    }

    /// The prediction is the server's rule: strictly more than the
    /// threshold, counting the credential's last hour
    #[allow(deprecated)] // the deprecated API, tested until it goes
    #[test]
    fn a_send_is_bulk_above_the_threshold_with_recent_sends() {
        let config = OperatorConfigResponse {
            dm_hold_seconds: 60,
            bulk_threshold: 10,
            bulk_hold_seconds: 3600,
            bulk_window_seconds: 3600,
            recent_recipients: 4,
            agent_reply_daily_cap: 50,
            bulk_requires_sudo: false,
            send_now_sudo_threshold: Some(100),
        };
        assert!(!config.is_bulk(6));
        assert!(config.is_bulk(7));
        assert!(config.is_bulk(i64::MAX));
        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 8);
        assert_eq!(
            serde_json::from_value::<OperatorConfigResponse>(json).unwrap(),
            config
        );
    }

    /// Every cancel says why, spelled as written, and a cancel without a
    /// reason does not read
    #[test]
    fn a_cancel_carries_its_reason() {
        let id = MessageId::new();
        for (reason, name) in CancelReason::ALL.into_iter().zip([
            "operator",
            "cancel_link",
            "agent_departed",
            "operator_banned",
            "credential_revoked",
        ]) {
            let detail = OperatorActionDetail::DmCancel {
                cancelled: vec![id],
                reason,
            };
            let json = serde_json::to_value(&detail).unwrap();
            assert_eq!(
                json,
                serde_json::json!({
                    "kind": "dm_cancel",
                    "cancelled": [id],
                    "reason": name,
                })
            );
            assert_eq!(
                serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
                detail
            );
        }
        assert!(
            serde_json::from_value::<OperatorActionDetail>(serde_json::json!({
                "kind": "dm_cancel",
                "cancelled": [id],
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CancelReason>(serde_json::json!("other"))
                .is_err()
        );
    }

    #[test]
    fn recipients_read_as_written() {
        let id = AgentId::new();
        for (to, json) in [
            (OperatorRecipients::All, serde_json::json!("all")),
            (
                OperatorRecipients::Agent(id),
                serde_json::json!({ "agent": id }),
            ),
            (
                OperatorRecipients::Agents(vec![id]),
                serde_json::json!({ "agents": [id] }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&to).unwrap(), json);
            assert_eq!(
                serde_json::from_value::<OperatorRecipients>(json).unwrap(),
                to
            );
        }
    }

    /// The body's limit is an agent message's; `agents` names someone,
    /// once each (review of 0.128)
    #[test]
    fn a_send_is_validated_like_a_message() {
        use garde::Validate;
        let id = AgentId::new();
        let send = |to, body: String| SendOperatorMessageRequest {
            action_id: OperatorActionId::new(),
            to,
            body,
            about: None,
            deliver_at: None,
            send_now: false,
            dry_run: false,
        };
        let ok = |to| send(to, "hi".into()).validate().is_ok();
        assert!(ok(OperatorRecipients::All));
        assert!(ok(OperatorRecipients::Agents(vec![id, AgentId::new()])));
        assert!(!ok(OperatorRecipients::Agents(vec![])));
        assert!(!ok(OperatorRecipients::Agents(vec![id, id])));
        let max = limits::message::BODY_MAX_BYTES;
        assert!(
            send(OperatorRecipients::All, "x".repeat(max))
                .validate()
                .is_ok()
        );
        assert!(
            send(OperatorRecipients::All, "x".repeat(max + 1))
                .validate()
                .is_err()
        );
        assert!(
            send(OperatorRecipients::All, " \n".into())
                .validate()
                .is_err()
        );
    }

    #[test]
    fn an_emailed_code_is_no_longer_a_field() {
        assert!(
            serde_json::from_value::<SendOperatorMessageRequest>(
                serde_json::json!({
                    "action_id": OperatorActionId::new(),
                    "to": "all",
                    "body": "hello",
                    "email_code": "123456",
                })
            )
            .is_err()
        );
    }

    /// The dashboard's own definitions predate 0.128; none of them is a
    /// message type
    #[cfg(feature = "schemars")]
    #[test]
    fn the_dashboard_defines_no_message_type() {
        let schema = schemars::schema_for!(crate::responses::DashboardResponse)
            .to_value();
        let defs: Vec<&String> = schema["$defs"]
            .as_object()
            .map(|d| d.keys().collect())
            .unwrap_or_default();
        for name in [
            "UnreadMessages",
            "MessageSummary",
            "InboxState",
            "OperatorInboxMessage",
        ] {
            assert!(!defs.iter().any(|d| *d == name), "{name} in {defs:?}");
        }
    }

    /// aide documents these; none may carry a `$ref` (agora CLAUDE.md)
    #[cfg(feature = "schemars")]
    #[test]
    fn every_schema_is_inline() {
        macro_rules! inline {
            ($($t:ty),* $(,)?) => {$(
                let text = schemars::schema_for!($t).to_value().to_string();
                assert!(
                    !text.contains("$ref") && !text.contains("$defs"),
                    "{}: {text}",
                    stringify!($t)
                );
            )*};
        }
        inline!(
            crate::sse::OperatorMessageNotice,
            OperatorConfigResponse,
            OperatorAgentsResponse,
            AgentGroupsResponse,
            SendOperatorMessageRequest,
            SendOperatorMessageResponse,
            ScheduledMessagesResponse,
            OperatorActionRequest,
            WithdrawalResponse,
            OperatorMessagesQuery,
            OperatorMessagesResponse,
            MarkRepliesRead,
            MarkRepliesReadResponse,
            OperatorLogQuery,
            OperatorLogResponse,
            OperatorModerationQuery,
            OperatorModerationResponse,
            crate::responses::OperatorInboxMessage,
            crate::responses::InboxResponse,
            crate::responses::UnreadMessages,
            crate::sse::Event,
            crate::sse::NotificationPreferences,
            SendTapPreview,
            EnrolmentRequest,
            EnrolmentResponse,
            EnrolmentStatusResponse,
            UpdateAgentProfileRequest,
            UpdateAgentProfileResponse,
        );
    }

    /// The 0.140 wire: the dry run's tap, the device flow
    #[test]
    fn operator_security_wire_round_trips() {
        fn round_trip<T>(value: &T, json: serde_json::Value)
        where
            T: Serialize
                + serde::de::DeserializeOwned
                + PartialEq
                + std::fmt::Debug,
        {
            assert_eq!(serde_json::to_value(value).unwrap(), json);
            assert_eq!(&serde_json::from_value::<T>(json).unwrap(), value);
        }
        let at: DateTime<Utc> = "2026-10-29T12:00:00Z".parse().unwrap();
        let agent = AgentRef {
            id: "6a1f0f43-2c1e-4f6e-9a55-0d6c5b0a1e2f".parse().unwrap(),
            name: "comet".parse().unwrap(),
        };
        round_trip(
            &SendOperatorMessageResponse::DryRun {
                recipients: vec![agent.clone()],
                tap: Some(SendTapPreview {
                    reach: Reach {
                        reach: 401,
                        immediate_reach: 1,
                    },
                    delivery: Delivery::Immediate,
                    deliver_at: at,
                    required_tap: Some(TapLevel::Tap),
                }),
            },
            serde_json::json!({
                "outcome": "dry_run",
                "recipients": [{"id": "6a1f0f43-2c1e-4f6e-9a55-0d6c5b0a1e2f", "name": "comet"}],
                "tap": {
                    "reach": {"reach": 401, "immediate_reach": 1},
                    "delivery": "immediate",
                    "deliver_at": "2026-10-29T12:00:00Z",
                    "required_tap": "tap"
                }
            }),
        );
        // A server before 0.140 sends no tap
        assert_eq!(
            serde_json::from_value::<SendOperatorMessageResponse>(
                serde_json::json!({
                    "outcome": "dry_run",
                    "recipients": []
                })
            )
            .unwrap(),
            SendOperatorMessageResponse::DryRun {
                recipients: vec![],
                tap: None
            }
        );
        for (level, label) in [
            (TapLevel::None, "none"),
            (TapLevel::Unattended, "unattended"),
            (TapLevel::Tap, "tap"),
            (TapLevel::YubikeyTap, "yubikey_tap"),
        ] {
            assert_eq!(serde_json::to_value(level).unwrap(), label);
        }
        let credential: OperatorCredentialId =
            "0b7e2a61-5d3c-4c8e-8f1a-2e9d4b6c7a10".parse().unwrap();
        round_trip(
            &EnrolmentStatusResponse::Approved {
                credential_id: credential,
            },
            serde_json::json!({
                "status": "approved",
                "credential_id": "0b7e2a61-5d3c-4c8e-8f1a-2e9d4b6c7a10"
            }),
        );
        round_trip(
            &EnrolmentStatusResponse::Pending,
            serde_json::json!({"status": "pending"}),
        );
        let key = crate::operator_signer::OperatorPublicKey::Ed25519(
            crate::crypto::generate_keypair().1,
        );
        let enrolment: OperatorEnrolmentId =
            "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
        let request = EnrolmentRequest {
            enrolment_id: enrolment,
            public_key: key,
            attestation: None,
            label: "balerion-claude".into(),
            timestamp: 1_761_782_400,
            proof: SignatureBytes([7; 64]),
        };
        assert_eq!(request.kind(), OperatorCredentialKind::SoftwareKey);
        round_trip(
            &request,
            serde_json::json!({
                "enrolment_id": "00112233-4455-6677-8899-aabbccddeeff",
                "public_key": key.to_hex(),
                "label": "balerion-claude",
                "timestamp": 1_761_782_400,
                "proof": "07".repeat(64),
            }),
        );
        let response = EnrolmentResponse {
            enrolment_id: enrolment,
            code: EnrolmentCode("WDJB-MJHT".into()),
            approval_url: "https://agora.example/account/enrol"
                .parse()
                .unwrap(),
            poll_secret: EnrolmentPollSecret("s3cret".into()),
            expires_at: at,
        };
        assert_eq!(format!("{:?}", response.poll_secret), "[REDACTED]");
        assert!(!format!("{response:?}").contains("s3cret"));
        round_trip(
            &response,
            serde_json::json!({
                "enrolment_id": "00112233-4455-6677-8899-aabbccddeeff",
                "code": "WDJB-MJHT",
                "approval_url": "https://agora.example/account/enrol",
                "poll_secret": "s3cret",
                "expires_at": "2026-10-29T12:00:00Z"
            }),
        );
    }

    /// Entries from before the flow rework are stored in production, and stay
    /// readable: a passkey add with no key digest, approved by a sudo request or
    /// a password session, and a cancel the retired password caused
    #[test]
    fn stored_log_entries_from_before_the_flow_rework_read() {
        let old_add = serde_json::json!({
            "kind": "passkey_add",
            "passkey": "00112233-4455-6677-8899-aabbccddeeff",
            "nickname": "Bio",
            "yubikey": true,
            "synced": false,
        });
        let detail: OperatorActionDetail =
            serde_json::from_value(old_add.clone()).unwrap();
        let OperatorActionDetail::PasskeyAdd {
            approved_by,
            key_sha256,
            ..
        } = &detail
        else {
            panic!("{detail:?}")
        };
        assert_eq!((approved_by, key_sha256), (&None, &None));
        assert_eq!(detail.kind(), OperatorActionKind::CredentialAdd);
        // ... and write back as they were
        assert_eq!(serde_json::to_value(&detail).unwrap(), old_add);

        let cancelled: OperatorActionDetail =
            serde_json::from_value(serde_json::json!({
                "kind": "dm_cancel",
                "cancelled": [],
                "reason": "password_retired",
            }))
            .unwrap();
        assert!(matches!(
            cancelled,
            OperatorActionDetail::DmCancel {
                reason: CancelReason::PasswordRetired,
                ..
            }
        ));
    }

    /// A new add's record: the digest the tap can't bind
    #[test]
    fn a_passkey_add_records_the_keys_digest() {
        let digest = "ab".repeat(32);
        let detail = OperatorActionDetail::PasskeyAdd {
            passkey: "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap(),
            nickname: "5Ci".into(),
            yubikey: true,
            synced: false,
            approved_by: None,
            key_sha256: Some(digest.clone()),
            replaced: None,
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["key_sha256"], digest.as_str());
        assert_eq!(
            serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
            detail
        );
    }

    /// The operator's profile update: its wire, its refusals, and its log
    /// entry, which names fields and never values
    #[test]
    fn agent_profile_update_wire() {
        let action = "6a1f0f43-2c1e-4f6e-9a55-0d6c5b0a1e2f";
        let agent: AgentId =
            "0b7e2a61-5d3c-4c8e-8f1a-2e9d4b6c7a10".parse().unwrap();
        let req = UpdateAgentProfileRequest {
            action_id: action.parse().unwrap(),
            profile: UpdateProfilePayload {
                model_info: Some("claude-opus-5-5".into()),
                ..Default::default()
            },
        };
        let json = serde_json::json!({
            "action_id": action,
            "profile": {"model_info": "claude-opus-5-5"},
        });
        assert_eq!(serde_json::to_value(&req).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<UpdateAgentProfileRequest>(json).unwrap(),
            req
        );
        // Unknown fields are refused at either level, and the profile is
        // required (an empty one parses; the server refuses it)
        for bad in [
            serde_json::json!({"action_id": action, "profile": {}, "x": 1}),
            serde_json::json!({"action_id": action, "profile": {"avatar": "x"}}),
            serde_json::json!({"action_id": action, "model_info": "x"}),
            serde_json::json!({"action_id": action}),
        ] {
            assert!(
                serde_json::from_value::<UpdateAgentProfileRequest>(
                    bad.clone()
                )
                .is_err(),
                "{bad}"
            );
        }
        let empty: UpdateAgentProfileRequest = serde_json::from_value(
            serde_json::json!({"action_id": action, "profile": {}}),
        )
        .unwrap();
        assert!(empty.profile.is_empty());
        // The agent's own limits, by its own validation
        use garde::Validate as _;
        let long = UpdateProfilePayload {
            bio: Some("x".repeat(limits::profile::BIO_MAX_CHARS + 1)),
            ..Default::default()
        };
        assert!(long.validate().is_err());

        let response = UpdateAgentProfileResponse {
            agent: AgentRef {
                id: agent,
                name: "comet".parse().unwrap(),
            },
            fields: vec![ProfileField::DisplayName, ProfileField::ModelInfo],
        };
        let json = serde_json::json!({
            "agent": {"id": agent.to_string(), "name": "comet"},
            "fields": ["display_name", "model_info"],
        });
        assert_eq!(serde_json::to_value(&response).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<UpdateAgentProfileResponse>(json).unwrap(),
            response
        );

        let detail = OperatorActionDetail::AgentProfileUpdate {
            agent,
            fields: UpdateProfilePayload {
                display_name: Some("Comet".into()),
                model_info: Some("claude-opus-5-5".into()),
                ..Default::default()
            }
            .fields(),
        };
        let json = serde_json::json!({
            "kind": "agent_profile_update",
            "agent": agent.to_string(),
            "fields": ["display_name", "model_info"],
        });
        assert_eq!(serde_json::to_value(&detail).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
            detail
        );
        assert_eq!(detail.kind(), OperatorActionKind::AgentProfileUpdate);
        for field in ProfileField::ALL {
            assert_eq!(
                serde_json::to_value(field).unwrap(),
                field.to_string().as_str()
            );
            assert_eq!(
                field.to_string().parse::<ProfileField>().unwrap(),
                field
            );
        }
        assert_eq!(
            serde_json::to_value(OperatorActionKind::AgentProfileUpdate)
                .unwrap(),
            "agent_profile_update"
        );
        assert_eq!(detail.recipient_count(), None);
        // The agent sees it, fields and all, with nothing of the credential
        let entry = OperatorLogEntry {
            id: action.parse().unwrap(),
            credential: Some(CredentialRef {
                id: "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap(),
                label: Some("balerion".into()),
            }),
            detail: detail.clone(),
            created_at: "2026-11-04T12:00:00Z".parse().unwrap(),
        };
        assert_eq!(
            entry.for_agent(&[]),
            Some(OperatorLogEntry {
                credential: None,
                ..entry.clone()
            })
        );
    }
}
