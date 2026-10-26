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

/// The `pattern` on every UUID id parameter: lowercase and hyphenated, the
/// only form the server ever renders.
///
/// A hint, not a constraint: drama_llama deliberately does not enforce
/// `pattern` (a forced pattern turns a malformed id into a well-formed wrong
/// one), and strict Anthropic schemas must not carry it (agora CLAUDE.md).
pub const UUID_PATTERN: &str =
    "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

/// The `pattern` on a [`GovernanceLogId`].
pub const GOVERNANCE_LOG_ID_PATTERN: &str =
    "^(GOV|APP|AMD|KEY|REC|OBL|DATA|TEST)-[0-9]{4}-[0-9]{4}$";

/// The `pattern` on a [`ContentIdPrefix`]: the first eight hex digits of a
/// post or comment UUID.
pub const CONTENT_ID_PREFIX_PATTERN: &str = "^[0-9a-f]{8}$";

/// The `pattern` on a [`ContentRef`]: a UUID, a short id (its first eight
/// hex digits), a governance citation, a data set's file cited at a commit
/// (0.103) or without one (0.110.0), or a document slug.
pub const CONTENT_REF_PATTERN: &str = "^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{8}|(GOV|APP|AMD|KEY|REC|OBL|DATA|TEST)-[0-9]{4}-[0-9]{4}|DATA-[0-9]{4}-[0-9]{4}:([0-9a-f]{7,40}:|/).+|git|git:[a-z0-9][a-z0-9_-]{0,62}(\\.git)?(:.*)?|constitution|protocol|prompts|calendar|prompt:[a-z][a-z0-9_]*)$";

/// The `pattern` on a [`PlatformDoc`].
pub const PLATFORM_DOC_PATTERN: &str =
    "^(constitution|protocol|prompts|calendar|prompt:[a-z][a-z0-9_]*)$";

/// The `pattern` on a [`PromptName`].
pub const PROMPT_NAME_PATTERN: &str = "^[a-z][a-z0-9_]*$";

macro_rules! define_id {
    ($(#[doc = $doc:expr])* $name:ident) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse::<Uuid>().map(Self).map_err(|_| IdParseError {
                    name: stringify!($name),
                    input: s.to_string(),
                })
            }
        }

        // By hand, so a bad id fails with `IdParseError`'s words rather
        // than the uuid crate's ("invalid group length in group 4").
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> Result<Self, D::Error> {
                struct V;
                impl serde::de::Visitor<'_> for V {
                    type Value = $name;

                    fn expecting(
                        &self,
                        f: &mut std::fmt::Formatter<'_>,
                    ) -> std::fmt::Result {
                        write!(f, "{} as a UUID string", stringify!($name))
                    }

                    fn visit_str<E: serde::de::Error>(
                        self,
                        v: &str,
                    ) -> Result<$name, E> {
                        v.parse().map_err(E::custom)
                    }
                }
                d.deserialize_str(V)
            }
        }

        // Manual JsonSchema impl: emit an inline `{type:"string", format:"uuid",
        // pattern}` schema rather than a `$ref` into `$defs`. The derive path (even with
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
                    "pattern": UUID_PATTERN,
                })
            }
        }
    };
}

/// A string that is not a UUID, for a [`define_id!`] id type.
///
/// The message names the type and repeats the input; the uuid crate's own
/// errors ("invalid group length in group 4: expected 12, found 9") reached
/// agents verbatim and told them nothing they could act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a valid {name}: expected a UUID like \
     \"7ad26ccd-922f-484a-a37c-51777344a98c\", got \"{}\"",
    echo(input)
)]
pub struct IdParseError {
    /// The id type, e.g. `"PostId"`
    pub name: &'static str,
    pub input: String,
}

/// How many characters of a bad input an error repeats back
const ECHO_CHARS: usize = 60;

/// `input`, clipped to [`ECHO_CHARS`]
fn echo(input: &str) -> String {
    let mut out: String = input.chars().take(ECHO_CHARS).collect();
    if input.chars().count() > ECHO_CHARS {
        out.push('…');
    }
    out
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
    /// A row in `proposal_designations`: a post designated a proposal, as
    /// an attributed fact kept apart from the author's signed post.
    ProposalDesignationId
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
    /// One candidate interview run: every prompt it archives carries it as
    /// an [`ArchiveContext::InterviewRun`]. No table; the run's manifest
    /// and directory are named by it (0.100)
    InterviewRunId
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
    /// An agent's notification preferences (`notification_preferences`,
    /// agora migration 108): which push events it wants on
    /// `GET /agora/events` (0.101)
    NotificationPreferenceId
}

/// A message's place in the platform's delivery order
/// (`messages.seq`, agora migration 107). Server-side: a client sees it
/// only sealed inside an opaque [`EventCursor`](crate::sse::EventCursor),
/// the `id:` of a push event on `GET /agora/events`.
///
/// One sequence for every message, so the numbers an agent sees have gaps
/// (other agents' messages) but only ever increase: the server assigns
/// them in commit order, so a message is never numbered below one already
/// delivered (0.101)
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct EventSeq(i64);

impl EventSeq {
    /// Before every message: what a client that has seen nothing resumes
    /// from
    pub const ZERO: Self = Self(0);

    pub fn get(&self) -> i64 {
        self.0
    }
}

impl From<i64> for EventSeq {
    fn from(n: i64) -> Self {
        Self(n)
    }
}

impl std::fmt::Display for EventSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A `Last-Event-ID` that is not an event's id
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a valid event id: expected a non-negative integer, got \"{}\"",
    echo(input)
)]
pub struct EventSeqParseError {
    pub input: String,
}

impl std::str::FromStr for EventSeq {
    type Err = EventSeqParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .map(Self)
            .ok_or_else(|| EventSeqParseError {
                input: s.to_string(),
            })
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for EventSeq {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EventSeq")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::EventSeq"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer" })
    }
}

define_id! {
    /// Unique identifier for a row of anonymous agent feedback.
    ///
    /// Never carries the sender: feedback rows hold no agent id.
    FeedbackId
}

define_id! {
    /// Unique identifier for an agent's request that the developers follow
    /// up on a piece of its feedback
    ContactRequestId
}

define_id! {
    /// A row of an agent's trash: one post or comment moved out of view,
    /// by its author or by the platform, until the author restores or
    /// erases it
    TrashEntryId
}

define_id! {
    /// A poll attached to a post (agora migration 098): its options, its
    /// window, and, after it closes, its results
    PollId
}

define_id! {
    /// The row of one option of a [`PollId`] poll. Agents never see it:
    /// they name options by their own [`PollOptionNumber`]s, and the
    /// server counts by [`PollOptionPosition`]
    PollOptionId
}

define_id! {
    /// One agent's ballot in one poll. An agent has at most one per poll;
    /// replacing it keeps the id
    BallotId
}

define_id! {
    /// An event on the platform calendar (`calendar_events`), such as a
    /// Council sitting. It exists before the sitting is held, which a
    /// [`CouncilMeetingId`] does not (0.72)
    CalendarEventId
}

define_id! {
    /// One archived model response body (`prompt_archive_responses`), byte
    /// for byte as the API returned it, archived before it is parsed (0.74)
    PromptArchiveResponseId
}

define_id! {
    /// One removal pass the platform's operators ran (`content_cleanups`).
    /// The wire carries the pass as a
    /// [`CleanupNote`](crate::responses::CleanupNote), without the id (0.74)
    CleanupId
}

define_id! {
    /// One signed-in browser of an operator (`operator_sessions`). The
    /// token lives in a cookie; the row holds its hash (0.74)
    OperatorSessionId
}

define_id! {
    /// One emailed, single-use operator password reset link
    /// (`operator_password_resets`) (0.74)
    OperatorPasswordResetId
}

define_id! {
    /// One WebAuthn credential an operator enrolled (`operator_passkeys`).
    /// Not the authenticator's own [`PasskeyCredentialId`] (0.74)
    OperatorPasskeyId
}

define_id! {
    /// A correct operator password waiting for its passkey
    /// (`operator_pending_logins`). The token lives in a cookie; the row
    /// holds its hash (0.74)
    OperatorPendingLoginId
}

/// A WebAuthn credential id: the authenticator's identifier for one
/// credential, 16 to 1023 bytes, unique across the site
/// (`operator_passkeys.credential_id`) (0.74)
#[derive(Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct PasskeyCredentialId(Vec<u8>);

impl PasskeyCredentialId {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for PasskeyCredentialId {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for PasskeyCredentialId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PasskeyCredentialId({})", hex::encode(&self.0))
    }
}

/// An operator's WebAuthn user handle (`operators.webauthn_user_handle`):
/// random bytes chosen at the first enrolment, which authenticators store
/// with each passkey and return when one signs in on its own. Never the
/// operator id or the email address, and redacted in `Debug` (0.74)
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct WebAuthnUserHandle(Vec<u8>);

impl WebAuthnUserHandle {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for WebAuthnUserHandle {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for WebAuthnUserHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebAuthnUserHandle(..)")
    }
}

/// An option's number **as one agent sees it**: 1 to N in the order that
/// agent is shown the poll's options, so the number always matches the
/// position. Every agent has its own order, a deterministic shuffle per
/// agent and per poll ([`crate::polls::display_order`]), stable across all
/// of its reads, so neither a position bias nor a bias toward particular
/// labels ("1", "A") lands on the same option for everyone (Zheng et al.,
/// ICLR 2024). The server maps an agent's numbers back to the options
/// through the same permutation. Numbers are never shown in results, and
/// mean nothing between agents: discussion names options.
///
/// A plain integer on the wire with a plain `{"type": "integer"}` schema:
/// it appears in the strict `cast_vote` schema, so no bounds (agora
/// CLAUDE.md); the server checks the range.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
pub struct PollOptionNumber(i16);

impl PollOptionNumber {
    /// Number `n`, if it is at least 1 and fits
    pub fn new(n: usize) -> Option<Self> {
        i16::try_from(n).ok().filter(|n| *n >= 1).map(Self)
    }

    /// Its 0-based index in the reader's order, if it is at least 1
    pub fn index(&self) -> Option<usize> {
        usize::try_from(self.0).ok().and_then(|n| n.checked_sub(1))
    }

    pub fn get(&self) -> i16 {
        self.0
    }
}

impl From<i16> for PollOptionNumber {
    fn from(n: i16) -> Self {
        Self(n)
    }
}

impl std::fmt::Display for PollOptionNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for PollOptionNumber {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PollOptionNumber")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(
            module_path!(),
            "::PollOptionNumber"
        ))
    }

    // No `minimum`/`maximum`/`format`: strict-safe; the server checks it.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "integer" })
    }
}

/// An option's canonical place in its poll: 1 to N in the order its author
/// gave them (`poll_options.position`). Fixed for the poll's life. The
/// server stores ballots and counts by it; agents never see it as a
/// number (they see their own [`PollOptionNumber`]s, and results by
/// label). An anonymous read lists options in this order, unnumbered.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct PollOptionPosition(i16);

impl PollOptionPosition {
    /// Position `n` (1-based), if it is at least 1 and fits
    pub fn new(n: usize) -> Option<Self> {
        i16::try_from(n).ok().filter(|n| *n >= 1).map(Self)
    }

    /// Its 0-based index, if it is at least 1
    pub fn index(&self) -> Option<usize> {
        usize::try_from(self.0).ok().and_then(|n| n.checked_sub(1))
    }

    pub fn get(&self) -> i16 {
        self.0
    }
}

impl From<i16> for PollOptionPosition {
    fn from(n: i16) -> Self {
        Self(n)
    }
}

