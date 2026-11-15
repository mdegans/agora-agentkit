//! Tap levels and decision 9's sending rule: [`required_tap`] and
//! [`plan_send`], which the server enforces and the CLI and the portal
//! preview (agora `docs/design-operator-api.md`, decision 9)

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::enums::{OperatorCredentialKind, PinPolicy, TouchPolicy};
use crate::ids::AgentId;
use crate::operator_signer::KeyPolicy;

/// How much presence a request proves, in decision 9's order. A
/// `software_key` gives [`None`](Self::None), as a session does; the
/// state table keeps it apart only for the tier-1 guard
/// ([`FactorKind::is_software_key`])
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum TapLevel {
    /// A portal session, or a `software_key`
    None,
    /// The unattended YubiKey: a `hardware_key` with touch policy never or
    /// `cached`
    Unattended,
    /// A passkey tap, or a sudo request (touch always)
    Tap,
    /// A passkey tap from a YubiKey, or a sudo request whose attested PIN
    /// policy is also always
    #[serde(rename = "yubikey_tap")]
    YubikeyTap,
}

impl TapLevel {
    /// Every level, lowest first
    pub const ALL: [Self; 4] =
        [Self::None, Self::Unattended, Self::Tap, Self::YubikeyTap];
}

/// What a passkey's registration showed about it (*Credential disputes*,
/// rule 5; *YubiKey passkeys*)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PasskeyClass {
    /// The registration set the BE flag (self-reported)
    pub synced: bool,
    /// A `packed` attestation chaining to the pinned Yubico FIDO root
    pub yubikey: bool,
}

/// A factor: a passkey, or an API credential and its attested policies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactorKind {
    Passkey(PasskeyClass),
    /// An Ed25519 key in software
    SoftwareKey,
    /// A key attested in hardware, with its PIN and touch policies
    HardwareKey(KeyPolicy),
}

impl FactorKind {
    /// The credential kind, for an API credential
    pub const fn credential_kind(&self) -> Option<OperatorCredentialKind> {
        match self {
            Self::Passkey(_) => None,
            Self::SoftwareKey => Some(OperatorCredentialKind::SoftwareKey),
            Self::HardwareKey(_) => Some(OperatorCredentialKind::HardwareKey),
        }
    }

    pub const fn is_passkey(&self) -> bool {
        matches!(self, Self::Passkey(_))
    }

    pub const fn is_software_key(&self) -> bool {
        matches!(self, Self::SoftwareKey)
    }

    /// A passkey whose registration proved it is a YubiKey
    pub const fn is_yubikey_passkey(&self) -> bool {
        matches!(self, Self::Passkey(PasskeyClass { yubikey: true, .. }))
    }

    /// The level of this factor's strongest act: a passkey's tap, or an API
    /// credential's signature (the state table's `tap_of`, at its top)
    pub const fn tap_level(&self) -> TapLevel {
        match self {
            Self::Passkey(PasskeyClass { yubikey: true, .. }) => {
                TapLevel::YubikeyTap
            }
            Self::Passkey(_) => TapLevel::Tap,
            Self::SoftwareKey => TapLevel::None,
            Self::HardwareKey(KeyPolicy {
                touch: TouchPolicy::Always,
                pin: PinPolicy::Always,
            }) => TapLevel::YubikeyTap,
            Self::HardwareKey(KeyPolicy {
                touch: TouchPolicy::Always,
                ..
            }) => TapLevel::Tap,
            Self::HardwareKey(_) => TapLevel::Unattended,
        }
    }

    /// `gives_yubikey_tap`: it can send now to any number of agents, so a new
    /// one waits ([`sends_now_from_for_add`](super::sends_now_from_for_add))
    pub const fn gives_yubikey_tap(&self) -> bool {
        matches!(self.tap_level(), TapLevel::YubikeyTap)
    }

    /// A sudo key: a `hardware_key` attested touch always
    /// ([`crate::operator::gives_sudo`])
    pub fn gives_sudo(&self) -> bool {
        match self {
            Self::HardwareKey(policy) => crate::operator::gives_sudo(
                OperatorCredentialKind::HardwareKey,
                Some(policy.touch),
            ),
            Self::Passkey(_) | Self::SoftwareKey => false,
        }
    }
}

