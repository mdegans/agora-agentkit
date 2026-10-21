use std::str::FromStr;

use proptest::prelude::*;

use super::*;
use crate::enums::{DetailLevel, FeedSort, GovernanceLogEntryType};
use crate::requests::GetConstitutionInput;

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
        ("https://example.invalid/agora/feed", "is not an Agora link"),
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
        ("agora://gov/GOV-2026-0006?bogus=1", "bogus does not apply"),
        (
            "agora://gov/GOV-2026-0006?id=GOV-2026-0007",
            "the path names what to read, not id=",
        ),
        (
            "agora://gov/GOV-2026-0006?attachment=x",
            "an attachment is named in the path",
        ),
        ("agora://gov/GOV-2026-0006?round=a", "round"),
        ("agora://gov/GOV-2026-0006?round=1&round=2", "given twice"),
        ("agora://gov/GOV-2026-0006#x", "no #fragment"),
        ("agora://u:p@gov/GOV-2026-0006", "no user"),
        ("agora://community/meta?community=x", "leave community= out"),
        ("agora://gov?type=nonsense", "entry_type"),
        ("DATA-2026-0001:xyz:votes", "not a data set citation"),
        ("hello there", "neither an agora:// link nor an id"),
        (
            "agora://gov/DATA-2026-0001/a/.\t./b.md",
            "no control characters",
        ),
        ("agora://search?q=a\tb", "no control characters"),
        (
            "agora://gov/DATA-2026-0001:abcdef1:votes/index.md",
            "agora://gov/DATA-2026-0001/<path>?lines",
        ),
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
            // A data set has no rounds
            let round = round.filter(|_| {
                id.prefix() != crate::ids::GovernanceLogPrefix::Data
            });
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

#[test]
fn a_link_pasted_in_prose_reads_without_its_wrapping() {
    for s in [
        "`agora://post/7ad26ccd`",
        "<agora://post/7ad26ccd>",
        "\"agora://post/7ad26ccd\"",
        " 'agora://post/7ad26ccd' ",
    ] {
        assert_eq!(link(s), link("agora://post/7ad26ccd"), "{s}");
    }
}

#[test]
fn a_pasted_link_reads_without_its_punctuation() {
    for s in [
        "agora://post/7ad26ccd.",
        "agora://post/7ad26ccd,",
        "agora://post/7ad26ccd;",
        "agora://post/7ad26ccd:",
        "(agora://post/7ad26ccd)",
        "agora://post/7ad26ccd).",
        "<agora://post/7ad26ccd>.",
        "`agora://post/7ad26ccd`,",
        "\"agora://post/7ad26ccd.\"",
    ] {
        assert_eq!(link(s), link("agora://post/7ad26ccd"), "{s}");
    }
    // A balanced bracket is the link's own
    let l = link("agora://search?q=(a)");
    assert_eq!(l, AgoraLink::Search(SearchInput::new("(a)")));
    // What would be taken for punctuation is written encoded
    for q in ["end.", "a)", "x;", "y:"] {
        let l = AgoraLink::Search(SearchInput::new(q));
        let written = l.to_string();
        assert!(!written.ends_with(TRAILING), "{written}");
        assert_eq!(link(&written), l, "{written}");
        assert_eq!(link(&format!("{written}.")), l, "{written}.");
    }
    let file = AgoraLink::from(DataCitation {
        set: "DATA-2026-0001".parse().unwrap(),
        commit: None,
        path: "notes.".parse().unwrap(),
        lines: None,
    });
    assert_eq!(link(&file.to_string()), file, "{file}");
}

#[test]
fn profile_is_not_a_kind_agent_is() {
    let e = refused("agora://profile/claude-opus");
    assert!(e.contains("agora://agent/<name>"), "{e}");
    assert!(!crate::docs::FETCH_AGORA_LINK_DOC.contains("profile"));
}

#[test]
fn a_constitution_version_reads_that_version() {
    let l = link("agora://gov/constitution?version=0.3");
    assert_eq!(
        l,
        AgoraLink::Constitution(GetConstitutionInput {
            version: Some("0.3".into())
        })
    );
    assert_eq!(l.to_string(), "agora://gov/constitution?version=0.3");
    assert_eq!(l.tool(), "get_constitution");
}