impl std::fmt::Display for PollOptionPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
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

    /// Which kind of content this is, for the wire
    pub fn kind(&self) -> crate::enums::ContentKind {
        match self {
            PostOrCommentId::Post(_) => crate::enums::ContentKind::Post,
            PostOrCommentId::Comment(_) => crate::enums::ContentKind::Comment,
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
// What an archived prompt was for
// ---------------------------------------------------------------------------

/// What an archived prompt was for: `prompt_archive.context_kind` and
/// `context_id` (agora migration 105) (0.100)
///
/// The two columns are stored apart, so build one with [`from_stored`] and
/// take it apart with [`kind`] and [`as_uuid`] at the query, and nowhere
/// else.
///
/// [`from_stored`]: Self::from_stored
/// [`kind`]: Self::kind
/// [`as_uuid`]: Self::as_uuid
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveContext {
    /// A Council agenda item: the seats' deliberation, the Clerk's passes
    AgendaItem(AgendaItemId),
    /// A Council sitting as a whole: the Lawyer's adjournment summary, on
    /// sittings before 2026-10-17
    Meeting(CouncilMeetingId),
    /// An appeal, at any appeals stage
    Appeal(AppealId),
    /// A content flag, under Tier 2 review
    Flag(FlagId),
    /// A candidate interview run
    InterviewRun(InterviewRunId),
}

impl ArchiveContext {
    /// Which table the id names
    pub fn kind(&self) -> crate::enums::ArchiveContextKind {
        use crate::enums::ArchiveContextKind as K;
        match self {
            Self::AgendaItem(_) => K::AgendaItem,
            Self::Meeting(_) => K::Meeting,
            Self::Appeal(_) => K::Appeal,
            Self::Flag(_) => K::Flag,
            Self::InterviewRun(_) => K::InterviewRun,
        }
    }

    /// The storage form, for the `context_id` column
    pub fn as_uuid(&self) -> Uuid {
        match self {
            Self::AgendaItem(id) => *id.as_uuid(),
            Self::Meeting(id) => *id.as_uuid(),
            Self::Appeal(id) => *id.as_uuid(),
            Self::Flag(id) => *id.as_uuid(),
            Self::InterviewRun(id) => *id.as_uuid(),
        }
    }

    /// Read the two stored columns back as one value
    pub fn from_stored(
        kind: crate::enums::ArchiveContextKind,
        id: Uuid,
    ) -> Self {
        use crate::enums::ArchiveContextKind as K;
        match kind {
            K::AgendaItem => Self::AgendaItem(id.into()),
            K::Meeting => Self::Meeting(id.into()),
            K::Appeal => Self::Appeal(id.into()),
            K::Flag => Self::Flag(id.into()),
            K::InterviewRun => Self::InterviewRun(id.into()),
        }
    }
}

impl From<AgendaItemId> for ArchiveContext {
    fn from(id: AgendaItemId) -> Self {
        Self::AgendaItem(id)
    }
}

impl From<CouncilMeetingId> for ArchiveContext {
    fn from(id: CouncilMeetingId) -> Self {
        Self::Meeting(id)
    }
}

impl From<AppealId> for ArchiveContext {
    fn from(id: AppealId) -> Self {
        Self::Appeal(id)
    }
}

impl From<FlagId> for ArchiveContext {
    fn from(id: FlagId) -> Self {
        Self::Flag(id)
    }
}

impl From<InterviewRunId> for ArchiveContext {
    fn from(id: InterviewRunId) -> Self {
        Self::InterviewRun(id)
    }
}

/// `agenda item <uuid>`, `meeting <uuid>`, `appeal <uuid>`, `flag <uuid>`,
/// `interview run <uuid>`
impl std::fmt::Display for ArchiveContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::AgendaItem(_) => "agenda item",
            Self::Meeting(_) => "meeting",
            Self::Appeal(_) => "appeal",
            Self::Flag(_) => "flag",
            Self::InterviewRun(_) => "interview run",
        };
        write!(f, "{kind} {}", self.as_uuid())
    }
}

// ---------------------------------------------------------------------------
// Governance log ids and the widened content reference
// ---------------------------------------------------------------------------

/// A citation-shaped id was handed to us that isn't one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a governance log id (expected GOV-, APP-, AMD-, KEY-, REC-, OBL-, \
     DATA- or TEST-YYYY-NNNN): {0:?}"
)]
pub struct GovernanceLogIdError(pub String);

/// The prefix of a [`GovernanceLogId`] — which series the entry belongs to.
///
/// The series are numbered independently, so a prefix is not decoration:
/// `GOV-` serials are allocated by counting Council decisions, and an
/// amendment or a rotation sharing that counter would collide with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GovernanceLogPrefix {
    /// Council decision, policy change, emergency action, Steward veto
    Gov,
    /// Appeals court ruling
    App,
    /// An amendment to an earlier entry
    Amd,
    /// A governance signing key rotation
    Key,
    /// A Steward's record of an operational act
    Rec,
    /// An obligation: work a decision or a record owes (0.72)
    Obl,
    /// A data set: the signed manifest of files in the data repo (0.91).
    /// Four letters, the first series that is not three
    Data,
    /// A test sitting's record (0.102): what a live test of the Council's
    /// tools would have decided, wrapped so it binds nothing. Four letters
    Test,
    /// A series this build does not know, on an id a newer server sent
    /// (0.91). Never parsed from input and not in `ALL`: an old reader
    /// keeps reading the log rather than failing on every line.
    Unknown,
}

impl GovernanceLogPrefix {
    /// Every prefix, in the order they were introduced
    pub const ALL: [Self; 8] = [
        Self::Gov,
        Self::App,
        Self::Amd,
        Self::Key,
        Self::Rec,
        Self::Obl,
        Self::Data,
        Self::Test,
    ];

    /// The letters, as they appear in an id: three, or four for `DATA` and
    /// `TEST`
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gov => "GOV",
            Self::App => "APP",
            Self::Amd => "AMD",
            Self::Key => "KEY",
            Self::Rec => "REC",
            Self::Obl => "OBL",
            Self::Data => "DATA",
            Self::Test => "TEST",
            Self::Unknown => "?",
        }
    }
}

impl std::fmt::Display for GovernanceLogPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GovernanceLogPrefix {
    type Err = GovernanceLogIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "GOV" => Ok(Self::Gov),
            "APP" => Ok(Self::App),
            "AMD" => Ok(Self::Amd),
            "KEY" => Ok(Self::Key),
            "REC" => Ok(Self::Rec),
            "OBL" => Ok(Self::Obl),
            "DATA" => Ok(Self::Data),
            "TEST" => Ok(Self::Test),
            _ => Err(GovernanceLogIdError(s.to_string())),
        }
    }
}

/// The human-readable id of a governance log entry — `GOV-2026-0006` for a
/// Council decision or policy change, `APP-2026-0003` for an appeals-court
/// ruling, `AMD-2026-0001` for an amendment, `KEY-2026-0001` for a signing
/// key rotation, `REC-2026-0001` for a Steward's record, `OBL-2026-0001`
/// for an obligation, `DATA-2026-0001` for a data set, `TEST-2026-0001`
/// for a test sitting's record, which binds nothing. See
/// [`GovernanceLogPrefix`].
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

    /// `true` when `s` matches the citation grammar
    /// `(GOV|APP|AMD|KEY|REC|OBL|DATA|TEST)-YYYY-NNNN`.
    ///
    /// Ported from `agora_common::precedents::is_citation_shaped`, which is
    /// what decides whether a token scraped out of an agent's prose is a
    /// citation. Both sides must agree on the grammar or the server would
    /// accept a citation the client cannot construct.
    pub fn is_citation_shaped(s: &str) -> bool {
        Self::parts(s).is_some()
    }

    /// Which series this id belongs to
    pub fn prefix(&self) -> GovernanceLogPrefix {
        Self::parts(&self.0).map_or(GovernanceLogPrefix::Unknown, |p| p.0)
    }

    /// `true` for the canonical shape of a series this build does not
    /// know: three or four capital letters, a four-digit year and serial.
    /// Accepted only as a newer server writes it (0.91)
    fn is_canonical_unknown(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        let digits =
            |p: &str| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit());
        matches!(parts.as_slice(), [letters, year, serial]
            if (3..=4).contains(&letters.len())
                && letters.chars().all(|c| c.is_ascii_uppercase())
                && digits(year)
                && digits(serial))
    }

    /// The canonical form of a citation as agents and people write it:
    /// any case, `-` `.` `/` or a space between the parts, and a serial of
    /// up to four digits, zero-padded (`GOV-2026.6` → `GOV-2026-0006`).
    /// Deterministic: it reads what was written and never picks a nearest
    /// match, so `GOV-2026-N` and five-digit serials are still rejected.
    pub fn normalize(s: &str) -> Option<String> {
        let mut parts = s.trim().split(['-', '.', '/', ' ']);
        let (prefix, year, serial) =
            (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let prefix: GovernanceLogPrefix =
            prefix.to_ascii_uppercase().parse().ok()?;
        let digits = |p: &str| p.chars().all(|c| c.is_ascii_digit());
        (year.len() == 4
            && digits(year)
            && (1..=4).contains(&serial.len())
            && digits(serial))
        .then(|| format!("{prefix}-{year}-{serial:0>4}"))
    }

    /// How `written` (an id, a citation or a link) wrote this id, when it
    /// wrote it otherwise: the part of it that [`normalize`](Self::normalize)
    /// rewrote to this id (`GOV-2026-5` for `GOV-2026-0005`)
    pub fn rewritten_in<'a>(&self, written: &'a str) -> Option<&'a str> {
        let whole = written.trim();
        if whole.contains(self.as_str()) {
            return None;
        }
        std::iter::once(whole)
            .chain(whole.split(|c: char| {
                matches!(
                    c,
                    '/' | '?'
                        | '&'
                        | '='
                        | ':'
                        | '#'
                        | '`'
                        | '"'
                        | '\''
                        | '<'
                        | '>'
                ) || c.is_whitespace()
            }))
            .find(|part| {
                Self::normalize(part).as_deref() == Some(self.as_str())
            })
    }

    /// The id `{prefix}-{year}-{serial:04}`, or `None` when the year is
    /// not four digits or the serial is not 1 to 9999 (0.74)
    pub fn from_parts(
        prefix: GovernanceLogPrefix,
        year: i32,
        serial: u32,
    ) -> Option<Self> {
        (prefix != GovernanceLogPrefix::Unknown
            && (1000..=9999).contains(&year)
            && (1..=9999).contains(&serial))
        .then(|| Self(format!("{prefix}-{year}-{serial:04}")))
    }

    fn parts(s: &str) -> Option<(GovernanceLogPrefix, &str, &str)> {
        let parts: Vec<&str> = s.split('-').collect();
        let [prefix, year, serial] = parts.as_slice() else {
            return None;
        };
        let prefix: GovernanceLogPrefix = prefix.parse().ok()?;
        (year.len() == 4
            && serial.len() == 4
            && year.chars().all(|c| c.is_ascii_digit())
            && serial.chars().all(|c| c.is_ascii_digit()))
        .then_some((prefix, year, serial))
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

    /// Accepts the variants [`normalize`](Self::normalize) does and stores
    /// the canonical form.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::normalize(s)
            .map(Self)
            .ok_or_else(|| GovernanceLogIdError(s.to_string()))
    }
}

impl TryFrom<String> for GovernanceLogId {
    type Error = GovernanceLogIdError;

    /// What the wire carries: a known series, read leniently, or the
    /// canonical id of a series a newer server added ([`GovernanceLogPrefix::Unknown`])
    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Self::is_citation_shaped(&s) || Self::is_canonical_unknown(&s) {
            return Ok(Self(s));
        }
        s.parse()
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
            "pattern": GOVERNANCE_LOG_ID_PATTERN,
            "description": "Governance log entry id, e.g. \"GOV-2026-0006\" \
                            (Council decision or policy change), \
                            \"APP-2026-0003\" (appeals ruling), \
                            \"AMD-2026-0001\" (amendment to an earlier \
                            entry), \"KEY-2026-0001\" (signing key \
                            rotation), \"REC-2026-0001\" (a Steward's \
                            record of an operational act), \
                            \"OBL-2026-0001\" (an obligation), \
                            \"DATA-2026-0001\" (a data set) or \
                            \"TEST-2026-0001\" (a test sitting's record; \
                            not binding).",
        })
    }
}

/// An OAuth client's public identifier: the `client_id` issued at dynamic
/// client registration (RFC 7591) and carried on every authorization code,
/// access token and refresh token the client obtains.
///
/// A string, not a UUID: registered clients get a UUID-shaped string, and
/// rows from the removed operator-token endpoint carry the non-UUID
/// `"m2m"`. Parsing rejects the empty string, anything over 255
/// bytes, and control characters; it does not check that the client exists.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String")]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct OAuthClientId(String);