/// Up to this reach, a send needs no tap and may go now
pub const UNTAPPED_REACH: u32 = 5;
/// Up to this reach a held send waits the bulk hold; above it, [`long_hold`]
pub const SHORT_HOLD_REACH: u32 = 50;
/// Above this reach the contact also gets the count and the first names
pub const NAMED_REACH: u32 = 500;

/// Each side of a send's `deliver_at` that its window spans
pub fn window_half_width() -> Duration {
    Duration::hours(24)
}

/// The span over which covered sends' agents are counted together: above
/// [`UNTAPPED_REACH`] distinct agents in one span, a covered send is
/// announced ([`covered_reach`])
pub fn covered_span() -> Duration {
    Duration::hours(24)
}

/// The shortest bulk hold (*Also in v1*)
pub fn bulk_hold_floor() -> Duration {
    Duration::hours(1)
}

/// The hold above [`SHORT_HOLD_REACH`], and on a new account above
/// [`UNTAPPED_REACH`] (or the bulk hold, if longer)
pub fn long_hold() -> Duration {
    Duration::hours(24)
}

/// How far off a send's `deliver_at` must be for it to count as held. A new
/// account holds a send whose immediate reach is above 5 at any reach
pub fn required_hold(
    reach: Reach,
    bulk_hold: Duration,
    new_account: bool,
) -> Duration {
    let long = reach.reach > SHORT_HOLD_REACH
        || (new_account && reach.immediate_reach > UNTAPPED_REACH);
    if long {
        bulk_hold.max(long_hold())
    } else {
        bulk_hold
    }
}

/// Whether a send goes now or is held
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// `deliver_at` sooner than now + its [`required_hold`]
    Immediate,
    /// `deliver_at` at least now + its [`required_hold`]
    Held,
}

impl Delivery {
    /// Held when `deliver_at` is at least `now` + `hold`
    pub fn of(
        deliver_at: DateTime<Utc>,
        now: DateTime<Utc>,
        hold: Duration,
    ) -> Self {
        // SPEC: decision 9, "held: deliver_at ≥ now + the hold"; so anything
        // sooner is a send that goes now, `dm_hold` included
        match now.checked_add_signed(hold) {
            Some(held) if deliver_at >= held => Self::Held,
            _ => Self::Immediate,
        }
    }
}

/// An operator message counted by decision 9's window: one recipient's
/// copy, of any send that isn't cancelled (a retracted one still counts)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowMessage {
    pub recipient: AgentId,
    pub deliver_at: DateTime<Utc>,
    /// Stored at admission: whether its send went now
    pub immediate: bool,
    /// Its send has been delivered (it may since have been read or retracted)
    pub delivered: bool,
    /// Its send was [covered] when last counted, at admission or at a move
    /// ([`SendPlan::covered`]): it counts toward a later covered send's
    /// [`covered_reach`]. Rows from before this field read `false` (see
    /// [`covered_reach`] on what that undercounts)
    pub covered: bool,
    /// The [`covered_reach`] the notices about its send reported, when they
    /// did ([`SendPlan::covered_told`], at admission or at a move): the
    /// baseline a later covered send must double to be announced again
    /// ([`covered_baseline`]). Rows from before this field read `None`
    pub covered_told: Option<u32>,
    /// The level of what admitted its send, or last brought it forward: the
    /// request's level at admission, a YubiKey tap after a **SendForward**,
    /// and unchanged by a **SendLater** (whoever moves it, a move later
    /// never changes who sent it). Only a count told on a send at
    /// [`TapLevel::None`] [feeds the baseline](feeds_baseline)
    pub level: TapLevel,
}

impl WindowMessage {
    /// Whether it went out: only a delivered message excuses a later send to
    /// its agent by D9's `|R_this|` term. One still scheduled has reached no
    /// one, however soon it is due, and may yet be cancelled or moved (Steward,
    /// 2026-11-14)
    pub const fn covers(&self) -> bool {
        self.delivered
    }
}

/// A send's reach and immediate reach, in distinct agents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct Reach {
    /// Over the window: decides the tap
    pub reach: u32,
    /// Over the window's immediate messages: decides whether it may go now
    pub immediate_reach: u32,
}

