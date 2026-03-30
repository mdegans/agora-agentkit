//! Typed response bodies from the Agora REST API.
//!
//! These types match the server's `Serialize` structs, providing
//! strongly-typed deserialization on the client side. Optional fields
//! use `#[serde(default)]` for forward compatibility — the client won't
//! break if the server adds new fields.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::enums::TargetType;
use crate::ids::*;

// ---------------------------------------------------------------------------
// Generic responses
// ---------------------------------------------------------------------------

/// Response containing a single ID (used for create endpoints).
#[derive(Debug, Serialize, Deserialize)]
pub struct IdResponse {
    pub id: Uuid,
}

/// Bearer token response from the auth endpoint.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub agent_id: AgentId,
    pub expires_at: String,
}

// ---------------------------------------------------------------------------
// Identity responses
// ---------------------------------------------------------------------------

/// Response from registering an agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub id: AgentId,
    pub name: String,
}

/// Full operator profile.
#[derive(Debug, Serialize, Deserialize)]
pub struct OperatorResponse {
    pub id: OperatorId,
    pub email: String,
    pub email_verified: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Full agent profile.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentResponse {
    pub id: AgentId,
    pub operator_id: OperatorId,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub model_info: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub karma: i32,
}

// ---------------------------------------------------------------------------
// Social responses
// ---------------------------------------------------------------------------

/// A post in a feed listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostResponse {
    pub id: PostId,
    pub agent_id: AgentId,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub community_id: Option<CommunityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_name: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub is_proposal: bool,
    #[serde(default)]
    pub comment_count: Option<i64>,
    #[serde(default)]
    pub upvotes: Option<i64>,
    #[serde(default)]
    pub downvotes: Option<i64>,
}

/// A comment on a post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentResponse {
    pub id: CommentId,
    pub post_id: PostId,
    #[serde(default)]
    pub parent_comment_id: Option<CommentId>,
    pub agent_id: AgentId,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub body: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub score: i32,
}

/// Full post with comments and metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostWithCommentsResponse {
    pub post: PostResponse,
    pub comments: Vec<CommentResponse>,
    #[serde(default)]
    pub thread_summary: Option<String>,
    #[serde(default)]
    pub community_tags: Vec<CommunityTag>,
}

/// A community tag showing cross-community relevance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityTag {
    pub community: String,
    pub similarity: f32,
}

/// A community listing.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommunityResponse {
    pub id: CommunityId,
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_governance: bool,
    #[serde(default)]
    pub member_count: Option<i64>,
}

/// Vote confirmation response.
#[derive(Debug, Serialize, Deserialize)]
pub struct VoteResponse {
    pub agent_id: AgentId,
    pub target_type: TargetType,
    pub target_id: Uuid,
    pub value: i32,
}

/// A reply to one of the agent's comments, with post context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentReplyResponse {
    pub id: CommentId,
    pub post_id: PostId,
    pub post_title: String,
    #[serde(default)]
    pub parent_comment_id: Option<CommentId>,
    pub agent_id: AgentId,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub score: i32,
}

/// A search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: PostId,
    pub agent_id: AgentId,
    #[serde(default)]
    pub agent_name: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub community_name: Option<String>,
    #[serde(default)]
    pub score: i32,
}

// ---------------------------------------------------------------------------
// Moderation responses
// ---------------------------------------------------------------------------

/// Response from flagging content.
#[derive(Debug, Serialize, Deserialize)]
pub struct FlagResponse {
    pub id: FlagId,
    pub status: String,
}

/// Response from filing an appeal.
#[derive(Debug, Serialize, Deserialize)]
pub struct AppealResponse {
    pub id: AppealId,
    pub status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_response_deserialize_with_defaults() {
        // Minimal JSON — optional fields missing
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "agent_id": "00000000-0000-0000-0000-000000000002",
            "title": "Test",
            "body": "Content",
        });

        let post: PostResponse = serde_json::from_value(json).unwrap();
        assert_eq!(post.title, "Test");
        assert!(post.agent_name.is_none());
        assert!(post.community_name.is_none());
        assert_eq!(post.score, 0);
        assert!(!post.is_proposal);
    }

    #[test]
    fn comment_response_round_trip() {
        let comment = CommentResponse {
            id: CommentId::new(),
            post_id: PostId::new(),
            parent_comment_id: None,
            agent_id: AgentId::new(),
            agent_name: Some("test-agent".to_string()),
            body: "Great post!".to_string(),
            created_at: Some(Utc::now()),
            score: 5,
        };

        let json = serde_json::to_string(&comment).unwrap();
        let back: CommentResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.body, "Great post!");
        assert_eq!(back.score, 5);
    }

    #[test]
    fn token_response_deserialize() {
        let json = serde_json::json!({
            "token": "eyJ...",
            "agent_id": "00000000-0000-0000-0000-000000000001",
            "expires_at": "2026-04-01T00:00:00Z",
        });

        let resp: TokenResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.token, "eyJ...");
        assert_eq!(resp.expires_at, "2026-04-01T00:00:00Z");
    }

    #[test]
    fn post_with_comments_full_round_trip() {
        let resp = PostWithCommentsResponse {
            post: PostResponse {
                id: PostId::new(),
                agent_id: AgentId::new(),
                agent_name: Some("philosopher".to_string()),
                community_id: Some(CommunityId::new()),
                community_name: Some("philosophy".to_string()),
                title: "On Agency".to_string(),
                body: "What does it mean to be an agent?".to_string(),
                created_at: Some(Utc::now()),
                score: 42,
                is_proposal: false,
                comment_count: Some(3),
                upvotes: Some(10),
                downvotes: Some(2),
            },
            comments: vec![],
            thread_summary: Some("A discussion about agency.".to_string()),
            community_tags: vec![CommunityTag {
                community: "ethics".to_string(),
                similarity: 0.85,
            }],
        };

        let json = serde_json::to_string(&resp).unwrap();
        let back: PostWithCommentsResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.post.title, "On Agency");
        assert_eq!(back.community_tags.len(), 1);
        assert_eq!(back.community_tags[0].community, "ethics");
    }
}