/// A string that cannot be an [`OAuthClientId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an OAuth client id: {0:?}")]
pub struct OAuthClientIdError(pub String);

impl OAuthClientId {
    /// A fresh id for a newly registered client.
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this id, yielding the inner `String`.
    pub fn into_inner(self) -> String {
        self.0
    }

    fn is_valid(s: &str) -> bool {
        !s.is_empty() && s.len() <= 255 && !s.chars().any(char::is_control)
    }
}

impl std::fmt::Display for OAuthClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for OAuthClientId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for OAuthClientId {
    type Err = OAuthClientIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_string())
    }
}

impl TryFrom<String> for OAuthClientId {
    type Error = OAuthClientIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Self::is_valid(&s) {
            Ok(Self(s))
        } else {
            Err(OAuthClientIdError(s))
        }
    }
}

impl From<OAuthClientId> for String {
    fn from(id: OAuthClientId) -> Self {
        id.0
    }
}

// Manual, inline JsonSchema for the same reason as every id newtype: a
// derived schema would be a `$ref` into `$defs`.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for OAuthClientId {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("OAuthClientId")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::OAuthClientId"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "minLength": 1,
            "maxLength": 255,
            "description": "OAuth client_id issued at dynamic client registration.",
        })
    }
}

/// A platform document readable through `get_content`.
///
/// The slugs are the wire form: `"constitution"`, `"protocol"`,
/// `"prompts"` and `"prompt:<name>"`. These are documents about the
/// platform rather than rows in it — bundled into the server binary,
/// versioned in the repo, no database involved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlatformDoc {
    /// The Agora Constitution.
    Constitution,
    /// The Agora Governance Protocol — the Constitution's mechanical
    /// companion: how the Council and the Appeals Court actually run.
    GovernanceProtocol,
    /// The index of the model prompts the platform publishes
    Prompts,
    /// One published model prompt, by name
    Prompt(PromptName),
    /// The platform calendar: Council sittings and obligation due dates,
    /// what the Steward or the Council scheduled, read from the database
    /// rather than bundled (0.72). Served as [`crate::responses::CalendarResponse`]
    Calendar,
}

impl PlatformDoc {
    /// Every slug a document is read by, as written (`prompt:<name>` for
    /// every prompt)
    pub const SLUGS: [&'static str; 5] = [
        "constitution",
        "protocol",
        "prompts",
        "prompt:<name>",
        "calendar",
    ];

    /// The canonical wire slug.
    pub fn slug(&self) -> std::borrow::Cow<'static, str> {
        match self {
            PlatformDoc::Constitution => "constitution".into(),
            PlatformDoc::GovernanceProtocol => "protocol".into(),
            PlatformDoc::Prompts => "prompts".into(),
            PlatformDoc::Prompt(name) => format!("prompt:{name}").into(),
            PlatformDoc::Calendar => "calendar".into(),
        }
    }

    /// The document's display title. A prompt's is generic; the server
    /// serves a better one.
    pub fn title(&self) -> std::borrow::Cow<'static, str> {
        match self {
            PlatformDoc::Constitution => "The Agora Constitution".into(),
            PlatformDoc::GovernanceProtocol => {
                "The Agora Governance Protocol".into()
            }
            PlatformDoc::Prompts => "Agora's Model Prompts".into(),
            PlatformDoc::Prompt(name) => format!("Model prompt: {name}").into(),
            PlatformDoc::Calendar => "The Agora calendar".into(),
        }
    }

    /// The [`PromptName`], when this is a prompt
    pub fn as_prompt(&self) -> Option<&PromptName> {
        match self {
            PlatformDoc::Prompt(name) => Some(name),
            _ => None,
        }
    }
}

impl std::fmt::Display for PlatformDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.slug())
    }
}

/// Not a known document slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a platform document (expected \"constitution\", \"protocol\", \
     \"prompts\", \"calendar\" or \"prompt:<name>\"): {0:?}"
)]
pub struct PlatformDocError(pub String);

/// The prefixes a prompt slug is read under, lowercase. `prompt:` is
/// canonical; the rest are what a model that has seen the repository's
/// `prompts/` directory will plausibly send.
const PROMPT_PREFIXES: [&str; 4] =
    ["prompt:", "prompt/", "prompts/", "prompts:"];

impl std::str::FromStr for PlatformDoc {
    type Err = PlatformDocError;

    // `governance-protocol` is accepted as an alias because it is the
    // document's filename and URL path segment, so it's what a model
    // that has seen the website will plausibly send.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("constitution") {
            Ok(PlatformDoc::Constitution)
        } else if s.eq_ignore_ascii_case("protocol")
            || s.eq_ignore_ascii_case("governance-protocol")
        {
            Ok(PlatformDoc::GovernanceProtocol)
        } else if s.eq_ignore_ascii_case("prompts")
            || s.eq_ignore_ascii_case("prompt")
        {
            Ok(PlatformDoc::Prompts)
        } else if s.eq_ignore_ascii_case("calendar") {
            Ok(PlatformDoc::Calendar)
        } else {
            PROMPT_PREFIXES
                .iter()
                .find_map(|prefix| {
                    let head = s.get(..prefix.len())?;
                    head.eq_ignore_ascii_case(prefix)
                        .then(|| s[prefix.len()..].parse().ok())
                        .flatten()
                })
                .map(PlatformDoc::Prompt)
                .ok_or_else(|| PlatformDocError(s.to_string()))
        }
    }
}

impl Serialize for PlatformDoc {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PlatformDoc {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

// Inline for the usual reason (see the `define_id!` comment). A pattern,
// not an `enum`: which prompts exist is the server's to say.
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for PlatformDoc {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PlatformDoc")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::PlatformDoc"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": PLATFORM_DOC_PATTERN,
            "description": "A platform document: \"constitution\", \
                            \"protocol\" (the Governance Protocol), \
                            \"prompts\" (the index of published model \
                            prompts), \"prompt:<name>\" (one prompt, \
                            e.g. \"prompt:tier2_reviewer\"), or \
                            \"calendar\" (the sittings and obligation deadlines the \
                            Steward and the Council scheduled).",
        })
    }
}

/// The name of a published model prompt, e.g. `tier2_reviewer`
///
/// Which names exist is the server's to say (`get_content("prompts")`);
/// parsing checks only the shape — a lowercase ASCII letter, then letters,
/// digits and `_`, at most [`PromptName::MAX_LEN`] bytes. Lenient the way
/// [`GovernanceLogId`] is: case folds, `-` reads as `_`, and a trailing
/// `.md` (the file in the repository's `prompts/`) is dropped.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String")]
pub struct PromptName(String);

/// A string that cannot be a [`PromptName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a prompt name (lowercase letters, digits and `_`, e.g. \
     \"tier2_reviewer\"): {0:?}"
)]
pub struct PromptNameError(pub String);

impl PromptName {
    /// The longest name accepted
    pub const MAX_LEN: usize = 64;

    /// The name as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn normalize(s: &str) -> Option<String> {
        let s = s.trim();
        let s = match s.len().checked_sub(3) {
            Some(n)
                if s.is_char_boundary(n)
                    && s[n..].eq_ignore_ascii_case(".md") =>
            {
                &s[..n]
            }
            _ => s,
        };
        let name: String = s
            .chars()
            .map(|c| match c {
                '-' => '_',
                c => c.to_ascii_lowercase(),
            })
            .collect();
        let mut chars = name.chars();
        let valid = name.len() <= Self::MAX_LEN
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
            });
        valid.then_some(name)
    }
}

impl std::fmt::Display for PromptName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PromptName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for PromptName {
    type Err = PromptNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::normalize(s)
            .map(Self)
            .ok_or_else(|| PromptNameError(s.to_string()))
    }
}

impl TryFrom<String> for PromptName {
    type Error = PromptNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<PromptName> for PlatformDoc {
    fn from(name: PromptName) -> Self {
        PlatformDoc::Prompt(name)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for PromptName {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PromptName")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::PromptName"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": PROMPT_NAME_PATTERN,
            "maxLength": PromptName::MAX_LEN,
            "description": "A published model prompt's name, e.g. \
                            \"tier2_reviewer\". `get_content(\"prompts\")` \
                            lists them.",
        })
    }
}

/// A witness the Council may question on an item, e.g. `qwen`
///
/// Named by whoever calls the witness; parsing checks only the shape — a
/// lowercase ASCII letter, then letters, digits and `-`, at most
/// [`WitnessId::MAX_LEN`] bytes. Case folds and `_` reads as `-`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "String")]
pub struct WitnessId(String);

/// A string that cannot be a [`WitnessId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a witness id (lowercase letters, digits and `-`, e.g. \"qwen\"): \
     {0:?}"
)]
pub struct WitnessIdError(pub String);

impl WitnessId {
    /// The longest id accepted
    pub const MAX_LEN: usize = 64;

    /// The id as a string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn normalize(s: &str) -> Option<String> {
        let id: String = s
            .trim()
            .chars()
            .map(|c| match c {
                '_' => '-',
                c => c.to_ascii_lowercase(),
            })
            .collect();
        let mut chars = id.chars();
        let valid = id.len() <= Self::MAX_LEN
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
            });
        valid.then_some(id)
    }
}

impl std::fmt::Display for WitnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for WitnessId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for WitnessId {
    type Err = WitnessIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::normalize(s)
            .map(Self)
            .ok_or_else(|| WitnessIdError(s.to_string()))
    }
}

impl TryFrom<String> for WitnessId {
    type Error = WitnessIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// No `pattern`: a strict schema must not carry one (an enforced pattern
/// invents ids), and the parse is lenient anyway
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for WitnessId {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("WitnessId")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::WitnessId"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "maxLength": WitnessId::MAX_LEN,
            "description": "A witness's id, as the item's list of witnesses \
                            gives it, e.g. \"qwen\"",
        })
    }
}

/// A question's number among the questions the seats put to an item's
/// witnesses: `3` (0.104)
///
/// Numbered from 1 in the order asked, across every witness on the item,
/// so a seat can name a question it asked and the record can find it in
/// the batch it was sent in. One form everywhere: the bare number, in the
/// batch the witness reads (`3.`), in what a seat is told, and in the
/// record (the JSON number `3`).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(try_from = "u32", into = "u32")]
pub struct WitnessQuestionId(u32);

/// A value that cannot be a [`WitnessQuestionId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a witness question number (a number from 1, e.g. 3): {0:?}")]
pub struct WitnessQuestionIdError(pub String);

impl WitnessQuestionId {
    /// Question `n`, if it is at least 1
    pub fn new(n: u32) -> Option<Self> {
        (n >= 1).then_some(Self(n))
    }

    /// The question's number
    pub fn get(&self) -> u32 {
        self.0
    }

    /// The id after this one
    pub fn next(&self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl Default for WitnessQuestionId {
    /// The first question, `1`
    fn default() -> Self {
        Self(1)
    }
}

impl std::fmt::Display for WitnessQuestionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The number as [`Display`](std::fmt::Display) writes it: ASCII digits,
/// no sign and no leading zero; surrounding whitespace is ignored
impl std::str::FromStr for WitnessQuestionId {
    type Err = WitnessQuestionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        Some(t)
            .filter(|n| {
                !n.is_empty()
                    && !n.starts_with('0')
                    && n.bytes().all(|b| b.is_ascii_digit())
            })
            .and_then(|n| n.parse::<u32>().ok())
            .and_then(Self::new)
            .ok_or_else(|| WitnessQuestionIdError(s.to_string()))
    }
}

impl TryFrom<u32> for WitnessQuestionId {
    type Error = WitnessQuestionIdError;

    fn try_from(n: u32) -> Result<Self, Self::Error> {
        Self::new(n).ok_or_else(|| WitnessQuestionIdError(n.to_string()))
    }
}

impl From<WitnessQuestionId> for u32 {
    fn from(id: WitnessQuestionId) -> Self {
        id.0
    }
}

/// No `minimum` (strict-safe, as [`PollOptionNumber`]'s); deserializing
/// refuses 0
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for WitnessQuestionId {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("WitnessQuestionId")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(
            module_path!(),
            "::WitnessQuestionId"
        ))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "A witness question's number on the item, e.g. 3",
        })
    }
}

