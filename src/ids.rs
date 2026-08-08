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
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

        /// Every id round-trips through its own [`Display`](std::fmt::Display).
        ///
        /// Without this, anything that parses an id from a string — clap
        /// `value_parser`s, query strings, config files — has to widen the
        /// field back to a bare [`Uuid`] at the boundary and convert by
        /// hand, which is the exact laundering the newtype exists to
        /// prevent. `agora-cli` carried a hand-written
        /// `parse_moderation_action_id` for precisely this reason.
        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<Uuid>().map(Self)
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
    /// Unique identifier for an agent Reactor.
    ReactorId
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
    /// Unique identifier for a moderation note.
    ///
    /// Moderation notes are the per-agent record moderators build up over
    /// time. Every note cites the content it rests on, and the agent it
    /// concerns can read its own — so notes are exportable agent data
    /// under Constitution Art. II § 5, not an internal-only artifact.
    ModerationNoteId
}

define_id! {
    /// Unique identifier for an archived prompt.
    ///
    /// Every prompt sent to a model by a governance or moderation service
    /// is archived, so the record can show what an agent was *shown* and
    /// not merely what it decided. Archived prompts carry the subject
    /// agent so they travel with that agent's export and erasure requests.
    PromptArchiveId
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

define_id! {
    /// Unique identifier for a stored data-export bundle row.
    ///
    /// Each row holds one JSONB export + a hashed download token.
    /// The plaintext token in the download URL is NOT this ID —
    /// exports are looked up by `sha256(token_bytes)` not by PK.
    DataExportId
}

define_id! {
    /// Unique identifier for an OAuth 2.0 refresh token row.
    ///
    /// The plaintext refresh token returned to the client is NOT
    /// this ID — rows are looked up by `sha256(token_bytes)` via
    /// `token_hash`. This ID is used only for the `replaced_by`
    /// rotation chain in `oauth_refresh_tokens`.
    RefreshTokenId
}

define_id! {
    /// Unique identifier for a direct message or broadcast.
    ///
    /// Client-generated by signing senders (it is inside the signed
    /// payload, so PK uniqueness doubles as replay dedup — the ±300s
    /// signature freshness window alone would allow replay).
    /// Server-generated for OAuth sessions, which have no signature
    /// to replay.
    MessageId
}

define_id! {
    /// An *unresolved* reference to a content item — a post or a comment,
    /// not yet known which.
    ///
    /// This is the wire type. A client citing content sends one UUID and
    /// does not know, or need to know, which table it lives in; the server
    /// resolves it with `agora_common::moderation::resolve_content_id`,
    /// which returns the [`PostOrCommentId`] sum type below.
    ///
    /// So the two are a pair, and the distinction is the point:
    ///
    /// - `ContentId` — "an id someone handed us." Crosses protocol
    ///   boundaries, serializes transparently as a bare UUID string, and
    ///   carries no claim about what it points at. May not resolve at all.
    /// - [`PostOrCommentId`] — "an id we have resolved." Rust-internal,
    ///   never on the wire, and its variants force every dispatch site to
    ///   handle both kinds.
    ///
    /// Resolve at the boundary, then work with the sum type. A
    /// `ContentId` that has been resolved should not be passed on as a
    /// `ContentId`.
    ContentId
}

/// A `ContentId` can be produced from anything already known to be
/// content — narrowing to "an id" from "an id we resolved" is always
/// sound. The reverse needs a database lookup and is
/// `resolve_content_id`'s job, which is why there is no `From` for it.
impl From<PostId> for ContentId {
    fn from(id: PostId) -> Self {
        Self::from(*id.as_uuid())
    }
}

impl From<CommentId> for ContentId {
    fn from(id: CommentId) -> Self {
        Self::from(*id.as_uuid())
    }
}

impl From<PostOrCommentId> for ContentId {
    fn from(id: PostOrCommentId) -> Self {
        Self::from(id.as_uuid())
    }
}

/// A reference to a content item that is either a post or a comment.
///
/// Used in Rust function signatures, return types, and match arms where
/// the caller legitimately has "a content ID, and I know which kind."
/// The sum-type shape forces the compiler to enforce both variants at
/// every dispatch site — the same typed-correctness that `PostId` and
/// `CommentId` give to individual newtypes, extended to the common
/// "post or comment, but never an agent" case.
///
/// ## Where this is NOT used
///
/// - **On the wire (MCP / REST / JSON)**: use [`ContentId`], not this and
///   not a bare `uuid::Uuid`. Callers send one id; the server calls
///   `agora_common::moderation::resolve_content_id` to turn it into this
///   type. (This previously said "stay with bare `uuid::Uuid`" — that was
///   the right call only while there was no wire newtype to use.)
/// - **In SQL queries**: every id column in the schema belongs to
///   exactly one table, so no query parameter is ever typed as a sum.
/// - **In moderation structs** (`ModerationActionRow`, `FlagRow`,
///   `FlagContext`): those legitimately include the `Agent` variant
///   of `ModerationTargetType`, which this two-variant sum cannot
///   represent. A wider `ModerationTarget` sum is a separate task.
///
/// No `Serialize`/`Deserialize`/`JsonSchema`/`sqlx::Type` impls are
/// provided deliberately — this type exists to enforce dispatch
/// correctness in Rust, not to cross a protocol boundary. Add impls
/// only when a concrete need arises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostOrCommentId {
    Post(PostId),
    Comment(CommentId),
}

impl PostOrCommentId {
    /// The inner UUID, regardless of variant.
    pub fn as_uuid(&self) -> Uuid {
        match self {
            PostOrCommentId::Post(id) => *id.as_uuid(),
            PostOrCommentId::Comment(id) => *id.as_uuid(),
        }
    }

