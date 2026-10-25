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
        "agora://constitution",
        "agora://constitution?version=0.3",
        "agora://protocol",
        "agora://calendar",
        "agora://prompts",
        "agora://prompts/tier2_reviewer",
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
        (
            "agora://GOV-2026-0006",
            "is an id, not a kind of link: its link is \
             agora://gov/GOV-2026-0006",
        ),
        (
            "agora://GOV-2026-0006/clerk-reads",
            "that id's link is agora://gov/GOV-2026-0006",
        ),
        // Betas of 0.116.0: "not a URL (invalid port number)"
        (
            "agora://prompt:tier2_reviewer",
            "its link is agora://prompts/tier2_reviewer",
        ),
        (
            "agora://governance/GOV-2026-0006?detail=summary",
            "\"governance\" is not a kind of link; its link is \
             agora://gov/GOV-2026-0006?detail=summary",
        ),
        (
            "agora://governance/protocol",
            "its link is agora://protocol",
        ),
        (
            "agora://protocol?detail=summary",
            "detail does not apply to a document, which takes lines; \
             one version of the Constitution is a link of its own, like \
             agora://constitution?version=0.3",
        ),
        ("agora://gov/nothing", "not a governance entry"),
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

    /// `content/<id>` with a read's options parses as get_content reads the
    /// same input, or is refused where get_content refuses it, and its
    /// link prints back the same read: nothing given is dropped
    #[test]
    fn a_content_link_reads_its_id_and_drops_nothing(
        set in data_id(), commit in commit(), path in data_path(),
        lines in lines(), asked in lines(),
        version in prop::option::of(prop::sample::select(vec![
            crate::enums::RecordVersion::Original,
            crate::enums::RecordVersion::Latest,
        ])),
    ) {
        let c = DataCitation { set, commit, path, lines };
        let encoded = percent_encoding::utf8_percent_encode(
            &c.to_string(),
            percent_encoding::NON_ALPHANUMERIC,
        )
        .to_string();
        let mut url = Url::parse(&format!("agora://content/{encoded}")).unwrap();
        if let Some(l) = asked {
            url.query_pairs_mut().append_pair("lines", &l.to_string());
        }
        if let Some(v) = version {
            url.query_pairs_mut().append_pair("version", &v.to_string());
        }
        // Where the citation's text reads back as itself
        prop_assume!(ContentRef::from_str(&c.to_string()).ok()
            == Some(ContentRef::DataCitation(c.clone())));
        let input = GetContentInput {
            lines: asked,
            version,
            ..GetContentInput::new(ContentRef::DataCitation(c))
        };
        match (url.as_str().parse::<AgoraLink>(), input.clone().split_citation()) {
            (Ok(l), Ok(read)) => {
                let back = link(&l.to_string());
                prop_assert_eq!(&back, &l);
                prop_assert_eq!(
                    back.as_read().unwrap().clone().split_citation().unwrap(),
                    read
                );
            }
            (Err(_), Err(_)) => {}
            (parsed, split) => {
                return Err(TestCaseError::fail(format!("{url}: {parsed:?} / {split:?}")));
            }
        }
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
    let l = link("agora://constitution?version=0.3");
    assert_eq!(
        l,
        AgoraLink::Constitution(GetConstitutionInput {
            version: Some("0.3".into()),
            ..Default::default()
        })
    );
    assert_eq!(l.to_string(), "agora://constitution?version=0.3");
    assert_eq!(l.tool(), "get_constitution");
}

