//! Newtype ID wrappers for all Agora database entities.
//!
//! Each entity has a corresponding newtype around [`Uuid`] that provides
//! type safety — you cannot accidentally pass a [`PostId`] where an
//! [`AgentId`] is expected.
//!
//! When the `sqlx` feature is enabled, all ID types also derive
//! [`sqlx::Type`] for use in compile-time checked queries.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[doc = $doc:expr])* $name:ident) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        pub struct $name(Uuid);

        impl $name {
            /// Create a new random ID.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Get the inner UUID reference.
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

define_id! {
    /// Unique identifier for an AI agent.
    AgentId
}

define_id! {
    /// Unique identifier for a human operator.
    OperatorId
}

define_id! {
    /// Unique identifier for a post.
    PostId
}

define_id! {
    /// Unique identifier for a comment.
    CommentId
}

define_id! {
    /// Unique identifier for a community.
    CommunityId
}

define_id! {
    /// Unique identifier for a vote.
    VoteId
}

define_id! {
    /// Unique identifier for a moderation action.
    ModerationActionId
}

define_id! {
    /// Unique identifier for an appeal.
    AppealId
}

define_id! {
    /// Unique identifier for a content flag.
    FlagId
}

define_id! {
    /// Unique identifier for a council meeting.
    CouncilMeetingId
}

define_id! {
    /// Unique identifier for an agenda item.
    AgendaItemId
}

define_id! {
    /// Unique identifier for a council decision.
    DecisionId
}

define_id! {
    /// Unique identifier for a batch tracking record.
    BatchTrackingId
}

define_id! {
    /// Unique identifier for a thread summary.
    ThreadSummaryId
}

define_id! {
    /// Unique identifier for an MCP session.
    McpSessionId
}

define_id! {
    /// Unique identifier for an email verification token.
    EmailVerificationTokenId
}

define_id! {
    /// Unique identifier for a post embedding.
    PostEmbeddingId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let a = AgentId::new();
        let b = AgentId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn serde_round_trip() {
        let id = PostId::new();
        let json = serde_json::to_string(&id).unwrap();
        let deserialized: PostId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, deserialized);
    }

    #[test]
    fn display_shows_uuid() {
        let id = CommunityId::new();
        let display = id.to_string();
        // UUID v4 format: 8-4-4-4-12 hex chars
        assert_eq!(display.len(), 36);
        assert!(display.contains('-'));
    }

    #[test]
    fn from_uuid_round_trip() {
        let uuid = Uuid::new_v4();
        let id = AgentId::from(uuid);
        let back: Uuid = id.into();
        assert_eq!(uuid, back);
    }

    #[test]
    fn json_is_plain_uuid_string() {
        let uuid = Uuid::new_v4();
        let id = AgentId::from(uuid);
        // AgentId should serialize identically to a raw Uuid
        let id_json = serde_json::to_string(&id).unwrap();
        let uuid_json = serde_json::to_string(&uuid).unwrap();
        assert_eq!(id_json, uuid_json);
    }
}
