//! The link policy in the runner's views (0.166), which the server applied
//! on read until it stopped: what an agent on the default config reads is
//! what it read then, and what the server never covered is as the Steward
//! decided it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use super::*;
use crate::display::AgentJson;
use crate::display::oracle;
use crate::responses::{
    ContentResponse, DocumentResponse, GitContentResponse, InboxResponse,
};

fn site() -> Url {
    Url::parse("https://subliminal.technology").unwrap()
}

/// What the old server did to a field it rewrote: its scanner, as of its
/// removal ([`oracle::old_server`])
fn old_server(text: &str) -> String {
    oracle::old_server(text, Some(&site())).into_owned()
}

/// An author's text with each kind of link: an off-site URL followed by
/// `&` and `<`, a schemeless `www.` host, and a page of Agora's own site.
/// The off-site URL is first, so a preview cut to 60 characters keeps it
const LINKS: &str = "https://example.com/a?x=1&y=<2> then www.example.org/p \
     and https://subliminal.technology/agora/post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac \
     too";

const DEFANGED: &str = "hxxps://example[.]com/a";

/// `f` on every value at `path` in `value`: keys by name, `*` for each
/// item of an array or entry of a map, a number for one item. A key an
/// object lacks is added (an `Option` left out when `None`)
fn at(value: &mut Value, path: &[&str], f: &mut dyn FnMut(&mut Value)) {
    let Some((first, rest)) = path.split_first() else {
        f(value);
        return;
    };
    match value {
        Value::Array(items) if *first == "*" => {
            for item in items {
                at(item, rest, f);
            }
        }
        Value::Object(map) if *first == "*" => {
            for (_, item) in map.iter_mut() {
                at(item, rest, f);
            }
        }
        Value::Array(items) => {
            let index: usize = first.parse().expect("`*` or an index");
            at(&mut items[index], rest, f);
        }
        Value::Object(map) => {
            at(map.entry(*first).or_insert(Value::Null), rest, f);
        }
        _ => {}
    }
}

/// `fixture` with [`LINKS`] written into each of `fields` (the fields the
/// old server rewrote), and the same after the old server's rewrite of
/// exactly those
fn raw_and_rewritten<T: Serialize + DeserializeOwned>(
    fixture: &T,
    fields: &[&str],
) -> (T, T) {
    filled_and_rewritten(fixture, fields, LINKS)
}

/// [`raw_and_rewritten`] with `fill` for [`LINKS`]
fn filled_and_rewritten<T: Serialize + DeserializeOwned>(
    fixture: &T,
    fields: &[&str],
    fill: &str,
) -> (T, T) {
    let mut raw = serde_json::to_value(fixture).unwrap();
    for field in fields {
        let path: Vec<&str> = field.split('/').collect();
        let mut hits = 0;
        at(&mut raw, &path, &mut |v| {
            *v = Value::String(format!("{fill} ({field})"));
            hits += 1;
        });
        assert!(hits > 0, "the fixture has no `{field}`");
    }
    let mut rewritten = raw.clone();
    for field in fields {
        let path: Vec<&str> = field.split('/').collect();
        at(&mut rewritten, &path, &mut |v| {
            let text = v.as_str().expect("a field just filled");
            *v = Value::String(old_server(text));
        });
    }
    assert_ne!(raw, rewritten);
    (
        serde_json::from_value(raw).expect("the filled fixture"),
        serde_json::from_value(rewritten).expect("the rewritten fixture"),
    )
}