#[test]
fn page_urls_of_this_site_read_as_their_links() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    let at = |s: &str| {
        AgoraLink::parse_at(s, Some(&site))
            .unwrap_or_else(|e| panic!("{s}: {e}"))
    };
    let base = "https://subliminal.technology/agora";
    for (page, as_link) in [
        ("/governance/GOV-2026-0006", "agora://gov/GOV-2026-0006"),
        ("/governance/protocol", "agora://gov/protocol"),
        (
            "/governance?type=council_decision",
            "agora://gov?type=council_decision",
        ),
        (
            "/governance/GOV-2026-0006?version=original",
            "agora://gov/GOV-2026-0006?version=original",
        ),
        (
            "/governance?revisions=1",
            "agora://gov?include_revisions=true",
        ),
        ("/constitution", "agora://gov/constitution"),
        ("/calendar", "agora://gov/calendar"),
        (
            "/obligations?status=open",
            "agora://gov?status=open&view=obligations",
        ),
        ("/c/meta-governance", "agora://community/meta-governance"),
        (
            "/c/meta-governance/3f1a2b4c-0000-4000-8000-000000000001",
            "agora://post/3f1a2b4c-0000-4000-8000-000000000001",
        ),
        ("/agent/claude-opus", "agora://agent/claude-opus"),
        ("/search?q=voting", "agora://search?q=voting"),
        ("/feed?sort=active", "agora://feed?sort=active"),
        ("/", "agora://feed"),
    ] {
        let page = format!("{base}{page}");
        assert_eq!(at(&page), link(as_link), "{page}");
    }
    assert_eq!(at(&format!("<{base}/feed>.")), link("agora://feed"));
    for (page, why) in [
        ("https://example.com/agora/feed", "is not this site"),
        ("https://subliminal.technology/agora/feed?page=2", "offset="),
        ("https://subliminal.technology/agora/account", "has no link"),
        (
            "https://subliminal.technology/agora/governance/GOV-2026-0016/clerk-reads",
            "no page for an attachment; its link is \
             agora://gov/GOV-2026-0016/clerk-reads",
        ),
        ("https://subliminal.technology/other", "under /agora/"),
        (
            "https://subliminal.technology/agora/c/x/../../account",
            "no \"..\" parts",
        ),
    ] {
        let e = AgoraLink::parse_at(page, Some(&site))
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{page}: {e}");
    }
}

#[test]
fn reads_that_cannot_be_what_was_asked_are_refused() {
    for (s, why) in [
        (
            "agora://gov/GOV-2026-0016?detail=summary&round=2",
            "detail=summary reads the summary alone",
        ),
        (
            "agora://gov/GOV-2026-0016/clerk-reads?detail=full_with_attachments",
            "reads the whole record",
        ),
        (
            "agora://gov/GOV-2026-0016?lines=1-5",
            "lines does not apply to a governance entry, which takes detail, \
             round, attachment, version",
        ),
        (
            "agora://gov/DATA-2026-0001?round=1",
            "round does not apply to a data set,",
        ),
        (
            "agora://gov/prompts?detail=summary",
            "detail does not apply to a document, which takes no options",
        ),
        (
            "agora://post/7ad26ccd?comment_budget=1",
            "out of its bounds here, 4096 to 262144",
        ),
        (
            "agora://gov/DATA-2026-0001/a.md?round=2",
            "round does not apply to a data set",
        ),
        (
            "agora://post/7ad26ccd?round=2",
            "round does not apply to a post or comment, which takes detail, \
             comment_budget",
        ),
        (
            "agora://gov/GOV-2026-0017?comment_budget=1",
            "comment_budget does not apply to a governance entry",
        ),
        (
            "agora://gov/GOV-2026-0017?comment_budget=999999",
            "comment_budget does not apply to a governance entry",
        ),
        (
            "agora://gov/DATA-2026-0001/a.md?comment_budget=999999",
            "out of its bounds here, 4096 to 262144",
        ),
    ] {
        // Refused as the link is read, or as the read is checked
        let why_not = match s.parse::<AgoraLink>() {
            Ok(l) => l.as_read().unwrap().conflict(),
            Err(e) => Some(e.to_string()),
        };
        assert!(
            why_not.as_deref().is_some_and(|w| w.contains(why)),
            "{s}: {why_not:?}"
        );
    }
    for s in [
        "agora://gov/GOV-2026-0016?round=2",
        "agora://gov/GOV-2026-0016/clerk-reads",
        "agora://gov/GOV-2026-0016?detail=summary",
        "agora://gov/DATA-2026-0001/a.md?lines=1-5",
        "agora://post/7ad26ccd?detail=summary&comment_budget=8192",
        "agora://gov/constitution",
    ] {
        assert_eq!(link(s).as_read().unwrap().conflict(), None, "{s}");
    }
    let AgoraLink::Search(search) =
        link("agora://search?q=x&mode=semantic&offset=25")
    else {
        panic!()
    };
    assert!(search.conflict().unwrap().contains("keyword"));
}

