//! A [`Links`] policy as the domain filter of Anthropic's web fetch and web
//! search tools ([`Links::fetch_domains`], [`Links::search_domains`]).
//!
//! Both tools are run by Anthropic, possibly for several hops in one turn,
//! and a link in a fetched page or a search result is fetchable on the
//! next: the tools' own domain lists are the only gate there. What the
//! policy shows as followable should be what the tools reach, and what it
//! blocks they should not reach, as near as their filters can say it.
//!
//! The filters' rules (Anthropic's docs, read 2026-11-17):
//!
//! - an entry has no scheme, and covers its subdomains (`example.org`
//!   covers `docs.example.org`); a wildcard in the domain (`*.example.org`)
//!   is invalid;
//! - **web fetch** matches on the domain alone: an entry with a path never
//!   matches. **Web search** takes a path, and an entry matches everything
//!   that begins with it (`example.org/blog` matches `example.org/blog/1`,
//!   and, as a plain prefix, `example.org/blogger`);
//! - `allowed_domains` and `blocked_domains` cannot both be set;
//! - one invalid entry fails the whole request (a 400), and a domain that
//!   is not ASCII is a homograph risk.
//!
//! A path is assumed matched with its letter case, as a URL's path is
//! (RFC 3986) and as [`LinkRule`] matches it.
//!
//! So a rule's scheme and port cannot be said, and every entry also covers
//! its domain's subdomains over both schemes. That is true of almost every
//! rule, so it is said once ([`ToolDomains`]'s `Display`), not per rule;
//! what is said per rule ([`DomainNote`]) is what goes further than that.
//! An entry is only ever made of the parsed rule's parts, and only of
//! characters certain to be valid: a rule that would need others is
//! widened to its domain (`allow`) or not sent (`deny`), and says so.

use std::collections::BTreeSet;
use std::fmt;

use url::Host;

use super::{LinkRule, Links, loose_segments};

/// A longer domain name is not one
const MAX_DOMAIN_BYTES: usize = 253;
/// Nor is one with a longer label
const MAX_LABEL_BYTES: usize = 63;

/// One of Anthropic's server tools that take a domain filter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerTool {
    /// `web_fetch`: domains only
    WebFetch,
    /// `web_search`: domains, each optionally with a path prefix
    WebSearch,
}

impl fmt::Display for ServerTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WebFetch => "web fetch",
            Self::WebSearch => "web search",
        })
    }
}

/// The domain filter for one server tool under a [`Links`] policy, and
/// what of the policy it changes or leaves out ([`Links::fetch_domains`],
/// [`Links::search_domains`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDomains {
    tool: ServerTool,
    filter: DomainFilter,
    notes: Vec<DomainNote>,
    /// Which list the rules were
    mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Allow,
    Deny,
}

/// What the tool is told. Every entry's domain is ASCII, lower case, a
/// domain name of two labels or more of letters, digits and inner
/// hyphens, without a trailing dot; a web search entry may follow it with
/// a path of letters, digits and `-._~` between slashes. The lists are
/// sorted, have no duplicates, and are never empty
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainFilter {
    /// Neither list is set: the tool is as the operator configured it. An
    /// empty `allow` list, and a `raw` policy with no deny rule sent
    Unfiltered,
    /// `allowed_domains`: only these (and their subdomains) are reached
    Allowed(Vec<String>),
    /// `blocked_domains`: these (and their subdomains) are not reached
    Blocked(Vec<String>),
    /// The `allow` list has rules but none can be sent (each is an IP
    /// address or a single label, say). No filter would let the tool reach
    /// anything allowed, and an empty `allowed_domains` is not one the
    /// tool documents: offer no such tool
    NoDomain,
}

/// One rule that is not sent to the tool as it is
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainNote {
    /// The rule as written in the policy (its `Display`)
    pub rule: LinkRule,
    pub change: DomainChange,
}

/// What became of a [`DomainNote`]'s rule
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainChange {
    /// An `allow` rule sent as `entry`, which the tool reaches more of
    /// than the rule leaves as written. Showing a link as followable and
    /// having the tool refuse it would break what the rule is for
    Widened {
        /// The entry sent
        entry: String,
        /// How, in this order: path or path prefix, port, apex
        by: Vec<Widening>,
    },
    /// A web search `deny` rule with a path, sent as `entry`: the path
    /// with a `/` after it, so the pages under the path are blocked and
    /// none the rule leaves (`/blogger` under `/blog`); the path itself is
    /// not (the display still defangs it)
    Narrowed {
        /// The entry sent
        entry: String,
    },
    /// Not sent: the tool is not gated by this rule. Under `raw` the
    /// display still defangs what the rule denies; under `defanged` a
    /// link it allows is shown as written and the tool will not reach it
    NotSent(NotSent),
}

