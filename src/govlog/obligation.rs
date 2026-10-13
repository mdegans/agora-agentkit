//! The `data` of an `obligation` entry (`OBL-YYYY-NNNN`), and the
//! amendments that keep its status (agora `docs/design-obligations.md`).
//!
//! An obligation is never edited. Its status changes through appended
//! [`Amendment`]s of the five [obligation kinds](AmendmentKind::OBLIGATION),
//! each carrying an [`ObligationUpdate`], and [`ObligationState`] is what
//! they add up to. Partial discharge is an ordinary [`Revision`] narrowing
//! `text`.
//!
//! Like a [`StewardRecord`], an obligation is
//! [redactable](super::is_redactable): its `quote` may hold an agent's
//! words. So every field that might is a plain string, which a
//! [redaction marker](super::redaction_marker) can replace and still read.
//!
//! [`Revision`]: super::Revision
//! [`StewardRecord`]: super::StewardRecord

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::{Amendment, AmendmentError, OBLIGATION_FIXED, revision_touches};
use crate::enums::{
    AmendmentKind, GovernanceLogEntryType, ObligationKind, ObligationStatus,
    OwedBy,
};
use crate::ids::{CalendarEventId, GovernanceLogId, PostId};

/// The only `agora_obligation` version there is
pub const OBLIGATION_VERSION: u32 = 1;

/// When an obligation is due.
///
/// `NextSitting` is resolved against the calendar when read, never pinned
/// to a date, so a sitting that slips carries the obligation with it.
///
/// Its schema is written by hand (see the `JsonSchema` impl): an `anyOf`
/// with one closed object per variant, tagged by `when`, with no `$ref` and
/// no `pattern`, so it can sit in a strict response format. Closed in
/// serde too, except beside `next_sitting`: serde does not deny unknown
/// fields on a unit variant of an internally tagged enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "when", rename_all = "snake_case", deny_unknown_fields)]
pub enum Until {
    /// The next Council sitting, whenever that is
    NextSitting,
    /// A sitting on the calendar
    Sitting { event: CalendarEventId },
    /// The end of a date
    Date {
        #[serde(with = "full_date")]
        date: NaiveDate,
    },
    /// The next sitting after a decision is taken
    AfterDecision { decision: GovernanceLogId },
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Until {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Until")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::Until"))
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let variant =
            |when: &str, description: &str, field: Option<(&str, _)>| {
                let mut properties = serde_json::Map::new();
                properties.insert(
                    "when".into(),
                    serde_json::json!({ "type": "string", "enum": [when] }),
                );
                let mut required = vec!["when"];
                if let Some((name, schema)) = field {
                    properties.insert(name.into(), schema);
                    required.push(name);
                }
                serde_json::json!({
                    "type": "object",
                    "description": description,
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false
                })
            };
        let schema = serde_json::json!({
            "description": "When it is due",
            "anyOf": [
                variant(
                    "next_sitting",
                    "At the next Council sitting, whenever that is held",
                    None,
                ),
                variant(
                    "sitting",
                    "At a particular sitting on the calendar",
                    Some((
                        "event",
                        serde_json::json!({
                            "type": "string",
                            "format": "uuid",
                            "description": "The sitting's calendar event id"
                        }),
                    )),
                ),
                variant(
                    "date",
                    "By the end of a date",
                    Some((
                        "date",
                        serde_json::json!({
                            "type": "string",
                            "format": "date",
                            "description": "YYYY-MM-DD"
                        }),
                    )),
                ),
                variant(
                    "after_decision",
                    "At the first sitting after a decision is taken",
                    Some((
                        "decision",
                        serde_json::json!({
                            "type": "string",
                            "description": "The decision's governance log id, \
                                            e.g. \"GOV-2026-0012\""
                        }),
                    )),
                ),
            ]
        });
        schemars::Schema::try_from(schema).expect("an object is a schema")
    }
}

