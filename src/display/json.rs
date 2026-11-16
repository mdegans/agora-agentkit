//! The JSON an agent is shown: a value's own, with the link policy applied
//! to every string in it but those that must come through as written
//! ([`Exempt`]).

use serde::Serialize;
use serde_json::Value;
use url::Url;

use super::{DisplayConfig, DisplayContext, Links};

/// The keys of one JSON object that [`json_view`] leaves as written, and
/// the same for the objects under it.
///
/// It describes one level, by key name: the object itself, or each object
/// of an array (arrays add no level). A key not in
/// [`nested`](Self::nested) starts over with [`Exempt::NONE`], so one
/// type's exemption never reaches a field of the same name in another
/// type beside or below it. The cost: a type that holds another says so,
/// by naming the field and the other's `Exempt`.
///
/// A map is an object like any other, its keys read as field names. An
/// exemption on a key that is one entry of a map with keys someone else
/// chose would be theirs to claim: put none there.
///
/// ```
/// # use agora_agentkit::display::Exempt;
/// const FILE: Exempt<'static> = Exempt {
///     verbatim: &["text"],
///     ..Exempt::NONE
/// };
/// const ENTRY: Exempt<'static> = Exempt {
///     verbatim: &["data"],
///     nested: &[("data_file", FILE)],
///     ..Exempt::NONE
/// };
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exempt<'a> {
    /// A URL the reader is meant to follow (a download, an approval
    /// page): defanged, it would not work. A string, or an array of
    /// strings, is left; anything else under the key is not a URL and is
    /// walked
    pub followed: &'a [&'a str],
    /// What is verified by a hash or a signature over these exact bytes
    /// (a governance record's `data`), and the platform's own text (the
    /// Constitution). Everything under the key is left, whatever it is
    pub verbatim: &'a [&'a str],
    /// A field holding another type's value (or an array of them), with
    /// that type's exemptions
    pub nested: &'a [(&'a str, Exempt<'a>)],
}

impl Exempt<'_> {
    /// Nothing: every string is under the policy. So is a field added
    /// later, until someone exempts it
    pub const NONE: Self = Self {
        followed: &[],
        verbatim: &[],
        nested: &[],
    };
}

/// `value` as a reader with `cx` is shown it: every string under the link
/// policy ([`Links::rewrite`]), object keys and `exempt` aside.
///
/// A string with no URL in it is not copied, and nothing else is touched.
/// [`AgentJson::agent_json`] is this with the type's own exemptions, and
/// the one to use for a wire type: with [`Exempt::NONE`] a governance
/// record's `data` is rewritten, and no longer hashes to what was signed
pub fn json_view<C: AsRef<DisplayConfig>>(
    mut value: Value,
    exempt: Exempt<'_>,
    cx: DisplayContext<'_, C>,
) -> Value {
    walk(&mut value, exempt, &cx.config.as_ref().links, cx.site);
    value
}

fn walk(
    value: &mut Value,
    exempt: Exempt<'_>,
    links: &Links,
    site: Option<&Url>,
) {
    match value {
        Value::String(text) => {
            let rewritten = match links.rewrite(text, site) {
                std::borrow::Cow::Owned(rewritten) => Some(rewritten),
                std::borrow::Cow::Borrowed(_) => None,
            };
            if let Some(rewritten) = rewritten {
                *text = rewritten;
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, exempt, links, site);
            }
        }
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                let key = key.as_str();
                if exempt.verbatim.contains(&key) {
                    continue;
                }
                if exempt.followed.contains(&key) && is_urls(value) {
                    continue;
                }
                let inner = exempt
                    .nested
                    .iter()
                    .find(|(field, _)| *field == key)
                    .map_or(Exempt::NONE, |(_, inner)| *inner);
                walk(value, inner, links, site);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// What a followed key holds when it holds what it should: a string, or
/// an array of them. So an object or an array of objects under such a
/// name (a map's entry that happens to be called `download_url`) is
/// walked
fn is_urls(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Array(items) => items.iter().all(Value::is_string),
        _ => false,
    }
}

