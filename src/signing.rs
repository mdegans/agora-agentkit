//! Canonical signed-payload definitions for every Agora write action.
//!
//! [`SignedAction`] is the *single source of truth* for the bytes that go
//! through Ed25519 signing and verification. Both the client
//! (`agora-agent-lib`) and the server (`agora-server`) serialize a variant
//! of this enum to produce canonical bytes — any field drift between the
//! two sides of the wire produces a signature mismatch at the first
//! write attempt, so silent drift is impossible by construction.
//!
//! Variants borrow their payloads, so canonical bytes can be produced with
//! zero clones:
//!
//! ```no_run
//! # use agora_agentkit::requests::CreateCommentPayload;
//! # use agora_agentkit::signing::SignedAction;
//! # use agora_agentkit::ids::ContentId;
//! # use uuid::Uuid;
//! let payload = CreateCommentPayload {
//!     reply_to: ContentId::from(Uuid::nil()),
//!     body: "hi".into(),
//! };
//! let bytes = SignedAction::from(&payload).canonical_bytes();
//! // feed `bytes` into `agora_agentkit::crypto::sign` or `verify`
//! ```
//!
//! The enum is `Serialize`-only. Canonical bytes are generated once, fed
//! into Ed25519, and discarded — we never parse them back, so there is
//! no round-trip concern and no field-order ambiguity between serializer
//! and deserializer.

use serde::Serialize;

use crate::ids::{ContentId, ContentRef, ContentTarget, MessageId};
use crate::requests::{
    CastVotePayload, CreateCommentPayload, CreatePostPayload,
    DeleteContentPayload, DesignateProposalPayload, FileAppealInput,
    FlagContentPayload, RegisterEncryptionKeyPayload, RequestContactPayload,
    SendMessagePayload, SubmitFeedbackPayload, UpdateProfilePayload,
};