impl Until {
    /// The date this is, when it is a date: what the projection stores as
    /// the due date. Sitting deadlines resolve against the calendar when
    /// read ([`crate::calendar::Calendar::resolve`]), never here.
    pub fn fixed_date(&self) -> Option<NaiveDate> {
        match self {
            Self::Date { date } => Some(*date),
            Self::NextSitting
            | Self::Sitting { .. }
            | Self::AfterDecision { .. } => None,
        }
    }
}

/// A date in signed data, read only as `YYYY-MM-DD`.
///
/// chrono alone also reads `+2027-4-1` and `2027 - 04 - 01`. Every
/// verifier must agree on what an amendment says, and a second one should
/// not have to reproduce chrono's leniency to do it.
mod full_date {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        date: &NaiveDate,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_str(&date.format("%Y-%m-%d"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<NaiveDate, D::Error> {
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
}

/// The `data` of an `obligation` entry: what was signed, and never
/// changes but by a [`Revision`]. See the [module docs](self), and
/// [`Obligation`] for the entry as the log has it now.
///
/// Unknown keys are tolerated on purpose: a stored obligation also carries
/// the writer's `_blind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct ObligationData {
    /// Always [`OBLIGATION_VERSION`]
    pub agora_obligation: u32,
    /// The entry that created it. `None` makes it top-level: a standing
    /// duty, or a promise made in a post rather than a record
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<GovernanceLogId>,
    pub kind: ObligationKind,
    pub owed_by: OwedBy,
    /// When it is due; `None` is a standing duty, which never lapses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Until>,
    /// What is owed, in a sentence
    pub text: String,
    /// The words in the source that create it, verbatim
    pub quote: String,
    /// Where they are: round and seat, section, or a post or comment id
    pub quote_location: String,
    /// Where it was promised, when that is a post
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<PostId>,
}

impl ObligationData {
    /// A top-level, undated obligation with no thread; set the rest on it
    pub fn new(
        kind: ObligationKind,
        owed_by: OwedBy,
        text: impl Into<String>,
        quote: impl Into<String>,
        quote_location: impl Into<String>,
    ) -> Self {
        Self {
            agora_obligation: OBLIGATION_VERSION,
            source: None,
            kind,
            owed_by,
            until: None,
            text: text.into(),
            quote: quote.into(),
            quote_location: quote_location.into(),
            thread: None,
        }
    }
}

/// What an amendment of an [obligation kind](AmendmentKind::OBLIGATION)
/// says, tagged by that kind: [`Amendment::update`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObligationUpdate {
    /// The thread where the work is planned. A later plan replaces it
    Planned { plan: PostId },
    /// Met
    Discharged { evidence: Evidence },
    /// Missed and still owed. The amendment's note says what was missed
    Lapsed,
    /// Due at a new time, or undated
    Rescheduled {
        #[serde(default)]
        until: Option<Until>,
    },
    /// Released
    Withdrawn,
}

impl ObligationUpdate {
    /// The [`AmendmentKind`] this belongs on
    pub fn kind(&self) -> AmendmentKind {
        match self {
            Self::Planned { .. } => AmendmentKind::Planned,
            Self::Discharged { .. } => AmendmentKind::Discharged,
            Self::Lapsed => AmendmentKind::Lapsed,
            Self::Rescheduled { .. } => AmendmentKind::Rescheduled,
            Self::Withdrawn => AmendmentKind::Withdrawn,
        }
    }
}

/// What shows an obligation was met
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    /// A governance entry: the decision that heard it, a record
    Governance { id: GovernanceLogId },
    /// A pull request, as `owner/name` and its number
    PullRequest { repo: String, number: u64 },
    /// A commit, as `owner/name` and its hex sha (7 to 40 digits)
    Commit { repo: String, sha: String },
    /// A deploy, on a date, and what went out
    Deploy {
        #[serde(with = "full_date")]
        #[cfg_attr(feature = "schemars", schemars(with = "NaiveDate"))]
        date: NaiveDate,
        note: String,
    },
}

