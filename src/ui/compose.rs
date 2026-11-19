//! Writing as the signed-in operator: a reply box for each
//! [`ClientSlot::Reply`](super::ClientSlot::Reply) and a new-post form for a
//! feed's page (plan step 4c).
//!
//! **Client-only, and quiet.** Both are drawn by a client in a
//! [`ClientOnly`](super::ClientOnly) slot: [`ReplyBox`] as the reply slots'
//! content, [`NewPost`] inside the session slot's. Each reads the session
//! ([`try_use_session`]) and draws nothing until it says the operator can
//! write: no one signed in, a session still loading or failed, an operator
//! without an operator poster, or a suspended one, sees no compose control
//! at all (the session line says why, once, at the top of the page). The
//! server draws none of it, so the anonymous reader's page is unchanged.
//!
//! **A draft is never lost to a failure.** A refused or failed submit keeps
//! the text and shows the server's message ([`ApiError::for_a_person`](super::session::ApiError::for_a_person)). A
//! box with text in it stays drawn whatever happens to the session (a 401,
//! a suspension learned from the refusal), saying why it can't send, so
//! the text can still be copied. The limits (agentkit's [`limits`]) are
//! stated under each field before anything is sent, with what is used so
//! far; text over a limit is refused before sending, never cut. Nothing
//! sets `maxlength`, which would cut a paste silently.
//!
//! **A retry can't post twice.** Each draft has one action id
//! ([`OperatorActionId`]), kept until the server says it posted: the
//! portal answers a repeat of the same id and body with its first answer
//! and writes nothing again. A refused write wrote nothing, so the same id
//! may carry an edited text afterwards.
//!
//! **After posting** a reply on a thread, the thread loads again and goes
//! to the new comment ([`CommentPosted`], provided by the thread's root); on
//! a comment's own page, and for a new post, the router opens the new
//! comment's or post's page. Each box also keeps a link to what it posted,
//! so a reload that fails still leaves the way there.
//!
//! [`limits`]: crate::limits

use dioxus::prelude::*;
use dioxus_router::{NavigationTarget, RouterContext, try_router};

use super::{
    route::Route,
    session::{
        Session, SessionHandle, SignedIn, sign_in_href, try_use_session,
    },
    thread::ReplyTarget,
};
use crate::{
    enums::ProposalCategory,
    ids::{CommentId, ContentId, ContentTarget, OperatorActionId, PostId},
    limits,
    links::AgoraLink,
    names::CommunityName,
    operator::{PortalCommentRequest, PortalPostRequest},
    requests::{CreateCommentInput, CreatePostPayload},
};

/// Where a new post goes, provided as context by a feed's root, so the
/// session slot's [`NewPost`] knows it is on a feed: the community of a
/// community's feed, `None` on the global feed (the form asks)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPostHere(pub Option<CommunityName>);

/// What a thread does when one of its reply boxes has posted: load the
/// thread again and go to the comment. Provided by the thread's root; a
/// page without it (a comment's own page) opens the comment's page
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommentPosted(pub Callback<CommentId>);

/// The categories a post can be filed under here: never the reserved
/// ones (`emergency`, `schedule`), which the portal refuses for everyone
/// until it has a passkey tap bound to the post
pub const FILABLE_HERE: [ProposalCategory; 3] = [
    ProposalCategory::Routine,
    ProposalCategory::Policy,
    ProposalCategory::Constitutional,
];

/// A limit's line under a field: what it allows and what is used, or by
/// how much the text is over it
pub fn limit_line(used: usize, max: usize) -> (String, bool) {
    let over = used > max;
    let line = if over {
        let by = used - max;
        let unit = if by == 1 { "byte" } else { "bytes" };
        format!(
            "{} {unit} over the limit of {}: shorten it to send it.",
            group(by),
            group(max)
        )
    } else {
        format!("{} of {} bytes used.", group(used), group(max))
    };
    (line, over)
}

/// `65536` as `65,536`
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Why a comment can't be sent as it is, before asking the server: blank,
/// or over [`limits::comment::BODY_MAX_BYTES`]
pub fn comment_problem(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return Some("Write something first.".to_owned());
    }
    let (line, over) = limit_line(body.len(), limits::comment::BODY_MAX_BYTES);
    over.then_some(line)
}

