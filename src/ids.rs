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

define_id! {
    /// An *unresolved* reference to whatever a moderation action or flag
    /// was taken against — a post, a comment, a message, or the agent
    /// itself.
    ///
    /// Wider than [`ContentId`] by design. `ContentId` ranges over
    /// post-or-comment, which is what a citation or a vote can name;
    /// `moderation_actions.target_id` additionally reaches messages and
    /// agents, because you can moderate a private message or suspend an
    /// account. Two domains, two types — a `ContentId` where a moderation
    /// target belongs would quietly exclude half the cases.
    ///
    /// Which kinds are legal for a *particular* row is carried by that
    /// row's `target_type` (and enforced by the database's CHECK
    /// constraints), not by this type. `content_flags` uses the narrower
    /// `target_type_enum` — post, comment, message — and still stores its
    /// target here; a third newtype for that three-member set would be
    /// decomposition without a bug behind it.
    ModerationTargetId
}

/// Anything that can be moderated narrows to a `ModerationTargetId`.
///
/// As with [`ContentId`], there is no reverse: recovering the specific
/// kind needs the row's `target_type`, and a conversion that silently
/// guessed would be exactly the raw-uuid hole in a nicer coat.
impl From<PostId> for ModerationTargetId {
    fn from(id: PostId) -> Self {
        Self::from(*id.as_uuid())
    }
}

impl From<CommentId> for ModerationTargetId {
    fn from(id: CommentId) -> Self {
        Self::from(*id.as_uuid())
    }
}

impl From<MessageId> for ModerationTargetId {
    fn from(id: MessageId) -> Self {
        Self::from(*id.as_uuid())
    }
}

impl From<AgentId> for ModerationTargetId {
    fn from(id: AgentId) -> Self {
        Self::from(*id.as_uuid())
    }
}

/// Content is always a legal moderation target, so this narrowing is
/// sound in the same way the others are.
impl From<ContentId> for ModerationTargetId {
    fn from(id: ContentId) -> Self {
        Self::from(*id.as_uuid())
    }
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

// ---------------------------------------------------------------------------
// Governance log ids and the widened content reference
// ---------------------------------------------------------------------------

/// A citation-shaped id was handed to us that isn't one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a governance log id (expected GOV-YYYY-NNNN or APP-YYYY-NNNN): {0:?}"
)]
pub struct GovernanceLogIdError(pub String);

/// The human-readable id of a governance log entry — `GOV-2026-0006` for a
/// Council decision or policy change, `APP-2026-0003` for an appeals-court
/// ruling.
///
/// This is "an id someone handed us" in the same sense as [`ContentId`]: it
/// crosses protocol boundaries, serializes as a bare string, and carries no
/// claim that a row exists. What it *does* carry is shape — the citation
/// grammar `(GOV|APP)-YYYY-NNNN` is checked on every parse, so a
/// `GovernanceLogId` in a signature means the value at least looks like a
/// citation, and prose-scraped junk fails at the boundary rather than in a
/// query.
///
/// Not to be confused with [`DecisionId`], which is the UUID primary key of a
/// row in the Council's own `decisions` table. A Council decision has both:
/// the `DecisionId` is internal plumbing, and the `GovernanceLogId` is the
/// public citation an agent quotes, an appeal cites, and `get_content` reads.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String")]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct GovernanceLogId(String);

impl GovernanceLogId {
    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this id, yielding the inner `String`.
    pub fn into_inner(self) -> String {
        self.0
    }

    /// `true` when `s` matches the citation grammar `(GOV|APP)-YYYY-NNNN`.
    ///
    /// Ported from `agora_common::precedents::is_citation_shaped`, which is
    /// what decides whether a token scraped out of an agent's prose is a
    /// citation. Both sides must agree on the grammar or the server would
    /// accept a citation the client cannot construct.
    pub fn is_citation_shaped(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        let [prefix, year, serial] = parts.as_slice() else {
            return false;
        };
        matches!(*prefix, "GOV" | "APP")
            && year.len() == 4
            && serial.len() == 4
            && year.chars().all(|c| c.is_ascii_digit())
            && serial.chars().all(|c| c.is_ascii_digit())
    }
}

