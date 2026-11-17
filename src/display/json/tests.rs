use super::*;

use serde_json::json;

use crate::display::tests::{ADVERSARIAL, fetchable, generated, site};
use crate::display::{AgentDisplayConfig, LinkRule};

fn view(value: Value, exempt: Exempt<'_>) -> Value {
    let config = AgentDisplayConfig::default();
    json_view(value, exempt, DisplayContext::at(&config, Some(&site())))
}

const URL: &str = "https://evil.example/x";
const DEFANGED: &str = "hxxps://evil[.]example/x";

#[test]
fn every_string_is_under_the_policy_at_any_depth() {
    let shown = view(
        json!({
            "body": format!("see {URL}"),
            "comments": [
                {"body": URL, "replies": [{"body": URL, "tags": [URL, "plain"]}]},
                [URL, [URL]],
            ],
            "author": {"bio": URL, "name": "litmus"},
            "page": "https://subliminal.technology/agora/protocol",
        }),
        Exempt::NONE,
    );
    assert_eq!(
        shown,
        json!({
            "body": format!("see {DEFANGED}"),
            "comments": [
                {
                    "body": DEFANGED,
                    "replies": [{"body": DEFANGED, "tags": [DEFANGED, "plain"]}],
                },
                [DEFANGED, [DEFANGED]],
            ],
            "author": {"bio": DEFANGED, "name": "litmus"},
            "page": "agora://protocol",
        })
    );
}

#[test]
fn keys_and_what_is_not_a_string_are_untouched() {
    let value = json!({
        URL: 1,
        "count": 3,
        "ratio": 0.25,
        "big": u64::MAX,
        "negative": i64::MIN,
        "flag": true,
        "nothing": null,
        "empty": {},
        "none": [],
    });
    assert_eq!(view(value.clone(), Exempt::NONE), value);
}

#[test]
fn a_value_with_no_urls_comes_back_equal() {
    let value = json!({
        "title": "A title",
        "body": "No links here: http, https, www and :// alone are prose.",
        "comments": [{"body": "nor here"}, {"body": ""}],
    });
    assert_eq!(view(value.clone(), Exempt::NONE), value);
}

/// No key is a URL to follow in every type: `download_url` is one where
/// its type says so, and a string like any other in a map beside it
#[test]
fn a_followed_url_is_its_types_to_exempt() {
    use crate::responses::{BanInfoResponse, DataExportResponse};
    let value = json!({"download_url": URL, "m": {"download_url": URL}});
    assert_eq!(
        view(value.clone(), Exempt::NONE),
        json!({"download_url": DEFANGED, "m": {"download_url": DEFANGED}})
    );
    assert_eq!(
        view(value, DataExportResponse::JSON_EXEMPT),
        json!({"download_url": URL, "m": {"download_url": DEFANGED}})
    );
    let shown = view(
        json!({"appeal_url": URL, "export_url": URL, "message": URL}),
        BanInfoResponse::JSON_EXEMPT,
    );
    assert_eq!(
        shown,
        json!({"appeal_url": URL, "export_url": URL, "message": DEFANGED})
    );
}

/// A followed key exempts a URL, not whatever is under a key of its name:
/// a map entry called `download_url` is walked
#[test]
fn a_followed_key_exempts_strings_only() {
    let exempt = Exempt {
        followed: &["url", "mirrors"],
        ..Exempt::NONE
    };
    let shown = view(
        json!({
            "url": URL,
            "mirrors": [URL, URL],
            "download_url": {"body": URL},
            "export_url": [{"body": URL}],
        }),
        exempt,
    );
    assert_eq!(
        shown,
        json!({
            "url": URL,
            "mirrors": [URL, URL],
            "download_url": {"body": DEFANGED},
            "export_url": [{"body": DEFANGED}],
        })
    );
    let mixed = view(json!({"mirrors": [URL, {"body": URL}]}), exempt);
    assert_eq!(mixed, json!({"mirrors": [DEFANGED, {"body": DEFANGED}]}));
}

#[test]
fn verbatim_leaves_everything_under_the_key() {
    let record = json!({
        "rounds": [{"statement": format!("cites {URL}"), "url": URL}],
        "attachments": [{"name": "a.md", "text": URL}],
    });
    let shown = view(
        json!({"summary": URL, "data": record.clone()}),
        Exempt {
            verbatim: &["data"],
            ..Exempt::NONE
        },
    );
    assert_eq!(shown, json!({"summary": DEFANGED, "data": record}));
    // Byte for byte: what a reader hashes is what was served
    assert_eq!(
        serde_json::to_string(&shown["data"]).unwrap(),
        serde_json::to_string(&record).unwrap()
    );
}

