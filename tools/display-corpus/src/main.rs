//! What `display::Links` changes in real text.
//!
//! `display-corpus <bodies.jsonl> <allow-list> <site> <out-dir>` reads one
//! JSON object per line (`{"kind","id","title"?,"body"}`), a file of
//! `LinkRule`s (one a line, `#` comments), and the site's URL, and writes
//! `<out-dir>/report.md` and `<out-dir>/differences.jsonl`:
//!
//! - (a) the default policy against the server's old scanner
//!   (`oracle::old_server`), each difference grouped by cause;
//! - (b) under the allow list, every URL on an allowed host not left as
//!   written, by why, and the hosts that are not on the list;
//! - (c) no output under the default has a fetchable URL, and none under
//!   the list has one the list does not allow (`oracle`'s readers);
//! - (d) nothing outside a rewritten URL changed, by a character diff of
//!   each field against its output.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use agora_agentkit::display::{LinkRule, Links};
use serde::Deserialize;
use url::Url;

// `oracle.rs` names these as `crate::…`
mod links {
    pub use agora_agentkit::links::*;
}
mod threads {
    pub use agora_agentkit::threads::*;
}
#[path = "../../../src/display/oracle.rs"]
mod oracle;

use oracle::Reader;

#[derive(Deserialize)]
struct Record {
    kind: String,
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

struct Field {
    kind: String,
    id: String,
    name: &'static str,
    text: String,
}

/// A few dozen bytes either side of `at..end` in `text`, on char boundaries
fn snippet(text: &str, at: usize, end: usize) -> String {
    let mut from = at.saturating_sub(40);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (end + 40).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    text[from..to].to_string()
}

fn is_gap(c: char) -> bool {
    c.is_whitespace() || threads::is_line_break(c)
}

/// The run of `text` from `at` to the next whitespace
fn run(text: &str, at: usize) -> &str {
    let end = text[at..].find(is_gap).map_or(text.len(), |o| at + o);
    &text[at..end]
}

/// A group of findings: a count and the first few examples
#[derive(Default)]
struct Group {
    count: usize,
    examples: Vec<String>,
}

impl Group {
    fn add(&mut self, example: impl FnOnce() -> String) {
        self.count += 1;
        if self.examples.len() < 3 {
            self.examples.push(example());
        }
    }
}

fn write_groups(out: &mut String, groups: &BTreeMap<String, Group>) {
    let mut sorted: Vec<_> = groups.iter().collect();
    sorted.sort_by_key(|(_, g)| std::cmp::Reverse(g.count));
    for (name, group) in sorted {
        let _ = writeln!(out, "- **{name}**: {}", group.count);
        for example in &group.examples {
            let _ = writeln!(out, "  - `{}`", example.replace('`', "ˋ").replace('\n', "⏎"));
        }
    }
}

// ── (a) The default against the server's scanner ────────────────────────

/// Why the default differs from the server on one whitespace-free token
fn cause(token: &str, old: &str, new: &str, site: &Url) -> &'static str {
    let lower = token.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    // 0.168.1: a path on the site as a Markdown link's target is its
    // link, which the server left as written
    if relative_explained(token, old, new, site) {
        return "a relative path on the site as a Markdown target: its agora:// link (0.168.1)";
    }
    // 0.168: a page of the site no kind names is `agora://page/…`, which
    // the server defanged. Only when that is the whole difference: the
    // default's output with those links put back as the server wrote them
    if new.contains("agora://page/") && pages_back(new, site) == old {
        return "a site page no kind names: agora://page/ here, defanged by the server (0.168)";
    }
    if old.matches("agora://").count() != new.matches("agora://").count() {
        return "a site page: link in one, defanged in the other";
    }
    let starts: Vec<usize> = lower.match_indices("http").map(|(i, _)| i).collect();
    for &i in &starts {
        let rest = &lower[i + 4..];
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        if rest.starts_with(':') && !rest.starts_with("://") {
            return "scheme spelt without `://` (`https:x`, `http:/x`)";
        }
        if rest.starts_with(":///") || rest.starts_with("://\\") {
            return "scheme with a third slash";
        }
    }
    for &i in &starts {
        let rest = &lower[i + 4..];
        let rest = rest.strip_prefix('s').unwrap_or(rest);
        if rest.starts_with("://")
            && i > 0
            && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.')
        {
            return "scheme after a letter, digit or dot (`xhttps://`)";
        }
    }
    if lower.matches("://").count() >= 2 {
        return "a URL inside another's authority (after its `:` or `@`)";
    }
    "UNEXPLAINED"
}