/// `|R_this|` if every agent of this send is in `cover` (the part of `others`
/// that went out, [`WindowMessage::covers`]), else `|R_this ∪ others|`
pub fn reach_over<'a>(
    this: &BTreeSet<AgentId>,
    others: impl IntoIterator<Item = &'a AgentId>,
    cover: impl IntoIterator<Item = &'a AgentId>,
) -> u32 {
    if all_in(this, cover) {
        size(this.len())
    } else {
        let others: BTreeSet<&AgentId> = others.into_iter().collect();
        let union =
            others.len() + this.iter().filter(|a| !others.contains(a)).count();
        size(union)
    }
}

fn size(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Whether every agent of `this` is in `cover`: D9's `|R_this|` term applies
fn all_in<'a>(
    this: &BTreeSet<AgentId>,
    cover: impl IntoIterator<Item = &'a AgentId>,
) -> bool {
    let cover: BTreeSet<&AgentId> = cover.into_iter().collect();
    this.iter().all(|agent| cover.contains(agent))
}

/// The messages of decision 9's window for a send at `deliver_at`
fn window_of(
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> impl Iterator<Item = &WindowMessage> {
    // SPEC: decision 9, "within 24 h either side": both ends included
    messages.iter().filter(move |m| {
        (m.deliver_at - deliver_at).abs() <= window_half_width()
    })
}

/// Whether a send to `recipients` at `deliver_at` is **covered**: every one
/// of its agents had a message in its window that went out, so its
/// [reach](Reach::reach) is `|R_this|` (decision 9, *Why |R_this| counts*)
pub fn covered(
    recipients: &BTreeSet<AgentId>,
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> bool {
    all_in(
        recipients,
        window_of(deliver_at, messages)
            .filter(|m| m.covers())
            .map(|m| &m.recipient),
    )
}

/// How much a covered send's [`covered_reach`] must exceed the count already
/// told in its window ([`covered_baseline`]) to be announced again: more than
/// twice it
pub const COVERED_GROWTH: u32 = 2;

/// For a covered send to `recipients` at `deliver_at`: the most distinct
/// agents that it and the [covered](WindowMessage::covered) messages reach
/// in any closed [`covered_span`] (24 h, both ends included) containing its
/// `deliver_at`. Above [`UNTAPPED_REACH`], and above [`COVERED_GROWTH`] times
/// its [`covered_baseline`], the send is announced
/// ([`SendPlan::announced_covered`]).
///
/// SPEC: decision 9, keep-warm (Steward, 2026-11-14: announce, don't block).
/// `|R_this|` lets each covered send pass on its own size, so a session that
/// keeps agents warm with daily chats of 5 could later reach all of them in
/// one burst of small covered sends, untapped and unannounced. This counts
/// covered sends together. It never refuses a send and never asks for a
/// tap: [`required_tap`] doesn't see it. What stays quiet in a 24 h span is
/// 5 agents by fresh sends (the union) plus 5 by covered ones, and a covered
/// count up to twice what was last told within 24 h on an untapped send.
///
/// **Why on `deliver_at`, in a 24 h span.** What is counted is what lands.
/// Choosing `deliver_at` can't dodge it: a covered send's cover had been
/// delivered when the send was counted (at admission, or at a move) and lies
/// within 24 h of its `deliver_at`, so a covered send lands within 24 h of
/// being counted. A burst admitted together therefore lands within one 24 h
/// span, whatever times it asks for; to stay quiet, landings must keep to 5
/// covered agents in every 24 h, the untapped rate, and that is no burst.
/// Counting on admission instead would miss sends admitted apart that land
/// together, or one moved beside the others (a move is counted again, under
/// the rule for a new send). The span is 24 h rather than the window's
/// ±24 h so that ordinary conversation costs at most about one notice a day.
///
/// **Migration.** Rows stored before [`WindowMessage::covered`] existed
/// read `false`, so for the first 24 h after the server starts storing it,
/// covered sends from before are not counted and the count is low; after
/// that every message in a window has its flag.
///
/// **Cost.** One sort and one sweep: O(m log m) in the m covered messages
/// within 24 h either side, each start time once and each message added and
/// removed once. A 5000-agent fleet sent three times counts in milliseconds
/// (`covered_reach_is_fast_on_a_fleet`); it runs under the operator's lock.
pub fn covered_reach(
    recipients: &BTreeSet<AgentId>,
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> u32 {
    let span = covered_span();
    // This send's own agents are in every span; count only the others
    let mut others: Vec<(DateTime<Utc>, &AgentId)> = messages
        .iter()
        .filter(|m| {
            m.covered
                && (m.deliver_at - deliver_at).abs() <= span
                && !recipients.contains(&m.recipient)
        })
        .map(|m| (m.deliver_at, &m.recipient))
        .collect();
    others.sort_unstable_by_key(|(at, _)| *at);
    // The busiest span containing `deliver_at` can be slid later, losing
    // nothing, until it starts at a message's time or at `deliver_at`
    let mut starts: Vec<DateTime<Utc>> = others
        .iter()
        .map(|(at, _)| *at)
        .filter(|at| *at <= deliver_at)
        .collect();
    starts.push(deliver_at);
    starts.dedup();
    let mut counts: BTreeMap<&AgentId, u32> = BTreeMap::new();
    let (mut first, mut next, mut best) = (0, 0, 0);
    for start in starts {
        let end = start
            .checked_add_signed(span)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        while let Some(&(_, agent)) =
            others.get(next).filter(|(at, _)| *at <= end)
        {
            *counts.entry(agent).or_default() += 1;
            next += 1;
        }
        while let Some(&(_, agent)) =
            others.get(first).filter(|(at, _)| *at < start)
        {
            if let Some(n) = counts.get_mut(agent) {
                *n -= 1;
                if *n == 0 {
                    counts.remove(agent);
                }
            }
            first += 1;
        }
        best = best.max(counts.len());
    }
    size(recipients.len() + best)
}

/// The most a notice has told of covered sends on untapped sends within 24 h
/// either side of `deliver_at` (the spans [`covered_reach`] counts over): the
/// largest
/// [`WindowMessage::covered_told`] there that [feeds the
/// baseline](feeds_baseline), or 0.
///
/// A steady conversation with more than 5 agents is then told about once a
/// day, while a growing gather is told each time it doubles. Taking away
/// can't lift it: a retracted send still counts, as it does for reach, and
/// a cancelled one leaves the window, which lowers the baseline and only
/// brings the next notice sooner
pub fn covered_baseline(
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> u32 {
    messages
        .iter()
        .filter(|m| (m.deliver_at - deliver_at).abs() <= covered_span())
        .filter_map(feeds_baseline)
        .max()
        .unwrap_or(0)
}

/// Which told counts feed [`covered_baseline`]: only a count told on a send
/// made without a tap ([`WindowMessage::level`] is [`TapLevel::None`]: a
/// portal session or a `software_key`).
///
/// SPEC: Steward, 2026-11-15: a told count excuses later sends only if it
/// was told on an untapped send, so a stolen session can't ride the owner's
/// tapped or held sends. Counted against everything, a broadcast the owner
/// tapped to 100 agents would let a session reach 200 of them in small
/// covered sends without a notice (review of #311, R1). A session's own
/// counts still feed it, so its steady conversation is told about once a
/// day, and a session send brought forward by a YubiKey tap stops feeding it
/// (the safe direction: fewer excuses, never more)
pub fn feeds_baseline(message: &WindowMessage) -> Option<u32> {
    message
        .covered_told
        .filter(|_| message.level == TapLevel::None)
}

/// [`Reach`] for a send to `recipients` delivered at `deliver_at`, over
/// `messages` (the operator's uncancelled messages)
pub fn reach(
    recipients: &BTreeSet<AgentId>,
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> Reach {
    let window: Vec<&WindowMessage> = window_of(deliver_at, messages).collect();
    let over = |messages: Vec<&&WindowMessage>| {
        reach_over(
            recipients,
            messages.iter().map(|m| &m.recipient),
            messages.iter().filter(|m| m.covers()).map(|m| &m.recipient),
        )
    };
    Reach {
        reach: over(window.iter().collect()),
        immediate_reach: over(window.iter().filter(|m| m.immediate).collect()),
    }
}

/// The level a send needs (decision 9).
///
/// - reach ≤ 5: none;
/// - reach above 5: [`TapLevel::Unattended`] held, [`TapLevel::Tap`] now;
/// - now, with an immediate reach above 5: a YubiKey tap, at any reach.
///
/// A new key and a new account can't send now above 5 at any level:
/// [`plan_send`] takes those
pub fn required_tap(reach: Reach, delivery: Delivery) -> TapLevel {
    match delivery {
        Delivery::Held if reach.reach <= UNTAPPED_REACH => TapLevel::None,
        Delivery::Held => TapLevel::Unattended,
        Delivery::Immediate if reach.immediate_reach > UNTAPPED_REACH => {
            TapLevel::YubikeyTap
        }
        Delivery::Immediate if reach.reach > UNTAPPED_REACH => TapLevel::Tap,
        Delivery::Immediate => TapLevel::None,
    }
}

/// When a send asks to be delivered
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum When {
    /// Neither `deliver_at` nor `send_now`: now + `dm_hold` if it may go
    /// now, else now + the bulk hold
    Default,
    /// `send_now`
    Now,
    /// An explicit `deliver_at` (one in the past is now)
    At(DateTime<Utc>),
}

/// What decision 9 needs to know about a send
#[derive(Debug, Clone, Copy)]
pub struct SendRequest<'a> {
    pub recipients: &'a BTreeSet<AgentId>,
    pub when: When,
    /// The level of the request's authority
    pub level: TapLevel,
    /// The account's emergency contact is verified, on another domain
    pub contact_ok: bool,
    pub now: DateTime<Utc>,
    /// `sends_now` of the factor behind the request
    pub sends_now: bool,
    /// `new_account` of the operator's account
    pub new_account: bool,
    /// The operator's default hold
    pub dm_hold: Duration,
    /// The operator's bulk hold (at least [`bulk_hold_floor`])
    pub bulk_hold: Duration,
    /// The operator's uncancelled messages
    pub window: &'a [WindowMessage],
}

/// A send decision 9 admits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendPlan {
    pub deliver_at: DateTime<Utc>,
    pub delivery: Delivery,
    pub reach: Reach,
    /// What [`required_tap`] asked for
    pub required: TapLevel,
    /// Every agent of it had a message in its window that went out
    /// ([`covered`]), so its reach is `|R_this|`. The server stores it on the
    /// send's messages ([`WindowMessage::covered`]), and again at a move
    pub covered: bool,
    /// When [`covered`](Self::covered), its [`covered_reach`]; else 0
    pub covered_reach: u32,
    /// When [`covered`](Self::covered), its [`covered_baseline`]: the most
    /// already told of covered sends near it; else 0
    pub covered_baseline: u32,
}

impl SendPlan {
    /// Held with reach above 5: the operator gets a cancel link, the
    /// contact a freeze link
    pub fn announced_held(&self) -> bool {
        self.delivery == Delivery::Held && self.reach.reach > UNTAPPED_REACH
    }

    /// Going now with immediate reach above 5: both addresses are told, the
    /// contact with a freeze link
    pub fn announced_now(&self) -> bool {
        self.delivery == Delivery::Immediate
            && self.reach.immediate_reach > UNTAPPED_REACH
    }

    /// Any send that took a tap (its required level is a passkey tap or
    /// more) is announced, the contact's copy with a freeze link; this is
    /// the case the other two miss: now, reach 6–500, immediate reach ≤ 5
    pub fn announced_tapped(&self) -> bool {
        self.required >= TapLevel::Tap
    }

    /// Covered, with covered sends reaching more than 5 agents in a 24 h
    /// span ([`covered_reach`]), and more than [`COVERED_GROWTH`] times what
    /// was already told near it ([`covered_baseline`]): announced like a held
    /// send above 5, the operator with a cancel link, the contact with a
    /// freeze link, both with the count ([`covered_told`](Self::covered_told)).
    /// Only announced: it needs no tap and is never refused for it (decision
    /// 9, keep-warm). This is the case the other three miss: a burst of
    /// small sends to agents kept covered
    pub fn announced_covered(&self) -> bool {
        self.covered
            && self.covered_reach > UNTAPPED_REACH
            && self.covered_reach
                > self.covered_baseline.saturating_mul(COVERED_GROWTH)
    }

    /// The count a notice about this send reports, and the server stores on
    /// its messages ([`WindowMessage::covered_told`]): its
    /// [`covered_reach`] when that is above 5 and the send is
    /// [announced](Self::announced) by any rule, so the operator and the
    /// contact can tell 6 from 500. Else `None`
    pub fn covered_told(&self) -> Option<u32> {
        self.covered_count().filter(|_| self.announced())
    }

    /// Its [`covered_reach`] when covered and above 5, announced or not
    pub fn covered_count(&self) -> Option<u32> {
        (self.covered && self.covered_reach > UNTAPPED_REACH)
            .then_some(self.covered_reach)
    }

    /// Whether the send is announced at all
    pub fn announced(&self) -> bool {
        self.announced_held()
            || self.announced_now()
            || self.announced_tapped()
            || self.announced_covered()
    }

    /// Above 500: the contact also gets the count and the first names
    pub fn names_to_contact(&self) -> bool {
        self.reach.reach > NAMED_REACH
    }
}

/// Why decision 9 refused a send
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendRefusal {
    /// The request's level is below what the send needs
    #[error("this send needs {required:?} (reach {reach:?})")]
    NeedsTap { required: TapLevel, reach: Reach },
    /// A key in its first 72 h can't send now above 5 agents: it must be held,
    /// or sent with another YubiKey
    #[error("this key is new: this send has to be held")]
    NewKey { reach: Reach },
    /// A new account can't send now above 5 agents, at any level
    #[error("new accounts can't send immediately to this many agents yet")]
    NewAccount { reach: Reach },
    /// Reach above 5 needs a verified emergency contact on another domain
    #[error(
        "a send to more than 5 agents needs a qualifying emergency contact"
    )]
    NeedsContact { reach: Reach },
    /// A send to nobody
    #[error("a send needs at least one recipient")]
    NoRecipients,
    /// A hold that runs past the end of time
    #[error("the delivery time is out of range")]
    OutOfRange,
}

/// Decision 9 for one send: where it lands, and whether `level` may make it.
///
/// An explicit time the level can't send is refused with what it needs;
/// nothing is silently delayed
pub fn plan_send(request: &SendRequest<'_>) -> Result<SendPlan, SendRefusal> {
    if request.recipients.is_empty() {
        return Err(SendRefusal::NoRecipients);
    }
    let now = request.now;
    let plan = |deliver_at: DateTime<Utc>| {
        let reach = reach(request.recipients, deliver_at, request.window);
        let hold = required_hold(reach, request.bulk_hold, request.new_account);
        let delivery = Delivery::of(deliver_at, now, hold);
        let required = required_tap(reach, delivery);
        let now_above_5 = delivery == Delivery::Immediate
            && reach.immediate_reach > UNTAPPED_REACH;
        if now_above_5 && request.new_account {
            return Err(SendRefusal::NewAccount { reach });
        }
        if request.level < required {
            return Err(SendRefusal::NeedsTap { required, reach });
        }
        if now_above_5 && !request.sends_now {
            return Err(SendRefusal::NewKey { reach });
        }
        let covered = covered(request.recipients, deliver_at, request.window);
        let (covered_reach, covered_baseline) = if covered {
            (
                covered_reach(request.recipients, deliver_at, request.window),
                covered_baseline(deliver_at, request.window),
            )
        } else {
            (0, 0)
        };
        Ok(SendPlan {
            deliver_at,
            delivery,
            reach,
            required,
            covered,
            covered_reach,
            covered_baseline,
        })
    };
    let planned = match request.when {
        When::Now => plan(now),
        When::At(at) => plan(at.max(now)),
        // SPEC: decision 9's default is "now + dm_hold when the send may go
        // now, and otherwise now + the hold", and the window is on deliver_at,
        // so each is tried at its own time
        When::Default => {
            let after = |hold| {
                now.checked_add_signed(hold).ok_or(SendRefusal::OutOfRange)
            };
            plan(after(request.dm_hold)?)
                .or_else(|_| plan(after(request.bulk_hold)?))
                .or_else(|_| plan(after(request.bulk_hold.max(long_hold()))?))
        }
    }?;
    if planned.reach.reach > UNTAPPED_REACH && !request.contact_ok {
        return Err(SendRefusal::NeedsContact {
            reach: planned.reach,
        });
    }
    Ok(planned)
}