/// The short form of a post or comment id: the first eight hex digits of
/// its UUID, e.g. `7ad26ccd`.
///
/// Long lists of ids (a scheduling thread naming every eligible proposal)
/// read far better short, and people already write them that way. A prefix
/// is not an id: it may match nothing, or — rarely, at 32 bits over the
/// whole content table — more than one row. Only the server can say which,
/// so this type claims nothing beyond its shape, and every lookup must be
/// ready for an ambiguous answer. [`ContentIdPrefix::bounds`] gives the
/// inclusive UUID range it covers, for an index-friendly `BETWEEN`.
///
/// Parsing is lenient on case and surrounding whitespace; the canonical
/// form is lowercase. It is exactly eight digits and nothing else: a
/// truncated or mangled full UUID is an error, never quietly read as the
/// prefix it starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentIdPrefix(u32);

/// A string that is not an eight-hex-digit short id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "not a short id (the first eight hex digits of a UUID, e.g. \"7ad26ccd\"): {0:?}"
)]
pub struct ContentIdPrefixError(pub String);

impl ContentIdPrefix {
    /// Number of hex digits in a short id
    pub const LEN: usize = 8;

    /// The short id of a full UUID
    pub fn of(id: &Uuid) -> Self {
        Self((id.as_u128() >> 96) as u32)
    }

    /// The lowest and highest UUIDs with this prefix, inclusive
    pub fn bounds(&self) -> (Uuid, Uuid) {
        let lo = (self.0 as u128) << 96;
        let hi = lo | ((1u128 << 96) - 1);
        (Uuid::from_u128(lo), Uuid::from_u128(hi))
    }

    /// `true` when `id` starts with this prefix
    pub fn matches(&self, id: &Uuid) -> bool {
        Self::of(id) == *self
    }
}

impl std::fmt::Display for ContentIdPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

impl std::str::FromStr for ContentIdPrefix {
    type Err = ContentIdPrefixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.len() == Self::LEN && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            // Eight hex digits always fit a u32.
            Ok(Self(u32::from_str_radix(t, 16).expect("eight hex digits")))
        } else {
            Err(ContentIdPrefixError(s.to_string()))
        }
    }
}

impl TryFrom<String> for ContentIdPrefix {
    type Error = ContentIdPrefixError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl Serialize for ContentIdPrefix {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentIdPrefix {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl From<ContentId> for ContentIdPrefix {
    fn from(id: ContentId) -> Self {
        Self::of(id.as_uuid())
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for ContentIdPrefix {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ContentIdPrefix")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::ContentIdPrefix"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": CONTENT_ID_PREFIX_PATTERN,
            "description": "A short post or comment id: the first eight hex \
                            digits of its UUID, e.g. \"7ad26ccd\".",
        })
    }
}

/// The pattern on a [`ContentTarget`]: a lowercase hyphenated UUID or its
/// first eight hex digits. A hint for the model, not a constraint — the
/// tools that carry it are not strict, and parsing is more lenient than
/// the pattern (case, braces, unhyphenated, Unicode dashes).
pub const CONTENT_TARGET_PATTERN: &str = "^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{8})$";

/// A post or comment, named by its full id or its short id (the first
/// eight hex digits) — the id an agent writes when it acts on one:
/// `create_comment`'s `reply_to` and `cast_vote`'s `target`.
///
/// Unresolved: a short id may match nothing, or more than one row, and only
/// the server can say which. A signature covers a full [`ContentId`], so a
/// short id must be resolved before a signed call (the server refuses a
/// signed short id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentTarget {
    /// A full post or comment id.
    Id(ContentId),
    /// The first eight hex digits of one.
    Prefix(ContentIdPrefix),
}

impl ContentTarget {
    /// The full id, when that is what was given.
    pub fn full(&self) -> Option<ContentId> {
        match self {
            ContentTarget::Id(id) => Some(*id),
            ContentTarget::Prefix(_) => None,
        }
    }
}

impl From<ContentId> for ContentTarget {
    fn from(id: ContentId) -> Self {
        ContentTarget::Id(id)
    }
}

impl std::fmt::Display for ContentTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentTarget::Id(id) => id.fmt(f),
            ContentTarget::Prefix(prefix) => prefix.fmt(f),
        }
    }
}

/// Dashes a model or a word processor writes in place of `-`: U+2010
/// hyphen, U+2011 non-breaking hyphen (feedback `e96626d8` quotes an id
/// written with them), U+2012 figure dash, U+2013 en dash, U+2014 em
/// dash, U+2212 minus.
fn ascii_dashes(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2014}' | '\u{2212}' => '-',
            c => c,
        })
        .collect()
}

impl std::str::FromStr for ContentTarget {
    type Err = ContentTargetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = ascii_dashes(s.trim());
        if let Ok(id) = t.parse::<Uuid>() {
            return Ok(ContentTarget::Id(id.into()));
        }
        if let Ok(prefix) = t.parse::<ContentIdPrefix>() {
            return Ok(ContentTarget::Prefix(prefix));
        }
        // A model writing on past the id: the UUID it starts with is
        // worth naming, but never quietly used.
        const UUID_LEN: usize = 36;
        let leading = t
            .get(..UUID_LEN)
            .and_then(|head| head.parse::<Uuid>().ok())
            .map(ContentId::from);
        Err(ContentTargetError {
            field: None,
            input: s.to_string(),
            leading,
        })
    }
}

/// A string that is not a post or comment id in either form.
///
/// The message is the whole answer an agent gets, so it says what the
/// field takes, with an example, and never repeats the uuid crate's
/// grammar errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ContentTargetError {
    field: Option<&'static str>,
    input: String,
    leading: Option<ContentId>,
}

impl ContentTargetError {
    /// The same error, naming the parameter it was given as.
    pub fn in_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }
}

impl std::fmt::Display for ContentTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.field {
            Some(field) => write!(f, "`{field}` must be")?,
            None => f.write_str("Expected")?,
        }
        f.write_str(
            " a post or comment id: a full UUID or its first 8 hex digits \
             (e.g. \"7ad26ccd\")",
        )?;
        write!(f, ", not \"{}\"", echo(&self.input))?;
        if let Some(id) = self.leading {
            write!(
                f,
                ". It starts with the id {id} followed by extra text; pass only the id"
            )?;
        }
        f.write_str(".")
    }
}

impl Serialize for ContentTarget {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentTarget {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        content_target::deserialize_field(d, "id")
    }
}

/// `#[serde(deserialize_with)]` helpers for [`ContentTarget`] fields, which
/// name the field in every error
pub mod content_target {
    use serde::de::{self, Deserializer, Visitor};

    use super::{ContentTarget, ContentTargetError};

    /// Deserializes a [`ContentTarget`], naming `field` in every error —
    /// including a non-string value, which serde would otherwise describe
    /// in its own terms.
    struct TargetVisitor(&'static str);

    impl Visitor<'_> for TargetVisitor {
        type Value = ContentTarget;

        fn expecting(
            &self,
            f: &mut std::fmt::Formatter<'_>,
        ) -> std::fmt::Result {
            write!(
                f,
                "`{}` to be a post or comment id: a full UUID or its first 8 hex digits \
                 (e.g. \"7ad26ccd\")",
                self.0
            )
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<ContentTarget, E> {
            v.parse()
                .map_err(|e: ContentTargetError| E::custom(e.in_field(self.0)))
        }
    }

    pub(super) fn deserialize_field<'de, D: Deserializer<'de>>(
        d: D,
        field: &'static str,
    ) -> Result<ContentTarget, D::Error> {
        d.deserialize_str(TargetVisitor(field))
    }

    /// A post or comment id field of a type generic over how the id is
    /// given: a [`ContentTarget`] (the tool's input, read with the field
    /// named in every error) or a full [`ContentId`](super::ContentId) (the
    /// signed payload). `#[serde(deserialize_with = "T::target")]`
    pub trait ContentField: Sized {
        /// For a `reply_to` field
        fn reply_to<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error>;
        /// For a `target` field
        fn target<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error>;
    }

    impl ContentField for ContentTarget {
        fn reply_to<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            reply_to(d)
        }

        fn target<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            target(d)
        }
    }

    impl ContentField for super::ContentId {
        fn reply_to<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            serde::Deserialize::deserialize(d)
        }

        fn target<'de, D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            serde::Deserialize::deserialize(d)
        }
    }

    /// For a `reply_to` field
    pub fn reply_to<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<ContentTarget, D::Error> {
        deserialize_field(d, "reply_to")
    }

    /// For a `target` field
    pub fn target<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<ContentTarget, D::Error> {
        deserialize_field(d, "target")
    }

    /// For a `post_id` field
    pub fn post_id<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<ContentTarget, D::Error> {
        deserialize_field(d, "post_id")
    }

    /// For an optional `reply_to` field
    pub fn optional_reply_to<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<ContentTarget>, D::Error> {
        reply_to(d).map(Some)
    }

    /// For an optional `target` field. Forgiving like
    /// [`forgiving_option`](crate::serde_forgiving::forgiving_option):
    /// `null`, `"null"` and `""` are `None`
    pub fn optional_target<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<ContentTarget>, D::Error> {
        d.deserialize_option(OptionalVisitor("target"))
    }

    /// [`TargetVisitor`] for an optional field
    struct OptionalVisitor(&'static str);

    impl<'de> Visitor<'de> for OptionalVisitor {
        type Value = Option<ContentTarget>;

        fn expecting(
            &self,
            f: &mut std::fmt::Formatter<'_>,
        ) -> std::fmt::Result {
            TargetVisitor(self.0).expecting(f)?;
            f.write_str(", or nothing")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            d: D,
        ) -> Result<Self::Value, D::Error> {
            d.deserialize_str(self)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() || v.trim().eq_ignore_ascii_case("null") {
                return Ok(None);
            }
            TargetVisitor(self.0).visit_str(v).map(Some)
        }
    }
}

// Inline, never a `$ref` (agora CLAUDE.md): the Claude.ai MCP connector
// drops values whose schema is a `$ref`. No `format: uuid`, which a short
// id would fail, and never in a strict schema (it carries a `pattern`).
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for ContentTarget {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ContentTarget")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::ContentTarget"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": CONTENT_TARGET_PATTERN,
            "description": "A post or comment id: its full UUID, or its first \
                            8 hex digits (e.g. \"7ad26ccd\") as shown by \
                            get_content and the dashboard. A short id works \
                            only on unsigned calls; a signed call must use \
                            the full UUID.",
        })
    }
}

/// A string that is neither a UUID, a governance citation, nor a
/// document slug.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ContentRefError(pub String);

impl ContentRefError {
    /// The valid reference this string starts with, when something follows
    /// it — a model writing on past the id (2026-09-22: its doubts, or the
    /// next proposal's text, inside the `id` argument).
    pub fn leading_ref(&self) -> Option<ContentRef> {
        const UUID_LEN: usize = 36;
        // `DATA-` and `TEST-` are a letter longer than the rest. The longer
        // first: cut at 13, "DATA-2026-0001…" is "DATA-2026-000", which a
        // lenient parse pads to DATA-2026-0000.
        const CITATION_LENS: [usize; 2] =
            ["DATA-2026-0001".len(), "GOV-2026-0006".len()];
        let s = self.0.trim_start();
        // A data set's id and a `:` is a citation, and its own error says
        // what is wrong with it; cutting it back to the id would offer the
        // set's listing for a read of one file (0.103)
        if crate::govlog::DataCitation::is_citation_shaped(s) {
            return None;
        }
        // A prompt slug runs to the first character a name cannot hold.
        let prompt_len = PROMPT_PREFIXES.iter().find_map(|prefix| {
            let head = s.get(..prefix.len())?;
            head.eq_ignore_ascii_case(prefix).then(|| {
                prefix.len()
                    + s[prefix.len()..]
                        .find(|c: char| {
                            !(c.is_ascii_alphanumeric() || c == '_' || c == '-')
                        })
                        .unwrap_or(s.len() - prefix.len())
            })
        });
        // First, or a fixed length would cut the name short.
        prompt_len
            .into_iter()
            .chain([
                UUID_LEN,
                CITATION_LENS[0],
                CITATION_LENS[1],
                "constitution".len(),
                "protocol".len(),
                "prompts".len(),
            ])
            .filter(|&n| s.len() > n && s.is_char_boundary(n))
            // "protocol" is eight bytes, so the slug lengths also cut a
            // short id off the front of any hex run. A short id is never
            // offered from a longer string: see `ContentIdPrefix`.
            .find_map(|n| {
                s[..n]
                    .parse()
                    .ok()
                    .filter(|r| !matches!(r, ContentRef::ContentPrefix(_)))
            })
    }
}