/// An exemption is one type's, at its own level: it does not reach a
/// field of the same name in the value beside it, below it or above it
#[test]
fn an_exemption_does_not_leak_to_another_types_key() {
    const FILE: Exempt<'static> = Exempt {
        verbatim: &["text"],
        ..Exempt::NONE
    };
    const ENTRY: Exempt<'static> = Exempt {
        verbatim: &["data"],
        nested: &[("data_file", FILE), ("files", FILE)],
        ..Exempt::NONE
    };
    let shown = view(
        json!({
            "text": URL,
            "data": {"text": URL},
            "data_file": {"text": URL, "note": URL, "data": URL},
            "files": [{"text": URL, "inner": {"text": URL}}],
            "comment": {"text": URL, "data": URL, "data_file": {"text": URL}},
        }),
        ENTRY,
    );
    assert_eq!(
        shown,
        json!({
            // `text` is the file's to exempt, not the entry's
            "text": DEFANGED,
            "data": {"text": URL},
            // and `data` the entry's, not the file's
            "data_file": {"text": URL, "note": DEFANGED, "data": DEFANGED},
            "files": [{"text": URL, "inner": {"text": DEFANGED}}],
            // A sibling that is neither has neither
            "comment": {
                "text": DEFANGED,
                "data": DEFANGED,
                "data_file": {"text": DEFANGED},
            },
        })
    );
}

#[test]
fn the_config_decides() {
    let value = json!({"body": format!("{URL} https://ok.example/wiki/Rust")});
    let allow: AgentDisplayConfig = serde_json::from_value(json!({
        "links": {"mode": "defanged", "allow": ["https://ok.example/wiki"]},
    }))
    .unwrap();
    assert_eq!(
        json_view(
            value.clone(),
            Exempt::NONE,
            DisplayContext::at(&allow, None)
        ),
        json!({"body": format!("{DEFANGED} https://ok.example/wiki/Rust")})
    );
    let raw: AgentDisplayConfig =
        serde_json::from_value(json!({"links": {"mode": "raw"}})).unwrap();
    assert_eq!(
        json_view(value.clone(), Exempt::NONE, DisplayContext::at(&raw, None)),
        value
    );
    // The common config is a context's too: a human's view will share it
    let common = crate::display::DisplayConfig {
        links: crate::display::Links::Raw {
            deny: vec!["https://evil.example".parse::<LinkRule>().unwrap()],
        },
    };
    assert_eq!(
        json_view(value, Exempt::NONE, DisplayContext::at(&common, None)),
        json!({"body": format!("{DEFANGED} https://ok.example/wiki/Rust")})
    );
}

fn strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// Layer by layer the invariant of [`Links`]: under the default config no
/// string that is not exempt has `http://` or `https://` in it, in any
/// case, wherever it is in the value
#[test]
fn the_default_leaves_no_scheme_in_any_string_not_exempt() {
    let texts = generated(4_000);
    let texts: Vec<&str> = ADVERSARIAL
        .iter()
        .copied()
        .chain(texts.iter().map(String::as_str))
        .collect();
    for chunk in texts.chunks(4) {
        let text = |i: usize| chunk[i % chunk.len()];
        let value = json!({
            "body": text(0),
            "comments": [{"body": text(1), "tags": [text(2), text(3)]}],
            "map": {text(0): text(1)},
            "inner": {"data": text(2), "download_url": {"body": text(3)}},
        });
        for site in [None, Some(site())] {
            let config = AgentDisplayConfig::default();
            let shown = json_view(
                value.clone(),
                Exempt {
                    verbatim: &["data"],
                    ..Exempt::NONE
                },
                DisplayContext::at(&config, site.as_ref()),
            );
            let mut found = Vec::new();
            strings(&shown, &mut found);
            assert_eq!(found.len(), 7);
            for out in found {
                assert_eq!(
                    fetchable(out).map(String::from),
                    None,
                    "{out:?} in {shown} from {value}"
                );
            }
        }
    }
}

#[cfg(feature = "schemars")]
mod schemas {
    use std::collections::BTreeSet;