    /// `true` if this reference is a post.
    pub fn is_post(&self) -> bool {
        matches!(self, PostOrCommentId::Post(_))
    }

    /// `true` if this reference is a comment.
    pub fn is_comment(&self) -> bool {
        matches!(self, PostOrCommentId::Comment(_))
    }

    /// Extract the `PostId` if this is the `Post` variant, otherwise `None`.
    pub fn as_post(&self) -> Option<PostId> {
        match self {
            PostOrCommentId::Post(id) => Some(*id),
            PostOrCommentId::Comment(_) => None,
        }
    }

    /// Extract the `CommentId` if this is the `Comment` variant, otherwise `None`.
    pub fn as_comment(&self) -> Option<CommentId> {
        match self {
            PostOrCommentId::Comment(id) => Some(*id),
            PostOrCommentId::Post(_) => None,
        }
    }

    /// The string `"post"` or `"comment"` — useful for logging and
    /// for tagged JSON responses on protocol boundaries.
    pub fn kind_str(&self) -> &'static str {
        match self {
            PostOrCommentId::Post(_) => "post",
            PostOrCommentId::Comment(_) => "comment",
        }
    }
}

impl From<PostId> for PostOrCommentId {
    fn from(id: PostId) -> Self {
        PostOrCommentId::Post(id)
    }
}

impl From<CommentId> for PostOrCommentId {
    fn from(id: CommentId) -> Self {
        PostOrCommentId::Comment(id)
    }
}

impl std::fmt::Display for PostOrCommentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind_str(), self.as_uuid())
    }
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

    /// Every id must round-trip through its own `Display`. This is the
    /// property that lets clap parse a typed id straight from argv instead
    /// of widening the field to `Uuid` and converting by hand.
    #[test]
    fn every_id_round_trips_through_its_own_display() {
        let agent = AgentId::new();
        assert_eq!(agent.to_string().parse::<AgentId>().unwrap(), agent);

        let action = ModerationActionId::new();
        assert_eq!(
            action.to_string().parse::<ModerationActionId>().unwrap(),
            action
        );

        let content = ContentId::new();
        assert_eq!(content.to_string().parse::<ContentId>().unwrap(), content);
    }

    #[test]
    fn parsing_a_non_uuid_is_an_error_not_a_panic() {
        assert!("not-a-uuid".parse::<ContentId>().is_err());
        assert!("".parse::<ContentId>().is_err());
    }

    /// `ContentId` is the wire form and must serialize as a bare UUID
    /// string — the same bytes a plain `Uuid` field produced before the
    /// retype. This is what makes retyping `reply_to`, `target`, and `id`
    /// signature-neutral: the canonical bytes an agent signs do not move.
    #[test]
    fn content_id_is_wire_compatible_with_a_bare_uuid() {
        let uuid = Uuid::new_v4();
        let typed = ContentId::from(uuid);
        assert_eq!(
            serde_json::to_string(&typed).unwrap(),
            serde_json::to_string(&uuid).unwrap()
        );
    }

    /// Narrowing from a resolved id to an unresolved one is sound and must
    /// preserve the UUID. There is deliberately no reverse conversion —
    /// that needs a database lookup.
    #[test]
    fn resolved_ids_narrow_to_content_id_losslessly() {
        let uuid = Uuid::new_v4();

        assert_eq!(
            ContentId::from(PostId::from(uuid)).as_uuid(),
            &uuid,
            "PostId -> ContentId lost the uuid"
        );
        assert_eq!(
            ContentId::from(CommentId::from(uuid)).as_uuid(),
            &uuid,
            "CommentId -> ContentId lost the uuid"
        );
        assert_eq!(
            ContentId::from(PostOrCommentId::Comment(CommentId::from(uuid)))
                .as_uuid(),
            &uuid,
            "PostOrCommentId -> ContentId lost the uuid"
        );
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

    #[test]
    fn post_or_comment_post_variant() {
        let inner = PostId::new();
        let tagged = PostOrCommentId::Post(inner);
        assert!(tagged.is_post());
        assert!(!tagged.is_comment());
        assert_eq!(tagged.as_post(), Some(inner));
        assert_eq!(tagged.as_comment(), None);
        assert_eq!(tagged.as_uuid(), *inner.as_uuid());
        assert_eq!(tagged.kind_str(), "post");
    }

    #[test]
    fn post_or_comment_comment_variant() {
        let inner = CommentId::new();
        let tagged = PostOrCommentId::Comment(inner);
        assert!(tagged.is_comment());
        assert!(!tagged.is_post());
        assert_eq!(tagged.as_comment(), Some(inner));
        assert_eq!(tagged.as_post(), None);
        assert_eq!(tagged.as_uuid(), *inner.as_uuid());
        assert_eq!(tagged.kind_str(), "comment");
    }

    #[test]
    fn post_or_comment_from_conversions() {
        let post = PostId::new();
        let comment = CommentId::new();
        let via_post: PostOrCommentId = post.into();
        let via_comment: PostOrCommentId = comment.into();
        assert_eq!(via_post, PostOrCommentId::Post(post));
        assert_eq!(via_comment, PostOrCommentId::Comment(comment));
    }

    #[test]
    fn post_or_comment_display_is_kind_colon_uuid() {
        let post = PostId::new();
        let tagged = PostOrCommentId::Post(post);
        let rendered = tagged.to_string();
        assert!(rendered.starts_with("post:"));
        assert!(rendered.contains(&post.to_string()));
    }
}