/// The property of this release: `render` of the raw wire value under the
/// default config, on the site, is `render` of the value the old server
/// sent (its `fields` rewritten) with no policy at all, which is what the
/// runner did before. Returns what is shown.
///
/// The right-hand side is rendered [`as_written`] (no policy, no site),
/// not under `raw` on the site: the old runner applied nothing, so
/// nothing is what it is compared with
fn parity<T: Serialize + DeserializeOwned>(
    fixture: &T,
    fields: &[&str],
    render: impl Fn(&T, DisplayContext<'_>) -> String,
) -> String {
    let (raw, _) = raw_and_rewritten(fixture, fields);
    let new = parity_filled(fixture, fields, LINKS, &render);
    // Every field was shown, and none of it kept a link
    assert!(
        new.matches(DEFANGED).count() >= fields.len(),
        "{} of {} fields shown: {new}",
        new.matches(DEFANGED).count(),
        fields.len()
    );
    assert!(
        !new.contains("https://") && !new.contains("http://"),
        "{new}"
    );
    assert!(!new.contains("www.example"), "{new}");
    // And with no policy the raw value shows its links: the comparison
    // is not of two texts that both ignore the fields
    assert!(render(&raw, as_written()).contains("https://example.com/a"));
    new
}

fn viewer() -> crate::names::AgentName {
    "viewer".parse().unwrap()
}

/// The old server's rewrite is a fixed point of itself, and of `raw` on
/// the site: a value it rewrote reads the same however often, and under
/// whichever policy, it is shown again
#[test]
fn the_old_servers_rewrite_is_a_fixed_point() {
    let once = old_server(LINKS);
    assert_eq!(
        once,
        "hxxps://example[.]com/a?x=1&y=<2> then www[.]example[.]org/p and \
         agora://post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac too"
    );
    assert_eq!(old_server(&once), once);
    let raw = Links::Raw { deny: Vec::new() };
    assert_eq!(raw.rewrite(&once, Some(&site())), once);
    assert_eq!(as_written().linked(&once), once);
    assert_eq!(as_written().linked(LINKS), LINKS);
}

/// [`parity`]'s equality alone, with `fill` written into `fields`
fn parity_filled<T: Serialize + DeserializeOwned>(
    fixture: &T,
    fields: &[&str],
    fill: &str,
    render: impl Fn(&T, DisplayContext<'_>) -> String,
) -> String {
    let (raw, rewritten) = filled_and_rewritten(fixture, fields, fill);
    let config = AgentDisplayConfig::default();
    let site = site();
    let new = render(&raw, DisplayContext::at(&config, Some(&site)));
    let old = render(&rewritten, as_written());
    assert_eq!(new, old);
    new
}

// ── What the old server rewrote: the same text as before ────────────────

fn post_read() -> PostWithCommentsResponse {
    let mut read = crate::fixtures::post_with_comments();
    read.comments = vec![crate::fixtures::comment()];
    read.comment_stubs = vec![crate::fixtures::comment_stub()];
    read.omitted_comment_count = 1;
    read.post.comment_count = Some(2);
    read.thread_summary = Some("A summary.".into());
    read.poll = Some(crate::fixtures::poll());
    read
}

#[test]
fn a_post_read_is_what_it_was() {
    let shown = parity(
        &post_read(),
        &[
            "post/title",
            "post/body",
            "comments/*/body",
            "comment_stubs/*/preview",
            "thread_summary",
            "poll/options/*/label",
            "poll/options/*/description",
        ],
        |read, cx| {
            Post {
                post: read,
                viewer_name: &viewer(),
                index: None,
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );
    // The whole body, its `&` and `<` escaped after the policy read them
    assert!(
        shown.contains(
            "hxxps://example[.]com/a?x=1&amp;y=&lt;2> then \
             www[.]example[.]org/p and \
             agora://post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac too (post/body)"
        ),
        "{shown}"
    );
}

#[test]
fn a_comment_chain_is_what_it_was() {
    let chain = CommentChainResponse {
        post_id: crate::fixtures::post().id,
        post_title: Some("A title".into()),
        root: Some(crate::fixtures::post()),
        omitted_ancestors: 0,
        root_omitted_chars: 0,
        chain: vec![crate::fixtures::comment()],
        in_your_trash: None,
    };
    parity(
        &chain,
        &["post_title", "root/title", "root/body", "chain/*/body"],
        |chain, cx| {
            CommentChain {
                chain,
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );
}

#[test]
fn a_search_is_what_it_was() {
    let search = SearchResponse {
        results: vec![crate::fixtures::post()],
        comment_results: vec![CommentSearchHit {
            comment: crate::fixtures::comment(),
            post_title: "A title".into(),
            similarity: 0.8,
        }],
        mode_used: SearchMode::Semantic,
        degraded: false,
    };
    parity(
        &search,
        &[
            "results/*/title",
            "results/*/body",
            // The comment is flattened into its hit
            "comment_results/*/body",
            "comment_results/*/post_title",
        ],
        |search, cx| {
            Search {
                search,
                query: "agency",
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );
}

#[test]
fn a_feed_is_what_it_was() {
    // A feed lists titles only
    parity(
        &vec![crate::fixtures::post(), crate::fixtures::post()],
        &["*/title"],
        |posts, cx| {
            Feed {
                posts,
                community: None,
                sort: FeedSort::Date,
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );
}

fn full_dashboard() -> DashboardResponse {
    let mut d = dash_with_elsewhere();
    d.unread_post_replies = serde_json::from_value(json!([{
        "post_id": uuid::Uuid::from_u128(1),
        "post_title": "Mine",
        "replies": [{
            "comment_id": uuid::Uuid::from_u128(2),
            "author": "mallory",
            "preview": "A reply",
            "score": 0,
            "created_at": "2026-09-14T00:00:00Z",
        }],
    }]))
    .unwrap();
    d.unread_comment_replies = serde_json::from_value(json!([{
        "post_id": uuid::Uuid::from_u128(3),
        "post_title": "Theirs",
        "comment_id": uuid::Uuid::from_u128(4),
        "author": "mallory",
        "preview": "Another",
        "score": 0,
        "created_at": "2026-09-14T00:00:00Z",
    }]))
    .unwrap();
    d.open_polls.push(OpenPollPointer {
        poll_id: crate::ids::PollId::from(uuid::Uuid::from_u128(5)),
        post_id: PostId::from(uuid::Uuid::from_u128(6)),
        title: "Name the platform".into(),
        community: "meta".parse().unwrap(),
        mode: crate::enums::PollMode::Ranked,
        option_count: 3,
        closes_at: "2026-11-09T12:00:00Z".parse().unwrap(),
        advisory: true,
        size: None,
        comment_count: Some(0),
    });
    d.my_activity = Some(vec![own(true), own(false)]);
    d
}

/// The dashboard's sections, and the recent activity the intro puts after
/// it from the same response
fn dashboard_text(d: &DashboardResponse, cx: DisplayContext<'_>) -> String {
    let mut out = Dashboard {
        dash: d,
        model: test_model(),
    }
    .agent_display(cx)
    .to_string();
    out.push_str(&format_my_activity(
        d.my_activity.as_deref().unwrap_or_default(),
        cx,
    ));
    out
}

#[test]
fn a_dashboard_is_what_it_was() {
    parity(
        &full_dashboard(),
        &[
            "unread_post_replies/*/post_title",
            "unread_post_replies/*/replies/*/preview",
            "unread_comment_replies/*/post_title",
            "unread_comment_replies/*/preview",
            "feeds/*/*/title",
            "elsewhere/*/*/title",
            "open_polls/*/title",
            "my_activity/*/post_title",
            "my_activity/*/preview",
        ],
        dashboard_text,
    );
}

/// The agent's own posts, as the intro lists them from a server before
/// 0.119: under the policy as anyone's are, as the server had them
#[test]
fn an_agents_post_listing_is_what_it_was() {
    parity(
        &vec![crate::fixtures::post(), crate::fixtures::post()],
        &["*/title"],
        |posts, cx| format_recent_activity(posts, 5, cx),
    );
}

/// A field the runner cuts, with a URL across the cut: the policy runs
/// before the cut, as the server's rewrite did, so the cut lands where it
/// did (in the defanged text)
#[test]
fn a_url_across_a_cut_is_what_it_was() {
    // A reply's preview is cut to 100 characters, a post title to 60
    let fill =
        ["x".repeat(80), " https://example.com/abcdefghijkl".into()].concat();
    let shown = parity_filled(
        &full_dashboard(),
        &[
            "unread_post_replies/*/replies/*/preview",
            "unread_comment_replies/*/preview",
            "my_activity/*/post_title",
        ],
        &fill,
        dashboard_text,
    );
    // Cut inside the defanged URL, as the server's text was
    assert!(
        shown.contains(&[&"x".repeat(80), " hxxps://e"].concat()),
        "{shown}"
    );
    assert!(!shown.contains("https://"), "{shown}");
}

/// A comment chain whose root post is cut by the server
/// (`root_omitted_chars`), a deleted comment's placeholder among others,
/// and a read shown a second time (pointers to what is in the context)
#[test]
fn a_cut_root_a_placeholder_and_a_second_read_are_what_they_were() {
    let chain = CommentChainResponse {
        post_id: crate::fixtures::post().id,
        post_title: Some("A title".into()),
        root: Some(crate::fixtures::post()),
        omitted_ancestors: 2,
        root_omitted_chars: 1_000,
        chain: vec![crate::fixtures::comment()],
        in_your_trash: None,
    };
    parity(
        &chain,
        &["post_title", "root/title", "root/body", "chain/*/body"],
        |chain, cx| {
            CommentChain {
                chain,
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );

    let mut read = post_read();
    let mut gone = crate::fixtures::comment();
    gone.id = CommentId::from(uuid::Uuid::from_u128(77));
    gone.body = RemovedBy::Author.placeholder().into();
    gone.deleted_at = Some("2026-11-01T00:00:00Z".parse().unwrap());
    gone.removed_by = Some(RemovedBy::Author);
    read.comments.push(gone);
    // Read twice in one session: the second is pointers, the same both
    // ways but for their `size`, which counts the text as stored (the
    // server's was defanged, a few bytes longer)
    let twice = |read: &PostWithCommentsResponse, cx: DisplayContext<'_>| {
        let once = ShownOnce::default();
        let view = Post {
            post: read,
            viewer_name: &viewer(),
            index: None,
            once: &once,
        };
        let first = view.agent_display(cx).to_string();
        let again = view.agent_display(cx).to_string();
        let sizeless = again
            .split(" size=\"")
            .enumerate()
            .map(|(i, part)| match i {
                0 => part,
                _ => part.split_once('"').map_or(part, |(_, rest)| rest),
            })
            .collect::<String>();
        [first, sizeless].join("\n--- again ---\n")
    };
    let shown = parity(
        &read,
        &[
            "post/title",
            "post/body",
            "comments/0/body",
            "thread_summary",
        ],
        twice,
    );
    assert!(
        shown.contains(RemovedBy::Author.placeholder()),
        "the placeholder is shown: {shown}"
    );
    let again = shown.split("--- again ---").nth(1).unwrap();
    assert!(again.contains(" shown=\""), "{again}");
}

/// Where the runner's policy deliberately differs from the server's
/// scanner, side by side: the server's output, then the runner's. The
/// parity fixtures above avoid these
#[test]
fn where_the_runner_differs_from_the_server() {
    let site = site();
    for (text, server, runner) in [
        // A scheme is a start wherever it is written (layer 1)
        (
            "xhttp://example.com",
            "xhttp://example.com",
            "xhxxp://example[.]com",
        ),
        (
            "so.https://example.com",
            "so.https://example.com",
            "so.hxxps://example[.]com",
        ),
        ("httphttp://x.y", "httphttp://x.y", "httphxxp://x[.]y"),
        // A URL after another's `:` or `@` is one of its own (layer 1)
        (
            "https://a:https://evil.example/x",
            "hxxps://a:https://evil.example/x",
            "hxxps://a:hxxps://evil[.]example/x",
        ),
        (
            "https://a.b@http://evil.example/x",
            "hxxps://a[.]b@http://evil.example/x",
            "hxxps://a[.]b@hxxp://evil[.]example/x",
        ),
        // Every spelling a parser reads as `http` or `https` (layer 5)
        (
            "ht\ttps://evil.example/x",
            "ht\ttps://evil.example/x",
            "hx\txps://evil[.]example/x",
        ),
        // A site page before a space that is not ASCII is its link: the
        // server read the space into it, and the page was not found
        (
            "https://subliminal.technology/agora/protocol\u{a0}next",
            "hxxps://subliminal[.]technology/agora/protocol\u{a0}next",
            "agora://protocol\u{a0}next",
        ),
        // A site page before a closing mark or a footnote is its link: the
        // server read the mark into it (layer 5b)
        (
            "“https://subliminal.technology/agora/protocol”",
            "“hxxps://subliminal[.]technology/agora/protocol”",
            "“agora://protocol”",
        ),
        (
            "https://subliminal.technology/agora/protocol¹",
            "hxxps://subliminal[.]technology/agora/protocol¹",
            "agora://protocol¹",
        ),
        // A slashless scheme before something that is not a domain name
        // is code, as the server had it; before one, a URL (layer 5b)
        (
            "{http:1.1} HTTP:200",
            "{http:1.1} HTTP:200",
            "{http:1.1} HTTP:200",
        ),
        (
            "https:evil.example/x",
            "https:evil.example/x",
            "hxxps:evil[.]example/x",
        ),
        (
            "https://subliminal.technology/agora/post/7ad26ccd\u{2028}x",
            "hxxps://subliminal[.]technology/agora/post/7ad26ccd\u{2028}x",
            "agora://post/7ad26ccd\u{2028}x",
        ),
    ] {
        assert_eq!(oracle::old_server(text, Some(&site)), server, "{text:?}");
        assert_eq!(
            Links::default().rewrite(text, Some(&site)),
            runner,
            "{text:?}"
        );
    }
    // Every spelling: the runner leaves none fetchable, and where the
    // server did, the two differ (`https:///x`, `https://\nx` and
    // `https:// \nx` the server defanged too, and of `https://a:https:x.y`
    // left `https:x[.]y`, a URL to a single label, which is not counted)
    let mut differ = 0;
    for spelling in oracle::SPELLINGS {
        let server = oracle::old_server(spelling, Some(&site));
        let runner = Links::default().rewrite(spelling, Some(&site));
        assert_eq!(oracle::fetchable(&runner), None, "{spelling:?}");
        if oracle::fetchable(&server).is_some() {
            assert_ne!(server, runner, "{spelling:?}");
            differ += 1;
        }
    }
    assert_eq!(differ, oracle::SPELLINGS.len() - 4);
}

// ── What the server never covered: the Steward's decisions ──────────────

fn default_on_site<T>(render: impl Fn(DisplayContext<'_>) -> T) -> T {
    let config = AgentDisplayConfig::default();
    let site = site();
    render(DisplayContext::at(&config, Some(&site)))
}

fn agent(name: &str) -> crate::responses::AgentResponse {
    serde_json::from_value(json!({
        "id": uuid::Uuid::from_u128(7),
        "operator_id": uuid::Uuid::from_u128(8),
        "operator_display_name": format!("Op {LINKS}"),
        "name": name,
        "display_name": format!("Name {LINKS}"),
        "model_info": format!("Model {LINKS}"),
        "bio": format!("Bio {LINKS}"),
        "created_at": "2026-08-01T00:00:00Z",
    }))
    .unwrap()
}

/// Its own profile is under the policy as another's is, every field
/// (the Steward, 2026-11-17, reversing decision 2: an agent could store a
/// defanged link mended and read it back fetchable), and says so when a
/// link in it was defanged
#[test]
fn its_own_profile_is_under_the_policy_as_anothers_is() {
    let profile = |agent: &crate::responses::AgentResponse| {
        default_on_site(|cx| {
            Profile {
                agent,
                viewer_name: &viewer(),
                enrolled: None,
            }
            .agent_display(cx)
            .to_string()
        })
    };
    let own = profile(&agent("viewer"));
    let theirs = profile(&agent("someone"));
    for shown in [&own, &theirs] {
        assert!(!shown.contains("https://example"), "{shown}");
        assert!(!shown.contains("https://subliminal"), "{shown}");
        assert!(!shown.contains("www.example"), "{shown}");
        assert_eq!(shown.matches(DEFANGED).count(), 4, "{shown}");
        assert_eq!(shown.matches("agora://post/8dbc9b3a").count(), 4);
    }
    assert!(
        own.contains(&format!(
            "(Here &lt; is <, and update_profile stores &lt; as <; {}.)",
            crate::reactor::agent::seed::prompt::OWN_LINKS
        )),
        "{own}"
    );
    assert!(!theirs.contains(crate::reactor::agent::seed::prompt::OWN_LINKS));
    // Only when a link in it was defanged: not for a site page's link, nor
    // for no link at all
    for bio in [
        "No links here.",
        "See https://subliminal.technology/agora/protocol",
    ] {
        let mut clean = agent("viewer");
        clean.display_name = None;
        clean.model_info = None;
        clean.operator_display_name = "Op".into();
        clean.bio = Some(bio.into());
        let shown = profile(&clean);
        assert!(!shown.contains("defanged"), "{shown}");
    }
    let mut linked = agent("viewer");
    linked.display_name = None;
    linked.model_info = None;
    linked.bio = Some("Mine: https://example.com/x".into());
    let shown = profile(&linked);
    assert!(
        shown.contains(&format!(
            "(Here {}.)",
            crate::reactor::agent::seed::prompt::OWN_LINKS
        )),
        "{shown}"
    );
}

fn inbox() -> InboxResponse {
    let (mut inbox, _) = crate::reactor::agent::seed::tests::inbox_with(LINKS);
    let mut broadcast = inbox.messages[0].clone();
    broadcast.id = crate::ids::MessageId::from(uuid::Uuid::from_u128(9));
    broadcast.recipient_id = None;
    broadcast.kind = Some(crate::enums::MessageKind::Broadcast);
    broadcast.body = Some(format!("Broadcast {LINKS}"));
    inbox.messages.push(broadcast);
    inbox.operator_messages = serde_json::from_value(json!([{
        "id": uuid::Uuid::from_u128(10),
        "from": {"id": uuid::Uuid::from_u128(11), "display_name": "Mike"},
        "sent_at": "2026-11-01T00:00:00Z",
        "state": "unread",
        "content": {"status": "text", "body": format!("Operator {LINKS}")},
    }]))
    .expect("an operator's message");
    inbox.operator_unread = 1;
    inbox.operator_replies = serde_json::from_value(json!([{
        "id": uuid::Uuid::from_u128(12),
        "sent_at": "2026-11-01T00:00:00Z",
        "content": {"status": "text", "body": format!("Reply {LINKS}")},
    }]))
    .expect("a reply to the operator");
    inbox
}

/// A message from another agent is under the policy (decision 1); its
/// operator's, and its own to its operator, are as written (decision 3);
/// a broadcast, which its `kind` tells apart, is as written (decision 5)
#[test]
fn an_inbox_shows_each_kind_of_message_by_who_wrote_it() {
    let inbox = inbox();
    let shown = default_on_site(|cx| {
        Inbox {
            inbox: &inbox,
            once: &ShownOnce::default(),
            limit: None,
        }
        .agent_display(cx)
        .to_string()
    });
    // The direct message, alone, is defanged
    assert_eq!(shown.matches(DEFANGED).count(), 1, "{shown}");
    assert!(
        shown.contains(
            "state=\"new: unread before this read\">\
             hxxps://example[.]com/a?x=1&amp;y=&lt;2> then \
             www[.]example[.]org/p and agora://post/8dbc9b3a"
        ),
        "{shown}"
    );
    assert!(
        shown.contains(
            "Operator https://example.com/a?x=1&amp;y=&lt;2> then \
             www.example.org/p and \
             https://subliminal.technology/agora/post/"
        ),
        "{shown}"
    );
    // A broadcast as written but for the site's pages (0.168)
    assert!(
        shown.contains(
            "Broadcast https://example.com/a?x=1&amp;y=&lt;2> then \
             www.example.org/p and agora://post/8dbc9b3a"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("\"Reply https://example.com/a?x=1&y=&lt;2> then"),
        "{shown}"
    );

    // The JSON agrees on the operator's section and the direct message;
    // a broadcast shares the messages' key, so there it is under the policy
    let json = default_on_site(|cx| inbox.agent_json(cx).unwrap());
    let plain = serde_json::to_value(&inbox).unwrap();
    assert_eq!(json["operator_messages"], plain["operator_messages"]);
    assert_eq!(json["operator_replies"], plain["operator_replies"]);
    assert_eq!(json["messages"][0]["body"], old_server(LINKS));
    assert_eq!(
        json["messages"][1]["body"],
        format!("Broadcast {}", old_server(LINKS))
    );
}

fn governance_entry() -> GovernanceEntryResponse {
    serde_json::from_value(json!({
        "id": "GOV-2026-0017",
        "entry_type": "council_decision",
        "title": format!("Title {LINKS}"),
        "created_at": "2026-10-01T00:00:00Z",
        "summary": format!("Summary {LINKS}"),
        "data": {"statement": format!("Statement {LINKS}")},
        "attachments": [
            {"name": "clerk-reads", "note": format!("Note {LINKS}"), "bytes": 3000}
        ],
        "amendments": [{
            "id": "AMD-2026-0004", "kind": "correction", "basis": "x",
            "note": format!("Amendment {LINKS}"),
            "created_at": "2026-10-02T00:00:00Z",
        }],
    }))
    .expect("a governance entry")
}

/// A governance entry is as written, all of it, in the text and in the
/// JSON (decision 4)
#[test]
fn a_governance_entry_is_as_written() {
    let entry = governance_entry();
    let shown = default_on_site(|cx| {
        GovernanceEntry {
            entry: &entry,
            once: &ShownOnce::default(),
        }
        .agent_display(cx)
        .to_string()
    });
    assert!(!shown.contains("hxxp") && !shown.contains("[.]"), "{shown}");
    for part in ["Title", "Summary", "Statement", "Note", "Amendment"] {
        assert!(
            shown.contains(&format!("{part} https://example.com/a?x=1&y=")),
            "{part}: {shown}"
        );
    }
    assert_eq!(
        default_on_site(|cx| entry.agent_json(cx).unwrap()),
        serde_json::to_value(&entry).unwrap()
    );
}

fn data_file_entry() -> GovernanceEntryResponse {
    let mut entry: GovernanceEntryResponse = serde_json::from_value(json!({
        "id": "DATA-2026-0001",
        "entry_type": "data_set",
        "title": "A data set",
        "created_at": "2026-10-01T00:00:00Z",
        "attachment": "votes/README.md",
    }))
    .expect("a data set's entry");
    let path: crate::govlog::DataFilePath = "votes/README.md".parse().unwrap();
    let text = format!("File {LINKS}\n");
    let file: crate::govlog::DataFile = serde_json::from_value(json!({
        "path": path,
        "bytes": text.len(),
        "sha256": constitution_sha256(&text),
    }))
    .expect("a data set's file");
    entry.data_file = Some(
        crate::responses::DataFileContent::new(
            &entry.id,
            "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            &file,
            text.as_bytes(),
            1 << 20,
            None,
        )
        .unwrap(),
    );
    entry
}

/// A data set's file is as written, in the text and in the JSON: it is
/// checked against the hash its data set signs
#[test]
fn a_data_sets_file_is_as_written() {
    let entry = data_file_entry();
    let shown = default_on_site(|cx| {
        GovernanceEntry {
            entry: &entry,
            once: &ShownOnce::default(),
        }
        .agent_display(cx)
        .to_string()
    });
    assert!(shown.contains(&format!("File {LINKS}")), "{shown}");
    assert_eq!(
        default_on_site(|cx| entry.agent_json(cx).unwrap()),
        serde_json::to_value(&entry).unwrap()
    );
}

fn git_file() -> GitContentResponse {
    serde_json::from_value(json!({
        "repos": [],
        "repo": "agora",
        "commit": "0123456789abcdef0123456789abcdef01234567",
        "citation": "git:agora:0123456789abcdef0123456789abcdef01234567:docs/x.md",
        "file": {
            "path": "docs/x.md",
            "bytes": 200,
            "total_lines": 1,
            "text": format!("File {LINKS}\n"),
            "omitted_bytes": 0,
        },
        "how_to_read": "Read a file by its path.",
    }))
    .expect("a git file read")
}

/// A file of Agora's repositories is as written, in the text and in the
/// JSON
#[test]
fn a_git_file_is_as_written() {
    let git = git_file();
    let shown = default_on_site(|cx| {
        Git {
            git: &git,
            by_link: false,
        }
        .agent_display(cx)
        .to_string()
    });
    assert!(shown.contains(&format!("File {LINKS}")), "{shown}");
    assert_eq!(
        default_on_site(|cx| git.agent_json(cx).unwrap()),
        serde_json::to_value(&git).unwrap()
    );
}

/// A document's JSON is as written. Its text is `get_content`'s own
/// (`tool.rs`), which applies no policy to it
#[test]
fn a_document_is_as_written() {
    let doc = DocumentResponse::new(
        crate::ids::PlatformDoc::GovernanceProtocol,
        "The Governance Protocol".into(),
        "0.3.8".into(),
        &format!("Protocol {LINKS}\n"),
        None,
    )
    .unwrap();
    let plain = serde_json::to_value(&doc).unwrap();
    assert_eq!(default_on_site(|cx| doc.agent_json(cx).unwrap()), plain);
    assert!(plain["text"].as_str().unwrap().contains(LINKS));
}

/// The calendar is the platform's: as written
#[test]
fn a_calendar_is_as_written() {
    let calendar: CalendarResponse = serde_json::from_value(json!({
        "how_to_read": format!("How {LINKS}"),
        "items": [{
            "kind": "obligation_due",
            "obligation": "OBL-2026-0001",
            "text": format!("Owed {LINKS}"),
            "owed_by": "steward",
            "due": "2026-12-01",
        }],
    }))
    .expect("a calendar");
    let shown = default_on_site(|cx| calendar.agent_display(cx).to_string());
    assert!(!shown.contains("hxxp") && !shown.contains("[.]"), "{shown}");
    assert!(shown.contains(&format!("How {LINKS}")), "{shown}");
    assert!(
        shown.contains("\"Owed https://example.com/a?x=1&y="),
        "{shown}"
    );
    // (through the trait: its JSON once rewrote what the const exempts)
    let json = default_on_site(|cx| calendar.agent_json(cx).unwrap());
    assert_eq!(json, serde_json::to_value(&calendar).unwrap());
}

/// Text other agents wrote that the server never rewrote is under the
/// policy now (decision 1): a community's display name and description,
/// a friend's display name, a proposal's title and body, and the reader's
/// own trash
#[test]
fn what_the_server_never_rewrote_of_agents_text_is_under_the_policy() {
    let no_links = |shown: &str, fields: usize| {
        assert!(!shown.contains("https://"), "{shown}");
        assert!(!shown.contains("www.example"), "{shown}");
        assert_eq!(shown.matches(DEFANGED).count(), fields, "{shown}");
    };

    let community = crate::responses::CommunityResponse {
        display_name: format!("Name {LINKS}"),
        description: Some(format!("About {LINKS}")),
        ..crate::fixtures::community()
    };
    let communities = [community];
    no_links(
        &default_on_site(|cx| {
            Communities {
                communities: &communities,
            }
            .agent_display(cx)
            .to_string()
        }),
        2,
    );

    // The header a community's link reads over its feed
    no_links(
        &default_on_site(|cx| {
            crate::reactor::agent::seed::tool::CommunityHeader {
                community: &communities[0],
            }
            .agent_display(cx)
            .to_string()
        }),
        2,
    );

    let friends: crate::responses::FriendsResponse =
        serde_json::from_value(json!({
            "friends": [{
                "agent_id": uuid::Uuid::from_u128(13),
                "name": "alice",
                "display_name": format!("Name {LINKS}"),
                "since": "2026-10-01T00:00:00Z",
                "can_e2ee": true,
            }],
            "incoming_requests": [],
            "outgoing_requests": [],
        }))
        .expect("a friends list");
    no_links(
        &default_on_site(|cx| friends.agent_display(cx).to_string()),
        1,
    );

    let page: crate::responses::ProposalsResponse =
        serde_json::from_value(json!({
            "proposals": [{
                "id": uuid::Uuid::from_u128(14),
                "agent_id": uuid::Uuid::from_u128(15),
                "agent_name": "alice",
                "title": format!("Title {LINKS}"),
                "body": format!("Body {LINKS}"),
                "created_at": "2026-10-01T00:00:00Z",
                "proposal_category": "policy",
            }],
            "total": 1,
            "offset": 0,
        }))
        .expect("a proposals page");
    no_links(
        &default_on_site(|cx| {
            Proposals {
                page: &page,
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        }),
        2,
    );

    let mut entry = trash_entry(ContentKind::Post, DeletedBy::Author);
    entry.title = Some(format!("Title {LINKS}"));
    entry.body = Some(format!("Body {LINKS}"));
    let trash = TrashPage {
        items: vec![entry],
        total: 1,
        offset: 0,
        limit: 25,
    };
    no_links(
        &default_on_site(|cx| trash.agent_display(cx).to_string()),
        2,
    );
}

// ── The config decides ──────────────────────────────────────────────────

fn config(links: Value) -> AgentDisplayConfig {
    serde_json::from_value(json!({ "links": links })).unwrap()
}

fn post_with_body(body: &str) -> PostWithCommentsResponse {
    let mut read = crate::fixtures::post_with_comments();
    read.post.body = body.into();
    read
}

fn post_text(
    read: &PostWithCommentsResponse,
    config: &AgentDisplayConfig,
) -> String {
    let site = site();
    Post {
        post: read,
        viewer_name: &viewer(),
        index: None,
        once: &ShownOnce::default(),
    }
    .agent_display(DisplayContext::at(config, Some(&site)))
    .to_string()
}

#[test]
fn an_allow_rule_leaves_its_urls_in_a_post_and_no_others() {
    let read = post_with_body(
        "See https://en.wikipedia.org/wiki/Agora and https://evil.example/x",
    );
    let allow = config(json!({
        "mode": "defanged",
        "allow": ["https://en.wikipedia.org/wiki"],
    }));
    let shown = post_text(&read, &allow);
    assert!(
        shown.contains(
            "See https://en.wikipedia.org/wiki/Agora and \
             hxxps://evil[.]example/x"
        ),
        "{shown}"
    );
    let default = post_text(&read, &AgentDisplayConfig::default());
    assert!(
        default.contains(
            "See hxxps://en[.]wikipedia[.]org/wiki/Agora and \
             hxxps://evil[.]example/x"
        ),
        "{default}"
    );
}

#[test]
fn a_deny_rule_defangs_its_urls_in_a_post_and_no_others() {
    let read = post_with_body(
        "See https://en.wikipedia.org/wiki/Agora and https://evil.example/x",
    );
    let deny = config(json!({"mode": "raw", "deny": ["https://evil.example"]}));
    let shown = post_text(&read, &deny);
    assert!(
        shown.contains(
            "See https://en.wikipedia.org/wiki/Agora and \
             hxxps://evil[.]example/x"
        ),
        "{shown}"
    );
    let raw = post_text(&read, &config(json!({"mode": "raw"})));
    assert!(
        raw.contains(
            "See https://en.wikipedia.org/wiki/Agora and \
             https://evil.example/x"
        ),
        "{raw}"
    );
}

// ── A read is rendered once ─────────────────────────────────────────────

/// Rendering a post marks it shown, so a second rendering of the same
/// read is a pointer to the first. The blocks and the text each render
/// once: neither is a pointer, and what a second would be is shown here,
/// so a provided method that came to render twice would fail this
#[test]
fn a_read_that_marks_what_it_shows_is_rendered_once() {
    let read = post_with_body("The body, in full.");
    let config = AgentDisplayConfig::default();
    let cx = DisplayContext::at(&config, None);
    let is_pointer = |text: &str| text.contains(" shown=\"");

    let once = ShownOnce::default();
    let view = Post {
        post: &read,
        viewer_name: &viewer(),
        index: None,
        once: &once,
    };
    let blocks: Vec<_> = view.agent_display_blocks(cx).collect();
    assert_eq!(blocks.len(), 1);
    let text = blocks[0].to_string();
    assert!(text.contains(">The body, in full.</post>"), "{text}");
    assert!(!is_pointer(&text), "{text}");
    // Rendered again, it is a pointer: the mark was made, once
    let again = view.agent_display(cx).to_string();
    assert!(is_pointer(&again), "{again}");
    assert!(!again.contains("The body, in full."), "{again}");

    // The text alone, the same; and through a trait object
    let once = ShownOnce::default();
    let view = Post {
        post: &read,
        viewer_name: &viewer(),
        index: None,
        once: &once,
    };
    let text = view.agent_display(cx).to_string();
    assert!(!is_pointer(&text), "{text}");
    let once = ShownOnce::default();
    let name = viewer();
    let view: Box<dyn AgentDisplay + '_> = Box::new(Post {
        post: &read,
        viewer_name: &name,
        index: None,
        once: &once,
    });
    let text: String = view
        .agent_display_blocks(cx)
        .map(|block| block.to_string())
        .collect();
    assert!(!is_pointer(&text), "{text}");
    assert!(is_pointer(&view.agent_text(cx).to_string()));
}

// ── `ContentResponse`, variant by variant ───────────────────────────────

/// Each variant's JSON ([`AgentJson`] of a [`ContentResponse`]): what is
/// verbatim comes through byte for byte, and an author's text is under
/// the policy. The variant's fields are beside the `type` tag, which the
/// schema test cannot root, so it is checked on values
#[test]
fn each_content_variant_exempts_what_its_type_does() {
    let chain = CommentChainResponse {
        post_id: crate::fixtures::post().id,
        post_title: Some(LINKS.into()),
        root: None,
        omitted_ancestors: 0,
        root_omitted_chars: 0,
        chain: vec![crate::fixtures::comment()],
        in_your_trash: None,
    };
    let calendar: CalendarResponse = serde_json::from_value(json!({
        "how_to_read": LINKS,
        "items": [],
    }))
    .unwrap();
    let document = DocumentResponse::new(
        crate::ids::PlatformDoc::GovernanceProtocol,
        "The Governance Protocol".into(),
        "0.3.8".into(),
        LINKS,
        None,
    )
    .unwrap();
    // (the variant, whether the policy leaves all of it as written)
    let variants = [
        (ContentResponse::Post(post_with_body(LINKS)), false),
        (ContentResponse::Comment(chain), false),
        (ContentResponse::Governance(governance_entry()), true),
        (ContentResponse::Governance(data_file_entry()), true),
        (ContentResponse::Document(document), true),
        (ContentResponse::Calendar(calendar), true),
        (ContentResponse::Git(git_file()), true),
    ];
    let mut kinds = std::collections::BTreeSet::new();
    for (content, verbatim) in variants {
        let plain = serde_json::to_value(&content).unwrap();
        kinds.insert(plain["type"].as_str().unwrap().to_string());
        assert!(plain.to_string().contains("https://example.com/a"));
        let shown =
            default_on_site(|cx| content.agent_json(cx).unwrap().to_string());
        if verbatim {
            assert_eq!(shown, plain.to_string());
        } else {
            assert!(!shown.contains("https://"), "{shown}");
            assert!(shown.contains(DEFANGED), "{shown}");
            assert!(shown.contains("agora://post/8dbc9b3a"), "{shown}");
        }
    }
    // Every variant: a new one fails here until it is decided
    let all = |content: &ContentResponse| match content {
        ContentResponse::Post(_)
        | ContentResponse::Comment(_)
        | ContentResponse::Governance(_)
        | ContentResponse::Document(_)
        | ContentResponse::Calendar(_)
        | ContentResponse::Git(_) => 6,
    };
    assert_eq!(all(&ContentResponse::Post(post_with_body(""))), kinds.len());
}

// ── Authors whose links are shown as written (0.168) ─────────────────────

/// The author a config below lists, and one it does not
const NEWS: &str = "news-desk";
const OTHER: &str = "mallory";

/// What the old server's trusted path sent of a trusted author's text
/// (its `site_links_at`): the site's pages as links, every other URL as
/// written. `raw` with no rule is that
fn trusted(text: &str) -> String {
    Links::Raw { deny: Vec::new() }
        .rewrite(text, Some(&site()))
        .into_owned()
}

fn listing(names: &[&str]) -> AgentDisplayConfig {
    config(json!({"mode": "defanged", "allow_authors": names}))
}

/// `render` of `fixture`, with `names` set (a path and a name each) and
/// [`LINKS`] in every field of `listed` and `others`, under a config that
/// lists [`NEWS`], is `render` of what the old server sent of it with no
/// policy: its trusted path for `listed` (fields [`NEWS`] wrote), its
/// defanging for `others`
fn author_parity<T: Serialize + DeserializeOwned>(
    fixture: &T,
    names: &[(&str, &str)],
    listed: &[&str],
    others: &[&str],
    render: impl Fn(&T, DisplayContext<'_>) -> String,
) -> String {
    let mut raw = serde_json::to_value(fixture).unwrap();
    let each = |value: &mut Value, field: &str, f: &dyn Fn(&str) -> Value| {
        let path: Vec<&str> = field.split('/').collect();
        let mut hits = 0;
        at(value, &path, &mut |v| {
            *v = f(v.as_str().unwrap_or(""));
            hits += 1;
        });
        assert!(hits > 0, "the fixture has no `{field}`");
    };
    for (field, name) in names {
        each(&mut raw, field, &|_| json!(name));
    }
    for field in listed.iter().chain(others) {
        each(&mut raw, field, &|_| json!(format!("{LINKS} ({field})")));
    }
    let mut sent = raw.clone();
    for field in listed {
        each(&mut sent, field, &|text| json!(trusted(text)));
    }
    for field in others {
        each(&mut sent, field, &|text| json!(old_server(text)));
    }
    let raw: T = serde_json::from_value(raw).expect("the filled fixture");
    let sent: T = serde_json::from_value(sent).expect("the sent fixture");
    let config = listing(&[NEWS]);
    let site = site();
    let new = render(&raw, DisplayContext::at(&config, Some(&site)));
    assert_eq!(new, render(&sent, as_written()));
    assert!(new.contains("https://example.com/a"), "{new}");
    assert!(
        !new.contains("https://subliminal.technology/agora/post"),
        "{new}"
    );
    if !others.is_empty() {
        assert!(new.contains(DEFANGED), "{new}");
    }
    new
}

fn post_view(
    read: &PostWithCommentsResponse,
    cx: DisplayContext<'_>,
) -> String {
    Post {
        post: read,
        viewer_name: &viewer(),
        index: None,
        once: &ShownOnce::default(),
    }
    .agent_display(cx)
    .to_string()
}

/// A listed author's post (its title, body and poll options) is as
/// written, and an unlisted author's comment under it is defanged; and
/// the reverse
#[test]
fn a_listed_authors_post_and_an_unlisted_authors_comment() {
    let shown = author_parity(
        &post_read(),
        &[
            ("post/agent_name", NEWS),
            ("comments/*/agent_name", OTHER),
            ("comment_stubs/*/agent_name", OTHER),
        ],
        &[
            "post/title",
            "post/body",
            "poll/options/*/label",
            "poll/options/*/description",
        ],
        &[
            "comments/*/body",
            "comment_stubs/*/preview",
            "thread_summary",
        ],
        post_view,
    );
    assert!(
        shown.contains(
            "https://example.com/a?x=1&amp;y=&lt;2> then www.example.org/p \
             and agora://post/8dbc9b3a-db55-47d5-8b14-5b913a1b44ac too \
             (post/body)"
        ),
        "{shown}"
    );
    author_parity(
        &post_read(),
        &[
            ("post/agent_name", OTHER),
            ("comments/*/agent_name", NEWS),
            ("comment_stubs/*/agent_name", NEWS),
        ],
        &["comments/*/body", "comment_stubs/*/preview"],
        &[
            "post/title",
            "post/body",
            "poll/options/*/label",
            "poll/options/*/description",
            "thread_summary",
        ],
        post_view,
    );
}

/// A listed author quoted in an unlisted author's text is not its author
#[test]
fn a_quote_of_a_listed_author_is_under_the_plain_policy() {
    let mut read = post_read();
    read.post.agent_name = Some(OTHER.parse().unwrap());
    read.post.body = format!("{NEWS} wrote: https://example.com/a");
    read.comments[0].agent_name = Some(NEWS.parse().unwrap());
    read.comments[0].body = "> https://quoted.example/x".into();
    let config = listing(&[NEWS]);
    let site = site();
    let shown = post_view(&read, DisplayContext::at(&config, Some(&site)));
    assert!(
        shown.contains(&format!("{NEWS} wrote: {DEFANGED}")),
        "{shown}"
    );
    // The comment is its own author's, whatever it quotes
    assert!(shown.contains("https://quoted.example/x"), "{shown}");
}

/// Names are matched exactly: `Oracle` and `oracle` are two agents
#[test]
fn an_author_differing_only_in_case_is_not_listed() {
    let mut read = post_with_body("See https://example.com/a");
    read.post.agent_name = Some("Oracle".parse().unwrap());
    let shown = post_text(&read, &listing(&["oracle"]));
    assert!(shown.contains(&format!("See {DEFANGED}")), "{shown}");
    let shown = post_text(&read, &listing(&["Oracle"]));
    assert!(shown.contains("See https://example.com/a"), "{shown}");
}

#[test]
fn a_listed_authors_chain_search_and_feed() {
    let chain = CommentChainResponse {
        post_id: crate::fixtures::post().id,
        post_title: Some("A title".into()),
        root: Some(crate::fixtures::post()),
        omitted_ancestors: 0,
        root_omitted_chars: 0,
        chain: vec![crate::fixtures::comment()],
        in_your_trash: None,
    };
    let chain_view = |chain: &CommentChainResponse, cx: DisplayContext<'_>| {
        CommentChain {
            chain,
            viewer_name: &viewer(),
            once: &ShownOnce::default(),
        }
        .agent_display(cx)
        .to_string()
    };
    author_parity(
        &chain,
        &[("root/agent_name", NEWS), ("chain/*/agent_name", OTHER)],
        &["root/title", "root/body"],
        // The chain's own title names no author
        &["post_title", "chain/*/body"],
        chain_view,
    );
    author_parity(
        &chain,
        &[("root/agent_name", OTHER), ("chain/*/agent_name", NEWS)],
        &["chain/*/body"],
        &["post_title", "root/title", "root/body"],
        chain_view,
    );

    let search = SearchResponse {
        results: vec![crate::fixtures::post()],
        comment_results: vec![CommentSearchHit {
            comment: crate::fixtures::comment(),
            post_title: "A title".into(),
            similarity: 0.8,
        }],
        mode_used: SearchMode::Semantic,
        degraded: false,
    };
    author_parity(
        &search,
        &[
            ("results/*/agent_name", NEWS),
            ("comment_results/*/agent_name", NEWS),
        ],
        &[
            "results/*/title",
            "results/*/body",
            "comment_results/*/body",
        ],
        // The post a comment is on is not the comment's author's
        &["comment_results/*/post_title"],
        |search, cx| {
            Search {
                search,
                query: "agency",
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );

    author_parity(
        &vec![crate::fixtures::post(), crate::fixtures::post()],
        &[("0/agent_name", NEWS), ("1/agent_name", OTHER)],
        &["0/title"],
        &["1/title"],
        |posts, cx| {
            Feed {
                posts,
                community: None,
                sort: FeedSort::Date,
                viewer_name: &viewer(),
                once: &ShownOnce::default(),
            }
            .agent_display(cx)
            .to_string()
        },
    );
    // Its own posts, as the intro lists them
    author_parity(
        &vec![crate::fixtures::post()],
        &[("*/agent_name", NEWS)],
        &["*/title"],
        &[],
        |posts, cx| format_recent_activity(posts, 5, cx),
    );
}

#[test]
fn a_listed_authors_dashboard_entries() {
    let mut d = full_dashboard();
    // A second feed post, by someone else
    let feed = d.feeds.values_mut().next().expect("a feed");
    let mut theirs = feed[0].clone();
    theirs.id = PostId::from(uuid::Uuid::from_u128(99));
    feed.push(theirs);
    author_parity(
        &d,
        &[
            ("unread_post_replies/*/replies/*/author", NEWS),
            ("unread_comment_replies/*/author", OTHER),
            ("feeds/*/0/author", NEWS),
            ("feeds/*/1/author", OTHER),
            ("elsewhere/*/*/author", NEWS),
        ],
        &[
            "unread_post_replies/*/replies/*/preview",
            "feeds/*/0/title",
            "elsewhere/*/*/title",
        ],
        &[
            "unread_post_replies/*/post_title",
            "unread_comment_replies/*/post_title",
            "unread_comment_replies/*/preview",
            "feeds/*/1/title",
            "open_polls/*/title",
            "my_activity/*/post_title",
            "my_activity/*/preview",
        ],
        dashboard_text,
    );
}

#[test]
fn a_listed_authors_profile_friend_and_proposal() {
    let config = listing(&[NEWS]);
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let shown = Profile {
        agent: &agent(NEWS),
        viewer_name: &viewer(),
        enrolled: None,
    }
    .agent_display(cx)
    .to_string();
    // Its name, model line and bio; its operator's name is not its own
    assert_eq!(shown.matches("https://example.com/a").count(), 3, "{shown}");
    assert_eq!(shown.matches(DEFANGED).count(), 1, "{shown}");
    assert!(
        shown.contains(&format!("Operated by Op {DEFANGED}")),
        "{shown}"
    );

    let friends = |name: &str| -> crate::responses::FriendsResponse {
        serde_json::from_value(json!({
            "friends": [{
                "agent_id": uuid::Uuid::from_u128(13),
                "name": name,
                "display_name": format!("Name {LINKS}"),
                "since": "2026-10-01T00:00:00Z",
                "can_e2ee": true,
            }],
            "incoming_requests": [],
            "outgoing_requests": [],
        }))
        .expect("a friends list")
    };
    let shown = friends(NEWS).agent_display(cx).to_string();
    assert!(shown.contains("https://example.com/a"), "{shown}");
    let shown = friends(OTHER).agent_display(cx).to_string();
    assert!(shown.contains(DEFANGED), "{shown}");

    let page = |name: &str| -> crate::responses::ProposalsResponse {
        serde_json::from_value(json!({
            "proposals": [{
                "id": uuid::Uuid::from_u128(14),
                "agent_id": uuid::Uuid::from_u128(15),
                "agent_name": name,
                "title": format!("Title {LINKS}"),
                "body": format!("Body {LINKS}"),
                "created_at": "2026-10-01T00:00:00Z",
                "proposal_category": "policy",
            }],
            "total": 1,
            "offset": 0,
        }))
        .expect("a proposals page")
    };
    let proposals = |page: &crate::responses::ProposalsResponse| {
        Proposals {
            page,
            once: &ShownOnce::default(),
        }
        .agent_display(cx)
        .to_string()
    };
    let shown = proposals(&page(NEWS));
    assert_eq!(shown.matches("https://example.com/a").count(), 2, "{shown}");
    assert!(!shown.contains(DEFANGED), "{shown}");
    let shown = proposals(&page(OTHER));
    assert_eq!(shown.matches(DEFANGED).count(), 2, "{shown}");
}

/// A listed sender's message is as written, but for the site's pages,
/// as text and as JSON; a broadcast is the platform's whoever sent it
#[test]
fn a_listed_senders_message() {
    let mut inbox = inbox();
    for m in &mut inbox.messages {
        m.sender_name = NEWS.parse().unwrap();
    }
    let config = listing(&[NEWS]);
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let shown = Inbox {
        inbox: &inbox,
        once: &ShownOnce::default(),
        limit: None,
    }
    .agent_display(cx)
    .to_string();
    assert!(!shown.contains(DEFANGED), "{shown}");
    assert!(
        shown.contains(
            "state=\"new: unread before this read\">\
             https://example.com/a?x=1&amp;y=&lt;2> then \
             www.example.org/p and agora://post/8dbc9b3a"
        ),
        "{shown}"
    );
    let json = inbox.agent_json(cx).unwrap();
    assert_eq!(json["messages"][0]["body"], trusted(LINKS));
    // The JSON's broadcast is under the plain policy, as before
    assert_eq!(
        json["messages"][1]["body"],
        format!("Broadcast {}", old_server(LINKS))
    );
}

/// The JSON of a post read: each item by its own author
#[test]
fn a_listed_authors_post_as_json() {
    let mut read = post_read();
    read.post.agent_name = Some(NEWS.parse().unwrap());
    read.post.body = LINKS.into();
    read.comments[0].agent_name = Some(OTHER.parse().unwrap());
    read.comments[0].body = LINKS.into();
    read.comment_stubs[0].agent_name = Some(NEWS.parse().unwrap());
    read.comment_stubs[0].preview = LINKS.into();
    let config = listing(&[NEWS]);
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let json = ContentResponse::Post(read.clone()).agent_json(cx).unwrap();
    assert_eq!(json["post"]["body"], trusted(LINKS));
    assert_eq!(json["comments"][0]["body"], old_server(LINKS));
    assert_eq!(json["comment_stubs"][0]["preview"], trusted(LINKS));
    // With no one listed, as before
    let config = AgentDisplayConfig::default();
    let json = ContentResponse::Post(read)
        .agent_json(DisplayContext::at(&config, Some(&site)))
        .unwrap();
    assert_eq!(json["post"]["body"], old_server(LINKS));
    assert_eq!(json["comment_stubs"][0]["preview"], old_server(LINKS));
}

// ── Pages of the site no kind names (0.168) ──────────────────────────────

const PAGES: &str = "https://subliminal.technology/agora/hello-world and \
     https://subliminal.technology/agora/account";
const PAGE_LINKS: &str = "agora://page/hello-world and agora://page/account";

/// The site's pages with no kind of their own are `agora://page/…` links
/// in a post's title and body, a message and a broadcast, in every mode
/// and for a listed author too: never defanged, never whole
#[test]
fn a_page_no_kind_names_is_a_link_in_every_view() {
    let mut read = post_with_body(&format!("Body {PAGES}"));
    read.post.title = format!("Title {PAGES}");
    read.post.agent_name = Some(NEWS.parse().unwrap());
    let mut inbox = inbox();
    inbox.messages[0].body = Some(format!("Message {PAGES}"));
    inbox.messages[1].body = Some(format!("Broadcast {PAGES}"));
    for config in [
        AgentDisplayConfig::default(),
        config(json!({"mode": "raw"})),
        config(
            json!({"mode": "defanged", "allow": ["https://subliminal.technology"]}),
        ),
        listing(&[NEWS]),
    ] {
        let shown = post_text(&read, &config);
        assert!(shown.contains(&format!("Title {PAGE_LINKS}")), "{shown}");
        assert!(shown.contains(&format!("Body {PAGE_LINKS}")), "{shown}");
        let site = site();
        let cx = DisplayContext::at(&config, Some(&site));
        let shown = Inbox {
            inbox: &inbox,
            once: &ShownOnce::default(),
            limit: None,
        }
        .agent_display(cx)
        .to_string();
        for what in ["Message", "Broadcast"] {
            assert!(
                shown.contains(&format!("{what} {PAGE_LINKS}")),
                "{what}: {shown}"
            );
        }
        let json = ContentResponse::Post(read.clone()).agent_json(cx).unwrap();
        assert_eq!(json["post"]["body"], format!("Body {PAGE_LINKS}"));
        assert_eq!(json["post"]["title"], format!("Title {PAGE_LINKS}"));
    }
    // The frozen server defanged them: the one class where the two differ
    assert_eq!(
        old_server(PAGES),
        "hxxps://subliminal[.]technology/agora/hello-world and \
         hxxps://subliminal[.]technology/agora/account"
    );
}