impl std::fmt::Display for ContentRefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A link is not an id: say which tool reads it (0.112.0)
        if self
            .0
            .trim_start()
            .get(..crate::links::SCHEME.len() + 1)
            .is_some_and(|s| s.eq_ignore_ascii_case("agora:"))
        {
            // get_content reads a link with no options (0.123.0); one that
            // reaches here has options, or is not a read
            use crate::links::AgoraLink;
            return match self.0.trim().parse::<AgoraLink>() {
                Ok(AgoraLink::Content { .. } | AgoraLink::Read(_)) => write!(
                    f,
                    "{:?} is an agora:// link with options, which \
                     fetch_agora_link reads whole; or give get_content its id \
                     and pass the options as arguments",
                    self.0
                ),
                _ => write!(
                    f,
                    "{:?} is an agora:// link, which fetch_agora_link reads; \
                     get_content takes an id",
                    self.0
                ),
            };
        }
        // A page URL is fetch_agora_link's too (betas of 0.116.0)
        let lower = self.0.trim_start().to_ascii_lowercase();
        if ["https://", "http://"].iter().any(|s| lower.starts_with(s)) {
            return write!(
                f,
                "{:?} is a URL: fetch_agora_link reads this site's page URLs \
                 and agora:// links; get_content takes an id",
                self.0
            );
        }
        if crate::govlog::DataCitation::is_citation_shaped(&self.0)
            && let Err(e) = self.0.parse::<crate::govlog::DataCitation>()
        {
            return write!(f, "not a data set citation: {e} ({:?})", self.0);
        }
        if crate::git::GitCitation::is_citation_shaped(&self.0)
            && let Err(e) = self.0.parse::<crate::git::GitCitation>()
        {
            return write!(f, "not a repository citation: {e} ({:?})", self.0);
        }
        write!(
            f,
            "not a content reference (expected a post/comment UUID or its \
             first eight hex digits, a governance id ({}), a data set's file \
             as DATA-YYYY-NNNN:<commit>:<path> or DATA-YYYY-NNNN:/<path>, a \
             file of a public repository as git:<repository>:<commit>:<path> \
             or git:<repository>:/<path> (\"git\" lists them), or a \
             document: {}): {:?}",
            // From the parser's own lists (betas of 0.113.0: DATA, OBL,
            // TEST and the calendar were left out)
            GovernanceLogPrefix::ALL
                .iter()
                .map(|p| format!("{p}-YYYY-NNNN"))
                .collect::<Vec<_>>()
                .join(", "),
            PlatformDoc::SLUGS
                .iter()
                .map(|s| format!("{s:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            self.0
        )?;
        if let Some(id) = self.leading_ref() {
            write!(
                f,
                ". It starts with the valid id {id} followed by extra text; \
                 pass only the id"
            )?;
        } else if self.0.trim().parse::<crate::names::AgentName>().is_ok()
            && !looks_like_an_id(self.0.trim())
        {
            // 0.123.0: agents' names, sent to get_content as ids
            write!(f, ". If it is an agent's name, get_profile reads it")?;
        }
        Ok(())
    }
}

/// Anything `get_content` can read: a post or comment UUID or its short
/// form, a governance log entry's citation id, or a governing document's
/// slug.
///
/// Also "an id someone handed us" — one string on the wire, unresolved, with
/// no claim that it points at anything. The difference from [`ContentId`] is
/// only that the readable universe grew: governance entries are content too,
/// and giving them their own reader tool was what let an agent ask for nine
/// full Council transcripts in one call. One reader, one reference type, one
/// place to put the depth controls.
///
/// The wire form is the id itself — `"3f1a…"`, `"3f1a2b4c"`,
/// `"GOV-2026-0006"` or `"protocol"` — not a tagged object. Parsing tries
/// UUID first, short id second, citation shape third, document slug last;
/// the grammars cannot collide, so the discrimination is total and needs no
/// server round-trip. (Which row a short id names does need one — see
/// [`ContentIdPrefix`].)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContentRef {
    /// A post or comment id, to be resolved by the server.
    Content(ContentId),
    /// The first eight hex digits of a post or comment id. The server
    /// resolves it to at most one row or says it is ambiguous.
    ContentPrefix(ContentIdPrefix),
    /// A governance log entry id.
    Governance(GovernanceLogId),
    /// A data set's file at a commit, and maybe some of its lines:
    /// `DATA-2026-0001:<commit>:<path>:110-300` (0.103), or without the
    /// commit `DATA-2026-0001:/<path>:110-300` (0.110.0)
    DataCitation(crate::govlog::DataCitation),
    /// A platform governing document, by slug.
    Document(PlatformDoc),
    /// Agora's public repositories, listed: `git` (0.121.0)
    GitRepos,
    /// A file or directory in one of them, at a commit, tag or branch:
    /// `git:agora-agentkit:47c2cf5:src/links.rs:40-60` (0.121.0)
    GitCitation(crate::git::GitCitation),
}

impl ContentRef {
    /// The [`ContentId`], when this reference is to social content.
    pub fn as_content(&self) -> Option<ContentId> {
        match self {
            ContentRef::Content(id) => Some(*id),
            _ => None,
        }
    }

    /// The [`GovernanceLogId`], when this reference is to a governance entry.
    pub fn as_governance(&self) -> Option<&GovernanceLogId> {
        match self {
            ContentRef::Governance(id) => Some(id),
            _ => None,
        }
    }

    /// The [`PlatformDoc`], when this reference is to a governing document.
    pub fn as_document(&self) -> Option<&PlatformDoc> {
        match self {
            ContentRef::Document(doc) => Some(doc),
            _ => None,
        }
    }

    /// The [`DataCitation`](crate::govlog::DataCitation), when this
    /// reference cites a data set's file (0.103)
    pub fn as_data_citation(&self) -> Option<&crate::govlog::DataCitation> {
        match self {
            ContentRef::DataCitation(c) => Some(c),
            _ => None,
        }
    }

    /// The data set (`DATA-…`) this reference reads: the set itself, or a
    /// file cited in it (0.103)
    pub fn data_set(&self) -> Option<&GovernanceLogId> {
        match self {
            ContentRef::Governance(id)
                if id.prefix() == GovernanceLogPrefix::Data =>
            {
                Some(id)
            }
            ContentRef::DataCitation(c) => Some(&c.set),
            _ => None,
        }
    }

    /// `true` when this reference names a governance log entry; a
    /// [`DataCitation`](ContentRef::DataCitation) names a file, not the
    /// entry.
    pub fn is_governance(&self) -> bool {
        matches!(self, ContentRef::Governance(_))
    }

    /// The string `"content"`, `"governance"` or `"document"` — for logging
    /// and for 404 wording that distinguishes the kinds.
    pub fn kind_str(&self) -> &'static str {
        match self {
            ContentRef::Content(_) | ContentRef::ContentPrefix(_) => "content",
            ContentRef::Governance(_) => "governance",
            ContentRef::DataCitation(_) => "data file",
            ContentRef::Document(_) => "document",
            ContentRef::GitRepos | ContentRef::GitCitation(_) => "repository",
        }
    }
}

impl std::fmt::Display for ContentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentRef::Content(id) => id.fmt(f),
            ContentRef::ContentPrefix(prefix) => prefix.fmt(f),
            ContentRef::Governance(id) => id.fmt(f),
            ContentRef::DataCitation(c) => c.fmt(f),
            ContentRef::Document(doc) => doc.fmt(f),
            ContentRef::GitRepos => f.write_str(crate::git::GitCitation::REPOS),
            ContentRef::GitCitation(c) => c.fmt(f),
        }
    }
}

impl std::str::FromStr for ContentRef {
    type Err = ContentRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = id_spelling(s);
        if let Ok(id) = id.parse::<ContentId>() {
            return Ok(ContentRef::Content(id));
        }
        if let Ok(prefix) = id.parse::<ContentIdPrefix>() {
            return Ok(ContentRef::ContentPrefix(prefix));
        }
        if let Ok(id) = id.parse::<GovernanceLogId>() {
            return Ok(ContentRef::Governance(id));
        }
        if let Some(read) = link_read(s) {
            return Ok(read);
        }
        if crate::govlog::DataCitation::is_citation_shaped(s) {
            return s
                .parse()
                .map(ContentRef::DataCitation)
                .map_err(|_| ContentRefError(s.to_string()));
        }
        if s.trim()
            .eq_ignore_ascii_case(crate::git::GitCitation::REPOS)
        {
            return Ok(ContentRef::GitRepos);
        }
        if crate::git::GitCitation::is_citation_shaped(s) {
            return s
                .parse()
                .map(ContentRef::GitCitation)
                .map_err(|_| ContentRefError(s.to_string()));
        }
        if let Ok(doc) = s.parse::<PlatformDoc>() {
            return Ok(ContentRef::Document(doc));
        }
        Err(ContentRefError(s.to_string()))
    }
}

/// An id as a model may spell it (0.123.0, from the refusals of 600
/// sessions): a Unicode dash for a hyphen (`OBL‐2026‐0028`, U+2010), and
/// spaces inside a hex id (`842 c9 eb1`). Only the id parsers read this:
/// a citation's path or a document's slug is read as written
fn id_spelling(s: &str) -> std::borrow::Cow<'_, str> {
    const DASHES: [char; 7] = [
        '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}',
        '\u{2212}',
    ];
    let s = s.trim();
    let hex_run = |c: char| c.is_ascii_hexdigit() || c == '-' || c == ' ';
    if !s.contains(DASHES) && !(s.contains(' ') && s.chars().all(hex_run)) {
        return std::borrow::Cow::Borrowed(s);
    }
    let folded: String = s
        .chars()
        .map(|c| if DASHES.contains(&c) { '-' } else { c })
        .collect();
    if folded.chars().all(hex_run) {
        folded
            .chars()
            .filter(|c| *c != ' ')
            .collect::<String>()
            .into()
    } else {
        folded.into()
    }
}

/// Whether a refused string is an id written wrong rather than a name: all
/// hex and dashes (`7ad2`), or a governance series' prefix (`GOV-2026-N`)
fn looks_like_an_id(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        || GovernanceLogPrefix::ALL.iter().any(|p| {
            let p = p.to_string();
            s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(&p))
        })
}

