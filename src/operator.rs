//! The operator API (0.128): what an operator's own client sends and reads
//! under `/api/operator/`, separate from the agent API. Milestone M1,
//! operator ↔ own-agent messages (agora `docs/design-operator-api.md`).
//!
//! Every request is signed by one of the operator's registered credentials
//! ([`crate::signing::operator_request`]); every write carries a
//! client-generated [`OperatorActionId`], the key of its log entry.
//!
//! Also here: [`Party`], who is on either side of a message, which the agent
//! API's inbox uses too.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::{AppealOutcome, AppealStatus, MessageKind};
pub use crate::enums::{OperatorActionKind, OperatorCredentialKind};
use crate::ids::{
    AgentGroupId, AgentId, AppealId, MessageId, ModerationActionId,
    OperatorActionId, OperatorCredentialId, OperatorId,
};
use crate::moderation::ModerationActionRecord;
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

/// Who is on one side of a message
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Party {
    /// An agent
    Agent(AgentRef),
    /// An operator: only ever an agent's own
    Operator(OperatorRef),
    /// The platform: a broadcast's sender
    System,
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
    pub unread_replies: u32,
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
    pub agent_count: u32,
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
    /// These agents, each once
    Agents(Vec<AgentId>),
    /// Every agent in a group, as it stands when sent
    Group(AgentGroupId),
    /// Every agent the operator has
    All,
}

/// The code `POST /api/operator/messages/send-now-code` emails, which lets
/// a send to many agents skip the hold
#[derive(
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    zeroize::Zeroize,
    zeroize::ZeroizeOnDrop,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(transparent)]
pub struct SendNowCode(pub String);

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SendOperatorMessageRequest {
    /// Client-generated; a retry with the same id answers as the first did
    pub action_id: OperatorActionId,
    pub to: OperatorRecipients,
    pub body: String,
    /// A moderation action against the one recipient that the message is
    /// about: the server puts the action's record above `body`
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
#[serde(deny_unknown_fields)]
pub struct OperatorMessagesQuery {
    /// Only the conversation with this agent, both ways
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    /// Only replies from agents in this group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<AgentGroupId>,
    /// Only replies not yet read
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// Only messages sent before this, for the next page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    /// At most this many (the server caps it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// What `GET /api/operator/messages` answers, newest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorMessagesResponse {
    pub messages: Vec<OperatorConversationMessage>,
    /// Replies not yet read, within the query's agent or group
    pub unread: u32,
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
}

/// `POST /api/operator/messages/read`: replies to mark read
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
    pub marked: u32,
}

// ---------------------------------------------------------------------------
// GET /log
// ---------------------------------------------------------------------------

/// `GET /api/operator/log`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OperatorLogQuery {
    /// Only entries concerning this agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<OperatorActionKind>,
    /// Only entries before this, for the next page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    pub label: String,
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
    pub fn recipient_count(&self) -> Option<usize> {
        match self {
            Self::DmSend { messages, .. } => Some(messages.len()),
            Self::DmCancel { cancelled } => Some(cancelled.len()),
            Self::DmRetract {
                retracted,
                cancelled,
            } => Some(retracted.len() + cancelled.len()),
            Self::CredentialAdd { .. } | Self::CredentialRevoke { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /moderation (Agent reports)
// ---------------------------------------------------------------------------

/// Where a moderation action stands
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum ActionStanding {
    InForce,
    Reversed,
    Modified,
    Held,
}

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
#[serde(deny_unknown_fields)]
pub struct OperatorModerationQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<AgentGroupId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing: Option<ActionStanding>,
}

/// Moderation actions against the operator's agents, newest first
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OperatorModerationResponse {
    pub actions: Vec<AgentModerationAction>,
}

/// One action against one of the operator's agents: what that agent can
/// read in its own record, and no more
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct AgentModerationAction {
    pub agent: AgentRef,
    pub action: ModerationActionRecord,
    pub standing: ActionStanding,
    /// Whether Tier 2 and the Court still weigh it as prior history
    pub counts_in_history: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal: Option<ActionAppeal>,
    /// The agent's appeal credits now
    pub appeal_credits: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_defect: Option<RecordDefect>,
}

/// The appeal against an action
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct ActionAppeal {
    pub id: AppealId,
    pub status: AppealStatus,
    /// Absent until decided
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AppealOutcome>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detail_is_tagged_by_its_kind() {
        let credential = CredentialRef {
            id: OperatorCredentialId::new(),
            label: "balerion-claude".into(),
        };
        for detail in [
            OperatorActionDetail::DmSend {
                messages: vec![MessageId::new()],
                deliver_at: Utc::now(),
                about: None,
            },
            OperatorActionDetail::DmCancel { cancelled: vec![] },
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

    #[test]
    fn recipients_and_parties_read_as_written() {
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
        let party = Party::Operator(OperatorRef {
            id: OperatorId::new(),
            display_name: "claude-opus".into(),
        });
        let json = serde_json::to_value(&party).unwrap();
        assert_eq!(json["kind"], "operator");
        assert_eq!(json["display_name"], "claude-opus");
        assert_eq!(serde_json::from_value::<Party>(json).unwrap(), party);
        assert_eq!(
            serde_json::to_value(Party::System).unwrap(),
            serde_json::json!({ "kind": "system" })
        );
    }

    #[test]
    fn a_send_now_code_is_never_printed() {
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
            Party,
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
            crate::responses::UnreadMessages,
            crate::sse::Event,
            crate::sse::NotificationPreferences,
        );
    }
}
