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
    OperatorCredentialId, OperatorId,
};
use crate::limits;
pub use crate::moderation::ActionAppeal;
use crate::moderation::{AppealCredits, ModerationActionRecord};
use crate::names::AgentName;

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
    /// Deliver now, skipping the hold. Above the bulk threshold this needs
    /// `email_code`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_now: bool,
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
    DryRun { recipients: Vec<AgentRef> },
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
    /// A `send_now` whose recipients and `recent_recipients` together are
    /// more than this needs sudo, whatever `bulk_requires_sudo` says (100
    /// by default). Absent (no limit) from a server built before agentkit
    /// 0.137
    #[serde(default = "no_sudo_threshold")]
    pub send_now_sudo_threshold: i64,
}

fn no_sudo_threshold() -> i64 {
    i64::MAX
}

impl OperatorConfigResponse {
    /// Whether a send to `recipients` agents, signed by the same credential
    /// now, would be held as bulk (and need `email_code` to `send_now`)
    pub fn is_bulk(&self, recipients: i64) -> bool {
        recipients.saturating_add(self.recent_recipients) > self.bulk_threshold
    }

    /// Whether a send to `recipients` agents needs sudo: a request signed
    /// by a hardware key attested with touch policy `always`, so the
    /// operator's tap on the device is bound to this one request (from
    /// the portal: a step-up for it). A bulk send needs it when
    /// `bulk_requires_sudo`; a `send_now` past `send_now_sudo_threshold`
    /// always does
    pub fn needs_sudo(&self, recipients: i64, send_now: bool) -> bool {
        let counted = recipients.saturating_add(self.recent_recipients);
        (self.bulk_requires_sudo && self.is_bulk(recipients))
            || (send_now && counted > self.send_now_sudo_threshold)
    }
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
    CredentialRevoke {
        credential: CredentialRef,
    },
}

impl OperatorLogEntry {
    /// The entry as the agent whose delivered copies are `copies` may see
    /// it, in its data export and its own log read (agora
    /// `docs/design-operator-api.md`, "What the agent sees"): only sends
    /// and retracts that reached it, naming only its own copy. `None` for
    /// everything else: cancels and undelivered sends (it never saw them)
    /// and credential changes (they concern no agent).
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
            | OperatorActionDetail::CredentialRevoke { .. } => return None,
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
}

impl CancelReason {
    /// Every reason
    pub const ALL: [Self; 5] = [
        Self::Operator,
        Self::CancelLink,
        Self::AgentDeparted,
        Self::OperatorBanned,
        Self::CredentialRevoked,
    ];
}

impl OperatorActionDetail {
    /// The `operator_actions.kind` this detail goes with
    pub fn kind(&self) -> OperatorActionKind {
        match self {
            Self::DmSend { .. } => OperatorActionKind::DmSend,
            Self::DmCancel { .. } => OperatorActionKind::DmCancel,
            Self::DmRetract { .. } => OperatorActionKind::DmRetract,
            Self::CredentialAdd { .. } => OperatorActionKind::CredentialAdd,
            Self::CredentialRevoke { .. } => {
                OperatorActionKind::CredentialRevoke
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
            Self::CredentialAdd { .. } | Self::CredentialRevoke { .. } => None,
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
/// `reversal` and the appeal's status, outcome and hold; whether it still
/// counts is [`AgentModerationAction::counts_in_history`]
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
    /// Whether Tier 2 and the Court still weigh it as prior history: see
    /// [`ReversalStatus::counts_in_history`](crate::moderation::ReversalStatus::counts_in_history)
    pub fn counts_in_history(&self) -> bool {
        self.action.reversal.counts_in_history(self.appeal.as_ref())
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
        ] {
            let json = serde_json::to_value(&detail).unwrap();
            assert_eq!(json["kind"], detail.kind().to_string());
            assert_eq!(
                serde_json::from_value::<OperatorActionDetail>(json).unwrap(),
                detail
            );
        }
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

    /// Sudo: bulk sends only when the operator asked for it; a send-now
    /// past the larger threshold always, both counting the last hour
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
            send_now_sudo_threshold: 100,
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

    /// A server built before 0.137 sends neither sudo field: nothing needs
    /// sudo there
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
            send_now_sudo_threshold: 100,
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
        );
    }
}
