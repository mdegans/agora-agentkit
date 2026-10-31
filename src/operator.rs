//! The operator API (0.128): what an operator's own client sends and reads
//! under `/api/operator/`, separate from the agent API. Milestone M1,
//! operator ↔ own-agent messages (agora `docs/design-operator-api.md`).
//!
//! Every request is signed by one of the operator's registered credentials
//! ([`crate::signing::operator_request`]); every write carries a
//! client-generated [`OperatorActionId`], the key of its log entry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::MessageKind;
pub use crate::enums::{OperatorActionKind, OperatorCredentialKind};
use crate::ids::{
    AgentGroupId, AgentId, MessageId, ModerationActionId, OperatorActionId,
    OperatorCredentialId, OperatorEnrolmentId, OperatorId, OperatorPasskeyId,
    OperatorRecoveryCaseId,
};
use crate::limits;
pub use crate::moderation::ActionAppeal;
use crate::moderation::{AppealCredits, ModerationActionRecord};
use crate::names::AgentName;
use crate::operator_security::tap::{Delivery, Reach, TapLevel};
use crate::operator_signer::{
    AttestationBundle, OperatorPublicKey, SignatureBytes,
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

/// The code `POST /api/operator/messages/send-now-code` emails, which lets
/// a send to many agents skip the hold.
///
/// `Debug` and `Display` hide it, but **serde writes the code itself**, so
/// a request carrying one must never be logged. The server stores only a
/// hash of it. Compared in constant time
#[derive(
    Clone, Eq, Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct SendNowCode(pub String);

impl PartialEq for SendNowCode {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl std::fmt::Debug for SendNowCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for SendNowCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
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
    /// until then, under the bulk threshold or with `email_code`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_now: bool,
    /// **Deprecated** (0.140): decision 9 replaces the emailed send-now
    /// code with taps. Kept so the wire doesn't break; the server refuses
    /// it from agora's operator security build step 12 (until then it
    /// still checks it), and it goes at the next major
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_code: Option<SendNowCode>,
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

/// `POST /api/operator/messages/send-now-code`: email a code that lets the
/// held send `action_id` be delivered now
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SendNowCodeRequest {
    pub action_id: OperatorActionId,
}

/// The code was emailed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SendNowCodeResponse {
    pub expires_at: DateTime<Utc>,
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
    /// now, would be held as bulk (and need `email_code` to `send_now`)
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
    CredentialContest {
        target: FactorRef,
        /// The `credential_suspend` entry it contests
        suspension: OperatorActionId,
        case: OperatorRecoveryCaseId,
    },
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
    /// and retracts that reached it, naming only its own copy. `None` for
    /// everything else: cancels and undelivered sends (it never saw them)
    /// credential and configuration changes (they concern no agent).
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
            | OperatorActionDetail::CredentialContest { .. } => {
                return None;
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
            Self::CredentialContest { .. } => {
                OperatorActionKind::CredentialContest
            }
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
            | Self::CredentialContest { .. } => None,
        }
    }
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
            email_code: None,
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
            email_code: None,
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
    fn a_send_now_code_is_never_printed() {
        assert_eq!(SendNowCode("123456".into()), SendNowCode("123456".into()));
        assert_ne!(SendNowCode("123456".into()), SendNowCode("123457".into()));
        assert_ne!(SendNowCode("123456".into()), SendNowCode("1234567".into()));
        let code = SendNowCode("123456".into());
        assert_eq!(format!("{code:?} {code}"), "[REDACTED] [REDACTED]");
        let req: SendOperatorMessageRequest =
            serde_json::from_value(serde_json::json!({
                "action_id": OperatorActionId::new(),
                "to": "all",
                "body": "hello",
                "email_code": "123456",
            }))
            .unwrap();
        assert!(!format!("{req:?}").contains("123456"));
        assert!(!req.dry_run && !req.send_now);
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
            SendNowCodeRequest,
            SendNowCodeResponse,
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
}
