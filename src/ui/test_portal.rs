//! A [`Portal`] for tests: answers what it is told, and keeps every
//! request it was sent (`cfg(test)` only)

use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Duration};

use chrono::{DateTime, Utc};

use futures::{
    FutureExt,
    channel::oneshot,
    future::{LocalBoxFuture, Shared},
};

use super::session::{ApiError, Portal, TapFailed};
use crate::{
    enums::MessageEncryption,
    ids::{CommentId, MessageId, PostId},
    operator::{
        CheckWindowStatus, PortalCommentRequest, PortalFriendshipRequest,
        PortalMarkReadRequest, PortalMarkReadResponse, PortalMessageRequest,
        PortalMessageTap, PortalPostRequest, PortalSessionResponse,
        PortalWindowOpenRequest, PortalWindowTap,
    },
    responses::{
        CommunityResponse, FriendsResponse, InboxResponse, PostCreated,
        SendMessageResponse, StatusResponse, WriteAck,
    },
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
    /// The inbox's answers, in order; past them, the last one again (an
    /// empty inbox when there was none)
    pub inbox_answers: VecDeque<Result<InboxResponse, ApiError>>,
    pub inbox_calls: usize,
    pub friends_answers: VecDeque<Result<FriendsResponse, ApiError>>,
    pub friends_calls: usize,
    /// Answers to mark-read; past them, everything named is marked
    pub mark_answers: VecDeque<Result<PortalMarkReadResponse, ApiError>>,
    pub marks: Vec<PortalMarkReadRequest>,
    /// Answers to friendship actions; past them, `requested`
    pub friendship_answers: VecDeque<Result<StatusResponse, ApiError>>,
    pub friendships: Vec<PortalFriendshipRequest>,
    /// Answers to the tap call; past them, a tap with [`tap_options`]
    pub tap_answers: VecDeque<Result<PortalMessageTap, ApiError>>,
    pub taps: Vec<PortalMessageRequest>,
    /// Answers to a send; past them, a 201 for [`sent_message`]
    pub message_answers: VecDeque<Result<SendMessageResponse, ApiError>>,
    pub messages: Vec<PortalMessageRequest>,
    /// The browser's answers to a passkey prompt; past them,
    /// [`assertion`]
    pub passkey_answers: VecDeque<Result<serde_json::Value, TapFailed>>,
    /// The options each prompt was opened with
    pub prompts: Vec<serde_json::Value>,
    /// Answers to the window's tap call; past them, [`window_options`]
    pub window_tap_answers: VecDeque<Result<PortalWindowTap, ApiError>>,
    pub window_taps: usize,
    /// Answers to the window's open; past them, the fixtures' window. An
    /// open past them also makes the bootstrap answer signed in
    pub open_answers: VecDeque<Result<CheckWindowStatus, ApiError>>,
    pub opens: Vec<PortalWindowOpenRequest>,
    /// The browser's clock; the fixtures' window's opening when `None`
    pub now: Option<DateTime<Utc>>,
    /// Each wait asked for, in order, with the sender that ends it
    pub waits: Vec<(Duration, Option<oneshot::Sender<()>>)>,
    /// The page's listeners for being shown again, in order
    pub shown: Vec<Option<oneshot::Sender<()>>>,
    /// The page's own path; `/agora/feed` when `None`
    pub here: Option<String>,
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

/// The tap options a tap call past the scripted answers gives
pub fn tap_options() -> serde_json::Value {
    serde_json::json!({"challenge": "Y2hhbGxlbmdl", "rpId": "localhost"})
}

/// The window's tap options past the scripted answers
pub fn window_options() -> serde_json::Value {
    serde_json::json!({"challenge": "d2luZG93", "rpId": "localhost"})
}

/// The bootstrap's refusal outside a check window
pub fn check_needed() -> ApiError {
    ApiError::Forbidden(
        crate::responses::ErrorResponse::coded(
            crate::errors::RefusalCode::CheckNeeded,
            "this needs a recent passkey check",
        )
        .into(),
    )
}

/// The browser's answer a prompt past the scripted answers gives
pub fn assertion() -> serde_json::Value {
    serde_json::json!({"id": "a2V5", "type": "public-key"})
}

/// The id a message past the scripted answers is sent under
pub fn sent_message() -> MessageId {
    MessageId::from(uuid::Uuid::from_u128(
        0x9_0000_0000_4000_8000_0000_0000_0003,
    ))
}

/// The answer to a send of `request`
pub fn message_sent(request: &PortalMessageRequest) -> SendMessageResponse {
    let _ = request;
    SendMessageResponse {
        id: sent_message(),
        encryption: MessageEncryption::Server,
        sent_at: Some(crate::fixtures::at()),
        notice: None,
        warning: None,
    }
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

    /// Signed in, outside the check window
    pub fn check_needed() -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().session = Some(Err(check_needed()));
        fake
    }

    /// Ends the `n`th wait asked for (a timer firing)
    pub fn fire(&self, n: usize) {
        let sender = self.0.borrow_mut().waits[n].1.take();
        sender.expect("not fired before").send(()).unwrap();
    }

    /// Fires the newest wait asked for (the current probe's timer)
    pub fn fire_last(&self) {
        let n = self.0.borrow().waits.len() - 1;
        self.fire(n);
    }

    /// The page is shown again (a tab brought back)
    pub fn show_again(&self) {
        let sender = self
            .0
            .borrow_mut()
            .shown
            .iter_mut()
            .rev()
            .find_map(Option::take);
        sender.expect("the page listens").send(()).unwrap();
    }

    /// No one is signed in
    pub fn signed_out() -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().session =
            Some(Err(ApiError::SignedOut("no session".into())));
        fake
    }
}