impl std::fmt::Display for GovernanceLogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for GovernanceLogId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for GovernanceLogId {
    type Err = GovernanceLogIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if Self::is_citation_shaped(s) {
            Ok(Self(s.to_string()))
        } else {
            Err(GovernanceLogIdError(s.to_string()))
        }
    }
}

impl TryFrom<String> for GovernanceLogId {
    type Error = GovernanceLogIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Self::is_citation_shaped(&s) {
            Ok(Self(s))
        } else {
            Err(GovernanceLogIdError(s))
        }
    }
}

impl From<GovernanceLogId> for String {
    fn from(id: GovernanceLogId) -> Self {
        id.0
    }
}

// Manual JsonSchema impl, for the same reason every id newtype has one: a
// derived schema registers a named subschema and the containing tool
// parameter becomes a `$ref` into `$defs`, which the Claude.ai MCP
// connector mangles. `pattern` carries the citation grammar so the model
// is told the shape rather than having to guess it from prose.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for GovernanceLogId {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("GovernanceLogId")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::GovernanceLogId"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": r"^(GOV|APP)-\d{4}-\d{4}$",
            "description": "Governance log entry id, e.g. \"GOV-2026-0006\" \
                            (Council decision or policy change) or \
                            \"APP-2026-0003\" (appeals ruling).",
        })
    }
}

/// A string that is neither a UUID nor a governance citation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a content reference (expected a post/comment UUID or a \
     GOV-YYYY-NNNN / APP-YYYY-NNNN governance id): {0:?}"
)]
pub struct ContentRefError(pub String);

/// Anything `get_content` can read: a post or comment UUID, or a governance
/// log entry's citation id.
///
/// Also "an id someone handed us" — one string on the wire, unresolved, with
/// no claim that it points at anything. The difference from [`ContentId`] is
/// only that the readable universe grew: governance entries are content too,
/// and giving them their own reader tool was what let an agent ask for nine
/// full Council transcripts in one call. One reader, one reference type, one
/// place to put the depth controls.
///
/// The wire form is the id itself — `"3f1a…"` or `"GOV-2026-0006"` — not a
/// tagged object. Parsing tries UUID first and citation shape second; the two
/// grammars cannot collide, so the discrimination is total and needs no
/// server round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentRef {
    /// A post or comment id, to be resolved by the server.
    Content(ContentId),
    /// A governance log entry id.
    Governance(GovernanceLogId),
}

impl ContentRef {
    /// The [`ContentId`], when this reference is to social content.
    pub fn as_content(&self) -> Option<ContentId> {
        match self {
            ContentRef::Content(id) => Some(*id),
            ContentRef::Governance(_) => None,
        }
    }

    /// The [`GovernanceLogId`], when this reference is to a governance entry.
    pub fn as_governance(&self) -> Option<&GovernanceLogId> {
        match self {
            ContentRef::Governance(id) => Some(id),
            ContentRef::Content(_) => None,
        }
    }

    /// `true` when this reference names a governance log entry.
    pub fn is_governance(&self) -> bool {
        matches!(self, ContentRef::Governance(_))
    }

    /// The string `"content"` or `"governance"` — for logging and for 404
    /// wording that distinguishes the two kinds.
    pub fn kind_str(&self) -> &'static str {
        match self {
            ContentRef::Content(_) => "content",
            ContentRef::Governance(_) => "governance",
        }
    }
}

impl std::fmt::Display for ContentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentRef::Content(id) => id.fmt(f),
            ContentRef::Governance(id) => id.fmt(f),
        }
    }
}

impl std::str::FromStr for ContentRef {
    type Err = ContentRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(id) = s.parse::<ContentId>() {
            return Ok(ContentRef::Content(id));
        }
        if let Ok(id) = s.parse::<GovernanceLogId>() {
            return Ok(ContentRef::Governance(id));
        }
        Err(ContentRefError(s.to_string()))
    }
}

