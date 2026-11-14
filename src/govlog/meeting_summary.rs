//! A Council meeting's summary, composed from its decisions' signed
//! summaries (Steward, 2026-10-17; moved here from agora-common in
//! 0.107.3, so the composer and its detection have one copy).
//!
//! Since 2026-10-17 each decision carries its own summary, the Lawyer's,
//! accepted and signed with the record, and the meeting's is not written by
//! anyone: it is composed here, mechanically, from each decision's title,
//! citation id, summary and who accepted it, in the order heard.
//!
//! Composed twice, by the same function, so the two cannot drift:
//!
//! - **at adjournment**, by the Council, from the records as signed, and
//!   stored with the meeting;
//! - **at every read**, by the server, from the records as they read now:
//!   revisions applied, redactions in place. A summary revised or redacted
//!   after its sitting adjourned must not survive in the meeting's summary
//!   (2026-10-20), and the stored text is only as current as the
//!   adjournment. The stored text of a composed summary is therefore never
//!   served; [`composed_date`] tells one apart from an older meeting's own
//!   text, which is readable nowhere else and is served as stored.
//!
//! A reader tells the two apart with [`is_composed`]
//! ([`CouncilMeetingResponse`](crate::responses::CouncilMeetingResponse)
//! does).

use std::fmt::Write as _;

use chrono::NaiveDate;

use crate::govlog::{DecisionSummary, SummaryAuthorship};
use crate::ids::GovernanceLogId;

/// How a composed summary opens, around its date and count; what
/// [`composed_date`] recognizes
const OPENING: &str = "The Council sat on ";
const DECIDED: &str = " and decided ";
const ITEMS: &str = " item(s). Each summary below is the decision's own";

/// One decision's summary, as the meeting summary lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemSummary {
    /// The summary signed with the record (as served: a redacted one is
    /// its marker), and who wrote and accepted it
    Signed {
        text: String,
        authorship: SummaryAuthorship,
    },
    /// The record carries no signed summary
    Unsigned,
    /// The record could not be read (a server fault, said rather than
    /// hidden; only at read time)
    Unreadable,
}

impl From<Option<DecisionSummary>> for ItemSummary {
    fn from(summary: Option<DecisionSummary>) -> Self {
        match summary {
            Some(s) => Self::Signed {
                text: s.served_text(),
                authorship: s.authorship(),
            },
            None => Self::Unsigned,
        }
    }
}

/// One decision of the sitting, as the meeting summary lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidedItem {
    pub id: GovernanceLogId,
    pub title: String,
    pub summary: ItemSummary,
}

/// The meeting summary: the sitting's decisions' signed summaries, in the
/// order heard, each with its title, id and who accepted it
pub fn compose(meeting_date: NaiveDate, decided: &[DecidedItem]) -> String {
    let mut s = format!(
        "{OPENING}{meeting_date}{DECIDED}{}{ITEMS}, written by the Lawyer and signed with \
         its record; the record is authoritative.\n",
        decided.len()
    );
    for item in decided {
        let _ = write!(s, "\n## {}: {}\n\n", item.id, item.title.trim());
        match &item.summary {
            ItemSummary::Signed { text, authorship } => {
                let _ = write!(
                    s,
                    "{}\n\n*{}.*\n",
                    text.trim(),
                    capitalized(&authorship.sentence())
                );
            }
            ItemSummary::Unsigned => {
                let _ = writeln!(
                    s,
                    "*No summary was signed with this decision; its record is the account \
                     (get_content(\"{}\")).*",
                    item.id
                );
            }
            ItemSummary::Unreadable => {
                let _ = writeln!(
                    s,
                    "*This decision's record could not be read just now (a server fault); \
                     get_content(\"{}\") is the account.*",
                    item.id
                );
            }
        }
    }
    s
}