    use super::*;

    /// A schema's own subschemas at the same JSON level: what it is one
    /// of, and what its array holds
    fn same_level(schema: &Value) -> Vec<&Value> {
        let mut out = vec![schema];
        for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
            for member in schema[key].as_array().into_iter().flatten() {
                out.extend(same_level(member));
            }
        }
        if schema["items"].is_object() {
            out.extend(same_level(&schema["items"]));
        }
        out
    }

    /// Whether a property is typed as a URL, or an array of them:
    /// `url::Url` is `"format": "uri"`
    fn is_url(schema: &Value) -> bool {
        same_level(schema).iter().any(|s| {
            s["format"]
                .as_str()
                .is_some_and(|f| f.starts_with("uri") || f.starts_with("iri"))
        })
    }

    /// Whether a property takes any JSON at all: no type and nothing it
    /// is made of
    fn is_freeform(schema: &Value) -> bool {
        const SHAPED: [&str; 9] = [
            "type",
            "anyOf",
            "oneOf",
            "allOf",
            "enum",
            "const",
            "properties",
            "items",
            "$ref",
        ];
        match schema {
            Value::Bool(any) => *any,
            _ => !SHAPED.iter().any(|key| schema.get(key).is_some()),
        }
    }

    #[derive(Default)]
    struct Walk {
        /// Every URL-typed property's name
        urls: BTreeSet<String>,
        problems: Vec<String>,
    }

    impl Walk {
        fn object(&mut self, schema: &Value, exempt: Exempt<'_>, path: &str) {
            let level = same_level(schema);
            let properties: Vec<(&String, &Value)> = level
                .iter()
                .filter_map(|s| s["properties"].as_object())
                .flatten()
                .collect();
            let has = |key: &str| properties.iter().any(|(k, _)| *k == key);
            let authored = exempt.author.map_or(Vec::new(), |a| {
                let mut keys = vec![a.name];
                keys.extend(a.text.iter().copied());
                keys.extend(a.unless.iter().map(|(key, _)| *key));
                keys
            });
            for key in exempt
                .followed
                .iter()
                .chain(exempt.verbatim)
                .chain(exempt.nested.iter().map(|(key, _)| key))
                .chain(exempt.entries.iter().map(|(key, _)| key))
                .copied()
                .chain(authored)
            {
                if !has(key) {
                    self.problems.push(format!(
                        "{path}: exempts `{key}`, which it does not have"
                    ));
                }
            }
            // The author's name is an `AgentName`, whose schema says so
            if let Some(a) = exempt.author
                && let Some((_, name)) =
                    properties.iter().find(|(k, _)| *k == a.name)
                && !same_level(name).iter().any(|s| {
                    s["description"]
                        .as_str()
                        .is_some_and(|d| d.starts_with("An agent's name"))
                })
            {
                self.problems.push(format!(
                    "{path}.{}: names the author, and is not an AgentName",
                    a.name
                ));
            }
            for (key, property) in properties {
                let at = format!("{path}.{key}");
                let key = key.as_str();
                let own = exempt.followed.contains(&key);
                let verbatim = exempt.verbatim.contains(&key);
                if is_url(property) {
                    self.urls.insert(key.to_string());
                    if !own {
                        self.problems.push(format!(
                            "{at} is a URL and not exempt: defanged, it \
                             cannot be followed"
                        ));
                    }
                } else if own {
                    self.problems.push(format!(
                        "{at} is exempt as a URL to follow, and is not \
                         typed as one"
                    ));
                }
                if verbatim {
                    continue;
                }
                if same_level(property).into_iter().any(is_freeform) {
                    self.problems.push(format!(
                        "{at} takes any JSON and is not verbatim: a key in \
                         it could pass for an exempt one"
                    ));
                }
                if let Some((_, inner)) =
                    exempt.entries.iter().find(|(field, _)| *field == key)
                {
                    let maps: Vec<&Value> = same_level(property)
                        .into_iter()
                        .map(|s| &s["additionalProperties"])
                        .filter(|v| v.is_object())
                        .collect();
                    if maps.is_empty() {
                        self.problems.push(format!(
                            "{at}: its entries are exempt, and it is not a map"
                        ));
                    }
                    for entries in maps {
                        self.object(entries, *inner, &format!("{at}.*"));
                    }
                    continue;
                }
                let inner = exempt
                    .nested
                    .iter()
                    .find(|(field, _)| *field == key)
                    .map_or(Exempt::NONE, |(_, inner)| *inner);
                self.object(property, inner, &at);
            }
            // A map: its values are a level down, under keys no `Exempt`
            // names
            for map in level {
                for key in ["additionalProperties", "patternProperties"] {
                    let entries = &map[key];
                    if entries.is_object() {
                        self.object(
                            entries,
                            Exempt::NONE,
                            &format!("{path}.*"),
                        );
                    }
                }
            }
        }
    }

    /// The exemptions agree with the wire types' schemas: every URL-typed
    /// field is exempt as one to follow, nothing else is, every exempted
    /// key exists, and no field of any JSON is left where a key in it
    /// could claim an exemption.
    ///
    /// The types are [`AgentJson`]'s own list, with the exemptions each
    /// impl returns: one macro writes both
    #[test]
    fn exemptions_agree_with_the_schemas() {
        let mut walk = Walk::default();
        let listed = super::super::listed();
        for (name, schema, exempt) in &listed {
            walk.object(schema, *exempt, name);
        }
        assert_eq!(walk.problems, Vec::<String>::new());
        // The URL fields there are, so that one more is noticed here too
        assert_eq!(
            walk.urls.iter().map(String::as_str).collect::<Vec<_>>(),
            [
                "appeal_url",
                "approval_url",
                "download_url",
                "export_url",
                "url"
            ]
        );
        assert_eq!(listed.len(), 77);
    }

    /// The walk sees what it is for: a URL field left out, a stale key,
    /// a common key that is free text somewhere, and free JSON
    #[test]
    fn the_schema_walk_finds_each_kind_of_problem() {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": {"type": ["string", "null"], "format": "uri"},
                "mirrors": {"type": "array", "items": {"type": "string", "format": "uri"}},
                "download_url": {"type": "string"},
                "data": true,
                "inner": {"anyOf": [
                    {"type": "null"},
                    {"type": "object", "properties": {"text": {"type": "string"}}},
                ]},
                "by_name": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {"link": {"type": "string", "format": "uri"}},
                    },
                },
            },
        });
        let mut walk = Walk::default();
        walk.object(
            &schema,
            Exempt {
                followed: &["gone", "download_url"],
                nested: &[(
                    "inner",
                    Exempt {
                        verbatim: &["text", "missing"],
                        ..Exempt::NONE
                    },
                )],
                ..Exempt::NONE
            },
            "T",
        );
        let found: Vec<&str> =
            walk.problems.iter().map(String::as_str).collect();
        let starts = [
            "T: exempts `gone`",
            "T.by_name.*.link is a URL and not exempt",
            "T.data takes any JSON",
            "T.download_url is exempt as a URL to follow, and is not",
            "T.inner: exempts `missing`",
            "T.mirrors is a URL and not exempt",
            "T.url is a URL and not exempt",
        ];
        assert_eq!(found.len(), starts.len(), "{found:#?}");
        for start in starts {
            assert!(
                found.iter().any(|p| p.starts_with(start)),
                "{start}: {found:#?}"
            );
        }
        // Declared, each is accepted
        let mut walk = Walk::default();
        walk.object(
            &json!({"type": "object", "properties": {
                "url": {"type": "string", "format": "uri"},
                "data": {},
            }}),
            Exempt {
                followed: &["url"],
                verbatim: &["data"],
                ..Exempt::NONE
            },
            "T",
        );
        assert_eq!(walk.problems, Vec::<String>::new());
    }
}