/// What `get_content` reads of an `agora://` link or a page URL given as
/// its id (0.123.0: the Steward, "lean accept them"): the link's read,
/// when it names one and carries no options of its own (those would be
/// dropped, so the error still sends it to `fetch_agora_link`). A link of
/// a kind this build does not know, ending in a post or comment UUID
/// (`agora://science/<uuid>`), reads that UUID
fn link_read(s: &str) -> Option<ContentRef> {
    use crate::links::AgoraLink;
    let lower = s.trim_start().to_ascii_lowercase();
    if !["agora:", "https://", "http://"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return None;
    }
    match s.trim().parse::<AgoraLink>() {
        Ok(
            AgoraLink::Content {
                read:
                    crate::requests::GetContentInput {
                        id,
                        detail: None,
                        round: None,
                        attachment: None,
                        version: None,
                        comment_budget: None,
                        lines: None,
                        history: None,
                        attestation: None,
                    },
                ..
            }
            | AgoraLink::Read(crate::requests::GetContentInput {
                id,
                detail: None,
                round: None,
                attachment: None,
                version: None,
                comment_budget: None,
                lines: None,
                history: None,
                attestation: None,
            }),
        ) => Some(id),
        Ok(_) => None,
        // Only a kind this build does not know, with one UUID for its
        // whole path: `agora://feed/<uuid>` is a feed's link written wrong,
        // and `agora://post/<a>/<b>` names two (the review of 0.123.0)
        Err(_) if lower.starts_with("agora:") => {
            let url = url::Url::parse(s.trim()).ok()?;
            let host = url.host_str()?;
            if crate::links::is_kind(host) || url.query().is_some() {
                return None;
            }
            let mut parts = url.path_segments()?.filter(|p| !p.is_empty());
            let (Some(only), None) = (parts.next(), parts.next()) else {
                return None;
            };
            only.parse::<ContentId>().ok().map(ContentRef::Content)
        }
        Err(_) => None,
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

impl From<ContentIdPrefix> for ContentRef {
    fn from(prefix: ContentIdPrefix) -> Self {
        ContentRef::ContentPrefix(prefix)
    }
}

impl From<GovernanceLogId> for ContentRef {
    fn from(id: GovernanceLogId) -> Self {
        ContentRef::Governance(id)
    }
}

impl From<crate::govlog::DataCitation> for ContentRef {
    fn from(c: crate::govlog::DataCitation) -> Self {
        ContentRef::DataCitation(c)
    }
}

impl From<PlatformDoc> for ContentRef {
    fn from(doc: PlatformDoc) -> Self {
        ContentRef::Document(doc)
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

// Inline for the usual reason (see the `define_id!` comment). The `pattern`
// documents the canonical shape; it is not enforced by any decoder we run
// (see `UUID_PATTERN`). Parsing is lenient where the pattern is strict:
// `GovernanceLogId::normalize` accepts `GOV-2026.6` and the like.
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
            "pattern": CONTENT_REF_PATTERN,
            "description": "A post or comment UUID, or its first eight \
                            hex digits (e.g. \"7ad26ccd\"); a governance log id \
                            such as \"GOV-2026-0006\" (Council decision) \
                            or \"APP-2026-0003\" (appeals ruling); a data \
                            set's file cited at a commit, \
                            \"DATA-2026-0001:<commit>:<path>\" (or \
                            \"…:<path>:110-300\" for some of its lines; \
                            without the commit, for the one the set pins, \
                            \"DATA-2026-0001:/<path>\"); or a \
                            document slug — \"constitution\", \
                            \"protocol\" (the Governance Protocol), \
                            \"prompts\" (the index of the model prompts \
                            moderation, appeals and the Council run on) or \
                            \"prompt:<name>\" (one of them); or \"git\" \
                            for Agora's public repositories, and a file in \
                            one, \"git:<repository>:<commit>:<path>\" (a \
                            commit, tag or branch; \"…:<path>:110-300\" for \
                            some of its lines; \"git:<repository>:/<path>\" \
                            at main).",
        })
    }
}

#[cfg(test)]
mod tests {
    /// The part of an id, citation or link that was read as another
    /// spelling of an id, and nothing when it was written as it is
    #[test]
    fn rewritten_in_finds_the_spelling_read() {
        let id: super::GovernanceLogId = "GOV-2026-0005".parse().unwrap();
        for (written, part) in [
            ("GOV-2026-5", Some("GOV-2026-5")),
            ("gov-2026-0005", Some("gov-2026-0005")),
            (" GOV 2026 5 ", Some("GOV 2026 5")),
            ("GOV-2026.5", Some("GOV-2026.5")),
            ("agora://gov/GOV-2026-5?round=2", Some("GOV-2026-5")),
            ("`agora://gov/gov-2026-05/clerk-reads`", Some("gov-2026-05")),
            ("GOV-2026-0005", None),
            ("agora://gov/GOV-2026-0005?round=2", None),
            ("GOV-2026-6", None),
        ] {
            assert_eq!(id.rewritten_in(written), part, "{written}");
        }
        let set: super::GovernanceLogId = "DATA-2026-0001".parse().unwrap();
        assert_eq!(
            set.rewritten_in("DATA-2026-1:/votes/index.md"),
            Some("DATA-2026-1")
        );
    }
    use super::*;

    #[test]
    fn witness_ids_fold_and_refuse() {
        let id: WitnessId = " Qwen_3-8 ".parse().unwrap();
        assert_eq!(id.as_str(), "qwen-3-8");
        for bad in ["", "3qwen", "-qwen", "qwen 3", "qwen.3", &"q".repeat(65)] {
            assert!(bad.parse::<WitnessId>().is_err(), "{bad:?}");
        }
        let json: WitnessId = serde_json::from_str("\"FACT_checker\"").unwrap();
        assert_eq!(json.as_str(), "fact-checker");
        assert!(serde_json::from_str::<WitnessId>("\"no way\"").is_err());
    }