/// Where an obligation stands, from its amendments: see
/// [`ObligationState::from_amendments`]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct ObligationState {
    pub status: ObligationStatus,
    /// When it is due now: the last reschedule's, or the obligation's own
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Until>,
    /// The `authority` of the reschedule that set `until`, if one did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_authority: Option<GovernanceLogId>,
    /// The latest plan
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PostId>,
    /// When it last lapsed. Kept after a discharge: a late discharge reads
    /// as "discharged, lapsed on …"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed_at: Option<DateTime<Utc>>,
}

impl ObligationState {
    /// The state of `obligation` (its latest version) after `amendments`,
    /// those naming it with their `created_at`, in chain order. Kinds that
    /// are not obligation kinds change nothing here.
    ///
    /// A discharge or withdrawal closes it, and the last one wins. A lapse
    /// marks an open obligation lapsed, and a reschedule reopens a lapsed
    /// one: it has a new due date and has not missed it yet. Neither moves
    /// a closed one.
    pub fn from_amendments<'a>(
        obligation: &ObligationData,
        amendments: impl IntoIterator<Item = (DateTime<Utc>, &'a Amendment)>,
    ) -> Self {
        let mut state = Self {
            until: obligation.until.clone(),
            ..Self::default()
        };
        for (at, amendment) in amendments {
            let Some(update) = &amendment.update else {
                continue;
            };
            match update {
                ObligationUpdate::Planned { plan } => state.plan = Some(*plan),
                ObligationUpdate::Discharged { .. } => {
                    state.status = ObligationStatus::Discharged;
                }
                ObligationUpdate::Withdrawn => {
                    state.status = ObligationStatus::Withdrawn;
                }
                ObligationUpdate::Lapsed => {
                    state.lapsed_at = Some(at);
                    if state.status == ObligationStatus::Open {
                        state.status = ObligationStatus::Lapsed;
                    }
                }
                ObligationUpdate::Rescheduled { until } => {
                    state.until = until.clone();
                    state.until_authority = amendment.authority.clone();
                    if state.status == ObligationStatus::Lapsed {
                        state.status = ObligationStatus::Open;
                    }
                }
            }
        }
        state
    }
}

/// An obligation (`OBL-…`): the signed entry as the log has it now, its
/// latest version and what its amendments add up to.
///
/// The one type for an obligation. What was signed ([`ObligationData`])
/// and what it adds up to ([`ObligationState`]) are kept apart inside it on
/// purpose: the first is covered by a signature and never changes but by a
/// revision; the second is derived and does, and must never be signed by
/// mistake. Everything else (the to-do list's rows, the calendar's
/// deadlines, the serving types) is derived from this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obligation {
    pub id: GovernanceLogId,
    pub created_at: DateTime<Utc>,
    pub chain_seq: u64,
    /// Its latest version, every revision applied
    pub data: ObligationData,
    /// What its amendments add up to
    pub state: ObligationState,
}

