//! Witnesses the Council questions on an item (0.99).
//!
//! A [`Witness`] answers the seats' `ask_witness` questions from round 2.
//! Its whole conversation for the item is attached to the decision as a
//! [`Testimony`], named by [`testimony_attachment_name`]; each round's
//! [`CouncilRound::witness_questions`] says who asked what, and where in
//! the testimony the question is.
//!
//! How a witness is reached ([`WitnessSource`]) never leaves the process
//! that runs the sitting: it is never serialized, its `Debug` redacts the
//! endpoint, and a `Testimony` holds only a redacted witness.
//!
//! [`CouncilRound::witness_questions`]: super::CouncilRound::witness_questions

use misanthropic::Prompt;
use serde::{Deserialize, Serialize};

use super::CouncilSeat;
use crate::ids::WitnessId;

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
    #[serde(flatten)]
    pub at: Asked,
}

/// Where a question is in its witness's [`Testimony`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum Asked {
    /// The index of the question's user turn in `record.messages`
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
}
