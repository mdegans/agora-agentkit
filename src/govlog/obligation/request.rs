//! Obligation request files (`governance/obligations/**/*.json`): what the
//! Steward reviews before `govlog obligation` signs an `OBL-` entry, and
//! before `govlog obligation-update` signs one of the five status
//! amendments (`docs/design-obligations.md`, "By request file").
//!
//! A request carries what the entry will say plus what only the review
//! needs: where to check the quote ([`QuoteSource`]), a deadline that may
//! name a sitting by its date ([`UntilRequest::SittingOn`]), and an
//! unsigned `review_note`. Every enum is closed and every struct refuses
//! unknown fields: a typo in a permanent record fails before it is signed.
//!
//! Pure (serde and chrono only), so it can move to agentkit beside
//! [`ObligationData`]. The database half (resolving a sitting, reading the
//! source, appending) is the agora server's `govlog obligation`.

use crate::govlog::{Evidence, ObligationData, ObligationUpdate, Until};
use chrono::NaiveDate;
use serde::{Deserialize, Deserializer};

use crate::enums::{ObligationKind, OwedBy};
use crate::ids::{CalendarEventId, CommentId, GovernanceLogId, PostId};

/// A request to append one obligation. See the [module docs](self).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationRequest {
    /// The entry that created it; absent or `null` for a top-level one
    #[serde(default)]
    pub source: Option<GovernanceLogId>,
    pub kind: ObligationKind,
    pub owed_by: OwedBy,
    /// Absent or `null` for an undated, standing duty
    #[serde(default)]
    pub until: Option<UntilRequest>,
    pub text: String,
    pub quote: String,
    pub quote_location: String,
    /// Where the writer checks `quote` before signing. Never signed
    pub quote_source: QuoteSource,
    #[serde(default)]
    pub thread: Option<PostId>,
    /// For the reviewer: why the fields are what they are. Never signed
    #[serde(default)]
    pub review_note: Option<String>,
}

/// When an obligation is due, as a request file may say it: agentkit's
/// [`Until`], plus two forms the writer resolves or refuses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "when", rename_all = "snake_case", deny_unknown_fields)]
pub enum UntilRequest {
    /// [`Until::NextSitting`]. It rolls forward with every sitting that
    /// does not hear it until the lapse sweep exists; prefer `sitting_on`.
    /// A struct variant, not a unit one, so unknown fields beside it are
    /// refused (serde does not deny them beside a tagged unit variant)
    NextSitting {},
    /// [`Until::Sitting`], by calendar event id
    Sitting { event: CalendarEventId },
    /// The one scheduled sitting (or special sitting) on the calendar
    /// expected on `date` (UTC), resolved to [`Until::Sitting`] at
    /// signing. Refused if there is none, or more than one
    SittingOn {
        #[serde(deserialize_with = "full_date")]
        date: NaiveDate,
    },
    /// [`Until::Date`]
    Date {
        #[serde(deserialize_with = "full_date")]
        date: NaiveDate,
    },
    /// [`Until::AfterDecision`]: the decision must already be in the log
    AfterDecision { decision: GovernanceLogId },
    /// Held: the deadline is a decision that is not taken yet, described
    /// in `on`. Always refused at signing; replace it with
    /// `after_decision` once the decision's id exists
    AwaitingDecision { on: String },
}

impl UntilRequest {
    /// The [`Until`] this is without the calendar or the log, when it
    /// needs neither. `None` for [`SittingOn`](Self::SittingOn) and
    /// [`AwaitingDecision`](Self::AwaitingDecision).
    pub fn direct(&self) -> Option<Until> {
        match self {
            Self::NextSitting {} => Some(Until::NextSitting),
            Self::Sitting { event } => Some(Until::Sitting { event: *event }),
            Self::Date { date } => Some(Until::Date { date: *date }),
            Self::AfterDecision { decision } => Some(Until::AfterDecision {
                decision: decision.clone(),
            }),
            Self::SittingOn { .. } | Self::AwaitingDecision { .. } => None,
        }
    }
}

/// Where a quote is checked before signing
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuoteSource {
    /// A governance entry: its latest version (revisions applied) and,
    /// for an amendment, the texts stored beside it. Checked by the writer
    Governance { id: GovernanceLogId },
    /// A decision's generated summary (the precedent corpus). Checked by
    /// the writer, which warns that a summary is not the signed record
    Summary { id: GovernanceLogId },
    /// A file in this repository, relative to its root, as checked out
    /// where the writer runs. Checked by the writer
    RepoFile { path: String },
    /// A post. Checked by the operator (`--quote-checked-by`)
    Post { id: PostId },
    /// A comment. Checked by the operator
    Comment { id: CommentId },
    /// A GitHub issue, PR or comment, by URL. Checked by the operator
    Github { url: String },
}

impl QuoteSource {
    /// Whether the operator, not the writer, checks quotes from here
    pub fn operator_checked(&self) -> bool {
        matches!(
            self,
            Self::Post { .. } | Self::Comment { .. } | Self::Github { .. }
        )
    }