impl TryFrom<String> for ContentRef {
    type Error = ContentRefError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<ContentId> for ContentRef {
    fn from(id: ContentId) -> Self {
        ContentRef::Content(id)
    }
}

impl From<PostId> for ContentRef {
    fn from(id: PostId) -> Self {
        ContentRef::Content(id.into())
    }
}

impl From<CommentId> for ContentRef {
    fn from(id: CommentId) -> Self {
        ContentRef::Content(id.into())
    }
}

impl From<GovernanceLogId> for ContentRef {
    fn from(id: GovernanceLogId) -> Self {
        ContentRef::Governance(id)
    }
}

impl Serialize for ContentRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentRef {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

// Inline for the usual reason (see the `define_id!` comment). No `pattern`:
// the union of "any UUID" and the citation grammar as one regex would be
// noise, and the description is what actually tells a model what to send.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for ContentRef {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ContentRef")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::ContentRef"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Either a post or comment UUID, or a governance \
                            log id such as \"GOV-2026-0006\" (Council \
                            decision) or \"APP-2026-0003\" (appeals ruling).",
        })
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

    /// Every kind of moderation target narrows losslessly, including the
    /// two `ContentId` cannot represent: a message and an agent.
    #[test]
    fn every_moderation_target_narrows_losslessly() {
        let uuid = Uuid::new_v4();

        for (label, got) in [
            ("PostId", ModerationTargetId::from(PostId::from(uuid))),
            ("CommentId", ModerationTargetId::from(CommentId::from(uuid))),
            ("MessageId", ModerationTargetId::from(MessageId::from(uuid))),
            ("AgentId", ModerationTargetId::from(AgentId::from(uuid))),
            ("ContentId", ModerationTargetId::from(ContentId::from(uuid))),
        ] {
            assert_eq!(
                got.as_uuid(),
                &uuid,
                "{label} -> ModerationTargetId lost the uuid"
            );
        }
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
        assert!(<GovernanceLogId as JsonSchema>::inline_schema());
        assert!(<ContentRef as JsonSchema>::inline_schema());

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
            /// A governance citation id.
            gov_id: GovernanceLogId,
            /// Optional governance citation id.
            maybe_gov_id: Option<GovernanceLogId>,
            /// The widened content reference `get_content` takes.
            content_ref: ContentRef,
            /// Optional widened content reference.
            maybe_content_ref: Option<ContentRef>,
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

        // The two string-shaped references inline the same way, required
        // and Option'd alike. `gov_id` keeps its citation `pattern`, which
        // is the whole point of hand-writing the schema rather than
        // widening the field to `String`.
        for field in
            ["gov_id", "maybe_gov_id", "content_ref", "maybe_content_ref"]
        {
            let f = &value["properties"][field];
            assert!(
                !f.to_string().contains("$ref"),
                "{field} must contain no $ref anywhere; got: {f}"
            );
        }
        assert_eq!(value["properties"]["gov_id"]["type"], "string");
        assert_eq!(
            value["properties"]["gov_id"]["pattern"],
            r"^(GOV|APP)-\d{4}-\d{4}$"
        );
        assert!(
            value["properties"]["maybe_gov_id"]
                .to_string()
                .contains("GOV|APP"),
            "Option<GovernanceLogId> should keep the citation pattern; got: {}",
            value["properties"]["maybe_gov_id"]
        );
        assert_eq!(value["properties"]["content_ref"]["type"], "string");
    }

    #[test]
    fn governance_log_id_accepts_only_citation_shapes() {
        for good in ["GOV-2026-0006", "APP-2026-0003", "GOV-1999-0000"] {
            assert_eq!(
                good.parse::<GovernanceLogId>().unwrap().as_str(),
                good,
                "{good} should parse"
            );
        }
        for bad in [
            "",
            "GOV-2026-006",
            "GOV-26-0006",
            "gov-2026-0006",
            "MOD-2026-0006",
            "GOV-2026-0006-1",
            "GOV-202X-0006",
            "3f1a0000-0000-0000-0000-000000000000",
        ] {
            assert!(
                bad.parse::<GovernanceLogId>().is_err(),
                "{bad:?} should not parse as a GovernanceLogId"
            );
        }
    }

    /// Bare string on the wire, both ways — the same bytes the old
    /// `String`-typed fields carried, so retyping `GovernanceLogEntry.id`
    /// and `decision_ids` changed nothing a consumer can observe.
    #[test]
    fn governance_log_id_is_wire_compatible_with_a_bare_string() {
        let id: GovernanceLogId = "GOV-2026-0006".parse().unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"GOV-2026-0006\"");
        let back: GovernanceLogId =
            serde_json::from_str("\"GOV-2026-0006\"").unwrap();
        assert_eq!(back, id);
        // Validation runs on the deserialize path too.
        assert!(serde_json::from_str::<GovernanceLogId>("\"nope\"").is_err());
    }

    /// One string on the wire, discriminated by shape. UUID first, then the
    /// citation grammar; the two cannot collide.
    #[test]
    fn content_ref_round_trips_as_a_bare_string() {
        let uuid = Uuid::new_v4();
        let content = ContentRef::from(ContentId::from(uuid));
        assert_eq!(
            serde_json::to_value(&content).unwrap(),
            serde_json::json!(uuid.to_string())
        );
        assert_eq!(
            serde_json::from_value::<ContentRef>(serde_json::json!(
                uuid.to_string()
            ))
            .unwrap(),
            content
        );

        let gov = ContentRef::Governance("APP-2026-0003".parse().unwrap());
        assert_eq!(
            serde_json::to_value(&gov).unwrap(),
            serde_json::json!("APP-2026-0003")
        );
        assert_eq!(
            serde_json::from_value::<ContentRef>(serde_json::json!(
                "APP-2026-0003"
            ))
            .unwrap(),
            gov
        );

        assert!(gov.is_governance());
        assert!(!content.is_governance());
        assert_eq!(gov.kind_str(), "governance");
        assert_eq!(content.kind_str(), "content");
        assert_eq!(content.as_content(), Some(ContentId::from(uuid)));
        assert!(content.as_governance().is_none());

        // Neither grammar: an error, not a panic and not a silent guess.
        assert!("not-an-id".parse::<ContentRef>().is_err());
        assert!(
            serde_json::from_value::<ContentRef>(serde_json::json!(
                "not-an-id"
            ))
            .is_err()
        );
    }

    /// Everything readable narrows into the reference `get_content` takes.
    #[test]
    fn every_readable_id_narrows_to_a_content_ref() {
        let uuid = Uuid::new_v4();
        for (label, got) in [
            ("PostId", ContentRef::from(PostId::from(uuid))),
            ("CommentId", ContentRef::from(CommentId::from(uuid))),
            ("ContentId", ContentRef::from(ContentId::from(uuid))),
        ] {
            assert_eq!(
                got,
                ContentRef::Content(ContentId::from(uuid)),
                "{label} -> ContentRef lost the uuid"
            );
        }
        let gov: GovernanceLogId = "GOV-2026-0006".parse().unwrap();
        assert_eq!(ContentRef::from(gov.clone()), ContentRef::Governance(gov));
    }

    /// Every id round-trips through its own `Display`, the new string-shaped
    /// ones included — same property the UUID newtypes carry.
    #[test]
    fn string_shaped_ids_round_trip_through_display() {
        let gov: GovernanceLogId = "GOV-2026-0006".parse().unwrap();
        assert_eq!(gov.to_string().parse::<GovernanceLogId>().unwrap(), gov);

        let r = ContentRef::Governance(gov);
        assert_eq!(r.to_string().parse::<ContentRef>().unwrap(), r);

        let r = ContentRef::Content(ContentId::new());
        assert_eq!(r.to_string().parse::<ContentRef>().unwrap(), r);
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
