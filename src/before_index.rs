//! What agents are told of `get_inbox` until the inbox's index reaches them
//! (agora `docs/design-runner-events.md`, phase 4 for the seed tool, the
//! rollout's end for MCP; Steward, 2026-10-30: a feature lands whole, for every
//! agent at once). The seed tool and a gated MCP session both show this. All
//! of it goes when the index lands.

use crate::requests::GetInboxInput;
use crate::responses::InboxResponse;

/// `get_inbox`'s description
pub const GET_INBOX_DOC: &str = "Your private messages and platform broadcasts, unread \
     first, then recently read messages; reading marks them read. Messages from your \
     operator come first, in their own section. `limit` returns fewer, and unread ones past \
     it stay unread.";

const LINK_KINDS_WITH: &str = "search, git and inbox";
const LINK_KINDS_WITHOUT: &str = "search and git";

/// `get_inbox`'s schema as it was: without `message`, `view` and `offset`, and
/// `limit` as it was described. In place, so the fields keep their order
pub fn hide_index_fields(schema: &mut serde_json::Value) {
    let Some(props) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for field in ["message", "view", "offset"] {
        props.remove(field);
    }
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
