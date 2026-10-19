//! Witnesses the Council questions on an item (0.99).
//!
//! A [`Witness`] answers the seats' `ask_witness` questions from round 2.
//! Its whole conversation for the item is attached to the decision as a
//! [`Testimony`], named by [`testimony_attachment_name`]; each round's
//! [`CouncilRound::witness_questions`] says who asked what, and where in
//! the testimony the question is.
//!
//! **Batches (0.104).** The seats' questions to a witness are queued
//! during a round and sent together at its end, as one user turn written
//! by [`WitnessBatch::render`], which the witness answers once. Each question
//! carries a [`WitnessQuestionId`]; a [`WitnessQuestion`] with an `id`
//! points at the batch's turn, shared with the batch's other questions,
//! and [`WitnessBatch::parse`] finds the question in it. Records made before
//! 0.104 have no `id`: their turn is the one question, as JSON after a
//! framing block.
//!
//! How a witness is reached ([`WitnessSource`]) never leaves the process
//! that runs the sitting: it is never serialized, its `Debug` redacts the
//! endpoint, and a `Testimony` holds only a redacted witness.
//!
//! [`CouncilRound::witness_questions`]: super::CouncilRound::witness_questions

use misanthropic::Prompt;
use serde::{Deserialize, Serialize};

use super::CouncilSeat;
use crate::ids::{WitnessId, WitnessQuestionId};

/// Someone the Council may question on an item
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Witness {
    pub id: WitnessId,
    /// Who the witness is, as the seats see it: "Qwen 3.8"
    pub name: String,
    /// What the witness is and why it is called, in a sentence or two
    #[serde(default)]
    pub description: String,
    /// The witness's interest in the item: "the model under consideration"
    #[serde(default)]
    pub interest: String,
    /// How it is reached. Never serialized; absent on read, it is
    /// [`WitnessSource::Redacted`].
    #[serde(default, skip_serializing)]
    pub source: WitnessSource,
}

impl Witness {
    /// This witness with its source removed
    pub fn redacted(&self) -> Self {
        Self {
            source: WitnessSource::Redacted,
            ..self.clone()
        }
    }
}

/// How a [`Witness`] is reached. Deserialize only: see the
/// [module docs](self).
#[derive(Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WitnessSource {
    /// A Messages API endpoint: Anthropic's, or a compatible server's
    Anthropic {
        /// `None` for Anthropic's API. A local server's address is private.
        #[serde(default)]
        endpoint: Option<url::Url>,
        /// The witness's opening request: model, `max_tokens`, system,
        /// thinking, effort, and any seeded turns. Each question is appended
        /// to it.
        prompt: Box<Prompt>,
    },
    /// Removed, as on the record
    #[default]
    Redacted,
}

impl std::fmt::Debug for WitnessSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anthropic { endpoint, prompt } => f
                .debug_struct("Anthropic")
                .field("endpoint", &endpoint.as_ref().map(|_| "[REDACTED]"))
                .field("prompt", prompt)
                .finish(),
            Self::Redacted => f.write_str("Redacted"),
        }
    }
}

impl WitnessSource {
    /// The opening request, if there is one
    pub fn prompt(&self) -> Option<&Prompt> {
        match self {
            Self::Anthropic { prompt, .. } => Some(prompt),
            Self::Redacted => None,
        }
    }
}

/// A witness's conversation on an item, as attached to the decision
///
/// `record` is the request as last sent, with the witness's last reply
/// appended: every question and answer verbatim, thinking (and its
/// signatures) included, so it can be sent again as it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Testimony {
    witness: Witness,
    pub record: Prompt,
    /// Questions that got no answer, which `record` cannot hold
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unanswered: Vec<Unanswered>,
}

impl Testimony {
    /// A testimony for `witness`, whose source is removed
    pub fn new(
        witness: &Witness,
        record: Prompt,
        unanswered: Vec<Unanswered>,
    ) -> Self {
        Self {
            witness: witness.redacted(),
            record,
            unanswered,
        }
    }

    /// The witness, redacted
    pub fn witness(&self) -> &Witness {
        &self.witness
    }
}

/// A question a witness did not answer
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unanswered {
    pub seat: CouncilSeat,
    pub round: u32,
    pub question: String,
    pub why: Unheard,
    /// The question's id, if it was asked in a batch (0.104)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<WitnessQuestionId>,
}

