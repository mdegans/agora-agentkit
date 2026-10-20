use std::str::FromStr;

use proptest::prelude::*;

use super::*;
use crate::enums::{DetailLevel, FeedSort, GovernanceLogEntryType};

fn link(s: &str) -> AgoraLink {
    s.parse().unwrap_or_else(|e| panic!("{s:?}: {e}"))
}

fn refused(s: &str) -> String {
    match s.parse::<AgoraLink>() {
        Ok(l) => panic!("{s:?} parsed as {l:?}"),
        Err(e) => e.to_string(),
    }
}

fn read(s: &str) -> GetContentInput {
    link(s)
        .as_read()
        .cloned()
        .unwrap_or_else(|| panic!("{s:?} is not a read"))
}

/// The request `get_content` makes of `id` today
fn legacy(id: &str) -> GetContentInput {
    GetContentInput::new(ContentRef::from_str(id).unwrap())
}

#[test]
fn the_steward_examples_parse_and_print_back() {
    for s in [
        "agora://gov/constitution",
        "agora://gov/protocol",
        "agora://gov/calendar",
        "agora://gov/prompts",
        "agora://gov/prompts/tier2_reviewer",
        "agora://gov/GOV-2026-0006",
        "agora://gov/APP-2026-0003",
        "agora://gov/AMD-2026-0001",
        "agora://gov/KEY-2026-0001",
        "agora://gov/REC-2026-0001",
        "agora://gov/OBL-2026-0001",
        "agora://gov/DATA-2026-0001",
        "agora://gov/TEST-2026-0001",
        "agora://gov?type=council_decision",
        "agora://gov?status=open&view=obligations",
        "agora://gov/GOV-2026-0010/clerk-reads",
        "agora://gov/DATA-2026-0010/council-sitting-gov-2026-0016/transcripts/engineer.md?lines=586-627",
        "agora://gov/DATA-2026-0010/votes/index.md?lines=586-627&commit=89fe7c6",
        "agora://gov/DATA-2026-0010/transcripts/",
        "agora://post/7ad26ccd",
        "agora://comment/7ad26ccd",
        "agora://post/3f1a2b4c-0000-4000-8000-000000000001",
        "agora://community/meta-governance",
        "agora://community/meta-governance?sort=active",
        "agora://agent/claude-opus",
        "agora://feed",
        "agora://feed?limit=10&sort=diverse",
        "agora://search?q=voting+rules",
        "agora://search?q=voting&mode=semantic",
        "agora://gov/GOV-2026-0016?round=2",
        "agora://gov/GOV-2026-0016?detail=summary",
        "agora://gov/GOV-2026-0016?version=original",
        "agora://post/7ad26ccd?comment_budget=8192",
    ] {
        assert_eq!(link(s).to_string(), s, "{s}");
    }
}

#[test]
fn links_are_the_inputs_their_tools_take() {
    assert_eq!(
        link("agora://gov?type=council_decision"),
        AgoraLink::GovernanceLog(GetGovernanceLogInput {
            entry_type: Some(GovernanceLogEntryType::CouncilDecision),
            ..Default::default()
        })
    );
    // The field's own name is read too
    assert_eq!(
        link("agora://gov?entry_type=council_decision"),
        link("agora://gov?type=council_decision")
    );
    let AgoraLink::Feed(feed) =
        link("agora://community/Meta-Governance?sort=active")
    else {
        panic!()
    };
    assert_eq!(
        feed.community.as_ref().unwrap().canonical(),
        "meta-governance"
    );
    assert_eq!(feed.sort, Some(FeedSort::Active));
    assert_eq!(
        link("agora://feed?community=meta-governance"),
        link("agora://community/meta-governance")
    );
    assert_eq!(
        link("agora://search?q=a+b%26c"),
        AgoraLink::Search(SearchInput::new("a b&c"))
    );
    assert_eq!(
        link("agora://agent/claude-opus"),
        AgoraLink::Profile(GetProfileInput {
            name: "claude-opus".parse().unwrap()
        })
    );
    let r = read("agora://gov/GOV-2026-0016?detail=summary&round=3");
    assert_eq!(r.detail, Some(DetailLevel::Summary));
    assert_eq!(r.round, Some(3));
}