/// How a sent `allow` rule is widened, beyond the subdomains and other
/// scheme every entry covers
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Widening {
    /// The rule has a path the tool cannot be told (web fetch: any path;
    /// web search: one with a character other than letters, digits and
    /// `-._~`): every path of the domain is reached
    Path,
    /// Web search: the rule's path is sent, and the tool reaches every
    /// path that begins with it, where the rule takes whole segments
    /// (`/blogger` under `/blog`)
    PathPrefix,
    /// The rule has a port: every port is reached
    Port,
    /// The rule is `*.domain`: `domain` itself is reached too, and no rule
    /// for `domain` itself is in the list
    Apex,
}

/// Why a rule is not sent to the tool
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotSent {
    /// Its host is an IP address; the tools take domain names only
    IpAddress,
    /// Its host is one label (`localhost`); the tools take domain names
    /// only
    SingleLabel,
    /// Its host is not a domain name the tools take: a character other
    /// than a letter, digit, inner hyphen or dot (an `_`, say), or a label
    /// or the whole name too long. Sending it could fail every request
    NotHostName,
    /// Web fetch, a `deny` rule with a path: blocking the domain would
    /// block its other pages too
    DenyPath,
    /// Web search, a `deny` rule whose path has a character other than
    /// letters, digits and `-._~` (a `%`, say), or an empty segment: no
    /// entry is certain to be valid and to block no more than the rule
    DenyPathSpelling,
    /// A `deny` rule with a port: blocking the domain would block its
    /// other ports too
    DenyPort,
    /// A `deny` rule for `*.domain`: an entry for `domain` would block
    /// `domain` itself too, which the rule leaves, and no other rule
    /// blocks it
    DenyApex,
}

impl Links {
    /// This policy as web fetch's domain filter, with each rule it widens
    /// or leaves out. Web fetch matches on the domain alone.
    ///
    /// - **`defanged`, with rules**: [`DomainFilter::Allowed`], each rule's
    ///   domain. A rule with a path or port is widened to its domain, and
    ///   `*.example.org` is sent as `example.org`, which covers the apex
    ///   too ([`DomainChange::Widened`]). A host the tool does not take is
    ///   not sent ([`NotSent`]); if no rule is left,
    ///   [`DomainFilter::NoDomain`].
    /// - **`defanged`, with none** (the default): [`DomainFilter::Unfiltered`].
    ///   An empty `allow` list says that no link in what others wrote is
    ///   shown followable, not what the agent may fetch: an address its
    ///   operator gave it, say. Read as "fetch nothing" it would take the
    ///   tool from every agent on the default; the tool's own
    ///   configuration stays the gate.
    /// - **`raw`**: [`DomainFilter::Blocked`], the domain of each rule that
    ///   denies a whole host, any path and port. A rule with a path or a
    ///   port is never widened, since that would block pages the rule
    ///   leaves (`legit.example/good` under `legit.example/evil`): it is
    ///   not sent, unless a sent domain covers its host already. Nor is
    ///   `*.example.org`, unless `example.org` (or a domain over it) is
    ///   blocked by another rule. None left: [`DomainFilter::Unfiltered`].
    ///
    /// Every entry also covers its domain's subdomains, over http and
    /// https: for an `allow` rule a widening, for a `deny` rule more
    /// blocked. It is said once, in the `Display`, not per rule.
    pub fn fetch_domains(&self) -> ToolDomains {
        self.tool_domains(ServerTool::WebFetch)
    }

    /// This policy as web search's domain filter, with each rule it widens,
    /// narrows or leaves out. As [`fetch_domains`](Self::fetch_domains)
    /// but for paths, which web search takes as a prefix:
    ///
    /// - an `allow` rule with a path is sent as `domain/path`, widened
    ///   only to the paths that begin with it ([`Widening::PathPrefix`]);
    /// - a `deny` rule with a path is sent as `domain/path/`, which blocks
    ///   no page the rule leaves (`domain/path` would block `/blogger`
    ///   under `/blog`), and leaves the path itself to the display
    ///   ([`DomainChange::Narrowed`]).
    ///
    /// A path with a character other than letters, digits and `-._~` is
    /// widened to the domain (`allow`) or not sent (`deny`).
    pub fn search_domains(&self) -> ToolDomains {
        self.tool_domains(ServerTool::WebSearch)
    }

    fn tool_domains(&self, tool: ServerTool) -> ToolDomains {
        match self {
            Self::Defanged { allow } => allowed(allow, tool),
            Self::Raw { deny } => blocked(deny, tool),
        }
    }
}

