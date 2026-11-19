//! A [`Portal`] for tests: answers what it is told, and keeps every
//! request it was sent (`cfg(test)` only)

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use futures::{
    FutureExt,
    channel::oneshot,
    future::{LocalBoxFuture, Shared},
};

use super::session::{ApiError, Portal};
use crate::{
    ids::{CommentId, PostId},
    operator::{
        PortalCommentRequest, PortalPostRequest, PortalSessionResponse,
    },
    responses::{CommunityResponse, PostCreated, WriteAck},
};

/// What the fake answers and what it was asked. Shared (`Rc`), so a test
/// keeps a copy after handing one to `provide`
#[derive(Clone, Default)]
pub struct FakePortal(pub Rc<RefCell<Fake>>);

#[derive(Default)]
pub struct Fake {
    /// The bootstrap's answer; a network failure when `None`
    pub session: Option<Result<PortalSessionResponse, ApiError>>,
    /// How many times the bootstrap was asked for
    pub session_calls: usize,
    /// Answers to posts, in order; a post past them gets a 201 for
    /// [`created_post`]
    pub post_answers: VecDeque<Result<PostCreated, ApiError>>,
    pub comment_answers: VecDeque<Result<WriteAck<CommentId>, ApiError>>,
    pub posts: Vec<PortalPostRequest>,
    pub comments: Vec<PortalCommentRequest>,
    pub communities: Option<Result<Vec<CommunityResponse>, ApiError>>,
    /// When set, every write waits for it before answering (a slow
    /// network): a test releases it by sending, or by dropping the sender
    pub gate: Option<Shared<oneshot::Receiver<()>>>,
    /// What `drafts_unsent` was told, in order
    pub unsent: Vec<bool>,
}

impl FakePortal {
    /// Holds every write's answer until the returned sender is used or
    /// dropped
    pub fn hold(&self) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().gate = Some(rx.shared());
        tx
    }
}

/// `answer`, after `gate` opens when there is one
fn gated<T: 'static>(
    gate: Option<Shared<oneshot::Receiver<()>>>,
    answer: T,
) -> LocalBoxFuture<'static, T> {
    async move {
        if let Some(gate) = gate {
            // Sent or dropped, the gate is open either way
            let _ = gate.await;
        }
        answer
    }
    .boxed_local()
}

/// The id a post past the scripted answers gets
pub fn created_post() -> PostId {
    PostId::from(uuid::Uuid::from_u128(
        0x9_0000_0000_4000_8000_0000_0000_0001,
    ))
}

/// The id a comment past the scripted answers gets
pub fn created_comment() -> CommentId {
    CommentId::from(uuid::Uuid::from_u128(
        0x9_0000_0000_4000_8000_0000_0000_0002,
    ))
}

impl FakePortal {
    /// Signed in as the fixtures' operator, with its poster or without
    pub fn signed_in(poster: bool) -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().session =
            Some(Ok(crate::fixtures::portal_session(poster)));
        fake
    }

    /// No one is signed in
    pub fn signed_out() -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().session =
            Some(Err(ApiError::SignedOut("no session".into())));
        fake
    }
}

fn ready<T: 'static>(value: T) -> LocalBoxFuture<'static, T> {
    futures::future::ready(value).boxed_local()
}

impl Portal for FakePortal {
    fn session(
        &self,
    ) -> LocalBoxFuture<'static, Result<PortalSessionResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.session_calls += 1;
        ready(
            fake.session
                .clone()
                .unwrap_or_else(|| Err(ApiError::Network("offline".into()))),
        )
    }

    fn create_post(
        &self,
        _: &PortalSessionResponse,
        request: &PortalPostRequest,
    ) -> LocalBoxFuture<'static, Result<PostCreated, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.posts.push(request.clone());
        let gate = fake.gate.clone();
        gated(
            gate,
            fake.post_answers.pop_front().unwrap_or_else(|| {
                Ok(PostCreated {
                    ack: WriteAck {
                        id: created_post(),
                        status: "created".into(),
                        verified: false,
                    },
                    designation: None,
                    hint: None,
                    poll: None,
                    poll_note: None,
                })
            }),
        )
    }

    fn create_comment(
        &self,
        _: &PortalSessionResponse,
        request: &PortalCommentRequest,
    ) -> LocalBoxFuture<'static, Result<WriteAck<CommentId>, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.comments.push(request.clone());
        let gate = fake.gate.clone();
        gated(
            gate,
            fake.comment_answers.pop_front().unwrap_or_else(|| {
                Ok(WriteAck {
                    id: created_comment(),
                    status: "created".into(),
                    verified: false,
                })
            }),
        )
    }

    fn communities(
        &self,
    ) -> LocalBoxFuture<'static, Result<Vec<CommunityResponse>, ApiError>> {
        ready(
            self.0
                .borrow()
                .communities
                .clone()
                .unwrap_or_else(|| Ok(vec![crate::fixtures::community()])),
        )
    }

    fn here(&self) -> Option<String> {
        Some("/agora/feed".into())
    }

    fn drafts_unsent(&self, unsent: bool) {
        self.0.borrow_mut().unsent.push(unsent);
    }
}