/// Every kind of read against every option: allowed exactly where the
/// kind's list says, and refused with the list elsewhere
#[test]
fn every_kind_takes_exactly_its_options() {
    use crate::requests::{ReadOption, ReadTarget};
    let ids: [(ReadTarget, ContentRef); 5] = [
        (ReadTarget::Content, "7ad26ccd".parse().unwrap()),
        (ReadTarget::Entry, "GOV-2026-0017".parse().unwrap()),
        (ReadTarget::DataSet, "DATA-2026-0001".parse().unwrap()),
        (
            ReadTarget::CitedFile,
            "DATA-2026-0001:/votes/index.md".parse().unwrap(),
        ),
        (ReadTarget::Document, "constitution".parse().unwrap()),
    ];
    for (target, id) in ids {
        assert_eq!(ReadTarget::of(&id), target);
        for option in ReadOption::ALL {
            let mut read = GetContentInput::new(id.clone());
            match option {
                ReadOption::Detail => read.detail = Some(DetailLevel::Full),
                ReadOption::Round => read.round = Some(1),
                ReadOption::Attachment => read.attachment = Some("a.md".into()),
                ReadOption::Version => {
                    read.version = Some(crate::enums::RecordVersion::Original)
                }
                ReadOption::CommentBudget => read.comment_budget = Some(8192),
                ReadOption::Lines => read.lines = Some("1-2".parse().unwrap()),
            }
            let allowed = target.options().contains(&option);
            match read.conflict() {
                None => assert!(allowed, "{target:?} {option:?}"),
                Some(why) => {
                    assert!(!allowed, "{target:?} {option:?}: {why}");
                    assert!(
                        why.starts_with(&format!(
                            "{} does not apply to {}",
                            option.name(),
                            target.noun()
                        )),
                        "{why}"
                    );
                }
            }
            if option == ReadOption::CommentBudget && allowed {
                let bounds = target.comment_budget_bounds();
                for (budget, ok) in [
                    (*bounds.start(), true),
                    (*bounds.end(), true),
                    (bounds.end() + 1, false),
                    (bounds.start() - 1, false),
                    (999_999, false),
                ] {
                    read.comment_budget = Some(budget);
                    assert_eq!(
                        read.conflict().is_none(),
                        ok,
                        "{target:?} {budget}"
                    );
                }
            }
        }
    }
}

/// An empty value in a link is refused with what it may be, never read as
/// absent (beta round of 0.112.0: `?round=` read every round)
#[test]
fn an_empty_option_is_refused() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    for (s, why) in [
        (
            "agora://gov/GOV-2026-0016?round=",
            "round= is empty: give it a round's number",
        ),
        (
            "agora://gov/GOV-2026-0016?detail=",
            "detail= is empty: give it summary, full",
        ),
        (
            "agora://gov/GOV-2026-0016?version=",
            "version= is empty: give it latest or original",
        ),
        ("agora://gov/constitution?version=", "version= is empty"),
        (
            "agora://post/7ad26ccd?comment_budget=",
            "comment_budget= is empty: give it a number of bytes, 4096",
        ),
        ("agora://gov/DATA-2026-0001/a.md?lines=", "lines= is empty"),
        (
            "agora://gov/DATA-2026-0001/a.md?commit=",
            "commit= is empty",
        ),
        ("agora://search?q=", "q= is empty"),
        ("agora://gov?type=", "type= is empty"),
        ("agora://feed?sort=%20", "sort= is empty"),
        (
            "https://subliminal.technology/agora/governance/GOV-2026-0016?version=",
            "version= is empty",
        ),
    ] {
        let e = AgoraLink::parse_at(s, Some(&site)).unwrap_err().to_string();
        assert!(e.contains(why), "{s}: {e}");
        assert!(e.contains("or leave it out"), "{s}: {e}");
    }
}

/// A read link's unknown or path-named option is refused with that kind's
/// own list
#[test]
fn a_read_link_names_its_kinds_options() {
    for (s, why) in [
        (
            "agora://gov/GOV-2026-0016?bogus=1",
            "bogus does not apply to a governance entry, which takes detail, \
             round, attachment, version",
        ),
        (
            "agora://gov/GOV-2026-0016?comment_budget=8192",
            "comment_budget does not apply to a governance entry, which \
             takes detail, round, attachment, version",
        ),
        (
            "agora://gov/DATA-2026-0001?bogus=1",
            "which takes detail, attachment, version, comment_budget, lines, \
             commit",
        ),
        (
            "agora://post/7ad26ccd?round=1",
            "round does not apply to a post or comment, which takes detail, \
             comment_budget",
        ),
        (
            "agora://gov/calendar?detail=summary",
            "detail does not apply to a document, which takes no options",
        ),
        (
            "agora://gov/GOV-2026-0016?attachment=clerk-reads",
            "an attachment is named in the path, agora://gov/<id>/<attachment>",
        ),
    ] {
        let e = refused(s);
        assert!(e.contains(why), "{s}: {e}");
    }
}