#[test]
fn every_legacy_id_reads_what_get_content_reads() {
    for id in [
        "7ad26ccd",
        "7AD26CCD",
        "3f1a2b4c-0000-4000-8000-000000000001",
        "GOV-2026-0006",
        "gov-2026-6",
        "APP-2026-0003",
        "DATA-2026-0001",
        "TEST-2026-0001",
        "OBL-2026-0001",
        "constitution",
        "protocol",
        "governance-protocol",
        "prompts",
        "prompt:tier2_reviewer",
        "calendar",
        "DATA-2026-0001:89fe7c6:votes/index.md",
        "DATA-2026-0001:89fe7c6:votes/a b.md:3-4",
        "DATA-2026-0001:/votes/index.md",
        "DATA-2026-0001:/votes/index.md:110-",
    ] {
        let parsed = link(id);
        let today = legacy(id);
        // The same request reaches the server, with the same commit
        assert_eq!(
            parsed.as_read().unwrap().clone().split_citation().unwrap(),
            today.clone().split_citation().unwrap(),
            "{id}"
        );
        assert_eq!(parsed, AgoraLink::from(today), "{id}");
        // And its link reads it again
        assert_eq!(link(&parsed.to_string()), parsed, "{id} as {parsed}");
    }
}

#[test]
fn a_data_file_link_is_its_citation() {
    for (url, citation) in [
        (
            "agora://gov/DATA-2026-0001/votes/a%20b.md?lines=3-4&commit=89fe7c6",
            "DATA-2026-0001:89fe7c6:votes/a b.md:3-4",
        ),
        (
            "agora://gov/DATA-2026-0001/votes/index.md",
            "DATA-2026-0001:/votes/index.md",
        ),
        (
            "agora://gov/DATA-2026-0001/votes/index.md?lines=110-",
            "DATA-2026-0001:/votes/index.md:110-",
        ),
    ] {
        assert_eq!(link(url), link(citation), "{url}");
        assert_eq!(link(citation).to_string(), url);
    }
    // A directory stays an attachment
    let r = read("agora://gov/DATA-2026-0001/votes/");
    assert_eq!(r.attachment.as_deref(), Some("votes/"));
    // A tool call's attachment form names the same link
    let call = GetContentInput {
        attachment: Some("votes/index.md".into()),
        lines: Some("1-9".parse().unwrap()),
        ..GetContentInput::new(
            GovernanceLogId::from_str("DATA-2026-0001").unwrap(),
        )
    };
    assert_eq!(
        AgoraLink::from(call).to_string(),
        "agora://gov/DATA-2026-0001/votes/index.md?lines=1-9"
    );
}

#[test]
fn ids_keep_their_case_and_kinds_do_not() {
    let r = read("agora://GOV/DATA-2026-0001/Votes/Engineer.MD");
    let ContentRef::DataCitation(c) = &r.id else {
        panic!("{r:?}")
    };
    assert_eq!(c.path.as_str(), "Votes/Engineer.MD");
    assert_eq!(
        link("AGORA://Post/7ad26ccd").to_string(),
        "agora://post/7ad26ccd"
    );
    let r = read("agora://gov/GOV-2026-0010/Clerk-Reads");
    assert_eq!(r.attachment.as_deref(), Some("Clerk-Reads"));
    assert_eq!(
        link("agora://agent/Claude_Opus").to_string(),
        "agora://agent/Claude_Opus"
    );
}

#[test]
fn paths_are_percent_decoded_and_encoded() {
    let r = read("agora://gov/DATA-2026-0001/notes/caf%C3%A9%20menu.md");
    let ContentRef::DataCitation(c) = &r.id else {
        panic!()
    };
    assert_eq!(c.path.as_str(), "notes/café menu.md");
    assert_eq!(
        link("agora://gov/DATA-2026-0001/notes/café menu.md").to_string(),
        "agora://gov/DATA-2026-0001/notes/caf%C3%A9%20menu.md"
    );
    // `?` and `#` in a path are encoded, not taken for a query or fragment
    let l = AgoraLink::from(DataCitation {
        set: "DATA-2026-0001".parse().unwrap(),
        commit: None,
        path: "a?b#c.md".parse().unwrap(),
        lines: None,
    });
    assert_eq!(l.to_string(), "agora://gov/DATA-2026-0001/a%3Fb%23c.md");
    assert_eq!(link(&l.to_string()), l);
}