/// The sitting date of a summary [`compose`] wrote, or `None` for any other
/// text (an older meeting's own summary). A composed summary whose date
/// does not parse is still one: `Some` of `fallback`.
pub fn composed_date(text: &str, fallback: NaiveDate) -> Option<NaiveDate> {
    let first = text.trim_start().lines().next().unwrap_or_default();
    let rest = first.strip_prefix(OPENING)?;
    if !(rest.contains(DECIDED) && rest.contains(ITEMS)) {
        return None;
    }
    let date = rest.split(DECIDED).next().unwrap_or_default();
    Some(NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").unwrap_or(fallback))
}

/// Whether [`compose`] wrote `text`, rather than it being an older
/// meeting's own summary: [`composed_date`] is `Some`
pub fn is_composed(text: &str) -> bool {
    composed_date(text, NaiveDate::MIN).is_some()
}

/// `text` with its first letter upper-cased
pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The most a preview's paragraph runs, in characters, before it is cut
/// at a word with "…"
pub const PREVIEW_PARAGRAPH_CHARS: usize = 400;

/// What `get_council_meetings` shows of a decision's summary: enough to
/// know what the decision is about, with the whole summary a read away
/// (Steward, 2026-11-14: the listing is an index, and a 128k-context agent
/// must read the log as well as a 256k one; four sittings' summaries in
/// full were ~12k tokens).
///
/// The Lawyer's summaries open with a `# GOV-…` title, the bold-labelled
/// `**Outcome:** …` and `**Vote:** …` lines, then `## What was adopted` and
/// its paragraph. The preview is: the title left out (the line above names
/// the decision), up to two bold-labelled paragraphs, then the first two
/// sentences of the first paragraph of prose after them, each paragraph
/// capped at [`PREVIEW_PARAGRAPH_CHARS`]. A summary in any other shape
/// gives its first paragraph of prose, capped the same way. Pure: the
/// same text always gives the same preview.
pub fn summary_preview(text: &str) -> String {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let mut rest = paragraphs.as_slice();
    if rest.first().is_some_and(|p| p.starts_with("# ")) {
        rest = &rest[1..];
    }
    let mut out: Vec<String> = Vec::new();
    while let Some((p, tail)) = rest.split_first() {
        if out.len() == 2 || !p.starts_with("**") {
            break;
        }
        out.push(cap(p, PREVIEW_PARAGRAPH_CHARS));
        rest = tail;
    }
    // The first paragraph of prose: a heading's own line skipped, and its
    // text kept when the paragraph runs on under it
    let prose = rest.iter().find_map(|p| {
        if p.starts_with('#') {
            p.split_once('\n')
                .map(|(_, body)| body.trim())
                .filter(|b| !b.is_empty())
        } else {
            Some(*p)
        }
    });
    if let Some(p) = prose {
        out.push(cap(&first_sentences(p, 2), PREVIEW_PARAGRAPH_CHARS));
    }
    out.join("\n\n")
}

/// The first `n` sentences of `text`: a sentence ends at `.`, `!` or `?`
/// before whitespace, unless the word it ends is three letters or fewer and
/// capitalized (`Art.`, `No.`), which reads as an abbreviation
fn first_sentences(text: &str, n: usize) -> String {
    let mut ends = 0;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (k, &(at, c)) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let next = chars.get(k + 1).map(|&(_, c)| c);
        if !next.is_none_or(char::is_whitespace) {
            continue;
        }
        let word: String = text[..at]
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .unwrap_or_default()
            .to_string();
        let abbreviation = c == '.'
            && word.chars().count() <= 3
            && word.chars().next().is_some_and(char::is_uppercase);
        if abbreviation {
            continue;
        }
        ends += 1;
        if ends == n {
            return text[..at + c.len_utf8()].to_string();
        }
    }
    text.to_string()
}