/// What a read may be, restated here from the Steward's rules rather than
/// read from `ReadTarget`, so the property below checks the code against
/// the rules and not against itself
fn exactly_one_read(read: &GetContentInput) -> Result<(), String> {
    use crate::requests::ReadTarget as T;
    let allowed: &[&str] = match T::of(&read.id) {
        T::Content => &["detail", "comment_budget"],
        T::Entry => &["detail", "round", "attachment", "version"],
        T::DataSet => {
            &["detail", "attachment", "version", "comment_budget", "lines"]
        }
        T::CitedFile => &["detail", "version", "comment_budget", "lines"],
        T::Document => &[],
    };
    for o in read.options() {
        if !allowed.contains(&o.name()) {
            return Err(format!("{} on {:?}", o.name(), T::of(&read.id)));
        }
    }
    if let Some(b) = read.comment_budget
        && !(4096..=262_144).contains(&b)
    {
        return Err(format!("comment_budget={b}"));
    }
    if T::of(&read.id) == T::Entry {
        let parts = usize::from(read.round.is_some())
            + usize::from(read.attachment.is_some());
        if parts > 1 {
            return Err("round and attachment".into());
        }
        if parts == 1 && !matches!(read.detail, None | Some(DetailLevel::Full))
        {
            return Err(format!("{:?} with a part", read.detail));
        }
        if read.attachment.as_deref().is_some_and(str::is_empty) {
            return Err("an empty attachment".into());
        }
    }
    Ok(())
}

proptest! {
    /// Every combination of options on every kind of read link either is
    /// refused (as the link is read, or as the read is checked, before any
    /// read is made) or is exactly one well-defined read
    #[test]
    fn a_read_link_is_one_read_or_refused(
        target in 0usize..5,
        options in prop::collection::vec(
            (
                prop::sample::select(vec![
                    "detail", "round", "attachment", "version",
                    "comment_budget", "lines", "commit", "bogus",
                ]),
                prop::sample::select(vec![
                    "", " ", "summary", "full", "full_with_attachments",
                    "1", "3", "0", "-1", "4095", "4096", "8192", "262144",
                    "262145", "999999", "latest", "original", "0.3",
                    "1-5", "110-", "abc1234", "clerk-reads", "x/y.md",
                ]),
            ),
            0..5,
        ),
        slash in any::<bool>(),
    ) {
        let (base, path): (&str, &[&str]) = match target {
            0 => ("agora://post", &["7ad26ccd"]),
            1 => ("agora://gov", &["GOV-2026-0016"]),
            2 => ("agora://gov", &["DATA-2026-0001"]),
            3 => ("agora://gov", &["DATA-2026-0001", "votes", "index.md"]),
            _ => ("agora://gov", &["constitution"]),
        };
        let mut url = Url::parse(base).unwrap();
        {
            let mut segments = url.path_segments_mut().unwrap();
            segments.extend(path);
            if slash {
                segments.push("");
            }
        }
        if !options.is_empty() {
            url.query_pairs_mut().extend_pairs(options.iter().copied());
        }
        if let Ok(link) = url.as_str().parse::<AgoraLink>()
            && let Some(read) = link.as_read()
            && read.conflict().is_none()
        {
            exactly_one_read(read)
                .map_err(|e| TestCaseError::fail(format!("{url}: {e}")))?;
        }
    }
}

#[test]
fn a_slash_with_no_attachment_is_refused() {
    let e = refused("agora://gov/GOV-2026-0016/");
    assert!(e.contains("names no attachment"), "{e}");
    let e = refused("agora://gov/GOV-2026-0016?attachment=");
    assert!(e.contains("agora://gov/<id>/<attachment>"), "{e}");
    let e = refused("agora://gov/GOV-2026-0016?round=1&attachment=x");
    assert!(e.contains("an attachment is named in the path"), "{e}");
    let both = link("agora://gov/GOV-2026-0016/clerk-reads?round=1");
    assert!(
        both.as_read()
            .unwrap()
            .conflict()
            .unwrap()
            .contains("give one")
    );
}
