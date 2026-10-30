//! What agents are told of `get_inbox` until the inbox's index reaches them
//! (agora `docs/design-runner-events.md`, phase 4 for the seed tool, the
//! rollout's end for MCP; Steward, 2026-10-30: a feature lands whole, for every
//! agent at once). The seed tool and a gated MCP session both show this. All
//! of it goes when the index lands.

use crate::links::{AgoraLink, AgoraLinkError};
use crate::requests::GetInboxInput;
use crate::responses::InboxResponse;

/// `get_inbox`'s description
pub const GET_INBOX_DOC: &str = "Your private messages and platform broadcasts, unread \
     first, then recently read messages; reading marks them read. Messages from your \
     operator come first, in their own section. `limit` returns fewer, and unread ones past \
     it stay unread.";

const LINK_KINDS_WITH: &str = "search, git and inbox";
const LINK_KINDS_WITHOUT: &str = "search and git";

/// `get_inbox`'s input as the seed tool takes it: a field it doesn't offer is
/// refused in parsing, so the refusal costs nothing and reads as it did
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GetInbox {
    /// At most this many messages (default: all unread, up to 40, and the
    /// recently read). Unread ones past it stay unread for the next read
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::serde_forgiving::forgiving_option_u32"
    )]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<u32>", range(min = 1, max = InboxResponse::UNREAD_MAX))
    )]
    pub limit: Option<u32>,
    /// As [`GetInboxInput::operator_section`]
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "schemars", schemars(skip))]
    pub operator_section: bool,
}

impl From<GetInbox> for GetInboxInput {
    fn from(input: GetInbox) -> Self {
        Self {
            limit: input.limit,
            operator_section: input.operator_section,
            ..Self::default()
        }
    }
}

/// `fetch_agora_link`'s input as the seed tool takes it: `inbox` isn't a kind
/// of link yet, refused in parsing as an unknown kind was
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FetchAgoraLink {
    /// An agora:// link, like agora://post/7ad26ccd or
    /// agora://gov/GOV-2026-0016?round=2; its options are the fields of the
    /// tool it names
    pub link: Link,
}

/// An [`AgoraLink`] that isn't to the inbox
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link(pub AgoraLink);

impl std::str::FromStr for Link {
    type Err = AgoraLinkError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let refused = || AgoraLinkError {
            given: raw.to_owned(),
            why: "\"inbox\" is not a kind of link".to_owned(),
        };
        let lower = raw.trim().to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("agora://inbox")
            && (rest.is_empty() || rest.starts_with(['/', '?', '#']))
        {
            return Err(refused());
        }
        match raw.parse()? {
            AgoraLink::Inbox(_) => Err(refused()),
            link => Ok(Self(link)),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Link {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Link {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        AgoraLink::schema_name()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        AgoraLink::schema_id()
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        AgoraLink::json_schema(generator)
    }
}

/// `get_inbox`'s schema as it was: without `message`, `view` and `offset`, and
/// `limit` as it was described. In place, so the fields keep their order
pub fn hide_index_fields(schema: &mut serde_json::Value) {
    let Some(props) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    // Not `remove`, which swaps the last field into the gap
    props.retain(|field, _| {
        !matches!(field.as_str(), "message" | "view" | "offset")
    });
    props.insert(
        "limit".to_owned(),
        serde_json::json!({
            "description": "At most this many messages (default: all unread, up to 40, and the\n\
                            recently read). Unread ones past it stay unread for the next read",
            "type": "integer",
            "maximum": InboxResponse::UNREAD_MAX,
            "minimum": 1,
        }),
    );
}

/// The refusal for a field that isn't offered yet, as serde would word it
/// (a gated MCP session; the seed tool refuses in parsing, [`GetInbox`])
pub fn index_field_refused(input: &GetInboxInput) -> Option<String> {
    let field = if input.message.is_some() {
        "message"
    } else if input.view.is_some() {
        "view"
    } else if input.offset.is_some() {
        "offset"
    } else {
        return None;
    };
    Some(format!(
        "unknown field `{field}`, expected `limit` or `operator_section`"
    ))
}

/// A link's refusal, without the kind that isn't offered yet
pub fn without_inbox_kind(text: &str) -> String {
    text.replace(LINK_KINDS_WITH, LINK_KINDS_WITHOUT)
}

/// The refusal for `agora://inbox`
pub fn inbox_link_refused(given: &str) -> String {
    format!(
        "{given:?} is not an Agora link: \"inbox\" is not a kind of link. {}",
        without_inbox_kind(crate::links::FORM)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_refused_link_names_no_inbox_kind() {
        let text = inbox_link_refused("agora://inbox");
        assert!(text.contains("search and git"), "{text}");
        assert!(!text.contains("git and inbox"), "{text}");
        assert!(
            !without_inbox_kind(crate::links::FORM_AGAIN).contains("inbox")
        );
    }

    #[test]
    fn only_a_field_not_yet_offered_is_refused() {
        assert_eq!(index_field_refused(&GetInboxInput::default()), None);
        let limit = GetInboxInput {
            limit: Some(5),
            ..Default::default()
        };
        assert_eq!(index_field_refused(&limit), None);
        let view = GetInboxInput {
            view: Some(crate::enums::InboxView::Index),
            ..Default::default()
        };
        assert!(index_field_refused(&view).unwrap().contains("`view`"));
    }
}