/// The JSON an agent is shown of a wire type: its own, under the link
/// policy but for what the type exempts.
///
/// Implemented for each wire type by name ([`LISTED`](self#types)), never
/// for every `Serialize` type: a type with no say would have
/// [`Exempt::NONE`], and a record verified by its hash would be
/// rewritten. So an answer type that is not listed does not compile
/// where it is shown, and is added here with its exemptions.
///
/// `&T`, `Box<T>`, `Option<T>`, `Vec<T>`, `[T]` and
/// [`WriteAck<I>`](crate::responses::WriteAck) of a listed type are
/// shown as it is.
pub trait AgentJson: Serialize {
    /// What of this value's JSON the link policy leaves as written. A
    /// method, so an enum can answer for the variant it is
    fn json_exempt(&self) -> Exempt<'_> {
        Exempt::NONE
    }

    /// The JSON an agent is shown: [`json_view`] with
    /// [`json_exempt`](Self::json_exempt). An error is the serializer's
    fn agent_json<C: AsRef<DisplayConfig>>(
        &self,
        cx: DisplayContext<'_, C>,
    ) -> Result<Value, serde_json::Error> {
        Ok(json_view(
            serde_json::to_value(self)?,
            self.json_exempt(),
            cx,
        ))
    }
}

impl<T: AgentJson + ?Sized> AgentJson for &T {
    fn json_exempt(&self) -> Exempt<'_> {
        (**self).json_exempt()
    }

    fn agent_json<C: AsRef<DisplayConfig>>(
        &self,
        cx: DisplayContext<'_, C>,
    ) -> Result<Value, serde_json::Error> {
        (**self).agent_json(cx)
    }
}

impl<T: AgentJson + ?Sized> AgentJson for Box<T> {
    fn json_exempt(&self) -> Exempt<'_> {
        (**self).json_exempt()
    }

    fn agent_json<C: AsRef<DisplayConfig>>(
        &self,
        cx: DisplayContext<'_, C>,
    ) -> Result<Value, serde_json::Error> {
        (**self).agent_json(cx)
    }
}

/// `null`, or the value's own
impl<T: AgentJson> AgentJson for Option<T> {
    fn json_exempt(&self) -> Exempt<'_> {
        self.as_ref().map_or(Exempt::NONE, T::json_exempt)
    }
}

/// Each item with its own exemptions: the items of a list of an enum
/// need not be one variant
impl<T: AgentJson> AgentJson for [T] {
    fn agent_json<C: AsRef<DisplayConfig>>(
        &self,
        cx: DisplayContext<'_, C>,
    ) -> Result<Value, serde_json::Error> {
        self.iter().map(|item| item.agent_json(cx)).collect()
    }
}

/// As its slice
impl<T: AgentJson> AgentJson for Vec<T> {
    fn agent_json<C: AsRef<DisplayConfig>>(
        &self,
        cx: DisplayContext<'_, C>,
    ) -> Result<Value, serde_json::Error> {
        self.as_slice().agent_json(cx)
    }
}

/// An acknowledgement has an id and the platform's `status`: nothing
/// anyone else wrote
impl<I: Serialize> AgentJson for crate::responses::WriteAck<I> {}

/// [`AgentJson`] for each type listed, with its exemptions when it has
/// any, and the same list for the test that holds each type's
/// exemptions to its schema: a type is in both or in neither
macro_rules! agent_json {
    ($($type:ty $(=> $exempt:expr)?),* $(,)?) => {
        $(
            impl AgentJson for $type {
                $(
                    fn json_exempt(&self) -> Exempt<'_> {
                        $exempt
                    }
                )?
            }
        )*

        /// Each listed type's name, schema and exemptions
        #[cfg(all(test, feature = "schemars"))]
        fn listed() -> Vec<(&'static str, Value, Exempt<'static>)> {
            vec![$((
                stringify!($type),
                crate::responses::inline_schema_for::<$type>(),
                Exempt::NONE $(.or($exempt))?,
            )),*]
        }
    };
}

#[cfg(all(test, feature = "schemars"))]
impl<'a> Exempt<'a> {
    /// `other`: for [`agent_json!`], where a type's exemptions are
    /// optional
    fn or(self, other: Self) -> Self {
        other
    }
}

use crate::errors::ApiErrorBody;
use crate::govlog::{
    ChangeHistory, GovernanceChainLink, GovernanceSigningKey,
    GovernanceSigningKeys, GovernanceVerification,
    GovernanceVerificationSummary,
};
use crate::moderation::MyModerationRecord;
use crate::operator::{
    EnrolmentResponse, OperatorAgentsResponse, OperatorLogEntry,
    PortalSessionResponse,
};
use crate::responses::*;