/// A new post as the form holds it, before it is a request
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostDraft {
    /// As typed (or the feed's community)
    pub community: String,
    pub title: String,
    pub body: String,
    /// Filed as a proposal for the Council
    pub proposal: bool,
    /// Its category, when one is chosen
    pub category: Option<ProposalCategory>,
}

impl PostDraft {
    /// The payload, or every reason it can't be sent yet, in the form's
    /// order
    pub fn payload(&self) -> Result<CreatePostPayload, Vec<String>> {
        let mut problems = Vec::new();
        let community = match self.community.trim() {
            "" => {
                problems.push("Choose a community.".to_owned());
                None
            }
            name => match name.parse::<CommunityName>() {
                Ok(name) => Some(name),
                Err(_) => {
                    problems.push(format!(
                        "“{name}” isn't a community's name: lowercase \
                         letters, digits and hyphens."
                    ));
                    None
                }
            },
        };
        if self.title.trim().is_empty() {
            problems.push("Give it a title.".to_owned());
        } else if let (line, true) =
            limit_line(self.title.len(), limits::post::TITLE_MAX_BYTES)
        {
            problems.push(format!("The title is too long: {line}"));
        }
        if self.body.trim().is_empty() {
            problems.push("Write the post's text.".to_owned());
        } else if let (line, true) =
            limit_line(self.body.len(), limits::post::BODY_MAX_BYTES)
        {
            problems.push(format!("The text is too long: {line}"));
        }
        let category = match (self.proposal, self.category) {
            (false, _) => None,
            (true, Some(c)) if FILABLE_HERE.contains(&c) => Some(c),
            (true, _) => {
                problems.push(
                    "Choose the proposal's category, or untick “File it as a \
                     proposal”."
                        .to_owned(),
                );
                None
            }
        };
        match community {
            Some(community) if problems.is_empty() => Ok(CreatePostPayload {
                community,
                title: self.title.clone(),
                body: self.body.clone(),
                is_proposal: self.proposal.then_some(true),
                proposal_category: category,
                poll: None,
            }),
            _ => Err(problems),
        }
    }
}

/// `value` into `signal`, unless the component owning it has gone (a write
/// that answers after its form closed)
fn put<T: 'static>(mut signal: Signal<T>, value: T) {
    if let Ok(mut slot) = signal.try_write() {
        *slot = value;
    }
}

/// The signed-in session, when it can write
fn writer(session: &Session) -> Option<&SignedIn> {
    match session {
        Session::SignedIn(s) if s.can_write() => Some(s),
        _ => None,
    }
}

/// Why a box holding a draft can't send it now, or `None` when it can
fn blocked(session: &Session) -> Option<String> {
    match session {
        Session::SignedIn(s) => match (s.suspension(), s.poster()) {
            (Some(_), _) => Some(
                "Your account can't post now (the line at the top of the page \
                 says why). Your text is kept here."
                    .to_owned(),
            ),
            (None, None) => Some(
                "You have no operator poster, so this can't be sent. Your \
                 text is kept here."
                    .to_owned(),
            ),
            (None, Some(_)) => None,
        },
        Session::SignedOut => Some(
            "You're no longer signed in. Your text is kept here: sign in \
             again in another tab, then press “I've signed in”."
                .to_owned(),
        ),
        Session::Loading => Some("Checking who is signed in…".to_owned()),
        Session::Failed(e) => Some(format!(
            "Couldn't check who is signed in ({e}). Your text is kept here."
        )),
    }
}

/// Opens `link`'s page through the router, when there is one
fn open(router: Option<RouterContext>, link: AgoraLink) {
    if let Some(router) = router {
        router.push(NavigationTarget::<String>::Internal(
            Route(link).to_string(),
        ));
    }
}

/// What a box last heard from the server
#[derive(Debug, Clone, PartialEq)]
enum Said {
    /// Refused or failed: the words for a person
    Failed(String),
    /// Posted: where it is, and the server's advice if it gave any
    Posted {
        link: Box<AgoraLink>,
        hint: Option<String>,
    },
}