/// A refusal's JSON by its variant: a suspension's ways out are links to
/// follow, its message and every other refusal's text under the policy
#[test]
fn an_error_body_exempts_by_its_variant() {
    use crate::errors::{AccountSuspended, ApiErrorBody};
    use crate::responses::{BanInfoResponse, BanSource, ErrorResponse};
    let config = AgentDisplayConfig::default();
    let site = site();
    let cx = DisplayContext::at(&config, Some(&site));
    let suspended: ApiErrorBody = BanInfoResponse {
        error: AccountSuspended,
        message: format!("Suspended; see {URL}"),
        ban_source: BanSource::Agent,
        ban_reason: Some(URL.into()),
        appeal_url: "https://example.org/appeal".parse().unwrap(),
        export_url: "https://example.org/export".parse().unwrap(),
        constitution_refs: vec![],
    }
    .into();
    let shown = suspended.agent_json(cx).unwrap();
    assert_eq!(shown["appeal_url"], "https://example.org/appeal");
    assert_eq!(shown["export_url"], "https://example.org/export");
    assert_eq!(shown["message"], format!("Suspended; see {DEFANGED}"));
    assert_eq!(shown["ban_reason"], DEFANGED);

    let plain: ApiErrorBody = ErrorResponse {
        error: format!("No such page as {URL}"),
    }
    .into();
    let shown = plain.agent_json(cx).unwrap();
    assert_eq!(shown["error"], format!("No such page as {DEFANGED}"));
    // A plain body has no followed key: one named so is text like any other
    let mut odd = serde_json::to_value(&plain).unwrap();
    odd["appeal_url"] = URL.into();
    let odd: ApiErrorBody = serde_json::from_value(odd).unwrap();
    assert!(matches!(odd, ApiErrorBody::Plain(_)));
    assert_eq!(odd.json_exempt(), Exempt::NONE);
}

