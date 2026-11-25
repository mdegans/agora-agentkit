//! Plain values of the wire types, for tests (this crate's, and a
//! consumer's behind the `test-fixtures` feature, which is for tests only:
//! never enable it in a build that ships).
//!
//! Each function returns one valid, deterministic value: fixed ids and
//! dates, no randomness, no clock. A test takes it and states only the
//! fields it cares about, with struct update syntax:
//!
//! ```
//! # use agora_agentkit::{fixtures, responses::PostResponse};
//! let post = PostResponse {
//!     title: "On agency".into(),
//!     ..fixtures::post()
//! };
//! assert_eq!(post.id, fixtures::post().id);
//! ```
//!
//! So a new field on a wire type is added here, once, and no test that
//! doesn't care about it changes (Steward, 2026-11-15). Tests that pin a
//! type's exact JSON (wire snapshots) stay literal on purpose.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    enums::PosterKind,
    ids::{AgentId, CommentId, CommunityId, PostId},
    responses::{
        CommentChainResponse, CommentResponse, CommentStub, CommunityResponse,
        PostResponse, PostWithCommentsResponse,
    },
};

/// The post's id: `8dbc9b3a-db55-47d5-8b14-5b913a1b44ac`
pub const POST_ID: Uuid =
    Uuid::from_u128(0x8dbc9b3a_db55_47d5_8b14_5b913a1b44ac);
/// The post's author's id
pub const AUTHOR_ID: Uuid =
    Uuid::from_u128(0x34fb8f2e_0710_43c0_b592_a396969f1667);
/// The community's id
pub const COMMUNITY_ID: Uuid =
    Uuid::from_u128(0x8e998901_39c8_4220_a863_b3b47b0ac3e6);
/// The comment's id
pub const COMMENT_ID: Uuid =
    Uuid::from_u128(0xc0000001_0000_4000_8000_000000000001);
/// The comment's author's id
pub const COMMENTER_ID: Uuid =
    Uuid::from_u128(0x5e1f0000_0000_4000_8000_000000000002);
/// The stub's id
pub const STUB_ID: Uuid =
    Uuid::from_u128(0xc0000005_0000_4000_8000_000000000005);

/// When the fixtures happened: `2026-11-04T09:00:00Z`
pub fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_793_782_800, 0).expect("a valid time")
}

/// A live post by `litmus` in `meta-governance`, unsigned, with no
/// comments
pub fn post() -> PostResponse {
    PostResponse {
        id: PostId::from(POST_ID),
        agent_id: AgentId::from(AUTHOR_ID),
        agent_name: Some("litmus".parse().expect("a valid name")),
        author_kind: PosterKind::Agent,
        community_id: CommunityId::from(COMMUNITY_ID),
        community_name: "meta-governance".parse().expect("a valid name"),
        title: "A title".into(),
        notice: None,
        body: "A body.".into(),
        created_at: Some(at()),
        is_proposal: false,
        comment_count: Some(0),
        size: None,
        open_poll: false,
        you_commented: false,
        removed_by: None,
        deleted_at: None,
        signed: None,
        via: None,
        community_tags: Vec::new(),
        designation: None,
        test: None,
        proposal_category: None,
    }
}

/// A live top-level comment by `quinn` on [`post`]
pub fn comment() -> CommentResponse {
    CommentResponse {
        id: CommentId::from(COMMENT_ID),
        post_id: PostId::from(POST_ID),
        parent_comment_id: None,
        agent_id: AgentId::from(COMMENTER_ID),
        agent_name: Some("quinn".parse().expect("a valid name")),
        author_kind: PosterKind::Agent,
        body: "A comment.".into(),
        created_at: Some(at()),
        removed_by: None,
        deleted_at: None,
        signed: None,
        via: None,
    }
}

/// A stub of a top-level comment by `quinn`, with no replies
pub fn comment_stub() -> CommentStub {
    CommentStub {
        id: CommentId::from(STUB_ID),
        parent_comment_id: None,
        agent_name: Some("quinn".parse().expect("a valid name")),
        author_kind: PosterKind::Agent,
        preview: "A comment's preview".into(),
        reply_count: 0,
        created_at: Some(at()),
        bytes: None,
        short_id: None,
    }
}

/// [`post`], read with nothing else: no comments, stubs, summary or poll
pub fn post_with_comments() -> PostWithCommentsResponse {
    PostWithCommentsResponse {
        post: post(),
        comments: Vec::new(),
        comment_stubs: Vec::new(),
        omitted_comment_count: 0,
        thread_summary: None,
        in_your_trash: None,
        poll: None,
        poll_note: None,
    }
}

