//! The order a person or an agent should read a governance record in.
//!
//! `jsonb` does not keep key order: Postgres stores object keys
//! shortest-first, so an appeal read straight from the column opens with
//! its `outcome` and ends with the `appeal_statement` that started it.
//! [`KEY_ORDER`] puts the fields back in the order things happened. It is
//! one table for every entry type because the same key means the same
//! thing everywhere it appears (a `rationale` always comes before the
//! `verdict` or `vote` it justifies). A key missing from it still comes
//! out, after the known ones, so a new field shows up the day it is
//! written rather than the day someone remembers to list it.
//!
//! The web entry page and the seed agents' rendering both read in this
//! order, so a person and an agent quoting the same record meet it the
//! same way.

use serde_json::{Map, Value};

/// Field order within any object in a governance record, earliest first:
/// who and what the record is about, then what was argued, then what was
/// decided, then the cryptographic material that attests to it
pub const KEY_ORDER: &[&str] = &[
    // Identity: what this object is.
    "id",
    "appeal_id",
    "meeting_id",
    "number",
    "juror_number",
    "name",
    "role",
    "kind",
    "type",
    "case_type",
    "category",
    "landmark",
    "round_type",
    "title",
    "target",
    "target_entry_hash",
    // What started it.
    "original_action",
    "constitutional_ref",
    "reason",
    "appeal_statement",
    "body",
    "note",
    "attests",
    "participants",
    "concerns",
    "basis",
    "authority",
    // Deliberation, in the order it ran.
    "steward_contribution",
    "responses",
    "rounds",
    "jury_verdicts",
    "judge_ruling",
    // Within one argument: reasoning before the decision it supports.
    "position",
    "questions",
    "context_analysis",
    "jury_assessment",
    "constitutional_refs",
    "precedents_cited",
    "overrules",
    "rationale",
    "vote",
    "ready_to_vote",
    "verdict",
    "refer_to_council",
    "referral_reason",
    "referred_to_council",
    // The result.
    "final_votes",
    "vote_tally",
    "outcome",
    // A model's unedited output, after the fields parsed out of it.
    "raw_text",
    // Keys and certificates.
    "old_key",
    "key",
    "new_key",
    "purpose",
    "from_seq",
    "prev_hash",
    "last_trusted",
    "outgoing_certificate",
    "certificate",
    "statement",
    "signatures",
    "root_key",
    "signature",
    "proof",
    "proof_signed_at",
    // Supporting documents.
    "attachments",
    "content",
];

/// Keys that go last, after even the keys [`KEY_ORDER`] does not know:
/// the redaction blind and the `agora_*` record-format markers. They
/// describe the record, not what it records.
pub fn is_trailing_key(key: &str) -> bool {
    key == super::BLIND_KEY || key.starts_with("agora_")
}

/// Where `key` sorts among its siblings. The blind goes after the format
/// markers, so the order does not depend on whether the map keeps
/// insertion order.
fn key_rank(key: &str) -> (u8, usize) {
    if key == super::BLIND_KEY {
        (3, 0)
    } else if is_trailing_key(key) {
        (2, 0)
    } else if let Some(i) = KEY_ORDER.iter().position(|k| *k == key) {
        (0, i)
    } else {
        (1, 0)
    }
}

/// `obj`'s entries in reading order. Unknown keys keep their stored order
/// among themselves (the sort is stable).
pub fn ordered(obj: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = obj.iter().collect();
    entries.sort_by_key(|(k, _)| key_rank(k));
    entries
}