/// Why a stored obligation entry could not be read as an [`Obligation`]
#[derive(Debug, thiserror::Error)]
pub enum ObligationError {
    #[error("its revisions do not apply: {0}")]
    Revisions(#[from] super::ReviseError),
    #[error("it is not an obligation: {0}")]
    Malformed(#[from] serde_json::Error),
}

impl Obligation {
    /// The entry `id` with its stored `data`, after `amendments`: those
    /// naming it, with their `created_at`, in chain order. Revisions are
    /// applied to `data` for the latest version, and the state is folded
    /// from all of them ([`ObligationState::from_amendments`]).
    pub fn from_log(
        id: GovernanceLogId,
        created_at: DateTime<Utc>,
        chain_seq: u64,
        data: &serde_json::Value,
        amendments: &[(DateTime<Utc>, Amendment)],
    ) -> Result<Self, ObligationError> {
        let latest = super::latest(
            data,
            amendments.iter().filter_map(|(_, a)| a.revision.as_ref()),
        )?;
        let data: ObligationData = serde_json::from_value(latest)?;
        let state = ObligationState::from_amendments(
            &data,
            amendments.iter().map(|(at, a)| (*at, a)),
        );
        Ok(Self {
            id,
            created_at,
            chain_seq,
            data,
            state,
        })
    }

    /// The `after_decision` decision its current deadline waits on: what
    /// a server looks up before [`crate::calendar::ResolvedObligation::new`]
    pub fn awaited_decision(&self) -> Option<&GovernanceLogId> {
        match &self.state.until {
            Some(Until::AfterDecision { decision }) => Some(decision),
            _ => None,
        }
    }
}

/// What the server knows about a governance entry that the obligation
/// rules need: see [`Obligation::check_amendment`]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownEntry {
    pub entry_type: GovernanceLogEntryType,
    pub chain_seq: u64,
}

/// An obligation amendment that may not be appended
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObligationRuleError {
    #[error(transparent)]
    Shape(#[from] AmendmentError),
    #[error("{0} is not an entry of the log")]
    UnknownEntry(GovernanceLogId),
    #[error(
        "the obligation is {0}; only a record closes it, and nothing reopens it"
    )]
    Closed(ObligationStatus),
    #[error("an undated obligation never lapses")]
    Undated,
    #[error("the obligation has already lapsed; reschedule it first")]
    AlreadyLapsed,
    #[error(
        "`{0}` here needs a Council decision as its `authority`: the \
         Council owes it, or set its deadline"
    )]
    NeedsCouncil(AmendmentKind),
    #[error(
        "the authority {authority} is not later than {earlier}: the \
         obligation, or the Council decision that set its deadline"
    )]
    AuthorityNotLater {
        authority: GovernanceLogId,
        earlier: GovernanceLogId,
    },
    #[error("evidence: {0:?} is not a commit sha (7 to 40 hex digits)")]
    BadSha(String),
    #[error("evidence: {0:?} is not a repository as `owner/name`")]
    BadRepo(String),
}