/// A copy of a scripted answer whose type isn't `Clone`, through its JSON
fn again<T: serde::Serialize + serde::de::DeserializeOwned>(
    answer: &Result<T, ApiError>,
) -> Result<T, ApiError> {
    match answer {
        Ok(t) => {
            Ok(serde_json::from_value(serde_json::to_value(t).unwrap())
                .unwrap())
        }
        Err(e) => Err(e.clone()),
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
        Some(
            self.0
                .borrow()
                .here
                .clone()
                .unwrap_or_else(|| "/agora/feed".into()),
        )
    }

    fn drafts_unsent(&self, unsent: bool) {
        self.0.borrow_mut().unsent.push(unsent);
    }

    fn inbox(
        &self,
    ) -> LocalBoxFuture<'static, Result<InboxResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.inbox_calls += 1;
        let answer = match fake.inbox_answers.len() {
            0 => Ok(crate::fixtures::inbox(Vec::new())),
            1 => again(&fake.inbox_answers[0]),
            _ => fake.inbox_answers.pop_front().unwrap(),
        };
        ready(answer)
    }

    fn friends(
        &self,
    ) -> LocalBoxFuture<'static, Result<FriendsResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.friends_calls += 1;
        let answer = match fake.friends_answers.len() {
            0 => Ok(FriendsResponse::default()),
            1 => again(&fake.friends_answers[0]),
            _ => fake.friends_answers.pop_front().unwrap(),
        };
        ready(answer)
    }

    fn mark_read(
        &self,
        _: &PortalSessionResponse,
        request: &PortalMarkReadRequest,
    ) -> LocalBoxFuture<'static, Result<PortalMarkReadResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.marks.push(request.clone());
        let gate = fake.gate.clone();
        let all = request.payload.messages.clone();
        gated(
            gate,
            fake.mark_answers.pop_front().unwrap_or(Ok(
                PortalMarkReadResponse {
                    marked: all,
                    not_marked: Vec::new(),
                },
            )),
        )
    }

    fn friendship(
        &self,
        _: &PortalSessionResponse,
        request: &PortalFriendshipRequest,
    ) -> LocalBoxFuture<'static, Result<StatusResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.friendships.push(request.clone());
        let gate = fake.gate.clone();
        gated(
            gate,
            fake.friendship_answers
                .pop_front()
                .unwrap_or_else(|| Ok(StatusResponse::new("requested"))),
        )
    }

    fn message_tap(
        &self,
        _: &PortalSessionResponse,
        request: &PortalMessageRequest,
    ) -> LocalBoxFuture<'static, Result<PortalMessageTap, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.taps.push(request.clone());
        let gate = fake.gate.clone();
        gated(
            gate,
            fake.tap_answers.pop_front().unwrap_or_else(|| {
                Ok(PortalMessageTap {
                    tap: Some(tap_options()),
                })
            }),
        )
    }

    fn send_message(
        &self,
        _: &PortalSessionResponse,
        request: &PortalMessageRequest,
    ) -> LocalBoxFuture<'static, Result<SendMessageResponse, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.messages.push(request.clone());
        let gate = fake.gate.clone();
        let sent = message_sent(request);
        gated(gate, fake.message_answers.pop_front().unwrap_or(Ok(sent)))
    }

    fn passkey(
        &self,
        options: &serde_json::Value,
    ) -> LocalBoxFuture<'static, Result<serde_json::Value, TapFailed>> {
        let mut fake = self.0.borrow_mut();
        fake.prompts.push(options.clone());
        ready(
            fake.passkey_answers
                .pop_front()
                .unwrap_or_else(|| Ok(assertion())),
        )
    }

    fn window_tap(
        &self,
    ) -> LocalBoxFuture<'static, Result<PortalWindowTap, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.window_taps += 1;
        let gate = fake.gate.clone();
        gated(
            gate,
            fake.window_tap_answers.pop_front().unwrap_or_else(|| {
                Ok(PortalWindowTap {
                    tap: window_options(),
                })
            }),
        )
    }

    fn open_window(
        &self,
        request: &PortalWindowOpenRequest,
    ) -> LocalBoxFuture<'static, Result<CheckWindowStatus, ApiError>> {
        let mut fake = self.0.borrow_mut();
        fake.opens.push(request.clone());
        let gate = fake.gate.clone();
        let answer = fake.open_answers.pop_front().unwrap_or_else(|| {
            fake.session = Some(Ok(crate::fixtures::portal_session(true)));
            Ok(crate::fixtures::portal_session(true).window)
        });
        gated(gate, answer)
    }

    fn now(&self) -> DateTime<Utc> {
        self.0
            .borrow()
            .now
            .unwrap_or(crate::fixtures::portal_session(true).window.opened_at)
    }

    fn shown_again(&self) -> LocalBoxFuture<'static, ()> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().shown.push(Some(tx));
        async move {
            if rx.await.is_err() {
                futures::future::pending::<()>().await;
            }
        }
        .boxed_local()
    }

    fn after(&self, delay: Duration) -> LocalBoxFuture<'static, ()> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().waits.push((delay, Some(tx)));
        async move {
            // Dropped unfired (the fake gone): the wait never ends
            if rx.await.is_err() {
                futures::future::pending::<()>().await;
            }
        }
        .boxed_local()
    }
}
