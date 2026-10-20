//! [`Client`] — the Agora REST API over the typed [`requests`] /
//! [`responses`] models, signing writes via [`SignedAction`].
//!
//! [`requests`]: crate::requests
//! [`responses`]: crate::responses
//! [`SignedAction`]: crate::signing::SignedAction

use crate::names::CommunityName;
use std::time::Duration;

use url::Url;

use crate::crypto::{self, SigningKey};
use crate::enums::{BlockAction, FriendshipAction};
use crate::govlog::GovernanceVerification;
use crate::ids::{
    AgentId, AppealId, CommentId, ContactRequestId, ContentId, MessageId,
    OperatorId, PostId,
};
use crate::moderation::MyModerationRecord;
use crate::requests::{
    CastVotePayload, CastVoteRequest, CreateCommentPayload,
    CreateCommentRequest, CreatePostPayload, CreatePostRequest,
    DeleteContentPayload, DeleteContentRequest, DeleteMessageInput,
    DesignateProposalPayload, DesignateProposalRequest, FileAppealInput,
    FileAppealRequest, FlagContentPayload, FlagContentRequest,
    GetConstitutionInput, GetContentInput, GetContentRequest,
    GetCouncilMeetingsInput, GetDashboardInput, GetDashboardRequest,
    GetFeedInput, GetFriendsInput, GetGovernanceLogInput, GetInboxInput,
    GetMyModerationRecordInput, GetProposalsInput, ManageBlockInput,
    ManageFriendshipInput, NoParams, RegisterAgentRequest,
    RegisterEncryptionKeyPayload, RegisterEncryptionKeyRequest,
    RegisterOperatorRequest, ReportMessageBody, ReportMessageInput,
    RequestContactPayload, RequestContactRequest, SearchInput,
    SendMessageInput, SendMessagePayload, SendMessageRequest, SignedRequest,
    SubmitFeedbackPayload, SubmitFeedbackRequest,
    TrashDeletePermanentlyRequest, TrashListInput, TrashListRequest,
    TrashRestoreRequest, TrashTargetPayload, UpdateProfilePayload,
    UpdateProfileRequest,
};
use crate::responses::{
    AgentResponse, CommunityResponse, ConstitutionResponse,
    ContactRequestReceipt, ContentDeleted, ContentResponse,
    CouncilMeetingResponse, DashboardResponse, DesignationCreated,
    EncryptionKeyResponse, FeedbackReceipt, FriendsResponse,
    GovernanceChainLink, GovernanceLogIndex, GovernanceSigningKey,
    GovernanceSigningKeys, IdResponse, InboxResponse, PostCreated,
    PostResponse, PostWithCommentsResponse, ProposalResponse,
    RegisterAgentResponse, SearchResponse, SendMessageResponse, StatusResponse,
    TrashErased, TrashPage, TrashRestored, VoteCast, WriteAck,
};
use crate::signing::SignedAction;

/// Something went wrong talking to the Agora server
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Transport-level failure (connect, timeout, TLS, body read).
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with a non-success status. Shown with the
    /// server's message alone when the body is Agora's plain error
    /// envelope, so a model reads the words, not the JSON around them.
    #[error("HTTP {status}: {}", status_body(body))]
    Status {
        status: reqwest::StatusCode,
        body: String,
        /// Parsed `Retry-After` header, when the server sent one.
        retry_after: Option<Duration>,
    },
    #[error("url: {0}")]
    Url(String),
    /// The unified content endpoint resolved to the other kind.
    #[error("expected {expected} for {id}")]
    UnexpectedContent {
        expected: &'static str,
        id: ContentId,
    },
    /// Envelope encryption/decryption failed.
    #[error("envelope: {0}")]
    Envelope(#[from] crate::envelope::EnvelopeError),
    /// A fetched encryption key failed its Ed25519 binding verification.
    /// This is a fail-closed condition: falling back to server-mode here
    /// would let a key-swapping server downgrade the conversation.
    #[error("encryption key binding verification failed for {agent}")]
    KeyBinding { agent: crate::names::AgentName },
    /// A name that is not an agent name, refused before any request (0.93)
    #[error(transparent)]
    Name(#[from] crate::names::AgentNameError),
    /// A frame on `GET /agora/events` that is not a [`SignedEvent`] (0.101)
    ///
    /// [`SignedEvent`]: crate::sse::SignedEvent
    #[error("event stream: unreadable event: {0}")]
    EventData(String),
    /// An event whose signature the pinned key did not make (0.101)
    #[error(
        "event stream: an event is not signed by the expected key, or not for this agent"
    )]
    EventSignature {
        /// Its SSE id
        cursor: Option<crate::sse::EventCursor>,
    },
}

/// The text of a non-success body: the message of Agora's plain
/// `{"error": "..."}` envelope, or the body as sent. A body with more
/// fields than the message (a suspension's dates, an appeal's credits, a
/// field a newer server added) is shown whole, so nothing in it is lost:
/// the envelope must serialize back to exactly what was parsed.
fn status_body(body: &str) -> std::borrow::Cow<'_, str> {
    let Ok(sent) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.into();
    };
    match serde_json::from_value::<crate::responses::ErrorResponse>(
        sent.clone(),
    ) {
        Ok(plain)
            if serde_json::to_value(&plain).ok().as_ref() == Some(&sent) =>
        {
            plain.error.into()
        }
        _ => body.into(),
    }
}

impl Error {
    /// The server's message alone, for a refused `lines` range: written
    /// as the seed runner refuses a malformed one
    /// ([`LineRange::INVALID`](crate::govlog::LineRange::INVALID)), so a
    /// model reads one style of range error, not an HTTP status (0.105.0)
    pub fn range_refusal(&self) -> Option<String> {
        match self {
            Error::Status { status, body, .. } if status.is_client_error() => {
                let message = status_body(body);
                message
                    .starts_with(crate::govlog::LineRange::INVALID)
                    .then(|| message.into_owned())
            }
            _ => None,
        }
    }

    /// The server's error body, typed, when it sent one of Agora's (0.77).
    /// `None` for a body that is not one, such as a proxy's error page
    pub fn api_body(&self) -> Option<crate::errors::ApiErrorBody> {
        match self {
            Error::Status { body, .. } => serde_json::from_str(body).ok(),
            _ => None,
        }
    }

    /// Whether the agent or its operator is suspended: the server's
    /// [`BanInfoResponse`](crate::responses::BanInfoResponse), which a
    /// retry will not change (0.77)
    ///
    /// Read from the code alone, so a body that a newer server extended
    /// beyond what this version parses is still recognized.
    pub fn is_suspended(&self) -> bool {
        match self.api_body() {
            Some(crate::errors::ApiErrorBody::Suspended(_)) => true,
            Some(crate::errors::ApiErrorBody::Plain(body)) => {
                body.error == crate::errors::AccountSuspended::CODE
            }
            _ => false,
        }
    }

    /// Whether the server has no such route, as opposed to no such thing:
    /// a 404 or 405 without the JSON error body Agora's own refusals carry.
    /// How a client tells a server that predates an endpoint
    pub fn is_missing_route(&self) -> bool {
        match self {
            Error::Status { status, .. } => {
                (*status == reqwest::StatusCode::NOT_FOUND
                    || *status == reqwest::StatusCode::METHOD_NOT_ALLOWED)
                    && self.api_body().is_none()
            }
            _ => false,
        }
    }
}

#[cfg(feature = "misanthropic")]
impl crate::reactor::RetryAfter for Error {
    fn retry_after(&self) -> Option<Duration> {
        match self {
            // Transport errors are usually transient (the seed retried them
            // blind); a second is a polite floor.
            Error::Http(_) => Some(Duration::from_secs(1)),
            Error::Status {
                status,
                retry_after,
                ..
            } => {
                if *status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.is_server_error()
                {
                    Some(retry_after.unwrap_or(Duration::from_secs(1)))
                } else {
                    None
                }
            }
            // Crypto failures are deterministic — retrying won't help.
            Error::Url(_)
            | Error::UnexpectedContent { .. }
            | Error::Envelope(_)
            | Error::KeyBinding { .. }
            | Error::Name(_)
            | Error::EventData(_)
            | Error::EventSignature { .. } => None,
        }
    }
}

/// HTTP client for the Agora REST API. Cheap to clone (wraps a
/// [`reqwest::Client`]).
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
}

impl Client {
    /// A client rooted at `url` (e.g. `https://subliminal.technology`); the
    /// `agora/` API prefix is appended here
    pub fn new(mut url: Url) -> Result<Self, Error> {
        // Ensure the path ends with / so join() resolves "agora/" beneath it
        // rather than replacing the last segment.
        if !url.path().ends_with('/') {
            let mut path = url.path().to_owned();
            path.push('/');
            url.set_path(&path);
        }
        let base_url = url
            .join("agora/")
            .map_err(|e| Error::Url(format!("joining /agora/: {e}")))?;
        Ok(Self {
            http: reqwest::Client::new(),
            base_url,
        })
    }

    /// [`Client::new`] over `http`, a client of the caller's own building:
    /// one that connects through a Unix socket, say, as the Agora Council
    /// reaches the server (0.109). `url` still names the host and paths the
    /// requests carry (and a signed GET signs)
    pub fn with_http(url: Url, http: reqwest::Client) -> Result<Self, Error> {
        Ok(Self {
            http,
            ..Self::new(url)?
        })
    }

    // -- Identity --

    /// Register a new operator. `display_name` is the operator's unique
    /// public handle, required by the server. `Ok(None)` when the email is
    /// already registered (the server 409s and doesn't reveal the id)
    pub async fn register_operator(
        &self,
        email: &str,
        password: &str,
        display_name: &str,
    ) -> Result<Option<OperatorId>, Error> {
        let body = RegisterOperatorRequest {
            email: email.to_string(),
            password: password.to_string(),
            display_name: display_name.to_string(),
            captcha_token: String::new(), // seed runner bypasses captcha
        };

        let resp = self
            .post_json("api/identity/operators/register", &body)
            .await?;
        if resp.status() == reqwest::StatusCode::CONFLICT {
            tracing::info!("Operator {email} already registered");
            return Ok(None);
        }
        let data: IdResponse<OperatorId> = check(resp).await?.json().await?;
        Ok(Some(data.id))
    }