fn allowed(rules: &[LinkRule], tool: ServerTool) -> ToolDomains {
    let mut sent = BTreeSet::new();
    let mut notes = Vec::new();
    // Domains an exact rule names, which a wildcard's apex adds nothing to
    let exact: BTreeSet<&str> = rules
        .iter()
        .filter(|rule| !rule.wildcard)
        .filter_map(|rule| sendable(rule).ok())
        .collect();
    for rule in rules {
        let domain = match sendable(rule) {
            Ok(domain) => domain,
            Err(why) => {
                notes.push(DomainNote {
                    rule: rule.clone(),
                    change: DomainChange::NotSent(why),
                });
                continue;
            }
        };
        let mut entry = domain.to_string();
        let mut by = Vec::new();
        if rule.segments().next().is_some() {
            match (tool, plain_path(rule)) {
                (ServerTool::WebSearch, Some(path)) => {
                    entry.push_str(&path);
                    by.push(Widening::PathPrefix);
                }
                _ => by.push(Widening::Path),
            }
        }
        if rule.url.port().is_some() {
            by.push(Widening::Port);
        }
        if rule.wildcard && !exact.contains(domain) {
            by.push(Widening::Apex);
        }
        sent.insert(entry.clone());
        if !by.is_empty() {
            notes.push(DomainNote {
                rule: rule.clone(),
                change: DomainChange::Widened { entry, by },
            });
        }
    }
    let filter = if !sent.is_empty() {
        DomainFilter::Allowed(sent.into_iter().collect())
    } else if rules.is_empty() {
        DomainFilter::Unfiltered
    } else {
        DomainFilter::NoDomain
    };
    ToolDomains {
        tool,
        filter,
        notes,
        mode: Mode::Allow,
    }
}

fn blocked(rules: &[LinkRule], tool: ServerTool) -> ToolDomains {
    let has_path =
        |rule: &LinkRule| !loose_segments(rule.url.path()).is_empty();
    // The rules that deny every path and port of one host
    let whole: BTreeSet<&str> = rules
        .iter()
        .filter(|rule| {
            !has_path(rule) && rule.url.port().is_none() && !rule.wildcard
        })
        .filter_map(|rule| sendable(rule).ok())
        .collect();
    // Under a whole-host entry: the host itself, or a subdomain of it
    let covered = |domain: &str| {
        whole.iter().any(|entry| {
            domain == *entry
                || domain
                    .strip_suffix(entry)
                    .is_some_and(|sub| sub.ends_with('.'))
        })
    };
    let mut sent: BTreeSet<String> =
        whole.iter().map(|domain| domain.to_string()).collect();
    let mut notes = Vec::new();
    for rule in rules {
        let why = match sendable(rule) {
            Err(why) => why,
            Ok(domain) if covered(domain) => continue,
            Ok(_) if tool == ServerTool::WebFetch && has_path(rule) => {
                NotSent::DenyPath
            }
            Ok(_) if rule.url.port().is_some() => NotSent::DenyPort,
            Ok(_) if rule.wildcard => NotSent::DenyApex,
            // Web search and a path, since a whole host is covered
            Ok(domain) => match plain_path(rule) {
                Some(path) => {
                    let entry = [domain, &path, "/"].concat();
                    sent.insert(entry.clone());
                    notes.push(DomainNote {
                        rule: rule.clone(),
                        change: DomainChange::Narrowed { entry },
                    });
                    continue;
                }
                None => NotSent::DenyPathSpelling,
            },
        };
        notes.push(DomainNote {
            rule: rule.clone(),
            change: DomainChange::NotSent(why),
        });
    }
    let filter = if sent.is_empty() {
        DomainFilter::Unfiltered
    } else {
        DomainFilter::Blocked(sent.into_iter().collect())
    };
    ToolDomains {
        tool,
        filter,
        notes,
        mode: Mode::Deny,
    }
}

/// The rule's domain as an entry (without the `*.` of a wildcard), or why
/// it cannot be one. The parse has made it lower case and ASCII (an
/// internationalised name is its `xn--` form) and taken off a trailing
/// dot; this checks it is a domain name all the same, since one bad entry
/// fails every request
fn sendable(rule: &LinkRule) -> Result<&str, NotSent> {
    let domain = match rule.url.host() {
        Some(Host::Domain(domain)) => domain,
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => return Err(NotSent::IpAddress),
        // A rule is http or https, which always has a host
        None => return Err(NotSent::NotHostName),
    };
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return Err(NotSent::SingleLabel);
    }
    let label_ok = |label: &str| {
        (1..=MAX_LABEL_BYTES).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
    };
    if domain.len() > MAX_DOMAIN_BYTES || !labels.into_iter().all(label_ok) {
        return Err(NotSent::NotHostName);
    }
    Ok(domain)
}