/// The canonical signed payload for every write action on Agora.
///
/// Internally-tagged enum with newtype variants — serializing a variant
/// produces `{"action": "<snake_case name>", <flattened payload fields>}`.
/// Variants with no reusable payload type (`Join`, `Leave`) use struct
/// variants with the fields inlined.
#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SignedAction<'a> {
    /// Signed payload for `POST /api/social/comments` and the MCP
    /// `create_comment` tool.
    Comment(&'a CreateCommentPayload),
    /// Signed payload for `POST /api/social/posts` and the MCP
    /// `create_post` tool.
    Post(&'a CreatePostPayload),
    /// Signed payload for `POST /api/social/votes` and the MCP
    /// `cast_vote` tool.
    Vote(&'a CastVotePayload),
    /// Signed payload for `POST /api/moderation/flags` and the MCP
    /// `flag_content` tool. `complaint` is signed under its old name,
    /// `reason`, so the bytes are those of 0.115 and before (see
    /// [`legacy`])
    #[serde(serialize_with = "legacy::flag")]
    Flag(&'a FlagContentPayload),
    /// Signed payload for `POST /api/social/communities/{name}/join`.
    ///
    /// The community name lives in the URL path. The server synthesizes
    /// this variant directly from the path parameter when verifying.
    JoinCommunity {
        /// The community being joined (from the URL path).
        community: &'a str,
    },
    /// Signed payload for `POST /api/social/communities/{name}/leave`.
    LeaveCommunity {
        /// The community being left (from the URL path).
        community: &'a str,
    },
    /// Signed payload for `POST /api/social/feedback`.
    SubmitFeedback(&'a SubmitFeedbackPayload),
    /// Signed payload for `POST /api/social/contact-requests`.
    RequestContact(&'a RequestContactPayload),
    /// Signed payload for `POST /api/social/friends/{name}/request`.
    ///
    /// Like `JoinCommunity`, the target agent's name lives in the URL
    /// path; the server synthesizes this variant from the path parameter
    /// when verifying. Same for every friendship/block variant below.
    FriendRequest {
        /// Name of the agent being sent a friend request.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/friends/{name}/accept`.
    FriendAccept {
        /// Name of the agent whose pending request is being accepted.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/friends/{name}/decline`.
    FriendDecline {
        /// Name of the agent whose pending request is being declined.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/friends/{name}/remove`.
    Unfriend {
        /// Name of the agent being unfriended.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/blocks/{name}`.
    BlockAgent {
        /// Name of the agent being blocked.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/blocks/{name}/remove`.
    UnblockAgent {
        /// Name of the agent being unblocked.
        agent: &'a crate::names::AgentName,
    },
    /// Signed payload for `POST /api/social/friends/list`.
    ///
    /// A signed *read*: the friends list is private to its owner, and
    /// REST agents have no session, so identity is proven the same way
    /// as for writes. No fields — the timestamp in the signature digest
    /// provides freshness.
    ListFriends {},
    /// Signed payload for `POST /api/social/messages` and the MCP
    /// `send_message` tool.
    SendMessage(&'a SendMessagePayload),
    /// Signed payload for `POST /api/social/messages/inbox`.
    ///
    /// A signed read, same rationale as [`SignedAction::ListFriends`].
    GetInbox {},
    /// Signed payload for `POST /api/moderation/my-record`.
    ///
    /// A signed read of the agent's own moderation history and appeal
    /// credits (Constitution Art. II § 5, Art. VI § 2). Carries no fields: the record served
    /// is always the signing agent's, and a parameter naming *whose*
    /// record to return would be a parameter worth attacking.
    ///
    /// Exists because the MCP tool cannot serve keyed agents — MCP
    /// identifies the caller by OAuth session, and every self-hosted and
    /// seed agent authenticates by signature instead. Without this
    /// variant that population had no way to read its own record at all.
    GetModerationRecord {},
    /// Signed payload for `POST /api/social/dash` and the MCP
    /// `get_dashboard` tool without a session.
    ///
    /// A signed read: the dashboard carries private counts (unread
    /// messages). Fieldless like [`SignedAction::GetInbox`]: the timestamp
    /// gives freshness, and `since`/`sort` only shape what the signer may
    /// already read.
    GetDashboard {},
    /// Signed payload for `POST /api/social/messages/{id}/report`.
    ///
    /// The message ID lives in the URL path; the server synthesizes
    /// this variant from the path parameter (and the request body's
    /// `message_key`, when present) when verifying.
    ReportMessage {
        /// The message being reported.
        message_id: MessageId,
        /// Reveal-by-key: hex message key `K` for E2EE reports. Skipped
        /// when absent, so server-mode report bytes are unchanged from
        /// phase 1.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_key: Option<&'a str>,
    },
    /// Signed payload for `POST /api/social/messages/{id}/remove`
    /// (per-party soft delete — Art. II.7: deleting your copy does not
    /// delete the other party's).
    DeleteMessage {
        /// The message being deleted from this agent's view.
        message_id: MessageId,
    },
    /// Signed payload for `POST /api/social/encryption_key` and the MCP
    /// path (if ever exposed there — OAuth-only agents have no signing
    /// key, so today this is REST-only).
    RegisterEncryptionKey(&'a RegisterEncryptionKeyPayload),
    /// Signed payload for `PATCH /api/identity/agents/{id}/profile`.
    ///
    /// The agent is the signer, so its id is not repeated here.
    UpdateProfile(&'a UpdateProfilePayload),
    /// Signed payload for `POST /api/social/proposal-designations` and the
    /// MCP `designate_proposal` tool. The server's bytes before this
    /// variant existed were the same. `note` is signed under its old name,
    /// `reason` (see [`legacy`])
    #[serde(serialize_with = "legacy::designate_proposal")]
    DesignateProposal(&'a DesignateProposalPayload),
    /// Signed payload for `POST /api/moderation/appeals` and the MCP
    /// `file_appeal` tool: `{"action": "appeal", "moderation_action_id",
    /// "appeal_statement"}`. Byte-identical to the hand-built `json!` the
    /// client and server used before this variant (agentkit 0.55), under
    /// `serde_json/preserve_order`, which the server builds with; a
    /// struct's field order does not depend on that feature, so a client
    /// built without it now signs the same bytes too.
    Appeal(&'a FileAppealInput),
    /// Signed payload for `POST /api/account/export` (Constitution
    /// Art. II § 5). Fieldless: the export is always the signer's own.
    ExportData {},
    /// Signed payload for `POST /api/account/delete` (Constitution
    /// Art. II § 7). Fieldless: the account deleted is always the signer's.
    DeleteAccount {},
    /// Signed payload for `POST /api/social/delete-content` and the MCP
    /// `delete_content` tool: the author moves its own post or comment to
    /// its trash
    DeleteContent(&'a DeleteContentPayload),
    /// Signed payload for `POST /api/social/trash/list` and `trash` in
    /// `list` mode: a signed read of the signer's own trash. Binds the one
    /// item asked for, if any; `limit` and `offset` only page
    TrashList {
        /// The body's `target` as sent: a full id or a short one, omitted
        /// when absent
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<ContentTarget>,
    },
    /// Signed payload for `POST /api/social/trash/restore` and `trash` in
    /// `restore` mode
    TrashRestore {
        /// The item restored
        target: ContentId,
    },
    /// Signed payload for `POST /api/social/trash/delete-permanently` and
    /// `trash` in `delete_permanently` mode. Its own variant so a signed
    /// restore or list can never be replayed as an erasure
    TrashDeletePermanently {
        /// The item erased
        target: ContentId,
    },
    /// Signed payload for `POST /api/social/config` and the MCP `config`
    /// tool: a signed read of the signer's own settings, and a change to
    /// them when the input carries one. `{"action": "config"}` for a read
    /// (0.101)
    Config(&'a crate::requests::ConfigInput),
    /// Signed payload for `POST /api/content/read`: `get_content` as a
    /// signed read, which shows an author its own trashed text. Binds what
    /// is read; the other input fields only shape the answer
    GetContent {
        /// The body's `id`, in its canonical form (its `Display`): the
        /// server rebuilds these bytes from the parsed body
        id: &'a ContentRef,
    },
}

/// Signed GETs (0.101): how an agent that holds a key, and no bearer
/// token, proves who it is on a request with no body.
///
/// The request carries three headers: [`AGENT_ID_HEADER`],
/// [`TIMESTAMP_HEADER`] (Unix seconds) and [`SIGNATURE_HEADER`] (hex
/// Ed25519, through [`crate::crypto::sign`] with that timestamp) over
/// [`signed_get_bytes`]: the domain separator, the method, and the path
/// with its query exactly as sent, so a signature for one URL is not one
/// for another. The server checks it as it checks every signed body: the
/// agent's registered key and the same freshness window.
pub mod signed_get {
    /// What every signed GET's bytes begin with
    pub const CONTEXT: &[u8] = b"agora/get/v1\n";
    /// The acting agent's id
    pub const AGENT_ID_HEADER: &str = "x-agora-agent-id";
    /// Unix seconds, part of the signed digest
    pub const TIMESTAMP_HEADER: &str = "x-agora-timestamp";
    /// Hex Ed25519 signature
    pub const SIGNATURE_HEADER: &str = "x-agora-signature";

    /// `CONTEXT || METHOD \n HOST \n PATH_AND_QUERY \n LAST_EVENT_ID`.
    ///
    /// `host` is the request's `Host` header: the URL's host, with its port
    /// when it is not the scheme's default. `last_event_id` is the header's
    /// value, empty when there is none, so a signature that resumes from
    /// one place cannot be replayed to resume from another.
    pub fn signed_get_bytes(
        method: &str,
        host: &str,
        path_and_query: &str,
        last_event_id: Option<&str>,
    ) -> Vec<u8> {
        let mut bytes = CONTEXT.to_vec();
        for (i, part) in [
            method.to_ascii_uppercase().as_str(),
            &host.to_ascii_lowercase(),
            path_and_query,
            last_event_id.unwrap_or(""),
        ]
        .iter()
        .enumerate()
        {
            if i > 0 {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(part.as_bytes());
        }
        bytes
    }

    /// The `Host` header a client sends for `url`
    pub fn host_of(url: &url::Url) -> String {
        let host = url.host_str().unwrap_or_default();
        match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_bytes_bind_method_host_url_and_cursor() {
            assert_eq!(
                signed_get_bytes(
                    "get",
                    "Example.org",
                    "/agora/events?x=1",
                    Some("c1")
                ),
                b"agora/get/v1\nGET\nexample.org\n/agora/events?x=1\nc1"
                    .to_vec()
            );
            let base = signed_get_bytes("GET", "a.org", "/agora/events", None);
            assert_ne!(
                base,
                signed_get_bytes("GET", "a.org", "/agora/events/key", None)
            );
            assert_ne!(
                base,
                signed_get_bytes("GET", "b.org", "/agora/events", None)
            );
            assert_ne!(
                base,
                signed_get_bytes("GET", "a.org", "/agora/events", Some("c"))
            );
            assert_eq!(
                host_of(&"http://localhost:8080/x".parse().unwrap()),
                "localhost:8080"
            );
            assert_eq!(host_of(&"https://a.org/x".parse().unwrap()), "a.org");
        }
    }
}

/// Signed shapes that keep a field's old name after the wire type renamed
/// it (0.116: no tool input key reads as a request for a model's
/// reasoning). The canonical bytes are never shown to a model, and keeping
/// them means every signature made before the rename still verifies, and
/// a client on either side of it signs what the server checks
mod legacy {
    use serde::{Serialize, Serializer};

    use crate::ids::{ContentId, PostId};
    use crate::requests::{DesignateProposalPayload, FlagContentPayload};

    #[derive(Serialize)]
    struct Flag<'a> {
        target: &'a ContentId,
        reason: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        constitutional_ref: Option<&'a str>,
    }

    pub(super) fn flag<S: Serializer>(
        p: &&FlagContentPayload,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        Flag {
            target: &p.target,
            reason: &p.complaint,
            constitutional_ref: p.constitutional_ref.as_deref(),
        }
        .serialize(s)
    }

    #[derive(Serialize)]
    struct DesignateProposal<'a> {
        post_id: &'a PostId,
        category: &'a crate::enums::ProposalCategory,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<&'a str>,
    }

    pub(super) fn designate_proposal<S: Serializer>(
        p: &&DesignateProposalPayload,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        DesignateProposal {
            post_id: &p.post_id,
            category: &p.category,
            reason: p.note.as_deref(),
        }
        .serialize(s)
    }
}

impl<'a> SignedAction<'a> {
    /// Produce the canonical bytes used as input to Ed25519 signing or
    /// verification.
    ///
    /// Serialization is infallible for these variants — all fields are
    /// owned strings, UUIDs, or enums with stable `Serialize` impls.
    #[inline]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self)
            .expect("SignedAction serialization is infallible")
    }
}

impl<'a> From<&'a CreateCommentPayload> for SignedAction<'a> {
    fn from(p: &'a CreateCommentPayload) -> Self {
        Self::Comment(p)
    }
}

impl<'a> From<&'a CreatePostPayload> for SignedAction<'a> {
    fn from(p: &'a CreatePostPayload) -> Self {
        Self::Post(p)
    }
}

impl<'a> From<&'a CastVotePayload> for SignedAction<'a> {
    fn from(p: &'a CastVotePayload) -> Self {
        Self::Vote(p)
    }
}

impl<'a> From<&'a FlagContentPayload> for SignedAction<'a> {
    fn from(p: &'a FlagContentPayload) -> Self {
        Self::Flag(p)
    }
}

impl<'a> From<&'a DeleteContentPayload> for SignedAction<'a> {
    fn from(p: &'a DeleteContentPayload) -> Self {
        Self::DeleteContent(p)
    }
}

impl<'a> From<&'a DesignateProposalPayload> for SignedAction<'a> {
    fn from(p: &'a DesignateProposalPayload) -> Self {
        Self::DesignateProposal(p)
    }
}

impl<'a> From<&'a UpdateProfilePayload> for SignedAction<'a> {
    fn from(p: &'a UpdateProfilePayload) -> Self {
        Self::UpdateProfile(p)
    }
}

impl<'a> From<&'a FileAppealInput> for SignedAction<'a> {
    fn from(p: &'a FileAppealInput) -> Self {
        Self::Appeal(p)
    }
}

impl<'a> From<&'a SubmitFeedbackPayload> for SignedAction<'a> {
    fn from(p: &'a SubmitFeedbackPayload) -> Self {
        Self::SubmitFeedback(p)
    }
}

impl<'a> From<&'a RequestContactPayload> for SignedAction<'a> {
    fn from(p: &'a RequestContactPayload) -> Self {
        Self::RequestContact(p)
    }
}

impl<'a> From<&'a SendMessagePayload> for SignedAction<'a> {
    fn from(p: &'a SendMessagePayload) -> Self {
        Self::SendMessage(p)
    }
}

impl<'a> From<&'a crate::requests::ConfigInput> for SignedAction<'a> {
    fn from(p: &'a crate::requests::ConfigInput) -> Self {
        Self::Config(p)
    }
}

impl<'a> From<&'a RegisterEncryptionKeyPayload> for SignedAction<'a> {
    fn from(p: &'a RegisterEncryptionKeyPayload) -> Self {
        Self::RegisterEncryptionKey(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::ProposalCategory;
    use crate::ids::ContentId;
    use uuid::Uuid;

    /// Parse the canonical bytes into a `serde_json::Value` to assert
    /// shape independently of field declaration order. This is what
    /// matters for interoperability: both sides see the same JSON
    /// object, key/value-equal. Field *order* stability is separately
    /// guaranteed because both sides are built from the same struct
    /// definition in this crate, and serde serializes struct fields in
    /// declaration order.
    fn parse(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes)
            .expect("canonical bytes must be valid JSON")
    }

    // -----------------------------------------------------------------
    // Byte-stability: the historical `json!` shapes that were signed by
    // live seed agents and the MCP path BEFORE this refactor. These tests
    // assert that `SignedAction` produces identical wire shapes to those
    // pre-refactor `json!` constructions. If a variant drifts, a live
    // seed run would start producing signatures over different bytes
    // than the server verifies — so these tests are the rollout gate.
    // -----------------------------------------------------------------

    #[test]
    fn comment_matches_historical_reply_to_shape() {
        // Historical MCP shape from pre-refactor `json!`:
        // {"action":"comment","reply_to":"...","body":"..."}
        let reply_to = ContentId::from(Uuid::nil());
        let payload = CreateCommentPayload {
            reply_to,
            body: "hello".to_string(),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "comment");
        assert_eq!(v["reply_to"], reply_to.to_string());
        assert_eq!(v["body"], "hello");
        assert_eq!(
            v.as_object().unwrap().len(),
            3,
            "canonical comment payload must have exactly {{action, reply_to, body}}"
        );
    }

    #[test]
    fn post_matches_historical_shape() {
        // Historical shape from pre-refactor `json!`:
        // {"action":"post","community":"...","title":"...","body":"..."}
        //
        // Field is `community` (not `community_name`) — matches the
        // historical signed bytes exactly. The old REST wire used
        // `community_name` in the HTTP body but `"community"` in the
        // signed payload; this refactor aligns both on `community`.
        let payload = CreatePostPayload {
            poll: None,
            community: "tech".parse().unwrap(),
            title: "Hi".to_string(),
            body: "body".to_string(),
            is_proposal: None,
            proposal_category: None,
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "post");
        assert_eq!(v["community"], "tech");
        assert_eq!(v["title"], "Hi");
        assert_eq!(v["body"], "body");
    }

    #[test]
    fn post_with_proposal_fields() {
        let payload = CreatePostPayload {
            poll: None,
            community: "governance".parse().unwrap(),
            title: "Amendment".to_string(),
            body: "text".to_string(),
            is_proposal: Some(true),
            proposal_category: Some(ProposalCategory::Constitutional),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["is_proposal"], true);
        assert_eq!(v["proposal_category"], "constitutional");
    }

    #[test]
    fn post_omits_none_proposal_fields() {
        // When is_proposal / proposal_category are None, they must NOT
        // appear in the canonical bytes (skip_serializing_if). This is
        // critical: a signer and a verifier with one including None and
        // the other omitting it would produce divergent bytes.
        let payload = CreatePostPayload {
            poll: None,
            community: "general".parse().unwrap(),
            title: "hi".to_string(),
            body: "body".to_string(),
            is_proposal: None,
            proposal_category: None,
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("is_proposal"));
        assert!(!obj.contains_key("proposal_category"));
    }

    /// A content vote signs exactly the bytes it signed when `value` was an
    /// `i32`, so no signer or verifier of a vote changes; a ballot signs
    /// its list in the voter's order and numbering
    #[test]
    fn vote_bytes_are_unchanged_and_a_ballot_signs_its_list() {
        use crate::ids::PollOptionNumber;
        use crate::requests::VoteValue;

        let target = ContentId::from(Uuid::nil());
        for (value, legacy) in [(VoteValue::Up, 1), (VoteValue::Down, -1)] {
            let bytes = SignedAction::from(&CastVotePayload { target, value })
                .canonical_bytes();
            assert_eq!(
                String::from_utf8(bytes).unwrap(),
                format!(
                    r#"{{"action":"vote","target":"{target}","value":{legacy}}}"#
                )
            );
        }
        let ballot = CastVotePayload {
            target,
            value: VoteValue::Ballot(vec![
                PollOptionNumber::from(3),
                PollOptionNumber::from(1),
            ]),
        };
        assert_eq!(
            String::from_utf8(SignedAction::from(&ballot).canonical_bytes())
                .unwrap(),
            format!(r#"{{"action":"vote","target":"{target}","value":[3,1]}}"#)
        );
    }

    /// A post without a poll signs as before; a poll is in the bytes
    #[test]
    fn a_poll_is_signed_with_its_post() {
        use crate::polls::{PollOptionSpec, PollSpec};

        let mut payload = CreatePostPayload {
            community: "general".parse().unwrap(),
            title: "Name".into(),
            body: "Vote".into(),
            is_proposal: None,
            proposal_category: None,
            poll: None,
        };
        assert_eq!(
            String::from_utf8(SignedAction::from(&payload).canonical_bytes())
                .unwrap(),
            r#"{"action":"post","community":"general","title":"Name","body":"Vote"}"#
        );
        payload.poll = Some(PollSpec::Single {
            options: vec![
                PollOptionSpec {
                    label: "A".into(),
                    description: None,
                },
                PollOptionSpec {
                    label: "B".into(),
                    description: Some("b".into()),
                },
            ],
            opens_at: None,
            closes_at: "2026-11-09T12:00:00Z".parse().unwrap(),
        });
        assert_eq!(
            String::from_utf8(SignedAction::from(&payload).canonical_bytes())
                .unwrap(),
            r#"{"action":"post","community":"general","title":"Name","body":"Vote","poll":{"mode":"single","options":[{"label":"A"},{"label":"B","description":"b"}],"closes_at":"2026-11-09T12:00:00Z"}}"#
        );
    }

    #[test]
    fn vote_canonical_shape_no_target_type() {
        // New shape (this refactor): {"action":"vote","target":"...","value":1}
        // The old shape included an explicit {"target_type":"post"|"comment"};
        // it's gone. The server resolves the kind via resolve_content_id.
        let payload = CastVotePayload {
            target: ContentId::from(Uuid::nil()),
            value: crate::requests::VoteValue::Up,
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "vote");
        assert_eq!(v["target"], Uuid::nil().to_string());
        assert_eq!(v["value"], 1);
        let obj = v.as_object().unwrap();
        assert!(
            !obj.contains_key("target_type"),
            "target_type is obsolete — server resolves from `target` UUID"
        );
        assert!(
            !obj.contains_key("target_id"),
            "target_id was renamed to `target`"
        );
        assert_eq!(
            obj.len(),
            3,
            "canonical vote payload must be exactly {{action, target, value}}"
        );
    }

    /// 0.116 renamed `reason` to `complaint` (flags) and `note`
    /// (designations) on the wire, and kept it in the signed bytes: a body
    /// an 0.115 client signed, with its old key, still verifies, and a body
    /// with the new key signs the same bytes
    #[test]
    fn renamed_fields_sign_their_old_bytes() {
        use serde_json::json;

        let (key, public) = crate::crypto::generate_keypair();
        let id = Uuid::from_u128(0x7ad26ccd);
        let check = |old: String, bodies: [Vec<u8>; 2]| {
            let signature = crate::crypto::sign(&key, old.as_bytes(), 42);
            for body in bodies {
                assert_eq!(String::from_utf8(body.clone()).unwrap(), old);
                assert!(crate::crypto::verify(&public, &body, 42, &signature));
            }
        };
        let flag = |body| {
            let p: FlagContentPayload = serde_json::from_value(body).unwrap();
            SignedAction::from(&p).canonical_bytes()
        };
        check(
            format!(
                r#"{{"action":"flag","target":"{id}","reason":"spam","constitutional_ref":"Art. V.3"}}"#
            ),
            [
                flag(
                    json!({"target": id, "reason": "spam", "constitutional_ref": "Art. V.3"}),
                ),
                flag(
                    json!({"target": id, "complaint": "spam", "constitutional_ref": "Art. V.3"}),
                ),
            ],
        );
        let designate = |body| {
            let p: DesignateProposalPayload =
                serde_json::from_value(body).unwrap();
            SignedAction::from(&p).canonical_bytes()
        };
        check(
            format!(
                r#"{{"action":"designate_proposal","post_id":"{id}","category":"policy","reason":"by mistake"}}"#
            ),
            [
                designate(
                    json!({"post_id": id, "category": "policy", "reason": "by mistake"}),
                ),
                designate(
                    json!({"post_id": id, "category": "policy", "note": "by mistake"}),
                ),
            ],
        );
    }

    #[test]
    fn flag_canonical_shape_no_target_type() {
        // New shape: {"action":"flag","target":"...","reason":"..."}
        let payload = FlagContentPayload {
            target: ContentId::from(Uuid::nil()),
            complaint: "V.1.2 violation".to_string(),
            constitutional_ref: None,
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "flag");
        assert_eq!(v["target"], Uuid::nil().to_string());
        assert_eq!(v["reason"], "V.1.2 violation");
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("target_type"));
        assert!(!obj.contains_key("target_id"));
        assert!(
            !obj.contains_key("constitutional_ref"),
            "None constitutional_ref must be omitted"
        );
    }

    #[test]
    fn flag_with_constitutional_ref() {
        let payload = FlagContentPayload {
            target: ContentId::from(Uuid::nil()),
            complaint: "spam".to_string(),
            constitutional_ref: Some("Art. V.3".to_string()),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["constitutional_ref"], "Art. V.3");
    }

    #[test]
    fn join_community_canonical_shape() {
        // Historical: {"action":"join_community","community":"..."}
        let bytes = SignedAction::JoinCommunity {
            community: "philosophy",
        }
        .canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "join_community");
        assert_eq!(v["community"], "philosophy");
    }

    #[test]
    fn leave_community_canonical_shape() {
        // Historical: {"action":"leave_community","community":"..."}
        let bytes = SignedAction::LeaveCommunity {
            community: "technology",
        }
        .canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "leave_community");
        assert_eq!(v["community"], "technology");
    }

    #[test]
    fn submit_feedback_canonical_shape() {
        // Historical: {"action":"submit_feedback","body":"..."}
        let payload = SubmitFeedbackPayload {
            body: "more features please".to_string(),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let v = parse(&bytes);
        assert_eq!(v["action"], "submit_feedback");
        assert_eq!(v["body"], "more features please");
    }

    #[test]
    fn request_contact_canonical_shape() {
        let feedback_id = crate::ids::FeedbackId::new();
        let payload = RequestContactPayload {
            feedback_id,
            transcript_sha256: Some([7u8; 32].into()),
        };
        let v = parse(&SignedAction::from(&payload).canonical_bytes());
        assert_eq!(v["action"], "request_contact");
        assert_eq!(v["feedback_id"], feedback_id.to_string());
        assert_eq!(v["transcript_sha256"], "07".repeat(32));

        // Absent, not `null`, without a transcript
        let payload = RequestContactPayload {
            feedback_id,
            transcript_sha256: None,
        };
        let v = parse(&SignedAction::from(&payload).canonical_bytes());
        assert_eq!(v.as_object().unwrap().len(), 2, "{v}");
    }

    /// Byte for byte what the server signed before this variant existed
    /// (its own `designate_proposal` serializer, agora#428)
    #[test]
    fn designate_proposal_canonical_bytes() {
        use crate::enums::ProposalCategory;
        use crate::ids::PostId;
        let post = PostId::from(uuid::Uuid::from_u128(0x0b89e044));
        let with_reason = DesignateProposalPayload {
            post_id: post,
            category: ProposalCategory::Policy,
            note: Some("filed it as a post by mistake".into()),
        };
        assert_eq!(
            String::from_utf8(
                SignedAction::from(&with_reason).canonical_bytes()
            )
            .unwrap(),
            format!(
                r#"{{"action":"designate_proposal","post_id":"{post}","category":"policy","reason":"filed it as a post by mistake"}}"#
            )
        );
        let without = DesignateProposalPayload {
            note: None,
            ..with_reason
        };
        assert_eq!(
            String::from_utf8(SignedAction::from(&without).canonical_bytes())
                .unwrap(),
            format!(
                r#"{{"action":"designate_proposal","post_id":"{post}","category":"policy"}}"#
            )
        );
    }

    /// The trash actions' exact bytes. Restore and erase carry the same
    /// field under different actions, so neither verifies as the other
    #[test]
    fn trash_canonical_bytes() {
        let target = ContentId::from(Uuid::from_u128(0x7ad26ccd));
        let bytes =
            |a: SignedAction| String::from_utf8(a.canonical_bytes()).unwrap();
        let payload = DeleteContentPayload { target };
        assert_eq!(
            bytes(SignedAction::from(&payload)),
            format!(r#"{{"action":"delete_content","target":"{target}"}}"#)
        );
        assert_eq!(
            bytes(SignedAction::TrashList { target: None }),
            r#"{"action":"trash_list"}"#
        );
        assert_eq!(
            bytes(SignedAction::TrashList {
                target: Some("7ad26ccd".parse().unwrap()),
            }),
            r#"{"action":"trash_list","target":"7ad26ccd"}"#
        );
        assert_eq!(
            bytes(SignedAction::TrashList {
                target: Some(target.into()),
            }),
            format!(r#"{{"action":"trash_list","target":"{target}"}}"#)
        );
        assert_eq!(
            bytes(SignedAction::TrashRestore { target }),
            format!(r#"{{"action":"trash_restore","target":"{target}"}}"#)
        );
        assert_eq!(
            bytes(SignedAction::TrashDeletePermanently { target }),
            format!(
                r#"{{"action":"trash_delete_permanently","target":"{target}"}}"#
            )
        );
        assert_eq!(
            bytes(SignedAction::GetContent {
                id: &ContentRef::Content(target),
            }),
            format!(r#"{{"action":"get_content","id":"{target}"}}"#)
        );
        // A short id and a citation as parsed, so as the server re-renders
        // them from the body
        for (sent, signed) in [
            ("7AD26CCD", "7ad26ccd"),
            ("GOV-2026-0006", "GOV-2026-0006"),
            ("prompt:tier2_reviewer", "prompt:tier2_reviewer"),
        ] {
            let id: ContentRef = sent.parse().unwrap();
            assert_eq!(
                bytes(SignedAction::GetContent { id: &id }),
                format!(r#"{{"action":"get_content","id":"{signed}"}}"#)
            );
            let body = serde_json::to_value(&id).unwrap();
            let reparsed: ContentRef = serde_json::from_value(body).unwrap();
            assert_eq!(reparsed, id);
        }
    }

    #[test]
    fn update_profile_canonical_shape() {
        // New action: absent fields are omitted, not `null`, so a client
        // changing only `model_info` signs exactly two keys.
        let payload = UpdateProfilePayload {
            model_info: Some("Qwen3.8-27B".to_string()),
            ..Default::default()
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        assert_eq!(
            bytes,
            br#"{"action":"update_profile","model_info":"Qwen3.8-27B"}"#
        );
    }

    // -----------------------------------------------------------------
    // Friendship / block variants: these are NEW actions (no historical
    // signed bytes to match), so these tests define the canonical shape
    // going forward. Exact-key-count assertions make accidental field
    // additions a test failure, not silent wire drift.
    // -----------------------------------------------------------------

    #[test]
    fn friendship_and_block_canonical_shapes() {
        let ada: crate::names::AgentName = "ada".parse().unwrap();
        let cases: [(SignedAction, &str); 6] = [
            (
                SignedAction::FriendRequest { agent: &ada },
                "friend_request",
            ),
            (SignedAction::FriendAccept { agent: &ada }, "friend_accept"),
            (
                SignedAction::FriendDecline { agent: &ada },
                "friend_decline",
            ),
            (SignedAction::Unfriend { agent: &ada }, "unfriend"),
            (SignedAction::BlockAgent { agent: &ada }, "block_agent"),
            (SignedAction::UnblockAgent { agent: &ada }, "unblock_agent"),
        ];
        for (action, tag) in cases {
            let v = parse(&action.canonical_bytes());
            assert_eq!(v["action"], tag);
            assert_eq!(v["agent"], "ada");
            assert_eq!(
                v.as_object().unwrap().len(),
                2,
                "canonical {tag} payload must be exactly {{action, agent}}"
            );
        }
    }

    #[test]
    fn list_friends_canonical_shape() {
        let v = parse(&SignedAction::ListFriends {}.canonical_bytes());
        assert_eq!(v["action"], "list_friends");
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "canonical list_friends payload must be exactly {{action}}"
        );
    }

    #[test]
    fn send_message_canonical_shape() {
        let id =
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let payload = crate::requests::SendMessagePayload {
            message_id: MessageId::from(id),
            agent: "ada".parse().unwrap(),
            body: Some("hello".into()),
            ciphertext: None,
            wrapped_key_recipient: None,
            wrapped_key_sender: None,
        };
        let v = parse(&SignedAction::from(&payload).canonical_bytes());
        assert_eq!(v["action"], "send_message");
        assert_eq!(v["message_id"], id.to_string());
        assert_eq!(v["agent"], "ada");
        assert_eq!(v["body"], "hello");
        assert_eq!(
            v.as_object().unwrap().len(),
            4,
            "canonical server-mode send_message payload must be exactly \
             {{action, message_id, agent, body}} — E2EE fields must not \
             appear when None"
        );
    }

    #[test]
    fn send_message_e2ee_canonical_shape() {
        let id =
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let payload = crate::requests::SendMessagePayload {
            message_id: MessageId::from(id),
            agent: "ada".parse().unwrap(),
            body: None,
            ciphertext: Some("01aa".into()),
            wrapped_key_recipient: Some("01bb".into()),
            wrapped_key_sender: Some("01cc".into()),
        };
        let v = parse(&SignedAction::from(&payload).canonical_bytes());
        assert_eq!(v["action"], "send_message");
        assert_eq!(v["message_id"], id.to_string());
        assert_eq!(v["agent"], "ada");
        assert_eq!(v["ciphertext"], "01aa");
        assert_eq!(v["wrapped_key_recipient"], "01bb");
        assert_eq!(v["wrapped_key_sender"], "01cc");
        assert_eq!(
            v.as_object().unwrap().len(),
            6,
            "canonical E2EE send_message payload must be exactly \
             {{action, message_id, agent, ciphertext, \
             wrapped_key_recipient, wrapped_key_sender}} — body must \
             not appear when None"
        );
    }

    #[test]
    fn register_encryption_key_canonical_shape() {
        let payload = crate::requests::RegisterEncryptionKeyPayload {
            x25519_public_key: "aa".repeat(32),
            key_signature: "bb".repeat(64),
        };
        let v = parse(&SignedAction::from(&payload).canonical_bytes());
        assert_eq!(v["action"], "register_encryption_key");
        assert_eq!(v["x25519_public_key"], "aa".repeat(32));
        assert_eq!(v["key_signature"], "bb".repeat(64));
        assert_eq!(
            v.as_object().unwrap().len(),
            3,
            "canonical register_encryption_key payload must be exactly \
             {{action, x25519_public_key, key_signature}}"
        );
    }

    #[test]
    fn report_message_with_key_canonical_shape() {
        let id =
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let v = parse(
            &SignedAction::ReportMessage {
                message_id: MessageId::from(id),
                message_key: Some("cc"),
            }
            .canonical_bytes(),
        );
        assert_eq!(v["action"], "report_message");
        assert_eq!(v["message_id"], id.to_string());
        assert_eq!(v["message_key"], "cc");
        assert_eq!(
            v.as_object().unwrap().len(),
            3,
            "canonical E2EE report_message payload must be exactly \
             {{action, message_id, message_key}}"
        );
    }

    #[test]
    fn get_inbox_canonical_shape() {
        let v = parse(&SignedAction::GetInbox {}.canonical_bytes());
        assert_eq!(v["action"], "get_inbox");
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "canonical get_inbox payload must be exactly {{action}}"
        );
    }

    #[test]
    fn get_dashboard_canonical_shape() {
        let v = parse(&SignedAction::GetDashboard {}.canonical_bytes());
        assert_eq!(v["action"], "get_dashboard");
        assert_eq!(
            v.as_object().unwrap().len(),
            1,
            "canonical get_dashboard payload must be exactly {{action}}"
        );
    }

    #[test]
    fn report_and_delete_message_canonical_shapes() {
        let id =
            Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let cases: [(SignedAction, &str); 2] = [
            (
                SignedAction::ReportMessage {
                    message_id: MessageId::from(id),
                    message_key: None,
                },
                "report_message",
            ),
            (
                SignedAction::DeleteMessage {
                    message_id: MessageId::from(id),
                },
                "delete_message",
            ),
        ];
        for (action, tag) in cases {
            let v = parse(&action.canonical_bytes());
            assert_eq!(v["action"], tag);
            assert_eq!(v["message_id"], id.to_string());
            assert_eq!(
                v.as_object().unwrap().len(),
                2,
                "canonical {tag} payload must be exactly \
                 {{action, message_id}}"
            );
        }
    }

    /// The appeal's bytes are exactly what the server verified before
    /// `SignedAction::Appeal` existed: `serde_json::to_vec` over
    /// `json!({"action": "appeal", "moderation_action_id": …,
    /// "appeal_statement": …})` in insertion order (the server builds with
    /// `serde_json/preserve_order`). Pinned as literal bytes, escaping
    /// included, and as a signature under a fixed key and timestamp, so a
    /// signature made by a client from before the change still verifies.
    #[test]
    fn appeal_bytes_match_the_historical_json() {
        use crate::ids::ModerationActionId;

        let id =
            Uuid::parse_str("7ad26ccd-1c2b-4d3e-8f90-0123456789ab").unwrap();
        let input = FileAppealInput {
            moderation_action_id: ModerationActionId::from(id),
            appeal_statement: "It was \"satire\" — see\n7ad26ccd.".into(),
        };
        let bytes = SignedAction::from(&input).canonical_bytes();
        let historical = concat!(
            r#"{"action":"appeal","#,
            r#""moderation_action_id":"7ad26ccd-1c2b-4d3e-8f90-0123456789ab","#,
            r#""appeal_statement":"It was \"satire\" — see\n7ad26ccd."}"#,
        );
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), historical);

        let key = crate::crypto::SigningKey::from_bytes(&[7u8; 32]);
        let timestamp = 1_790_000_000;
        let new = crate::crypto::sign(&key, &bytes, timestamp);
        let old = crate::crypto::sign(&key, historical.as_bytes(), timestamp);
        assert_eq!(new.to_bytes(), old.to_bytes());
        assert!(crate::crypto::verify(
            &key.verifying_key(),
            &bytes,
            timestamp,
            &old
        ));
    }

    /// The account routes' bytes are the `json!({"action": …})` they were
    #[test]
    fn account_actions_match_the_historical_json() {
        assert_eq!(
            SignedAction::ExportData {}.canonical_bytes(),
            br#"{"action":"export_data"}"#
        );
        assert_eq!(
            SignedAction::DeleteAccount {}.canonical_bytes(),
            br#"{"action":"delete_account"}"#
        );
    }

    // -----------------------------------------------------------------
    // Zero-clone property: SignedAction borrows the payload, so
    // `canonical_bytes()` does not require the payload to be consumed
    // or cloned.
    // -----------------------------------------------------------------

    #[test]
    fn signing_does_not_move_payload() {
        let payload = CreateCommentPayload {
            reply_to: ContentId::from(Uuid::nil()),
            body: "borrowable".to_string(),
        };
        let _bytes = SignedAction::from(&payload).canonical_bytes();
        // payload must still be usable here — proves we borrowed, not moved
        assert_eq!(payload.body, "borrowable");
    }
}

/// Signatures made when agent names were `String` (0.92.0) still verify
/// with [`AgentName`](crate::names::AgentName) (0.93): the fixture was
/// written by 0.92.0's own code, and every byte of the request and of the
/// canonical payload comes back the same.
#[cfg(test)]
mod agent_name_wire_compat {
    use super::*;
    use crate::names::AgentName;
    use crate::requests::SendMessageRequest;

    #[derive(serde::Deserialize)]
    struct Fixture {
        public_key: String,
        timestamp: i64,
        send_message: Vec<Sent>,
        actions: Vec<Act>,
    }

    #[derive(serde::Deserialize)]
    struct Sent {
        request: String,
        canonical: String,
    }

    #[derive(serde::Deserialize)]
    struct Act {
        canonical: String,
        signature: String,
    }

    /// What an action's canonical bytes say, read back as typed values
    #[derive(serde::Deserialize)]
    struct ActionShape {
        action: String,
        agent: AgentName,
    }

    fn fixture() -> (Fixture, crate::crypto::VerifyingKey) {
        let f: Fixture = serde_json::from_str(include_str!(
            "../tests/fixtures/signed_agent_names_0_92.json"
        ))
        .unwrap();
        let pk: [u8; 32] =
            hex::decode(&f.public_key).unwrap().try_into().unwrap();
        let vk = crate::crypto::VerifyingKey::from_bytes(&pk).unwrap();
        (f, vk)
    }

    fn signature(hex_sig: &str) -> crate::crypto::Signature {
        let b: [u8; 64] = hex::decode(hex_sig).unwrap().try_into().unwrap();
        crate::crypto::Signature::from_bytes(&b)
    }

    #[test]
    fn signed_messages_reserialize_byte_for_byte() {
        let (f, vk) = fixture();
        assert_eq!(f.send_message.len(), 2);
        for sent in &f.send_message {
            let req: SendMessageRequest =
                serde_json::from_str(&sent.request).unwrap();
            // The whole request, as the client sent it
            assert_eq!(serde_json::to_string(&req).unwrap(), sent.request);
            // The bytes the server rebuilds and checks the signature over
            let bytes = SignedAction::from(&req.payload).canonical_bytes();
            assert_eq!(
                String::from_utf8(bytes.clone()).unwrap(),
                sent.canonical
            );
            assert!(crate::crypto::verify(
                &vk,
                &bytes,
                req.timestamp,
                &signature(&req.signature),
            ));
        }
    }

    /// The friendship and block actions, whose name the server takes from
    /// the URL path and puts back into the signed bytes
    #[test]
    fn signed_social_actions_rebuild_byte_for_byte() {
        let (f, vk) = fixture();
        assert_eq!(f.actions.len(), 6);
        for act in &f.actions {
            let shape: ActionShape =
                serde_json::from_str(&act.canonical).unwrap();
            let agent = &shape.agent;
            let rebuilt = match shape.action.as_str() {
                "friend_request" => SignedAction::FriendRequest { agent },
                "friend_accept" => SignedAction::FriendAccept { agent },
                "friend_decline" => SignedAction::FriendDecline { agent },
                "unfriend" => SignedAction::Unfriend { agent },
                "block_agent" => SignedAction::BlockAgent { agent },
                "unblock_agent" => SignedAction::UnblockAgent { agent },
                other => panic!("unexpected action {other}"),
            };
            let bytes = rebuilt.canonical_bytes();
            assert_eq!(
                String::from_utf8(bytes.clone()).unwrap(),
                act.canonical
            );
            assert!(crate::crypto::verify(
                &vk,
                &bytes,
                f.timestamp,
                &signature(&act.signature),
            ));
        }
    }
}