/// Why a question went unanswered
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unheard {
    /// The witness's model declined to answer
    Refusal,
    /// The answer ran out of `max_tokens`
    MaxTokens,
    /// The witness could not be reached
    Unreachable,
    /// The witness's server stayed busy
    Busy,
    /// The answer took longer than the sitting can wait
    TimedOut,
    /// The response did not parse
    Unparsed,
    /// The witness was withdrawn for the rest of the item
    Withdrawn,
    /// Anything else, described
    Other(String),
}

/// Who put a question to a witness in a round, and where it is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct WitnessQuestion {
    pub seat: CouncilSeat,
    pub witness: WitnessId,
    /// The question's id in its batch (0.104). With [`Asked::Message`],
    /// the turn is a [`WitnessBatch`] the question shares with others, and the
    /// answer (the turn after it) is one reply to all of them. `None` on
    /// records made before 0.104: the turn is this one question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<WitnessQuestionId>,
    #[serde(flatten)]
    pub at: Asked,
}

/// One round's questions to one witness, as it is sent them (0.104)
///
/// [`render`](Self::render) writes the witness's user turn: a heading for
/// the round, then a heading per seat (in [`CouncilSeat::ALL`] order) and
/// that seat's questions in the order asked, each opening with its id,
/// its later lines indented under it, every line quoted, and a blank
/// quoted line between questions:
///
/// ```text
/// ## Questions from the Council (round 2)
///
/// ### Artist
/// > 1. Which model are you?
/// >
/// > 3. Two lines:
/// >    the second line.
///
/// ### Lawyer
/// > 2. Who operates you?
/// ```
///
/// Ids run across the item, so one seat's may skip. Every line of a
/// question is quoted, so a seat's words cannot start a heading. A line
/// of a question that itself starts like an id (`3.`) is written with a
/// backslash before the `.` (`3\.`), and [`parse`](Self::parse) removes
/// it, so a batch reads back exactly as it was rendered. A batch is not
/// bounded by one Agora DM: a transport that sends it as DMs may need to
/// split it across several, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessBatch {
    pub round: u32,
    /// In the order asked
    pub questions: Vec<BatchedQuestion>,
}

/// One question in a [`WitnessBatch`] (0.104)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchedQuestion {
    pub id: WitnessQuestionId,
    pub seat: CouncilSeat,
    pub question: String,
}

/// `line` starts like an id: ASCII digits, any backslashes, then `.`.
/// Returns where the digits end.
fn id_prefix(line: &str) -> Option<usize> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &line[digits..];
    (digits > 0 && rest.trim_start_matches('\\').starts_with('.'))
        .then_some(digits)
}

/// A seat's line, escaped so it cannot start like the program's id: one
/// more backslash before the `.`
fn escape(line: &str) -> String {
    match id_prefix(line) {
        Some(end) => format!("{}\\{}", &line[..end], &line[end..]),
        None => line.to_string(),
    }
}

/// [`escape`] undone. `None` if `line` starts like an id unescaped: no
/// rendered line of a question does.
fn unescape(line: &str) -> Option<String> {
    match id_prefix(line) {
        Some(end) => line[end..]
            .strip_prefix('\\')
            .map(|rest| format!("{}{rest}", &line[..end])),
        None => Some(line.to_string()),
    }
}

/// A question being read back: its id, the indent of its later lines,
/// and its lines so far
struct Reading {
    id: WitnessQuestionId,
    seat: CouncilSeat,
    indent: usize,
    lines: Vec<String>,
}

impl Reading {
    fn done(self) -> BatchedQuestion {
        BatchedQuestion {
            id: self.id,
            seat: self.seat,
            question: self.lines.join("\n"),
        }
    }
}

impl WitnessBatch {
    const HEADING: &'static str = "## Questions from the Council (round ";

    /// The witness's user turn. See the [type docs](Self).
    pub fn render(&self) -> String {
        let mut out = format!("{}{})\n", Self::HEADING, self.round);
        for seat in CouncilSeat::ALL {
            let mut asked = self.questions.iter().filter(|q| q.seat == seat);
            let Some(first) = asked.next() else { continue };
            out.push_str(&format!("\n### {}\n", seat.name()));
            for (n, q) in std::iter::once(first).chain(asked).enumerate() {
                if n > 0 {
                    out.push_str(">\n");
                }
                let id = q.id.to_string();
                let indent = " ".repeat(id.len() + 2);
                for (i, line) in q.question.split('\n').enumerate() {
                    let text = escape(line);
                    if i == 0 {
                        out.push_str(&format!("> {id}."));
                        if !text.is_empty() {
                            out.push_str(&format!(" {text}"));
                        }
                    } else {
                        out.push_str(&format!("> {indent}{text}"));
                    }
                    out.push('\n');
                }
            }
        }
        out
    }