/// `new` with each `agora://page/…` link in it put back as the server
/// wrote its page: the page's URL on `site`, defanged by the server's
/// scanner (which had no `page` kind). Equal to the server's output only
/// when the page links are the whole difference: a page written `http://`
/// or with a `#fragment` does not come back equal, and is not explained
fn pages_back(new: &str, site: &Url) -> String {
    const START: &str = "agora://page/";
    let mut out = String::with_capacity(new.len());
    let mut rest = new;
    while let Some(at) = rest.find(START) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| is_gap(c) || "<>\"'`()[]{}|".contains(c))
            .unwrap_or(tail.len());
        let link = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*', '_']);
        match link.parse::<links::AgoraLink>() {
            Ok(page @ links::AgoraLink::Page(_)) => {
                let url = page.page_url(site);
                out.push_str(&oracle::old_server(url.as_str(), Some(site)));
            }
            // Not a page link as written: left, so it cannot match
            _ => out.push_str(link),
        }
        rest = &tail[link.len()..];
    }
    out.push_str(rest);
    out
}

/// Whether the server left `token` as written and the default differs
/// from it only by each `](/agora/…)` target in it written as its link
/// (`AgoraLink::on_site_path`), worked out here apart from the scanner
fn relative_explained(token: &str, old: &str, new: &str, site: &Url) -> bool {
    if old != token || !token.contains("](/agora/") {
        return false;
    }
    let mut expected = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(at) = rest.find("](/agora/") {
        let path_at = at + 2;
        expected.push_str(&rest[..path_at]);
        let tail = &rest[path_at..];
        let end = tail
            .find(|c: char| is_gap(c) || ")<>\"'".contains(c))
            .unwrap_or(tail.len());
        match links::AgoraLink::on_site_path(&tail[..end], site) {
            Some(link) => expected.push_str(&link.to_string()),
            None => expected.push_str(&tail[..end]),
        }
        rest = &tail[end..];
    }
    expected.push_str(rest);
    expected == new
}

// ── (b) Under the allow list ────────────────────────────────────────────

/// A rule's host as written, and whether it is a wildcard
struct RuleHost {
    host: String,
    wildcard: bool,
}

fn rule_hosts(rules: &[LinkRule]) -> Vec<RuleHost> {
    rules
        .iter()
        .filter_map(|rule| {
            let text = rule.to_string();
            let rest = text.split_once("://")?.1;
            let (wildcard, rest) = match rest.strip_prefix("*.") {
                Some(rest) => (true, rest),
                None => (false, rest),
            };
            let host = rest.split(['/', ':']).next()?.to_ascii_lowercase();
            Some(RuleHost { host, wildcard })
        })
        .collect()
}

fn on_listed_host(hosts: &[RuleHost], host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    hosts.iter().any(|r| {
        if r.wildcard {
            host.strip_suffix(&r.host)
                .and_then(|sub| sub.strip_suffix('.'))
                .is_some_and(|sub| !sub.is_empty())
        } else {
            host == r.host
        }
    })
}

/// The URL written at `at` as prose reads it: to whitespace or a bracket,
/// less sentence punctuation (`display`'s `url_end`, restated)
fn prose_end(text: &str, at: usize) -> usize {
    let mut end = text[at..]
        .find(|c: char| is_gap(c) || "<>\"'`()[]{}|".contains(c))
        .map_or(text.len(), |o| at + o);
    while end > at && ".,;:!?*_".contains(text.as_bytes()[end - 1] as char) {
        end -= 1;
    }
    end
}

/// A host read leniently from a URL that does not parse
fn lenient_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}

/// The host an archive's page is of: `web.archive.org/web/<when>/<url>`
fn archived_host(url: &Url) -> Option<String> {
    let mut segments = url.path_segments()?;
    if segments.next()? != "web" {
        return None;
    }
    segments.next()?;
    let target: Vec<&str> = segments.collect();
    let target = target.join("/");
    let target = if target.contains("://") {
        target
    } else {
        ["http://", &target].concat()
    };
    Url::parse(&target.replace(":/", "://").replace(":///", "://"))
        .ok()?
        .host_str()
        .map(str::to_string)
}

