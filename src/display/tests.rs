use super::*;

use serde_json::json;

fn site() -> Url {
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
fn generated(count: usize) -> Vec<String> {
    const PIECES: &[&str] = &[
        "http://",
        "https://",
        "HTTPS://",
        "hTtP://",
        "http:/",
        "http",
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

const ADVERSARIAL: &[&str] = &[
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

/// Under the default policy nothing `http://` or `https://` is left, in
/// any case, whatever the text and whatever the site
#[test]
fn the_default_leaves_no_scheme() {
    let sites = [
        None,
        Some(site()),
        Some(url("https://subliminal.technology/agora")),
    ];
    let texts = generated(20_000);
    for text in ADVERSARIAL
        .iter()
        .copied()
        .chain(texts.iter().map(String::as_str))
    {
        for site in &sites {
            let out = Links::default().rewrite(text, site.as_ref());
            assert!(
                !has_scheme(out.as_bytes()),
                "{text:?} at {site:?} gave {out:?}"
            );
        }
    }
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

/// Under `raw`, no URL left in the result is one a deny rule matches, read
/// from its scheme to its end as written
#[test]
fn raw_leaves_no_denied_url() {
    let links = deny(&[
        "https://evil.example",
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
        .copied()
        .chain(texts.iter().map(String::as_str))
    {
        for site in &sites {
            let out = links.rewrite(text, site.as_ref());
            let bytes = out.as_bytes();
            for at in
                (0..bytes.len()).filter(|i| scheme_at(bytes, *i).is_some())
            {
                // As any reader would take it, nested URL and all
                let end = url_end(bytes, at).unwrap_or(bytes.len());
                let Ok(found) = Url::parse(&out[at..end]) else {
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
    assert!(left > 1_000, "the texts left {left} URLs: too few to tell");
    // No denied host in any adversarial text survives with a scheme
    for text in ADVERSARIAL {
        let out = links.rewrite(text, None).to_ascii_lowercase();
        for scheme in ["http://", "https://"] {
            assert!(!out.contains(&[scheme, "evil.example"].concat()), "{out}");
        }
    }
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
    // No link: defanged
    assert_eq!(
        at("https://subliminal.technology/agora/account"),
        "hxxps://subliminal[.]technology/agora/account"
    );
    assert_eq!(
        at("https://subliminal.technology/agora/governance/GOV-2026-0003"),
        "hxxps://subliminal[.]technology/agora/governance/GOV-2026-0003"
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
    // A page of the site with no link is any other URL
    let account = "https://subliminal.technology/agora/account";
    let fanged = "hxxps://subliminal[.]technology/agora/account";
    for (links, want) in [
        (Links::default(), fanged),
        (allow(&["https://subliminal.technology"]), account),
        (Links::Raw { deny: Vec::new() }, account),
        (deny(&["https://subliminal.technology/agora"]), fanged),
    ] {
        assert_eq!(links.rewrite(account, Some(&site)), want, "{links:?}");
    }
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
    ];
    for links in &policies {
        for unit in [
            "http://a/",
            "http://a/ ",
            "www.a/",
            "https://subliminal.technology/x/",
            "https://subliminal.technology/agora/post/",
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
    // Elsewhere, another port and another path are not the rule's
    for text in [
        "https://ok.example/x",
        "https://evil.example:8443/x",
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

    // A deny rule on the default port covers both schemes; on another
    // port, that scheme and port only
    assert!(r.denies(&url("http://example.org/")));
    assert!(r.denies(&url("https://example.org/")));
    assert!(!r.denies(&url("https://example.org:8443/")));
    assert!(!r.denies(&url("http://example.org:443/")));
    assert!(port.denies(&url("https://example.org:8443/")));
    assert!(!port.denies(&url("http://example.org:8443/")));
    assert!(!port.denies(&url("https://example.org/")));
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
        "https://example.org/wiki/AC%2FDC",
    ] {
        assert!(!r.allows(&url(written)), "{written}");
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
    // A rule's dot segments are resolved as well
    assert_eq!(rule("https://example.org/a/../wiki"), r);
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
    let links = Links::Defanged { allow: vec![idn] };
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
    let links = Links::Defanged { allow: vec![v6] };
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
        ("https://example.org/a/./b/../c", "https://example.org/a/c"),
    ] {
        let r = rule(written);
        assert_eq!(r.to_string(), shown);
        assert_eq!(rule(shown), r);
    }
}

// ── Config and serde ────────────────────────────────────────────────────

#[test]
fn the_default_is_everything_defanged() {
    assert_eq!(Links::default(), Links::Defanged { allow: Vec::new() });
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