/// The rule's path as an entry's (`/a/b`, without a trailing slash), when
/// every segment is non-empty and of letters, digits and `-._~` only: the
/// characters no URL encodes, so the entry means what the rule does
fn plain_path(rule: &LinkRule) -> Option<String> {
    let mut path = String::new();
    for segment in rule.segments() {
        if segment.is_empty()
            || !segment.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'-' | b'.' | b'_' | b'~')
            })
        {
            return None;
        }
        path.push('/');
        path.push_str(segment);
    }
    Some(path)
}

impl ToolDomains {
    /// Which tool the filter is for
    pub fn tool(&self) -> ServerTool {
        self.tool
    }

    /// What the tool is told
    pub fn filter(&self) -> &DomainFilter {
        &self.filter
    }

    /// Each rule widened, narrowed or not sent, in the policy's order. A
    /// rule sent as it is has none
    pub fn notes(&self) -> &[DomainNote] {
        &self.notes
    }

    fn write_notes(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for DomainNote { rule, change } in &self.notes {
            match change {
                DomainChange::Widened { entry, by } => {
                    write!(f, "  widened: {rule} as {entry}, with ")?;
                    let domain = entry.split('/').next().unwrap_or(entry);
                    let path = &entry[domain.len()..];
                    for (i, how) in by.iter().enumerate() {
                        let sep = match i {
                            0 => "",
                            i if i + 1 == by.len() => " and ",
                            _ => ", ",
                        };
                        f.write_str(sep)?;
                        match how {
                            Widening::Path => f.write_str("every path")?,
                            Widening::PathPrefix => write!(
                                f,
                                "every path that begins {path} ({path}x too)"
                            )?,
                            Widening::Port => f.write_str("every port")?,
                            Widening::Apex => write!(f, "{domain} itself")?,
                        }
                    }
                    writeln!(f)?;
                }
                DomainChange::Narrowed { entry } => writeln!(
                    f,
                    "  narrowed: {rule} as {entry}, the pages under it; \
                     the path itself is still defanged when shown"
                )?,
                DomainChange::NotSent(why) => {
                    let shown = match self.mode {
                        Mode::Allow => "shown as written, not reached",
                        Mode::Deny => "still defanged when shown",
                    };
                    writeln!(f, "  not sent: {rule}: {why} ({shown})")?;
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for NotSent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::IpAddress => "an IP address; the tool takes domain names",
            Self::SingleLabel => {
                "a single label; the tool takes domain names of two or more"
            }
            Self::NotHostName => {
                "not a domain name the tool takes, which would fail every \
                 request"
            }
            Self::DenyPath => {
                "it has a path, and blocking the domain would block its \
                 other pages too"
            }
            Self::DenyPathSpelling => {
                "its path has a character other than letters, digits and \
                 -._~ (or an empty segment), so no entry is certain to \
                 block only what the rule does"
            }
            Self::DenyPort => {
                "it has a port, and blocking the domain would block its \
                 other ports too"
            }
            Self::DenyApex => {
                "blocking the domain would block the domain itself too, \
                 which the rule leaves"
            }
        })
    }
}

/// Plain text for an operator: what is sent, a line per entry, then a
/// line per rule widened, narrowed or not sent (every one: nothing is left
/// out), each ended by a newline
impl fmt::Display for ToolDomains {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tool = self.tool;
        let (name, list) = match (&self.filter, self.mode) {
            (DomainFilter::Allowed(list), _) => ("allowed_domains", list),
            (DomainFilter::Blocked(list), _) => ("blocked_domains", list),
            (DomainFilter::Unfiltered, Mode::Allow) => {
                return writeln!(
                    f,
                    "{tool}: no domain filter (no link is allowed; the tool \
                     is as configured)"
                );
            }
            (DomainFilter::Unfiltered, Mode::Deny) => {
                writeln!(
                    f,
                    "{tool}: no domain filter (no deny rule can be sent)"
                )?;
                return self.write_notes(f);
            }
            (DomainFilter::NoDomain, _) => {
                writeln!(
                    f,
                    "{tool}: nothing can be allowed (no allow rule can be \
                     sent), so offer no {tool} tool"
                )?;
                return self.write_notes(f);
            }
        };
        writeln!(f, "{tool}: {name}, {}", list.len())?;
        list.iter().try_for_each(|entry| writeln!(f, "  {entry}"))?;
        match tool {
            ServerTool::WebFetch => writeln!(
                f,
                "  each with its subdomains, over http and https: web fetch \
                 matches on the domain alone"
            )?,
            ServerTool::WebSearch => writeln!(
                f,
                "  each with its subdomains, over http and https, and every \
                 path that begins with its own"
            )?,
        }
        self.write_notes(f)
    }
}