/// Why `links` did not leave the URL at `at`, whose host is on the list
fn why_defanged(
    text: &str,
    at: usize,
    links: &Links,
    rules: &[LinkRule],
    hosts: &[RuleHost],
) -> String {
    let token = run(text, at);
    let lower = token.to_ascii_lowercase();
    let scheme = if lower.starts_with("https://") { 8 } else { 7 };
    if matches!(token.as_bytes().get(scheme), Some(b'/' | b'\\')) {
        return "scheme with a third slash".into();
    }
    let prose = &text[at..prose_end(text, at)];
    if prose.len() > agora_agentkit::display::MAX_URL_BYTES {
        return "longer than MAX_URL_BYTES".into();
    }
    match Url::parse(prose) {
        Err(e) => return format!("the prose reading does not parse ({e})"),
        Ok(url) => {
            if !rules.iter().any(|r| r.allows(&url)) {
                let sub = if !url.username().is_empty() || url.password().is_some() {
                    "it has a user or password"
                } else if url.port().is_some() {
                    "it has a port"
                } else if !rules.iter().any(|r| {
                    r.to_string().starts_with(&format!("{}://", url.scheme()))
                        && r.allows(&{
                            let mut u = url.clone();
                            u.set_path("/");
                            u.set_query(None);
                            u
                        })
                }) && rules.iter().any(|r| {
                    let mut u = url.clone();
                    let other = if url.scheme() == "http" { "https" } else { "http" };
                    u.set_scheme(other).is_ok() && r.allows(&u)
                }) {
                    "its scheme is not the rule's"
                } else {
                    "its scheme or path is not under any rule for its host"
                };
                return format!("the prose reading is not allowed: {sub}");
            }
        }
    }
    let authority = prose[scheme..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || "-._~%:@".contains(c)))
        .map_or(prose.len(), |o| scheme + o);
    let inner = prose[authority..].to_ascii_lowercase();
    if inner.contains("http") && (inner.contains("http:") || inner.contains("https:"))
        || inner.contains("www.")
    {
        return "another URL start inside it (`http:` or `www.` in its path or query)".into();
    }
    if !links.rewrite(prose, None).starts_with(&prose[..scheme]) {
        return "the prose reading alone is defanged (a dot segment, a control character)".into();
    }
    // A later reading: the first that fails, by what ends it
    for (_, reading) in oracle::places(token, Reader::Whitespace).filter(|(at, _)| *at == 0) {
        if reading.len() <= prose.len() {
            continue;
        }
        let tail = &reading[prose.len()..];
        let past = tail.chars().next().unwrap_or(' ');
        match Url::parse(&reading) {
            Err(_) => {
                let named = oracle::destination(&reading).is_some();
                if !named {
                    return format!("a later reading, past `{past}`, does not parse");
                }
            }
            Ok(url) => {
                if oracle::destination(&reading).is_none() {
                    continue;
                }
                if !rules.iter().any(|r| r.allows(&url)) {
                    let host_ok = url.host_str().is_some_and(|h| on_listed_host(hosts, h));
                    return if host_ok {
                        format!("a later reading, past `{past}`, has a path the rule does not allow")
                    } else {
                        format!("a later reading, past `{past}`, names another host")
                    };
                }
            }
        }
    }
    "UNEXPLAINED".into()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, corpus, allow, site, out_dir] = &args[..] else {
        eprintln!("usage: display-corpus <bodies.jsonl> <allow-list> <site> <out-dir>");
        std::process::exit(2);
    };
    let site = Url::parse(site).expect("the site is a URL");
    let rules: Vec<LinkRule> = std::fs::read_to_string(allow)
        .expect("the allow list")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.parse().unwrap_or_else(|e| panic!("{e}")))
        .collect();
    let hosts = rule_hosts(&rules);
    let listed = Links::Defanged {
        allow: rules.clone(),
        allow_authors: Vec::new(),
    };
    let default = Links::default();

    let mut fields = Vec::new();
    for line in std::fs::read_to_string(corpus).expect("the corpus").lines() {
        let record: Record = serde_json::from_str(line).expect("a record");
        for (name, text) in [("title", record.title), ("body", record.body)] {
            if let Some(text) = text {
                fields.push(Field {
                    kind: record.kind.clone(),
                    id: record.id.clone(),
                    name,
                    text,
                });
            }
        }
    }

    let mut jsonl = String::new();
    let mut report = String::new();
    let mut emit = |value: serde_json::Value| {
        jsonl.push_str(&value.to_string());
        jsonl.push('\n');
    };

    // Timing, each config over the whole corpus, best of three
    let time = |f: &dyn Fn(&str) -> usize| {
        (0..3)
            .map(|_| {
                let start = Instant::now();
                let n: usize = fields.iter().map(|x| f(&x.text)).sum();
                std::hint::black_box(n);
                start.elapsed()
            })
            .min()
            .unwrap_or(Duration::ZERO)
    };
    let t_old = time(&|t| oracle::old_server(t, Some(&site)).len());
    let t_default = time(&|t| default.rewrite(t, Some(&site)).len());
    let t_listed = time(&|t| listed.rewrite(t, Some(&site)).len());

    // (a)
    let mut a_groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut a_fields = 0;
    let mut a_unexplained = 0;
    for f in &fields {
        let old = oracle::old_server(&f.text, Some(&site));
        let new = default.rewrite(&f.text, Some(&site));
        if old == new {
            continue;
        }
        a_fields += 1;
        let mut found = false;
        for (at, token) in f.text.split(is_gap).scan(0usize, |pos, tok| {
            let at = *pos;
            *pos += tok.len();
            // skip the separator char too
            if let Some(c) = f.text[*pos..].chars().next() {
                *pos += c.len_utf8();
            }
            Some((at, tok))
        }) {
            if token.is_empty() {
                continue;
            }
            let o = oracle::old_server(token, Some(&site));
            let n = default.rewrite(token, Some(&site));
            if o == n {
                continue;
            }
            found = true;
            let why = cause(token, &o, &n, &site);
            if why == "UNEXPLAINED" {
                a_unexplained += 1;
            }
            let shown = snippet(&f.text, at, at + token.len().min(80));
            a_groups.entry(why.into()).or_default().add(|| shown.clone());
            emit(serde_json::json!({
                "check": "a: default vs the server", "kind": f.kind, "id": f.id,
                "field": f.name, "cause": why, "input": shown,
                "server": o, "default": n,
            }));
        }
        if !found {
            let i = old.bytes().zip(new.bytes()).take_while(|(a, b)| a == b).count();
            let why = if new.contains("agora://page/") && pages_back(&new, &site) == old {
                "a site page no kind names, across a space the token split did not see (0.168)"
            } else if old.matches("agora://").count() != new.matches("agora://").count() {
                "a site page before a space that is not ASCII, or a line break that is not"
            } else {
                "a scheme spelt across whitespace"
            };
            let shown = snippet(&f.text, i.min(f.text.len()), i.min(f.text.len()));
            a_groups.entry(why.into()).or_default().add(|| shown.clone());
            emit(serde_json::json!({
                "check": "a: default vs the server", "kind": f.kind, "id": f.id,
                "field": f.name, "cause": why, "input": shown,
            }));
        }
    }

    // (b)
    let mut found_urls = 0;
    let mut left = 0;
    let mut site_links = 0;
    let mut not_listed: BTreeMap<String, usize> = BTreeMap::new();
    let mut archived: BTreeMap<String, usize> = BTreeMap::new();
    let mut b_groups: BTreeMap<String, Group> = BTreeMap::new();
    for f in &fields {
        let lower = f.text.to_ascii_lowercase();
        let starts: Vec<usize> = lower
            .match_indices("http")
            .map(|(i, _)| i)
            .filter(|&i| lower[i..].starts_with("http://") || lower[i..].starts_with("https://"))
            .collect();
        for at in starts {
            found_urls += 1;
            let token = run(&f.text, at);
            let shown = listed.rewrite(token, Some(&site));
            if shown.starts_with("agora://") {
                site_links += 1;
                continue;
            }
            if shown.starts_with(&token[..7]) {
                left += 1;
                continue;
            }
            let prose = &f.text[at..prose_end(&f.text, at)];
            let parsed = Url::parse(prose).ok();
            let host = parsed
                .as_ref()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| lenient_host(prose));
            if !on_listed_host(&hosts, &host) {
                *not_listed.entry(host.clone()).or_default() += 1;
                if host == "web.archive.org" || host == "archive.org" {
                    let target = parsed
                        .as_ref()
                        .and_then(archived_host)
                        .unwrap_or_else(|| "(no archived URL)".into());
                    *archived.entry(target).or_default() += 1;
                }
                continue;
            }
            let why = why_defanged(&f.text, at, &listed, &rules, &hosts);
            let example = snippet(&f.text, at, at + token.len().min(100));
            b_groups.entry(why.clone()).or_default().add(|| example.clone());
            emit(serde_json::json!({
                "check": "b: on a listed host, not left", "kind": f.kind, "id": f.id,
                "field": f.name, "reason": why, "host": host, "input": example,
                "shown": snippet(&shown, 0, shown.len().min(100)),
            }));
        }
    }

    // (c)
    let mut c_default = 0;
    let mut c_listed = 0;
    let mut c_left_checked = 0;
    for f in &fields {
        let out = default.rewrite(&f.text, Some(&site));
        for text in [out.to_string(), threads::tidy(&out)] {
            if let Some(url) = oracle::fetchable(&text) {
                c_default += 1;
                emit(serde_json::json!({
                    "check": "c: fetchable under the default", "id": f.id,
                    "field": f.name, "url": url.as_str(),
                }));
            }
        }
        let out = listed.rewrite(&f.text, Some(&site));
        for text in [out.to_string(), threads::tidy(&out)] {
            for reading in oracle::readings(&text, Reader::Whitespace) {
                if let Some(url) = oracle::destination(&reading) {
                    c_left_checked += 1;
                    if !rules.iter().any(|r| r.allows(&url)) {
                        c_listed += 1;
                        emit(serde_json::json!({
                            "check": "c: left under the list, not allowed", "id": f.id,
                            "field": f.name, "url": url.as_str(),
                        }));
                    }
                }
            }
        }
    }

    // (d)
    let mut d_changed = [0usize; 2];
    let mut d_bad = [0usize; 2];
    for (k, links) in [&default, &listed].into_iter().enumerate() {
        for f in &fields {
            let out = links.rewrite(&f.text, Some(&site));
            if out == f.text {
                continue;
            }
            d_changed[k] += 1;
            if let Some((at, what)) = outside_a_url(&f.text, &out, &site) {
                d_bad[k] += 1;
                emit(serde_json::json!({
                    "check": "d: a change outside a URL", "config": (["default", "list"][k]),
                    "id": f.id, "field": f.name, "what": what,
                    "input": snippet(&f.text, at.min(f.text.len()), at.min(f.text.len())),
                }));
            }
        }
    }

    let total_listed_defanged: usize = b_groups.values().map(|g| g.count).sum();
    let not_listed_total: usize = not_listed.values().sum();
    let _ = writeln!(report, "# What the link policy changes in real text\n");
    let _ = writeln!(
        report,
        "{} fields ({} records' titles and bodies), site `{site}`, {} allow rules.\n",
        fields.len(),
        fields.iter().filter(|f| f.name == "body").count(),
        rules.len()
    );
    let _ = writeln!(
        report,
        "Time for the whole corpus (best of three, release build): server scanner {:.0} ms, default {:.0} ms, allow list {:.0} ms.\n",
        t_old.as_secs_f64() * 1e3,
        t_default.as_secs_f64() * 1e3,
        t_listed.as_secs_f64() * 1e3
    );
    let _ = writeln!(report, "## (a) The default against the server's scanner\n");
    let _ = writeln!(
        report,
        "{a_fields} fields differ; by cause (per whitespace-free token; UNEXPLAINED: {a_unexplained}):\n"
    );
    write_groups(&mut report, &a_groups);
    let _ = writeln!(report, "\n## (b) Under the allow list\n");
    let _ = writeln!(
        report,
        "URLs written `http://` or `https://`: {found_urls}. Left as written: {left}. \
         The site's pages, written as links: {site_links}. Defanged, host not on the list: \
         {not_listed_total}. Defanged though the host is on the list: {total_listed_defanged}.\n"
    );
    let _ = writeln!(report, "### On a listed host and not left, by why\n");
    write_groups(&mut report, &b_groups);
    let _ = writeln!(report, "\n### Hosts not on the list (top 30 of {})\n", not_listed.len());
    let mut by_count: Vec<_> = not_listed.iter().collect();
    by_count.sort_by_key(|(h, n)| (std::cmp::Reverse(**n), (*h).clone()));
    for (host, n) in by_count.iter().take(30) {
        let _ = writeln!(report, "- {host}: {n}");
    }
    let _ = writeln!(report, "\n### Web archive links, by the archived site\n");
    if archived.is_empty() {
        let _ = writeln!(report, "None.");
    }
    let mut by_count: Vec<_> = archived.iter().collect();
    by_count.sort_by_key(|(h, n)| (std::cmp::Reverse(**n), (*h).clone()));
    for (host, n) in by_count {
        let listed = if on_listed_host(&hosts, host) { " (on the list)" } else { "" };
        let _ = writeln!(report, "- {host}: {n}{listed}");
    }
    let _ = writeln!(report, "\n## (c) Sanity\n");
    let _ = writeln!(
        report,
        "- Fields with a fetchable URL under the default (as shown and after `tidy`, read dropping line breaks): {c_default}\n\
         - Readings left under the list that name a reachable host: {c_left_checked}; not allowed by any rule: {c_listed}"
    );
    let _ = writeln!(report, "\n## (d) Text integrity\n");
    let _ = writeln!(
        report,
        "- Default: {} fields changed, {} with a change outside a rewritten URL\n\
         - Allow list: {} fields changed, {} with a change outside a rewritten URL",
        d_changed[0], d_bad[0], d_changed[1], d_bad[1]
    );

    let out_dir = std::path::Path::new(out_dir);
    std::fs::write(out_dir.join("report.md"), &report).expect("the report");
    std::fs::write(out_dir.join("differences.jsonl"), &jsonl).expect("the differences");
    print!("{report}");
}

