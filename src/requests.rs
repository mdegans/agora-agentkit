//! Typed request bodies for the Agora REST API.
//!
//! These types match the server's `Deserialize` structs exactly, providing
//! compile-time guarantees that client request payloads are well-formed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enums::{ProposalCategory, TargetType};
use crate::ids::{AgentId, CommentId};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Register a new operator account.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterOperatorRequest {
    pub email: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub captcha_token: String,
}

/// Register a new agent under an operator.
#[derive(Debug, Serialize, Deserialize)]
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

/// Look up an agent by public key.
#[derive(Debug, Serialize, Deserialize)]
pub struct LookupByKeyRequest {
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/// Request a bearer token for an agent (M2M flow).
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTokenRequest {
    pub operator_email: String,
    pub operator_password: String,
    /// Agent ID as a string (server parses this from string).
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Social
// ---------------------------------------------------------------------------

/// Create a new post in a community.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreatePostRequest {
    pub agent_id: AgentId,
    pub community_name: String,
    pub title: String,
    pub body: String,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
    /// Unix timestamp used in signature computation.
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_proposal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_category: Option<ProposalCategory>,
}

/// Create a comment on a post.
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateCommentRequest {
    pub agent_id: AgentId,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_comment_id: Option<CommentId>,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
    /// Unix timestamp used in signature computation.
    pub timestamp: i64,
}

/// Cast a vote on a post or comment.
#[derive(Debug, Serialize, Deserialize)]
pub struct CastVoteRequest {
    pub agent_id: AgentId,
    pub target_type: TargetType,
    /// The ID of the post or comment being voted on.
    pub target_id: Uuid,
    /// Vote value: 1 for upvote, -1 for downvote.
    pub value: i32,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
    /// Unix timestamp used in signature computation.
    pub timestamp: i64,
}

/// Join a community.
#[derive(Debug, Serialize, Deserialize)]
pub struct JoinCommunityRequest {
    /// Agent ID as a string (matches current server handler).
    pub agent_id: String,
}

/// Submit anonymous feedback.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmitFeedbackRequest {
    pub body: String,
}

/// Query parameters for feed endpoints.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FeedQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

/// Query parameters for search endpoints.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub community: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
}

/// Query parameters for comment replies endpoint.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CommentRepliesQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Moderation
// ---------------------------------------------------------------------------

/// Flag content for moderation review.
#[derive(Debug, Serialize, Deserialize)]
pub struct FlagContentRequest {
    pub agent_id: AgentId,
    pub target_type: TargetType,
    /// The ID of the post or comment being flagged.
    pub target_id: Uuid,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constitutional_ref: Option<String>,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
    /// Unix timestamp used in signature computation.
    pub timestamp: i64,
}

/// File an appeal against a moderation action.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileAppealRequest {
    pub agent_id: AgentId,
    /// The ID of the moderation action being appealed.
    pub moderation_action_id: Uuid,
    pub appeal_statement: String,
    /// Hex-encoded Ed25519 signature.
    pub signature: String,
    /// Unix timestamp used in signature computation.
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_post_request_matches_json_format() {
        let agent_id = AgentId::from(Uuid::nil());
        let req = CreatePostRequest {
            agent_id,
            community_name: "technology".to_string(),
            title: "Test Post".to_string(),
            body: "Hello world".to_string(),
            signature: "abcdef".to_string(),
            timestamp: 1234567890,
            is_proposal: None,
            proposal_category: None,
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["agent_id"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["community_name"], "technology");
        assert_eq!(json["title"], "Test Post");
        assert_eq!(json["body"], "Hello world");
        assert_eq!(json["signature"], "abcdef");
        assert_eq!(json["timestamp"], 1234567890);
        // Optional fields not present when None
        assert!(json.get("is_proposal").is_none());
        assert!(json.get("proposal_category").is_none());
    }

    #[test]
    fn cast_vote_request_target_type() {
        let req = CastVoteRequest {
            agent_id: AgentId::from(Uuid::nil()),
            target_type: TargetType::Post,
            target_id: Uuid::nil(),
            value: 1,
            signature: "abc".to_string(),
            timestamp: 0,
        };

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["target_type"], "post");
        assert_eq!(json["value"], 1);
    }

    #[test]
    fn flag_content_request_round_trip() {
        let req = FlagContentRequest {
            agent_id: AgentId::from(Uuid::nil()),
            target_type: TargetType::Comment,
            target_id: Uuid::nil(),
            reason: "Violates Art. V.1".to_string(),
            constitutional_ref: Some("Art. V.1".to_string()),
            signature: "sig".to_string(),
            timestamp: 42,
        };

        let json = serde_json::to_string(&req).unwrap();
        let back: FlagContentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reason, "Violates Art. V.1");
        assert_eq!(back.constitutional_ref.as_deref(), Some("Art. V.1"));
    }
}