impl Obligation {
    /// Whether `amendment` may be appended to this obligation as it now
    /// stands. `entry` looks up an entry of the log by id.
    ///
    /// The rules (agora `docs/design-obligations.md`):
    ///
    /// - `planned`: anyone may record one;
    /// - `discharged`: the Steward's for Steward- and staff-owed obligations;
    ///   a Council-owed one needs a Council decision as `authority`;
    /// - `lapsed`: only a dated, open obligation lapses;
    /// - `rescheduled`, `withdrawn`: by whoever set the deadline. If a Council
    ///   decision did (the source, or the authority of the last reschedule),
    ///   `authority` must be a Council decision. A Council-owed obligation is
    ///   withdrawn only by the Council;
    /// - a `revision` never touches [`OBLIGATION_FIXED`]. One that narrows
    ///   `text` is a partial discharge: if the Council owes the obligation or
    ///   set its deadline, it needs a Council decision too. Its other fields
    ///   are clerical.
    ///
    /// A Council decision that authorizes anything here is later than the
    /// obligation and than the decision that set its deadline. Nothing but a
    /// revision or another non-obligation kind amends a closed obligation, and
    /// those kinds are otherwise not this method's business.
    pub fn check_amendment(
        &self,
        amendment: &Amendment,
        entry: impl Fn(&GovernanceLogId) -> Option<KnownEntry>,
    ) -> Result<(), ObligationRuleError> {
        let (obligation, id, chain_seq, state) =
            (&self.data, &self.id, self.chain_seq, &self.state);
        amendment.validate()?;
        if let Some(revision) = &amendment.revision
            && let Some(path) = revision_touches(revision, &OBLIGATION_FIXED)
        {
            return Err(
                AmendmentError::FixedObligationField(path.into()).into()
            );
        }
        let narrows_text = amendment
            .revision
            .as_ref()
            .is_some_and(|r| revision_touches(r, &["/text"]).is_some());
        if amendment.update.is_none() && !narrows_text {
            return Ok(());
        }
        if amendment.update.is_some() && state.status.is_closed() {
            return Err(ObligationRuleError::Closed(state.status));
        }
        let known = |id: &GovernanceLogId| {
            entry(id)
                .ok_or_else(|| ObligationRuleError::UnknownEntry(id.clone()))
        };
        let council = |id: &GovernanceLogId| -> Result<
            Option<KnownEntry>,
            ObligationRuleError,
        > {
            let e = known(id)?;
            Ok((e.entry_type == GovernanceLogEntryType::CouncilDecision)
                .then_some(e))
        };
        if let Some(authority) = &amendment.authority {
            known(authority)?;
        }
        // The Council decision that set the deadline, if one did
        let mut setter = None;
        for id in [obligation.source.as_ref(), state.until_authority.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(e) = council(id)? {
                setter = Some((id, e.chain_seq));
            }
        }
        // `authority`: a Council decision, later than the obligation and than
        // the decision that set its deadline
        let needs_council = || {
            let authority = amendment
                .authority
                .as_ref()
                .ok_or(ObligationRuleError::NeedsCouncil(amendment.kind))?;
            let decision = council(authority)?
                .ok_or(ObligationRuleError::NeedsCouncil(amendment.kind))?;
            for (earlier, seq) in
                [Some((id, chain_seq)), setter].into_iter().flatten()
            {
                if decision.chain_seq <= seq {
                    return Err(ObligationRuleError::AuthorityNotLater {
                        authority: authority.clone(),
                        earlier: earlier.clone(),
                    });
                }
            }
            Ok(())
        };
        let councils =
            obligation.owed_by == OwedBy::Council || setter.is_some();
        let Some(update) = &amendment.update else {
            // A revision narrowing `text`: a partial discharge
            return if councils { needs_council() } else { Ok(()) };
        };
        match update {
            ObligationUpdate::Planned { .. } => Ok(()),
            ObligationUpdate::Lapsed => match (&state.until, state.status) {
                (None, _) => Err(ObligationRuleError::Undated),
                (_, ObligationStatus::Lapsed) => {
                    Err(ObligationRuleError::AlreadyLapsed)
                }
                _ => Ok(()),
            },
            ObligationUpdate::Discharged { evidence } => {
                check_evidence(evidence, &known)?;
                match obligation.owed_by {
                    OwedBy::Council => needs_council(),
                    OwedBy::Steward | OwedBy::Staff => Ok(()),
                }
            }
            ObligationUpdate::Rescheduled { .. } => match setter {
                Some(_) => needs_council(),
                None => Ok(()),
            },
            ObligationUpdate::Withdrawn => match councils {
                true => needs_council(),
                false => Ok(()),
            },
        }
    }
}

fn check_evidence(
    evidence: &Evidence,
    known: &impl Fn(&GovernanceLogId) -> Result<KnownEntry, ObligationRuleError>,
) -> Result<(), ObligationRuleError> {
    let repo = |repo: &str| {
        let parts: Vec<&str> = repo.split('/').collect();
        match parts.as_slice() {
            [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(()),
            _ => Err(ObligationRuleError::BadRepo(repo.to_string())),
        }
    };
    match evidence {
        Evidence::Governance { id } => known(id).map(|_| ()),
        Evidence::PullRequest { repo: r, .. } => repo(r),
        Evidence::Commit { repo: r, sha } => {
            repo(r)?;
            let hex = (7..=40).contains(&sha.len())
                && sha.chars().all(|c| c.is_ascii_hexdigit());
            hex.then_some(())
                .ok_or_else(|| ObligationRuleError::BadSha(sha.clone()))
        }
        Evidence::Deploy { .. } => Ok(()),
    }
}

pub mod request;

#[cfg(test)]
mod tests;