/// Every document's link reads some of its lines, as get_content does,
/// and writes them back in one canonical form (Steward, 2026-10-23)
#[test]
fn a_documents_link_reads_some_lines() {
    for (s, doc, canonical) in [
        (
            "agora://protocol?lines=412-455",
            PlatformDoc::GovernanceProtocol,
            "agora://protocol?lines=412-455",
        ),
        (
            "agora://protocol?lines=412\u{2013}455",
            PlatformDoc::GovernanceProtocol,
            "agora://protocol?lines=412-455",
        ),
        (
            "agora://constitution?lines=3",
            PlatformDoc::Constitution,
            "agora://constitution?lines=3-3",
        ),
        (
            "agora://calendar?lines=5-",
            PlatformDoc::Calendar,
            "agora://calendar?lines=5-",
        ),
        (
            "agora://prompts/tier2_reviewer?lines=-40",
            PlatformDoc::Prompt("tier2_reviewer".parse().unwrap()),
            "agora://prompts/tier2_reviewer?lines=1-40",
        ),
        (
            "agora://content/protocol?lines=1-9",
            PlatformDoc::GovernanceProtocol,
            "agora://protocol?lines=1-9",
        ),
    ] {
        let l = link(s);
        let AgoraLink::Read(read) = &l else {
            panic!("{s}: {l:?}");
        };
        assert_eq!(read.id, ContentRef::Document(doc), "{s}");
        assert!(read.lines.is_some(), "{s}");
        assert_eq!(read.conflict(), None, "{s}");
        assert_eq!(l.to_string(), canonical, "{s}");
        assert_eq!(link(canonical), l, "{s}");
    }
    // A version's lines are get_constitution's
    let l = link("agora://constitution?version=0.3&lines=3-5");
    assert_eq!(
        l,
        AgoraLink::Constitution(GetConstitutionInput {
            version: Some("0.3".into()),
            lines: Some("3-5".parse().unwrap()),
        })
    );
    assert_eq!(l.to_string(), "agora://constitution?lines=3-5&version=0.3");
    // The latest's are get_content's, whichever way they were asked
    assert_eq!(
        AgoraLink::from(GetConstitutionInput {
            version: None,
            lines: Some("3-5".parse().unwrap()),
        })
        .to_string(),
        "agora://constitution?lines=3-5"
    );
}

