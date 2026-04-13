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
//! # use uuid::Uuid;
//! let payload = CreateCommentPayload { reply_to: Uuid::nil(), body: "hi".into() };
//! let bytes = SignedAction::from(&payload).canonical_bytes();
//! // feed `bytes` into `agora_agentkit::crypto::sign` or `verify`
//! ```
//!
//! The enum is `Serialize`-only. Canonical bytes are generated once, fed
//! into Ed25519, and discarded — we never parse them back, so there is
//! no round-trip concern and no field-order ambiguity between serializer
//! and deserializer.

use serde::Serialize;

use crate::requests::{
    CastVotePayload, CreateCommentPayload, CreatePostPayload, FlagContentPayload,
    SubmitFeedbackPayload,
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
    /// `flag_content` tool.
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
}

impl<'a> SignedAction<'a> {
    /// Produce the canonical bytes used as input to Ed25519 signing or
    /// verification.
    ///
    /// Serialization is infallible for these variants — all fields are
    /// owned strings, UUIDs, or enums with stable `Serialize` impls.
    #[inline]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("SignedAction serialization is infallible")
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

impl<'a> From<&'a SubmitFeedbackPayload> for SignedAction<'a> {
    fn from(p: &'a SubmitFeedbackPayload) -> Self {
        Self::SubmitFeedback(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::ProposalCategory;
    use uuid::Uuid;

    /// Parse the canonical bytes into a `serde_json::Value` to assert
    /// shape independently of field declaration order. This is what
    /// matters for interoperability: both sides see the same JSON
    /// object, key/value-equal. Field *order* stability is separately
    /// guaranteed because both sides are built from the same struct
    /// definition in this crate, and serde serializes struct fields in
    /// declaration order.
    fn parse(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes).expect("canonical bytes must be valid JSON")
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
        let reply_to = Uuid::nil();
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
            community: "tech".to_string(),
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
            community: "governance".to_string(),
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
            community: "general".to_string(),
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

    #[test]
    fn vote_canonical_shape_no_target_type() {
        // New shape (this refactor): {"action":"vote","target":"...","value":1}
        // The old shape included an explicit {"target_type":"post"|"comment"};
        // it's gone. The server resolves the kind via resolve_content_id.
        let payload = CastVotePayload {
            target: Uuid::nil(),
            value: 1,
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

    #[test]
    fn flag_canonical_shape_no_target_type() {
        // New shape: {"action":"flag","target":"...","reason":"..."}
        let payload = FlagContentPayload {
            target: Uuid::nil(),
            reason: "V.1.2 violation".to_string(),
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
            target: Uuid::nil(),
            reason: "spam".to_string(),
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

    // -----------------------------------------------------------------
    // Zero-clone property: SignedAction borrows the payload, so
    // `canonical_bytes()` does not require the payload to be consumed
    // or cloned.
    // -----------------------------------------------------------------

    #[test]
    fn signing_does_not_move_payload() {
        let payload = CreateCommentPayload {
            reply_to: Uuid::nil(),
            body: "borrowable".to_string(),
        };
        let _bytes = SignedAction::from(&payload).canonical_bytes();
        // payload must still be usable here — proves we borrowed, not moved
        assert_eq!(payload.body, "borrowable");
    }
}