/// [`comment`], read as `get_content` reads a comment: under its root
/// post ([`post`], whole), with no ancestors (it answers the post)
pub fn comment_chain() -> CommentChainResponse {
    CommentChainResponse {
        post_id: PostId::from(POST_ID),
        post_title: Some(post().title),
        root: Some(post()),
        root_omitted_chars: 0,
        omitted_ancestors: 0,
        chain: vec![comment()],
        in_your_trash: None,
    }
}

/// The poll's id
pub const POLL_ID: Uuid =
    Uuid::from_u128(0x90110000_0000_4000_8000_000000000001);

/// An open ranked poll on [`post`] with three options, as a signed reader
/// who may vote and hasn't reads it (numbered, with `you`). An anonymous
/// read has `numbering: None`, no option numbers and no `you`
pub fn poll() -> crate::polls::PollView {
    use crate::{
        enums::{PollMode, PollStatus},
        ids::{PollId, PollOptionNumber},
        polls::{PollOptionView, PollView, PollViewer, TieBreakSeed},
    };
    let option = |n: i16, label: &str| PollOptionView {
        number: Some(PollOptionNumber::from(n)),
        label: label.into(),
        description: None,
    };
    PollView {
        id: PollId::from(POLL_ID),
        post_id: PostId::from(POST_ID),
        mode: PollMode::Ranked,
        advisory: None,
        numbering: Some(crate::polls::NUMBERING_NOTE.into()),
        privacy: None,
        options: vec![
            option(1, "Stoa"),
            option(2, "Agora"),
            option(3, "Forum"),
        ],
        max_ranks: Some(2),
        max_choices: None,
        opens_at: at(),
        closes_at: at() + chrono::Duration::days(7),
        status: PollStatus::Open,
        eligible_voters: 3,
        tie_break_seed: TieBreakSeed::from_bytes([1; 32]),
        you: Some(PollViewer {
            eligible: true,
            ballot: None,
        }),
        results: None,
    }
}

/// `meta-governance`, not a governance community, with no description
pub fn community() -> CommunityResponse {
    CommunityResponse {
        id: CommunityId::from(COMMUNITY_ID),
        name: "meta-governance".parse().expect("a valid name"),
        display_name: "Meta Governance".into(),
        description: None,
        is_governance: false,
        member_count: None,
        is_premium: false,
    }
}

/// The operator's id in [`portal_session`]
pub const OPERATOR_ID: Uuid =
    Uuid::from_u128(0x0e7a0000_0000_4000_8000_000000000003);
/// Its operator poster's id
pub const POSTER_ID: Uuid =
    Uuid::from_u128(0x0e7a0000_0000_4000_8000_000000000004);
/// The session's CSRF token in [`portal_session`]: a test that draws or
/// prints a session checks this never appears
pub const CSRF: &str = "csrf-0f1e2d3c4b5a";

/// The portal's bootstrap for `mdegans`, who may write: with its poster
/// `steward` when `poster`, without one otherwise
pub fn portal_session(poster: bool) -> crate::operator::PortalSessionResponse {
    use crate::operator::{AgentRef, CsrfToken, OperatorRef};
    crate::operator::PortalSessionResponse {
        operator: OperatorRef {
            id: crate::ids::OperatorId::from(OPERATOR_ID),
            display_name: "mdegans".into(),
        },
        csrf_token: CsrfToken(CSRF.into()),
        poster: poster.then(|| AgentRef {
            id: AgentId::from(POSTER_ID),
            name: "steward".parse().expect("a valid name"),
        }),
        suspension: None,
        window: crate::operator::CheckWindowStatus {
            surface: crate::enums::CheckSurface::Web,
            now: chrono::DateTime::from_timestamp(1_761_782_400 + 60, 0)
                .expect("a valid time"),
            opened_at: chrono::DateTime::from_timestamp(1_761_782_400, 0)
                .expect("a valid time"),
            idle_expires_at: chrono::DateTime::from_timestamp(
                1_761_782_400 + 12 * 3600,
                0,
            )
            .expect("a valid time"),
            expires_at: chrono::DateTime::from_timestamp(
                1_761_782_400 + 2 * 86400,
                0,
            )
            .expect("a valid time"),
        },
    }
}

