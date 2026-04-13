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

        // Manual JsonSchema impl: emit an inline `{type:"string", format:"uuid"}`
        // schema rather than a `$ref` into `$defs`. The derive path (even with
        // `schemars(transparent)`) registers the newtype as a named subschema
        // because the struct-level doc comment defeats the fully-default
        // transparency delegation. The Claude.ai MCP connector drops parameter
        // values whose schema is a `$ref`, so ID params must be inlined.
        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn schema_id() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(concat!(module_path!(), "::", stringify!($name)))
            }

            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "format": "uuid",
                })
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

    // Regression: the Claude.ai MCP connector drops parameter values whose
    // schema is a `$ref` into `$defs`. ID newtypes must inline their schema
    // so that tool parameters using them don't appear as `$ref` nodes in the
    // containing struct's schema. See bug report 2026-04-12.
    #[cfg(feature = "schemars")]
    #[test]
    fn id_json_schema_is_inlined() {
        use schemars::JsonSchema;

        assert!(
            <PostId as JsonSchema>::inline_schema(),
            "PostId::inline_schema() must return true to avoid $ref in containing schemas"
        );
        assert!(<AgentId as JsonSchema>::inline_schema());
        assert!(<CommentId as JsonSchema>::inline_schema());
        assert!(<CommunityId as JsonSchema>::inline_schema());

        // Generate a schema for a struct containing a PostId field and assert
        // the field's schema is inlined as `type: string, format: uuid`
        // rather than a `$ref`.
        #[derive(schemars::JsonSchema)]
        #[allow(dead_code)]
        struct Container {
            /// The post ID to retrieve.
            post_id: PostId,
            /// Optional agent ID.
            agent_id: Option<AgentId>,
        }

        let schema = schemars::schema_for!(Container);
        let value = serde_json::to_value(&schema).unwrap();

        // No $defs should be created at all — every ID is inline.
        assert!(
            value.get("$defs").is_none(),
            "no $defs should be emitted for ID-only container; got schema: {value}"
        );

        // post_id field should be inline: {type: "string", format: "uuid"}
        let post_id = &value["properties"]["post_id"];
        assert!(
            post_id.get("$ref").is_none(),
            "post_id must not be a $ref; got: {post_id}"
        );
        assert_eq!(post_id["type"], "string");
        assert_eq!(post_id["format"], "uuid");

        // agent_id (Option<AgentId>) should collapse to the JSON Schema union
        // form: {type: ["string","null"], format: "uuid"}. Either that or an
        // anyOf with inline variants is acceptable — the critical property is
        // that no $ref appears anywhere in the field's schema.
        let agent_id = &value["properties"]["agent_id"];
        assert!(
            agent_id.get("$ref").is_none(),
            "agent_id must not be a $ref; got: {agent_id}"
        );
        let agent_id_str = agent_id.to_string();
        assert!(
            !agent_id_str.contains("$ref"),
            "agent_id schema must contain no $ref anywhere; got: {agent_id}"
        );
        assert!(
            agent_id_str.contains("\"format\":\"uuid\""),
            "agent_id should still carry format=uuid; got: {agent_id}"
        );
    }
}