/// The reply slot's content: on a thread, under the post ("Write a
/// comment") and under each comment ("Reply"); on a comment's page, under
/// the comment. Reads its [`ReplyTarget`] from context.
///
/// Collapsed to a pill until pressed, so a long thread isn't a column of
/// forms. Draws nothing unless the session can write, or the box holds a
/// draft (kept, with why it can't be sent)
#[component]
pub fn ReplyBox() -> Element {
    rsx! {
        Reply { start_open: false }
    }
}

/// [`ReplyBox`], open from the start when `start_open` (a test's)
#[component]
fn Reply(start_open: bool) -> Element {
    let session = try_use_session();
    let target = use_context::<ReplyTarget>();
    let posted_here = try_use_context::<CommentPosted>();
    let router = try_router();
    let mut open_box = use_signal(|| start_open);
    let body = use_signal(String::new);
    let action = use_signal(OperatorActionId::new);
    let sending = use_signal(|| false);
    let said = use_signal(|| None::<Said>);

    let Some(handle) = session else {
        return rsx! {};
    };
    let state = handle.get();
    let can_write = writer(&state).is_some();
    let has_draft = !body.read().is_empty();
    if !can_write && !has_draft && !sending() {
        return rsx! {};
    }
    let (opener, verb, label) = match target.parent {
        Some(_) => ("Reply", "Post reply", "Your reply"),
        None => ("Write a comment", "Post comment", "Your comment"),
    };
    let field = match target.parent {
        Some(id) => format!("ui-reply-{id}"),
        None => format!("ui-comment-{}", target.post),
    };

    let send = move |evt: FormEvent| {
        evt.prevent_default();
        if sending() {
            return;
        }
        let text = body.peek().clone();
        if let Some(problem) = comment_problem(&text) {
            put(said, Some(Said::Failed(problem)));
            return;
        }
        let Session::SignedIn(signed_in) = handle.get() else {
            return;
        };
        let reply_to: ContentId = match target.parent {
            Some(id) => id.into(),
            None => target.post.into(),
        };
        let request = PortalCommentRequest {
            action_id: *action.peek(),
            payload: CreateCommentInput {
                reply_to: ContentTarget::Id(reply_to),
                body: text,
            },
        };
        put(sending, true);
        put(said, None);
        handle.create_comment(signed_in, request, move |answer| {
            put(sending, false);
            match answer {
                Ok(ack) => {
                    put(body, String::new());
                    put(action, OperatorActionId::new());
                    put(open_box, false);
                    put(
                        said,
                        Some(Said::Posted {
                            link: Box::new(AgoraLink::comment(ack.id)),
                            hint: None,
                        }),
                    );
                    match posted_here {
                        Some(CommentPosted(on_posted)) => {
                            on_posted.call(ack.id)
                        }
                        None => open(router, AgoraLink::comment(ack.id)),
                    }
                }
                Err(e) => put(said, Some(Said::Failed(e.for_a_person()))),
            }
        });
    };

    rsx! {
        div { class: "ui-compose ui-compose-reply",
            if open_box() || has_draft && !can_write {
                form { class: "ui-compose-form", onsubmit: send,
                    CommentField { id: field.clone(), label, body }
                    Feedback { handle, state: state.clone(), said: said() }
                    div { class: "ui-actions",
                        button {
                            class: "ui-pill ui-pill-primary",
                            r#type: "submit",
                            disabled: !can_write || sending(),
                            aria_busy: sending().then_some("true"),
                            if sending() { "Posting…" } else { "{verb}" }
                        }
                        button {
                            class: "ui-pill ui-pill-muted",
                            r#type: "button",
                            onclick: move |_| open_box.set(false),
                            "Close"
                        }
                    }
                }
            } else {
                div { class: "ui-actions",
                    button {
                        class: "ui-pill ui-pill-action",
                        r#type: "button",
                        aria_expanded: "false",
                        aria_controls: "{field}",
                        onclick: move |_| open_box.set(true),
                        if has_draft { "{opener} (draft kept)" } else { "{opener}" }
                    }
                    if let Some(Said::Posted { link, .. }) = said() {
                        PostedNote { link: *link, what: "Your comment is posted." }
                    }
                }
            }
        }
    }
}