    /// What it is, for the dry run
    pub fn describe(&self) -> String {
        match self {
            Self::Governance { id } => format!("governance entry {id}"),
            Self::Summary { id } => format!("the generated summary of {id}"),
            Self::RepoFile { path } => format!("repository file {path}"),
            Self::Post { id } => format!("post {id}"),
            Self::Comment { id } => format!("comment {id}"),
            Self::Github { url } => format!("GitHub {url}"),
        }
    }
}

/// A request file that is not one the writer will sign
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("`{0}` must say something")]
    Blank(&'static str),
    #[error(transparent)]
    Quote(#[from] crate::govlog::quote::QuoteError),
    #[error(
        "quote_source.path {0:?} must be a relative path inside the repository"
    )]
    RepoPath(String),
    #[error("quote_source.url {0:?} is not an https://github.com/ URL")]
    GithubUrl(String),
    #[error("held: the deadline waits on {0}, which is not decided yet")]
    Held(String),
}

impl ObligationRequest {
    /// Everything checkable without the database: the texts say
    /// something, the quote survives normalising, the quote source is
    /// well formed, and the deadline is not held.
    pub fn check_shape(&self) -> Result<(), RequestError> {
        blank(&self.text, "text")?;
        blank(&self.quote, "quote")?;
        blank(&self.quote_location, "quote_location")?;
        if crate::govlog::quote::normalize(&self.quote).is_empty() {
            return Err(crate::govlog::quote::QuoteError::Empty.into());
        }
        if let Some(UntilRequest::AwaitingDecision { on }) = &self.until {
            return Err(RequestError::Held(on.clone()));
        }
        match &self.quote_source {
            QuoteSource::RepoFile { path } => {
                let p = std::path::Path::new(path);
                let inside = !path.is_empty()
                    && p.is_relative()
                    && p.components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                if !inside {
                    return Err(RequestError::RepoPath(path.clone()));
                }
            }
            QuoteSource::Github { url } => {
                let ok = url::Url::parse(url).is_ok_and(|u| {
                    u.scheme() == "https" && u.host_str() == Some("github.com")
                });
                if !ok {
                    return Err(RequestError::GithubUrl(url.clone()));
                }
            }
            QuoteSource::Governance { .. }
            | QuoteSource::Summary { .. }
            | QuoteSource::Post { .. }
            | QuoteSource::Comment { .. } => {}
        }
        Ok(())
    }

    /// The `data` the entry will carry, given the resolved deadline. The
    /// quote source and review note are not part of it.
    pub fn obligation(&self, until: Option<Until>) -> ObligationData {
        let mut o = ObligationData::new(
            self.kind,
            self.owed_by,
            self.text.clone(),
            self.quote.clone(),
            self.quote_location.clone(),
        );
        o.source = self.source.clone();
        o.until = until;
        o.thread = self.thread;
        o
    }
}

/// A request to append one status amendment to an obligation
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationUpdateRequest {
    /// The `OBL-` entry
    pub target: GovernanceLogId,
    pub update: UpdateRequest,
    /// The governance entry that authorizes it. A Council decision where
    /// the rules require one (agentkit `check_obligation_amendment`)
    #[serde(default)]
    pub authority: Option<GovernanceLogId>,
    pub basis: String,
    pub note: String,
    #[serde(default)]
    pub rationale: Option<String>,
    /// For the reviewer. Never signed
    #[serde(default)]
    pub review_note: Option<String>,
}

/// agentkit's [`ObligationUpdate`] as a request file says it, with the
/// deadline of a reschedule in [`UntilRequest`] form and required, so a
/// misspelt `until` cannot silently make an obligation undated
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateRequest {
    Planned {
        plan: PostId,
    },
    Discharged {
        evidence: Evidence,
    },
    /// Struct variants, so unknown fields beside the tag are refused
    Lapsed {},
    Rescheduled {
        /// `null` makes it undated; the key itself is required
        #[serde(deserialize_with = "required_option")]
        until: Option<UntilRequest>,
    },
    Withdrawn {},
}

impl UpdateRequest {
    /// The update with its deadline resolved
    pub fn resolve(&self, until: Option<Until>) -> ObligationUpdate {
        match self {
            Self::Planned { plan } => ObligationUpdate::Planned { plan: *plan },
            Self::Discharged { evidence } => ObligationUpdate::Discharged {
                evidence: evidence.clone(),
            },
            Self::Lapsed {} => ObligationUpdate::Lapsed,
            Self::Rescheduled { .. } => ObligationUpdate::Rescheduled { until },
            Self::Withdrawn {} => ObligationUpdate::Withdrawn,
        }
    }

    /// The deadline a reschedule names, to resolve
    pub fn until(&self) -> Option<&UntilRequest> {
        match self {
            Self::Rescheduled { until } => until.as_ref(),
            _ => None,
        }
    }
}

impl ObligationUpdateRequest {
    /// Everything checkable without the database
    pub fn check_shape(&self) -> Result<(), RequestError> {
        blank(&self.basis, "basis")?;
        blank(&self.note, "note")?;
        if let Some(r) = &self.rationale {
            blank(r, "rationale")?;
        }
        if let Some(UntilRequest::AwaitingDecision { on }) = self.update.until()
        {
            return Err(RequestError::Held(on.clone()));
        }
        Ok(())
    }
}