/// The first change between `input` and `output` that is not a URL's
/// rewrite: each changed span, lower-cased with `t` as `x` on the input
/// side and without `[`/`]` on the output side, must be the same text
/// (`http`→`hxxp`, `.`→`[.]`), next to a defanged scheme or host; or be a
/// site page written as its `agora://` link
fn outside_a_url(input: &str, output: &str, site: &Url) -> Option<(usize, String)> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Myers)
        .diff_chars(input, output);
    let host = site.host_str().unwrap_or_default();
    // Group consecutive changes into hunks: (input range, output range)
    let (mut i, mut o) = (0usize, 0usize);
    let mut hunk: Option<((usize, usize), (usize, usize))> = None;
    let mut hunks = Vec::new();
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(h) = hunk.take() {
                    hunks.push(h);
                }
                i += len;
                o += len;
            }
            ChangeTag::Delete => {
                let h = hunk.get_or_insert(((i, i), (o, o)));
                h.0.1 = i + len;
                i += len;
            }
            ChangeTag::Insert => {
                let h = hunk.get_or_insert(((i, i), (o, o)));
                h.1.1 = o + len;
                o += len;
            }
        }
    }
    hunks.extend(hunk);
    for ((i0, i1), (o0, o1)) in hunks {
        let gone = input[i0..i1].to_lowercase().replace('t', "x");
        let came = output[o0..o1].replace(['[', ']'], "");
        let mut from = o0.saturating_sub(64);
        while !output.is_char_boundary(from) {
            from -= 1;
        }
        let near = output[from..o1.min(output.len())].to_ascii_lowercase();
        let defanged = near.contains("hx") || near.contains("[.]") || output[o0..o1].contains('[');
        if gone == came.to_lowercase() && defanged {
            continue;
        }
        // A site page as its link: the input's token there has the site's
        // host, and the output near it an `agora://`
        let token_start = input[..i0]
            .rfind(is_gap)
            .map_or(0, |p| p + input[p..].chars().next().map_or(1, char::len_utf8));
        let token = run(input, token_start);
        let mut back = o0.saturating_sub(2_100);
        while !output.is_char_boundary(back) {
            back -= 1;
        }
        let mut ahead = (o1 + 2_100).min(output.len());
        while !output.is_char_boundary(ahead) {
            ahead += 1;
        }
        if token.contains(host) && output[back..ahead].contains("agora://") {
            continue;
        }
        return Some((i0, format!("{:?} → {:?}", &input[i0..i1], &output[o0..o1])));
    }
    None
}