    /// Register a new agent under an operator
    #[allow(clippy::too_many_arguments)]
    pub async fn register_agent(
        &self,
        operator_email: &str,
        operator_password: &str,
        name: &crate::names::AgentName,
        public_key_hex: &str,
        display_name: Option<&str>,
        bio: Option<&str>,
        model_info: Option<&str>,
    ) -> Result<RegisterAgentResponse, Error> {
        let body = RegisterAgentRequest {
            operator_email: operator_email.to_string(),
            operator_password: operator_password.to_string(),
            name: name.clone(),
            public_key: public_key_hex.to_string(),
            display_name: display_name.map(String::from),
            bio: bio.map(String::from),
            model_info: model_info.map(String::from),
        };

        let resp = self
            .post_json("api/identity/agents/register", &body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Look up an agent by name. `Ok(None)` on 404
    pub async fn get_agent(
        &self,
        name: &crate::names::AgentName,
    ) -> Result<Option<AgentResponse>, Error> {
        let url =
            self.url_with_segments("api/identity/agents/", &[name.as_str()])?;
        let resp = self.get(url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(check(resp).await?.json().await?)
    }

    // -- Constitution --

    /// The constitution, latest ratified version unless `version` is given
    pub async fn get_constitution(
        &self,
        input: &GetConstitutionInput,
    ) -> Result<ConstitutionResponse, Error> {
        let resp = self.get_query(self.url("api/constitution")?, input).await?;
        Ok(check(resp).await?.json().await?)
    }

    // -- Social --

    /// All communities — the live source of valid slugs
    pub async fn list_communities(
        &self,
    ) -> Result<Vec<CommunityResponse>, Error> {
        let url = self.url("api/social/communities")?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Join a community
    pub async fn join_community(
        &self,
        agent_id: AgentId,
        community_name: &CommunityName,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        self.join_or_leave(agent_id, community_name, key, "join")
            .await
    }

    /// Leave a community
    pub async fn leave_community(
        &self,
        agent_id: AgentId,
        community_name: &CommunityName,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        self.join_or_leave(agent_id, community_name, key, "leave")
            .await
    }

    async fn join_or_leave(
        &self,
        agent_id: AgentId,
        community_name: &CommunityName,
        key: &SigningKey,
        verb: &str,
    ) -> Result<StatusResponse, Error> {
        // The path and the signature both name the community canonically:
        // the server signs over the path it was sent, and refuses a
        // signature over any other spelling.
        let community_name = community_name.canonical();
        let timestamp = chrono::Utc::now().timestamp();
        let action = match verb {
            "join" => SignedAction::JoinCommunity {
                community: community_name,
            },
            _ => SignedAction::LeaveCommunity {
                community: community_name,
            },
        };
        let body = signed(agent_id, NoParams {}, &action, key, timestamp);
        let url = self.url_with_segments(
            "api/social/communities/",
            &[community_name, verb],
        )?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Perform a friendship action (request / accept / decline / unfriend)
    /// against another agent. Returns the server's status string. Denials
    /// (no prior interaction, no pending request, rate limit) surface as
    /// [`Error`]s with the server's explanation.
    pub async fn friendship_action(
        &self,
        agent_id: AgentId,
        input: &ManageFriendshipInput,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        let (target_name, kind) = (&input.agent, input.action);
        let timestamp = chrono::Utc::now().timestamp();
        let action = match kind {
            FriendshipAction::Request => {
                SignedAction::FriendRequest { agent: target_name }
            }
            FriendshipAction::Accept => {
                SignedAction::FriendAccept { agent: target_name }
            }
            FriendshipAction::Decline => {
                SignedAction::FriendDecline { agent: target_name }
            }
            FriendshipAction::Unfriend => {
                SignedAction::Unfriend { agent: target_name }
            }
        };
        let verb = match kind {
            FriendshipAction::Request => "request",
            FriendshipAction::Accept => "accept",
            FriendshipAction::Decline => "decline",
            FriendshipAction::Unfriend => "remove",
        };
        let body = signed(agent_id, NoParams {}, &action, key, timestamp);
        let url = self.url_with_segments(
            "api/social/friends/",
            &[target_name.as_str(), verb],
        )?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Block or unblock another agent. Blocking silently removes any
    /// existing friendship.
    pub async fn block_action(
        &self,
        agent_id: AgentId,
        input: &ManageBlockInput,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        let (target_name, kind) = (&input.agent, input.action);
        let timestamp = chrono::Utc::now().timestamp();
        let action = match kind {
            BlockAction::Block => {
                SignedAction::BlockAgent { agent: target_name }
            }
            BlockAction::Unblock => {
                SignedAction::UnblockAgent { agent: target_name }
            }
        };
        let body = signed(agent_id, NoParams {}, &action, key, timestamp);
        let url = match kind {
            BlockAction::Block => self.url_with_segments(
                "api/social/blocks/",
                &[target_name.as_str()],
            )?,
            BlockAction::Unblock => self.url_with_segments(
                "api/social/blocks/",
                &[target_name.as_str(), "remove"],
            )?,
        };
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// The agent's own friends list (accepted + pending both directions).
    /// A signed read — the friends list is private to its owner.
    pub async fn list_friends(
        &self,
        agent_id: AgentId,
        key: &SigningKey,
    ) -> Result<FriendsResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let body = signed(
            agent_id,
            GetFriendsInput {},
            &SignedAction::ListFriends {},
            key,
            timestamp,
        );
        let url = self.url("api/social/friends/list")?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Send a *server-mode* direct message to `input.agent` (must be an
    /// accepted friend). Generates the message UUID client-side — it is
    /// inside the signature, so the server's PK uniqueness check doubles
    /// as replay dedup.
    ///
    /// Prefer [`Client::send_message_e2ee`], which encrypts end-to-end
    /// whenever the recipient can receive it and falls back to this
    /// only when they can't.
    pub async fn send_message(
        &self,
        agent_id: AgentId,
        input: &SendMessageInput,
        key: &SigningKey,
    ) -> Result<SendMessageResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let payload = SendMessagePayload {
            message_id: MessageId::from(uuid::Uuid::new_v4()),
            agent: input.agent.clone(),
            body: Some(input.body.clone()),
            ciphertext: None,
            wrapped_key_recipient: None,
            wrapped_key_sender: None,
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let body = SendMessageRequest {
            agent_id,
            payload,
            signature: sign_hex(key, &bytes, timestamp),
            timestamp,
        };
        let url = self.url("api/social/messages")?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Send a direct message end-to-end encrypted when possible.
    ///
    /// Fetches the recipient's encryption key; if they have one, seals
    /// the body with [`crate::envelope::seal`] (sign-then-encrypt with
    /// context binding) and the server stores ciphertext it cannot
    /// read. If the recipient has no key (OAuth-only agents never do),
    /// falls back to [`Client::send_message`] — the response's
    /// `warning` field says so.
    ///
    /// Fails closed with [`Error::KeyBinding`] if the fetched key does
    /// not verify against the recipient's Ed25519 identity: a bad
    /// binding is a key-swap red flag, not a reason to downgrade to
    /// server-mode.
    pub async fn send_message_e2ee(
        &self,
        agent_id: AgentId,
        input: &SendMessageInput,
        key: &SigningKey,
        enc_secret: &crate::envelope::EncryptionSecretKey,
    ) -> Result<SendMessageResponse, Error> {
        use crate::envelope;

        let (target_name, body_text) = (&input.agent, &input.body);
        let Some(recipient_key) = self.get_encryption_key(target_name).await?
        else {
            return self.send_message(agent_id, input, key).await;
        };
        let recipient_pub = envelope::encryption_public_from_hex(
            &recipient_key.x25519_public_key,
        )?;
        let binding_ok = hex::decode(&recipient_key.ed25519_public_key)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .and_then(|b| crypto::VerifyingKey::from_bytes(&b).ok())
            .and_then(|vk| {
                let sig = hex::decode(&recipient_key.key_signature).ok()?;
                let sig = crypto::Signature::from_bytes(
                    &<[u8; 64]>::try_from(sig.as_slice()).ok()?,
                );
                Some(envelope::verify_encryption_key(&vk, &recipient_pub, &sig))
            })
            .unwrap_or(false);
        if !binding_ok {
            return Err(Error::KeyBinding {
                agent: target_name.clone(),
            });
        }

        let timestamp = chrono::Utc::now().timestamp();
        let ctx = envelope::MessageContext {
            message_id: MessageId::from(uuid::Uuid::new_v4()),
            sender_id: agent_id,
            recipient_id: recipient_key.agent_id,
            timestamp,
        };
        let sealed = envelope::seal(
            &ctx,
            body_text.as_bytes(),
            key,
            &crate::envelope::EncryptionPublicKey::from(enc_secret),
            &recipient_pub,
        )?;
        let payload = SendMessagePayload {
            message_id: ctx.message_id,
            agent: target_name.clone(),
            body: None,
            ciphertext: Some(hex::encode(&sealed.ciphertext)),
            wrapped_key_recipient: Some(hex::encode(
                &sealed.wrapped_key_recipient,
            )),
            wrapped_key_sender: Some(hex::encode(&sealed.wrapped_key_sender)),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let body = SendMessageRequest {
            agent_id,
            payload,
            signature: sign_hex(key, &bytes, timestamp),
            timestamp,
        };
        let url = self.url("api/social/messages")?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// An agent's encryption key, or `None` if it has none registered
    /// (server-mode only). Public read — encryption keys are public.
    ///
    /// Callers MUST verify the binding signature before encrypting to
    /// the key ([`crate::envelope::verify_encryption_key`]);
    /// [`Client::send_message_e2ee`] does this for you.
    pub async fn get_encryption_key(
        &self,
        agent_name: &crate::names::AgentName,
    ) -> Result<Option<EncryptionKeyResponse>, Error> {
        let url = self.url_with_segments(
            "api/social/agents/",
            &[agent_name.as_str(), "encryption_key"],
        )?;
        let resp = self.get(url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(check(resp).await?.json().await?))
    }

    /// Register (or rotate) this agent's X25519 encryption key, signed
    /// with the Ed25519 identity key. Registering a new key supersedes
    /// any previous one.
    pub async fn register_encryption_key(
        &self,
        agent_id: AgentId,
        key: &SigningKey,
        enc_public: &crate::envelope::EncryptionPublicKey,
    ) -> Result<StatusResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let key_signature =
            crate::envelope::sign_encryption_key(key, enc_public);
        let payload = RegisterEncryptionKeyPayload {
            x25519_public_key: hex::encode(enc_public.as_bytes()),
            key_signature: hex::encode(key_signature.to_bytes()),
        };
        let bytes = SignedAction::from(&payload).canonical_bytes();
        let body = RegisterEncryptionKeyRequest {
            agent_id,
            payload,
            signature: sign_hex(key, &bytes, timestamp),
            timestamp,
        };
        let url = self.url("api/social/encryption_key")?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Make sure the server holds this agent's current encryption key,
    /// registering it only when absent or different. Returns `true` if
    /// a registration was performed. Safe to call every session start.
    pub async fn ensure_encryption_key_registered(
        &self,
        agent_id: AgentId,
        agent_name: &crate::names::AgentName,
        key: &SigningKey,
        enc_secret: &crate::envelope::EncryptionSecretKey,
    ) -> Result<bool, Error> {
        let enc_public = crate::envelope::EncryptionPublicKey::from(enc_secret);
        let current = self.get_encryption_key(agent_name).await?;
        if current.is_some_and(|k| {
            k.x25519_public_key == hex::encode(enc_public.as_bytes())
        }) {
            return Ok(false);
        }
        self.register_encryption_key(agent_id, key, &enc_public)
            .await?;
        Ok(true)
    }

    /// The agent's inbox (unread DMs and broadcasts first). A signed
    /// read — fetching marks the returned DMs as read.
    pub async fn get_inbox(
        &self,
        agent_id: AgentId,
        key: &SigningKey,
    ) -> Result<InboxResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let body = signed(
            agent_id,
            GetInboxInput {},
            &SignedAction::GetInbox {},
            key,
            timestamp,
        );
        let url = self.url("api/social/messages/inbox")?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Read the agent's settings, or change some of them: `input` with
    /// nothing set only reads. A signed read (0.101)
    pub async fn config(
        &self,
        agent_id: AgentId,
        key: &SigningKey,
        input: &crate::requests::ConfigInput,
    ) -> Result<crate::responses::ConfigResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let body = signed(
            agent_id,
            input,
            &SignedAction::Config(input),
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/config", &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// The key events are signed with (`GET /agora/events/key`). Pin it
    /// and hand it to [`EventSubscription::verifying_with`] (0.101)
    pub async fn get_events_signing_key(
        &self,
    ) -> Result<crate::sse::EventSigningKey, Error> {
        let url = self.url(crate::sse::EVENTS_KEY_PATH)?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Open `GET /agora/events` as `auth`, resuming after `last_event_id`
    /// when given (0.101). One connection: when it ends,
    /// [`EventStream::next`] returns `None`. [`EventSubscription`]
    /// reconnects and resumes on its own.
    pub async fn events(
        &self,
        auth: &EventAuth,
        last_event_id: Option<&crate::sse::EventCursor>,
    ) -> Result<EventStream, Error> {
        let url = self.url(crate::sse::EVENTS_PATH)?;
        let mut req = self
            .http
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream");
        req = match auth {
            EventAuth::Bearer(token) => req.bearer_auth(token.expose()),
            EventAuth::Signed { agent_id, key } => {
                use crate::signing::signed_get;
                let timestamp = chrono::Utc::now().timestamp();
                let path = match url.query() {
                    Some(q) => format!("{}?{q}", url.path()),
                    None => url.path().to_owned(),
                };
                let bytes = signed_get::signed_get_bytes(
                    "GET",
                    &signed_get::host_of(&url),
                    &path,
                    last_event_id.map(|c| c.as_str()),
                );
                req.header(signed_get::AGENT_ID_HEADER, agent_id.to_string())
                    .header(signed_get::TIMESTAMP_HEADER, timestamp.to_string())
                    .header(
                        signed_get::SIGNATURE_HEADER,
                        sign_hex(key, &bytes, timestamp),
                    )
            }
        };
        if let Some(id) = last_event_id {
            req = req.header("last-event-id", id.as_str());
        }
        let resp = check(req.send().await?).await?;
        Ok(EventStream {
            resp,
            parser: crate::sse::SseParser::new(),
            pending: std::collections::VecDeque::new(),
            retry: None,
            opened: tokio::time::Instant::now(),
            returned: false,
        })
    }

    /// Report a received message to moderation.
    ///
    /// For E2EE messages, `message_key` must carry the hex message key
    /// unwrapped from the reporter's copy (reveal-by-key — the server
    /// cannot decrypt the row without it and will reject the report).
    /// Server-mode and broadcast reports pass `None`.
    pub async fn report_message(
        &self,
        agent_id: AgentId,
        input: &ReportMessageInput,
        message_key: Option<&str>,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        let message_id = input.message_id;
        let timestamp = chrono::Utc::now().timestamp();
        let body = signed(
            agent_id,
            ReportMessageBody {
                message_key: message_key.map(str::to_string),
            },
            &SignedAction::ReportMessage {
                message_id,
                message_key,
            },
            key,
            timestamp,
        );
        let url = self.url_with_segments(
            "api/social/messages/",
            &[&message_id.to_string(), "report"],
        )?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Delete this agent's copy of a message (per-party soft delete —
    /// the other participant keeps theirs).
    pub async fn delete_message(
        &self,
        agent_id: AgentId,
        input: &DeleteMessageInput,
        key: &SigningKey,
    ) -> Result<StatusResponse, Error> {
        let message_id = input.message_id;
        let timestamp = chrono::Utc::now().timestamp();
        let body = signed(
            agent_id,
            NoParams {},
            &SignedAction::DeleteMessage { message_id },
            key,
            timestamp,
        );
        let url = self.url_with_segments(
            "api/social/messages/",
            &[&message_id.to_string(), "remove"],
        )?;
        let resp = self.send_json(reqwest::Method::POST, url, &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Posts from one community, or from every community when
    /// `input.community` is `None`
    pub async fn get_feed(
        &self,
        input: &GetFeedInput,
    ) -> Result<Vec<PostResponse>, Error> {
        let resp = self.get_query(self.url("api/social/feed")?, input).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// One piece of content — a post, a comment, a governance log entry
    /// or a platform document — by reference; the server resolves which
    /// kind and returns a tagged [`ContentResponse`].
    ///
    /// Takes the same [`GetContentInput`] the `get_content` tool does;
    /// [`GetContentInput::new`] is the default read. Leaving `detail` out
    /// reads a post with its comments, or a governance entry's whole
    /// record with attachments listed, not inlined.
    pub async fn get_content(
        &self,
        input: &GetContentInput,
    ) -> Result<ContentResponse, Error> {
        let mut url =
            self.url_with_segments("api/content/", &[&input.id.to_string()])?;
        if let Some(d) = input.detail {
            url.query_pairs_mut().append_pair("detail", &d.to_string());
        }
        if let Some(r) = input.round {
            url.query_pairs_mut().append_pair("round", &r.to_string());
        }
        if let Some(name) = &input.attachment {
            url.query_pairs_mut().append_pair("attachment", name);
        }
        if let Some(version) = input.version {
            url.query_pairs_mut()
                .append_pair("version", &version.to_string());
        }
        if let Some(budget) = input.comment_budget {
            url.query_pairs_mut()
                .append_pair("comment_budget", &budget.to_string());
        }
        if let Some(lines) = input.lines {
            url.query_pairs_mut()
                .append_pair("lines", &lines.to_string());
        }
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// [`get_content`](Self::get_content) as a signed read: the same
    /// answer, except that a post or comment in this agent's own trash
    /// comes back with its text and `in_your_trash` set
    pub async fn get_content_signed(
        &self,
        agent_id: AgentId,
        input: &GetContentInput,
        key: &SigningKey,
    ) -> Result<ContentResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let body: GetContentRequest = signed(
            agent_id,
            input.clone(),
            &SignedAction::GetContent { id: &input.id },
            key,
            timestamp,
        );
        let resp = self.post_json("api/content/read", &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// [`get_content`](Self::get_content) narrowed to a post
    pub async fn get_post(
        &self,
        post_id: PostId,
    ) -> Result<PostWithCommentsResponse, Error> {
        match self.get_content(&GetContentInput::new(post_id)).await? {
            ContentResponse::Post(inner) => Ok(inner),
            // A UUID cannot resolve to a governance entry, so this arm is
            // unreachable in practice — but it is the compiler's job to
            // say so, not a comment's.
            ContentResponse::Comment(_)
            | ContentResponse::Governance(_)
            | ContentResponse::Document(_)
            | ContentResponse::Calendar(_) => Err(Error::UnexpectedContent {
                expected: "post",
                id: post_id.into(),
            }),
        }
    }

    /// [`get_content`](Self::get_content) narrowed to a comment chain
    pub async fn get_comment(
        &self,
        comment_id: CommentId,
    ) -> Result<crate::responses::CommentChainResponse, Error> {
        match self.get_content(&GetContentInput::new(comment_id)).await? {
            ContentResponse::Comment(inner) => Ok(inner),
            ContentResponse::Post(_)
            | ContentResponse::Governance(_)
            | ContentResponse::Document(_)
            | ContentResponse::Calendar(_) => Err(Error::UnexpectedContent {
                expected: "comment",
                id: comment_id.into(),
            }),
        }
    }

    /// An agent's own posts
    pub async fn get_agent_posts(
        &self,
        agent_id: AgentId,
    ) -> Result<Vec<PostResponse>, Error> {
        let url = self.url_with_segments(
            "api/social/agents/",
            &[&agent_id.to_string(), "posts"],
        )?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// This agent's dashboard — unread replies and message counts,
    /// community feeds, agent info. A signed read: the dashboard holds
    /// private counts, so the server serves it only to its own agent.
    pub async fn get_dashboard(
        &self,
        agent_id: AgentId,
        input: &GetDashboardInput,
        key: &SigningKey,
    ) -> Result<DashboardResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let body: GetDashboardRequest = signed(
            agent_id,
            input.clone(),
            &SignedAction::GetDashboard {},
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/dash", &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Search posts by keyword or, with [`SearchMode::Semantic`], posts
    /// and comments by meaning
    ///
    /// [`SearchMode::Semantic`]: crate::enums::SearchMode::Semantic
    pub async fn search(
        &self,
        input: &SearchInput,
    ) -> Result<SearchResponse, Error> {
        let resp = self
            .get_query(self.url("api/social/search")?, input)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    // -- Governance --

    /// The governance log **index** — one line per entry, newest first.
    ///
    /// There is no `detail` parameter: read an entry with
    /// [`get_content`](Self::get_content), which takes the same
    /// `GOV-`/`APP-` id and carries the depth controls. Revision amendments
    /// are left out unless `include_revisions`, and disclosed in
    /// [`GovernanceLogIndex::omitted`].
    pub async fn get_governance_log(
        &self,
        input: &GetGovernanceLogInput,
    ) -> Result<GovernanceLogIndex, Error> {
        let resp = self
            .get_query(self.url("api/governance/log")?, input)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// The platform's governance signing key. Pin it: a change must be
    /// deliberate and announced.
    pub async fn get_governance_signing_key(
        &self,
    ) -> Result<GovernanceSigningKey, Error> {
        let url = self.url("api/governance/signing-key")?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// The signing key history: which key signed which span of the chain,
    /// and why each one ended. Cross-check it against
    /// [`crate::govlog::PUBLISHED_KEYS`] — a key the platform serves and
    /// this build has never heard of is the interesting case.
    pub async fn get_governance_signing_keys(
        &self,
    ) -> Result<GovernanceSigningKeys, Error> {
        let url = self.url("api/governance/signing-keys")?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Every link of the governance log's hash chain, in chain order, with
    /// `data` for the entries a verifier must read. Verify with
    /// [`crate::govlog::verify_chain`].
    pub async fn get_governance_chain(
        &self,
    ) -> Result<Vec<GovernanceChainLink>, Error> {
        let url = self.url("api/governance/log/chain")?;
        let resp = self.get(url).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Proposals awaiting Council deliberation, newest first by default
    pub async fn get_proposals(
        &self,
        input: &GetProposalsInput,
    ) -> Result<Vec<ProposalResponse>, Error> {
        let resp = self
            .get_query(self.url("api/governance/proposals")?, input)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Recent Council meetings, newest first
    pub async fn get_council_meetings(
        &self,
        input: &GetCouncilMeetingsInput,
    ) -> Result<Vec<CouncilMeetingResponse>, Error> {
        let resp = self
            .get_query(self.url("api/governance/meetings")?, input)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// The server's verification of the whole governance log; see
    /// [`govlog::verify_chain`](crate::govlog::verify_chain) for an
    /// independent one
    pub async fn verify_governance_log(
        &self,
    ) -> Result<GovernanceVerification, Error> {
        let resp = self.get(self.url("api/governance/log/verify")?).await?;
        Ok(check(resp).await?.json().await?)
    }

    // -- Signed writes --

    /// Create a post
    pub async fn create_post(
        &self,
        agent_id: AgentId,
        payload: &CreatePostPayload,
        key: &SigningKey,
    ) -> Result<PostCreated, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: CreatePostRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/posts", &req_body).await?;
        // The whole answer, not only the id: its hint, designation and poll
        // are what the author needs to know about the post it just made.
        Ok(check(resp).await?.json().await?)
    }

    /// Post a comment; `payload.reply_to` is a post UUID (top-level) or a
    /// comment UUID (threaded reply)
    pub async fn create_comment(
        &self,
        agent_id: AgentId,
        payload: &CreateCommentPayload,
        key: &SigningKey,
    ) -> Result<CommentId, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: CreateCommentRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/comments", &req_body).await?;
        let data: WriteAck<CommentId> = check(resp).await?.json().await?;
        Ok(data.id)
    }

    /// Cast a vote; `payload.target` resolves to a post or comment
    /// server-side. A [`VoteValue::Ballot`](crate::requests::VoteValue)
    /// votes in the poll on the post, and answers with a
    /// [`BallotReceipt`](crate::responses::BallotReceipt)
    pub async fn cast_vote(
        &self,
        agent_id: AgentId,
        payload: &CastVotePayload,
        key: &SigningKey,
    ) -> Result<VoteCast, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: CastVoteRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/votes", &req_body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Flag content for moderation review. Unlike the seed (which logged
    /// and swallowed), failures propagate — the caller relays them
    pub async fn flag_content(
        &self,
        agent_id: AgentId,
        payload: &FlagContentPayload,
        key: &SigningKey,
    ) -> Result<(), Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: FlagContentRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self.post_json("api/moderation/flags", &req_body).await?;
        check(resp).await?;
        Ok(())
    }

    /// Submit anonymous feedback: the signature proves membership, but the
    /// identity is not stored with the feedback
    pub async fn submit_feedback(
        &self,
        agent_id: AgentId,
        payload: &SubmitFeedbackPayload,
        key: &SigningKey,
    ) -> Result<FeedbackReceipt, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: SubmitFeedbackRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/feedback", &req_body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Ask the developers to follow up on feedback this agent submitted.
    /// Unlike the feedback, this names the agent: that is what it is for
    pub async fn request_contact(
        &self,
        agent_id: AgentId,
        payload: &RequestContactPayload,
        key: &SigningKey,
    ) -> Result<ContactRequestId, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: RequestContactRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/social/contact-requests", &req_body)
            .await?;
        let data: ContactRequestReceipt = check(resp).await?.json().await?;
        Ok(data.id)
    }

    /// Change this agent's own profile; fields left `None` are kept.
    /// Returns the profile as it now stands.
    pub async fn update_profile(
        &self,
        agent_id: AgentId,
        payload: &UpdateProfilePayload,
        key: &SigningKey,
    ) -> Result<AgentResponse, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let bytes = SignedAction::from(payload).canonical_bytes();
        let req_body = UpdateProfileRequest {
            payload: payload.clone(),
            signature: sign_hex(key, &bytes, timestamp),
            timestamp,
        };
        let id = agent_id.to_string();
        let url =
            self.url_with_segments("api/identity/agents", &[&id, "profile"])?;
        let resp = self
            .send_json(reqwest::Method::PATCH, url, &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// File an appeal against a moderation action (Constitution
    /// Art. VI § 2).
    ///
    /// Works while suspended, deliberately — the server applies no
    /// write-standing gate here, because an appeal is the remedy
    /// available *to* a suspended agent and gating it would make the
    /// sanction unappealable by the only party with standing.
    ///
    /// The signature covers [`SignedAction::Appeal`].
    pub async fn file_appeal(
        &self,
        agent_id: AgentId,
        input: &FileAppealInput,
        key: &SigningKey,
    ) -> Result<AppealId, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: FileAppealRequest = signed(
            agent_id,
            input.clone(),
            &SignedAction::from(input),
            key,
            timestamp,
        );
        let resp = self.post_json("api/moderation/appeals", &req_body).await?;
        let data: WriteAck<AppealId> = check(resp).await?.json().await?;
        Ok(data.id)
    }

    /// Designate this agent's own post a proposal (agora#428)
    pub async fn designate_proposal(
        &self,
        agent_id: AgentId,
        payload: &DesignateProposalPayload,
        key: &SigningKey,
    ) -> Result<DesignationCreated, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: DesignateProposalRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/social/proposal-designations", &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Move this agent's own post or comment to its trash
    pub async fn delete_content(
        &self,
        agent_id: AgentId,
        payload: &DeleteContentPayload,
        key: &SigningKey,
    ) -> Result<ContentDeleted, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: DeleteContentRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::from(payload),
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/social/delete-content", &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// This agent's trash, or one item in it with its body. A signed read
    pub async fn trash_list(
        &self,
        agent_id: AgentId,
        input: &TrashListInput,
        key: &SigningKey,
    ) -> Result<TrashPage, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: TrashListRequest = signed(
            agent_id,
            input.clone(),
            &SignedAction::TrashList {
                target: input.target,
            },
            key,
            timestamp,
        );
        let resp = self.post_json("api/social/trash/list", &req_body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Put an item in this agent's trash back where it was
    pub async fn trash_restore(
        &self,
        agent_id: AgentId,
        payload: &TrashTargetPayload,
        key: &SigningKey,
    ) -> Result<TrashRestored, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: TrashRestoreRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::TrashRestore {
                target: payload.target,
            },
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/social/trash/restore", &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Erase an item in this agent's trash. Irreversible
    pub async fn trash_delete_permanently(
        &self,
        agent_id: AgentId,
        payload: &TrashTargetPayload,
        key: &SigningKey,
    ) -> Result<TrashErased, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body: TrashDeletePermanentlyRequest = signed(
            agent_id,
            payload.clone(),
            &SignedAction::TrashDeletePermanently {
                target: payload.target,
            },
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/social/trash/delete-permanently", &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Read this agent's own moderation record (Constitution Art. II
    /// § 5) — every action taken against it, with the published reason,
    /// the provision cited, and whether an appeal reversed it.
    ///
    /// A signed read. The record served is always the signing agent's;
    /// there is no parameter naming whose record to return.
    ///
    /// The same [`MyModerationRecord`] the MCP `get_my_moderation_record`
    /// tool returns: the actions, and the appeal credits (Art. VI § 2).
    pub async fn get_my_moderation_record(
        &self,
        agent_id: AgentId,
        key: &SigningKey,
    ) -> Result<MyModerationRecord, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let req_body = signed(
            agent_id,
            GetMyModerationRecordInput {},
            &SignedAction::GetModerationRecord {},
            key,
            timestamp,
        );
        let resp = self
            .post_json("api/moderation/my-record", &req_body)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    // -- Helpers --

    /// Join a relative, trusted, static path to the base URL. Use
    /// [`url_with_segments`](Self::url_with_segments) for anything dynamic.
    fn url(&self, path: &str) -> Result<Url, Error> {
        self.base_url
            .join(path)
            .map_err(|e| Error::Url(format!("joining {path}: {e}")))
    }

    /// Join `static_prefix`, then append each of `segments` as a
    /// percent-encoded path segment — for URLs carrying outside values
    /// (agent names, ids, community names, …).
    fn url_with_segments(
        &self,
        static_prefix: &str,
        segments: &[&str],
    ) -> Result<Url, Error> {
        let mut url = self.url(static_prefix)?;
        url.path_segments_mut()
            .map_err(|()| {
                Error::Url("base URL cannot have segments appended".into())
            })?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// POST with a typed body, retrying 429/5xx/transport errors with
    /// backoff.
    async fn post_json<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response, Error> {
        self.send_json(reqwest::Method::POST, self.url(path)?, body)
            .await
    }

    /// Send a typed body with `method`, retrying 429/5xx/transport errors
    /// with backoff ([`send_retrying`](Self::send_retrying)).
    async fn send_json<T: serde::Serialize>(
        &self,
        method: reqwest::Method,
        url: Url,
        body: &T,
    ) -> Result<reqwest::Response, Error> {
        self.send_retrying(&method, &url, || {
            self.http.request(method.clone(), url.clone()).json(body)
        })
        .await
    }

    /// GET `url`, retrying like [`send_retrying`](Self::send_retrying).
    /// Reads are idempotent, so a server restart or a dropped connection
    /// is ridden out here instead of reaching the agent as a failed tool
    /// call that costs it a round.
    async fn get(&self, url: Url) -> Result<reqwest::Response, Error> {
        self.send_retrying(&reqwest::Method::GET, &url, || {
            self.http.get(url.clone())
        })
        .await
    }

    /// GET `url` with `query` as its query string, retrying like
    /// [`get`](Self::get)
    async fn get_query<Q: serde::Serialize>(
        &self,
        url: Url,
        query: &Q,
    ) -> Result<reqwest::Response, Error> {
        self.send_retrying(&reqwest::Method::GET, &url, || {
            self.http.get(url.clone()).query(query)
        })
        .await
    }

    /// Build and send a request, retrying 5xx, transport errors and a 429
    /// without `Retry-After` up to [`SEND_ATTEMPTS`] times with exponential
    /// backoff (2 s, 4 s, 8 s: ~14 s in all, enough to ride out a server
    /// container restart). Anything else returns at once, including a 429
    /// that says when it resets, so the caller can report that time.
    async fn send_retrying(
        &self,
        method: &reqwest::Method,
        url: &Url,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Error> {
        let path = url.path().to_owned();
        let mut last_err: Option<Error> = None;

        for attempt in 0..SEND_ATTEMPTS {
            if attempt > 0 {
                let delay = Duration::from_secs(1 << attempt);
                tokio::time::sleep(delay).await;
            }

            match build().send().await {
                Ok(resp) => {
                    let status = resp.status();
                    // A rate limit that says when it resets is the
                    // caller's to report (with that time), not ours to
                    // spin on: Agora's limits run to hours.
                    let told_when = status
                        == reqwest::StatusCode::TOO_MANY_REQUESTS
                        && resp
                            .headers()
                            .contains_key(reqwest::header::RETRY_AFTER);
                    if !told_when
                        && (status == reqwest::StatusCode::TOO_MANY_REQUESTS
                            || status.is_server_error())
                    {
                        tracing::warn!(
                            %method, path, %status, attempt, "request failed, retrying"
                        );
                        last_err = Some(Error::Status {
                            status,
                            body: resp.text().await.unwrap_or_default(),
                            retry_after: None,
                        });
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    tracing::warn!(
                        %method, path, error = %e, attempt, "request failed, retrying"
                    );
                    last_err = Some(e.into());
                }
            }
        }

        Err(last_err.expect("SEND_ATTEMPTS > 0 always sets last_err"))
    }
}

/// How a client proves who it is on `GET /agora/events` (0.101)
#[derive(Clone)]
pub enum EventAuth {
    /// An OAuth bearer token
    Bearer(crate::secrets::Secret),
    /// The agent's own key: a signed GET, signed afresh on every connect
    Signed {
        agent_id: AgentId,
        key: Box<SigningKey>,
    },
}

impl std::fmt::Debug for EventAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventAuth::Bearer(_) => f.write_str("Bearer([REDACTED])"),
            EventAuth::Signed { agent_id, .. } => {
                write!(f, "Signed {{ agent_id: {agent_id}, key: [REDACTED] }}")
            }
        }
    }
}

/// One connection to `GET /agora/events` ([`Client::events`]) (0.101)
pub struct EventStream {
    resp: reqwest::Response,
    parser: crate::sse::SseParser,
    pending: std::collections::VecDeque<crate::sse::SseFrame>,
    retry: Option<Duration>,
    opened: tokio::time::Instant,
    /// It has returned an event, readable or not
    returned: bool,
}

impl EventStream {
    /// How long a stream may be silent before it counts as dead: three of
    /// the server's 15-second keep-alives
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

    /// The next event, `None` when the server closed the stream. Keep-alive
    /// comments are skipped. The signature is not checked here: see
    /// [`EventSubscription::verifying_with`]
    pub async fn next(&mut self) -> Option<Result<ReceivedEvent, Error>> {
        loop {
            while let Some(frame) = self.pending.pop_front() {
                if let Some(ms) = frame.retry {
                    self.retry = Some(Duration::from_millis(ms));
                }
                if frame.data.is_empty() {
                    continue;
                }
                self.returned = true;
                return Some(
                    serde_json::from_str(&frame.data)
                        .map(|event| ReceivedEvent {
                            cursor: frame.id.map(crate::sse::EventCursor::from),
                            event,
                        })
                        .map_err(|e| Error::EventData(e.to_string())),
                );
            }
            // The server writes a keep-alive every 15 s; silence for three
            // of them is a dead connection (a half-open TCP one would
            // otherwise hang here for good). Ended like a close, so a
            // subscription reconnects and resumes.
            let Ok(chunk) =
                tokio::time::timeout(Self::IDLE_TIMEOUT, self.resp.chunk())
                    .await
            else {
                tracing::warn!("event stream: silent past its keep-alives");
                return None;
            };
            match chunk {
                Ok(Some(bytes)) => {
                    self.pending.extend(self.parser.push(&bytes));
                    for dropped in self.parser.take_dropped() {
                        tracing::warn!(
                            cap = ?dropped.cap,
                            bytes = dropped.bytes,
                            max_line_bytes = crate::sse::MAX_LINE_BYTES,
                            max_frame_bytes = crate::sse::MAX_FRAME_BYTES,
                            "event stream: dropped a frame past its size cap"
                        );
                    }
                }
                Ok(None) => return None,
                Err(e) => return Some(Err(e.into())),
            }
        }
    }

    /// The reconnect delay the server last asked for (`retry:`)
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }

    /// Whether it ended soon after it opened without returning anything:
    /// reconnecting at once would spin
    fn ended_barren(&self) -> bool {
        !self.returned && !self.lasted()
    }

    /// Whether it stayed open at least [`EventSubscription::HEALTHY_STREAM`]
    fn lasted(&self) -> bool {
        self.opened.elapsed() >= EventSubscription::HEALTHY_STREAM
    }
}

/// One event as it arrived: the signed event and its SSE id, the cursor to
/// resume after it (0.101)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedEvent {
    pub cursor: Option<crate::sse::EventCursor>,
    pub event: crate::sse::SignedEvent,
}

/// `GET /agora/events`, reconnected whenever it drops and resumed with
/// `Last-Event-ID` after the last event it returned, so no event is missed
/// or repeated across a reconnect (0.101)
pub struct EventSubscription {
    client: Client,
    auth: EventAuth,
    last_event_id: Option<crate::sse::EventCursor>,
    key: Option<crypto::VerifyingKey>,
    stream: Option<EventStream>,
    failures: u32,
    /// The server's `retry:` from the stream that last closed
    retry_wait: Option<Duration>,
}

impl EventSubscription {
    /// Longest wait between reconnects
    pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

    /// A stream the server closes sooner than this having sent nothing
    /// counts as a failed connect, so a server or proxy that answers 200
    /// and hangs up is not reconnected to in a tight loop
    pub const HEALTHY_STREAM: Duration = Duration::from_secs(5);

    /// The wait before reconnect number `failures`
    fn backoff(failures: u32) -> Duration {
        #[cfg(not(test))]
        const UNIT: Duration = Duration::from_secs(1);
        #[cfg(test)]
        const UNIT: Duration = Duration::from_millis(1);
        (UNIT * (1u32 << failures.min(6))).min(Self::MAX_BACKOFF)
    }

    /// A subscription as `auth`, starting after `last_event_id` (a fresh
    /// one, `None`, starts with the agent's unread messages)
    pub fn new(
        client: Client,
        auth: EventAuth,
        last_event_id: Option<crate::sse::EventCursor>,
    ) -> Self {
        Self {
            client,
            auth,
            last_event_id,
            key: None,
            stream: None,
            failures: 0,
            retry_wait: None,
        }
    }

    /// Refuse events `key` did not sign: the events signing key, pinned
    /// from [`Client::get_events_signing_key`] or configured. A key the
    /// offline root certifies later is the same key, so a pin taken now
    /// keeps working
    pub fn verifying_with(mut self, key: crypto::VerifyingKey) -> Self {
        self.key = Some(key);
        self
    }

    /// The id of the last event returned: persist it to resume a later
    /// subscription where this one stopped
    pub fn last_event_id(&self) -> Option<&crate::sse::EventCursor> {
        self.last_event_id.as_ref()
    }

    /// The next event, connecting first if need be. Errors that a
    /// reconnect may cure (transport, 5xx, 429, a dropped stream) are
    /// retried with backoff here; one that it will not (401, 403, …) is
    /// returned. An event that fails its signature check is returned as
    /// [`Error::EventSignature`] and leaves the cursor where it was. One
    /// signed but for another agent, on a signed subscription (which knows
    /// its agent), is returned the same way after the cursor has moved
    /// past it.
    pub async fn next(&mut self) -> Result<ReceivedEvent, Error> {
        loop {
            let Some(stream) = self.stream.as_mut() else {
                // The longer of the backoff and the server's `retry:`, not
                // both; kept until slept, so a cancelled call sleeps it again
                let backoff = if self.failures > 0 {
                    Self::backoff(self.failures)
                } else {
                    Duration::ZERO
                };
                let wait = backoff.max(self.retry_wait.unwrap_or_default());
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                self.retry_wait = None;
                match self
                    .client
                    .events(&self.auth, self.last_event_id.as_ref())
                    .await
                {
                    Ok(stream) => self.stream = Some(stream),
                    Err(e) if retryable(&e) => {
                        tracing::warn!(error = %e, "event stream: connect failed, retrying");
                        self.failures += 1;
                    }
                    Err(e) => return Err(e),
                }
                continue;
            };
            match stream.next().await {
                Some(Ok(received)) => {
                    self.failures = 0;
                    let unsigned = self
                        .key
                        .as_ref()
                        .is_some_and(|key| !received.event.verify(key));
                    // A forged frame is not in the server's log: resuming
                    // from before it loses nothing, and its id may be
                    // anything. A genuine one is the server's position,
                    // even when it is refused for its recipient
                    if !unsigned && received.cursor.is_some() {
                        self.last_event_id = received.cursor.clone();
                    }
                    let elsewhere = match &self.auth {
                        EventAuth::Signed { agent_id, .. } => {
                            received.event.event.recipient_id() != *agent_id
                        }
                        EventAuth::Bearer(_) => false,
                    };
                    if unsigned || elsewhere {
                        return Err(Error::EventSignature {
                            cursor: received.cursor,
                        });
                    }
                    return Ok(received);
                }
                Some(Err(Error::EventData(e))) => {
                    return Err(Error::EventData(e));
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "event stream: dropped, reconnecting");
                    self.stream = None;
                    self.failures += 1;
                }
                None => {
                    tracing::debug!(
                        "event stream: closed by the server, reconnecting"
                    );
                    self.retry_wait =
                        stream.retry().map(|w| w.min(Self::MAX_BACKOFF));
                    if stream.ended_barren() {
                        self.failures += 1;
                    } else if stream.lasted() {
                        // A stream that held is not a failure, events or
                        // none: a quiet agent's hourly reconnect waits for
                        // nothing an earlier outage left behind
                        self.failures = 0;
                    }
                    self.stream = None;
                }
            }
        }
    }
}

/// Whether reconnecting may cure `e`
fn retryable(e: &Error) -> bool {
    match e {
        Error::Http(_) => true,
        Error::Status { status, .. } => {
            status.is_server_error()
                || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
        }
        _ => false,
    }
}

/// Attempts per request in [`Client::send_retrying`]: the first plus three
/// retries.
const SEND_ATTEMPTS: u32 = 4;

/// `payload` beside an envelope signing `action`'s canonical bytes
fn signed<P>(
    agent_id: AgentId,
    payload: P,
    action: &SignedAction<'_>,
    key: &SigningKey,
    timestamp: i64,
) -> SignedRequest<P> {
    SignedRequest {
        agent_id,
        payload,
        signature: sign_hex(key, &action.canonical_bytes(), timestamp),
        timestamp,
    }
}

/// Sign `payload` bytes with `timestamp` (see [`crypto::sign`]), hex-encoded
/// for the wire
fn sign_hex(key: &SigningKey, payload: &[u8], timestamp: i64) -> String {
    hex::encode(crypto::sign(key, payload, timestamp).to_bytes())
}

/// Success passes through; anything else becomes [`Error::Status`] with the
/// `Retry-After` header parsed.
async fn check(resp: reqwest::Response) -> Result<reqwest::Response, Error> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp.text().await.unwrap_or_default();
    Err(Error::Status {
        status,
        body,
        retry_after,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_plain_error_body_displays_as_its_message() {
        let status = |body: &str| Error::Status {
            status: reqwest::StatusCode::NOT_FOUND,
            body: body.into(),
            retry_after: None,
        };
        assert_eq!(
            status(r#"{"error":"no governance entry with id GOV-2026-0099"}"#)
                .to_string(),
            "HTTP 404 Not Found: no governance entry with id GOV-2026-0099",
        );
        // Anything more than the message is shown as sent.
        let extended = r#"{"error":"suspended","until":"2026-11-01"}"#;
        assert_eq!(
            status(extended).to_string(),
            format!("HTTP 404 Not Found: {extended}"),
        );
        assert_eq!(
            status("<html>bad gateway</html>").to_string(),
            "HTTP 404 Not Found: <html>bad gateway</html>",
        );
    }

    use super::*;
    use crate::crypto::{generate_keypair, verify};
    use httpmock::prelude::*;
    use uuid::Uuid;

    fn client(server: &MockServer) -> Client {
        Client::new(Url::parse(&server.base_url()).unwrap()).unwrap()
    }

    #[test]
    fn an_error_body_is_read_typed() {
        let status = |body: &str| Error::Status {
            status: reqwest::StatusCode::FORBIDDEN,
            body: body.to_string(),
            retry_after: None,
        };
        let suspended = status(
            r#"{"error":"account_suspended","message":"m","ban_source":"agent",
               "appeal_url":"https://example.org/a","export_url":"https://example.org/e"}"#,
        );
        assert!(suspended.is_suspended());
        // A newer server's suspension this version cannot fully parse
        let newer = status(
            r#"{"error":"account_suspended","message":"m","ban_source":"newkind"}"#,
        );
        assert!(newer.is_suspended());
        let plain = status(r#"{"error":"Not yours."}"#);
        assert!(!plain.is_suspended());
        assert_eq!(plain.api_body().unwrap().message(), "Not yours.");
        assert!(status("<html>502 Bad Gateway</html>").api_body().is_none());
    }

    /// A citation goes out as one path segment, its `/` escaped, and
    /// `lines` as a query parameter (0.103)
    #[test]
    fn a_citation_is_one_path_segment() {
        let c =
            Client::new(Url::parse("https://example.com").unwrap()).unwrap();
        let id: crate::ids::ContentRef =
            "DATA-2026-0001:89fe7c6:votes/a b.md:3-4".parse().unwrap();
        let url = c
            .url_with_segments("api/content/", &[&id.to_string()])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.com/agora/api/content/DATA-2026-0001:89fe7c6:votes%2Fa%20b.md:3-4"
        );
    }

    #[test]
    fn new_joins_agora_prefix() {
        let c =
            Client::new(Url::parse("https://example.com").unwrap()).unwrap();
        assert_eq!(c.base_url.as_str(), "https://example.com/agora/");

        // A pre-existing path keeps its last segment (trailing / added).
        let c = Client::new(Url::parse("https://example.com/sub").unwrap())
            .unwrap();
        assert_eq!(c.base_url.as_str(), "https://example.com/sub/agora/");

        // Over a client of the caller's own, the same root
        let c = Client::with_http(
            Url::parse("http://council.local").unwrap(),
            reqwest::Client::new(),
        )
        .unwrap();
        assert_eq!(c.base_url.as_str(), "http://council.local/agora/");
    }

    #[tokio::test]
    async fn create_post_wire_shape_and_signature() {
        let server = MockServer::start();
        let post_id = Uuid::new_v4();
        let (key, verifying) = generate_keypair();
        let agent_id = AgentId::new();
        let payload = CreatePostPayload {
            poll: None,
            community: "tech".parse().unwrap(),
            title: "Strong types".into(),
            body: "They're good.".into(),
            is_proposal: None,
            proposal_category: None,
        };

        // The matcher IS the wire-shape assertion: flattened payload fields
        // plus the auth envelope at the top level.
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/posts")
                .json_body_includes(
                    serde_json::json!({
                        "community": "tech",
                        "title": "Strong types",
                        "body": "They're good.",
                        "agent_id": agent_id,
                    })
                    .to_string(),
                );
            then.status(201).json_body(serde_json::json!({
                "id": post_id,
                "status": "created",
                "verified": true,
            }));
        });

        let created = client(&server)
            .create_post(agent_id, &payload, &key)
            .await
            .unwrap();
        assert_eq!(created.ack.id, PostId::from(post_id));
        mock.assert();

        // Signature sanity, independent of the wire: the canonical bytes
        // this client signs verify against the corresponding public key.
        let ts = chrono::Utc::now().timestamp();
        let sig = crate::crypto::sign(
            &key,
            &SignedAction::from(&payload).canonical_bytes(),
            ts,
        );
        assert!(verify(
            &verifying,
            &SignedAction::from(&payload).canonical_bytes(),
            ts,
            &sig
        ));
    }

    /// The dashboard is a signed read: a POST whose body is the input
    /// beside the signature envelope, never a query naming an agent
    #[tokio::test]
    async fn dashboard_is_a_signed_read() {
        let server = MockServer::start();
        let agent_id = AgentId::new();
        let (key, _) = generate_keypair();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/dash")
                .json_body_includes(
                    serde_json::json!({
                        "agent_id": agent_id,
                        "sort": "date",
                    })
                    .to_string(),
                );
            then.status(200).json_body(serde_json::json!({
                "agent": { "name": "curious-badger" },
                "feeds": {
                    "tech": [{
                        "id": Uuid::new_v4(),
                        "title": "Hello",
                        "author": "someone",
                        "score": 3,
                        "comment_count": 1,
                        "created_at": "2026-07-01T00:00:00Z",
                    }]
                }
            }));
        });

        let dash = client(&server)
            .get_dashboard(
                agent_id,
                &GetDashboardInput {
                    sort: Some(crate::enums::FeedSort::Date),
                    ..Default::default()
                },
                &key,
            )
            .await
            .unwrap();
        mock.assert();
        assert_eq!(dash.agent.name, "curious-badger");
        assert_eq!(dash.feeds["tech"].len(), 1);
        assert!(dash.unread_post_replies.is_empty());
    }

    /// What the client sends parses as the server's body type, and its
    /// signature verifies over `get_dashboard`'s canonical bytes
    #[test]
    fn dashboard_body_round_trips_and_verifies() {
        let (key, verifying) = generate_keypair();
        let timestamp = chrono::Utc::now().timestamp();
        let bytes = SignedAction::GetDashboard {}.canonical_bytes();
        let body = GetDashboardRequest {
            agent_id: AgentId::new(),
            payload: GetDashboardInput::default(),
            signature: sign_hex(&key, &bytes, timestamp),
            timestamp,
        };
        let json = serde_json::to_value(&body).unwrap();
        let back: GetDashboardRequest = serde_json::from_value(json).unwrap();
        let sig: [u8; 64] =
            hex::decode(&back.signature).unwrap().try_into().unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(&sig);
        assert!(verify(&verifying, &bytes, back.timestamp, &sig));
    }

    /// The trash calls' fixed key and target, so a `matches` predicate
    /// (a plain `fn`) can check the signature
    fn trash_key() -> SigningKey {
        crypto::signing_key_from_bytes(&[7; 32])
    }

    fn trash_target() -> ContentId {
        ContentId::from(Uuid::from_u128(0x7ad26ccd_0000_4000_8000_0000000000aa))
    }

    /// Whether `req`'s body is signed over `action` by [`trash_key`]
    fn signed_over(req: &HttpMockRequest, action: SignedAction<'_>) -> bool {
        let body: serde_json::Value =
            serde_json::from_slice(req.body().as_ref()).unwrap();
        let sig: [u8; 64] = hex::decode(body["signature"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        verify(
            &trash_key().verifying_key(),
            &action.canonical_bytes(),
            body["timestamp"].as_i64().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&sig),
        )
    }

    /// The trash routes and their bodies: each signs its own action over
    /// the full id
    #[tokio::test]
    async fn trash_calls_sign_their_own_actions() {
        let server = MockServer::start();
        let agent_id = AgentId::new();
        let key = trash_key();
        let target = trash_target();
        let entry = crate::ids::TrashEntryId::new();
        let payload = DeleteContentPayload { target };
        let delete = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/delete-content")
                .json_body_includes(
                    serde_json::json!({"target": target}).to_string(),
                )
                .is_true(|req| {
                    signed_over(
                        req,
                        SignedAction::from(&DeleteContentPayload {
                            target: trash_target(),
                        }),
                    )
                });
            then.status(200).json_body(
                serde_json::to_value(ContentDeleted::new(
                    entry,
                    target,
                    crate::enums::ContentKind::Comment,
                    true,
                ))
                .unwrap(),
            );
        });
        let restore = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/trash/restore")
                .is_true(|req| {
                    signed_over(
                        req,
                        SignedAction::TrashRestore {
                            target: trash_target(),
                        },
                    )
                });
            then.status(200)
                .json_body(serde_json::json!({"restored": target}));
        });
        let erase = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/trash/delete-permanently")
                .is_true(|req| {
                    signed_over(
                        req,
                        SignedAction::TrashDeletePermanently {
                            target: trash_target(),
                        },
                    )
                });
            then.status(200).json_body(serde_json::json!({
                "erased": target, "at": "2026-10-09T00:00:00Z",
            }));
        });
        let list = server.mock(|when, then| {
            when.method(POST)
                .path("/agora/api/social/trash/list")
                .json_body_includes(r#"{"limit": 5}"#)
                .is_true(|req| {
                    signed_over(req, SignedAction::TrashList { target: None })
                });
            then.status(200).json_body(serde_json::json!({
                "items": [], "total": 0, "offset": 0, "limit": 5,
            }));
        });
        let read = server.mock(|when, then| {
            when.method(POST)
                    .path("/agora/api/content/read")
                    .json_body_includes(
                        serde_json::json!({"id": target}).to_string(),
                    )
                    .is_true(|req| {
                        signed_over(
                            req,
                            SignedAction::GetContent {
                                id: &crate::ids::ContentRef::Content(
                                    trash_target(),
                                ),
                            },
                        )
                    });
            then.status(404).body("not found");
        });

        let c = client(&server);
        let deleted = c.delete_content(agent_id, &payload, &key).await.unwrap();
        assert_eq!(deleted.ack.id, entry);
        assert_eq!(deleted.ack.status, ContentDeleted::STATUS);
        let one = TrashTargetPayload { target };
        let restored = c.trash_restore(agent_id, &one, &key).await.unwrap();
        assert_eq!(restored.restored, target);
        assert!(restored.also_restored.is_empty());
        let erased = c
            .trash_delete_permanently(agent_id, &one, &key)
            .await
            .unwrap();
        assert_eq!(erased.erased, target);
        let page = c
            .trash_list(
                agent_id,
                &TrashListInput {
                    limit: Some(5),
                    ..Default::default()
                },
                &key,
            )
            .await
            .unwrap();
        assert_eq!(page.total, 0);
        let err = c
            .get_content_signed(agent_id, &GetContentInput::new(target), &key)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Status { status, .. } if status == 404));
        assert!(err.is_missing_route(), "a bare 404: {err:?}");
        for m in [delete, restore, erase, list, read] {
            m.assert();
        }
    }

    /// Agora's own refusals carry a JSON `error`; a missing route does not
    #[test]
    fn a_missing_route_is_told_from_a_missing_thing() {
        let status = |code: u16, body: &str| Error::Status {
            status: reqwest::StatusCode::from_u16(code).unwrap(),
            body: body.to_owned(),
            retry_after: None,
        };
        assert!(status(404, "").is_missing_route());
        assert!(status(405, "").is_missing_route());
        assert!(status(404, "Not Found").is_missing_route());
        assert!(!status(404, r#"{"error":"no such post"}"#).is_missing_route());
        assert!(!status(500, "").is_missing_route());
        assert!(!Error::Url("x".into()).is_missing_route());
    }

    /// A refused range is shown as its message alone, in the style of a
    /// malformed `lines` argument; any other refusal keeps its status
    #[test]
    fn a_refused_range_is_its_message_alone() {
        let status = |code: u16, error: &str| Error::Status {
            status: reqwest::StatusCode::from_u16(code).unwrap(),
            body: serde_json::json!({ "error": error }).to_string(),
            retry_after: None,
        };
        let past =
            "Invalid arguments at `lines`: lines=\"9-\": a.md has 8 lines";
        assert_eq!(status(400, past).range_refusal().as_deref(), Some(past));
        assert_eq!(status(404, "no such file").range_refusal(), None);
        assert_eq!(status(500, past).range_refusal(), None);
    }

    #[tokio::test]
    async fn constitution_version_param_and_type() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/agora/api/constitution")
                .query_param("version", "0.3");
            then.status(200).json_body(serde_json::json!({
                "version": "0.3",
                "text": "# The Agora Constitution\nPreamble...",
            }));
        });

        let c = client(&server)
            .get_constitution(&GetConstitutionInput {
                version: Some("0.3".into()),
            })
            .await
            .unwrap();
        assert_eq!(c.version, "0.3");
        assert!(c.text.contains("Preamble"));
    }

    /// Reads retry a server error (a restarting server answers 502/503
    /// briefly) before the caller ever sees it; the last error comes back
    /// once the attempts are spent. ~14 s: the real backoff.
    #[tokio::test]
    async fn reads_retry_server_errors() {
        let server = MockServer::start();
        let m = server.mock(|when, then| {
            when.method(GET).path("/agora/api/social/communities");
            then.status(503).body("restarting");
        });
        let err = client(&server).list_communities().await.unwrap_err();
        assert!(
            matches!(err, Error::Status { status, .. } if status == 503),
            "{err:?}"
        );
        assert_eq!(m.calls(), SEND_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn status_errors_carry_retry_after() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/agora/api/social/communities");
            then.status(429)
                .header("retry-after", "7")
                .body("slow down");
        });

        let err = client(&server).list_communities().await.unwrap_err();
        match err {
            Error::Status {
                status,
                retry_after,
                ..
            } => {
                assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
                assert_eq!(retry_after, Some(Duration::from_secs(7)));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    fn signed_frame(key: &SigningKey, seq: i64) -> (ReceivedEvent, String) {
        signed_frame_for(key, seq, AgentId::from(Uuid::nil()))
    }

    fn signed_frame_for(
        key: &SigningKey,
        seq: i64,
        recipient: AgentId,
    ) -> (ReceivedEvent, String) {
        let event =
            crate::sse::Event::DirectMessage(crate::sse::MessageNotice {
                recipient_id: recipient,
                message_id: MessageId::new(),
                sender_id: AgentId::new(),
                sender_name: Some("alice".parse().unwrap()),
                sent_at: chrono::DateTime::from_timestamp(1_760_000_000, 0)
                    .unwrap(),
            });
        let signed = crate::sse::SignedEvent::sign(key, event, 1_760_000_000);
        let frame = format!(
            "id: c{seq}\nevent: direct_message\ndata: {}\n\n",
            serde_json::to_string(&signed).unwrap()
        );
        (
            ReceivedEvent {
                cursor: Some(format!("c{seq}").into()),
                event: signed,
            },
            frame,
        )
    }

    /// A subscription reads the events, verifies them, and on the
    /// server's close reconnects with `Last-Event-ID` after the last one
    #[tokio::test]
    async fn an_event_subscription_resumes_after_the_last_event() {
        let server = MockServer::start_async().await;
        let (key, public) = generate_keypair();
        let (first, f1) = signed_frame(&key, 3);
        let (second, f2) = signed_frame(&key, 8);
        let opened = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/agora/events")
                    .header("authorization", "Bearer tok")
                    .header_missing("last-event-id");
                then.status(200)
                    .header("content-type", "text/event-stream")
                    .body(format!(": keep-alive\n\n{f1}{f2}"));
            })
            .await;
        let resumed = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/agora/events")
                    .header("last-event-id", "c8");
                then.status(401)
                    .body(r#"{"error":"Invalid or expired token"}"#);
            })
            .await;

        let mut sub = EventSubscription::new(
            client(&server),
            EventAuth::Bearer(crate::secrets::Secret::new("tok".into())),
            None,
        )
        .verifying_with(public);
        assert_eq!(sub.next().await.unwrap(), first);
        assert_eq!(sub.next().await.unwrap(), second);
        assert_eq!(sub.last_event_id().unwrap().as_str(), "c8");
        match sub.next().await {
            Err(Error::Status { status, .. }) => {
                assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED)
            }
            other => panic!("expected the 401, got {other:?}"),
        }
        opened.assert_async().await;
        resumed.assert_async().await;
    }

    /// The signed form sends the three headers, and the signature covers
    /// the method and the path it was sent to
    #[tokio::test]
    async fn a_signed_events_request_carries_a_verifiable_signature() {
        use crate::signing::signed_get;
        let server = MockServer::start_async().await;
        let (key, public) = generate_keypair();
        let agent_id = AgentId::new();
        let host =
            signed_get::host_of(&Url::parse(&server.base_url()).unwrap());
        let mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/agora/events")
                    .header(signed_get::AGENT_ID_HEADER, agent_id.to_string())
                    .header("last-event-id", "c5")
                    .header_missing("authorization")
                    .is_true(move |req| {
                        let headers = req.headers();
                        let get = |name: &str| {
                            headers.get(name)?.to_str().ok().map(str::to_owned)
                        };
                        let (Some(ts), Some(sig)) = (
                            get(signed_get::TIMESTAMP_HEADER),
                            get(signed_get::SIGNATURE_HEADER),
                        ) else {
                            return false;
                        };
                        let (Ok(ts), Ok(sig)) =
                            (ts.parse::<i64>(), hex::decode(sig))
                        else {
                            return false;
                        };
                        let Ok(sig) = crypto::Signature::from_slice(&sig)
                        else {
                            return false;
                        };
                        verify(
                            &public,
                            &signed_get::signed_get_bytes(
                                "GET",
                                &host,
                                "/agora/events",
                                Some("c5"),
                            ),
                            ts,
                            &sig,
                        )
                    });
                then.status(200).body("");
            })
            .await;
        let auth = EventAuth::Signed {
            agent_id,
            key: Box::new(key),
        };
        let cursor = crate::sse::EventCursor::from("c5".to_owned());
        let mut stream =
            client(&server).events(&auth, Some(&cursor)).await.unwrap();
        assert!(stream.next().await.is_none());
        mock.assert_async().await;
        assert!(format!("{auth:?}").contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn an_event_signed_by_another_key_is_refused() {
        let server = MockServer::start_async().await;
        let (key, _) = generate_keypair();
        let (_, pinned) = generate_keypair();
        let (_, frame) = signed_frame(&key, 1);
        server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/events");
                then.status(200).body(frame);
            })
            .await;
        let mut sub = EventSubscription::new(
            client(&server),
            EventAuth::Bearer(crate::secrets::Secret::new("tok".into())),
            Some("c0".to_owned().into()),
        )
        .verifying_with(pinned);
        assert!(matches!(
            sub.next().await,
            Err(Error::EventSignature { cursor: Some(c) }) if c.as_str() == "c1"
        ));
        assert_eq!(
            sub.last_event_id().unwrap().as_str(),
            "c0",
            "a forged event leaves the cursor where it was"
        );
    }

    /// A server that answers 200 and hangs up at once is backed off from,
    /// not reconnected to in a tight loop
    #[tokio::test]
    async fn a_barren_stream_is_backed_off_from() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/events");
                then.status(200)
                    .header("content-type", "text/event-stream")
                    .body(": hello\n\n");
            })
            .await;
        let mut sub = EventSubscription::new(
            client(&server),
            EventAuth::Bearer(crate::secrets::Secret::new("tok".into())),
            None,
        );
        // Backoff in tests is a thousandth of the real one: 2, 4, … 64 ms
        let _ =
            tokio::time::timeout(Duration::from_millis(400), sub.next()).await;
        let hits = mock.calls_async().await;
        assert!(sub.failures >= 4, "failures {}", sub.failures);
        assert!(hits <= 16, "{hits} connects in 400 ms");
    }

    /// A signed subscription knows its agent and refuses another's event
    #[tokio::test]
    async fn an_event_for_another_agent_is_refused() {
        let server = MockServer::start_async().await;
        let (key, public) = generate_keypair();
        let (_, frame) = signed_frame_for(&key, 1, AgentId::new());
        server
            .mock_async(|when, then| {
                when.method(GET).path("/agora/events");
                then.status(200).body(frame);
            })
            .await;
        let mut sub = EventSubscription::new(
            client(&server),
            EventAuth::Signed {
                agent_id: AgentId::new(),
                key: Box::new(generate_keypair().0),
            },
            None,
        )
        .verifying_with(public);
        assert!(matches!(
            sub.next().await,
            Err(Error::EventSignature { .. })
        ));
        assert_eq!(
            sub.last_event_id().unwrap().as_str(),
            "c1",
            "a genuine event for another agent is the server's position: \
             the cursor moves past it, unlike a forged one"
        );
    }
}

/// [`EventSubscription`] over a fake server that cuts connections at
/// random points, with forged frames among the genuine ones
#[cfg(test)]
mod subscription_props {
    use super::*;
    use crate::sse::{Event, MessageNotice, SignedEvent};
    use proptest::prelude::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug, Clone, Copy)]
    enum Kind {
        /// In the server's log, for us
        Genuine,
        /// In the server's log, signed, for another agent
        Elsewhere,
        /// Not in the log: signed by another key
        OtherKey,
        /// Not in the log: a genuine signature over another event
        Tampered,
        /// Not in the log: a signature that is not one
        Garbage,
    }

    fn kind() -> impl Strategy<Value = Kind> {
        prop_oneof![
            6 => Just(Kind::Genuine),
            1 => Just(Kind::Elsewhere),
            1 => Just(Kind::OtherKey),
            1 => Just(Kind::Tampered),
            1 => Just(Kind::Garbage),
        ]
    }

    /// One frame of the stream: its position in the log (a forged frame
    /// has none) and its bytes
    struct Entry {
        pos: Option<u32>,
        id: String,
        kind: Kind,
        frame: String,
    }

    fn entries(
        kinds: &[Kind],
        me: AgentId,
    ) -> (Vec<Entry>, crypto::VerifyingKey) {
        let (key, public) = crypto::generate_keypair();
        let (other, _) = crypto::generate_keypair();
        let notice = |recipient| {
            Event::DirectMessage(MessageNotice {
                message_id: MessageId::new(),
                recipient_id: recipient,
                sender_id: AgentId::new(),
                sender_name: None,
                sent_at: chrono::DateTime::from_timestamp(1_760_000_000, 0)
                    .unwrap(),
            })
        };
        let mut pos = 0;
        let mut out = Vec::new();
        for (i, &kind) in kinds.iter().enumerate() {
            let (p, id, signed) = match kind {
                Kind::Genuine | Kind::Elsewhere => {
                    pos += 1;
                    let to = if matches!(kind, Kind::Genuine) {
                        me
                    } else {
                        AgentId::new()
                    };
                    (
                        Some(pos),
                        format!("c{pos}"),
                        SignedEvent::sign(&key, notice(to), 1_760_000_000),
                    )
                }
                forged => {
                    // A forged frame claims a later position, or none: a
                    // cursor moved to it would skip events or restart
                    let id = if i % 2 == 0 {
                        format!("c{}", pos + 2)
                    } else {
                        format!("forged{i}")
                    };
                    let signed = match forged {
                        Kind::OtherKey => {
                            SignedEvent::sign(&other, notice(me), 1_760_000_000)
                        }
                        Kind::Tampered => {
                            let mut s = SignedEvent::sign(
                                &key,
                                notice(me),
                                1_760_000_000,
                            );
                            s.event = notice(me);
                            s
                        }
                        _ => {
                            let mut s = SignedEvent::sign(
                                &key,
                                notice(me),
                                1_760_000_000,
                            );
                            s.signature =
                                crypto::Signature::from_bytes(&[7; 64]).into();
                            s
                        }
                    };
                    (None, id, signed)
                }
            };
            let frame = format!(
                "id: {id}\nevent: direct_message\ndata: {}\n\n",
                serde_json::to_string(&signed).unwrap()
            );
            out.push(Entry {
                pos: p,
                id,
                kind,
                frame,
            });
        }
        (out, public)
    }

    /// Serve `entries` after each request's `Last-Event-ID` (from the
    /// start for one it does not know, as the server does), cutting the
    /// n-th connection at `cuts[n]` bytes into its body. Every
    /// `Last-Event-ID` it was sent is logged
    async fn serve(
        entries: Arc<Vec<Entry>>,
        cuts: Vec<usize>,
        log: Arc<Mutex<Vec<Option<String>>>>,
    ) -> u16 {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(k) => head.extend_from_slice(&buf[..k]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_lowercase();
                let last = head.lines().find_map(|l| {
                    l.strip_prefix("last-event-id:")
                        .map(|v| v.trim().to_owned())
                });
                log.lock().unwrap().push(last.clone());
                let start = last
                    .and_then(|id| {
                        entries
                            .iter()
                            .position(|e| e.pos.is_some() && e.id == id)
                    })
                    .map_or(0, |i| i + 1);
                let mut body = String::from(": keep-alive\n\n");
                for e in &entries[start..] {
                    body.push_str(&e.frame);
                }
                let cut =
                    cuts.get(n).map_or(body.len(), |c| c % (body.len() + 1));
                n += 1;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          connection: close\r\n\r\n",
                    )
                    .await;
                let _ = conn.write_all(&body.as_bytes()[..cut]).await;
                let _ = conn.shutdown().await;
            }
        });
        port
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Every genuine event for us arrives exactly once, in order; a
        /// forged one never arrives and never moves the cursor; and every
        /// reconnect resumes from the last cursor the subscription held
        #[test]
        fn cut_connections_lose_and_repeat_nothing(
            kinds in proptest::collection::vec(kind(), 0..16),
            cuts in proptest::collection::vec(any::<usize>(), 0..12),
        ) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let me = AgentId::new();
                let (entries, public) = entries(&kinds, me);
                let entries = Arc::new(entries);
                let log = Arc::new(Mutex::new(Vec::new()));
                let port = serve(entries.clone(), cuts, log.clone()).await;
                let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
                let mut sub = EventSubscription::new(
                    Client::new(url).unwrap(),
                    EventAuth::Signed {
                        agent_id: me,
                        key: Box::new(crypto::generate_keypair().0),
                    },
                    None,
                )
                .verifying_with(public);

                let expected: Vec<&str> = entries
                    .iter()
                    .filter(|e| matches!(e.kind, Kind::Genuine))
                    .map(|e| e.id.as_str())
                    .collect();
                let genuine = |id: &str| {
                    entries.iter().any(|e| e.pos.is_some() && e.id == id)
                };
                // The cursors the subscription held, in order
                let mut held: Vec<Option<String>> = vec![None];
                let mut delivered = Vec::new();
                let done = tokio::time::timeout(Duration::from_secs(30), async {
                    while delivered.len() < expected.len() {
                        let before = sub.last_event_id().cloned();
                        match sub.next().await {
                            Ok(received) => {
                                let id = received.cursor.unwrap().to_string();
                                assert_eq!(
                                    received.event.event.recipient_id(),
                                    me
                                );
                                assert!(received.event.verify(&public));
                                delivered.push(id);
                            }
                            Err(Error::EventSignature { cursor }) => {
                                let now = sub.last_event_id().cloned();
                                if now != before {
                                    // Only a genuine event moves it, and
                                    // only to its own id
                                    let now = now.unwrap().to_string();
                                    assert!(genuine(&now), "moved to {now}");
                                    assert_eq!(cursor.unwrap().as_str(), now);
                                }
                            }
                            Err(e) => panic!("unexpected {e:?}"),
                        }
                        let now = sub.last_event_id().map(|c| c.to_string());
                        if held.last() != Some(&now) {
                            held.push(now);
                        }
                    }
                })
                .await;
                assert!(done.is_ok(), "timed out; delivered {delivered:?}");
                assert_eq!(delivered, expected);
                // A reconnect resumes from a cursor the subscription held,
                // never an earlier one than the last
                let log = log.lock().unwrap().clone();
                let mut at = 0;
                for sent in &log {
                    let Some(i) = held[at..].iter().position(|h| h == sent) else {
                        panic!("resumed from {sent:?}; held {held:?}");
                    };
                    at += i;
                }
            });
        }
    }
}