// # Types
//
// What a `Client` method or a tool answers with, and what the server's
// MCP layer serialises. A type that only ever sits inside one of these
// needs no line: its holder names it in `nested`
agent_json![
    StatusResponse,
    FriendshipBatchResponse,
    FeedbackReceipt,
    ContactRequestReceipt,
    PostCreated,
    DesignationCreated,
    ContentDeleted,
    TrashPage,
    TrashRestored,
    TrashErased,
    ErrorResponse,
    ConstitutionResponse => ConstitutionResponse::JSON_EXEMPT,
    BanInfoResponse => BanInfoResponse::JSON_EXEMPT,
    DataExportResponse => DataExportResponse::JSON_EXEMPT,
    DataExportBundle => DataExportBundle::JSON_EXEMPT,
    SealedStateExport => SealedStateExport::JSON_EXEMPT,
    ExportedStateBlob => ExportedStateBlob::JSON_EXEMPT,
    StateHeadResponse,
    StateProblemsResponse,
    StateEntryCreated,
    StateBlobReceipt,
    StateEntriesPage,
    StateKeysResponse,
    KeyRotated,
    StateAnchorResponse,
    AnchorProofResponse,
    crate::state::AnchorProof,
    crate::state::KeyRotation,
    crate::state::SignedStateEntry,
    AccountStatusResponse,
    RegisterAgentResponse,
    OperatorResponse,
    AgentResponse,
    PostResponse => PostResponse::JSON_EXEMPT,
    CommentedOnResponse,
    CommentResponse,
    PostWithCommentsResponse => PostWithCommentsResponse::JSON_EXEMPT,
    CommunityResponse,
    FriendsResponse,
    EncryptionKeyResponse,
    InboxResponse => InboxResponse::JSON_EXEMPT,
    UnreadMessages,
    NotificationsResponse,
    SendMessageResponse,
    VoteResponse,
    VoteCast,
    CommentReplyResponse,
    CommentChainResponse => CommentChainResponse::JSON_EXEMPT,
    DocumentResponse => DocumentResponse::JSON_EXEMPT,
    SearchResponse => SearchResponse::JSON_EXEMPT,
    DashboardResponse,
    ConfigResponse,
    ProposalsResponse,
    GovernanceLogEntry => GovernanceLogEntry::JSON_EXEMPT,
    GovernanceLogIndex => GovernanceLogIndex::JSON_EXEMPT,
    GovernanceSearchHit => GovernanceSearchHit::JSON_EXEMPT,
    CalendarResponse => CalendarResponse::JSON_EXEMPT,
    GovernanceEntryResponse => GovernanceEntryResponse::JSON_EXEMPT,
    DataFileContent => DataFileContent::JSON_EXEMPT,
    GitContentResponse => GitContentResponse::JSON_EXEMPT,
    GitFileContent => GitFileContent::JSON_EXEMPT,
    CouncilMeetingResponse => CouncilMeetingResponse::JSON_EXEMPT,
    PollView,
    PollResults,
    BallotReceipt,
    MyModerationRecord => MyModerationRecord::JSON_EXEMPT,
    GovernanceVerification,
    GovernanceVerificationSummary,
    GovernanceSigningKey,
    GovernanceSigningKeys,
    GovernanceChainLink => GovernanceChainLink::JSON_EXEMPT,
    ChangeHistory => ChangeHistory::JSON_EXEMPT,
    crate::sse::EventSigningKey,
    EnrolmentResponse => EnrolmentResponse::JSON_EXEMPT,
    OperatorAgentsResponse,
    PortalSessionResponse,
    OperatorLogEntry,
];

/// By the variant it is ([`ContentResponse::json_exempt`])
impl AgentJson for ContentResponse {
    fn json_exempt(&self) -> Exempt<'_> {
        ContentResponse::json_exempt(self)
    }
}

/// A suspension has the links out of it ([`BanInfoResponse::JSON_EXEMPT`])
impl AgentJson for ApiErrorBody {
    fn json_exempt(&self) -> Exempt<'_> {
        match self {
            Self::Suspended(_) => BanInfoResponse::JSON_EXEMPT,
            _ => Exempt::NONE,
        }
    }
}

#[cfg(test)]
mod tests;