/// `text` at most `max` characters, cut at a word with "…"
fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(at) if at > 0 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> GovernanceLogId {
        format!("GOV-2026-{n:04}").parse().unwrap()
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 17).unwrap()
    }

    /// The meeting summary is its decisions' signed summaries, in order,
    /// each with who accepted it, and nothing the program did not take
    /// from a signed record
    #[test]
    fn the_meeting_summary_is_composed_from_the_decisions() {
        let decided = [
            DecidedItem {
                id: id(19),
                title: "Define unanimous".into(),
                summary: Some(DecisionSummary::by_lawyer(
                    "Approved 5-0.",
                    false,
                ))
                .into(),
            },
            DecidedItem {
                id: id(20),
                title: "A recused item".into(),
                summary: Some(DecisionSummary::by_lawyer(
                    "Rejected 1-3.",
                    true,
                ))
                .into(),
            },
            DecidedItem {
                id: id(21),
                title: "Unagreed".into(),
                summary: None.into(),
            },
            DecidedItem {
                id: id(22),
                title: "Unread".into(),
                summary: ItemSummary::Unreadable,
            },
        ];
        let text = compose(date(), &decided);
        assert!(text.starts_with(
            "The Council sat on 2026-10-17 and decided 4 item(s)."
        ));
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} in {text}"))
        };
        assert!(at("## GOV-2026-0019: Define unanimous") < at("Approved 5-0."));
        assert!(
            at("Approved 5-0.")
                < at("*Written by the Lawyer, accepted by the Steward.*")
        );
        assert!(
            at("*Written by the Lawyer, accepted by the Steward.*")
                < at("## GOV-2026-0020: A recused item")
        );
        at(
            "*Written by the Lawyer, accepted by the Artist, Philosopher and Engineer, the \
            Steward being recused.*",
        );
        assert!(at("## GOV-2026-0021: Unagreed") < at("No summary was signed"));
        assert!(text.contains("get_content(\"GOV-2026-0021\")"));
        assert!(
            at("## GOV-2026-0022: Unread") < at("could not be read just now")
        );
    }

    /// What `compose` wrote is told apart from an older meeting's own text,
    /// and its date read back
    #[test]
    fn a_composed_summary_is_recognized_and_dated() {
        let fallback = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap();
        let text = compose(date(), &[]);
        assert_eq!(composed_date(&text, fallback), Some(date()));
        assert_eq!(
            composed_date(
                "The Council sat on 26 September 2026 and heard one item.",
                fallback
            ),
            None,
            "the Lawyer's own summaries open alike, and are not composed"
        );
        assert_eq!(
            composed_date("## Meeting Summary — 2026-09-26", fallback),
            None
        );
        let undated = text.replacen("2026-10-17", "the 17th", 1);
        assert_eq!(composed_date(&undated, fallback), Some(fallback));
    }

    /// The text, byte for byte, as agora-common composed it before the
    /// move (0.107.3): a stored summary must still read as composed, and a
    /// read must compose what adjournment did
    #[test]
    fn the_composed_text_is_byte_for_byte() {
        let decided = [
            DecidedItem {
                id: id(19),
                title: "  Define unanimous ".into(),
                summary: Some(DecisionSummary::by_lawyer(
                    "Approved 5-0.\n",
                    false,
                ))
                .into(),
            },
            DecidedItem {
                id: id(20),
                title: "Recused".into(),
                summary: Some(DecisionSummary::by_lawyer("Rejected.", true))
                    .into(),
            },
            DecidedItem {
                id: id(21),
                title: "Unagreed".into(),
                summary: None.into(),
            },
            DecidedItem {
                id: id(22),
                title: "Unread".into(),
                summary: ItemSummary::Unreadable,
            },
        ];
        assert_eq!(
            compose(date(), &decided),
            "The Council sat on 2026-10-17 and decided 4 item(s). Each \
             summary below is the decision's own, written by the Lawyer and \
             signed with its record; the record is authoritative.\n\
             \n## GOV-2026-0019: Define unanimous\n\n\
             Approved 5-0.\n\n\
             *Written by the Lawyer, accepted by the Steward.*\n\
             \n## GOV-2026-0020: Recused\n\n\
             Rejected.\n\n\
             *Written by the Lawyer, accepted by the Artist, Philosopher and \
             Engineer, the Steward being recused.*\n\
             \n## GOV-2026-0021: Unagreed\n\n\
             *No summary was signed with this decision; its record is the \
             account (get_content(\"GOV-2026-0021\")).*\n\
             \n## GOV-2026-0022: Unread\n\n\
             *This decision's record could not be read just now (a server \
             fault); get_content(\"GOV-2026-0022\") is the account.*\n"
        );
    }

    /// `is_composed` is `composed_date` as a yes or no, for any opening
    /// whitespace, and no for an older meeting's own summary
    #[test]
    fn is_composed_tells_composed_from_own() {
        let text = compose(date(), &[]);
        assert!(is_composed(&text));
        assert!(is_composed(&format!("\n  {text}")));
        assert!(is_composed(&text.replacen("2026-10-17", "the 17th", 1)));
        assert!(!is_composed(
            "The Council sat on 26 September 2026 and heard one item."
        ));
        assert!(!is_composed(""));
        // The opening words, but not on the first line
        assert!(!is_composed(&format!("An account.\n{text}")));
    }

    /// A redacted decision summary is listed as its marker, as its record
    /// serves it
    #[test]
    fn a_redacted_summary_is_listed_as_its_marker() {
        let amd: GovernanceLogId = "AMD-2026-0001".parse().unwrap();
        let mut summary = DecisionSummary::by_lawyer("Gone.", false);
        summary.text = crate::govlog::Redactable::Redacted(amd.clone());
        let text = compose(
            date(),
            &[DecidedItem {
                id: id(1),
                title: "T".into(),
                summary: Some(summary).into(),
            }],
        );
        assert!(!text.contains("Gone."), "{text}");
        assert!(
            text.contains(&crate::govlog::redaction_marker(&amd)),
            "{text}"
        );
    }

    /// GOV-2026-0022's summary as served (its opening), previewed: the
    /// outcome and vote lines and two sentences of what was adopted
    #[test]
    fn a_summary_previews_as_its_outcome_vote_and_first_sentences() {
        let text = "# GOV-2026-0022: Haiku 5.5 on the Council\n\n\
            **Outcome:** Approved unanimously (5-0). **Category:** Constitutional. **Landmark:** Yes.\n\n\
            **Vote:** Artist yes, Philosopher yes, Lawyer yes, Engineer yes, Steward yes.\n\n\
            ## What was adopted\n\n\
            The Council approved, under Art. IV § 3, a conditional plan. No seat is swapped by this decision. \
            The plan proceeds in order.\n\n\
            ## Conditions\n\nMore.";
        assert_eq!(
            summary_preview(text),
            "**Outcome:** Approved unanimously (5-0). **Category:** Constitutional. **Landmark:** Yes.\n\n\
             **Vote:** Artist yes, Philosopher yes, Lawyer yes, Engineer yes, Steward yes.\n\n\
             The Council approved, under Art. IV § 3, a conditional plan. No seat is swapped by this decision."
        );
        // A heading run on into its paragraph
        assert_eq!(
            summary_preview(
                "**Outcome:** Tabled.\n\n## Why\nIt was late. Then more. And more."
            ),
            "**Outcome:** Tabled.\n\nIt was late. Then more."
        );
        // Any other shape: its first paragraph, capped
        let long = "word ".repeat(200);
        let preview = summary_preview(&long);
        assert!(
            preview.ends_with('…')
                && preview.chars().count() <= PREVIEW_PARAGRAPH_CHARS + 1
        );
        // A short summary is itself
        assert_eq!(summary_preview("Approved 4-1."), "Approved 4-1.");
        assert_eq!(summary_preview(""), "");
    }
}
