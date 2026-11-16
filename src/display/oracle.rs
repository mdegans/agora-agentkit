//! What [`Links`](super::Links) is held to, by code that shares none of
//! its scanner: where a URL could be read in a text ([`places`]), what a
//! fetcher makes of a reading ([`fetched`], [`destination`]), and the
//! server's scanner as of its removal ([`old_server`]).
//!
//! Compiled into this crate's tests, and included whole by
//! `tools/display-corpus`, which provides `crate::links` and
//! `crate::threads` from this crate.
#![allow(dead_code)]

use std::borrow::Cow;

use url::{Host, Url};

use crate::links::AgoraLink;
use crate::threads;

/// How far a reader reads a URL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// To the next space, with tabs and line breaks (and the whitespace
    /// before a line break, which [`threads::tidy`] trims) dropped, as a
    /// WHATWG parser handed all of it does. What the default is held to
    Dropping,
    /// To the first whitespace or line break of any kind. What rules are
    /// held to: a reader that deletes line breaks inside a URL is outside
    /// what is guarded (the Steward, 2026-11-16)
    Whitespace,
}

fn is_gap(c: char) -> bool {
    c.is_whitespace() || threads::is_line_break(c)
}

/// Every place in `text` a URL could start, and each reading of it from
/// there, found by splitting on whitespace and brackets: the run to its
/// end (by `reader`), cut at each bracket or quote and at each place
/// something was dropped, with closing punctuation off the end, and with
/// the dots of a last `.` or `..` segment too
pub fn places(
    text: &str,
    reader: Reader,
) -> impl Iterator<Item = (usize, String)> + '_ {
    const BRACKETS: &str = "<>\"'`()[]{}|";
    const CLOSING: &str = "<>\"'`()[]{}|.,;:!?*_";
    text.char_indices()
        .filter(|(_, c)| matches!(c, 'h' | 'H' | 'w' | 'W'))
        .flat_map(move |(at, _)| {
            let mut token = String::new();
            let mut stops = Vec::new();
            let mut rest = &text[at..];
            while let Some(c) = rest.chars().next() {
                if !is_gap(c) {
                    if BRACKETS.contains(c) {
                        stops.push(token.len());
                    }
                    token.push(c);
                    rest = &rest[c.len_utf8()..];
                    continue;
                }
                if reader == Reader::Whitespace {
                    break;
                }
                let gap = rest.find(|c: char| !is_gap(c)).unwrap_or(rest.len());
                let Some(dropped) = rest[..gap]
                    .rfind(|c| c == '\t' || threads::is_line_break(c))
                else {
                    break;
                };
                stops.push(token.len());
                let dropped = &rest[dropped..];
                let last = dropped.chars().next().map_or(0, char::len_utf8);
                rest = &dropped[last..];
            }
            stops.push(token.len());
            stops
                .into_iter()
                .flat_map(|stop| {
                    let read = &token[..stop];
                    let closed = read.trim_end_matches(|c| CLOSING.contains(c));
                    let dots = read[closed.len()..]
                        .chars()
                        .take_while(|c| *c == '.')
                        .count();
                    let segment = closed.rsplit('/').next().unwrap_or("");
                    let dotted = closed.contains('/')
                        && matches!(
                            segment
                                .to_ascii_lowercase()
                                .replace("%2e", ".")
                                .as_str(),
                            "" | "."
                        );
                    (0..=dots.min(2) * usize::from(dotted))
                        .map(|n| (at, [closed, &"..."[..n]].concat()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
}

/// [`places`] without where each starts
pub fn readings(
    text: &str,
    reader: Reader,
) -> impl Iterator<Item = String> + '_ {
    places(text, reader).map(|(_, reading)| reading)
}

/// A reading as a fetcher's parser has it, when that is `http` or `https`
/// with a host that could be one (a letter or a digit in it, or an
/// address)
pub fn fetched(reading: &str) -> Option<Url> {
    let url = Url::parse(reading).ok()?;
    let real = match url.host()? {
        Host::Domain(name) => name.contains(|c: char| c.is_alphanumeric()),
        Host::Ipv4(_) | Host::Ipv6(_) => true,
    };
    (matches!(url.scheme(), "http" | "https") && real).then_some(url)
}

/// [`fetched`], when its host could be reached: a domain's last label has
/// no ASCII punctuation but `-` and `_` (no domain name does), or it is an
/// address
pub fn destination(reading: &str) -> Option<Url> {
    fetched(reading).filter(|url| match url.host() {
        Some(Host::Domain(name)) => {
            let name = name.strip_suffix('.').unwrap_or(name);
            let last = name.rsplit('.').next().unwrap_or(name);
            !last
                .bytes()
                .any(|b| b.is_ascii_punctuation() && !matches!(b, b'-' | b'_'))
        }
        _ => true,
    })
}

/// The first URL in `text` that a fetcher could be handed and would
/// fetch, read [`Reader::Dropping`]: [`Links`](super::Links)' default
/// invariant says there is none
pub fn fetchable(text: &str) -> Option<Url> {
    readings(text, Reader::Dropping).find_map(|reading| fetched(&reading))
}

/// Spellings of a scheme that a URL parser reads as `http` or `https`
/// (each is `evil.example` to it), which the server's scanner did not see
pub const SPELLINGS: &[&str] = &[
    "https:/evil.example/x",
    "https:evil.example/x",
    "https:\\\\evil.example/x",
    "http:/\\evil.example",
    "http:\\/evil.example",
    "https:///evil.example/x",
    "ht\ttps://evil.example/x",
    "h\nt\rt\tp\ns\n:\n/\n/evil.example/x",
    "https\r\n://evil.example/x",
    "https:\n//evil.example/x",
    "https://\nevil.example/x",
    "https:/\t/\nevil.example/x",
    // What the runner's view makes a line break of, and trims before one
    "https:   \nevil.example/x",
    "https: \u{a0} \r\n//evil.example/x",
    "ht\u{2028}tps://evil.example/x",
    "http\u{b}s:\u{c}evil.example/x",
    "https\u{1c}:\u{85}/\u{2029}/evil.example",
    "https:// \nevil.example/x",
    "HTTPS:EVIL.EXAMPLE",
    "hTtP:\\evil.example",
    "xhttps:evil.example/x",
    "http::@evil.example/x",
    "http:u:p@evil.example/x",
    "http:\".evil.example/x",
    "http:'@evil.example/x",
    "http:(evil.example)",
    "http:%65vil.example",
    "http:[::1]/x",
    "http:[::]/x",
    "http:2130706433/x",
    "https:\u{fc}ber.example/x",
    "https://a:https:evil.example/x",
    "https://ok.example/x/http:evil.example",
    "www.ok.example/https:evil.example",
];

// ── agora-server's scanner, `crates/agora-server/src/urls.rs` as of its
// removal (agora `5b606ca1^`), copied as it was: an oracle, not to be
// fixed. Only `rewrite_at` with `defang` set, what every agent was served,
// and its helpers; `AgoraLink` is this crate's, which the server's was

fn is_authority_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(b, b'-' | b'.' | b'_' | b'~' | b'%' | b':' | b'@')
}

fn matches_at(bytes: &[u8], i: usize, pat: &[u8]) -> bool {
    bytes.len() >= i + pat.len()
        && bytes[i..i + pat.len()]
            .iter()
            .zip(pat)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn at_word_boundary(bytes: &[u8], i: usize) -> bool {
    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'.')
}

fn url_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = start;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && !matches!(
            bytes[end],
            b'<' | b'>'
                | b'"'
                | b'\''
                | b'`'
                | b'('
                | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'|'
        )
    {
        end += 1;
        if end - start > MAX_PAGE_URL_BYTES {
            return None;
        }
    }
    while end > start
        && matches!(
            bytes[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b'*' | b'_'
        )
    {
        end -= 1;
    }
    Some(end)
}

const MAX_PAGE_URL_BYTES: usize = 2_048;

fn on_site_host(bytes: &[u8], at: usize, site: &Url) -> bool {
    let Some(host) = site.host_str() else {
        return false;
    };
    matches_at(bytes, at, host.as_bytes())
        && bytes
            .get(at + host.len())
            .is_none_or(|b| !is_authority_byte(*b) || *b == b':')
}

/// What the server sent an agent of `text` an author wrote: its
/// `defang_at`
pub fn old_server<'a>(text: &'a str, site: Option<&Url>) -> Cow<'a, str> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0;
    let mut copied = 0;

    while i < bytes.len() {
        let (scheme_len, https) =
            if at_word_boundary(bytes, i) && matches_at(bytes, i, b"http") {
                if matches_at(bytes, i + 4, b"s://") {
                    (8, true)
                } else if matches_at(bytes, i + 4, b"://") {
                    (7, false)
                } else {
                    i += 1;
                    continue;
                }
            } else if at_word_boundary(bytes, i)
                && matches_at(bytes, i, b"www.")
                && bytes.get(i + 4).copied().is_some_and(is_authority_byte)
            {
                (0, false)
            } else {
                i += 1;
                continue;
            };

        if let Some(site) = site.filter(|s| {
            scheme_len > 0 && on_site_host(bytes, i + scheme_len, s)
        }) && let Some(end) = url_end(bytes, i)
            && let Ok(link) = AgoraLink::parse_at(&text[i..end], Some(site))
        {
            let buf = out
                .get_or_insert_with(|| String::with_capacity(text.len() + 16));
            buf.push_str(&text[copied..i]);
            buf.push_str(&link.to_string());
            copied = end;
            i = end;
            continue;
        }

        let buf =
            out.get_or_insert_with(|| String::with_capacity(text.len() + 16));
        buf.push_str(&text[copied..i]);
        if scheme_len > 0 {
            buf.push_str(if https { "hxxps://" } else { "hxxp://" });
        }

        let mut j = i + scheme_len;
        while j < bytes.len() && is_authority_byte(bytes[j]) {
            if bytes[j] == b'.' {
                buf.push_str("[.]");
            } else {
                buf.push(bytes[j] as char);
            }
            j += 1;
        }
        copied = j;
        i = j.max(i + 1);
    }

    match out {
        Some(mut buf) => {
            buf.push_str(&text[copied..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(text),
    }
}