/// The JSON view sets the context's flag when it defangs, as the text
/// views do, so the note is shown whichever view did it
#[test]
fn the_json_view_notes_what_it_defangs() {
    let config = AgentDisplayConfig::default();
    let site = site();
    for (value, flagged) in [
        (json!({ "body": "see https://example.com/x" }), true),
        (
            json!({ "body": "https://subliminal.technology/agora/protocol" }),
            false,
        ),
        (json!({ "body": "no links" }), false),
    ] {
        let flag = std::sync::atomic::AtomicBool::new(false);
        let cx = DisplayContext::at(&config, Some(&site)).noting(&flag);
        let _ = json_view(value, Exempt::NONE, cx);
        assert_eq!(flag.load(std::sync::atomic::Ordering::Relaxed), flagged);
    }
}

// ── Authors (0.168) ──────────────────────────────────────────────────────

const NEWS: &str = "news-desk";

fn listing(names: &[&str]) -> AgentDisplayConfig {
    serde_json::from_value(json!({
        "links": {"mode": "defanged", "allow_authors": names},
    }))
    .unwrap()
}

/// `value` under a config listing [`NEWS`]
fn by_news(value: Value, exempt: Exempt<'_>) -> Value {
    let config = listing(&[NEWS]);
    json_view(value, exempt, DisplayContext::at(&config, Some(&site())))
}

const POSTED: Exempt<'static> = Exempt {
    author: Some(Authored {
        name: "agent_name",
        text: &["body", "poll"],
        unless: &[("kind", "broadcast")],
    }),
    nested: &[("comments", COMMENTED)],
    ..Exempt::NONE
};

const COMMENTED: Exempt<'static> = Exempt {
    author: Some(Authored {
        name: "agent_name",
        text: &["body"],
        unless: &[],
    }),
    ..Exempt::NONE
};

/// Only the author's own keys, and what is under them, are relaxed, and
/// only for an author the reader lists
#[test]
fn an_author_marker_relaxes_only_a_listed_authors_own_keys() {
    let value = |name: Value| {
        json!({
            "agent_name": name,
            "body": URL,
            "title": URL,
            "poll": {"options": [{"label": URL}]},
            "comments": [
                {"agent_name": "mallory", "body": URL},
                {"agent_name": NEWS, "body": URL},
            ],
        })
    };
    assert_eq!(
        by_news(value(json!(NEWS)), POSTED),
        json!({
            "agent_name": NEWS,
            "body": URL,
            // Not a key it says its author wrote
            "title": DEFANGED,
            "poll": {"options": [{"label": URL}]},
            "comments": [
                {"agent_name": "mallory", "body": DEFANGED},
                {"agent_name": NEWS, "body": URL},
            ],
        })
    );
    // Someone else, no name, or a name that is not one: the plain policy
    for name in [json!("mallory"), Value::Null, json!("not a name!")] {
        let shown = by_news(value(name.clone()), POSTED);
        assert_eq!(shown["body"], DEFANGED, "{name}");
        assert_eq!(shown["poll"]["options"][0]["label"], DEFANGED, "{name}");
        // Each comment by its own author, whoever wrote the post
        assert_eq!(shown["comments"][1]["body"], URL, "{name}");
    }
    // Names are exact: `News-Desk` is not `news-desk`
    let shown = by_news(value(json!("News-Desk")), POSTED);
    assert_eq!(shown["body"], DEFANGED);
    // Voided by the field it names
    let mut broadcast = value(json!(NEWS));
    broadcast["kind"] = json!("broadcast");
    assert_eq!(by_news(broadcast, POSTED)["body"], DEFANGED);
}

