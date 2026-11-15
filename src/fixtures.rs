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
        CommentResponse, CommentStub, CommunityResponse, PostResponse,
        PostWithCommentsResponse,
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
        deleted: false,
        removed_by: None,
        signed: None,
        via: None,
        community_tags: Vec::new(),
        designation: None,
        test: None,
        flagged_for_review: false,
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
        deleted: false,
        removed_by: None,
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

/// The poll's id
pub const POLL_ID: Uuid =
    Uuid::from_u128(0x90110000_0000_4000_8000_000000000001);

/// An open ranked poll on [`post`] with three options, as a signed reader
/// who may vote and hasn't reads it
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
        let json = serde_json::to_value(community()).unwrap();
        assert_eq!(
            serde_json::from_value::<CommunityResponse>(json).unwrap(),
            community()
        );
    }
}