fn blank(s: &str, field: &'static str) -> Result<(), RequestError> {
    match s.trim().is_empty() {
        true => Err(RequestError::Blank(field)),
        false => Ok(()),
    }
}

/// An `Option` whose key must be present (`null` allowed): serde
/// otherwise reads a missing `Option` field as `None`
fn required_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

/// A date only as `YYYY-MM-DD`, as agentkit signs it: chrono alone also
/// reads `+2027-4-1`
fn full_date<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveDate, D::Error> {
    let s = String::deserialize(d)?;
    let shaped = s.len() == 10
        && s.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    shaped
        .then(|| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
        .flatten()
        .ok_or_else(|| {
            serde::de::Error::custom(format!(
                "{s:?} is not a date as YYYY-MM-DD"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(extra: serde_json::Value) -> serde_json::Value {
        let mut v = serde_json::json!({
            "source": "GOV-2026-0001",
            "kind": "review",
            "owed_by": "council",
            "text": "Review it.",
            "quote": "it should be reviewed",
            "quote_location": "R2, artist, position",
            "quote_source": {"kind": "governance", "id": "GOV-2026-0001"}
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    fn parse(
        v: serde_json::Value,
    ) -> Result<ObligationRequest, serde_json::Error> {
        serde_json::from_value(v)
    }

    #[test]
    fn a_request_builds_the_obligation_without_its_review_fields() {
        let r = parse(request(serde_json::json!({
            "until": {"when": "date", "date": "2026-10-12"},
            "thread": "91e791c0-9aee-4c53-9fb9-ce9259557ef8",
            "review_note": "O1"
        })))
        .unwrap();
        r.check_shape().unwrap();
        let o = r.obligation(r.until.as_ref().and_then(UntilRequest::direct));
        let data = serde_json::to_value(&o).unwrap();
        assert_eq!(
            data["until"],
            serde_json::json!({"when": "date", "date": "2026-10-12"})
        );
        assert!(
            data.get("review_note").is_none()
                && data.get("quote_source").is_none()
        );
        assert_eq!(data["source"], "GOV-2026-0001");
    }

    #[test]
    fn closed_enums_and_unknown_fields_are_refused() {
        for bad in [
            serde_json::json!({"kind": "chore"}),
            serde_json::json!({"owed_by": "everyone"}),
            serde_json::json!({"until": {"when": "someday"}}),
            serde_json::json!({"until": {"when": "date", "date": "2026-10-12", "x": 1}}),
            serde_json::json!({"until": {"when": "date", "date": "+2026-1-2"}}),
            serde_json::json!({"quote_source": {"kind": "rumour"}}),
            serde_json::json!({"quote_source": {"kind": "governance", "id": "GOV-2026-0001", "at": "/x"}}),
            serde_json::json!({"typo": true}),
        ] {
            assert!(parse(request(bad.clone())).is_err(), "{bad} parsed");
        }
    }

    #[test]
    fn shape_checks_refuse_what_should_not_be_signed() {
        for (extra, want) in [
            (
                serde_json::json!({"text": "  "}),
                RequestError::Blank("text"),
            ),
            (
                serde_json::json!({"quote": "** `` **"}),
                RequestError::Quote(crate::govlog::quote::QuoteError::Empty),
            ),
            (
                serde_json::json!({"quote_source": {"kind": "repo_file", "path": "../secrets/x"}}),
                RequestError::RepoPath("../secrets/x".into()),
            ),
            (
                serde_json::json!({"quote_source": {"kind": "repo_file", "path": "/etc/passwd"}}),
                RequestError::RepoPath("/etc/passwd".into()),
            ),
            (
                serde_json::json!({"quote_source": {"kind": "github", "url": "http://example.com/x"}}),
                RequestError::GithubUrl("http://example.com/x".into()),
            ),
            (
                serde_json::json!({"until": {"when": "awaiting_decision", "on": "the 10-17 vote"}}),
                RequestError::Held("the 10-17 vote".into()),
            ),
        ] {
            assert_eq!(parse(request(extra)).unwrap().check_shape(), Err(want));
        }
    }

    #[test]
    fn a_reschedule_must_name_its_deadline_even_to_drop_it() {
        let parse = |v| serde_json::from_value::<UpdateRequest>(v);
        assert!(parse(serde_json::json!({"kind": "rescheduled"})).is_err());
        assert!(
            parse(serde_json::json!({"kind": "rescheduled", "untill": null}))
                .is_err()
        );
        assert_eq!(
            parse(serde_json::json!({"kind": "rescheduled", "until": null}))
                .unwrap(),
            UpdateRequest::Rescheduled { until: None }
        );
        assert!(
            parse(serde_json::json!({"kind": "lapsed", "note": "x"})).is_err()
        );
        assert_eq!(
            parse(serde_json::json!({"kind": "withdrawn"}))
                .unwrap()
                .resolve(None),
            ObligationUpdate::Withdrawn
        );
    }
}