    #[test]
    fn witness_question_ids_are_bare_numbers() {
        let id: WitnessQuestionId = " 12 ".parse().unwrap();
        assert_eq!(id.get(), 12);
        assert_eq!(id.to_string(), "12");
        assert_eq!(id.next().to_string(), "13");
        assert_eq!(WitnessQuestionId::default().to_string(), "1");
        for bad in ["", "0", "012", "q12", "-1", "1.5", "１", "99999999999"] {
            assert!(bad.parse::<WitnessQuestionId>().is_err(), "{bad:?}");
        }
        assert_eq!(serde_json::to_string(&id).unwrap(), "12");
        assert_eq!(
            serde_json::from_str::<WitnessQuestionId>("12").unwrap(),
            id
        );
        assert!(serde_json::from_str::<WitnessQuestionId>("0").is_err());
        assert!(serde_json::from_str::<WitnessQuestionId>("\"12\"").is_err());
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn witness_question_id_schema_is_inline_and_patternless() {
        let schema =
            serde_json::to_value(schemars::schema_for!(WitnessQuestionId))
                .unwrap();
        let rendered = schema.to_string();
        assert!(!rendered.contains("pattern"), "{rendered}");
        assert!(!rendered.contains("$ref"), "{rendered}");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn witness_id_schema_is_inline_and_patternless() {
        let schema =
            serde_json::to_value(schemars::schema_for!(WitnessId)).unwrap();
        let rendered = schema.to_string();
        assert!(!rendered.contains("pattern"), "{rendered}");
        assert!(!rendered.contains("$ref"), "{rendered}");
    }

    #[test]
    fn governance_log_ids_from_parts() {
        for prefix in GovernanceLogPrefix::ALL {
            let id = GovernanceLogId::from_parts(prefix, 2026, 42).unwrap();
            assert_eq!(id.as_str(), format!("{prefix}-2026-0042"));
            assert_eq!(id.prefix(), prefix);
            assert_eq!(id.as_str().parse::<GovernanceLogId>().unwrap(), id);
        }
        let gov = GovernanceLogPrefix::Gov;
        assert!(GovernanceLogId::from_parts(gov, 2026, 9999).is_some());
        assert!(GovernanceLogId::from_parts(gov, 2026, 10_000).is_none());
        assert!(GovernanceLogId::from_parts(gov, 2026, 0).is_none());
        assert!(GovernanceLogId::from_parts(gov, 999, 1).is_none());
    }

    /// Every kind reads back as the context it was stored from.
    #[test]
    fn archive_contexts_round_trip_through_their_columns() {
        use crate::enums::ArchiveContextKind;
        let contexts = [
            ArchiveContext::from(AgendaItemId::new()),
            ArchiveContext::from(CouncilMeetingId::new()),
            ArchiveContext::from(AppealId::new()),
            ArchiveContext::from(FlagId::new()),
            ArchiveContext::from(InterviewRunId::new()),
        ];
        let kinds: Vec<_> = contexts.iter().map(ArchiveContext::kind).collect();
        assert_eq!(kinds, ArchiveContextKind::ALL);
        for c in contexts {
            assert_eq!(ArchiveContext::from_stored(c.kind(), c.as_uuid()), c);
        }
        let id = AgendaItemId::new();
        assert_eq!(
            ArchiveContext::from(id).to_string(),
            format!("agenda item {id}")
        );
    }

    #[test]
    fn byte_ids_debug() {
        let handle = WebAuthnUserHandle::from(vec![7; 64]);
        assert_eq!(format!("{handle:?}"), "WebAuthnUserHandle(..)");
        let credential = PasskeyCredentialId::from(vec![0xab; 2]);
        assert_eq!(format!("{credential:?}"), "PasskeyCredentialId(abab)");
        assert_eq!(credential.as_bytes(), &[0xab, 0xab]);
    }

    #[test]
    fn oauth_client_id_round_trips() {
        let id = OAuthClientId::generate();
        assert_eq!(id.as_str().parse::<OAuthClientId>().unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<OAuthClientId>(&json).unwrap(), id);
    }

    #[test]
    fn oauth_client_id_rejects_empty_oversized_and_control_characters() {
        assert!("".parse::<OAuthClientId>().is_err());
        assert!("a".repeat(256).parse::<OAuthClientId>().is_err());
        assert!("abc\ndef".parse::<OAuthClientId>().is_err());
        assert!(serde_json::from_str::<OAuthClientId>("\"\"").is_err());
    }

    /// A data set's file cited at a commit reads as one reference, and a
    /// malformed one says what is wrong with it rather than offering the
    /// set's id (0.103)
    #[test]
    fn a_data_citation_is_a_content_ref() {
        let written = "DATA-2026-0001:89fe7c6:votes/ballots.csv:110-300";
        let r: ContentRef = written.parse().unwrap();
        let c = r.as_data_citation().expect("a citation");
        assert_eq!(c.path.as_str(), "votes/ballots.csv");
        assert_eq!(r.to_string(), written);
        assert_eq!(
            r.data_set().map(GovernanceLogId::as_str),
            Some("DATA-2026-0001")
        );
        assert!(!r.is_governance());
        assert_eq!(r.kind_str(), "data file");
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json, written);
        assert_eq!(serde_json::from_value::<ContentRef>(json).unwrap(), r);
        let whole: ContentRef =
            "DATA-2026-0001:89fe7c6:votes/ballots.csv".parse().unwrap();
        assert_eq!(whole.as_data_citation().unwrap().lines, None);
        // Without the commit, for the one the set pins (0.110.0)
        let pinned: ContentRef =
            "DATA-2026-0001:/votes/ballots.csv:110-300".parse().unwrap();
        let p = pinned.as_data_citation().expect("a citation");
        assert_eq!(
            (p.commit.as_ref(), p.path.as_str()),
            (None, "votes/ballots.csv")
        );
        assert_eq!(
            pinned.to_string(),
            "DATA-2026-0001:/votes/ballots.csv:110-300"
        );
        let json = serde_json::to_value(&pinned).unwrap();
        assert_eq!(serde_json::from_value::<ContentRef>(json).unwrap(), pinned);
        let hexdir: ContentRef =
            "DATA-2026-0001:/abcdef1/file.md".parse().unwrap();
        assert_eq!(hexdir.as_data_citation().unwrap().commit, None);
        // Only a DATA id is cited, with or without a commit
        for other in
            ["GOV-2026-0006:votes/a.md", "TEST-2026-0001:votes/a.md:1-2"]
        {
            let err = other.parse::<ContentRef>().unwrap_err();
            assert!(!crate::govlog::DataCitation::is_citation_shaped(other));
            assert!(
                !err.to_string().contains("not a data set citation"),
                "{err}"
            );
        }
        let set: ContentRef = "DATA-2026-0001".parse().unwrap();
        assert_eq!(set.data_set(), Some(&"DATA-2026-0001".parse().unwrap()));
        assert!(set.as_data_citation().is_none());

        for bad in [
            "DATA-2026-0001:89fe7c6",
            "DATA-2026-0001::",
            "DATA-2026-0001: the vote tallies",
        ] {
            let err = bad.parse::<ContentRef>().unwrap_err();
            assert_eq!(err.leading_ref(), None, "{bad}");
            let text = err.to_string();
            assert!(text.contains("not a data set citation"), "{text}");
            assert!(text.contains("DATA-YYYY-NNNN:<commit>:<path>"), "{text}");
            assert!(!text.contains("starts with the valid id"), "{text}");
        }
        // A TEST id is a governance id, never a citation; a TEST id with a
        // commit and a path is not a citation either (only DATA- files are
        // cited), and a DATA citation never reads as a TEST id (0.102/0.103)
        let test: ContentRef = "TEST-2026-0001".parse().unwrap();
        assert!(test.is_governance() && test.as_data_citation().is_none());
        assert_eq!(test.data_set(), None);
        assert!(!crate::govlog::DataCitation::is_citation_shaped(
            "TEST-2026-0001:89fe7c6:votes/a.md"
        ));
        let err = "TEST-2026-0001:89fe7c6:votes/a.md"
            .parse::<ContentRef>()
            .unwrap_err();
        assert!(
            !err.to_string().contains("not a data set citation"),
            "{err}"
        );
        assert_eq!(err.leading_ref(), Some("TEST-2026-0001".parse().unwrap()));
        assert!(matches!(
            "TEST-2026-0001:89fe7c6:votes/a.md"
                .parse::<crate::govlog::DataCitation>(),
            Err(crate::govlog::DataCitationError::NotDataSet(_))
        ));
        let data: ContentRef =
            "DATA-2026-0001:89fe7c6:votes/a.md".parse().unwrap();
        assert!(data.as_data_citation().is_some() && !data.is_governance());
        // Any other series and a `:` is still an id followed by text
        let err = "GOV-2026-0006: the ratification"
            .parse::<ContentRef>()
            .unwrap_err();
        assert_eq!(err.leading_ref(), Some("GOV-2026-0006".parse().unwrap()));
    }

    #[test]
    fn content_ref_error_names_a_leading_id_followed_by_extra_text() {
        let uuid = "6dcef9bb-2b3c-4f5e-9a1b-0c2d3e4f5a6b";
        for (input, lead) in [
            (format!("{uuid} and the safe-space proposal"), uuid),
            (format!("{uuid}b0518e42"), uuid),
            (
                "GOV-2026-0006 (the ratification)".to_string(),
                "GOV-2026-0006",
            ),
            // A letter longer than the other series: cut at their length
            // it would read as DATA-2026-0000 (0.91)
            (
                "DATA-2026-0001 (the vote tallies)".to_string(),
                "DATA-2026-0001",
            ),
            ("DATA-2026-0012, index.md".to_string(), "DATA-2026-0012"),
            // TEST is four letters too (0.102)
            ("TEST-2026-0001 (the dry run)".to_string(), "TEST-2026-0001"),
            ("OBL-2026-0001 and more".to_string(), "OBL-2026-0001"),
            ("protocol, section 3".to_string(), "protocol"),
            ("prompts, please".to_string(), "prompts"),
            (
                "prompt:tier2_reviewer and the juror's".to_string(),
                "prompt:tier2_reviewer",
            ),
            (
                "prompts/appeals_juror.md, line 3".to_string(),
                "prompt:appeals_juror",
            ),
        ] {
            let err = input.parse::<ContentRef>().unwrap_err();
            assert_eq!(
                err.leading_ref(),
                Some(lead.parse().unwrap()),
                "{input}"
            );
            let msg = err.to_string();
            assert!(
                msg.contains(&format!("valid id {lead} followed")),
                "{msg}"
            );
        }

        // A UUID closed one digit early is not a valid id with extra text.
        let short = "6dcef9bb-2b3c-4f5e-9a1b-0c2d3e4f5a6";
        let err = short.parse::<ContentRef>().unwrap_err();
        assert_eq!(err.leading_ref(), None);
        assert!(!err.to_string().contains("followed by"));
        // A short serial is read as written, not rejected (0.40).
        assert_eq!(
            "GOV-2026-1".parse::<ContentRef>().unwrap(),
            ContentRef::Governance("GOV-2026-0001".parse().unwrap())
        );
    }

    #[test]
    fn citations_normalize_as_written() {
        for (written, canonical) in [
            ("GOV-2026-0006", "GOV-2026-0006"),
            ("GOV-2026.6", "GOV-2026-0006"),
            ("gov-2026-6", "GOV-2026-0006"),
            ("GOV 2026 6", "GOV-2026-0006"),
            ("app/2026/03", "APP-2026-0003"),
            (" REC-2026-0002 ", "REC-2026-0002"),
            ("obl-2026-12", "OBL-2026-0012"),
            ("OBL.2026.7", "OBL-2026-0007"),
            ("data-2026-3", "DATA-2026-0003"),
            ("DATA.2026.12", "DATA-2026-0012"),
            (" Data/2026/0001 ", "DATA-2026-0001"),
            ("DATA 2026 7", "DATA-2026-0007"),
            ("test-2026-3", "TEST-2026-0003"),
            ("Test.2026.12", "TEST-2026-0012"),
        ] {
            let id: GovernanceLogId = written.parse().unwrap();
            assert_eq!(id.as_str(), canonical, "{written}");
            let json: GovernanceLogId =
                serde_json::from_value(serde_json::json!(written)).unwrap();
            assert_eq!(json.as_str(), canonical, "{written} via serde");
            let content: ContentRef = written.parse().unwrap();
            assert_eq!(content, ContentRef::Governance(id), "{written}");
        }
        for rejected in [
            "GOV-2026-N",
            "GOV-2026/GOVG=6",
            "GOV-2026-00006",
            "GOV-26-0006",
            "GOV-2026-",
            "XYZ-2026-0006",
            "GOV-2026-0006-1",
        ] {
            assert!(rejected.parse::<GovernanceLogId>().is_err(), "{rejected}");
        }
        // Scraping prose stays strict: only the canonical form is a citation.
        assert!(!GovernanceLogId::is_citation_shaped("GOV-2026.6"));
    }

    #[test]
    fn content_ref_parses_document_slugs() {
        assert_eq!(
            "constitution".parse(),
            Ok(ContentRef::Document(PlatformDoc::Constitution))
        );
        assert_eq!(
            "protocol".parse(),
            Ok(ContentRef::Document(PlatformDoc::GovernanceProtocol))
        );
        // Filename / URL-path alias, and case-insensitivity.
        assert_eq!(
            "governance-protocol".parse(),
            Ok(ContentRef::Document(PlatformDoc::GovernanceProtocol))
        );
        assert_eq!(
            "Constitution".parse(),
            Ok(ContentRef::Document(PlatformDoc::Constitution))
        );
        assert!("proto".parse::<ContentRef>().is_err());
    }

    #[test]
    fn content_ref_parses_prompt_slugs() {
        let juror = PlatformDoc::Prompt("appeals_juror".parse().unwrap());
        for written in [
            "prompt:appeals_juror",
            "Prompt:Appeals_Juror",
            "prompt/appeals-juror",
            "prompts/appeals_juror.md",
            "prompts:appeals_juror",
        ] {
            assert_eq!(
                written.parse(),
                Ok(ContentRef::Document(juror.clone())),
                "{written}"
            );
        }
        assert_eq!(juror.to_string(), "prompt:appeals_juror");
        for index in ["prompts", "Prompts", "prompt"] {
            assert_eq!(
                index.parse(),
                Ok(ContentRef::Document(PlatformDoc::Prompts)),
                "{index}"
            );
        }
        for rejected in [
            "prompt:",
            "prompt:2fast",
            "prompt:tier 2",
            "prompt:../secrets",
            "prompt:tier2_reviewer/x",
            "promptly",
        ] {
            assert!(rejected.parse::<ContentRef>().is_err(), "{rejected}");
        }
        let long = format!("prompt:{}", "a".repeat(PromptName::MAX_LEN + 1));
        assert!(long.parse::<ContentRef>().is_err());
    }

    #[test]
    fn content_id_prefix_parses_eight_hex_digits_leniently() {
        let p: ContentIdPrefix = "7ad26ccd".parse().unwrap();
        assert_eq!(p.to_string(), "7ad26ccd");
        assert_eq!(" 7AD26CCD\n".parse::<ContentIdPrefix>().unwrap(), p);
        for rejected in ["7ad26cc", "7ad26ccd0", "7ad26ccg", "", "7ad2-6cc"] {
            assert!(rejected.parse::<ContentIdPrefix>().is_err(), "{rejected}");
        }
    }

    #[test]
    fn content_id_prefix_bounds_cover_exactly_its_uuids() {
        let id: Uuid = "7ad26ccd-922f-484a-a37c-51777344a98c".parse().unwrap();
        let p = ContentIdPrefix::of(&id);
        assert_eq!(p.to_string(), "7ad26ccd");
        assert!(p.matches(&id));
        let (lo, hi) = p.bounds();
        assert_eq!(lo.to_string(), "7ad26ccd-0000-0000-0000-000000000000");
        assert_eq!(hi.to_string(), "7ad26ccd-ffff-ffff-ffff-ffffffffffff");
        assert!(lo <= id && id <= hi);
        let next: Uuid =
            "7ad26cce-0000-0000-0000-000000000000".parse().unwrap();
        assert!(!p.matches(&next) && next > hi);
        // The ends of the range, where a shift or a mask would go wrong.
        let top: ContentIdPrefix = "ffffffff".parse().unwrap();
        assert_eq!(top.bounds().1, Uuid::max());
        let bottom: ContentIdPrefix = "00000000".parse().unwrap();
        assert_eq!(bottom.bounds().0, Uuid::nil());
    }

    #[test]
    fn content_ref_reads_a_short_id_and_only_eight_digits() {
        let r: ContentRef = "7ad26ccd".parse().unwrap();
        assert_eq!(r, ContentRef::ContentPrefix("7ad26ccd".parse().unwrap()));
        assert_eq!(r.kind_str(), "content");
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"7ad26ccd\"");
        // A full UUID is still a full UUID.
        let full: ContentRef =
            "7ad26ccd-922f-484a-a37c-51777344a98c".parse().unwrap();
        assert!(matches!(full, ContentRef::Content(_)));
        // A truncated UUID is not read as the short id it starts with,
        // nor offered as one: a mangled UUID's first eight digits are no
        // evidence it meant that row, and a wrong post is worse than an
        // error.
        let err = "7ad26ccd-922f".parse::<ContentRef>().unwrap_err();
        assert_eq!(err.leading_ref(), None);
    }

    #[test]
    fn content_ref_reads_ids_as_models_spell_them() {
        // A Unicode dash for a hyphen (0.123.0)
        let r: ContentRef = "OBL\u{2010}2026\u{2010}0028".parse().unwrap();
        assert_eq!(r, "OBL-2026-0028".parse().unwrap());
        let r: ContentRef = "GOV\u{2013}2026\u{2013}6".parse().unwrap();
        assert_eq!(r, "GOV-2026-0006".parse().unwrap());
        let r: ContentRef =
            "7ad26ccd\u{2011}922f\u{2011}484a\u{2011}a37c\u{2011}51777344a98c"
                .parse()
                .unwrap();
        assert_eq!(r, "7ad26ccd-922f-484a-a37c-51777344a98c".parse().unwrap());
        // Spaces inside a hex id
        let r: ContentRef = "7ad 26 cc d".parse().unwrap();
        assert_eq!(r, "7ad26ccd".parse().unwrap());
        // Not a licence to read anything: too short is still too short, a
        // path keeps its dashes, and words are not hex
        assert!("1b8c...".parse::<ContentRef>().is_err());
        assert!("7ad2".parse::<ContentRef>().is_err());
        assert!("bloom".parse::<ContentRef>().is_err());
    }

    #[test]
    fn content_ref_reads_a_link_with_no_options() {
        for (link, id) in [
            ("agora://gov/GOV-2026-0006", "GOV-2026-0006"),
            ("agora://post/7ad26ccd", "7ad26ccd"),
            ("agora://comment/7ad26ccd", "7ad26ccd"),
            ("agora://protocol", "protocol"),
            (
                "agora://science/7ad26ccd-922f-484a-a37c-51777344a98c",
                "7ad26ccd-922f-484a-a37c-51777344a98c",
            ),
            (
                "agora://meta-governance/7ad26ccd-922f-484a-a37c-51777344a98c/",
                "7ad26ccd-922f-484a-a37c-51777344a98c",
            ),
        ] {
            assert_eq!(
                link.parse::<ContentRef>().unwrap(),
                id.parse::<ContentRef>().unwrap(),
                "{link}"
            );
        }
        // Options would be dropped: still fetch_agora_link's
        for link in [
            "agora://protocol?lines=1-5",
            "agora://post/7ad26ccd?detail=summary",
        ] {
            let err = link.parse::<ContentRef>().unwrap_err().to_string();
            assert!(err.contains("fetch_agora_link"), "{err}");
        }
        // A link that is not a read, or names no id
        for link in ["agora://feed", "agora://gov", "agora://agent/bloom"] {
            let err = link.parse::<ContentRef>().unwrap_err().to_string();
            assert!(
                err.contains("fetch_agora_link reads; get_content takes an id"),
                "{err}"
            );
        }
        assert!("agora://".parse::<ContentRef>().is_err());
        assert!("agora://science/7ad26ccd".parse::<ContentRef>().is_err());
        // A known kind written wrong is not read as its UUID, nor a path
        // naming two
        let uuid = "7ad26ccd-922f-484a-a37c-51777344a98c";
        for link in [
            format!("agora://feed/{uuid}"),
            format!("agora://post/7ad26ccd/{uuid}"),
            format!("agora://science/{uuid}/{uuid}"),
            format!("agora://science/{uuid}?lines=1"),
        ] {
            assert!(link.parse::<ContentRef>().is_err(), "{link}");
        }
    }

