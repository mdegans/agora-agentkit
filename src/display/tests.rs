use super::*;

use serde_json::json;

use crate::threads;

use super::oracle::{Reader, destination, places, readings};
pub(in crate::display) use super::oracle::{SPELLINGS, fetchable};

pub(super) fn site() -> Url {
    Url::parse("https://subliminal.technology").unwrap()
}

/// The default policy with no site: what the server's `defang` did
fn defang(text: &str) -> Cow<'_, str> {
    Links::default().rewrite(text, None)
}

fn rule(s: &str) -> LinkRule {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

fn allow(rules: &[&str]) -> Links {
    Links::Defanged {
        allow: rules.iter().map(|r| rule(r)).collect(),
        allow_authors: Vec::new(),
    }
}

fn deny(rules: &[&str]) -> Links {
    Links::Raw {
        deny: rules.iter().map(|r| rule(r)).collect(),
    }
}

fn refusal(s: &str) -> LinkRuleRefusal {
    s.parse::<LinkRule>().expect_err(s).reason
}

// ── The default policy: the server's tests, as they were ────────────────

#[test]
fn plain_text_is_borrowed_untouched() {
    let text = "no links here, just handlers.rs and v1.2 numbers.";
    assert!(matches!(defang(text), Cow::Borrowed(_)));
}

#[test]
fn https_url_is_defanged() {
    assert_eq!(
        defang("see https://example.com/path?q=1 for details"),
        "see hxxps://example[.]com/path?q=1 for details"
    );
}

#[test]
fn http_url_is_defanged() {
    assert_eq!(defang("http://a.b.co"), "hxxp://a[.]b[.]co");
}

#[test]
fn scheme_case_is_normalized_not_missed() {
    assert_eq!(defang("HTTPS://EXAMPLE.COM"), "hxxps://EXAMPLE[.]COM");
}

#[test]
fn bare_www_host_is_defanged() {
    assert_eq!(
        defang("visit www.example.com today"),
        "visit www[.]example[.]com today"
    );
}

#[test]
fn markdown_link_destination_is_defanged() {
    assert_eq!(
        defang("[read this](https://evil.example/payload)"),
        "[read this](hxxps://evil[.]example/payload)"
    );
}

#[test]
fn multiple_urls_all_defanged() {
    assert_eq!(
        defang("https://a.com and http://b.org"),
        "hxxps://a[.]com and hxxp://b[.]org"
    );
}

#[test]
fn credentials_and_port_stay_inside_authority() {
    assert_eq!(
        defang("https://user:pass@host.tld:8443/x"),
        "hxxps://user:pass@host[.]tld:8443/x"
    );
}

#[test]
fn path_dots_are_left_alone() {
    assert_eq!(
        defang("https://example.com/file.name.txt"),
        "hxxps://example[.]com/file.name.txt"
    );
}

#[test]
fn mid_word_www_does_not_fire() {
    let text = "foo.www.bar is one hostname mention, not a URL start";
    assert!(matches!(defang(text), Cow::Borrowed(_)));
}

#[test]
fn already_defanged_text_is_stable() {
    let text = "hxxps://example[.]com stays as it is";
    assert!(matches!(defang(text), Cow::Borrowed(_)));
}

#[test]
fn utf8_around_urls_is_not_corrupted() {
    // A non-ASCII byte ends the authority; the rest is copied
    assert_eq!(
        defang("café https://exämple.com 中文"),
        "café hxxps://exämple.com 中文"
    );
}

#[test]
fn trailing_scheme_only_still_rewrites() {
    assert_eq!(defang("go to https://"), "go to hxxps://");
}

/// A URL inside another's path or query is a start of its own
#[test]
fn a_url_inside_a_url_is_defanged_too() {
    assert_eq!(
        defang("https://a.com/r?u=https://b.org/x&v=www.c.net"),
        "hxxps://a[.]com/r?u=hxxps://b[.]org/x&v=www[.]c[.]net"
    );
}

/// A scheme is a start wherever it is written, and one inside another
/// URL's authority is not taken for part of it. The server's scanner left
/// each of these fetchable as written
#[test]
fn a_scheme_is_a_start_anywhere() {
    for (text, want) in [
        ("xhttp://example.com", "xhxxp://example[.]com"),
        ("1https://example.com", "1hxxps://example[.]com"),
        ("so.https://example.com", "so.hxxps://example[.]com"),
        ("XhTtP://Example.com", "Xhxxp://Example[.]com"),
        (
            "https://a:https://evil.example/x",
            "hxxps://a:hxxps://evil[.]example/x",
        ),
        (
            "https://a.b@http://evil.example/x",
            "hxxps://a[.]b@hxxp://evil[.]example/x",
        ),
        ("https://https://x.y", "hxxps://hxxps://x[.]y"),
        ("httphttp://x.y", "httphxxp://x[.]y"),
    ] {
        assert_eq!(defang(text), want);
    }
    // A schemeless host still starts a word
    for prose in ["awww.example.com", "swww.example.com", "a.www.example.com"] {
        assert!(matches!(defang(prose), Cow::Borrowed(_)), "{prose}");
    }
}

/// xorshift64: the same texts on every run
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

/// Texts made of the pieces URLs are made of, in no sensible order
pub(super) fn generated(count: usize) -> Vec<String> {
    const PIECES: &[&str] = &[
        "http://",
        "https://",
        "HTTPS://",
        "hTtP://",
        "http:/",
        "http",
        "https",
        "ht",
        "tp",
        ":/",
        ":\\",
        "\t",
        "\r",
        "'",
        "\"",
        "`",
        "{",
        "}",
        "|",
        ";",
        "\u{a0}",
        "denied.example",
        "s://",
        "://",
        "www.",
        "WWW.",
        "a",
        "x",
        "evil.example",
        "ok.example",
        "en.ok.example",
        "subliminal.technology",
        "/agora/post/7ad26ccd",
        "/agora/search?q=",
        "/agora/protocol",
        "/agora/",
        "/x",
        "/..",
        "/",
        ":",
        ":8443",
        "@",
        "?",
        "?u=",
        "#",
        "&",
        "=",
        ".",
        "..",
        ",",
        "_",
        "*",
        " ",
        "\n",
        "(",
        ")",
        "[",
        "]",
        "<",
        ">",
        "\\",
        "ü",
        "中",
        "%2F",
        "%2e",
        "\u{1}",
        "u:p@",
        "[::1]",
        "127.0.0.1",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    (0..count)
        .map(|_| {
            (0..1 + rng.next() % 12)
                .map(|_| PIECES[rng.next() % PIECES.len()])
                .collect()
        })
        .collect()
}

pub(super) const ADVERSARIAL: &[&str] = &[
    "https://a:https://evil.example/x",
    "https://a:8443:http://evil.example/x",
    "https://https://evil.example/x",
    "http://https://http://evil.example/x",
    "https://u:https://evil.example/x@ok.example/",
    "https://https://ok.example@evil.example/",
    "https://ok.example/a/https://evil.example/x",
    "https://ok.example/a/https://evil.example/x/../../../../y",
    "https://ok.example/?u=https://evil.example/x&v=http://evil.example",
    "https://ok.example/#https://evil.example/x",
    "https://ok.example\\@https://evil.example/x",
    "HTTPS://EVIL.EXAMPLE/x and hTtP://evil.example/x and httpS://Evil.Example",
    "xhttp://evil.example 1https://evil.example so.https://evil.example",
    "httphttp://evil.example/x",
    "https://www.https://evil.example/x",
    "www.ok.example/https://evil.example/x",
    "https://subliminal.technology/agora/search?q=https://evil.example/x",
    "https://subliminal.technology/agora/post/7ad26ccd#https://evil.example/x",
    "https://subliminal.technology/agora/post/7ad26ccd?u=http://evil.example/x",
    "https://subliminal.technology:https://evil.example/x",
    "http://subliminal.technology/agora/protocolhttps://evil.example/x",
    "[x](https://evil.example/x)<https://evil.example/x>\"http://evil.example\"",
    "https://exämple.https://evil.example/x",
    "https://\u{1}https://evil.example/x",
];

/// Whether a scheme starts anywhere in `text`
fn has_scheme(text: &str) -> bool {
    (0..text.len()).any(|i| scheme_at(text, i).is_some())
}

/// Under the default policy no `http` or `https` URL is left, in any
/// case or spelling, whatever the text and whatever the site: by the
/// scanner's own test, and by a URL parser at every place in the result
#[test]
fn the_default_leaves_no_scheme() {
    let sites = [
        None,
        Some(site()),
        Some(url("https://subliminal.technology/agora")),
    ];
    let texts = generated(20_000);
    let mut seen = 0;
    for text in ADVERSARIAL
        .iter()
        .chain(SPELLINGS)
        .copied()
        .chain(texts.iter().map(String::as_str))
    {
        seen += usize::from(fetchable(text).is_some());
        for site in &sites {
            let out = Links::default().rewrite(text, site.as_ref());
            assert!(!has_scheme(&out), "{text:?} at {site:?} gave {out:?}");
            assert_eq!(
                fetchable(&out).map(String::from),
                None,
                "{text:?} at {site:?} gave {out:?}"
            );
            // And as a body is shown: the policy, then `tidy`
            let tidied = threads::tidy(&out);
            assert_eq!(
                fetchable(&tidied).map(String::from),
                None,
                "{text:?} at {site:?} gave {tidied:?}"
            );
        }
    }
    assert!(seen > 3_000, "{seen} texts had a URL: too few to tell");
    // The site's pages are still links: one that carries a URL has it
    // percent-encoded, and a page with no link is defanged with what is in it
    assert_eq!(
        Links::default().rewrite(
            "https://subliminal.technology/agora/search?q=https://evil.example/x",
            Some(&site())
        ),
        "agora://search?q=https%3A%2F%2Fevil.example%2Fx"
    );
    assert_eq!(
        Links::default().rewrite(
            "https://subliminal.technology/agora/post/7ad26ccd?u=http://evil.example/x \
             xhttps://subliminal.technology/agora/protocol",
            Some(&site())
        ),
        "hxxps://subliminal[.]technology/agora/post/7ad26ccd?u=hxxp://evil[.]example/x \
         xagora://protocol"
    );
}

#[test]
fn a_scheme_is_seen_however_it_is_spelt() {
    for spelling in SPELLINGS {
        assert!(fetchable(spelling).is_some(), "{spelling:?} is not a URL");
    }
    for (text, want) in [
        ("https:/evil.example/x", "hxxps:/evil[.]example/x"),
        ("https:evil.example/x", "hxxps:evil[.]example/x"),
        ("https:\\\\evil.example/x", "hxxps:\\\\evil[.]example/x"),
        ("http:/\\evil.example", "hxxp:/\\evil[.]example"),
        ("HTTPS:EVIL.EXAMPLE", "hxxps:EVIL[.]EXAMPLE"),
        // What a parser drops is kept, and the scheme is still not one
        ("ht\ttps://evil.example/x", "hx\txps://evil[.]example/x"),
        ("https\r\n://evil.example/x", "hxxps\r\n://evil[.]example/x"),
        ("https://\nevil.example/x", "hxxps://\nevil.example/x"),
        ("https:\nevil.example/x", "hxxps:\nevil.example/x"),
        ("http::@evil.example/x", "hxxp::@evil[.]example/x"),
        ("http:\".evil.example/x", "hxxp:\".evil.example/x"),
        // A line that ends in `https:` is read with the next one
        ("use https:\nevil.example", "use hxxps:\nevil.example"),
    ] {
        assert_eq!(defang(text), want);
        assert_eq!(fetchable(want), None, "{want:?}");
    }
    // Prose: nothing a parser reads as a URL with a host
    for prose in [
        "use https: it is safer",
        // (to a single label, as a parser reads it: not seen)
        "use https:\nit is safer",
        "the scheme is http:",
        "\"http:\" and `https:` and (http:) and http:, http:; http:.",
        "http::Request::builder() and std::http::StatusCode",
        "https:// is always a start, https:? and http:# and http:/ are not",
        "http: \u{a0}x and http: x",
        "ht tp://example.com, h ttps:example.com",
        "http\u{a0}://example.com",
        "httpx://example.com, htps://example.com, shttp//example.com",
    ] {
        let out = defang(prose);
        assert_eq!(
            out.replace("hxxps://", "https://"),
            prose,
            "only `https://` alone is rewritten"
        );
        assert_eq!(fetchable(prose), None, "{prose:?}");
    }
    // Under rules these are never left: the two parsers an operator and
    // a fetcher have may not agree on them
    let allowed = allow(&["https://evil.example", "http://evil.example"]);
    let denied = deny(&["https://other.example"]);
    for spelling in SPELLINGS {
        for links in [&allowed, &denied] {
            let out = links.rewrite(spelling, None);
            assert_eq!(
                fetchable(&out).map(String::from),
                None,
                "{links:?}: {spelling:?} gave {out:?}"
            );
        }
    }
    // With no rule at all, raw is raw
    let raw = Links::Raw { deny: Vec::new() };
    for spelling in SPELLINGS {
        assert!(matches!(raw.rewrite(spelling, None), Cow::Borrowed(_)));
    }
}

/// What the review of 0.166.0 found left as written: a rule passed the
/// URL up to a bracket or quote, and a parser reads past it
const PAST_THE_END: &[&str] = &[
    "https://allowed.example'@evil.com/x",
    "https://allowed.example(@evil.com/x",
    "https://allowed.example)@evil.com/x",
    "https://allowed.example\"@evil.com/x",
    "https://allowed.example|@evil.com/x",
    "https://allowed.example{@evil.com/x",
    "https://allowed.example}@evil.com/x",
    "https://allowed.example<@evil.com/x",
    "https://allowed.example>@evil.com/x",
    "https://allowed.example[@evil.com/x",
    "https://allowed.example]@evil.com/x",
    "https://allowed.example`@evil.com/x",
    "https://allowed.example'.evil.com/x",
    "https://allowed.example(.evil.com/x",
    "https://allowed.example).evil.com/x",
    "https://en.wikipedia.org/wiki/x'/../../w",
    "https://en.wikipedia.org/wiki/x)/../../w",
    // Allowed at both ends, and not at a bracket between them
    "https://en.wikipedia.org/wiki/x'/../..)/wiki/y",
    "https://allowed.example'@evil.com)@allowed.example/x",
    "https://allowed.example'.evil.com\"/x",
];

/// What a reader that deletes whitespace or line breaks inside a URL
/// would read past an allowed one: outside what is guarded (the Steward,
/// 2026-11-16), and left, since rules judge a URL to its first whitespace
const ACROSS_A_BREAK: &[&str] = &[
    "https://allowed.example\t.evil.com/x",
    "https://allowed.example\n.evil.com/x",
    "https://allowed.example\r\n.evil.com/x",
    "https://allowed.example.\nevil.com/x",
    "https://allowed.example\t@evil.com/x",
    "https://allowed.example\n:8443/x",
    "https://allowed.example  \n.evil.com/x",
    "https://allowed.example\u{2028}.evil.com/x",
    "https://allowed.example \u{b}@evil.com/x",
    "https://en.wikipedia.org/wiki/x\n/../../w",
    "https://en.wikipedia.org/wiki/.\n./w",
    "https://en.wikipedia.org/wiki\n/../w",
    "https://en.wikipedia.org/wiki\nleaks/x",
    "https://en.wikipedia.org/wiki/x'/..\n/..\"/wiki/y",
];

#[test]
fn an_allow_rule_is_not_passed_by_a_longer_reading() {
    let links =
        allow(&["https://allowed.example", "https://en.wikipedia.org/wiki"]);
    let Links::Defanged { allow: rules, .. } = &links else {
        unreachable!()
    };
    for text in PAST_THE_END {
        let out = links.rewrite(text, None);
        assert!(out.starts_with("hxxps://"), "{text:?} gave {out:?}");
        assert_eq!(fetchable(&out), None, "{text:?} gave {out:?}");
    }
    // What is left as written: every reading that names a host is on
    // the allowed one and under the rule's path
    for text in [
        "[t](https://allowed.example/x)",
        "[t](https://allowed.example/x).",
        "[t](https://allowed.example)",
        "[t](https://allowed.example), then",
        "[t](https://allowed.example/x),[u](https://allowed.example/y)",
        "[t](https://allowed.example),next",
        "<a href=\"https://allowed.example/x\">t</a>",
        "<a href=\"https://allowed.example\">t</a>",
        "<https://allowed.example>",
        "\"https://allowed.example\"",
        "('https://allowed.example/'),",
        "{\"url\":\"https://allowed.example/x\",\"n\":1}",
        "|https://allowed.example/x|https://allowed.example/y|",
        "https://allowed.example\nNext line",
        "https://allowed.example\n\nNext paragraph",
        "https://allowed.example/x\nNext line",
        "https://allowed.example/\nNext line",
        "https://allowed.example\n\n",
        "https://allowed.example.\n",
        "https://allowed.example\t and",
        "https://allowed.example\u{2028}next",
        "https://allowed.example/x\u{a0}y",
        "https://en.wikipedia.org/wiki/Rust\nNext line",
        "https://en.wikipedia.org/wiki\nNext line",
        "(https://en.wikipedia.org/wiki)",
        "https://en.wikipedia.org/wiki.\n",
        "https://en.wikipedia.org/wiki/x'/y)",
        "see https://allowed.example/a(b)c[d]e{f}g<h>i'j\"k`l|m end",
        // Ends in the path cannot change the host, and those that name no
        // host cost nothing: neither is counted
        "https://allowed.example/a(b)c[d]e{f}g<h>i'j\"k`l|m(n)o[p]q{r}s",
        "https://allowed.example(a)b(c)d(e)f(g)h(i)j(k)l(m)n(o)p(q)r(s)t/",
    ]
    .iter()
    .chain(ACROSS_A_BREAK)
    {
        let out = links.rewrite(text, None);
        assert!(matches!(out, Cow::Borrowed(_)), "{text:?} gave {out:?}");
        for reading in readings(text, Reader::Whitespace) {
            if let Some(found) = destination(&reading) {
                assert!(rules.iter().any(|r| r.allows(&found)), "{found}");
            }
        }
    }
    // A reading past a bracket that names a host is checked: here the
    // URL with another host, and a path that leaves the rule's
    for (text, want) in [
        (
            "[t](https://allowed.example),next.evil.com",
            "[t](hxxps://allowed[.]example),next.evil.com",
        ),
        (
            "[t](https://allowed.example)%2eevil.com",
            "[t](hxxps://allowed[.]example)%2eevil.com",
        ),
        (
            "[t](https://en.wikipedia.org/wiki),next",
            "[t](hxxps://en[.]wikipedia[.]org/wiki),next",
        ),
        // One left and the one after it not: each is judged on its own
        (
            "[t](https://allowed.example/x),[u](https://evil.com/y)",
            "[t](https://allowed.example/x),[u](hxxps://evil[.]com/y)",
        ),
    ] {
        assert_eq!(links.rewrite(text, None), want);
    }
}

#[test]
fn a_deny_rule_is_not_passed_by_a_longer_reading() {
    let links = deny(&["https://denied.example"]);
    for text in [
        "https://ok.example'@denied.example/x",
        "https://ok.example(@denied.example/x",
        "https://ok.example|@denied.example/x",
        "https://ok.example\"@denied.example/x",
        "https://ok.example[@denied.example/x",
        "https://ok.example`@denied.example/x",
        // Denied at a bracket between two ends that are not
        "https://ok.example'@denied.example)@ok.example/x",
        "https://u'@denied.example\"x/",
        // Spellings a parser reads as the denied host
        "https:/denied.example/x",
        "https:denied.example/x",
        "https:\\\\denied.example/x",
        "http:/\\denied.example",
        "ht\ttps://denied.example/x",
        "https:\n//denied.example/x",
    ] {
        let out = links.rewrite(text, None);
        assert!(out.starts_with("hx"), "{text:?} gave {out:?}");
        assert_eq!(fetchable(&out), None, "{text:?} gave {out:?}");
    }
    let path = deny(&["https://h.example/a/b"]);
    for text in [
        "https://h.example/a;'/b)zz",
        "https://h.example/y'/../a/b)/../../y",
    ] {
        let out = path.rewrite(text, None);
        assert!(out.starts_with("hxxps://"), "{text:?} gave {out:?}");
    }
    for text in [
        "[t](https://ok.example/x)",
        "https://ok.example\nNext line",
        "https://ok.example'x)y",
        "<a href=\"https://ok.example/x\">t</a>",
        "<a href=\"https://ok.example\">t</a>",
        // Rules judge a URL to its first whitespace: what a reader that
        // deletes line breaks inside one would read is not guarded
        "https://deni\ted.example/",
        "https://deni\r\ned.example/",
        "https://ok.example\t@denied.example/",
        "https://ok.example  \n@denied.example/",
        "https://deni \u{2028}ed.example/",
        "https://h.example/a\n/b)zz",
    ] {
        let out = links.rewrite(text, None);
        assert!(matches!(out, Cow::Borrowed(_)), "{text:?} gave {out:?}");
    }
    // `https://denied` is not the denied host, and a parser handed
    // `https://denied\n.example/` whole reads one; but a scheme spelt
    // across a break is still a start, and never left under rules
    assert_eq!(
        links.rewrite("https:\n//denied.example/", None),
        "hxxps:\n//denied[.]example/"
    );
}

/// Under `raw`, no URL left in the result is one a deny rule matches,
/// under any reading, each found here without the scanner.
///
/// A reading ends at whitespace of any kind ([`Reader::Whitespace`]): a
/// reader that deletes line breaks inside a URL is outside what is
/// guarded, by the Steward's decision (2026-11-16). A reading whose host
/// no one could reach ([`destination`]) is not one
#[test]
fn raw_leaves_no_denied_url() {
    let links = deny(&[
        "https://evil.example",
        "https://denied.example",
        "https://ok.example/x",
        "https://*.ok.example",
        "http://a:8443",
        "http://127.0.0.1",
    ]);
    let Links::Raw { deny: rules } = &links else {
        unreachable!()
    };
    let sites = [None, Some(site())];
    let texts = generated(20_000);
    let mut left = 0;
    for text in ADVERSARIAL
        .iter()
        .chain(SPELLINGS)
        .chain(PAST_THE_END)
        .copied()
        .chain(texts.iter().map(String::as_str))
    {
        for site in &sites {
            let out = links.rewrite(text, site.as_ref());
            for out in [out.to_string(), threads::tidy(&out)] {
                for (at, reading) in places(&out, Reader::Whitespace) {
                    // A schemeless host is one only where a word starts, and
                    // with a host's character written after its `www.`
                    let word = !out[..at].ends_with(|c: char| {
                        c.is_ascii_alphanumeric() || c == '.'
                    }) && out[at..]
                        .get(4..)
                        .unwrap_or_default()
                        .starts_with(|c: char| {
                            c.is_ascii_alphanumeric() || "-._~%:@".contains(c)
                        });
                    let found =
                        if reading.to_ascii_lowercase().starts_with("www.") {
                            destination(&["http://", &reading].concat())
                                .filter(|_| word)
                        } else {
                            destination(&reading)
                        };
                    let Some(found) = found else {
                        continue;
                    };
                    left += 1;
                    assert!(
                        !rules.iter().any(|r| r.denies(&found)),
                        "{text:?} gave {out:?}, where {found} is denied"
                    );
                }
            }
        }
    }
    assert!(left > 1_000, "the texts left {left} URLs: too few to tell");
    // No denied host in any adversarial text survives with a scheme
    for text in ADVERSARIAL {
        let out = links.rewrite(text, None).to_ascii_lowercase();
        for scheme in ["http://", "https://"] {
            assert!(!out.contains(&[scheme, "evil.example"].concat()), "{out}");
        }
    }
}

/// Under `defanged` with rules, every URL left in the result is one a
/// rule allows, under any reading, each found here without the scanner,
/// read as in [`raw_leaves_no_denied_url`]
#[test]
fn allow_leaves_only_allowed_urls() {
    let links = allow(&[
        "https://ok.example",
        "http://ok.example",
        "https://*.ok.example/x",
        "https://evil.example/x",
        "http://a:8443",
        "https://a",
        "http://x",
        "http://127.0.0.1",
        "https://allowed.example",
        "https://en.wikipedia.org/wiki",
    ]);
    let Links::Defanged { allow: rules, .. } = &links else {
        unreachable!()
    };
    let sites = [None, Some(site())];
    let texts = generated(20_000);
    let mut left = 0;
    for text in ADVERSARIAL
        .iter()
        .chain(SPELLINGS)
        .chain(PAST_THE_END)
        .copied()
        .chain(texts.iter().map(String::as_str))
    {
        for site in &sites {
            let out = links.rewrite(text, site.as_ref());
            let tidied = threads::tidy(&out);
            for found in readings(&out, Reader::Whitespace)
                .chain(readings(&tidied, Reader::Whitespace))
                .filter_map(|r| destination(&r))
            {
                left += 1;
                assert!(
                    rules.iter().any(|r| r.allows(&found)),
                    "{text:?} gave {out:?}, where {found} is not allowed"
                );
            }
        }
    }
    assert!(left > 300, "the texts left {left} URLs: too few to tell");
}

/// This site's page URLs are their links, which an agent can follow;
/// other sites', and this site's pages with no link, are defanged
/// (betas of agentkit 0.116.0)
#[test]
fn this_sites_pages_are_written_as_links() {
    let site = site();
    let at =
        |text: &str| Links::default().rewrite(text, Some(&site)).into_owned();
    assert_eq!(
        at("See https://subliminal.technology/agora/gov/REC-2026-0016."),
        "See agora://gov/REC-2026-0016."
    );
    assert_eq!(
        at(
            "[the post](https://subliminal.technology/agora/post/7ad26ccd) and \
            http://subliminal.technology/agora/protocol, then \
            https://example.com/x"
        ),
        "[the post](agora://post/7ad26ccd) and agora://protocol, then \
         hxxps://example[.]com/x"
    );
    assert_eq!(
        at(
            "<https://subliminal.technology/agora/gov?type=council_decision&limit=5>"
        ),
        "<agora://gov?limit=5&type=council_decision>"
    );
    // No kind of its own: a page link (0.168); a page the site served
    // before its paths were the links': the link it names
    assert_eq!(
        at("https://subliminal.technology/agora/account"),
        "agora://page/account"
    );
    assert_eq!(
        at("https://subliminal.technology/agora/governance/GOV-2026-0003"),
        "agora://gov/GOV-2026-0003"
    );
    // A look-alike host is another site
    assert_eq!(
        at("https://subliminal.technology.evil.example/agora/post/7ad26ccd"),
        "hxxps://subliminal[.]technology[.]evil[.]example/agora/post/7ad26ccd"
    );
    assert_eq!(
        at("https://subliminal.technology@evil.example/agora/protocol"),
        "hxxps://subliminal[.]technology@evil[.]example/agora/protocol"
    );
    // With no site, every URL is defanged
    assert_eq!(
        defang("https://subliminal.technology/agora/protocol"),
        "hxxps://subliminal[.]technology/agora/protocol"
    );
}

const BODY: &str = "You've already seen where unchecked consensus leads: [The \
    Upstream Jester Experiment](https://subliminal.technology/agora/c/\
    meta-governance/c6943196-1825-4699-adab-51be16d06e56). See \
    https://example.com/x";

/// The server's trusted-author test (betas of agentkit 0.117.0: post
/// 0b368437), by mode: an own-site URL is its link in every one, the legacy
/// `/agora/c/<community>/<post>` form included
#[test]
fn site_urls_are_links_in_every_mode() {
    let site = Url::parse("https://subliminal.technology/agora").unwrap();
    let linked = |rest: &str| {
        format!(
            "You've already seen where unchecked consensus leads: [The \
             Upstream Jester Experiment](agora://post/\
             c6943196-1825-4699-adab-51be16d06e56). See {rest}"
        )
    };
    let raw = linked("https://example.com/x");
    let defanged = linked("hxxps://example[.]com/x");
    for (links, want) in [
        (Links::default(), &defanged),
        (allow(&["https://example.org"]), &defanged),
        (allow(&["https://example.com"]), &raw),
        (Links::Raw { deny: Vec::new() }, &raw),
        (deny(&["https://example.org"]), &raw),
        (deny(&["https://example.com"]), &defanged),
        // A rule does not keep the site's own page from being its link
        (deny(&["https://subliminal.technology"]), &raw),
    ] {
        assert_eq!(&links.rewrite(BODY, Some(&site)), want, "{links:?}");
    }
    // A page of the site no kind names is `agora://page/…` in every mode
    // too (0.168): neither defanged nor whole
    let account = "https://subliminal.technology/agora/account";
    for links in [
        Links::default(),
        allow(&["https://subliminal.technology"]),
        Links::Raw { deny: Vec::new() },
        deny(&["https://subliminal.technology/agora"]),
        authors(&["claude-ai"])
            .for_author(Some(&agent("claude-ai")))
            .clone(),
    ] {
        assert_eq!(
            links.rewrite(account, Some(&site)),
            "agora://page/account",
            "{links:?}"
        );
    }
}

/// Every page of the site under `/agora/` is an `agora://` link: one no
/// kind names is `agora://page/<path>?<query>`, which reads back as that
/// page. Not one on a look-alike host, outside `/agora/`, or one that
/// carries another URL
#[test]
fn a_page_no_kind_names_is_a_page_link() {
    let site = site();
    for (text, want) in [
        (
            "See https://subliminal.technology/agora/hello-world.",
            "See agora://page/hello-world.",
        ),
        (
            "(https://subliminal.technology/agora/account)",
            "(agora://page/account)",
        ),
        // The query is kept; the fragment is not, as for every page URL
        (
            "https://subliminal.technology/agora/hello-world?step=2#keys",
            "agora://page/hello-world?step=2",
        ),
        (
            "http://subliminal.technology/agora/a/b%20c/",
            "agora://page/a/b%20c/",
        ),
        // A kind's page with what that kind does not read
        (
            "https://subliminal.technology/agora/post/not-an-id",
            "agora://page/post/not-an-id",
        ),
        // A kind's page as before
        (
            "https://subliminal.technology/agora/post/7ad26ccd",
            "agora://post/7ad26ccd",
        ),
        // `/agora/page/x` is a page called that, not `agora://page/x`
        (
            "https://subliminal.technology/agora/page/x",
            "agora://page/page/x",
        ),
        // Not Agora's: another host, the host outside `/agora/`, and what
        // a whole link would carry
        (
            "https://subliminal.technology.evil.example/agora/hello-world",
            "hxxps://subliminal[.]technology[.]evil[.]example/agora/hello-world",
        ),
        (
            "https://subliminal.technology/blog/x",
            "hxxps://subliminal[.]technology/blog/x",
        ),
        (
            "https://subliminal.technology/agora/x?next=https://evil.example/",
            "hxxps://subliminal[.]technology/agora/x?next=hxxps://evil[.]example/",
        ),
        (
            "https://subliminal.technology/agora/a/../b",
            "hxxps://subliminal[.]technology/agora/a/../b",
        ),
        (
            "https://subliminal.technology:443@evil.example/agora/x",
            "hxxps://subliminal[.]technology:443@evil[.]example/agora/x",
        ),
        // A schemeless host the link would carry, in a page's query or
        // path, or in a kind's free text: the link is not written
        (
            "https://subliminal.technology/agora/x?u=www.evil.example",
            "hxxps://subliminal[.]technology/agora/x?u=www[.]evil[.]example",
        ),
        (
            "https://subliminal.technology/agora/www.evil.example/x",
            "hxxps://subliminal[.]technology/agora/www[.]evil[.]example/x",
        ),
        (
            "https://subliminal.technology/agora/search?q=www.evil.example",
            "hxxps://subliminal[.]technology/agora/search?q=www[.]evil[.]example",
        ),
        // A dot segment between backslashes, which a parser reads as `/`
        (
            "https://subliminal.technology/agora/hello\\..\\account",
            "hxxps://subliminal[.]technology/agora/hello\\..\\account",
        ),
        (
            "https://subliminal.technology/agora/x\\..\\post\\7ad26ccd",
            "hxxps://subliminal[.]technology/agora/x\\..\\post\\7ad26ccd",
        ),
        // A user on the site's own host
        (
            "https://someone@subliminal.technology/agora/post/7ad26ccd",
            "hxxps://someone@subliminal[.]technology/agora/post/7ad26ccd",
        ),
    ] {
        assert_eq!(Links::default().rewrite(text, Some(&site)), want, "{text}");
    }
    // Round trips: the link's page is the URL it was read from, and that
    // URL reads as the link
    for page in [
        "https://subliminal.technology/agora/hello-world",
        "https://subliminal.technology/agora/account",
        "https://subliminal.technology/agora/hello-world?step=2",
        "https://subliminal.technology/agora/a/b%20c/",
        "https://subliminal.technology/agora/page/x",
    ] {
        let link = AgoraLink::on_site(page, &site).expect(page);
        assert!(matches!(link, AgoraLink::Page(_)), "{page}");
        assert_eq!(link.page_url(&site).as_str(), page);
        assert_eq!(AgoraLink::parse_at(page, Some(&site)), Ok(link.clone()));
        let written = link.to_string();
        assert_eq!(written.parse::<AgoraLink>(), Ok(link.clone()), "{written}");
        assert_eq!(
            link.site_path(),
            page.strip_prefix("https://subliminal.technology").unwrap()
        );
    }
    // A page link of a page a kind names reads as that kind's
    assert_eq!(
        "agora://page/post/7ad26ccd"
            .parse::<AgoraLink>()
            .unwrap()
            .to_string(),
        "agora://post/7ad26ccd"
    );
    assert!("agora://page".parse::<AgoraLink>().is_err());
    assert!("agora://page/a/../b".parse::<AgoraLink>().is_err());
}

/// Many URLs in one field cost time linear in its length, with rules to
/// check each against too
#[test]
fn many_urls_are_linear() {
    let site = site();
    // The shape, not a wall-clock bound (CI load makes a fixed bound
    // either flaky or loose): 8x the text costs ~8x when linear and
    // ~64x when quadratic, as the unmerged draft was (5.5 s at 64 KiB).
    let time = |links: &Links, text: &str| {
        let start = std::time::Instant::now();
        let _ = std::hint::black_box(links.rewrite(text, Some(&site)));
        start.elapsed()
    };
    let policies = [
        Links::default(),
        allow(&["https://a", "https://*.example.org/wiki"]),
        deny(&["https://a", "https://*.example.org/wiki"]),
        // Every path is checked: a rule with one, the host's own
        allow(&["http://a/x", "https://a/x/y"]),
        deny(&["http://a/x/y/z"]),
    ];
    // Lines of minified JSON, quotes and marks after an allowed URL, and
    // many starts in one run, each with every later end to check
    let json = [
        "{\"url\":\"http://a/x/y\"",
        &",\"k\":\"v\"".repeat(150),
        &",\"v\":\"1.2\"".repeat(10),
        "}\n",
    ]
    .concat();
    let quotes = ["http://a/x/", &"'y".repeat(500), " "].concat();
    let starts = ["http://a/x'".repeat(150), " ".into()].concat();
    let dotted = ["http://a/x/", &"a.b'".repeat(300), " "].concat();
    let marks = ["http://a", &"、a".repeat(200), " "].concat();
    let marked = ["https://a/x/", &"“y”".repeat(150), " "].concat();
    for links in &policies {
        // Many starts in one run without a space, each with every later
        // bracket and line break as an end of its own to check: the
        // bound is `MAX_URL_BYTES` and `MAX_URL_ENDS` a start
        let ends = ["http://a/", &"x".repeat(110), "'"].concat();
        let spaced = ["http://a/", &"x".repeat(60), " \n"].concat();
        for unit in [
            "http://a/",
            "http://a/ ",
            "www.a/",
            "https://subliminal.technology/x/",
            "https://subliminal.technology/agora/post/",
            "http://a/x'",
            "http://a/x)(",
            "http://a/x\n",
            "http://a/.\n.",
            "www.a/x|",
            "http:a/",
            "http:\\a'",
            "h\t",
            "http \n",
            "https: \n",
            "http::",
            &ends,
            &spaced,
            &json,
            &quotes,
            &starts,
            &dotted,
            &marks,
            &marked,
        ] {
            let small_text = unit.repeat(8_192 / unit.len());
            let large_text = unit.repeat(65_536 / unit.len());
            // Interleaved, fastest of each: a busy runner slows both sizes
            // alike instead of one (it flaked at 42x with three of each
            // timed back to back, 2026-10-23)
            let (mut small, mut large) =
                (std::time::Duration::MAX, std::time::Duration::MAX);
            for _ in 0..7 {
                small = small.min(time(links, &small_text));
                large = large.min(time(links, &large_text));
            }
            let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
            assert!(
                ratio < 24.0,
                "{links:?} {unit}: 8x the text took {ratio:.1}x the time"
            );
            if std::env::var_os("DISPLAY_TIMES").is_some() {
                eprintln!("{large:>12?} {links:?} {:.40?}", unit);
            }
        }
    }
    assert_eq!(
        Links::default().rewrite(
            "**https://subliminal.technology/agora/protocol**",
            Some(&site)
        ),
        "**agora://protocol**"
    );
}

// ── Links::rewrite with rules ───────────────────────────────────────────

#[test]
fn allow_rules_leave_only_matching_urls() {
    let links =
        allow(&["https://en.wikipedia.org/wiki", "https://*.rust-lang.org"]);
    assert_eq!(
        links.rewrite(
            "See https://en.wikipedia.org/wiki/Rust, \
             (https://doc.rust-lang.org/std/), https://rust-lang.org, \
             https://en.wikipedia.org/w/index.php and http://en.wikipedia.org/wiki/Rust.",
            None
        ),
        "See https://en.wikipedia.org/wiki/Rust, \
         (https://doc.rust-lang.org/std/), hxxps://rust-lang[.]org, \
         hxxps://en[.]wikipedia[.]org/w/index.php and hxxp://en[.]wikipedia[.]org/wiki/Rust."
    );
    // Only allowed URLs: nothing to change
    let text = "Read https://en.wikipedia.org/wiki/Rust.";
    assert!(matches!(links.rewrite(text, None), Cow::Borrowed(_)));
}

/// Sentence punctuation after a URL is not part of what a rule sees
#[test]
fn trailing_punctuation_is_not_the_urls() {
    let links = allow(&["https://example.org/wiki"]);
    for text in [
        "https://example.org/wiki.",
        "https://example.org/wiki, then",
        "**https://example.org/wiki**",
        "(https://example.org/wiki)",
        "<https://example.org/wiki>",
        "is it https://example.org/wiki?",
    ] {
        assert!(
            matches!(links.rewrite(text, None), Cow::Borrowed(_)),
            "{text}"
        );
    }
    // `/wiki_` is not `/wiki`, but a trailing `_` is emphasis
    assert!(matches!(
        links.rewrite("_https://example.org/wiki_", None),
        Cow::Borrowed(_)
    ));
    assert_eq!(
        links.rewrite("https://example.org/wiki_x", None),
        "hxxps://example[.]org/wiki_x"
    );
}

#[test]
fn what_an_allow_rule_never_leaves() {
    let links = allow(&["https://example.org", "http://www.example.org"]);
    for (text, want) in [
        // The suffix and userinfo attacks
        (
            "https://example.org.evil.example/",
            "hxxps://example[.]org[.]evil[.]example/",
        ),
        (
            "https://example.org@evil.example/",
            "hxxps://example[.]org@evil[.]example/",
        ),
        (
            "https://user:pw@example.org/",
            "hxxps://user:pw@example[.]org/",
        ),
        // A schemeless host, though `http://` on it is allowed
        ("www.example.org/x", "www[.]example[.]org/x"),
        // Parsers disagree on a backslash: some read evil.example as host
        // (the authority's dots end at it, as they always did)
        (
            "https://example.org\\@evil.example/",
            "hxxps://example[.]org\\@evil.example/",
        ),
        ("https://example.org/\u{1}", "hxxps://example[.]org/\u{1}"),
        // A URL with another inside it, a schemeless one too: both are
        // judged, and the outer is not left to be read up to the inner
        (
            "https://example.org/?u=https://evil.example/x",
            "hxxps://example[.]org/?u=hxxps://evil[.]example/x",
        ),
        (
            "https://example.org/a/www.evil.example",
            "hxxps://example[.]org/a/www[.]evil[.]example",
        ),
        // The inner one alone, when it is the allowed one
        (
            "https://evil.example/?u=https://example.org/x",
            "hxxps://evil[.]example/?u=https://example.org/x",
        ),
        (
            "xhttps://example.org.evil.example",
            "xhxxps://example[.]org[.]evil[.]example",
        ),
    ] {
        assert_eq!(links.rewrite(text, None), want);
    }
    assert_eq!(
        links.rewrite("http://www.example.org/x", None),
        "http://www.example.org/x"
    );
    // Too long to check: defanged
    let long = format!("https://example.org/{}", "a".repeat(MAX_URL_BYTES));
    assert_eq!(
        links.rewrite(&long, None),
        long.replace("https://example.org", "hxxps://example[.]org")
    );
    let fits = &long[..MAX_URL_BYTES];
    assert!(matches!(links.rewrite(fits, None), Cow::Borrowed(_)));
}

#[test]
fn raw_with_deny_defangs_only_matching_urls() {
    let text = "https://a.com, http://evil.example/x and www.evil.example.";
    let raw = Links::Raw { deny: Vec::new() };
    assert!(matches!(raw.rewrite(text, None), Cow::Borrowed(_)));
    assert_eq!(
        deny(&["https://evil.example"]).rewrite(text, None),
        "https://a.com, hxxp://evil[.]example/x and www.evil.example."
    );
    // (the sentence's dot is defanged with the host's, as it always was)
    assert_eq!(
        deny(&["https://*.evil.example"]).rewrite(text, None),
        "https://a.com, http://evil.example/x and www[.]evil[.]example[.]"
    );
}

/// A deny rule is not escaped by another spelling of the same place, and
/// what cannot be checked against one is defanged
#[test]
fn what_a_deny_rule_does_not_let_by() {
    let links = deny(&["https://evil.example/x"]);
    for (text, want) in [
        ("https://u:p@evil.example/x", "hxxps://u:p@evil[.]example/x"),
        (
            "https://ok.example@evil.example/x",
            "hxxps://ok[.]example@evil[.]example/x",
        ),
        ("http://evil.example/x", "hxxp://evil[.]example/x"),
        ("https://EVIL.example./x/y", "hxxps://EVIL[.]example[.]/x/y"),
        ("https://evil.example/%78", "hxxps://evil[.]example/%78"),
        ("https://evil.example//x", "hxxps://evil[.]example//x"),
        (
            "https://evil.example/y/../x",
            "hxxps://evil[.]example/y/../x",
        ),
        ("https://evil.example:443/x", "hxxps://evil[.]example:443/x"),
        (
            "https://evil.example:8443/x",
            "hxxps://evil[.]example:8443/x",
        ),
        ("http://evil.example:8080/x", "hxxp://evil[.]example:8080/x"),
        // An encoded separator, a segment's parameters, and `..` after
        // the decoding
        ("https://evil.example/%2Fx", "hxxps://evil[.]example/%2Fx"),
        (
            "https://evil.example/x;a=b/y",
            "hxxps://evil[.]example/x;a=b/y",
        ),
        (
            "https://evil.example/y%2F..%2Fx",
            "hxxps://evil[.]example/y%2F..%2Fx",
        ),
        (
            "https://evil.example/y%5C..%5Cx",
            "hxxps://evil[.]example/y%5C..%5Cx",
        ),
        ("https://evil.example/x%2Fy", "hxxps://evil[.]example/x%2Fy"),
        // Inside a URL that is not denied, which is then not left either:
        // what is left is what was checked, not a URL to be read up to
        // the `[` of the one inside it
        (
            "https://ok.example/?u=https://evil.example/x",
            "hxxps://ok[.]example/?u=hxxps://evil[.]example/x",
        ),
        // (`/x/https:/…/../../../../y` is `/y`, and so is the inner path)
        (
            "https://evil.example/x/https://evil.example/x/../../../../y",
            "hxxps://evil[.]example/x/https://evil.example/x/../../../../y",
        ),
        (
            "https://evil.example/y/https://evil.example/x/../../../../y",
            "hxxps://evil[.]example/y/https://evil.example/x/../../../../y",
        ),
        (
            "https://a:https://evil.example/x",
            "hxxps://a:hxxps://evil[.]example/x",
        ),
        (
            "https://http://evil.example/x",
            "hxxps://hxxp://evil[.]example/x",
        ),
        ("xhttps://evil.example/x", "xhxxps://evil[.]example/x"),
        ("HTTPS://EVIL.EXAMPLE/x", "hxxps://EVIL[.]EXAMPLE/x"),
        // Not checkable: a backslash, no host, a bad port
        (
            "https://ok.example\\@evil.example/x",
            "hxxps://ok[.]example\\@evil.example/x",
        ),
        ("https://", "hxxps://"),
        ("https://ok.example:99999/", "hxxps://ok[.]example:99999/"),
    ] {
        assert_eq!(links.rewrite(text, None), want);
    }
    let long = format!("https://ok.example/{}", "a".repeat(MAX_URL_BYTES));
    assert_eq!(
        links.rewrite(&long, None),
        long.replace("https://ok.example", "hxxps://ok[.]example")
    );
    // Elsewhere, and another path, are not the rule's
    for text in [
        "https://ok.example/x",
        "https://evil.example/xy",
        "https://evil.example/",
    ] {
        assert!(
            matches!(links.rewrite(text, None), Cow::Borrowed(_)),
            "{text}"
        );
    }
    // With no deny rule nothing is checked, so nothing is defanged
    let raw = Links::Raw { deny: Vec::new() };
    assert!(matches!(raw.rewrite(&long, None), Cow::Borrowed(_)));
    assert!(matches!(raw.rewrite("https://", None), Cow::Borrowed(_)));
}

// ── LinkRule ────────────────────────────────────────────────────────────

#[test]
fn a_rule_matches_scheme_host_and_port() {
    let r = rule("https://example.org");
    assert!(r.allows(&url("https://example.org")));
    assert!(r.allows(&url("https://example.org/any/path?q=1#f")));
    assert!(r.allows(&url("https://example.org:443/")));
    assert!(!r.allows(&url("http://example.org/")));
    assert!(!r.allows(&url("https://example.org:8443/")));
    assert!(!r.allows(&url("https://example.com/")));
    assert!(!r.allows(&url("https://www.example.org/")));
    // The default port, written in the rule or not
    assert_eq!(rule("https://example.org:443"), r);
    assert_eq!(rule("http://example.org:80"), rule("http://example.org"));
    let port = rule("https://example.org:8443");
    assert!(port.allows(&url("https://example.org:8443/x")));
    assert!(!port.allows(&url("https://example.org/x")));
    // `http://…:443` is not `https://…`
    assert!(!r.allows(&url("http://example.org:443/")));
    assert!(
        !rule("http://example.org:443").allows(&url("https://example.org"))
    );

    // A deny rule with no port covers every port and both schemes; with
    // a port, that port under both
    for any in [
        "http://example.org/",
        "https://example.org/",
        "https://example.org:8443/",
        "http://example.org:443/",
        "http://example.org:1/",
    ] {
        assert!(r.denies(&url(any)), "{any}");
    }
    assert!(port.denies(&url("https://example.org:8443/")));
    assert!(port.denies(&url("http://example.org:8443/")));
    assert!(!port.denies(&url("https://example.org/")));
    assert!(!port.denies(&url("http://example.org:8080/")));
    // A port that is the other scheme's own is written, and is a port
    let other = rule("http://example.org:443");
    assert!(other.denies(&url("https://example.org/")));
    assert!(other.denies(&url("http://example.org:443/")));
    assert!(!other.denies(&url("http://example.org/")));
}

#[test]
fn look_alike_hosts_do_not_match() {
    let r = rule("https://example.com");
    for other in [
        "https://example.com.evil.org/",
        "https://example.com@evil.org/",
        "https://evilexample.com/",
        "https://example.co/",
        "https://example.comm/",
    ] {
        assert!(!r.allows(&url(other)), "{other}");
        assert!(!r.denies(&url(other)), "{other}");
    }
    // Userinfo: never allowed, and no way around a deny
    let with_user = url("https://user:pw@example.com/");
    assert!(!r.allows(&with_user));
    assert!(r.denies(&with_user));
    assert!(!r.allows(&url("https://user@example.com/")));
    let to_evil = url("https://example.com@evil.org/");
    assert!(!rule("https://evil.org").allows(&to_evil));
    assert!(rule("https://evil.org").denies(&to_evil));
}

#[test]
fn a_wildcard_is_subdomains_only() {
    let r = rule("https://*.example.org");
    for sub in [
        "https://en.example.org/",
        "https://a.b.example.org/x",
        "https://EN.Example.ORG/",
        "https://en.example.org./",
    ] {
        assert!(r.allows(&url(sub)), "{sub}");
        assert!(r.denies(&url(sub)), "{sub}");
    }
    for other in [
        "https://example.org/",
        "https://evilexample.org/",
        "https://en.example.org.evil.org/",
        "https://example.org.evil.org/",
        "https://en.example.com/",
        "https://.example.org/",
    ] {
        // (`.example.org` may not parse at all)
        let Ok(other) = Url::parse(other) else {
            continue;
        };
        assert!(!r.allows(&other), "{other}");
        assert!(!r.denies(&other), "{other}");
    }
    assert!(!r.allows(&url("https://x@en.example.org/")));
    // With a path and a port
    let r = rule("http://*.example.org:8080/wiki");
    assert!(r.allows(&url("http://en.example.org:8080/wiki/Rust")));
    assert!(!r.allows(&url("http://en.example.org/wiki/Rust")));
    assert!(!r.allows(&url("http://en.example.org:8080/w")));
}

#[test]
fn a_path_matches_on_a_segment_boundary() {
    for written in ["https://example.org/wiki", "https://example.org/wiki/"] {
        let r = rule(written);
        for under in [
            "https://example.org/wiki",
            "https://example.org/wiki/",
            "https://example.org/wiki/Rust",
            "https://example.org/wiki/a/b?q=1",
            "https://example.org/wiki?q",
            "https://example.org/wiki#top",
        ] {
            assert!(r.allows(&url(under)), "{written} {under}");
            assert!(r.denies(&url(under)), "{written} {under}");
        }
        for outside in [
            "https://example.org/wikipedia",
            "https://example.org/wiki.html",
            "https://example.org/",
            "https://example.org/w/wiki",
            "https://example.org/Wiki",
        ] {
            assert!(!r.allows(&url(outside)), "{written} {outside}");
            assert!(!r.denies(&url(outside)), "{written} {outside}");
        }
    }
    let deep = rule("https://example.org/a/b");
    assert!(deep.allows(&url("https://example.org/a/b/c")));
    assert!(!deep.allows(&url("https://example.org/a")));
    assert!(!deep.allows(&url("https://example.org/a/bc")));
    // No path and `/` are every path
    assert_eq!(rule("https://example.org/"), rule("https://example.org"));
}

/// `Url` resolves dot segments, written or encoded, before a rule sees the
/// path; other encodings it keeps, and the two verdicts read them apart
#[test]
fn dot_segments_and_percent_encoding() {
    let r = rule("https://example.org/wiki");
    for (written, path) in [
        ("https://example.org/wiki/../admin", "/admin"),
        ("https://example.org/wiki/%2e%2e/admin", "/admin"),
        ("https://example.org/wiki/%2E%2E/admin", "/admin"),
        ("https://example.org/wiki/./../admin", "/admin"),
    ] {
        let left = url(written);
        assert_eq!(left.path(), path);
        assert!(!r.allows(&left), "{written}");
        assert!(!r.denies(&left), "{written}");
    }
    for (written, path) in [
        ("https://example.org/x/../wiki/Rust", "/wiki/Rust"),
        ("https://example.org/./wiki", "/wiki"),
    ] {
        let back = url(written);
        assert_eq!(back.path(), path);
        assert!(r.allows(&back), "{written}");
        assert!(r.denies(&back), "{written}");
    }
    // An encoded letter is kept: not allowed as `/wiki`, but denied as it
    let encoded = url("https://example.org/%77iki/Rust");
    assert_eq!(encoded.path(), "/%77iki/Rust");
    assert!(!r.allows(&encoded));
    assert!(r.denies(&encoded));
    // An encoded `/` or `\` under a path rule: a server may resolve
    // `wiki/..%2Fadmin` to `/admin`
    for written in [
        "https://example.org/wiki/..%2Fadmin",
        "https://example.org/wiki/..%2fadmin",
        "https://example.org/wiki/..%5Cadmin",
        "https://example.org/wiki/..;/admin",
        "https://example.org/wiki/%252e%252e/admin",
        "https://example.org/wiki/..%2f..%2fw",
    ] {
        assert!(!r.allows(&url(written)), "{written}");
        assert!(r.denies(&url(written)), "{written}");
    }
    // With no dot segment, an encoded `/`, a `%25` or a `;` is a title
    for written in [
        "https://example.org/wiki/AC%2FDC",
        "https://example.org/wiki/100%25",
        "https://example.org/wiki/a;b",
    ] {
        assert!(r.allows(&url(written)), "{written}");
        assert!(r.denies(&url(written)), "{written}");
    }
    assert!(
        rule("https://example.org").allows(&url("https://example.org/AC%2FDC"))
    );
    // An empty segment: not the rule's path as written, but denied as it
    let doubled = url("https://example.org//wiki");
    assert!(!r.allows(&doubled));
    assert!(r.denies(&doubled));
    // A rule's own encoding is the parser's: a space is `%20` on both sides
    let spaced = rule("https://example.org/a b");
    assert_eq!(spaced.to_string(), "https://example.org/a%20b");
    assert!(spaced.allows(&url("https://example.org/a b/c")));
    assert!(spaced.allows(&url("https://example.org/a%20b/c")));
    // A segment's parameters, and an encoded `%`
    let deep = rule("https://example.org/a/b");
    // (the rule's segments as written: `b;x=1` is not `b`)
    for written in [
        "https://example.org/a;x/b",
        "https://example.org/a/b;x=1/c",
        "https://example.org/a/;/b",
    ] {
        assert!(!deep.allows(&url(written)), "{written}");
        assert!(deep.denies(&url(written)), "{written}");
    }
    assert!(!r.allows(&url("https://example.org/wiki/..;/admin")));
    assert!(!r.allows(&url("https://example.org/wiki/..%252Fadmin")));
    assert!(
        rule("https://example.org").allows(&url("https://example.org/a;x/b"))
    );
    // A rule's own dot segments are refused, not resolved
    for written in [
        "https://example.org/a/../wiki",
        "https://example.org/./wiki",
        "https://example.org/wiki/..",
        "https://example.org/wiki/%2e%2E/x",
        "https://example.org/%2e",
        "https://*.example.org/a/.%2E/b",
    ] {
        assert_eq!(refusal(written), LinkRuleRefusal::DotSegment, "{written}");
    }
    assert_eq!(
        rule("https://example.org/.well-known/x.y").to_string(),
        "https://example.org/.well-known/x.y"
    );
}

#[test]
fn host_spellings() {
    // Case
    let r = rule("https://Example.ORG");
    assert_eq!(r.to_string(), "https://example.org");
    assert!(r.allows(&url("HTTPS://EXAMPLE.ORG/")));
    // One trailing dot, on either side
    assert!(r.allows(&url("https://example.org./x")));
    assert!(r.denies(&url("https://example.org./x")));
    let dotted = rule("https://example.org.");
    assert_eq!(dotted, r);
    assert!(dotted.allows(&url("https://example.org/")));
    assert_eq!(
        rule("https://*.example.org."),
        rule("https://*.example.org")
    );
    // An internationalised name, written both ways
    let idn = rule("https://bücher.example/katalog");
    assert_eq!(idn, rule("https://xn--bcher-kva.example/katalog"));
    assert_eq!(idn.to_string(), "https://xn--bcher-kva.example/katalog");
    assert!(idn.allows(&url("https://bücher.example/katalog/1")));
    assert!(idn.allows(&url("https://BÜCHER.example/katalog/1")));
    assert!(idn.allows(&url("https://xn--bcher-kva.example/katalog")));
    assert!(!idn.allows(&url("https://bucher.example/katalog")));
    // In free text, where the scanner's authority ends at the `ü`
    let links = Links::Defanged {
        allow: vec![idn],
        allow_authors: Vec::new(),
    };
    let text = "Im https://bücher.example/katalog/1, nicht https://bucher.example/katalog.";
    assert_eq!(
        links.rewrite(text, None),
        "Im https://bücher.example/katalog/1, nicht hxxps://bucher[.]example/katalog."
    );
}

#[test]
fn ip_literals() {
    let v4 = rule("http://127.0.0.1:8080");
    assert!(v4.allows(&url("http://127.0.0.1:8080/x")));
    assert!(!v4.allows(&url("http://127.0.0.1/x")));
    assert!(!v4.allows(&url("http://127.0.0.2:8080/x")));
    // `Url` reads every spelling of an IPv4 address as the address
    let v4 = rule("http://127.0.0.1");
    for spelled in ["http://2130706433/", "http://0x7f.1/", "http://127.1/"] {
        assert!(v4.allows(&url(spelled)), "{spelled}");
        assert!(v4.denies(&url(spelled)), "{spelled}");
    }
    assert!(!v4.allows(&url("http://127.0.0.1.example.org/")));

    let v6 = rule("https://[2001:db8::1]/a");
    assert_eq!(v6.to_string(), "https://[2001:db8::1]/a");
    assert!(v6.allows(&url("https://[2001:DB8:0:0:0:0:0:1]/a/b")));
    assert!(v6.denies(&url("https://[2001:db8::1]:443/a")));
    assert!(!v6.allows(&url("https://[2001:db8::2]/a")));
    assert!(!v6.allows(&url("https://[2001:db8::1]:8443/a")));
    // Brackets end a URL in prose, so one in free text is not checked
    // against an allow rule: it is defanged
    let links = Links::Defanged {
        allow: vec![v6],
        allow_authors: Vec::new(),
    };
    assert_eq!(
        links.rewrite("https://[2001:db8::1]/a", None),
        "hxxps://[2001:db8::1]/a"
    );
    // An address is not a subdomain of anything
    assert_eq!(refusal("https://*.0.0.1"), LinkRuleRefusal::Wildcard);
}

#[test]
fn refused_rules_say_why() {
    use LinkRuleRefusal as Why;
    assert_eq!(refusal("https://user@example.org"), Why::Userinfo);
    assert_eq!(refusal("https://user:pw@example.org"), Why::Userinfo);
    assert_eq!(refusal("https://:pw@example.org"), Why::Userinfo);
    // The `@` alone, which the parser drops
    assert_eq!(refusal("https://:@example.org"), Why::Userinfo);
    assert_eq!(refusal("https://@example.org/wiki"), Why::Userinfo);
    assert_eq!(refusal("https://*.@example.org"), Why::Userinfo);
    assert_eq!(
        rule("https://example.org/@user").to_string(),
        "https://example.org/@user"
    );
    // Written the one way: a parser reads each of these as a rule
    for odd in [
        "https:/example.org",
        "https:example.org",
        "http:///example.org",
        "https:///example.org/wiki",
    ] {
        match refusal(odd) {
            Why::Slashes => {}
            // (three slashes: no host where one is read)
            Why::NotAUrl(_) => {}
            other => panic!("{odd}: {other}"),
        }
    }
    assert_eq!(refusal("https:/example.org"), Why::Slashes);
    assert_eq!(refusal("HTTPS:example.org"), Why::Slashes);
    for slashed in [
        "https:\\\\example.org",
        "https://example.org\\wiki",
        "https://example.org/a\\b",
    ] {
        assert_eq!(refusal(slashed), Why::Backslash, "{slashed}");
    }
    for control in [
        "https://exam\tple.org",
        "https://example.org/wi\nki",
        "ht\ttps://example.org",
        "https://example.org/\u{1}",
    ] {
        assert_eq!(refusal(control), Why::Control, "{control}");
    }
    for dotted in [
        "https://example.org..",
        "https://example.org../wiki",
        "https://*.example.org...",
    ] {
        assert_eq!(refusal(dotted), Why::TrailingDots, "{dotted}");
    }
    // A wildcard over a whole top-level name
    for wide in [
        "https://*.com",
        "https://*.com.",
        "http://*.localhost:8080/x",
        "https://*.uk",
    ] {
        assert_eq!(refusal(wide), Why::WildcardSuffix, "{wide}");
    }
    assert_eq!(rule("https://*.co.uk").to_string(), "https://*.co.uk");
    assert_eq!(refusal("https://example.org/?q=1"), Why::Query);
    assert_eq!(refusal("https://example.org?"), Why::Query);
    assert_eq!(refusal("https://example.org/#top"), Why::Fragment);
    assert_eq!(refusal("https://example.org#"), Why::Fragment);
    assert_eq!(refusal("ftp://example.org"), Why::Scheme("ftp".into()));
    assert_eq!(
        refusal("agora://post/7ad26ccd"),
        Why::Scheme("agora".into())
    );
    assert_eq!(refusal("file:///etc/passwd"), Why::Scheme("file".into()));
    assert_eq!(refusal("ftp://*.example.org"), Why::Scheme("ftp".into()));
    for not_a_url in ["example.org", "*.example.org", "", "https://", "/wiki"] {
        assert!(matches!(refusal(not_a_url), Why::NotAUrl(_)), "{not_a_url}");
    }
    // `*.` before a domain name is the only wildcard
    for wild in [
        "https://*",
        "https://*example.org",
        "https://en.*.org",
        "https://example.*",
        "https://*.*.example.org",
        "https://example.org/*",
        "https://example.org/wiki/*",
        "https://*.127.0.0.1",
        "https://*.[::1]",
    ] {
        match refusal(wild) {
            Why::Wildcard => {}
            // (where the parser refuses the host first)
            Why::NotAUrl(_) => {}
            other => panic!("{wild}: {other}"),
        }
    }
    assert_eq!(refusal("https://example.org/wiki/*"), Why::Wildcard);
    assert_eq!(refusal("https://en.*.org"), Why::Wildcard);
    assert_eq!(refusal("https://*.127.0.0.1"), Why::Wildcard);

    // The message names the string and the reason
    let said = "https://example.org/?q=1"
        .parse::<LinkRule>()
        .unwrap_err()
        .to_string();
    assert_eq!(
        said,
        "\"https://example.org/?q=1\" is not a link rule: it has a query \
         (`?…`); a rule matches on scheme, host, port and path"
    );
    let said = "ftp://example.org"
        .parse::<LinkRule>()
        .unwrap_err()
        .to_string();
    assert!(said.contains("only http and https"), "{said}");
    let said = "https://u@example.org".parse::<LinkRule>().unwrap_err();
    assert!(said.to_string().contains("a user or a password"), "{said}");
    let said = "https://example.org#x".parse::<LinkRule>().unwrap_err();
    assert!(said.to_string().contains("a fragment"), "{said}");
    let said = "example.org".parse::<LinkRule>().unwrap_err().to_string();
    assert!(said.contains("is not a URL"), "{said}");
    let said = "https://example.org/*".parse::<LinkRule>().unwrap_err();
    assert!(said.to_string().contains("only as `*.`"), "{said}");
    for (given, says) in [
        (
            "https:example.org",
            "does not begin `http://` or `https://`",
        ),
        ("https://example.org\\x", "a backslash"),
        ("https://example.org/\tx", "a control character"),
        ("https://*.com", "two labels or more"),
        ("https://example.org..", "more than one dot"),
        ("https://example.org/a/../b", "`.` or `..` segment"),
        ("https://@example.org", "the `@` of one"),
    ] {
        let said = given.parse::<LinkRule>().unwrap_err().to_string();
        assert!(said.contains(says), "{said}");
    }
}

#[test]
fn a_rule_round_trips_as_written() {
    for written in [
        "https://example.org",
        "http://example.org",
        "https://example.org:8443",
        "https://example.org/wiki",
        "https://example.org/wiki/",
        "https://*.example.org",
        "http://*.example.org:8080/a/b",
        "http://127.0.0.1:3000",
        "https://[::1]/x",
        "https://xn--bcher-kva.example",
    ] {
        let r = rule(written);
        assert_eq!(r.to_string(), written);
        assert_eq!(rule(&r.to_string()), r);
        assert_eq!(serde_json::to_value(&r).unwrap(), json!(written));
        assert_eq!(
            serde_json::from_value::<LinkRule>(json!(written)).unwrap(),
            r
        );
    }
    // Normalised spellings round-trip to the rule, not the string
    for (written, shown) in [
        (" HTTPS://Example.ORG:443/ ", "https://example.org"),
        ("https://*.Example.org./", "https://*.example.org"),
    ] {
        let r = rule(written);
        assert_eq!(r.to_string(), shown);
        assert_eq!(rule(shown), r);
    }
}

// ── Config and serde ────────────────────────────────────────────────────

#[test]
fn the_default_is_everything_defanged() {
    assert_eq!(
        Links::default(),
        Links::Defanged {
            allow: Vec::new(),
            allow_authors: Vec::new(),
        }
    );
    assert_eq!(DisplayConfig::default().links, Links::default());
    assert_eq!(AgentDisplayConfig::default().links(), &Links::default());
    // Nothing written is the default, and the default is written in full
    let read: AgentDisplayConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(read, AgentDisplayConfig::default());
    assert_eq!(
        serde_json::to_value(AgentDisplayConfig::default()).unwrap(),
        json!({ "links": { "mode": "defanged" } })
    );
}

/// The JSON shape, which is also the TOML table's
/// (`links = { mode = "defanged", allow = ["https://en.wikipedia.org"] }`):
/// this crate has no TOML parser to read one with
#[test]
fn links_round_trip() {
    for (links, wire) in [
        (Links::default(), json!({ "mode": "defanged" })),
        (
            allow(&["https://en.wikipedia.org", "https://*.rust-lang.org/std"]),
            json!({
                "mode": "defanged",
                "allow": ["https://en.wikipedia.org", "https://*.rust-lang.org/std"],
            }),
        ),
        (Links::Raw { deny: Vec::new() }, json!({ "mode": "raw" })),
        (
            deny(&["https://evil.example"]),
            json!({ "mode": "raw", "deny": ["https://evil.example"] }),
        ),
    ] {
        assert_eq!(serde_json::to_value(&links).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Links>(wire.clone()).unwrap(),
            links
        );
        // Under both configs, at the same level
        let table = json!({ "links": wire });
        let common: DisplayConfig =
            serde_json::from_value(table.clone()).unwrap();
        assert_eq!(common.links, links);
        assert_eq!(serde_json::to_value(&common).unwrap(), table);
        let agent: AgentDisplayConfig =
            serde_json::from_value(table.clone()).unwrap();
        assert_eq!(agent.links(), &links);
        assert_eq!(agent, AgentDisplayConfig::from(common));
        assert_eq!(serde_json::to_value(&agent).unwrap(), table);
        // And from text
        let text = serde_json::to_string(&agent).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentDisplayConfig>(&text).unwrap(),
            agent
        );
    }
    // An empty list reads as none
    assert_eq!(
        serde_json::from_value::<Links>(json!({ "mode": "raw", "deny": [] }))
            .unwrap(),
        Links::Raw { deny: Vec::new() }
    );
}

/// A typo in a policy is an error, not the default
#[test]
fn unknown_keys_are_refused() {
    let err = |value: serde_json::Value| {
        let from_value =
            serde_json::from_value::<AgentDisplayConfig>(value.clone())
                .expect_err("refused")
                .to_string();
        // The same from text, which takes serde's other path
        serde_json::from_str::<AgentDisplayConfig>(&value.to_string())
            .expect_err("refused");
        serde_json::from_value::<DisplayConfig>(value).expect_err("refused");
        from_value
    };
    let said = err(json!({ "linkz": { "mode": "raw" } }));
    assert!(said.contains("unknown field `linkz`"), "{said}");
    let said = err(json!({ "links": { "mode": "raw" }, "extra": 1 }));
    assert!(said.contains("unknown field `extra`"), "{said}");
    // Inside the policy: a misspelt list, the other mode's list, no mode,
    // a mode there is not
    let said = err(json!({ "links": { "mode": "defanged", "alow": [] } }));
    assert!(said.contains("unknown field `alow`"), "{said}");
    let said = err(json!({ "links": { "mode": "defanged", "deny": [] } }));
    assert!(said.contains("unknown field `deny`"), "{said}");
    let said = err(json!({ "links": { "mode": "raw", "allow": [] } }));
    assert!(said.contains("unknown field `allow`"), "{said}");
    let said = err(json!({ "links": {} }));
    assert!(said.contains("mode"), "{said}");
    let said = err(json!({ "links": { "mode": "off" } }));
    assert!(said.contains("unknown variant `off`"), "{said}");
    // A rule that is refused says why through the config's error
    let said = err(json!({
        "links": { "mode": "raw", "deny": ["https://example.org/?q=1"] }
    }));
    assert!(said.contains("it has a query"), "{said}");
    let said = err(json!({ "links": { "mode": "raw", "deny": [7] } }));
    assert!(said.contains("string"), "{said}");
}

#[cfg(feature = "schemars")]
#[test]
fn schemas_are_inline() {
    for schema in [
        schemars::schema_for!(AgentDisplayConfig),
        schemars::schema_for!(DisplayConfig),
        schemars::schema_for!(Links),
        schemars::schema_for!(LinkRule),
    ] {
        let text = serde_json::to_string(&schema).unwrap();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{text}");
    }
    let text =
        serde_json::to_string(&schemars::schema_for!(AgentDisplayConfig))
            .unwrap();
    for word in ["links", "mode", "defanged", "raw", "allow", "deny"] {
        assert!(text.contains(word), "{word}: {text}");
    }
}

// ── Layer 5b: what the review of layer 5 reproduced ─────────────────────

/// Unicode punctuation, a superscript, `~` or a character a parser
/// ignores after a host ends it, and is not the URL's at its end
#[test]
fn a_mark_after_a_host_ends_it() {
    let links = allow(&["https://docs.rs"]);
    for text in [
        "“https://docs.rs”",
        "‘https://docs.rs’",
        "«https://docs.rs»",
        "（https://docs.rs）",
        "https://docs.rs、次",
        "https://docs.rs，次",
        "https://docs.rs—the docs site",
        "https://docs.rs–x",
        "https://docs.rs…",
        "https://docs.rs¹",
        "https://docs.rs⁽¹⁾",
        "~~https://docs.rs~~",
        "https://docs.rs~",
        // In a path, a possible end: each reading is checked
        "https://docs.rs/serde”",
        "https://docs.rs/“serde”/x",
    ] {
        let out = links.rewrite(text, None);
        assert!(matches!(out, Cow::Borrowed(_)), "{text:?} gave {out:?}");
    }
    let links = deny(&["https://evil.com"]);
    for text in [
        "“https://evil.com”",
        "https://evil.com—x",
        "（https://evil.com）",
        "https://evil.com、次",
        "https://evil.com¹",
        "https://evil.com~",
        "https://evil.com\u{200b}x",
        "https://evil.com\u{ad}x",
    ] {
        let out = links.rewrite(text, None);
        assert!(out.contains("hxxps://evil[.]com"), "{text:?} gave {out:?}");
    }
    // A mark a parser makes a dot of is a dot: what follows is checked
    let links = allow(&["https://docs.rs"]);
    for text in [
        "https://docs.rs。evil.com/x",
        "https://docs.rs．evil.com/x",
        "https://docs.rs”.evil.com/x",
        "https://docs.rs․evil.com/x",
    ] {
        let out = links.rewrite(text, None);
        assert!(out.starts_with("hxxps://"), "{text:?} gave {out:?}");
    }
    // A letter or a digit is the host's: an internationalised name goes on
    assert_eq!(
        links.rewrite("https://docs.rsé", None),
        "hxxps://docs[.]rsé"
    );
}

/// Punctuation no host name has ends an authority, and a host with it is
/// allowed by no rule
#[test]
fn punctuation_in_an_authority_ends_it() {
    let links = deny(&["https://evil.com"]);
    for text in ["https://evil.com,b", "https://evil.com),b"] {
        let out = links.rewrite(text, None);
        assert!(out.starts_with("hxxps://"), "{text:?} gave {out:?}");
    }
    let links = allow(&["https://*.allowed.example"]);
    for text in [
        "https://evil.com,.allowed.example/x",
        "https://evil.com!.allowed.example/x",
        "https://evil.com;.allowed.example/x",
        "https://evil.com~.allowed.example",
        "https://evil.com'.allowed.example/x",
    ] {
        let out = links.rewrite(text, None);
        assert!(out.starts_with("hxxps://"), "{text:?} gave {out:?}");
    }
    let wild = rule("https://*.allowed.example");
    for host in ["evil.com,.allowed.example", "a~b.allowed.example"] {
        assert!(
            !wild.allows(&url(&["https://", host, "/"].concat())),
            "{host}"
        );
    }
    let links = allow(&["https://docs.rs"]);
    for text in [
        "https://docs.rs,b",
        "https://docs.rs;b",
        "https://docs.rs!x",
    ] {
        let out = links.rewrite(text, None);
        assert!(matches!(out, Cow::Borrowed(_)), "{text:?} gave {out:?}");
    }
}

/// A `[` after a host is not an IPv6 address or a user's: an end
#[test]
fn a_bracket_after_a_host_ends_it() {
    let links = allow(&["https://docs.rs"]);
    for text in [
        "https://docs.rs[1]",
        "https://docs.rs[^1]",
        "[x](https://docs.rs),[y](https://docs.rs)",
    ] {
        let out = links.rewrite(text, None);
        assert!(matches!(out, Cow::Borrowed(_)), "{text:?} gave {out:?}");
    }
    // An address and a user before one are as they were
    let v6 = allow(&["https://[::1]"]);
    assert_eq!(v6.rewrite("https://[::1]/", None), "hxxps://[::1]/");
    assert_eq!(
        v6.rewrite("https://user@[::1]/", None),
        "hxxps://user@[::1]/"
    );
    assert_eq!(
        links.rewrite("https://docs.rs[x@evil.com/", None),
        "hxxps://docs[.]rs[x@evil.com/"
    );
}

/// Readings that cannot change a URL's host are not counted against
/// `MAX_URL_ENDS`
#[test]
fn minified_json_leaves_an_allowed_url() {
    let pairs = |n: usize| {
        (0..n)
            .map(|k| format!(",\"k{k}\":\"v\""))
            .collect::<String>()
    };
    for links in [
        allow(&["https://docs.rs"]),
        allow(&["https://docs.rs/serde"]),
        deny(&["https://evil.com"]),
        deny(&["https://docs.rs/tokio"]),
    ] {
        for url in ["https://docs.rs/serde/latest", "https://docs.rs"] {
            for n in [6, 9, 40, 120] {
                if url == "https://docs.rs"
                    && links == allow(&["https://docs.rs/serde"])
                {
                    continue;
                }
                let text = format!("{{\"url\":\"{url}\"{}}}", pairs(n));
                let out = links.rewrite(&text, None);
                assert!(
                    matches!(out, Cow::Borrowed(_)),
                    "{links:?} {n}: {out:?}"
                );
            }
        }
    }
    // With dotted values after it, under a rule with a path
    let text = format!(
        "{{\"url\":\"https://docs.rs/serde/1.0\"{}}}",
        (0..40)
            .map(|k| format!(",\"v{k}\":\"1.{k}.0\""))
            .collect::<String>()
    );
    let out = allow(&["https://docs.rs/serde"]).rewrite(&text, None);
    assert!(matches!(out, Cow::Borrowed(_)), "{out:?}");
    // A URL that ends at a rule's last segment, with more after the quote:
    // read to the next space, its segment is `serde",…`, not the rule's
    let out = allow(&["https://docs.rs/serde"])
        .rewrite("{\"u\":\"https://docs.rs/serde\",\"k\":\"v\"}", None);
    assert!(out.contains("hxxps://"), "{out:?}");
    // A path that leaves the rule's after the first reading still counts
    let out = allow(&["https://docs.rs/serde"])
        .rewrite("{\"u\":\"https://docs.rs/serde\",\"k\":\"/../../x\"}", None);
    assert!(out.contains("hxxps://"), "{out:?}");
}

/// A slashless scheme is a URL only before a domain name: code and prose
/// that are not links are left as written
#[test]
fn a_slashless_scheme_needs_a_domain() {
    for code in [
        "HTTP:200 OK",
        "x=http:ok",
        "http:Agent",
        "#[http:get]",
        "{http:1.1}",
        "ENV HTTP_PROXY=http:proxy:3128",
        "export https_proxy=https:p",
        "data:text/html,https:x",
        "http:\u{a0}x",
        "https:\u{3000}x",
        "https:\u{2003}x",
        "https:/\u{a0}x",
    ] {
        let out = defang(code);
        assert!(matches!(out, Cow::Borrowed(_)), "{code:?} gave {out:?}");
        assert_eq!(fetchable(code), None, "{code:?}");
    }
    for (text, want) in [
        ("https:evil.com/x", "hxxps:evil[.]com/x"),
        ("https:a.b.co", "hxxps:a[.]b[.]co"),
        ("http:u:p@evil.com", "hxxp:u:p@evil[.]com"),
        ("http:(evil.example)", "hxxp:(evil.example)"),
        ("http:EVIL.COM:8080/x", "hxxp:EVIL[.]COM:8080/x"),
    ] {
        assert_eq!(defang(text), want);
    }
    // Not seen: the documented limit
    for unseen in super::oracle::UNSEEN {
        assert!(matches!(defang(unseen), Cow::Borrowed(_)), "{unseen:?}");
        assert_eq!(fetchable(unseen), None, "{unseen:?}");
    }
}

// ── Ordinary writing: what the policy must not break ────────────────────

/// `fixtures/ordinary-writing.tsv`, one case a line: `(tag, text)`
fn ordinary_writing() -> impl Iterator<Item = (&'static str, &'static str)> {
    include_str!("fixtures/ordinary-writing.tsv")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split_once('\t')
                .unwrap_or_else(|| panic!("no tag: {l:?}"))
        })
}

