//! Tap levels and decision 9's sending rule: [`required_tap`] and
//! [`plan_send`], which the server enforces and the CLI and the portal
//! preview (agora `docs/design-operator-api.md`, decision 9)

use std::collections::BTreeSet;

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

/// `|R_this ∪ others|` if this send adds an agent not in `others`, else
/// `|R_this|`
pub fn reach_over<'a>(
    this: &BTreeSet<AgentId>,
    others: impl IntoIterator<Item = &'a AgentId>,
) -> u32 {
    let others: BTreeSet<&AgentId> = others.into_iter().collect();
    let size = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    if this.iter().all(|agent| others.contains(agent)) {
        size(this.len())
    } else {
        let union =
            others.len() + this.iter().filter(|a| !others.contains(a)).count();
        size(union)
    }
}

/// [`Reach`] for a send to `recipients` delivered at `deliver_at`, over
/// `messages` (the operator's uncancelled messages)
pub fn reach(
    recipients: &BTreeSet<AgentId>,
    deliver_at: DateTime<Utc>,
    messages: &[WindowMessage],
) -> Reach {
    // SPEC: decision 9, "within 24 h either side": both ends included
    let in_window = |m: &&WindowMessage| {
        (m.deliver_at - deliver_at).abs() <= window_half_width()
    };
    let window: Vec<&WindowMessage> =
        messages.iter().filter(in_window).collect();
    Reach {
        reach: reach_over(recipients, window.iter().map(|m| &m.recipient)),
        immediate_reach: reach_over(
            recipients,
            window.iter().filter(|m| m.immediate).map(|m| &m.recipient),
        ),
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

    /// Whether the send is announced at all
    pub fn announced(&self) -> bool {
        self.announced_held() || self.announced_now() || self.announced_tapped()
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
        Ok(SendPlan {
            deliver_at,
            delivery,
            reach,
            required,
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
