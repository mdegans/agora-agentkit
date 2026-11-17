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
//! So a rule's scheme and port cannot be said, and every entry also covers
//! its domain's subdomains, over both schemes and on any port. That is true of almost every
//! rule, so it is said once ([`ToolDomains`]'s `Display`), not per rule;
//! what is said per rule ([`DomainNote`]) is what goes further than that.
//!
//! - **Allowed**: an entry reaches more than its rule allows (a rule for
//!   `example.org` does not allow `docs.example.org`), so a link shown
//!   followable is reached, and some that are shown defanged are too.
//! - **Blocked**: an entry blocks more than its rule denies:
//!   [`LinkRule::denies`] takes the rule's own host, so `deny
//!   https://example.org` defangs no link to `blog.example.org`, but the
//!   entry `example.org` blocks it. That is accepted: blocking a
//!   subdomain of a denied host loses little, where a path rule's entry
//!   would block the rest of an innocent site. So only a rule that denies
//!   a whole host is sent, for either tool. Web search documents
//!   `domain/path/*` as an entry, which could carry a deny rule's path
//!   exactly; it is not used until its matching is verified.
//!
//! Two things are assumed of Anthropic's matching, not documented: a path
//! matches with its letter case, as a URL's path does (RFC 3986) and as
//! [`LinkRule`] matches it (an allowed web search path); and a host
//! written with a trailing dot (`example.org.`), which a rule allows like
//! the host without it, may not be covered by the entry.
//!
//! An entry is only ever made of the parsed rule's parts, and only of
//! characters certain to be valid: a rule that would need others is
//! widened to its domain (`allow`) or not sent (`deny`), and says so.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use url::Host;

use super::{LinkRule, Links};

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
/// [`Links::search_domains`]). A runner uses [`offer`](Self::offer)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDomains {
    tool: ServerTool,
    filter: DomainFilter,
    notes: Vec<DomainNote>,
    /// Which list the rules were, and whether there were any
    mode: Mode,
    rules: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Allow,
    Deny,
}

/// What the tool is told. Every entry's domain is ASCII, lower case, a
/// domain name of two labels or more of letters, digits and inner
/// hyphens, without a trailing dot; a web search allow entry may follow it
/// with a path of letters, digits and `-._~` between slashes. The lists
/// are sorted, have no duplicates, and are never empty
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainFilter {
    /// Neither list is set: the tool is as the operator configured it. An
    /// empty `allow` list, and a `raw` policy with no deny rule sent
    Unfiltered,
    /// `allowed_domains`: only these (and their subdomains) are reached
    Allowed(Vec<String>),
    /// `blocked_domains`: these (and their subdomains) are not reached
    Blocked(Vec<String>),
    /// Do not offer the tool. The `allow` list has rules but none can be
    /// sent (each is an IP address or a single label, say): "only these"
    /// would become "anything" unfiltered, and an empty `allowed_domains`
    /// is not one the tool documents
    Withhold,
}

/// Whether to offer a tool, and its filter: [`ToolDomains::offer`]
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "`Withhold` means the tool must not be offered"]
pub enum ToolOffer {
    /// Do not offer the tool ([`DomainFilter::Withhold`])
    Withhold,
    /// Offer it with these fields, as misanthropic's `WebFetch` and
    /// `WebSearch` hold them: at most one is `Some`, and `Some` is never
    /// empty. Both `None` leaves the tool as the operator configured it
    Offer {
        allowed_domains: Option<Vec<Cow<'static, str>>>,
        blocked_domains: Option<Vec<Cow<'static, str>>>,
    },
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
    /// Not sent: the tool is not gated by this rule. Under `raw` the tool
    /// can still reach what the rule denies (the display still defangs
    /// it); under `defanged` a link the rule allows is shown as written
    /// and the tool will not reach it
    NotSent(NotSent),
}

/// How a sent `allow` rule is widened, beyond the subdomains, the other
/// scheme and the other ports every entry covers
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
    /// The rule names a port: every port is reached (as for any rule, but
    /// this one was written to one)
    Port,
    /// The rule is `*.domain`, whose entry `domain` always reaches
    /// `domain` itself too. Noted unless another rule allows the whole of
    /// `domain` itself (no path, no port), when the entry adds nothing
    /// there that rule does not
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
    /// A `deny` rule with a path: blocking the domain would block its
    /// other pages too
    DenyPath,
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
    ///   [`DomainFilter::Withhold`].
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

    /// This policy as web search's domain filter, with each rule it widens
    /// or leaves out. As [`fetch_domains`](Self::fetch_domains) but for an
    /// `allow` rule's path, which web search takes as a prefix: sent as
    /// `domain/path`, widened only to the paths that begin with it
    /// ([`Widening::PathPrefix`]). A path with a character other than
    /// letters, digits and `-._~` is widened to the domain. A `deny` rule
    /// with a path is not sent, as for web fetch (see the module docs).
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

/// Whether `rule` takes every path and port of one host, as `mode` reads
/// it, and is not a wildcard
fn whole_host(rule: &LinkRule, mode: Mode) -> bool {
    let some_paths = match mode {
        Mode::Allow => rule.allows_some_paths(),
        Mode::Deny => rule.denies_some_paths(),
    };
    !rule.wildcard && !some_paths && rule.url.port().is_none()
}

/// Whether an entry for `domain` reaches `host` (Anthropic's rule: the
/// domain or a subdomain of it). Not [`LinkRule`]'s host matching, which
/// takes an exact host alone and a wildcard's subdomains without the apex
fn entry_covers(domain: &str, host: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|sub| sub.ends_with('.'))
}