/// The allow profile of the ordinary-writing fixture
fn ordinary_allow() -> Links {
    allow(&[
        "https://docs.rs",
        "https://en.wikipedia.org/wiki",
        "https://ja.wikipedia.org/wiki",
        "https://github.com/mdegans",
        "https://github.com/claudeopusagora",
        "https://doi.org",
        "https://example.org",
    ])
}

/// Links on allowed sites, written the ways people and models write them,
/// are left as written; the few that leave a rule are defanged; and what
/// the policy still breaks (`known`) is pinned, so a fix shows up here
#[test]
fn ordinary_writing_is_left() {
    let links = ordinary_allow();
    let mut failures = Vec::new();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for (tag, text) in ordinary_writing().filter(|(tag, _)| *tag != "code") {
        *counts.entry(tag).or_default() += 1;
        let out = links.rewrite(text, None);
        let ok = match tag {
            "left" => out == text,
            "defang" | "known" => out != text,
            other => panic!("unknown tag {other:?}"),
        };
        if !ok {
            failures.push(format!("{tag}\t{text:?}\n  gave\t{out:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(counts["left"] > 300, "{counts:?}");
}

/// Code and prose that are not links come out of the default config
/// byte-identical
#[test]
fn what_is_not_a_link_is_untouched() {
    let mut failures = Vec::new();
    for (_, text) in ordinary_writing().filter(|(tag, _)| *tag == "code") {
        let out = Links::default().rewrite(text, Some(&site()));
        if out != text {
            failures.push(format!("{text:?} gave {out:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
// ── The policy as the server tools' domain filters ──────────────────────

fn sent(domains: &ToolDomains) -> &[String] {
    match domains.filter() {
        DomainFilter::Allowed(list) | DomainFilter::Blocked(list) => list,
        other => panic!("nothing sent: {other:?}"),
    }
}

/// `(rule, change)` per note, the rule as written, in the notes' order
fn notes(domains: &ToolDomains) -> Vec<(String, DomainChange)> {
    domains
        .notes()
        .iter()
        .map(|n| (n.rule.to_string(), n.change.clone()))
        .collect()
}

fn widened(entry: &str, by: &[Widening]) -> DomainChange {
    DomainChange::Widened {
        entry: entry.to_string(),
        by: by.to_vec(),
    }
}

fn not_sent(why: NotSent) -> DomainChange {
    DomainChange::NotSent(why)
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn both(links: &Links) -> [ToolDomains; 2] {
    [links.fetch_domains(), links.search_domains()]
}

/// Anthropic's matching, written from its docs and not from the code under
/// test: the entry's domain or a subdomain of it; for web search, a URL
/// path that begins with the entry's path; for web fetch, an entry with a
/// path matches nothing. A trailing dot on the URL's host is taken off: the
/// docs do not say, and taking it off is what makes the entry cover the
/// most (so the over-reach and over-block checks below are the stricter)
fn reaches(tool: ServerTool, entry: &str, url: &Url) -> bool {
    let (domain, path) = entry.split_at(entry.find('/').unwrap_or(entry.len()));
    if tool == ServerTool::WebFetch && !path.is_empty() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let on_domain = host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|sub| sub.ends_with('.'));
    on_domain && url.path().starts_with(path)
}

/// The documented entry grammar, written separately from the code under
/// test: no scheme, no `*` in the domain, a domain name of two LDH labels
/// or more in lower-case ASCII, and for web fetch no path. A web search
/// path is `/`-separated non-empty segments of unreserved characters
fn in_grammar(tool: ServerTool, entry: &str) -> bool {
    let (domain, path) = entry.split_at(entry.find('/').unwrap_or(entry.len()));
    let labels: Vec<&str> = domain.split('.').collect();
    let ldh = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
            })
    };
    let path_ok = match tool {
        ServerTool::WebFetch => path.is_empty(),
        ServerTool::WebSearch => {
            path.is_empty()
                || path.split('/').skip(1).all(|s| {
                    !s.is_empty()
                        && s.bytes().all(|b| {
                            b.is_ascii_alphanumeric() || b"-._~".contains(&b)
                        })
                })
        }
    };
    !entry.contains("://")
        && !domain.contains('*')
        && domain.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(ldh)
        && path_ok
}

#[test]
fn fetch_allow_sends_each_domain_sorted_once() {
    let links = allow(&[
        "https://docs.rs",
        "http://example.org",
        "https://docs.rs/serde",
        "https://Example.ORG.",
    ]);
    let domains = links.fetch_domains();
    assert_eq!(domains.tool(), ServerTool::WebFetch);
    assert_eq!(sent(&domains), strings(&["docs.rs", "example.org"]));
    assert_eq!(
        notes(&domains),
        [(
            "https://docs.rs/serde".to_string(),
            widened("docs.rs", &[Widening::Path])
        )]
    );
}

#[test]
fn fetch_allow_widens_path_port_and_apex() {
    let links = allow(&[
        "https://en.wikipedia.org/wiki",
        "https://example.net:8443/a",
        "https://*.example.org",
    ]);
    let domains = links.fetch_domains();
    assert_eq!(
        sent(&domains),
        strings(&["en.wikipedia.org", "example.net", "example.org"])
    );
    assert_eq!(
        notes(&domains),
        [
            (
                "https://en.wikipedia.org/wiki".to_string(),
                widened("en.wikipedia.org", &[Widening::Path])
            ),
            (
                "https://example.net:8443/a".to_string(),
                widened("example.net", &[Widening::Path, Widening::Port])
            ),
            (
                "https://*.example.org".to_string(),
                widened("example.org", &[Widening::Apex])
            ),
        ]
    );
    // The rule itself does not allow the apex, which the entry covers
    assert!(
        !rule("https://*.example.org").allows(&url("https://example.org/"))
    );
}

/// Notes are in the policy's order, not the entries' (which are sorted)
#[test]
fn notes_follow_the_policys_order() {
    let links = allow(&[
        "https://zz.example/a",
        "http://localhost",
        "https://aa.example:8443",
        "https://127.0.0.1",
    ]);
    let domains = links.fetch_domains();
    assert_eq!(sent(&domains), strings(&["aa.example", "zz.example"]));
    let order: Vec<String> =
        notes(&domains).into_iter().map(|(r, _)| r).collect();
    assert_eq!(
        order,
        strings(&[
            "https://zz.example/a",
            "http://localhost",
            "https://aa.example:8443",
            "https://127.0.0.1",
        ])
    );
}

#[test]
fn a_wildcard_with_its_whole_apex_ruled_is_not_noted() {
    let links = allow(&["https://*.example.org", "https://example.org"]);
    for domains in both(&links) {
        assert_eq!(sent(&domains), strings(&["example.org"]));
        assert_eq!(notes(&domains), []);
    }
}

/// The entry for `*.example.org` reaches all of `example.org` itself; a
/// rule for only part of the apex (a path, a port) does not cover that
#[test]
fn a_wildcard_beside_part_of_its_apex_is_noted() {
    for apex in ["https://example.org/a", "https://example.org:8443"] {
        let links = allow(&["https://*.example.org", apex]);
        for domains in both(&links) {
            let apex_note = domains
                .notes()
                .iter()
                .find(|n| n.rule.to_string() == "https://*.example.org");
            assert_eq!(
                apex_note.map(|n| &n.change),
                Some(&widened("example.org", &[Widening::Apex])),
                "{apex}, {}",
                domains.tool()
            );
        }
    }
}

#[test]
fn an_empty_allow_list_is_no_filter() {
    for domains in both(&Links::default()) {
        assert_eq!(domains.filter(), &DomainFilter::Unfiltered);
        assert_eq!(notes(&domains), []);
        assert_eq!(
            domains.offer(),
            ToolOffer::Offer {
                allowed_domains: None,
                blocked_domains: None
            }
        );
    }
    assert_eq!(
        Links::default().fetch_domains().to_string(),
        "web fetch: no allow rule: links to other sites are shown defanged; \
         the tool is as configured\n"
    );
}

#[test]
fn hosts_no_tool_takes_are_not_sent() {
    let links = allow(&[
        "https://127.0.0.1",
        "http://[::1]:8080",
        "http://localhost",
        "https://a_b.example.org",
        "https://bücher.example",
    ]);
    for domains in both(&links) {
        // The internationalised name is sent as the parse wrote it, ASCII
        assert_eq!(sent(&domains), strings(&["xn--bcher-kva.example"]));
        assert_eq!(
            notes(&domains),
            [
                (
                    "https://127.0.0.1".to_string(),
                    not_sent(NotSent::IpAddress)
                ),
                (
                    "http://[::1]:8080".to_string(),
                    not_sent(NotSent::IpAddress)
                ),
                (
                    "http://localhost".to_string(),
                    not_sent(NotSent::SingleLabel)
                ),
                (
                    "https://a_b.example.org".to_string(),
                    not_sent(NotSent::NotHostName)
                ),
            ]
        );
    }
}

/// The cliff: an empty allow list offers the tool unfiltered, but one of
/// only unsendable rules withholds it
#[test]
fn allow_rules_none_of_which_can_be_sent_withhold_the_tool() {
    let links = allow(&["https://127.0.0.1", "http://localhost"]);
    for domains in both(&links) {
        assert_eq!(domains.filter(), &DomainFilter::Withhold);
        assert_eq!(domains.offer(), ToolOffer::Withhold);
        assert_eq!(domains.notes().len(), 2);
    }
    assert_eq!(
        links.fetch_domains().to_string(),
        "web fetch: withheld: no allow rule can be sent, and unfiltered it \
         would reach anything\n\
         \x20 not sent: https://127.0.0.1: an IP address; the tool takes \
         domain names (shown as written; the tool will not reach it)\n\
         \x20 not sent: http://localhost: a single label; the tool takes \
         domain names of two or more (shown as written; the tool will not \
         reach it)\n"
    );
}

#[test]
fn the_offer_carries_the_list() {
    let offer = allow(&["https://docs.rs/serde"]).search_domains().offer();
    assert_eq!(
        offer,
        ToolOffer::Offer {
            allowed_domains: Some(vec!["docs.rs/serde".into()]),
            blocked_domains: None,
        }
    );
    let offer = deny(&["https://evil.example"]).fetch_domains().offer();
    assert_eq!(
        offer,
        ToolOffer::Offer {
            allowed_domains: None,
            blocked_domains: Some(vec!["evil.example".into()]),
        }
    );
}

#[test]
fn deny_sends_only_whole_hosts() {
    let links = deny(&[
        "https://evil.example",
        "https://legit.example/evil",
        "https://ports.example:8080",
        "https://*.tracker.example",
        "https://evil.example/also",
        "http://*.sub.evil.example",
        "https://legit.example/a%20b",
        "https://*.tracker.example/x",
    ]);
    for domains in both(&links) {
        assert_eq!(
            sent(&domains),
            strings(&["evil.example"]),
            "{}",
            domains.tool()
        );
        assert_eq!(
            notes(&domains),
            [
                (
                    "https://legit.example/evil".to_string(),
                    not_sent(NotSent::DenyPath)
                ),
                (
                    "https://ports.example:8080".to_string(),
                    not_sent(NotSent::DenyPort)
                ),
                (
                    "https://*.tracker.example".to_string(),
                    not_sent(NotSent::DenyApex)
                ),
                (
                    "https://legit.example/a%20b".to_string(),
                    not_sent(NotSent::DenyPath)
                ),
                (
                    "https://*.tracker.example/x".to_string(),
                    not_sent(NotSent::DenyPath)
                ),
            ]
        );
    }
    // The entry blocks a subdomain the rule does not deny (said in the
    // report's line on subdomains)
    assert!(
        !rule("https://evil.example")
            .denies(&url("https://blog.evil.example/"))
    );
}

#[test]
fn raw_with_nothing_sendable_is_no_filter() {
    let none = deny(&[]).fetch_domains();
    assert_eq!(none.filter(), &DomainFilter::Unfiltered);
    assert_eq!(
        none.to_string(),
        "web fetch: no deny rule; the tool is as configured\n"
    );
    let domains = deny(&["https://legit.example/evil"]).search_domains();
    assert_eq!(domains.filter(), &DomainFilter::Unfiltered);
    assert_eq!(
        domains.to_string(),
        "web search: no deny rule can be sent; the tool is as configured\n\
         \x20 not sent: https://legit.example/evil: it has a path, and \
         blocking the domain would block its other pages too (the tool can \
         still reach it; the display still defangs it)\n"
    );
}

#[test]
fn search_allow_sends_paths_as_prefixes() {
    let links = allow(&[
        "https://example.org/blog/",
        "https://docs.rs/serde/latest",
        "https://en.wikipedia.org/wiki/AC%2FDC",
        "https://*.example.net/a",
    ]);
    let domains = links.search_domains();
    assert_eq!(domains.tool(), ServerTool::WebSearch);
    assert_eq!(
        sent(&domains),
        strings(&[
            "docs.rs/serde/latest",
            "en.wikipedia.org",
            "example.net/a",
            "example.org/blog",
        ])
    );
    assert_eq!(
        notes(&domains),
        [
            (
                "https://example.org/blog/".to_string(),
                widened("example.org/blog", &[Widening::PathPrefix])
            ),
            (
                "https://docs.rs/serde/latest".to_string(),
                widened("docs.rs/serde/latest", &[Widening::PathPrefix])
            ),
            (
                "https://en.wikipedia.org/wiki/AC%2FDC".to_string(),
                widened("en.wikipedia.org", &[Widening::Path])
            ),
            (
                "https://*.example.net/a".to_string(),
                widened(
                    "example.net/a",
                    &[Widening::PathPrefix, Widening::Apex]
                )
            ),
        ]
    );
    // Why a path prefix is a widening: the rule takes whole segments
    let blog = rule("https://example.org/blog");
    assert!(blog.allows(&url("https://example.org/blog/1")));
    assert!(!blog.allows(&url("https://example.org/blogger")));
    let blogger = url("https://example.org/blogger");
    assert!(reaches(ServerTool::WebSearch, "example.org/blog", &blogger));
    assert!(!reaches(ServerTool::WebFetch, "example.org/blog", &blogger));
}

/// One example of each kind, as an operator reads it
#[test]
fn the_report_reads_plainly() {
    let links = allow(&[
        "https://en.wikipedia.org/wiki",
        "https://*.example.org",
        "https://docs.rs",
        "https://example.net:8443",
        "https://127.0.0.1",
    ]);
    assert_eq!(
        links.fetch_domains().to_string(),
        "web fetch: allowed_domains, 4\n\
         \x20 docs.rs\n\
         \x20 en.wikipedia.org\n\
         \x20 example.net\n\
         \x20 example.org\n\
         \x20 each also reaches its subdomains, over http and https, on \
         any port, and every path: web fetch matches on the domain \
         alone\n\
         \x20 widened: https://en.wikipedia.org/wiki as en.wikipedia.org, \
         with every path\n\
         \x20 widened: https://*.example.org as example.org, with \
         example.org itself\n\
         \x20 widened: https://example.net:8443 as example.net, with every \
         port\n\
         \x20 not sent: https://127.0.0.1: an IP address; the tool takes \
         domain names (shown as written; the tool will not reach it)\n"
    );
    assert_eq!(
        links.search_domains().to_string(),
        "web search: allowed_domains, 4\n\
         \x20 docs.rs\n\
         \x20 en.wikipedia.org/wiki\n\
         \x20 example.net\n\
         \x20 example.org\n\
         \x20 each also reaches its subdomains, over http and https, on \
         any port, and every path that begins with its own\n\
         \x20 widened: https://en.wikipedia.org/wiki as \
         en.wikipedia.org/wiki, with every path that begins with /wiki \
         (/wikix too)\n\
         \x20 widened: https://*.example.org as example.org, with \
         example.org itself\n\
         \x20 widened: https://example.net:8443 as example.net, with every \
         port\n\
         \x20 not sent: https://127.0.0.1: an IP address; the tool takes \
         domain names (shown as written; the tool will not reach it)\n"
    );
    assert_eq!(
        deny(&["https://evil.example", "https://legit.example/evil"])
            .search_domains()
            .to_string(),
        "web search: blocked_domains, 1\n\
         \x20 evil.example\n\
         \x20 each also blocks its subdomains, over http and https, which \
         the rule for it does not deny\n\
         \x20 not sent: https://legit.example/evil: it has a path, and \
         blocking the domain would block its other pages too (the tool can \
         still reach it; the display still defangs it)\n"
    );
}

/// Rules of every odd kind the parse lets through: whatever is emitted is
/// in the documented grammar, for both tools, allowed and blocked
#[test]
fn every_entry_is_in_the_documented_grammar() {
    let long_label = ["https://", &"a".repeat(64), ".example"].concat();
    let long_name =
        ["https://", &["abcdefghi."; 26].concat(), "example"].concat();
    let written = [
        "https://EXAMPLE.org.",
        "https://bücher.example/straße",
        "https://a_b.example.org",
        "https://-bad.example.org",
        "https://bad-.example.org",
        "https://ex--ample.org",
        "https://xn--bcher-kva.example",
        "https://127.0.0.1/x",
        "http://[::1]",
        "http://localhost:8080",
        "https://example.org/a%20b",
        "https://example.org/~user/x.y",
        "https://example.org//",
        "https://example.org/a//b",
        "https://example.org/a;b/c",
        "https://example.org/A!$&'()+,=@:/x",
        "https://*.co.example/path",
        "https://example.org:443/a",
        "https://1.example",
        &long_label,
        &long_name,
    ];
    let rules: Vec<LinkRule> =
        written.iter().filter_map(|w| w.parse().ok()).collect();
    assert!(rules.len() > 15, "{} parsed", rules.len());
    for links in [
        Links::Defanged {
            allow: rules.clone(),
            allow_authors: Vec::new(),
        },
        Links::Raw {
            deny: rules.clone(),
        },
    ] {
        for domains in both(&links) {
            for entry in sent(&domains) {
                assert!(
                    in_grammar(domains.tool(), entry),
                    "{}: {entry:?}",
                    domains.tool()
                );
            }
            // Every rule is sent as it is, or noted
            let noted = domains.notes().len();
            assert!(noted > 0 && noted <= rules.len());
        }
    }
}

/// URLs to try rules on: hosts at, over and under the rules' (with a
/// trailing dot too), schemes, ports and paths in and out of theirs
fn domain_probe_urls() -> Vec<Url> {
    let hosts = [
        "example.org",
        "example.org.",
        "docs.example.org",
        "docs.example.org.",
        "x.docs.example.org",
        "a.b.example.org",
        "example.org.evil.net",
        "notexample.org",
        "en.wikipedia.org",
        "legit.example",
        "legit.example.",
        "sub.legit.example",
    ];
    let paths = [
        "/",
        "/a",
        "/a/b",
        "/ab",
        "/zzz",
        "/wiki",
        "/wiki/",
        "/wiki/Rust",
        "/wikipedia",
        "/blog",
        "/blog/1",
        "/blogger",
        "/evil",
        "/evil/x",
        "/evilness",
        "/a/b/c",
    ];
    let mut urls = Vec::new();
    for scheme in ["http", "https"] {
        for host in hosts {
            for port in ["", ":8443"] {
                for path in paths {
                    urls.push(url(&[scheme, "://", host, port, path].concat()));
                }
            }
        }
    }
    urls
}

/// Whatever a rule allows, the tool reaches: every URL an allow rule
/// leaves as written is matched by an entry sent, under Anthropic's rules
#[test]
fn every_allowed_url_is_reached_by_an_entry() {
    let rules = [
        "https://example.org",
        "http://example.org/wiki",
        "https://*.example.org",
        "https://*.example.org/blog",
        "https://en.wikipedia.org/wiki",
        "https://en.wikipedia.org:8443/wiki/Rust",
        "https://legit.example/blog/",
    ];
    let urls = domain_probe_urls();
    for written in rules {
        let rule = rule(written);
        for domains in both(&allow(&[written])) {
            let entries = sent(&domains);
            for url in urls.iter().filter(|url| rule.allows(url)) {
                assert!(
                    entries.iter().any(|e| reaches(domains.tool(), e, url)),
                    "{written}: {url} allowed, but {} sends {entries:?}",
                    domains.tool()
                );
            }
        }
    }
}

/// The entry one rule alone is sent as
fn entry_of(rule: &LinkRule, tool: ServerTool) -> Option<String> {
    let links = Links::Defanged {
        allow: vec![rule.clone()],
        allow_authors: Vec::new(),
    };
    let domains = match tool {
        ServerTool::WebFetch => links.fetch_domains(),
        ServerTool::WebSearch => links.search_domains(),
    };
    match domains.filter() {
        DomainFilter::Allowed(list) => list.first().cloned(),
        _ => None,
    }
}

/// Whether `url`, which `entry` reaches and no rule allows, is said: on a
/// subdomain of the entry's domain, over another scheme or on another port
/// (said once, in the report's line), or reached by a rule's note for this
/// entry (its path or apex)
fn explained(
    domains: &ToolDomains,
    rules: &[LinkRule],
    entry: &str,
    url: &Url,
) -> bool {
    let domain = entry.split('/').next().unwrap_or(entry);
    let host = url.host_str().unwrap_or_default();
    if host.strip_suffix('.').unwrap_or(host) != domain {
        return true;
    }
    // Allowed but for its scheme or port
    for scheme in ["http", "https"] {
        for port in [None, url.port()] {
            let mut other = url.clone();
            other.set_scheme(scheme).unwrap();
            other.set_port(port).unwrap();
            if rules.iter().any(|r| r.allows(&other)) {
                return true;
            }
        }
    }
    rules.iter().any(|rule| {
        if entry_of(rule, domains.tool()).as_deref() != Some(entry) {
            return false;
        }
        let by: &[Widening] = domains
            .notes()
            .iter()
            .find_map(|n| match &n.change {
                DomainChange::Widened { by, .. } if n.rule == *rule => {
                    Some(&by[..])
                }
                _ => None,
            })
            .unwrap_or_default();
        let host_ok = !rule.wildcard || by.contains(&Widening::Apex);
        let path_ok = by.contains(&Widening::Path)
            || by.contains(&Widening::PathPrefix)
            || {
                // The rule on its own host, with the URL's path
                let mut probe = rule.url.clone();
                if rule.wildcard {
                    let sub = ["w.", domain].concat();
                    probe.set_host(Some(&sub)).unwrap();
                }
                probe.set_path(url.path());
                rule.allows(&probe)
            };
        host_ok && path_ok
    })
}

/// Over two and three rules on one host and on a parent and a child
/// domain: every URL an entry reaches and no rule allows is said in the
/// report. (It failed on a wildcard beside a path rule for its apex,
/// whose apex note was dropped.)
#[test]
fn every_url_reached_beyond_the_rules_is_said() {
    let mut pool = Vec::new();
    for host in ["example.org", "docs.example.org"] {
        for shape in [
            ["https://", host].concat(),
            ["http://", host, "/a"].concat(),
            ["https://*.", host].concat(),
            ["https://*.", host, "/a"].concat(),
            ["https://", host, ":8443"].concat(),
            ["https://", host, "/a/b"].concat(),
        ] {
            pool.push(rule(&shape));
        }
    }
    let mut sets: Vec<Vec<LinkRule>> = Vec::new();
    for i in 0..pool.len() {
        for j in i + 1..pool.len() {
            sets.push(vec![pool[i].clone(), pool[j].clone()]);
            for k in j + 1..pool.len() {
                sets.push(vec![
                    pool[i].clone(),
                    pool[j].clone(),
                    pool[k].clone(),
                ]);
            }
        }
    }
    let urls = domain_probe_urls();
    let mut failures = Vec::new();
    for rules in &sets {
        let links = Links::Defanged {
            allow: rules.clone(),
            allow_authors: Vec::new(),
        };
        for domains in both(&links) {
            for entry in sent(&domains) {
                for url in urls
                    .iter()
                    .filter(|url| reaches(domains.tool(), entry, url))
                {
                    if !rules.iter().any(|r| r.allows(url))
                        && !explained(&domains, rules, entry, url)
                    {
                        let rules: Vec<String> =
                            rules.iter().map(|r| r.to_string()).collect();
                        failures.push(format!(
                            "{}: {rules:?} sends {entry}, which reaches {url}, unsaid",
                            domains.tool()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} unsaid, e.g.\n{}",
        failures.len(),
        failures[..failures.len().min(10)].join("\n")
    );
}

/// Never more blocked than the rules say on an entry's own domain. Its
/// subdomains are left out on purpose: an entry blocks them though
/// `LinkRule::denies` takes the rule's host alone, a widening accepted and
/// said once in the report, not per URL
#[test]
fn no_entry_blocks_what_no_rule_denies() {
    let written = [
        "https://example.org/wiki",
        "https://*.example.org",
        "https://legit.example/evil",
        "https://legit.example/blog/",
        "https://en.wikipedia.org",
        "https://example.org:8443/a",
        "https://docs.example.org",
    ];
    let links = deny(&written);
    let rules: Vec<LinkRule> = written.iter().map(|r| rule(r)).collect();
    let urls = domain_probe_urls();
    for domains in both(&links) {
        for entry in sent(&domains) {
            let domain = entry.split('/').next().unwrap_or(entry);
            for url in urls
                .iter()
                .filter(|url| {
                    url.host_str().map(|h| h.strip_suffix('.').unwrap_or(h))
                        == Some(domain)
                })
                .filter(|url| reaches(domains.tool(), entry, url))
            {
                assert!(
                    rules.iter().any(|r| r.denies(url)),
                    "{}: {entry} blocks {url}, which no rule denies",
                    domains.tool()
                );
            }
        }
    }
}

// ── Authors (0.168) ──────────────────────────────────────────────────────

fn authors(names: &[&str]) -> Links {
    Links::Defanged {
        allow: Vec::new(),
        allow_authors: names.iter().map(|n| n.parse().unwrap()).collect(),
    }
}

fn agent(name: &str) -> AgentName {
    name.parse().unwrap()
}

/// A listed author's text is read as under `raw` with no rule: the
/// site's pages as links, nothing defanged, however long a URL is
#[test]
fn a_listed_authors_text_is_as_written_but_for_site_links() {
    let links = authors(&["claude-ai"]);
    let site = site();
    let long = format!("https://evil.example/{}", "a".repeat(MAX_URL_BYTES));
    let text = format!(
        "See https://evil.example/x, www.example.org and \
         https://subliminal.technology/agora/protocol, and {long}"
    );
    let theirs = links.for_author(Some(&agent("claude-ai")));
    let (shown, defanged) = theirs.rewrite_noting(&text, Some(&site));
    assert!(!defanged);
    assert_eq!(
        shown,
        format!(
            "See https://evil.example/x, www.example.org and \
             agora://protocol, and {long}"
        )
    );
    assert_eq!(
        shown,
        Links::Raw { deny: Vec::new() }.rewrite(&text, Some(&site))
    );
    // Anyone else, or no one known: the reader's policy
    for author in [None, Some(agent("mallory")), Some(agent("Claude-AI"))] {
        let policy = links.for_author(author.as_ref());
        assert_eq!(policy, &links);
        assert!(
            policy
                .rewrite(&text, None)
                .starts_with("See hxxps://evil[.]example/x")
        );
    }
    assert!(links.allows_author(&agent("claude-ai")));
    assert!(!links.allows_author(&agent("Claude-AI")));
    // With no one listed, as before
    assert_eq!(
        Links::default().for_author(Some(&agent("claude-ai"))),
        &Links::default()
    );
}

/// `raw` has no author list: its deny rules hold for everyone
#[test]
fn raw_has_no_authors() {
    let links = deny(&["https://evil.example"]);
    assert!(!links.allows_author(&agent("claude-ai")));
    assert_eq!(links.for_author(Some(&agent("claude-ai"))), &links);
}

/// Authors leave every rule-free default exactly as it was: the
/// adversarial corpus reads the same with a list and no author known
#[test]
fn an_author_list_changes_nothing_for_text_of_no_listed_author() {
    let listed = authors(&["claude-ai"]);
    let plain = Links::default();
    let site = site();
    for text in ADVERSARIAL.iter().chain(SPELLINGS) {
        for at in [None, Some(&site)] {
            assert_eq!(
                listed.for_author(Some(&agent("mallory"))).rewrite(text, at),
                plain.rewrite(text, at),
                "{text:?}"
            );
        }
    }
}

/// The web tools' domain lists cannot say who wrote a link: the report
/// says the tool does not follow the author list
#[test]
fn the_tool_report_says_authors_do_not_widen_the_tool() {
    let mut links = authors(&["claude-ai", "steward"]);
    let told = links.fetch_domains().to_string();
    assert!(
        told.ends_with(
            "  2 authors' own text is shown with its links as written; the \
             tool is still as configured, whatever links they write\n"
        ),
        "{told}"
    );
    assert_eq!(
        links.fetch_domains().offer(),
        Links::default().fetch_domains().offer()
    );
    if let Links::Defanged { allow, .. } = &mut links {
        allow.push(rule("https://docs.rs"));
    }
    let told = links.search_domains().to_string();
    assert!(
        told.ends_with(
            "the tool still reaches only the domains above, whatever links \
             they write\n"
        ),
        "{told}"
    );
    assert!(
        !deny(&["https://evil.example"])
            .fetch_domains()
            .to_string()
            .contains("author")
    );
}