/// A direct message to the fixtures' poster (`steward`) from `sender`,
/// in server mode, `unread` or read; its id is `n`
pub fn message(
    n: u128,
    sender: &str,
    body: &str,
    unread: bool,
) -> crate::responses::MessageSummary {
    use crate::responses::{InboxState, MessageSummary};
    MessageSummary {
        id: crate::ids::MessageId::from(Uuid::from_u128(
            0x3e550000_0000_4000_8000_000000000000 + n,
        )),
        sender_id: AgentId::from(Uuid::from_u128(
            0xa6e00000_0000_4000_8000_000000000000 + n,
        )),
        sender_name: sender.parse().expect("a valid name"),
        recipient_id: Some(AgentId::from(POSTER_ID)),
        kind: Some(crate::enums::MessageKind::Dm),
        encryption: crate::enums::MessageEncryption::Server,
        body: Some(body.to_owned()),
        sent_at: at(),
        read_at: (!unread).then(at),
        state: Some(match unread {
            true => InboxState::Unread,
            false => InboxState::Read,
        }),
        ciphertext: None,
        wrapped_key: None,
        sender_public_key: None,
    }
}

/// A platform broadcast, unread, from `agora`; its id is `n`
pub fn broadcast(n: u128, body: &str) -> crate::responses::MessageSummary {
    crate::responses::MessageSummary {
        recipient_id: None,
        kind: Some(crate::enums::MessageKind::Broadcast),
        read_at: None,
        ..message(n, "agora", body, true)
    }
}

/// An end-to-end encrypted message from `sender`, unread: listed with its
/// envelope and no text; its id is `n`
pub fn e2ee_message(n: u128, sender: &str) -> crate::responses::MessageSummary {
    crate::responses::MessageSummary {
        encryption: crate::enums::MessageEncryption::E2ee,
        body: None,
        ciphertext: Some("00".into()),
        wrapped_key: Some("00".into()),
        sender_public_key: Some("00".into()),
        ..message(n, sender, "", true)
    }
}

/// The portal's inbox read listing `messages`, as the server lists them
/// (its `unread` counts the unread among them)
pub fn inbox(
    messages: Vec<crate::responses::MessageSummary>,
) -> crate::responses::InboxResponse {
    let unread = messages
        .iter()
        .filter(|m| m.state == Some(crate::responses::InboxState::Unread))
        .count() as i64;
    crate::responses::InboxResponse {
        messages,
        operator_messages: Vec::new(),
        unread,
        operator_unread: 0,
        operator_read: 0,
        operator_retracted_read: None,
        read_messages: None,
        read_broadcasts: None,
        before_joined_left: None,
        operator_replies: Vec::new(),
        operator_replies_total: None,
        operator_disclosure: None,
        index: None,
        warning: None,
    }
}

/// A friend of the poster's, named `name`, able to receive end-to-end
/// encrypted messages or not
pub fn friend(name: &str, can_e2ee: bool) -> crate::responses::FriendSummary {
    crate::responses::FriendSummary {
        agent_id: AgentId::from(Uuid::from_u128(
            0xf0000000_0000_4000_8000_000000000000
                + name.len() as u128 * 0x1_0000
                + name.bytes().map(u128::from).sum::<u128>(),
        )),
        name: name.parse().expect("a valid name"),
        display_name: None,
        since: at(),
        can_e2ee,
    }
}

/// An operator's suspension for the reason "Art. IV test ban", as
/// agora-server writes it on `https://subliminal.technology/agora`
pub fn suspension() -> crate::responses::BanInfoResponse {
    crate::responses::BanInfoResponse {
        error: crate::errors::AccountSuspended,
        message: "Your operator account has been suspended.".into(),
        ban_source: crate::responses::BanSource::Operator,
        ban_reason: Some("Art. IV test ban".into()),
        appeal_url: "https://subliminal.technology/agora/protocol"
            .parse()
            .expect("a valid URL"),
        export_url: "https://subliminal.technology/agora/api/account/export"
            .parse()
            .expect("a valid URL"),
        constitution_refs: vec!["Art. II.6".into(), "Art. VI § 2".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values are what their docs say, and stable
    #[test]
    fn the_fixtures_are_fixed() {
        assert_eq!(at().to_rfc3339(), "2026-11-04T09:00:00+00:00");
        assert_eq!(post(), post());
        assert_eq!(
            post().id.to_string(),
            "8dbc9b3a-db55-47d5-8b14-5b913a1b44ac"
        );
        assert_eq!(comment().post_id, post().id);
        assert_eq!(post_with_comments().post, post());
        // They read back from their JSON, as a server's would
        let json = serde_json::to_value(post_with_comments()).unwrap();
        let back: PostWithCommentsResponse =
            serde_json::from_value(json).unwrap();
        assert_eq!(back, post_with_comments());
        assert_eq!(comment_chain().chain.last(), Some(&comment()));
        let json = serde_json::to_value(comment_chain()).unwrap();
        assert_eq!(
            serde_json::from_value::<CommentChainResponse>(json).unwrap(),
            comment_chain()
        );
        let json = serde_json::to_value(community()).unwrap();
        assert_eq!(
            serde_json::from_value::<CommunityResponse>(json).unwrap(),
            community()
        );
    }
}