/// The domains of the rules in `rules` that take a whole host
fn whole_domains(rules: &[LinkRule], mode: Mode) -> BTreeSet<&str> {
    rules
        .iter()
        .filter(|rule| whole_host(rule, mode))
        .filter_map(|rule| sendable(rule).ok())
        .collect()
}

fn allowed(rules: &[LinkRule], tool: ServerTool) -> ToolDomains {
    let mut sent = BTreeSet::new();
    let mut notes = Vec::new();
    let whole = whole_domains(rules, Mode::Allow);
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
        if rule.allows_some_paths() {
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
        if rule.wildcard && !whole.contains(domain) {
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
        DomainFilter::Withhold
    };
    ToolDomains {
        tool,
        filter,
        notes,
        mode: Mode::Allow,
        rules: rules.len(),
    }
}

fn blocked(rules: &[LinkRule], tool: ServerTool) -> ToolDomains {
    let whole = whole_domains(rules, Mode::Deny);
    let covered =
        |host: &str| whole.iter().any(|domain| entry_covers(domain, host));
    let mut notes = Vec::new();
    for rule in rules {
        let why = match sendable(rule) {
            Err(why) => why,
            Ok(domain) if covered(domain) => continue,
            Ok(_) if rule.denies_some_paths() => NotSent::DenyPath,
            Ok(_) if rule.url.port().is_some() => NotSent::DenyPort,
            Ok(_) => NotSent::DenyApex,
        };
        notes.push(DomainNote {
            rule: rule.clone(),
            change: DomainChange::NotSent(why),
        });
    }
    let filter = if whole.is_empty() {
        DomainFilter::Unfiltered
    } else {
        DomainFilter::Blocked(whole.iter().map(|d| d.to_string()).collect())
    };
    ToolDomains {
        tool,
        filter,
        notes,
        mode: Mode::Deny,
        rules: rules.len(),
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

    /// Each rule widened or not sent, in the policy's order. A rule sent
    /// as it is has none
    pub fn notes(&self) -> &[DomainNote] {
        &self.notes
    }

    /// Whether to offer the tool, and the fields to set on it: what a
    /// runner uses.
    ///
    /// Mind the cliff between two `defanged` policies. An **empty** allow
    /// list offers the tool unfiltered, as the operator configured it (it
    /// says what is shown, not what is fetched; see
    /// [`Links::fetch_domains`]). An allow list holding **only rules that
    /// cannot be sent** (`http://localhost`, an IP address) is
    /// [`ToolOffer::Withhold`]: "only these" must never become "anything".
    pub fn offer(&self) -> ToolOffer {
        let list = |list: &[String]| {
            Some(list.iter().map(|d| Cow::Owned(d.clone())).collect())
        };
        match &self.filter {
            DomainFilter::Withhold => ToolOffer::Withhold,
            DomainFilter::Unfiltered => ToolOffer::Offer {
                allowed_domains: None,
                blocked_domains: None,
            },
            DomainFilter::Allowed(allowed) => ToolOffer::Offer {
                allowed_domains: list(allowed),
                blocked_domains: None,
            },
            DomainFilter::Blocked(blocked) => ToolOffer::Offer {
                allowed_domains: None,
                blocked_domains: list(blocked),
            },
        }
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
                                "every path that begins with {path} \
                                 ({path}x too)"
                            )?,
                            Widening::Port => f.write_str("every port")?,
                            Widening::Apex => write!(f, "{domain} itself")?,
                        }
                    }
                    writeln!(f)?;
                }
                DomainChange::NotSent(why) => {
                    let then = match self.mode {
                        Mode::Allow => {
                            "shown as written; the tool will not reach it"
                        }
                        Mode::Deny => {
                            "the tool can still reach it; the display still \
                             defangs it"
                        }
                    };
                    writeln!(f, "  not sent: {rule}: {why} ({then})")?;
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
/// line per rule widened or not sent (every one: nothing is left out),
/// each ended by a newline
impl fmt::Display for ToolDomains {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tool = self.tool;
        let (name, list) = match (&self.filter, self.mode) {
            (DomainFilter::Allowed(list), _) => ("allowed_domains", list),
            (DomainFilter::Blocked(list), _) => ("blocked_domains", list),
            (DomainFilter::Unfiltered, Mode::Allow) => {
                return writeln!(
                    f,
                    "{tool}: no allow rule: links to other sites are shown \
                     defanged; the tool is as configured"
                );
            }
            (DomainFilter::Unfiltered, Mode::Deny) if self.rules == 0 => {
                return writeln!(
                    f,
                    "{tool}: no deny rule; the tool is as configured"
                );
            }
            (DomainFilter::Unfiltered, Mode::Deny) => {
                writeln!(
                    f,
                    "{tool}: no deny rule can be sent; the tool is as \
                     configured"
                )?;
                return self.write_notes(f);
            }
            (DomainFilter::Withhold, _) => {
                writeln!(
                    f,
                    "{tool}: withheld: no allow rule can be sent, and \
                     unfiltered it would reach anything"
                )?;
                return self.write_notes(f);
            }
        };
        writeln!(f, "{tool}: {name}, {}", list.len())?;
        list.iter().try_for_each(|entry| writeln!(f, "  {entry}"))?;
        match (self.mode, tool) {
            (Mode::Allow, ServerTool::WebFetch) => writeln!(
                f,
                "  each also reaches its subdomains, over http and https, \
                 on any port, and every path: web fetch matches on the \
                 domain alone"
            )?,
            (Mode::Allow, ServerTool::WebSearch) => writeln!(
                f,
                "  each also reaches its subdomains, over http and https, \
                 on any port, and every path that begins with its own"
            )?,
            (Mode::Deny, _) => writeln!(
                f,
                "  each also blocks its subdomains, over http and https, \
                 which the rule for it does not deny"
            )?,
        }
        self.write_notes(f)
    }
}