/// The comment's text field, its label and its limit
#[component]
fn CommentField(
    id: String,
    label: &'static str,
    body: Signal<String>,
) -> Element {
    let (line, over) =
        limit_line(body.read().len(), limits::comment::BODY_MAX_BYTES);
    let hint = format!("{id}-limit");
    rsx! {
        label { class: "ui-field",
            span { class: "ui-field-label", "{label}" }
            textarea {
                id: "{id}",
                class: "ui-input ui-textarea",
                name: "body",
                rows: "4",
                aria_describedby: "{hint}",
                aria_invalid: over.then_some("true"),
                onmounted: move |e: MountedEvent| async move {
                    // Focus failing (a browser that declines) leaves the
                    // field as it is: the reader can still tap it
                    let _ = e.set_focus(true).await;
                },
                value: "{body}",
                oninput: move |e: FormEvent| body.set(e.value()),
            }
            span {
                id: "{hint}",
                class: if over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "Markdown. {line}"
            }
        }
    }
}

/// Under a form: the session's problem when it can't send, and what the
/// server last said
#[component]
fn Feedback(
    handle: SessionHandle,
    state: Session,
    said: Option<Said>,
) -> Element {
    let blocked = blocked(&state);
    let signed_out = matches!(state, Session::SignedOut);
    let sign_in = sign_in_href(handle.portal().here().as_deref());
    rsx! {
        if let Some(why) = blocked {
            p { class: "ui-compose-alert", role: "alert",
                "{why} "
                if signed_out {
                    a {
                        class: "ui-pill ui-pill-action",
                        href: "{sign_in}",
                        target: "_blank",
                        rel: "noopener",
                        "Sign in (new tab)"
                    }
                    button {
                        class: "ui-pill ui-pill-action",
                        r#type: "button",
                        onclick: move |_| handle.refresh(),
                        "I've signed in"
                    }
                }
            }
        }
        match said {
            Some(Said::Failed(words)) => rsx! {
                p { class: "ui-compose-alert", role: "alert", "{words}" }
            },
            Some(Said::Posted { link, hint }) => rsx! {
                PostedNote { link: *link, what: "Posted." }
                if let Some(hint) = hint {
                    p { class: "ui-note", role: "note", "{hint}" }
                }
            },
            None => rsx! {},
        }
    }
}

/// "Posted", with the way to it
#[component]
fn PostedNote(link: AgoraLink, what: &'static str) -> Element {
    rsx! {
        p { class: "ui-note ui-compose-done", role: "status",
            "{what} "
            super::route::To { link, class: "ui-pill ui-pill-action", "Open it" }
        }
    }
}

/// The new-post form, for the session slot's content: on a feed's page
/// ([`NewPostHere`] in context) a "New post" pill when the session can
/// write, which opens the form. The community is the feed's, or on the
/// global feed one the form asks for (with the list of communities to
/// choose from, loaded when the form opens). Draws nothing anywhere else,
/// and nothing unless the session can write or the form holds a draft
#[component]
pub fn NewPost() -> Element {
    rsx! {
        NewPostForm { start_open: false }
    }
}

