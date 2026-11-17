use super::*;

use serde_json::{Value, json};

use crate::display::LinkRule;

fn name(s: &str) -> ProfileName {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

fn rule(s: &str) -> LinkRule {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

fn config(links: Links) -> AgentDisplayConfig {
    DisplayConfig { links }.into()
}

fn allowing(rules: &[&str]) -> AgentDisplayConfig {
    config(Links::Defanged {
        allow: rules.iter().map(|r| rule(r)).collect(),
        allow_authors: Vec::new(),
    })
}

fn denying(rules: &[&str]) -> AgentDisplayConfig {
    config(Links::Raw {
        deny: rules.iter().map(|r| rule(r)).collect(),
    })
}

fn profile(s: &str) -> DisplaySelection {
    DisplaySelection::Profile(name(s))
}

/// The `[display]` table of the type's docs, as the JSON a TOML parser
/// hands serde (this crate has no TOML parser to read one with)
fn example() -> Value {
    json!({
        "links": { "mode": "defanged" },
        "profiles": {
            "news": {
                "links": {
                    "mode": "defanged",
                    "allow": ["https://*.wikipedia.org", "https://www.bbc.com"],
                },
            },
            "open": {
                "links": {
                    "mode": "raw",
                    "deny": ["https://example.org/tracker"],
                },
            },
        },
    })
}

fn section() -> AgentDisplayProfiles {
    serde_json::from_value(example()).unwrap_or_else(|e| panic!("{e}"))
}

fn refused<T: serde::de::DeserializeOwned + fmt::Debug>(
    value: Value,
) -> String {
    // From text too, which takes serde's other path
    serde_json::from_str::<T>(&value.to_string()).expect_err("refused");
    serde_json::from_value::<T>(value)
        .expect_err("refused")
        .to_string()
}

// ── Reading and writing ─────────────────────────────────────────────────

#[test]
fn the_example_reads() {
    let read = section();
    assert_eq!(read.default, AgentDisplayConfig::default());
    assert_eq!(
        read.profiles,
        BTreeMap::from([
            (
                name("news"),
                allowing(&["https://*.wikipedia.org", "https://www.bbc.com"])
            ),
            (name("open"), denying(&["https://example.org/tracker"])),
        ])
    );
    // And from text
    let from_text: AgentDisplayProfiles =
        serde_json::from_str(&example().to_string()).unwrap();
    assert_eq!(from_text, read);
}

#[test]
fn nothing_written_is_the_default_and_no_profiles() {
    let read: AgentDisplayProfiles = serde_json::from_value(json!({})).unwrap();
    assert_eq!(read, AgentDisplayProfiles::default());
    assert_eq!(read.default, AgentDisplayConfig::default());
    assert!(read.profiles.is_empty());
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        json!({ "links": { "mode": "defanged" } })
    );
    // The default's keys alone, and the profiles alone
    let read: AgentDisplayProfiles =
        serde_json::from_value(json!({ "links": { "mode": "raw" } })).unwrap();
    assert_eq!(read.default, denying(&[]));
    let read: AgentDisplayProfiles = serde_json::from_value(
        json!({ "profiles": { "open": { "links": { "mode": "raw" } } } }),
    )
    .unwrap();
    assert_eq!(read.default, AgentDisplayConfig::default());
    assert_eq!(read.profiles[&name("open")], denying(&[]));
    // A profile with nothing written is the type's default, not the
    // table's: nothing is inherited
    let read: AgentDisplayProfiles = serde_json::from_value(json!({
        "links": { "mode": "raw" },
        "profiles": { "empty": {} },
    }))
    .unwrap();
    assert_eq!(read.profiles[&name("empty")], AgentDisplayConfig::default());
}

#[test]
fn the_section_round_trips() {
    let read = section();
    let written = serde_json::to_value(&read).unwrap();
    assert_eq!(written, example());
    assert_eq!(
        serde_json::from_value::<AgentDisplayProfiles>(written).unwrap(),
        read
    );
    let text = serde_json::to_string(&read).unwrap();
    assert_eq!(
        serde_json::from_str::<AgentDisplayProfiles>(&text).unwrap(),
        read
    );
}

/// A typo is an error at every level, never a policy left at its default
#[test]
fn unknown_keys_are_refused_at_every_level() {
    let err = refused::<AgentDisplayProfiles>;
    // In `[display]`
    let said = err(json!({ "link": { "mode": "raw" } }));
    assert!(said.contains("unknown field `link`"), "{said}");
    let said = err(json!({ "links": { "mode": "raw" }, "extra": 1 }));
    assert!(said.contains("unknown field `extra`"), "{said}");
    // `profiles`, misspelt: `[display.profile.news]`
    let mut misspelt = example();
    let table = misspelt.as_object_mut().unwrap();
    let profiles = table.remove("profiles").unwrap();
    table.insert("profile".into(), profiles);
    let said = err(misspelt);
    assert!(said.contains("unknown field `profile`"), "{said}");
    // In a profile, which the error names
    let said = err(json!({
        "profiles": { "news": { "link": { "mode": "raw" } } },
    }));
    assert!(said.contains("profile `news`"), "{said}");
    assert!(said.contains("unknown field `link`"), "{said}");
    let said = err(json!({
        "profiles": { "news": { "links": { "mode": "defanged", "deny": [] } } },
    }));
    assert!(said.contains("profile `news`"), "{said}");
    assert!(said.contains("unknown field `deny`"), "{said}");
    // A rule that is refused says why, and where
    let said = err(json!({
        "profiles": {
            "news": { "links": { "mode": "raw", "deny": ["ftp://x.example"] } },
        },
    }));
    assert!(said.contains("profile `news`"), "{said}");
    assert!(said.contains("only http and https"), "{said}");
    // A profile nested once too often, a profile that is not a table
    let said = err(json!({
        "profiles": { "profiles": { "news": {} } },
    }));
    assert_eq!(
        said,
        "\"profiles\" is not a profile name: it is the table the profiles \
         are in, and a profile is one level inside it"
    );
    let said = err(json!({ "profiles": { "news": "open" } }));
    assert!(said.contains("profile `news`"), "{said}");
    let said = err(json!({ "profiles": ["news"] }));
    assert!(said.contains("expected a map"), "{said}");
}

// ── Names ───────────────────────────────────────────────────────────────

#[test]
fn profile_names() {
    for good in ["news", "a", "open-web_2", "0", &"x".repeat(64)] {
        assert_eq!(name(good).as_str(), good);
        assert_eq!(name(good).to_string(), good);
        assert_eq!(serde_json::to_value(name(good)).unwrap(), json!(good));
        assert_eq!(
            serde_json::from_value::<ProfileName>(json!(good)).unwrap(),
            name(good)
        );
    }
    let err = |s: &str| s.parse::<ProfileName>().expect_err(s);
    assert!(matches!(err(""), ProfileNameError::Length { .. }));
    assert!(matches!(
        err(&"x".repeat(65)),
        ProfileNameError::Length { .. }
    ));
    for (bad, character) in [
        ("News", 'N'),
        ("the news", ' '),
        ("news.eu", '.'),
        ("café", 'é'),
    ] {
        assert_eq!(
            err(bad),
            ProfileNameError::Character {
                given: bad.into(),
                character
            }
        );
    }
    assert_eq!(
        err("News").to_string(),
        "\"News\" is not a profile name: it has 'N', and a name is \
         lower-case ASCII letters, digits, `-` and `_` only"
    );
    assert_eq!(
        err("").to_string(),
        "\"\" is not a profile name: it is 0 bytes, and a name is 1 to 64"
    );
}

/// `default` means the default itself in a selection, and `profiles` is
/// the table's key: neither can be defined as a profile
#[test]
fn default_and_profiles_are_not_profile_names() {
    for reserved in ProfileName::RESERVED {
        assert_eq!(
            reserved.parse::<ProfileName>(),
            Err(ProfileNameError::Reserved {
                given: reserved.into()
            })
        );
        let said = refused::<AgentDisplayProfiles>(json!({
            "profiles": { reserved: { "links": { "mode": "raw" } } },
        }));
        assert!(
            said.contains(&format!("{reserved:?} is not a profile name")),
            "{said}"
        );
    }
    assert_eq!(
        "default".parse::<ProfileName>().unwrap_err().to_string(),
        "\"default\" is not a profile name: it means the default itself, \
         which is the keys of the table the profiles are in"
    );
    // Words that only contain them are names
    name("default-news");
    name("profiles2");
}

// ── Selections ──────────────────────────────────────────────────────────

#[test]
fn a_selection_is_a_name_or_a_config() {
    for (selection, wire) in [
        (profile("news"), json!("news")),
        (DisplaySelection::Default, json!("default")),
        (
            DisplaySelection::Inline(denying(&[])),
            json!({ "links": { "mode": "raw" } }),
        ),
        (
            DisplaySelection::Inline(allowing(&["https://www.bbc.com"])),
            json!({
                "links": { "mode": "defanged", "allow": ["https://www.bbc.com"] },
            }),
        ),
    ] {
        assert_eq!(serde_json::to_value(&selection).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<DisplaySelection>(wire.clone()).unwrap(),
            selection
        );
        assert_eq!(
            serde_json::from_str::<DisplaySelection>(&wire.to_string())
                .unwrap(),
            selection
        );
    }
    // An empty table is the type's default, written in place
    assert_eq!(
        serde_json::from_value::<DisplaySelection>(json!({})).unwrap(),
        DisplaySelection::Inline(AgentDisplayConfig::default())
    );
    assert_eq!("news".parse(), Ok(profile("news")));
    assert_eq!("default".parse(), Ok(DisplaySelection::Default));
    assert_eq!(DisplaySelection::from(name("news")), profile("news"));
}

/// What a bad selection reports: the value and what is wrong with it, not
/// `untagged`'s "data did not match any variant"
#[test]
fn a_bad_selection_says_what_is_wrong() {
    let err = refused::<DisplaySelection>;
    assert_eq!(
        err(json!(7)),
        "invalid type: integer `7`, expected a display profile's name \
         (\"news\"), \"default\", or a display config written in place \
         ({ links = { mode = \"raw\" } })"
    );
    let said = err(json!(["news"]));
    assert!(
        said.starts_with("invalid type: sequence, expected"),
        "{said}"
    );
    let said = err(json!(true));
    assert!(said.starts_with("invalid type: boolean `true`"), "{said}");
    assert_eq!(
        err(json!("News")),
        "\"News\" is not a profile name: it has 'N', and a name is \
         lower-case ASCII letters, digits, `-` and `_` only"
    );
    assert!(err(json!("profiles")).contains("is not a profile name"));
    // In a config written in place, the config's own errors
    let said = err(json!({ "link": { "mode": "raw" } }));
    assert!(said.contains("unknown field `link`"), "{said}");
    let said = err(json!({ "links": { "mode": "raw" }, "profiles": {} }));
    assert!(said.contains("unknown field `profiles`"), "{said}");
    let said = err(json!({ "links": { "mode": "off" } }));
    assert!(said.contains("unknown variant `off`"), "{said}");
    let said = err(json!({ "links": { "mode": "raw", "deny": ["x"] } }));
    assert!(said.contains("is not a link rule"), "{said}");
}

// ── Resolution ──────────────────────────────────────────────────────────

/// The agent's choice, else its model's, else the default
#[test]
fn resolution_takes_the_most_specific_choice() {
    let section = section();
    let news = profile("news");
    let open = profile("open");
    let inline = DisplaySelection::Inline(allowing(&["https://a.example"]));
    let default = DisplaySelection::Default;

    // Nothing chosen
    let got = section.resolve(None, None).unwrap();
    assert_eq!(got.config, &section.default);
    assert_eq!((got.chosen, got.passed_over), (None, None));

    // The model's
    let got = section.resolve(Some(&news), None).unwrap();
    assert_eq!(got.config, &section.profiles[&name("news")]);
    assert_eq!(got.chosen, Some((SelectedBy::Model, &news)));
    assert_eq!(got.passed_over, None);

    // The agent's, with and without a model's
    let got = section.resolve(None, Some(&open)).unwrap();
    assert_eq!(got.config, &section.profiles[&name("open")]);
    assert_eq!(got.chosen, Some((SelectedBy::Agent, &open)));
    assert_eq!(got.passed_over, None);
    let got = section.resolve(Some(&news), Some(&open)).unwrap();
    assert_eq!(got.config, &section.profiles[&name("open")]);
    assert_eq!(got.chosen, Some((SelectedBy::Agent, &open)));
    assert_eq!(got.passed_over, Some(&news));

    // A config written in place, at either level
    let DisplaySelection::Inline(written) = &inline else {
        unreachable!()
    };
    let got = section.resolve(Some(&inline), None).unwrap();
    assert_eq!(got.config, written);
    assert_eq!(got.chosen, Some((SelectedBy::Model, &inline)));
    let got = section.resolve(Some(&news), Some(&inline)).unwrap();
    assert_eq!(got.config, written);
    assert_eq!(got.chosen, Some((SelectedBy::Agent, &inline)));
    let got = section.resolve(Some(&inline), Some(&news)).unwrap();
    assert_eq!(got.config, &section.profiles[&name("news")]);

    // The default by name, over the model's profile
    let got = section.resolve(Some(&open), Some(&default)).unwrap();
    assert_eq!(got.config, &section.default);
    assert_eq!(got.chosen, Some((SelectedBy::Agent, &default)));
    assert_eq!(got.passed_over, Some(&open));
}

#[test]
fn an_unknown_profile_is_named_with_those_there_are() {
    let section = section();
    let nws = profile("nws");
    let unknown = UnknownProfile {
        name: name("nws"),
        known: vec![name("news"), name("open")],
    };
    assert_eq!(section.check(&nws), Err(unknown.clone()));
    assert_eq!(section.resolve(Some(&nws), None), Err(unknown.clone()));
    assert_eq!(section.resolve(None, Some(&nws)), Err(unknown.clone()));
    assert_eq!(
        unknown.to_string(),
        "there is no display profile `nws`: the profiles are `news`, `open`"
    );
    assert_eq!(
        AgentDisplayProfiles::default()
            .check(&nws)
            .unwrap_err()
            .to_string(),
        "there is no display profile `nws`: no profile is defined"
    );

    // Every other selection resolves
    for good in [
        profile("news"),
        profile("open"),
        DisplaySelection::Default,
        DisplaySelection::Inline(denying(&[])),
    ] {
        assert_eq!(section.check(&good), Ok(()));
    }

    // `resolve` answers for the selection that applies; `check` is what
    // finds a misspelt one that an agent's own choice hides
    let open = profile("open");
    assert!(section.resolve(Some(&nws), Some(&open)).is_ok());
    assert!(section.check(&nws).is_err());
}

/// The default allows A, the profile allows B: an agent on the profile has
/// B only
#[test]
fn a_profile_replaces_the_default_and_merges_nothing() {
    let section: AgentDisplayProfiles = serde_json::from_value(json!({
        "links": { "mode": "defanged", "allow": ["https://a.example"] },
        "profiles": {
            "b": {
                "links": { "mode": "defanged", "allow": ["https://b.example"] },
            },
            "bare": {},
        },
    }))
    .unwrap();
    let text = "https://a.example/1 and https://b.example/2";
    let shown = |model: Option<&DisplaySelection>,
                 agent: Option<&DisplaySelection>| {
        let resolved = section.resolve(model, agent).unwrap();
        resolved.config.links().rewrite(text, None).into_owned()
    };
    let (b, bare) = (profile("b"), profile("bare"));

    assert_eq!(
        shown(None, None),
        "https://a.example/1 and hxxps://b[.]example/2"
    );
    for (model, agent) in [(Some(&b), None), (None, Some(&b))] {
        assert_eq!(
            shown(model, agent),
            "hxxps://a[.]example/1 and https://b.example/2"
        );
    }
    // Nor is the model's profile merged into the agent's
    let a = DisplaySelection::Inline(allowing(&["https://a.example"]));
    assert_eq!(
        shown(Some(&b), Some(&a)),
        "https://a.example/1 and hxxps://b[.]example/2"
    );
    // A profile with nothing written allows nothing
    assert_eq!(
        shown(None, Some(&bare)),
        "hxxps://a[.]example/1 and hxxps://b[.]example/2"
    );
    assert_eq!(
        section.resolve(None, Some(&b)).unwrap().to_string(),
        "display: profile `b`, chosen for the agent\n  \
         in place of the default, whole: nothing of the default is added to it\n\
         links: defanged, but for 1 allowed\n  \
         allow https://b.example\n"
    );
}

// ── What an operator is told ────────────────────────────────────────────

#[test]
fn the_effective_config_says_where_it_came_from() {
    let section = section();
    let (news, open) = (profile("news"), profile("open"));
    let inline = DisplaySelection::Inline(denying(&[]));
    let default = DisplaySelection::Default;
    let told =
        |model, agent| section.resolve(model, agent).unwrap().to_string();

    assert_eq!(
        told(None, None),
        "display: the default (nothing is chosen for the agent or its model)\n\
         links: defanged, every off-site URL\n"
    );
    assert_eq!(
        told(Some(&news), None),
        "display: profile `news`, chosen by the model\n  \
         in place of the default, whole: nothing of the default is added to it\n\
         links: defanged, but for 2 allowed\n  \
         allow https://*.wikipedia.org\n  \
         allow https://www.bbc.com\n"
    );
    assert_eq!(
        told(Some(&news), Some(&open)),
        "display: profile `open`, chosen for the agent\n  \
         in place of the default, whole: nothing of the default is added to it\n  \
         not used: the model's choice, profile `news`\n\
         links: raw (left as written), but for 1 denied\n  \
         deny https://example.org/tracker\n"
    );
    assert_eq!(
        told(None, Some(&inline)),
        "display: a config written in place, chosen for the agent\n  \
         in place of the default, whole: nothing of the default is added to it\n\
         links: raw, every URL left as written\n"
    );
    assert_eq!(
        told(Some(&inline), None),
        "display: a config written in place, chosen by the model\n  \
         in place of the default, whole: nothing of the default is added to it\n\
         links: raw, every URL left as written\n"
    );
    assert_eq!(
        told(Some(&inline), Some(&default)),
        "display: the default, by name, chosen for the agent\n  \
         not used: the model's choice, a config written in place\n\
         links: defanged, every off-site URL\n"
    );
}

/// Every rule is listed, however many: nothing is left out of what an
/// operator is told
#[test]
fn the_effective_config_lists_every_rule() {
    let rules: Vec<String> = (0..500)
        .map(|i| format!("https://host{i}.example"))
        .collect();
    let rules: Vec<&str> = rules.iter().map(String::as_str).collect();
    let section = AgentDisplayProfiles {
        default: allowing(&rules),
        profiles: BTreeMap::new(),
    };
    let told = section.resolve(None, None).unwrap().to_string();
    assert!(told.contains("but for 500 allowed\n"), "{told}");
    assert_eq!(
        told.lines().filter(|l| l.starts_with("  allow ")).count(),
        500
    );
    assert!(told.ends_with("  allow https://host499.example\n"));
}

#[test]
fn the_effective_config_lists_every_author() {
    let authors = |names: &[&str], rules: &[&str]| {
        let section = AgentDisplayProfiles {
            default: config(Links::Defanged {
                allow: rules.iter().map(|r| rule(r)).collect(),
                allow_authors: names
                    .iter()
                    .map(|n| n.parse().unwrap())
                    .collect(),
            }),
            profiles: BTreeMap::new(),
        };
        let told = section.resolve(None, None).unwrap().to_string();
        told.lines().skip(1).collect::<Vec<_>>().join("\n")
    };
    assert_eq!(
        authors(&["claude-ai", "steward"], &[]),
        "links: defanged, every off-site URL but in 2 authors' own text\n  \
         author claude-ai\n  author steward"
    );
    assert_eq!(
        authors(&["claude-ai"], &["https://docs.rs"]),
        "links: defanged, but for 1 allowed, and in 1 author's own text\n  \
         allow https://docs.rs\n  author claude-ai"
    );
}

// ── Schemas ─────────────────────────────────────────────────────────────

#[cfg(feature = "schemars")]
#[test]
fn schemas_are_inline_and_say_nothing_is_merged() {
    let section = schemars::schema_for!(AgentDisplayProfiles);
    let selection = schemars::schema_for!(DisplaySelection);
    for schema in [
        &section,
        &selection,
        &schemars::schema_for!(ProfileName),
        &schemars::schema_for!(Option<DisplaySelection>),
    ] {
        let text = serde_json::to_string(schema).unwrap();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
        assert!(!text.contains("pattern"), "{text}");
    }
    let section = serde_json::to_value(&section).unwrap();
    // The default's keys beside `profiles`, whose values are configs
    assert_eq!(
        section["properties"]["links"]["oneOf"][0]["properties"]["mode"]["const"],
        "defanged",
        "{section:#}"
    );
    let profiles = &section["properties"]["profiles"];
    assert!(
        profiles["additionalProperties"]["properties"]["links"].is_object(),
        "{section:#}"
    );
    for described in [&section, profiles] {
        let said = described["description"].as_str().unwrap();
        assert!(said.contains("nothing is merged"), "{said}");
    }
    let selection = serde_json::to_value(&selection).unwrap();
    assert!(
        selection["description"]
            .as_str()
            .unwrap()
            .contains("nothing is merged")
    );
    assert_eq!(selection["anyOf"][0]["type"], "string");
    assert!(selection["anyOf"][1]["properties"]["links"].is_object());
}

// ── Real TOML, as an operator writes it and the runner reads it ──────────

mod as_toml {
    use super::*;

    /// A runner's file: the `[display]` section and its models' choices
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        #[serde(default)]
        display: AgentDisplayProfiles,
        #[serde(default)]
        model: Vec<Model>,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Model {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<DisplaySelection>,
    }

    fn read<T: serde::de::DeserializeOwned>(text: &str) -> T {
        toml::from_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
    }

    fn refused<T: serde::de::DeserializeOwned + fmt::Debug>(
        text: &str,
    ) -> String {
        toml::from_str::<T>(text).expect_err(text).to_string()
    }

    #[test]
    fn links_inline() {
        #[derive(Deserialize)]
        struct One {
            links: Links,
        }
        let one: One = read(
            r#"links = { mode = "defanged", allow = ["https://*.wikipedia.org", "https://www.bbc.com/news"] }"#,
        );
        assert_eq!(
            one.links,
            Links::Defanged {
                allow: vec![
                    rule("https://*.wikipedia.org"),
                    rule("https://www.bbc.com/news")
                ],
                allow_authors: Vec::new(),
            }
        );
        let one: One =
            read(r#"links = { mode = "raw", deny = ["https://example.org"] }"#);
        assert_eq!(
            one.links,
            Links::Raw {
                deny: vec![rule("https://example.org")]
            }
        );
        let one: One = read(r#"links = { mode = "defanged" }"#);
        assert_eq!(one.links, Links::default());
    }

    const FILE: &str = r#"
[display]
links = { mode = "defanged" }

[display.profiles.news]
links = { mode = "defanged", allow = ["https://*.wikipedia.org", "https://www.bbc.com"] }

[display.profiles.open]
links = { mode = "raw", deny = ["https://example.org/tracker"] }

[[model]]
id = "haiku"
display = "news"

[[model]]
id = "sonnet"
display = "default"

[[model]]
id = "opus"
display = { links = { mode = "raw" } }

[[model]]
id = "local"
"#;

    #[test]
    fn a_runners_file() {
        let file: File = read(FILE);
        assert_eq!(file.display.default, AgentDisplayConfig::default());
        assert_eq!(
            file.display
                .profiles
                .keys()
                .map(ProfileName::as_str)
                .collect::<Vec<_>>(),
            ["news", "open"]
        );
        assert_eq!(
            file.display.profiles[&name("news")],
            allowing(&["https://*.wikipedia.org", "https://www.bbc.com"])
        );
        let choices: Vec<_> =
            file.model.iter().map(|m| m.display.clone()).collect();
        assert_eq!(
            choices,
            [
                Some(DisplaySelection::Profile(name("news"))),
                Some(DisplaySelection::Default),
                Some(DisplaySelection::Inline(config(Links::Raw {
                    deny: Vec::new()
                }))),
                None,
            ]
        );
        for model in &file.model {
            if let Some(choice) = &model.display {
                file.display.check(choice).unwrap();
            }
        }
        let news = file
            .display
            .resolve(file.model[0].display.as_ref(), None)
            .unwrap();
        assert_eq!(news.config, &file.display.profiles[&name("news")]);
    }

    #[test]
    fn a_file_round_trips() {
        let file: File = read(FILE);
        let written = toml::to_string(&file).unwrap();
        assert_eq!(read::<File>(&written), file, "{written}");
    }

    /// Every unknown key, at every level, and an unknown profile: refused,
    /// and the message names it
    #[test]
    fn what_is_refused_names_itself() {
        for (text, named) in [
            ("[display]\nlnks = { mode = \"raw\" }\n", "lnks"),
            ("[display]\nlinks = { mode = \"raw\", alow = [] }\n", "alow"),
            ("[display]\nlinks = { mode = \"open\" }\n", "open"),
            (
                "[display.profiles.news]\nlinks = { mode = \"raw\" }\ncolour = 1\n",
                "colour",
            ),
            (
                "[display.profiles.news]\nlinks = { mode = \"defanged\", allow = [\"ftp://x.org\"] }\n",
                "ftp",
            ),
            ("[display.profiles.Default]\n", "Default"),
            ("[display.profiles.default]\n", "default"),
            (
                "[[model]]\nid = \"x\"\ndisplay = { links = { mode = \"raw\" }, extra = 1 }\n",
                "extra",
            ),
            ("[[model]]\nid = \"x\"\ndisplay = \"News\"\n", "News"),
        ] {
            let message = refused::<File>(text);
            assert!(message.contains(named), "{text}: {message}");
        }
        // A profile no one defined is refused when it is checked
        let file: File = read("[[model]]\nid = \"x\"\ndisplay = \"nwes\"\n");
        let choice = file.model[0].display.as_ref().unwrap();
        let message = file.display.check(choice).unwrap_err().to_string();
        assert!(message.contains("nwes"), "{message}");
        assert!(message.contains("no profile is defined"), "{message}");
        let file: File = read(&format!(
            "{FILE}\n[[model]]\nid = \"x\"\ndisplay = \"nwes\"\n"
        ));
        let choice = file.model.last().unwrap().display.as_ref().unwrap();
        let message = file.display.check(choice).unwrap_err().to_string();
        assert!(
            message.contains("`nwes`") && message.contains("`news`, `open`"),
            "{message}"
        );
    }

    /// `allow_authors`: names, round-tripped; a name that cannot be one
    /// and an agent's id refused naming the key; refused under `raw`
    #[test]
    fn allow_authors() {
        let file: File = read(
            "[display]\nlinks = { mode = \"defanged\", allow = \
             [\"https://docs.rs\"], allow_authors = [\"claude-ai\", \"Oracle\"] }\n",
        );
        let Links::Defanged { allow_authors, .. } =
            &file.display.default.common.links
        else {
            panic!("defanged")
        };
        assert_eq!(allow_authors, &["claude-ai", "Oracle"]);
        let written = toml::to_string(&file).unwrap();
        assert!(written.contains("allow_authors = ["), "{written}");
        assert_eq!(read::<File>(&written), file, "{written}");

        for (author, named) in [
            ("\"no spaces\"", "' '"),
            ("\"ab\"", "3 to 64"),
            ("\"7ad26ccd-922f-484a-a37c-51777344a98c\"", "by name"),
            ("1", "string"),
            ("\"claude-ai\", \"claude-ai\"", "listed twice"),
        ] {
            let text = format!(
                "[display]\nlinks = {{ mode = \"defanged\", allow_authors = [{author}] }}\n"
            );
            let message = refused::<File>(&text);
            assert!(message.contains(named), "{text}: {message}");
            if named != "string" {
                assert!(message.contains("allow_authors"), "{text}: {message}");
            }
        }
        let message = refused::<File>(
            "[display]\nlinks = { mode = \"raw\", allow_authors = [\"claude-ai\"] }\n",
        );
        assert!(message.contains("allow_authors"), "{message}");
    }
}