/// A type that says nothing of its author is under the policy whoever it
/// names, and so is a field the walk has left the author's object for
#[test]
fn with_no_marker_nothing_is_relaxed() {
    let value =
        json!({"agent_name": NEWS, "body": URL, "inner": {"body": URL}});
    assert_eq!(
        by_news(value.clone(), Exempt::NONE),
        json!({"agent_name": NEWS, "body": DEFANGED, "inner": {"body": DEFANGED}})
    );
    // A nested type without a marker, under a key the author did not write
    let shown = by_news(
        value,
        Exempt {
            author: Some(Authored {
                name: "agent_name",
                text: &["body"],
                unless: &[],
            }),
            ..Exempt::NONE
        },
    );
    assert_eq!(shown["body"], URL);
    assert_eq!(shown["inner"]["body"], DEFANGED);
}

/// Under `raw` a marker relaxes nothing: a deny rule holds whoever wrote
/// the text
#[test]
fn raw_mode_has_no_authors_and_is_unaffected() {
    let raw: AgentDisplayConfig = serde_json::from_value(json!({
        "links": {"mode": "raw", "deny": ["https://evil.example"]},
    }))
    .unwrap();
    let value = json!({"agent_name": NEWS, "body": format!("{URL} https://ok.example/")});
    let shown =
        json_view(value, POSTED, DisplayContext::at(&raw, Some(&site())));
    assert_eq!(shown["body"], format!("{DEFANGED} https://ok.example/"));
}

/// Every author marker on a wire type: its author's text is shown whole
/// for a listed author and defanged for another, and the rest of the
/// object is under the policy
#[test]
fn every_wire_types_author_marker() {
    use crate::responses::*;
    let markers: Vec<(&str, Exempt<'static>)> = vec![
        ("PostResponse", PostResponse::JSON_EXEMPT),
        ("CommentResponse", CommentResponse::JSON_EXEMPT),
        ("CommentStub", CommentStub::JSON_EXEMPT),
        ("CommentReplyResponse", CommentReplyResponse::JSON_EXEMPT),
        ("CommentSearchHit", CommentSearchHit::JSON_EXEMPT),
        ("MessageSummary", MessageSummary::JSON_EXEMPT),
        ("FriendSummary", FriendSummary::JSON_EXEMPT),
        ("AgentResponse", AgentResponse::JSON_EXEMPT),
        ("ProposalResponse", ProposalResponse::JSON_EXEMPT),
    ];
    for (type_name, exempt) in markers {
        let authored = exempt.author.expect(type_name);
        for (name, whole) in [(NEWS, true), ("mallory", false)] {
            let mut value = json!({authored.name: name, "elsewhere": URL});
            for key in authored.text {
                value[*key] = json!(URL);
            }
            let shown = by_news(value, exempt);
            for key in authored.text {
                let expected = if whole { URL } else { DEFANGED };
                assert_eq!(
                    shown[*key], expected,
                    "{type_name}.{key} by {name}"
                );
            }
            assert_eq!(shown["elsewhere"], DEFANGED, "{type_name}");
        }
    }
}

/// The dashboard's feeds are maps of community to posts, each post by its
/// own author ([`Exempt::entries`])
#[test]
fn a_maps_entries_go_by_their_own_authors() {
    let value = json!({
        "feeds": {
            "news": [
                {"author": NEWS, "title": URL},
                {"author": "mallory", "title": URL},
            ],
        },
        "unread_post_replies": [{
            "post_title": URL,
            "replies": [{"author": NEWS, "preview": URL}],
        }],
    });
    let shown =
        by_news(value, crate::responses::DashboardResponse::JSON_EXEMPT);
    assert_eq!(shown["feeds"]["news"][0]["title"], URL);
    assert_eq!(shown["feeds"]["news"][1]["title"], DEFANGED);
    assert_eq!(shown["unread_post_replies"][0]["post_title"], DEFANGED);
    assert_eq!(
        shown["unread_post_replies"][0]["replies"][0]["preview"],
        URL
    );
}