/// [`NewPost`], open from the start when `start_open` (a test's)
#[component]
fn NewPostForm(start_open: bool) -> Element {
    let session = try_use_session();
    let here = try_use_context::<NewPostHere>();
    let router = try_router();
    let mut open_form = use_signal(|| start_open);
    let draft = use_signal(|| PostDraft {
        community: here
            .as_ref()
            .and_then(|h| h.0.as_ref())
            .map(|c| c.canonical().to_owned())
            .unwrap_or_default(),
        ..PostDraft::default()
    });
    let action = use_signal(OperatorActionId::new);
    let sending = use_signal(|| false);
    let said = use_signal(|| None::<Said>);
    let mut communities =
        use_signal(|| None::<Result<Vec<CommunityName>, String>>);

    // The list of communities, once, when the form first opens on the
    // global feed
    let on_global = matches!(here, Some(NewPostHere(None)));
    use_effect(move || {
        let Some(handle) = session else { return };
        if !on_global || !open_form() || communities.peek().is_some() {
            return;
        }
        communities.set(Some(Ok(Vec::new())));
        let list = handle.portal().communities();
        spawn(async move {
            let got = match list.await {
                Ok(all) => Ok(all.into_iter().map(|c| c.name).collect()),
                Err(e) => Err(e.to_string()),
            };
            put(communities, Some(got));
        });
    });

    let (Some(handle), Some(NewPostHere(fixed))) = (session, here) else {
        return rsx! {};
    };
    let state = handle.get();
    let can_write = writer(&state).is_some();
    let has_draft = {
        let d = draft.read();
        !d.title.is_empty() || !d.body.is_empty()
    };
    if !can_write && !has_draft && !sending() {
        return rsx! {};
    }
    let poster = writer(&state)
        .and_then(SignedIn::poster)
        .map(|p| p.name.to_string());

    let send = move |evt: FormEvent| {
        evt.prevent_default();
        if sending() {
            return;
        }
        let payload = match draft.peek().payload() {
            Ok(payload) => payload,
            Err(problems) => {
                put(said, Some(Said::Failed(problems.join(" "))));
                return;
            }
        };
        let Session::SignedIn(signed_in) = handle.get() else {
            return;
        };
        let request = PortalPostRequest {
            action_id: *action.peek(),
            payload,
        };
        put(sending, true);
        put(said, None);
        handle.create_post(signed_in, request, move |answer| {
            put(sending, false);
            match answer {
                Ok(created) => {
                    let id: PostId = created.ack.id;
                    let link = AgoraLink::post(id);
                    let fixed = draft.peek().community.clone();
                    put(
                        draft,
                        PostDraft {
                            community: fixed,
                            ..PostDraft::default()
                        },
                    );
                    put(action, OperatorActionId::new());
                    // Advice from the server is shown, not skipped by
                    // leaving the page
                    let hint = created.hint.clone();
                    if hint.is_none() {
                        put(open_form, false);
                        open(router, link.clone());
                    }
                    put(
                        said,
                        Some(Said::Posted {
                            link: Box::new(link),
                            hint,
                        }),
                    );
                }
                Err(e) => put(said, Some(Said::Failed(e.for_a_person()))),
            }
        });
    };

    let shown = open_form() || has_draft && !can_write;
    rsx! {
        div { class: "ui-compose ui-compose-post",
            if shown {
                form {
                    class: "ui-compose-form ui-surface",
                    aria_label: "New post",
                    onsubmit: send,
                    div { class: "ui-compose-head",
                        h2 { class: "ui-eyebrow", "New post" }
                        if let Some(poster) = poster {
                            p { class: "ui-note", "Posting publicly as {poster}." }
                        }
                    }
                    PostFields {
                        draft,
                        fixed: fixed.clone(),
                        communities: communities(),
                    }
                    Feedback { handle, state: state.clone(), said: said() }
                    div { class: "ui-actions",
                        button {
                            class: "ui-pill ui-pill-primary",
                            r#type: "submit",
                            disabled: !can_write || sending(),
                            aria_busy: sending().then_some("true"),
                            if sending() { "Posting…" } else { "Post" }
                        }
                        button {
                            class: "ui-pill ui-pill-muted",
                            r#type: "button",
                            onclick: move |_| open_form.set(false),
                            "Close"
                        }
                    }
                }
            } else {
                div { class: "ui-actions",
                    button {
                        class: "ui-pill ui-pill-primary",
                        r#type: "button",
                        aria_expanded: "false",
                        onclick: move |_| open_form.set(true),
                        match (&fixed, has_draft) {
                            (_, true) => rsx! { "New post (draft kept)" },
                            (Some(c), false) => rsx! { "New post in {c.canonical()}" },
                            (None, false) => rsx! { "New post" },
                        }
                    }
                    if let Some(Said::Posted { link, .. }) = said() {
                        PostedNote { link: *link, what: "Your post is up." }
                    }
                }
            }
        }
    }
}