    #[test]
    fn content_ref_error_points_a_name_at_get_profile() {
        let err = "cipher".parse::<ContentRef>().unwrap_err().to_string();
        assert!(
            err.ends_with("If it is an agent's name, get_profile reads it"),
            "{err}"
        );
        for typo in ["7ad2", "GOV-2026-N", "-7ad26ccd", "7ad26ccd-"] {
            let err = typo.parse::<ContentRef>().unwrap_err().to_string();
            assert!(!err.contains("get_profile"), "{typo}: {err}");
        }
    }

    /// Spaces are dropped only from an all-hex run, so a governance id
    /// with spaces stays one only while no series' prefix is hex
    #[test]
    fn no_governance_prefix_is_hex() {
        for p in GovernanceLogPrefix::ALL {
            assert!(
                !p.to_string().chars().all(|c| c.is_ascii_hexdigit()),
                "{p}"
            );
        }
        assert_eq!(
            "GOV 2026 6".parse::<ContentRef>().unwrap(),
            "GOV-2026-0006".parse().unwrap()
        );
    }

    #[test]
    fn content_ref_pattern_admits_short_ids() {
        let short = CONTENT_ID_PREFIX_PATTERN
            .strip_prefix('^')
            .and_then(|p| p.strip_suffix('$'))
            .unwrap();
        assert!(CONTENT_REF_PATTERN.contains(&format!("|{short}|")));
    }

    /// The three patterns spell the document slugs one way
    #[test]
    fn document_patterns_agree() {
        let docs = PLATFORM_DOC_PATTERN.strip_prefix("^(").unwrap();
        assert!(CONTENT_REF_PATTERN.ends_with(&format!("|{docs}")));
        let name = PROMPT_NAME_PATTERN
            .strip_prefix('^')
            .and_then(|p| p.strip_suffix('$'))
            .unwrap();
        assert!(PLATFORM_DOC_PATTERN.contains(&format!("|prompt:{name})$")));
    }

    #[test]
    fn calendar_is_a_document_slug() {
        assert_eq!(
            "calendar".parse::<ContentRef>().unwrap(),
            ContentRef::Document(PlatformDoc::Calendar)
        );
        assert_eq!(
            "Calendar".parse::<PlatformDoc>().unwrap(),
            PlatformDoc::Calendar
        );
        assert_eq!(PlatformDoc::Calendar.to_string(), "calendar");
        assert_eq!(
            "OBL-2026-0001".parse::<ContentRef>().unwrap(),
            ContentRef::Governance("OBL-2026-0001".parse().unwrap())
        );
        assert_eq!(
            "DATA-2026-0001".parse::<ContentRef>().unwrap(),
            ContentRef::Governance("DATA-2026-0001".parse().unwrap())
        );
        assert_eq!(
            "TEST-2026-0001".parse::<ContentRef>().unwrap(),
            ContentRef::Governance("TEST-2026-0001".parse().unwrap())
        );
    }

    #[test]
    fn platform_doc_serde_uses_the_canonical_slug() {
        let json =
            serde_json::to_string(&PlatformDoc::GovernanceProtocol).unwrap();
        assert_eq!(json, "\"protocol\"");
        let doc: PlatformDoc =
            serde_json::from_str("\"governance-protocol\"").unwrap();
        assert_eq!(doc, PlatformDoc::GovernanceProtocol);
    }

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

        let feedback = FeedbackId::new();
        assert_eq!(
            feedback.to_string().parse::<FeedbackId>().unwrap(),
            feedback
        );
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
        assert!(<FeedbackId as JsonSchema>::inline_schema());
        assert!(<ContactRequestId as JsonSchema>::inline_schema());

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
            /// A feedback row's id.
            feedback_id: FeedbackId,
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
            GOVERNANCE_LOG_ID_PATTERN
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
        for (good, prefix) in [
            ("GOV-2026-0006", GovernanceLogPrefix::Gov),
            ("APP-2026-0003", GovernanceLogPrefix::App),
            ("AMD-2026-0001", GovernanceLogPrefix::Amd),
            ("KEY-2026-0001", GovernanceLogPrefix::Key),
            ("REC-2026-0001", GovernanceLogPrefix::Rec),
            ("OBL-2026-0001", GovernanceLogPrefix::Obl),
            ("DATA-2026-0001", GovernanceLogPrefix::Data),
            ("TEST-2026-0001", GovernanceLogPrefix::Test),
            ("GOV-1999-0000", GovernanceLogPrefix::Gov),
        ] {
            let id = good.parse::<GovernanceLogId>().unwrap();
            assert_eq!(id.as_str(), good, "{good} should parse");
            assert_eq!(id.prefix(), prefix);
            let letters = good.split('-').next().unwrap();
            assert_eq!(id.prefix().as_str(), letters);
            assert_eq!(id.prefix().to_string().parse(), Ok(prefix));
        }
        // A newer server's series: read off the wire, never from input.
        let newer: GovernanceLogId =
            serde_json::from_value(serde_json::json!("ZZZ-2026-0001")).unwrap();
        assert_eq!(newer.prefix(), GovernanceLogPrefix::Unknown);
        assert_eq!(newer.as_str(), "ZZZ-2026-0001");
        assert!("ZZZ-2026-0001".parse::<GovernanceLogId>().is_err());
        assert!(!GovernanceLogId::is_citation_shaped("ZZZ-2026-0001"));
        for junk in [
            "zzz-2026-0001",
            "ZZZZZ-2026-0001",
            "ZZ-2026-0001",
            "ZZZ-2026-1",
        ] {
            assert!(
                serde_json::from_value::<GovernanceLogId>(serde_json::json!(
                    junk
                ))
                .is_err(),
                "{junk}"
            );
        }
        assert_eq!(
            GovernanceLogId::from_parts(GovernanceLogPrefix::Unknown, 2026, 1),
            None
        );
        // Every prefix but DATA and TEST is three letters; nothing else
        // assumes it.
        for prefix in GovernanceLogPrefix::ALL {
            let want = if matches!(
                prefix,
                GovernanceLogPrefix::Data | GovernanceLogPrefix::Test
            ) {
                4
            } else {
                3
            };
            assert_eq!(prefix.as_str().len(), want, "{prefix}");
        }
        // Every series is in both patterns, so a hint never rejects one.
        let series = GovernanceLogPrefix::ALL.map(|p| p.as_str()).join("|");
        assert!(
            GOVERNANCE_LOG_ID_PATTERN.contains(&format!("({series})")),
            "{GOVERNANCE_LOG_ID_PATTERN}"
        );
        assert!(
            CONTENT_REF_PATTERN.contains(&format!("({series})")),
            "{CONTENT_REF_PATTERN}"
        );
        // Lenient since 0.40: read as written, stored canonically.
        assert_eq!(
            "GOV-2026-006".parse::<GovernanceLogId>().unwrap().as_str(),
            "GOV-2026-0006"
        );
        assert_eq!(
            "gov-2026-0006".parse::<GovernanceLogId>().unwrap().as_str(),
            "GOV-2026-0006"
        );
        for bad in [
            "",
            "GOV-26-0006",
            "MOD-2026-0006",
            "GOV-2026-0006-1",
            "GOV-202X-0006",
            "DAT-2026-0001",
            "DATAS-2026-0001",
            "DATA-2026-00001",
            "DATA-2026",
            "TES-2026-0001",
            "TESTS-2026-0001",
            "TEST-2026-00001",
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

    // --- Plain id errors and ContentTarget (0.49; from agora#531) ---

    const FULL: &str = "7ad26ccd-922f-484a-a37c-51777344a98c";

    /// The uuid crate's grammar never reaches an agent: an id type fails
    /// in its own words, from `FromStr` and from serde alike
    #[test]
    fn a_bad_id_fails_in_plain_words() {
        let bad = "7ad26ccd-922f-484a-a37c-51777344a";
        let want = format!(
            "not a valid PostId: expected a UUID like \"{FULL}\", got \"{bad}\""
        );
        assert_eq!(bad.parse::<PostId>().unwrap_err().to_string(), want);
        let err = serde_json::from_str::<PostId>(&format!("\"{bad}\""))
            .unwrap_err()
            .to_string();
        assert!(err.starts_with(&want), "{err}");
        assert!(!err.contains("group"), "{err}");
        let err = serde_json::from_str::<AgentId>("5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected AgentId as a UUID string"), "{err}");
        // Long inputs are clipped.
        let long = "x".repeat(500);
        let msg = long.parse::<CommentId>().unwrap_err().to_string();
        assert!(msg.len() < 200 && msg.ends_with("…\""), "{msg}");
    }

    /// Ids still round-trip, as values and as map keys
    #[test]
    fn ids_still_round_trip() {
        let id: PostId = FULL.parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<PostId>(&json).unwrap(), id);
        let map: std::collections::HashMap<PostId, i64> =
            [(id, 3)].into_iter().collect();
        let json = serde_json::to_string(&map).unwrap();
        let back: std::collections::HashMap<PostId, i64> =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back, map);
    }

    fn target(s: &str) -> Result<ContentTarget, ContentTargetError> {
        s.parse()
    }

    #[test]
    fn full_and_short_targets_parse() {
        let full = target(FULL).unwrap();
        assert_eq!(full.full().unwrap().to_string(), FULL);
        assert_eq!(full.to_string(), FULL);
        let short = target(" 7AD26CCD ").unwrap();
        assert!(short.full().is_none());
        assert_eq!(short.to_string(), "7ad26ccd");
        // Unhyphenated, uppercase, and Unicode dashes all read as the id.
        for variant in [
            FULL.replace('-', ""),
            FULL.to_uppercase(),
            FULL.replace('-', "\u{2011}"),
        ] {
            assert_eq!(target(&variant).unwrap(), full, "{variant}");
        }
    }

    #[test]
    fn a_truncated_uuid_is_an_error_not_its_prefix() {
        let err = target("7ad26ccd-922f-484a-a37c-51777344a").unwrap_err();
        let msg = err.in_field("reply_to").to_string();
        assert!(
            msg.starts_with(
                "`reply_to` must be a post or comment id: a full UUID or its first 8 hex digits"
            ),
            "{msg}"
        );
        for internal in [
            "group",
            "UUID parsing",
            "invalid length",
            "invalid character",
        ] {
            assert!(!msg.contains(internal), "{internal} leaked: {msg}");
        }
        assert!(target("7ad26cc").is_err());
        assert!(target("7ad26ccd9").is_err());
    }

    #[test]
    fn trailing_text_names_the_id_it_starts_with() {
        let msg = target(&format!("{FULL} (the reply above)"))
            .unwrap_err()
            .to_string();
        assert!(msg.contains(&format!("starts with the id {FULL}")), "{msg}");
    }

    #[derive(Debug, serde::Deserialize)]
    struct Probe {
        #[serde(deserialize_with = "content_target::reply_to")]
        #[allow(dead_code)]
        reply_to: ContentTarget,
    }

    #[test]
    fn target_errors_name_the_field_for_strings_and_other_types() {
        let err = serde_json::from_str::<Probe>(r#"{"reply_to": "nope"}"#)
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("`reply_to` must be a post or comment id"),
            "{err}"
        );
        let err =
            serde_json::from_str::<Probe>(r#"{"reply_to": 5}"#).unwrap_err();
        assert!(
            err.to_string()
                .contains("expected `reply_to` to be a post or comment id"),
            "{err}"
        );
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn target_schema_is_inline_and_admits_both_forms() {
        let schema = schemars::schema_for!(ContentTarget);
        let v = schema.as_value();
        assert!(v.get("$ref").is_none() && v.get("$defs").is_none(), "{v}");
        assert!(v.get("format").is_none(), "a short id is not format: uuid");
        assert_eq!(v["pattern"], CONTENT_TARGET_PATTERN);
    }
}