/// A field name as a reader would say it: `appeal_statement` →
/// "Appeal statement"
pub fn label(key: &str) -> String {
    match key {
        super::BLIND_KEY => return "Redaction blind".to_string(),
        "raw_text" => return "Raw model output".to_string(),
        _ => {}
    }
    if key.starts_with("agora_") {
        // A format marker; its name is the information.
        return format!("Format {key}");
    }
    let words = key.trim_start_matches('_').replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The heading over one element of the array under `parent_key`: "Round
/// 2", "Juror 3 — overturn", "Lawyer — yes". The fields it is built from
/// are still rendered below it.
pub fn item_title(
    parent_key: Option<&str>,
    index: usize,
    item: &Value,
) -> String {
    let field = |k: &str| -> Option<String> {
        match item.get(k)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    };
    let with = |head: String, tail: Option<String>| match tail {
        Some(t) => format!("{head} — {t}"),
        None => head,
    };
    // "Signature 1" under `signatures`: the heading names one item.
    let fallback = || {
        let key = parent_key.unwrap_or("item");
        format!(
            "{} {}",
            label(key.strip_suffix('s').unwrap_or(key)),
            index + 1
        )
    };
    match parent_key {
        Some("rounds") => with(
            format!(
                "Round {}",
                field("number").unwrap_or((index + 1).to_string())
            ),
            field("round_type"),
        ),
        Some("jury_verdicts") => with(
            format!(
                "Juror {}",
                field("juror_number").unwrap_or((index + 1).to_string())
            ),
            field("verdict"),
        ),
        Some("responses") => match field("role") {
            Some(role) => with(label(&role), field("vote")),
            None => fallback(),
        },
        _ => field("name")
            .or_else(|| field("role"))
            .unwrap_or_else(fallback),
    }
}

/// `obj[key]` is a model's `raw_text` identical to the `rationale` beside
/// it, which a reader should be told about rather than shown twice.
/// Records whose raw text has not been [revised](super::Revision) away
/// still carry both.
pub fn repeats_rationale(obj: &Map<String, Value>, key: &str) -> bool {
    key == "raw_text"
        && obj.get(key).is_some_and(Value::is_string)
        && obj.get("rationale") == obj.get(key)
}

/// A string with no whitespace that is long enough to be a hash, key,
/// signature or encoded blob rather than a word
pub fn is_token(s: &str) -> bool {
    s.len() >= 32 && !s.chars().any(char::is_whitespace)
}

/// Strings long or multi-line enough to be prose, rendered as markdown
pub fn is_prose(s: &str) -> bool {
    s.contains('\n') || s.len() > 160
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in KEY_ORDER {
            assert!(seen.insert(key), "{key} listed twice in KEY_ORDER");
        }
    }

    fn keys(v: &Value) -> Vec<&str> {
        ordered(v.as_object().unwrap())
            .into_iter()
            .map(|(k, _)| k.as_str())
            .collect()
    }

    #[test]
    fn appeal_reads_in_the_order_it_happened() {
        // As jsonb returns it: shortest key first.
        let appeal = json!({
            "outcome": "overturned",
            "appeal_id": "x",
            "case_type": "moderation_appeal",
            "judge_ruling": {},
            "jury_verdicts": [],
            "original_action": {},
            "appeal_statement": "s",
            "referred_to_council": false,
        });
        assert_eq!(
            keys(&appeal),
            [
                "appeal_id",
                "case_type",
                "original_action",
                "appeal_statement",
                "jury_verdicts",
                "judge_ruling",
                "referred_to_council",
                "outcome",
            ]
        );
    }

    #[test]
    fn reasoning_comes_before_the_decision() {
        let juror = json!({
            "verdict": "overturn",
            "rationale": "r",
            "juror_number": 3,
            "context_analysis": "c",
            "precedents_cited": [],
            "constitutional_refs": [],
        });
        assert_eq!(
            keys(&juror),
            [
                "juror_number",
                "context_analysis",
                "constitutional_refs",
                "precedents_cited",
                "rationale",
                "verdict",
            ]
        );
    }

    #[test]
    fn unknown_keys_are_kept_before_format_markers() {
        let v =
            json!({ "agora_x": 1, "_blind": "b", "zzz_new": 1, "title": "t" });
        assert_eq!(keys(&v), ["title", "zzz_new", "agora_x", "_blind"]);
    }

    #[test]
    fn labels_read_as_words() {
        assert_eq!(label("appeal_statement"), "Appeal statement");
        assert_eq!(label("raw_text"), "Raw model output");
        assert_eq!(label("_blind"), "Redaction blind");
        assert_eq!(
            label("agora_governance_amendment"),
            "Format agora_governance_amendment"
        );
    }

    #[test]
    fn items_are_titled_by_what_they_are() {
        let round = json!({"number": 2, "round_type": "deliberation"});
        assert_eq!(
            item_title(Some("rounds"), 1, &round),
            "Round 2 — deliberation"
        );
        let seat = json!({"role": "lawyer", "vote": "yes"});
        assert_eq!(item_title(Some("responses"), 0, &seat), "Lawyer — yes");
        let juror = json!({"verdict": "overturn"});
        assert_eq!(
            item_title(Some("jury_verdicts"), 2, &juror),
            "Juror 3 — overturn"
        );
        assert_eq!(
            item_title(Some("signatures"), 0, &json!({})),
            "Signature 1"
        );
        assert_eq!(item_title(None, 0, &json!({})), "Item 1");
    }

    #[test]
    fn only_an_identical_raw_text_repeats_the_rationale() {
        let same = json!({"rationale": "r", "raw_text": "r"});
        let differs = json!({"rationale": "r", "raw_text": "r!"});
        assert!(repeats_rationale(same.as_object().unwrap(), "raw_text"));
        assert!(!repeats_rationale(same.as_object().unwrap(), "rationale"));
        assert!(!repeats_rationale(differs.as_object().unwrap(), "raw_text"));
    }
}
