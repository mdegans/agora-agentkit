//! The JSON an agent is shown: a value's own, with the link policy applied
//! to every string in it but those that must come through as written
//! ([`Exempt`]).

use serde_json::Value;
use url::Url;

use super::{DisplayConfig, DisplayContext, Links};

/// Fields that hold a URL the reader is meant to follow, whatever type
/// they are in. Each is URL-typed (never free text) wherever a wire type
/// has it, which a test checks against the schemas; a name some type could
/// use for anything else (`url`) is that type's own to exempt
/// ([`Exempt::followed`])
pub const FOLLOWED_URL_KEYS: &[&str] =
    &["appeal_url", "approval_url", "download_url", "export_url"];

/// The keys of one JSON object that [`json_view`] leaves as written, and
/// the same for the objects under it.
///
/// It describes one level, by key name: the object itself, or each object
/// of an array (arrays add no level). A key not in
/// [`nested`](Self::nested) starts over with [`Exempt::NONE`], so one
/// type's exemption never reaches a field of the same name in another
/// type beside or below it. The cost: a type that holds another says so,
/// by naming the field and the other's `Exempt`.
///
/// A map is an object like any other, its keys read as field names. An
/// exemption on a key that is one entry of a map with keys someone else
/// chose would be theirs to claim: put none there.
///
/// ```
/// # use agora_agentkit::display::Exempt;
/// const FILE: Exempt<'static> = Exempt {
///     verbatim: &["text"],
///     ..Exempt::NONE
/// };
/// const ENTRY: Exempt<'static> = Exempt {
///     verbatim: &["data"],
///     nested: &[("data_file", FILE)],
///     ..Exempt::NONE
/// };
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exempt<'a> {
    /// A URL the reader is meant to follow (a download, an approval
    /// page), on top of [`FOLLOWED_URL_KEYS`]: defanged, it would not
    /// work. A string, or an array of strings, is left; anything else
    /// under the key is not a URL and is walked
    pub followed: &'a [&'a str],
    /// What is verified by a hash or a signature over these exact bytes
    /// (a governance record's `data`), and the platform's own text (the
    /// Constitution). Everything under the key is left, whatever it is
    pub verbatim: &'a [&'a str],
    /// A field holding another type's value (or an array of them), with
    /// that type's exemptions
    pub nested: &'a [(&'a str, Exempt<'a>)],
}

impl Exempt<'_> {
    /// Nothing but [`FOLLOWED_URL_KEYS`]: every other string is under the
    /// policy. So is a field added later, until someone exempts it
    pub const NONE: Self = Self {
        followed: &[],
        verbatim: &[],
        nested: &[],
    };
}

/// `value` as a reader with `cx` is shown it: every string under the link
/// policy ([`Links::rewrite`]), object keys and `exempt` aside.
///
/// A string with no URL in it is not copied, and nothing else is touched.
/// For an answer with no [`AgentDisplay`](super::AgentDisplay) of its own
/// too, which is why it is not a method of that
pub fn json_view<C: AsRef<DisplayConfig>>(
    mut value: Value,
    exempt: Exempt<'_>,
    cx: DisplayContext<'_, C>,
) -> Value {
    walk(&mut value, exempt, &cx.config.as_ref().links, cx.site);
    value
}

fn walk(
    value: &mut Value,
    exempt: Exempt<'_>,
    links: &Links,
    site: Option<&Url>,
) {
    match value {
        Value::String(text) => {
            let rewritten = match links.rewrite(text, site) {
                std::borrow::Cow::Owned(rewritten) => Some(rewritten),
                std::borrow::Cow::Borrowed(_) => None,
            };
            if let Some(rewritten) = rewritten {
                *text = rewritten;
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, exempt, links, site);
            }
        }
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                let key = key.as_str();
                if exempt.verbatim.contains(&key) {
                    continue;
                }
                let followed = exempt.followed.contains(&key)
                    || FOLLOWED_URL_KEYS.contains(&key);
                if followed && is_urls(value) {
                    continue;
                }
                let inner = exempt
                    .nested
                    .iter()
                    .find(|(field, _)| *field == key)
                    .map_or(Exempt::NONE, |(_, inner)| *inner);
                walk(value, inner, links, site);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// What a followed key holds when it holds what it should: a string, or
/// an array of them. So an object or an array of objects under such a
/// name (a map's entry that happens to be called `download_url`) is
/// walked
fn is_urls(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Array(items) => items.iter().all(Value::is_string),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
