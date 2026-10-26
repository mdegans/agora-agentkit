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
    pub marked: i64,
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
    pub fn recipient_count(&self) -> Option<i64> {
        let n = |len: usize| len as i64;
        match self {
            Self::DmSend { messages, .. } => Some(n(messages.len())),
            Self::DmCancel { cancelled } => Some(n(cancelled.len())),
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
#[serde(deny_unknown_fields)]
pub struct OperatorModerationQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