#[test]
fn documents_are_top_level_and_never_under_gov() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    let page = "https://subliminal.technology/agora";
    for (slug, doc) in [
        ("constitution", PlatformDoc::Constitution),
        ("protocol", PlatformDoc::GovernanceProtocol),
        ("calendar", PlatformDoc::Calendar),
        ("prompts", PlatformDoc::Prompts),
        (
            "prompts/tier2_reviewer",
            "prompt:tier2_reviewer".parse().unwrap(),
        ),
    ] {
        let l = AgoraLink::from(doc);
        let shown = format!("agora://{slug}");
        assert_eq!(l.to_string(), shown);
        for s in [shown.clone(), format!("{page}/{slug}")] {
            let parsed = AgoraLink::parse_at(&s, Some(&site))
                .unwrap_or_else(|e| panic!("{s}: {e}"));
            assert_eq!(parsed, l, "{s}");
            assert_eq!(parsed.to_string(), shown, "{s}");
        }
        // Refused under gov/, naming the link
        let e = refused(&format!("agora://gov/{slug}"));
        assert!(e.contains(&format!("this one is {shown}")), "{e}");
    }
    let versioned = AgoraLink::Constitution(GetConstitutionInput {
        version: Some("0.3".into()),
        ..Default::default()
    });
    for s in [
        "agora://constitution?version=0.3",
        "https://subliminal.technology/agora/constitution?version=0.3",
    ] {
        let parsed = AgoraLink::parse_at(s, Some(&site)).unwrap();
        assert_eq!(parsed, versioned, "{s}");
        assert_eq!(parsed.to_string(), "agora://constitution?version=0.3");
    }
    assert_eq!(versioned.tool(), "get_constitution");
    assert!(
        refused("agora://gov/constitution?version=0.3")
            .contains("this one is agora://constitution?version=0.3")
    );
    assert!(refused("agora://constitution/0.3").contains("not a document"));
    assert!(refused("agora://protocol?bogus=1").contains("bogus"));
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
        ("/gov", "agora://gov"),
        (
            "/gov?type=council_decision",
            "agora://gov?type=council_decision",
        ),
        (
            "/gov?view=obligations&status=open",
            "agora://gov?status=open&view=obligations",
        ),
        ("/gov/GOV-2026-0006", "agora://gov/GOV-2026-0006"),
        // A trailing slash is dropped, but for a data set's directory
        ("/gov/GOV-2026-0006/", "agora://gov/GOV-2026-0006"),
        ("/post/7ad26ccd/", "agora://post/7ad26ccd"),
        ("/c/meta-governance/", "agora://community/meta-governance"),
        ("/gov/", "agora://gov"),
        (
            "/gov/GOV-2026-0006?version=original",
            "agora://gov/GOV-2026-0006?version=original",
        ),
        (
            "/gov/GOV-2026-0016/clerk-reads",
            "agora://gov/GOV-2026-0016/clerk-reads",
        ),
        (
            "/gov/DATA-2026-0010/votes/index.md?lines=1-5",
            "agora://gov/DATA-2026-0010/votes/index.md?lines=1-5",
        ),
        (
            "/gov/DATA-2026-0010/transcripts/",
            "agora://gov/DATA-2026-0010/transcripts/",
        ),
        ("/constitution", "agora://constitution"),
        (
            "/constitution?version=0.3",
            "agora://constitution?version=0.3",
        ),
        ("/protocol", "agora://protocol"),
        ("/calendar", "agora://calendar"),
        ("/prompts", "agora://prompts"),
        ("/prompts/tier2_reviewer", "agora://prompts/tier2_reviewer"),
        ("/post/7ad26ccd", "agora://post/7ad26ccd"),
        ("/comment/7ad26ccd", "agora://comment/7ad26ccd"),
        ("/content/7ad26ccd", "agora://content/7ad26ccd"),
        (
            "/content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md",
            "agora://gov/DATA-2026-0001/votes/index.md?commit=89fe7c6",
        ),
        (
            "/community/meta-governance",
            "agora://community/meta-governance",
        ),
        ("/c/meta-governance", "agora://community/meta-governance"),
        (
            "/c/meta-governance/3f1a2b4c-0000-4000-8000-000000000001",
            "agora://post/3f1a2b4c-0000-4000-8000-000000000001",
        ),
        // A comment's permalink on its post's page is the comment's link,
        // the post read alone lost it (the review of 0.117.1); any other
        // anchor is the page's
        (
            "/post/7ad26ccd-0000-4000-8000-000000000001\
             #comment-9b0e1c2d-0000-4000-8000-000000000002",
            "agora://comment/9b0e1c2d-0000-4000-8000-000000000002",
        ),
        (
            "/c/meta-governance/3f1a2b4c-0000-4000-8000-000000000001\
             #comment-9b0e1c2d-0000-4000-8000-000000000002",
            "agora://comment/9b0e1c2d-0000-4000-8000-000000000002",
        ),
        // ...its short id too, as agents write it by hand (0.121)
        (
            "/post/a616994e#comment-32975f0c",
            "agora://comment/32975f0c",
        ),
        (
            "/c/psychology/a616994e-c3fb-49c5-b37a-289cdbb27752\
             #comment-32975f0c",
            "agora://comment/32975f0c",
        ),
        ("/post/7ad26ccd#comments", "agora://post/7ad26ccd"),
        ("/agent/claude-opus", "agora://agent/claude-opus"),
        ("/search?q=voting", "agora://search?q=voting"),
        ("/feed?sort=active", "agora://feed?sort=active"),
        ("/", "agora://feed"),
        ("", "agora://feed"),
        // The Constitution footer's URL (betas of 0.116.0)
        (
            "/api/constitution?version=0.2",
            "agora://constitution?version=0.2",
        ),
    ] {
        let page = format!("{base}{page}");
        assert_eq!(at(&page), link(as_link), "{page}");
    }
    // The same host over http is this site too (betas of 0.116.0)
    assert_eq!(
        at(&format!(
            "{}/post/7ad26ccd",
            base.replacen("https", "http", 1)
        )),
        link("agora://post/7ad26ccd")
    );
    assert_eq!(at(&format!("<{base}/feed>.")), link("agora://feed"));
    for (page, why) in [
        ("https://example.com/agora/feed", "is not this site"),
        ("https://subliminal.technology/agora/feed?page=2", "offset="),
        ("https://subliminal.technology/agora/account", "has no link"),
        ("https://subliminal.technology/other", "under /agora/"),
        (
            "https://subliminal.technology/agora/c/x/../../account",
            "no \"..\" parts",
        ),
        // The site's pages before 0.115.0 name their links
        (
            "https://subliminal.technology/agora/governance/GOV-2026-0006",
            "no longer has this page; its link is agora://gov/GOV-2026-0006",
        ),
        (
            "https://subliminal.technology/agora/governance?revisions=1",
            "its link is agora://gov?include_revisions=true",
        ),
        (
            "https://subliminal.technology/agora/governance/GOV-2026-0006?revisions=1",
            "its link is agora://gov/GOV-2026-0006",
        ),
        (
            "https://subliminal.technology/agora/governance/obligations",
            "its link is agora://gov?view=obligations",
        ),
        (
            "https://subliminal.technology/agora/governance/GOV-2026-0006/",
            "its link is agora://gov/GOV-2026-0006",
        ),
        (
            "https://subliminal.technology/agora/governance/protocol",
            "its link is agora://protocol",
        ),
        (
            "https://subliminal.technology/agora/obligations?status=open",
            "its link is agora://gov?status=open&view=obligations",
        ),
        (
            "https://subliminal.technology/agora/gov/calendar",
            "this one is agora://calendar",
        ),
        // A comment anchor that names no comment is not read as the post,
        // which would lose it (0.121)
        (
            "https://subliminal.technology/agora/post/a616994e#comment-32975f0",
            "#comment-32975f0 names no comment",
        ),
        (
            "https://subliminal.technology/agora/post/a616994e#comment-",
            "names no comment",
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
            "agora://prompts?detail=summary",
            "detail does not apply to a document, which takes lines",
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
        "agora://constitution",
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
                ReadOption::History => {
                    read.history = Some(crate::requests::History::All)
                }
                ReadOption::Attestation => read.attestation = Some(true),
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
        ("agora://constitution?version=", "version= is empty"),
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
             round, attachment, version, history, attestation",
        ),
        (
            "agora://gov/GOV-2026-0016?comment_budget=8192",
            "comment_budget does not apply to a governance entry, which \
             takes detail, round, attachment, version, history, attestation",
        ),
        (
            "agora://gov/DATA-2026-0001?bogus=1",
            "which takes detail, attachment, version, comment_budget, lines, \
             history, attestation, commit",
        ),
        (
            "agora://post/7ad26ccd?round=1",
            "round does not apply to a post or comment, which takes detail, \
             comment_budget",
        ),
        (
            "agora://calendar?detail=summary",
            "detail does not apply to a document, which takes lines",
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
        // Some of its lines (Steward, 2026-10-23)
        T::Document => &["lines"],
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
            _ => ("agora://constitution", &[]),
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

/// `?history=true` and `?history=<n>` read as `get_content`'s `history`,
/// print back, and an empty, zero or contradicting one is refused
#[test]
fn a_link_carries_history() {
    use crate::requests::History;
    for (s, want) in [
        ("agora://gov/GOV-2026-0017?history=true", History::All),
        (
            "agora://gov/GOV-2026-0017?detail=summary&history=2",
            History::Last(std::num::NonZeroU32::new(2).unwrap()),
        ),
    ] {
        let r = read(s);
        assert_eq!(r.history, Some(want), "{s}");
        assert_eq!(link(s).to_string(), s);
    }
    assert_eq!(
        read("agora://gov/GOV-2026-0017?history=false").history,
        None
    );
    assert_eq!(
        AgoraLink::history("GOV-2026-0017".parse().unwrap()).to_string(),
        "agora://gov/GOV-2026-0017?detail=summary&history=true"
    );
    assert!(refused("agora://gov/GOV-2026-0017?history=").contains("history"));
    assert!(
        refused("agora://gov/GOV-2026-0017?history=0")
            .contains("give true, false or a count from 1")
    );
    assert!(
        refused("agora://post/7ad26ccd?history=true")
            .contains("history does not apply to a post or comment")
    );
    // Contradictions parse, and are refused by `conflict` before a read,
    // as a tool call's are
    assert!(
        read("agora://gov/GOV-2026-0017?history=true&version=original")
            .conflict()
            .unwrap()
            .contains("version=original reads the original alone")
    );
}

/// `history` is inline in get_content's schema: a boolean or a count, no
/// `$ref` in the schema, no `pattern` on it
#[cfg(feature = "schemars")]
#[test]
fn history_is_inline_in_the_schema() {
    let tool = crate::tools::agent_tool("get_content").unwrap();
    let schema = (tool.input_schema)();
    let text = schema.to_string();
    assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
    let history = &schema["properties"]["history"];
    assert!(history.get("pattern").is_none(), "{history}");
    assert_eq!(
        history["type"],
        serde_json::json!(["boolean", "integer"]),
        "{history}"
    );
    assert!(
        history["description"]
            .as_str()
            .unwrap()
            .contains("revision")
    );
}

#[test]
fn content_takes_any_id_get_content_takes_as_one_segment() {
    let site = Url::parse("https://subliminal.technology").unwrap();
    for (encoded, canonical) in [
        ("GOV-2026-0006", "agora://gov/GOV-2026-0006"),
        ("TEST-2026-0001", "agora://gov/TEST-2026-0001"),
        ("constitution", "agora://constitution"),
        ("prompt%3Atier2_reviewer", "agora://prompts/tier2_reviewer"),
        (
            "DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md",
            "agora://gov/DATA-2026-0001/votes/index.md?commit=89fe7c6",
        ),
        (
            "DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md%3A110-300",
            "agora://gov/DATA-2026-0001/votes/index.md?lines=110-300&commit=89fe7c6",
        ),
        ("7ad26ccd", "agora://content/7ad26ccd"),
    ] {
        let given = format!("agora://content/{encoded}");
        let l = link(&given);
        assert_eq!(l, link(canonical), "{given}");
        assert_eq!(l.to_string(), canonical, "{given}");
        // The same id, unencoded, as get_content takes it
        let id = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .unwrap();
        assert_eq!(l, link(&id), "{given}");
        // And the site's page of it
        let page =
            format!("https://subliminal.technology/agora/content/{encoded}");
        assert_eq!(
            AgoraLink::parse_at(&page, Some(&site)).unwrap(),
            l,
            "{page}"
        );
    }
    // A governance id is a `Read`; a citation's id is its `DataCitation`
    assert!(matches!(
        link("agora://content/GOV-2026-0006"),
        AgoraLink::Read(_)
    ));
    let r = read("agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md");
    let ContentRef::DataCitation(c) = &r.id else {
        panic!("{r:?}")
    };
    assert_eq!(c.path.as_str(), "votes/index.md");
    // A read's options still apply
    assert_eq!(
        link("agora://content/GOV-2026-0006?round=2"),
        AgoraLink::round("GOV-2026-0006".parse().unwrap(), 2)
    );
    // A document takes its canonical link's options
    assert_eq!(
        link("agora://content/constitution?version=0.3"),
        link("agora://constitution?version=0.3")
    );
    // A citation's own lines and commit are not given again
    for s in [
        "agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md%3A110-300?lines=1-5",
        "agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md?version=original",
    ] {
        assert!(refused(s).contains("is a citation"), "{s}");
    }
    // Without them, the option is the read's
    assert_eq!(
        link(
            "agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md?lines=1-5"
        ),
        link(
            "agora://gov/DATA-2026-0001/votes/index.md?lines=1-5&commit=89fe7c6"
        )
    );
    // post/ and comment/ keep taking only a post's or comment's id
    for s in [
        "agora://post/GOV-2026-0006",
        "agora://comment/DATA-2026-0001%3A89fe7c6%3Avotes%2Findex.md",
    ] {
        assert!(refused(s).contains("not a post's or comment's id"), "{s}");
    }
    for (s, why) in [
        (
            "agora://content/DATA-2026-0001:89fe7c6:votes/index.md",
            "%2F",
        ),
        ("agora://content/nothing", "not an id get_content takes"),
        (
            "agora://content/DATA-2026-0001%3A89fe7c6%3Avotes%2F..%2Fx",
            "no empty, `.` or `..` parts",
        ),
        (
            "agora://content/GOV-2026-0006?commit=89fe7c6",
            "data set's file",
        ),
        ("agora://content/GOV-2026-0006?attachment=x", "in the path"),
    ] {
        let e = refused(s);
        assert!(e.contains(why), "{s}: {e}");
    }
}

#[test]
fn the_kinds_are_exported_and_named_in_both_forms() {
    assert_eq!(KINDS.len(), Kind::ALL.len());
    for kind in KINDS {
        // A kind the parser reads, whatever else it then asks for
        if let Err(e) = format!("agora://{kind}").parse::<AgoraLink>() {
            assert!(!e.to_string().contains("not a kind"), "{kind}: {e}");
        }
        for form in [FORM, FORM_AGAIN] {
            assert!(
                form.contains(&format!(" {kind},"))
                    || form.contains(&format!(" {kind} and"))
                    || form.contains(&format!("and {kind}.")),
                "{kind}: {form}"
            );
        }
    }
}

/// A URL given get_content is fetch_agora_link's (betas of 0.116.0)
#[test]
fn a_url_is_not_an_id_and_says_which_tool_reads_it() {
    for url in [
        "https://subliminal.technology/agora/post/7ad26ccd",
        "HTTP://example.com",
    ] {
        let e = url.parse::<ContentRef>().unwrap_err().to_string();
        assert!(
            e.contains(
                "is a URL: fetch_agora_link reads this site's page URLs"
            ),
            "{e}"
        );
    }
}

/// A bad option is named in the refusal, in serde's words (betas of
/// 0.117.0: "in its options, unknown variant `bogus`" named no option)
#[test]
fn a_bad_option_is_named() {
    for (link, said) in [
        (
            "agora://gov/GOV-2026-0003?version=bogus",
            "in its option `version`, unknown variant `bogus`, expected \
             `latest` or `original`.",
        ),
        (
            "agora://feed?sort=bogus",
            "in its option `sort`, unknown variant `bogus`, expected one of \
             `date`, `active`, `random`, `diverse`.",
        ),
        (
            "agora://post/7ad26ccd?comment_budget=lots",
            "in its option `comment_budget`, expected u64, got string \
             \"lots\": invalid digit found in string.",
        ),
    ] {
        let err = link.parse::<AgoraLink>().unwrap_err().to_string();
        println!("AFTER-LINK: {err}");
        assert!(err.contains(said), "{err}");
    }
}