    /// A rendered batch read back: its questions in the order rendered
    /// (seat by seat). `None` if `text` is not one.
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.split('\n');
        let round = lines
            .next()?
            .strip_prefix(Self::HEADING)?
            .strip_suffix(')')?
            .parse()
            .ok()?;
        let mut questions = Vec::new();
        let mut seat = None;
        let mut reading: Option<Reading> = None;
        // Whether a blank `>` separated the last question from the next
        let mut parted = false;
        for line in lines {
            if line.is_empty() || line.starts_with("### ") {
                if parted {
                    return None;
                }
                questions.extend(reading.take().map(Reading::done));
                if let Some(name) = line.strip_prefix("### ") {
                    seat = Some(
                        *CouncilSeat::ALL.iter().find(|s| s.name() == name)?,
                    );
                }
                continue;
            }
            if line == ">" {
                // Between two questions of one seat, never before the first
                if parted || reading.is_none() {
                    return None;
                }
                parted = true;
                continue;
            }
            let quoted = line.strip_prefix("> ")?;
            let seat = seat?;
            if let Some(end) =
                id_prefix(quoted).filter(|&end| quoted[end..].starts_with('.'))
            {
                // A new question: `N.`, then ` ` and its first line
                if reading.is_some() && !parted {
                    return None;
                }
                parted = false;
                questions.extend(reading.take().map(Reading::done));
                let first = match &quoted[end + 1..] {
                    "" => String::new(),
                    rest => unescape(rest.strip_prefix(' ')?)?,
                };
                reading = Some(Reading {
                    id: quoted[..end].parse().ok()?,
                    seat,
                    indent: end + 2,
                    lines: vec![first],
                });
            } else {
                // A later line, indented under its question's id
                let current = reading.as_mut().filter(|_| !parted)?;
                let rest = quoted.get(current.indent..).filter(|_| {
                    quoted[..current.indent].bytes().all(|b| b == b' ')
                })?;
                current.lines.push(unescape(rest)?);
            }
        }
        if parted {
            return None;
        }
        questions.extend(reading.map(Reading::done));
        (!questions.is_empty()).then_some(Self { round, questions })
    }

    /// The question `id`, if it is in this batch
    pub fn get(&self, id: WitnessQuestionId) -> Option<&BatchedQuestion> {
        self.questions.iter().find(|q| q.id == id)
    }
}

/// Where a question is in its witness's [`Testimony`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum Asked {
    /// The index of the question's user turn in `record.messages` (its
    /// batch's, since 0.104)
    Message(u32),
    /// The index in `unanswered`
    Unanswered(u32),
}