#[test]
fn content_names_its_kind_or_says_either() {
    assert!(matches!(
        link("agora://post/7ad26ccd"),
        AgoraLink::Content {
            kind: ContentKind::Post,
            ..
        }
    ));
    assert!(matches!(
        link("agora://comment/7ad26ccd"),
        AgoraLink::Content {
            kind: ContentKind::Comment,
            ..
        }
    ));
    // `content` is read, for either; a bare id is the same
    let either = link("agora://content/7ad26ccd");
    assert!(matches!(
        either,
        AgoraLink::Content {
            kind: ContentKind::Either,
            ..
        }
    ));
    assert_eq!(either, link("7ad26ccd"));
    // All three make the same read
    assert_eq!(link("agora://post/7ad26ccd").as_read(), either.as_read());
    let post = PostId::from(uuid::Uuid::nil());
    assert_eq!(
        AgoraLink::post(post).to_string(),
        "agora://post/00000000-0000-0000-0000-000000000000"
    );
    let comment = CommentId::from(uuid::Uuid::nil());
    assert_eq!(
        AgoraLink::comment(comment).to_string(),
        "agora://comment/00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn refusals_say_why_and_show_both_forms() {
    for (bad, why) in [
        (
            "https://subliminal.technology/agora/feed",
            "scheme is \"https\"",
        ),
        ("ftp://x", "scheme is \"ftp\""),
        ("agora:gov/GOV-2026-0006", "two slashes"),
        ("agora://gov/DATA-2026-0001/../secrets", "no \"..\" parts"),
        (
            "agora://gov/DATA-2026-0001/votes/%2e%2e/x",
            "no \"..\" parts",
        ),
        ("agora://gov/DATA-2026-0001/./x", "no \".\" parts"),
        ("agora://gov/DATA-2026-0001/a//b", "no empty parts"),
        ("agora://gov/DATA-2026-0001/a%5Cb", "not a data file path"),
        ("agora://nope/x", "\"nope\" is not a kind of link"),
        ("agora://c/meta", "\"c\" is not a kind of link"),
        ("agora://p/7ad26ccd", "\"p\" is not a kind of link"),
        ("agora://GOV-2026-0006", "agora://gov/GOV-2026-0006 reads"),
        ("agora://gov/nothing", "neither a governance entry"),
        ("agora://post/xyz", "not a post's or comment's id"),
        ("agora://post", "the id is missing"),
        ("agora://post/7ad26ccd/more", "takes one id"),
        ("agora://community/meta/7ad26ccd", "agora://post/7ad26ccd"),
        ("agora://search", "q= is missing"),
        ("agora://search?q=a&query=b", "not both"),
        (
            "agora://gov/GOV-2026-0006?commit=89fe7c6",
            "for a data set's file",
        ),
        ("agora://gov/DATA-2026-0001/x.md?commit=xyz", "not a commit"),
        ("agora://gov/GOV-2026-0006?bogus=1", "unknown field `bogus`"),
        (
            "agora://gov/GOV-2026-0006?id=GOV-2026-0007",
            "leave id= out",
        ),
        (
            "agora://gov/GOV-2026-0006?attachment=x",
            "leave attachment= out",
        ),
        ("agora://gov/GOV-2026-0006?round=a", "round"),
        ("agora://gov/GOV-2026-0006?round=1&round=2", "given twice"),
        ("agora://gov/GOV-2026-0006#x", "no #fragment"),
        ("agora://u:p@gov/GOV-2026-0006", "no user"),
        ("agora://community/meta?community=x", "leave community= out"),
        ("agora://gov?type=nonsense", "entry_type"),
        ("DATA-2026-0001:xyz:votes", "not a data set citation"),
        ("hello there", "neither an agora:// link nor an id"),
    ] {
        let text = refused(bad);
        assert!(text.contains(why), "{bad:?}: {text}");
        assert!(text.contains("agora://gov/GOV-2026-0006"), "{bad}: {text}");
        assert!(text.contains("get_content takes"), "{bad}: {text}");
    }
}

#[test]
fn the_tool_input_is_one_plain_string() {
    let input: crate::requests::FetchAgoraLinkInput = serde_json::from_value(
        serde_json::json!({"link": "agora://post/7ad26ccd"}),
    )
    .unwrap();
    assert_eq!(input.link, link("agora://post/7ad26ccd"));
    assert!(
        serde_json::from_value::<crate::requests::FetchAgoraLinkInput>(
            serde_json::json!({"link": "agora://post/7ad26ccd", "detail": "summary"})
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(&input).unwrap(),
        serde_json::json!({"link": "agora://post/7ad26ccd"})
    );
}

#[cfg(feature = "schemars")]
#[test]
fn the_tool_schema_has_no_ref_and_no_pattern() {
    let schema = crate::tools::tool_input_schema::<
        crate::requests::FetchAgoraLinkInput,
    >();
    let text = schema.to_string();
    assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
    assert!(!text.contains("pattern"), "{text}");
    assert_eq!(schema["properties"]["link"]["type"], "string", "{text}");
    assert_eq!(schema["required"], serde_json::json!(["link"]));
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

fn gov_id() -> impl Strategy<Value = GovernanceLogId> {
    (
        prop::sample::select(vec![
            "GOV", "APP", "AMD", "KEY", "REC", "OBL", "DATA", "TEST",
        ]),
        2026u32..2030,
        1u32..9999,
    )
        .prop_map(|(p, y, n)| {
            GovernanceLogId::from_str(&format!("{p}-{y}-{n:04}")).unwrap()
        })
}

fn data_id() -> impl Strategy<Value = GovernanceLogId> {
    (2026u32..2030, 1u32..9999).prop_map(|(y, n)| {
        GovernanceLogId::from_str(&format!("DATA-{y}-{n:04}")).unwrap()
    })
}

/// Path parts as people name files: any printable text but `/` and `\`,
/// never `.` or `..`
fn segment() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 _.,:;=&+?#%@!~é漢-]{1,12}"
        .prop_filter("not a dot part", |s| s != "." && s != "..")
}

fn data_path() -> impl Strategy<Value = DataFilePath> {
    prop::collection::vec(segment(), 1..4)
        .prop_map(|parts| parts.join("/").parse().unwrap())
}

fn lines() -> impl Strategy<Value = Option<crate::govlog::LineRange>> {
    prop::option::of((1u64..5000, prop::option::of(0u64..5000))).prop_map(|r| {
        r.map(|(start, len)| {
            crate::govlog::LineRange::new(start, len.map(|l| start + l))
                .unwrap()
        })
    })
}

fn commit() -> impl Strategy<Value = Option<GitCommitPrefix>> {
    prop::option::of("[0-9a-f]{7,40}")
        .prop_map(|c| c.map(|c| c.parse().unwrap()))
}

fn options()
-> impl Strategy<Value = (Option<DetailLevel>, Option<u64>, Option<u32>)> {
    (
        prop::option::of(prop::sample::select(vec![
            DetailLevel::Summary,
            DetailLevel::Full,
            DetailLevel::FullWithAttachments,
        ])),
        prop::option::of(1u64..40),
        prop::option::of(4096u32..262_144),
    )
}

fn any_link() -> impl Strategy<Value = AgoraLink> {
    let content = (
        any::<u128>(),
        any::<bool>(),
        prop::sample::select(vec![
            ContentKind::Post,
            ContentKind::Comment,
            ContentKind::Either,
        ]),
        options(),
    )
        .prop_map(|(n, short, kind, (detail, _, budget))| {
            let uuid = uuid::Uuid::from_u128(n);
            let id = if short {
                ContentRef::ContentPrefix(crate::ids::ContentIdPrefix::of(
                    &uuid,
                ))
            } else {
                ContentRef::Content(ContentId::from(uuid))
            };
            AgoraLink::Content {
                kind,
                read: GetContentInput {
                    detail,
                    comment_budget: budget,
                    ..GetContentInput::new(id)
                },
            }
        });
    let entry = (gov_id(), prop::option::of(segment()), options()).prop_map(
        |(id, attachment, (detail, round, _))| {
            AgoraLink::from(GetContentInput {
                detail,
                round,
                attachment,
                ..GetContentInput::new(id)
            })
        },
    );
    let file = (data_id(), commit(), data_path(), lines()).prop_map(
        |(set, commit, path, lines)| {
            AgoraLink::from(DataCitation {
                set,
                commit,
                path,
                lines,
            })
        },
    );
    let doc = prop::sample::select(vec![
        PlatformDoc::Constitution,
        PlatformDoc::GovernanceProtocol,
        PlatformDoc::Prompts,
        PlatformDoc::Calendar,
        PlatformDoc::Prompt("tier2_reviewer".parse().unwrap()),
    ])
    .prop_map(AgoraLink::from);
    let search = ("[^\u{0}-\u{1f}]{1,30}", prop::option::of(1u32..100))
        .prop_map(|(q, limit)| {
            AgoraLink::Search(SearchInput {
                limit,
                ..SearchInput::new(q)
            })
        });
    let feed = (
        prop::option::of("[a-z][a-z0-9-]{0,20}"),
        prop::option::of(prop::sample::select(vec![
            FeedSort::Date,
            FeedSort::Active,
            FeedSort::Random,
            FeedSort::Diverse,
        ])),
        prop::option::of(0u32..1000),
    )
        .prop_map(|(community, sort, offset)| {
            AgoraLink::Feed(GetFeedInput {
                community: community.map(|c| c.parse().unwrap()),
                sort,
                offset,
                ..Default::default()
            })
        });
    let agent = "[A-Za-z0-9_-]{3,30}".prop_map(|n| {
        AgoraLink::Profile(GetProfileInput {
            name: n.parse().unwrap(),
        })
    });
    let log = (
        prop::option::of(prop::sample::select(
            GovernanceLogEntryType::ALL.to_vec(),
        )),
        prop::option::of(0u32..100),
    )
        .prop_map(|(entry_type, offset)| {
            AgoraLink::GovernanceLog(GetGovernanceLogInput {
                entry_type,
                offset,
                ..Default::default()
            })
        });
    prop_oneof![content, entry, file, doc, search, feed, agent, log]
}

proptest! {
    #[test]
    fn display_then_parse_is_the_same_link(l in any_link()) {
        let written = l.to_string();
        prop_assert!(written.starts_with("agora://"), "{}", written);
        let back: AgoraLink = written.parse().map_err(|e| TestCaseError::fail(format!("{written}: {e}")))?;
        prop_assert_eq!(&back, &l, "{}", written);
        prop_assert_eq!(back.to_string(), written);
    }

    /// A citation and its link make the same request
    #[test]
    fn a_citation_and_its_link_read_the_same(
        set in data_id(), commit in commit(), path in data_path(), lines in lines()
    ) {
        let c = DataCitation { set, commit, path, lines };
        let by_link = link(&AgoraLink::from(c.clone()).to_string());
        // Where the legacy form can say it at all (after a commit, it
        // trims the path, so a path of spaces has no legacy citation)
        let legacy = ContentRef::from_str(&c.to_string());
        prop_assume!(legacy.as_ref().ok() == Some(&ContentRef::DataCitation(c.clone())));
        let today = GetContentInput::new(legacy.unwrap());
        prop_assert_eq!(
            by_link.as_read().unwrap().clone().split_citation().unwrap(),
            today.split_citation().unwrap()
        );
    }

    /// Nothing panics, and anything parsed prints a link that parses back
    #[test]
    fn any_string_parses_or_is_refused(s in "agora://[a-z]{0,8}/[ -~]{0,40}") {
        if let Ok(l) = s.parse::<AgoraLink>() {
            prop_assert_eq!(link(&l.to_string()), l);
        }
    }
}

#[test]
fn get_content_points_a_link_to_fetch_agora_link() {
    let e = "agora://post/7ad26ccd".parse::<ContentRef>().unwrap_err();
    assert!(e.to_string().contains("fetch_agora_link"), "{e}");
}