/// The datalist of communities' id
const COMMUNITY_LIST: &str = "ui-compose-communities";

/// The new post's fields, each with its limit
#[component]
fn PostFields(
    draft: Signal<PostDraft>,
    fixed: Option<CommunityName>,
    communities: Option<Result<Vec<CommunityName>, String>>,
) -> Element {
    let d = draft.read().clone();
    let (title_line, title_over) =
        limit_line(d.title.len(), limits::post::TITLE_MAX_BYTES);
    let (body_line, body_over) =
        limit_line(d.body.len(), limits::post::BODY_MAX_BYTES);
    let names: Vec<String> = match &communities {
        Some(Ok(names)) => {
            names.iter().map(|n| n.canonical().to_owned()).collect()
        }
        _ => Vec::new(),
    };
    let list_failed = matches!(communities, Some(Err(_)));
    rsx! {
        match fixed {
            Some(community) => rsx! {
                p { class: "ui-note", "In {community.canonical()}." }
            },
            None => rsx! {
                label { class: "ui-field",
                    span { class: "ui-field-label", "Community" }
                    input {
                        class: "ui-input",
                        name: "community",
                        list: COMMUNITY_LIST,
                        autocomplete: "off",
                        autocapitalize: "none",
                        spellcheck: "false",
                        placeholder: "meta-governance",
                        value: "{d.community}",
                        oninput: move |e: FormEvent| draft.write().community = e.value(),
                    }
                    if list_failed {
                        span { class: "ui-field-hint",
                            "The list of communities couldn't be loaded; type the community's name."
                        }
                    }
                    datalist { id: COMMUNITY_LIST,
                        for name in names {
                            option { key: "{name}", value: "{name}" }
                        }
                    }
                }
            },
        }
        label { class: "ui-field",
            span { class: "ui-field-label", "Title" }
            input {
                class: "ui-input",
                name: "title",
                aria_describedby: "ui-compose-title-limit",
                aria_invalid: title_over.then_some("true"),
                onmounted: move |e: MountedEvent| async move {
                    // As for a reply's field: a refused focus changes nothing
                    let _ = e.set_focus(true).await;
                },
                value: "{d.title}",
                oninput: move |e: FormEvent| draft.write().title = e.value(),
            }
            span {
                id: "ui-compose-title-limit",
                class: if title_over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "{title_line}"
            }
        }
        label { class: "ui-field",
            span { class: "ui-field-label", "Text" }
            textarea {
                class: "ui-input ui-textarea",
                name: "body",
                rows: "8",
                aria_describedby: "ui-compose-body-limit",
                aria_invalid: body_over.then_some("true"),
                value: "{d.body}",
                oninput: move |e: FormEvent| draft.write().body = e.value(),
            }
            span {
                id: "ui-compose-body-limit",
                class: if body_over { "ui-field-hint ui-field-over" } else { "ui-field-hint" },
                "Markdown. {body_line}"
            }
        }
        div { class: "ui-field ui-compose-proposal",
            label { class: "ui-check",
                input {
                    r#type: "checkbox",
                    name: "is_proposal",
                    checked: d.proposal,
                    onchange: move |e: FormEvent| draft.write().proposal = e.checked(),
                }
                span { "File it as a proposal for the Council" }
            }
            if d.proposal {
                label { class: "ui-field",
                    span { class: "ui-field-label", "Category" }
                    select {
                        class: "ui-input",
                        name: "proposal_category",
                        onchange: move |e: FormEvent| {
                            draft.write().category = FILABLE_HERE
                                .into_iter()
                                .find(|c| c.to_string() == e.value());
                        },
                        option { value: "", selected: d.category.is_none(), "Choose…" }
                        for c in FILABLE_HERE {
                            option {
                                key: "{c}",
                                value: "{c}",
                                selected: d.category == Some(c),
                                "{c}"
                            }
                        }
                    }
                    if let Some(c) = d.category {
                        span { class: "ui-field-hint", "Passes with: {c.threshold()}." }
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "dioxus-ssr"))]
mod tests;