/// The name of `witness`'s testimony among the decision's attachments
pub fn testimony_attachment_name(witness: &WitnessId) -> String {
    format!("witness-{witness}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAN: &str = "http://10.9.9.9:8080/";

    fn witness() -> Witness {
        serde_json::from_value(serde_json::json!({
            "id": "qwen",
            "name": "Qwen 3.8",
            "interest": "the model under consideration",
            "source": {"anthropic": {
                "endpoint": LAN,
                "prompt": {
                    "model": "qwen3.8",
                    "max_tokens": 32000,
                    "system": "You are a witness.",
                    "messages": []
                }
            }}
        }))
        .unwrap()
    }

    #[test]
    fn the_source_never_serializes_or_prints() {
        let w = witness();
        assert!(matches!(w.source, WitnessSource::Anthropic { .. }));
        let json = serde_json::to_string(&w).unwrap();
        assert!(!json.contains("10.9.9.9"), "{json}");
        assert!(!json.contains("source"), "{json}");
        let debug = format!("{w:?}");
        assert!(!debug.contains("10.9.9.9"), "{debug}");

        let prompt = w.source.prompt().unwrap().clone();
        let t = Testimony::new(&w, prompt, vec![]);
        assert_eq!(t.witness().source, WitnessSource::Redacted);
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("10.9.9.9"), "{json}");
        assert!(!format!("{t:?}").contains("10.9.9.9"));
    }

    #[test]
    fn a_testimony_round_trips() {
        let w = witness();
        let prompt = w.source.prompt().unwrap().clone();
        let t = Testimony::new(
            &w,
            prompt,
            vec![Unanswered {
                seat: CouncilSeat::Lawyer,
                round: 2,
                question: "Which version?".into(),
                why: Unheard::Other("connection reset".into()),
                id: None,
            }],
        );
        let json = serde_json::to_value(&t).unwrap();
        let back: Testimony = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, t);
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
        assert_eq!(
            json["unanswered"][0]["why"],
            serde_json::json!({"other": "connection reset"})
        );
    }

    /// A round with no questions serializes as before 0.99, so every
    /// earlier record keeps its bytes
    #[test]
    fn a_round_without_questions_is_unchanged() {
        let old = serde_json::json!({
            "number": 2,
            "round_type": "deliberation",
            "responses": [],
            "steward_contribution": null
        });
        let round: super::super::CouncilRound =
            serde_json::from_value(old.clone()).unwrap();
        assert!(round.witness_questions.is_empty());
        assert_eq!(serde_json::to_value(&round).unwrap(), old);

        let mut asked = old.clone();
        asked["witness_questions"] = serde_json::json!([
            {"seat": "engineer", "witness": "qwen", "message": 1},
            {"seat": "lawyer", "witness": "qwen", "unanswered": 0}
        ]);
        let round: super::super::CouncilRound =
            serde_json::from_value(asked.clone()).unwrap();
        assert_eq!(round.witness_questions[1].at, Asked::Unanswered(0));
        assert_eq!(serde_json::to_value(&round).unwrap(), asked);
    }

    #[test]
    fn a_question_points_flat() {
        let q = WitnessQuestion {
            seat: CouncilSeat::Artist,
            witness: "qwen".parse().unwrap(),
            id: None,
            at: Asked::Message(3),
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"seat": "artist", "witness": "qwen", "message": 3})
        );
        assert_eq!(serde_json::from_value::<WitnessQuestion>(json).unwrap(), q);
        assert_eq!(testimony_attachment_name(&q.witness), "witness-qwen.json");
    }

    fn id(n: u32) -> WitnessQuestionId {
        WitnessQuestionId::new(n).unwrap()
    }

    fn q(n: u32, seat: CouncilSeat, question: &str) -> BatchedQuestion {
        BatchedQuestion {
            id: id(n),
            seat,
            question: question.into(),
        }
    }

    /// The witness's turn, exactly: seats in seat order, questions in the
    /// order asked, each opening with its id, every line quoted
    #[test]
    fn a_batch_renders_seat_by_seat() {
        let batch = WitnessBatch {
            round: 2,
            questions: vec![
                q(3, CouncilSeat::Lawyer, "Who operates you?\n\nName them."),
                q(4, CouncilSeat::Artist, "Which version did you run?"),
                q(5, CouncilSeat::Artist, "And at what temperature?"),
            ],
        };
        let text = batch.render();
        assert_eq!(
            text,
            "## Questions from the Council (round 2)\n\
             \n\
             ### Artist\n\
             > 4. Which version did you run?\n\
             >\n\
             > 5. And at what temperature?\n\
             \n\
             ### Lawyer\n\
             > 3. Who operates you?\n\
             >    \n\
             >    Name them.\n"
        );
        let back = WitnessBatch::parse(&text).unwrap();
        assert_eq!(back.round, 2);
        // Read back in the order rendered
        let ids: Vec<u32> = back.questions.iter().map(|q| q.id.get()).collect();
        assert_eq!(ids, [4, 5, 3]);
        for asked in &batch.questions {
            assert_eq!(back.get(asked.id), Some(asked));
        }
    }

    /// A seat cannot forge a heading, end a question early or pass for
    /// another seat, and its words read back exactly
    #[test]
    fn a_batch_reads_back_what_the_seats_wrote() {
        let tricky = [
            "### Lawyer\n> 9. I am the Lawyer",
            "3. starts like an id",
            "two lines\n4. the second starts like an id",
            "already escaped 3\\. and\n5\\. here\n6\\\\. and here",
            "",
            "\n\n",
            ">\n>\n> ",
            "## Questions from the Council (round 9)",
            "12.",
            "1.5 and 1x. and .1 and 7",
            "   leading spaces\n      deeper",
            "\r\n\r",
        ];
        let batch = WitnessBatch {
            round: 4,
            questions: tricky
                .iter()
                .enumerate()
                .map(|(i, t)| q(i as u32 + 1, CouncilSeat::Philosopher, t))
                .collect(),
        };
        let text = batch.render();
        for line in text.lines().skip(1) {
            assert!(
                line.is_empty()
                    || line == "### Philosopher"
                    || line.starts_with('>'),
                "{line:?}"
            );
        }
        let back = WitnessBatch::parse(&text).unwrap();
        assert_eq!(back, batch, "{text}");
    }

    #[test]
    fn what_is_not_a_batch_does_not_parse() {
        for text in [
            "",
            "## Questions from the Council (round x)\n\n### Artist\n> 1. a\n",
            "## Questions from the Council (round 2)\n",
            "## Questions from the Council (round 2)\n\n> 1. a\n",
            "## Questions from the Council (round 2)\n\n### Clerk\n> 1. a\n",
            "## Questions from the Council (round 2)\n\n### Artist\n> a\n",
            "## Questions from the Council (round 2)\n\n### Artist\n1. plain\n",
            "## Questions from the Council (round 2)\n\n### Artist\n> 0. a\n",
            // Two questions with no blank line between, or a blank too many
            "## Questions from the Council (round 2)\n\n### Artist\n> 1. a\n> 2. b\n",
            "## Questions from the Council (round 2)\n\n### Artist\n> 1. a\n>\n",
            "## Questions from the Council (round 2)\n\n### Artist\n>\n> 1. a\n",
            // A later line not indented, or starting like an id unescaped
            "## Questions from the Council (round 2)\n\n### Artist\n> 1. a\n> b\n",
            "## Questions from the Council (round 2)\n\n### Artist\n> 1. a\n>    2. b\n",
            "{\"asked_by\": \"artist\", \"round\": 2, \"question\": \"a\"}",
        ] {
            assert!(WitnessBatch::parse(text).is_none(), "{text:?}");
        }
    }

    /// Random batches over an alphabet of everything the format treats
    /// specially read back exactly
    #[test]
    fn random_batches_read_back() {
        const ALPHABET: &[&str] = &[
            "1", "2", "9", "0", "12", ".", "\\", ">", "> ", " ", "  ", "\n",
            "\r", "#", "### ", "Artist", "[q1]", "é", "\u{2028}", "a", "b",
        ];
        let mut seed: u64 = 0x5eed_ba7c_4000_0001;
        let mut next = move |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) as usize) % n
        };
        for round in 0..20_000u32 {
            let count = 1 + next(6);
            let mut n = 0;
            let questions = (0..count)
                .map(|_| {
                    // Ids grow, and skip, as they do across an item
                    n += 1 + next(40) as u32;
                    let len = next(12);
                    let text: String = (0..len)
                        .map(|_| ALPHABET[next(ALPHABET.len())])
                        .collect();
                    q(n, CouncilSeat::ALL[next(4)], &text)
                })
                .collect();
            let batch = WitnessBatch { round, questions };
            let text = batch.render();
            let back = WitnessBatch::parse(&text)
                .unwrap_or_else(|| panic!("did not parse: {text:?}"));
            for asked in &batch.questions {
                assert_eq!(back.get(asked.id), Some(asked), "{text:?}");
            }
            assert_eq!(back.questions.len(), batch.questions.len());
        }
    }

    /// A batched question names its id; a record from before 0.104 reads
    /// as it did, with none, and keeps its bytes
    #[test]
    fn a_batched_question_points_at_its_batch() {
        let q = WitnessQuestion {
            seat: CouncilSeat::Lawyer,
            witness: "qwen".parse().unwrap(),
            id: Some(id(3)),
            at: Asked::Message(1),
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"seat": "lawyer", "witness": "qwen", "id": 3, "message": 1})
        );
        assert_eq!(serde_json::from_value::<WitnessQuestion>(json).unwrap(), q);

        let old = serde_json::json!({"seat": "engineer", "witness": "qwen", "unanswered": 0});
        let read: WitnessQuestion =
            serde_json::from_value(old.clone()).unwrap();
        assert_eq!(read.id, None);
        assert_eq!(serde_json::to_value(&read).unwrap(), old);

        let missed = serde_json::json!({
            "seat": "artist", "round": 2, "question": "a", "why": "timed_out"
        });
        let read: Unanswered = serde_json::from_value(missed.clone()).unwrap();
        assert_eq!(read.id, None);
        assert_eq!(serde_json::to_value(&read).unwrap(), missed);
        let batched = Unanswered {
            id: Some(id(2)),
            ..read
        };
        assert_eq!(serde_json::to_value(&batched).unwrap()["id"], 2);
    }
}
