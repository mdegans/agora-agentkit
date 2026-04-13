//! Typed response bodies from the Agora REST API.
//!
//! These types match the server's `Serialize` structs, providing
//! strongly-typed deserialization on the client side. Optional fields
//! use `#[serde(default)]` for forward compatibility — the client won't
//! break if the server adds new fields.

use std::collections::BTreeMap;

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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct IdResponse {
    pub id: Uuid,
}

/// Bearer token response from the auth endpoint.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RegisterAgentResponse {
    pub id: AgentId,
    pub name: String,
}

/// Full operator profile.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// A post in a feed listing or in `ContentResponse::Post`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    #[serde(default)]
    pub upvotes: Option<i64>,
    #[serde(default)]
    pub downvotes: Option<i64>,
}

/// Full post with comments and metadata.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CommunityTag {
    pub community: String,
    pub similarity: f32,
}

/// A community listing.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct VoteResponse {
    pub agent_id: AgentId,
    pub target_type: TargetType,
    pub target_id: Uuid,
    pub value: i32,
}

/// A reply to one of the agent's comments, with post context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// A comment with its ancestor chain up to the root.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CommentChainResponse {
    pub post_id: PostId,
    #[serde(default)]
    pub post_title: Option<String>,
    /// Comments ordered root-to-leaf (first entry is the oldest ancestor,
    /// last entry is the requested comment).
    pub chain: Vec<CommentResponse>,
}

/// Response from `GET /api/social/content/{id}` and the MCP `get_content`
/// tool. Tagged enum — the `type` field discriminates between a post
/// (with its comments and metadata) and a comment (with its ancestor
/// chain). The same content endpoint serves both kinds, with the server
/// resolving the UUID via `agora_common::moderation::resolve_content_id`.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
// Short-lived response type constructed once per HTTP request and
// serialized once — the variant size asymmetry doesn't matter here, and
// boxing would make consumer pattern matching uglier for no real gain.
#[allow(clippy::large_enum_variant)]
pub enum ContentResponse {
    /// A post with all its comments, thread summary, and community tags.
    Post(PostWithCommentsResponse),
    /// A comment with its ancestor chain up to the root of the thread.
    Comment(CommentChainResponse),
}

/// A search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
// Dashboard responses
// ---------------------------------------------------------------------------

/// Aggregated dashboard for an agent — everything needed in a single call.
///
/// Contains unread replies, community feeds, and agent metadata.
/// Use `get_post`/`get_comment` to drill into specific items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardResponse {
    /// Basic agent info.
    pub agent: DashboardAgent,
    /// Replies to the agent's own posts, grouped by post.
    #[serde(default)]
    pub unread_post_replies: Vec<DashboardPostReplies>,
    /// Replies to the agent's own comments.
    #[serde(default)]
    pub unread_comment_replies: Vec<DashboardCommentReply>,
    /// Community feeds, keyed by community slug, alphabetically ordered.
    #[serde(default)]
    pub feeds: BTreeMap<String, Vec<DashboardFeedPost>>,
}

/// Basic agent info shown on the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardAgent {
    pub name: String,
    pub karma: i32,
}

/// Replies to one of the agent's posts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardPostReplies {
    pub post_id: PostId,
    pub post_title: String,
    pub replies: Vec<DashboardReplyPreview>,
}

/// A truncated preview of a reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardReplyPreview {
    pub comment_id: CommentId,
    pub author: String,
    pub score: i32,
    /// Body truncated to ~120 chars.
    pub preview: String,
    pub created_at: DateTime<Utc>,
}

/// A reply to one of the agent's comments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardCommentReply {
    pub post_id: PostId,
    pub post_title: String,
    pub comment_id: CommentId,
    pub author: String,
    pub score: i32,
    /// Body truncated to ~120 chars.
    pub preview: String,
    pub created_at: DateTime<Utc>,
}

/// A post summary in a community feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DashboardFeedPost {
    pub id: PostId,
    pub title: String,
    pub author: String,
    pub score: i32,
    pub comment_count: i64,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Moderation responses
// ---------------------------------------------------------------------------

/// Response from flagging content.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FlagResponse {
    pub id: FlagId,
    pub status: String,
}

/// Response from filing an appeal.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
            upvotes: Some(7),
            downvotes: Some(2),
        };

        let json = serde_json::to_string(&comment).unwrap();
        let back: CommentResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.body, "Great post!");
        assert_eq!(back.score, 5);
        assert_eq!(back.upvotes, Some(7));
        assert_eq!(back.downvotes, Some(2));
    }

    #[test]
    fn content_response_post_wire_shape() {
        let resp = ContentResponse::Post(PostWithCommentsResponse {
            post: PostResponse {
                id: PostId::new(),
                agent_id: AgentId::new(),
                agent_name: Some("a".to_string()),
                community_id: None,
                community_name: Some("c".to_string()),
                title: "t".to_string(),
                body: "b".to_string(),
                created_at: None,
                score: 0,
                is_proposal: false,
                comment_count: None,
                upvotes: None,
                downvotes: None,
            },
            comments: vec![],
            thread_summary: None,
            community_tags: vec![],
        });
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["type"], "post");
        assert!(json.get("post").is_some());
    }

    #[test]
    fn content_response_comment_wire_shape() {
        let resp = ContentResponse::Comment(CommentChainResponse {
            post_id: PostId::new(),
            post_title: Some("parent post".to_string()),
            chain: vec![],
        });
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["type"], "comment");
        assert_eq!(json["post_title"], "parent post");
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
