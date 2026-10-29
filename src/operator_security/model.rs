//! The account's state: factors, sessions, sends, pending changes, the
//! contact, recovery cases, and the predicates the state table derives
//! from them (agora `docs/design-operator-state-table.md`, §1 and §4.1)

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};

use super::tap::{
    Delivery, FactorKind, PasskeyClass, Reach, WindowMessage, bulk_hold_floor,
};
use crate::ids::{
    AgentId, OAuthClientId, OperatorActionId, OperatorCredentialId,
    OperatorEmailAddressId, OperatorEnrolmentId, OperatorFreezeLinkId,
    OperatorId, OperatorPasskeyId, OperatorPendingChangeId,
    OperatorRecoveryCaseId, OperatorSessionId,
};

/// A suspension's wait before revocation
pub fn suspension_deadline() -> Duration {
    Duration::hours(72)
}

/// The longest default hold a change may set (the operator config's "at
/// most a day")
pub fn max_dm_hold() -> Duration {
    Duration::days(1)
}

/// The longest bulk hold a change may set
pub fn max_bulk_hold() -> Duration {
    // SPEC: the spec bounds the bulk hold below (≥ 1 h) but names no upper
    // bound; a week, the longest hold its threat discussion mentions
    Duration::days(7)
}

/// A pending change's wait (decision 10)
pub fn change_wait() -> Duration {
    Duration::hours(72)
}

/// A recovery outcome's lock (CD6)
pub fn recovery_lock() -> Duration {
    Duration::days(7)
}

/// How old a factor must be to unfreeze, and how far back `recent_desc`
/// looks (decision 18)
pub fn unfreeze_age() -> Duration {
    Duration::days(7)
}

/// A freeze link's life (decision 18)
pub fn freeze_link_life() -> Duration {
    Duration::days(7)
}

/// One freeze per address per this long (decision 18)
pub fn freeze_interval() -> Duration {
    Duration::hours(24)
}

/// A session's sliding expiry (`SESSION_TTL`)
pub fn session_ttl() -> Duration {
    Duration::days(30)
}

/// A passkey or an API credential
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactorId {
    Passkey(OperatorPasskeyId),
    Credential(OperatorCredentialId),
}

/// Why a factor is suspended
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuspensionCause {
    /// A passkey tap suspended it directly
    Direct { by: FactorId },
    /// Its root's suspension reached it; `by` is the root's suspender
    /// (none for a recovery's)
    Cascade {
        root: FactorId,
        by: Option<FactorId>,
    },
    /// A freeze excluded it
    Freeze { entry: OperatorActionId },
    /// The losing side of a counter-claim
    Recovery { case: OperatorRecoveryCaseId },
}

/// A suspension: its cause, its `credential_suspend` entry and its clock
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Suspension {
    pub cause: SuspensionCause,
    /// The per-target `credential_suspend` entry, which a contest names
    pub entry: OperatorActionId,
    pub at: DateTime<Utc>,
    /// 72 h after `at`, or the root's while it follows one
    pub deadline: DateTime<Utc>,
    /// The root whose fate it shares, until it contests on its own
    pub follows_root: Option<FactorId>,
}

/// A factor's state (§2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialState {
    /// A passkey registered in this session, not yet approved
    Pending {
        session: OperatorSessionId,
    },
    Active,
    Suspended(Suspension),
    /// Suspended, clock stopped, in manual recovery
    Contested {
        suspension: Suspension,
        case: OperatorRecoveryCaseId,
    },
    Revoked,
    /// A pending passkey never approved
    Discarded,
}

impl CredentialState {
    /// `effective`
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// `live`: not revoked (nor discarded or pending)
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Active | Self::Suspended(_) | Self::Contested { .. }
        )
    }

    /// Suspended or contested
    pub const fn suspension(&self) -> Option<&Suspension> {
        match self {
            Self::Suspended(s) | Self::Contested { suspension: s, .. } => {
                Some(s)
            }
            _ => None,
        }
    }

    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Revoked | Self::Discarded)
    }
}

/// One factor of the account
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Factor {
    pub kind: FactorKind,
    pub state: CredentialState,
    /// The factor whose tap or signature approved this one's add; set once
    pub parent: Option<FactorId>,
    pub added_at: DateTime<Utc>,
    /// A passkey has signed in before (its first sign-in is announced)
    pub signed_in: bool,
}

/// How the account signs in (§4.1 `auth`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMode {
    /// With a password; no passkey yet
    PasswordOnly,
    Passkeys,
}

/// A freeze in force
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frozen {
    /// The freeze's log entry: 0x08 and 0x0F name it
    pub entry: OperatorActionId,
    /// The factor behind the notice the freeze was made on
    pub excluded: FactorId,
    pub at: DateTime<Utc>,
    /// The address that froze
    pub address: OperatorEmailAddressId,
    /// The compromise notice has gone out
    pub notice_sent: bool,
}

/// `hold`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hold {
    Open,
    Frozen(Frozen),
}

/// `standing`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStanding {
    Good,
    Banned,
}

/// The three enforcement flags and the password cutover: platform-wide,
/// each off → on only
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags {
    /// `AGORA_OPERATOR_TIER_ENFORCEMENT`: no `software_key` above 5
    pub tier: bool,
    /// `AGORA_OPERATOR_TIER_MINIMUM_ENFORCEMENT`: the contact and two
    /// devices above 5
    pub minimum: bool,
    /// `AGORA_OPERATOR_YUBIKEY_MINIMUM_ENFORCEMENT`: a YubiKey above 50
    pub yubikey: bool,
    /// The password cutover has happened
    pub cutover: bool,
}

/// A flag the platform turns on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    Tier,
    Minimum,
    Yubikey,
}

impl Flags {
    /// Whether every flag on in `self` is on in `later`
    pub fn none_turned_off(&self, later: &Self) -> bool {
        (!self.tier || later.tier)
            && (!self.minimum || later.minimum)
            && (!self.yubikey || later.yubikey)
            && (!self.cutover || later.cutover)
    }
}

/// Who controls an address, for decision 18's domain rule: its registrable
/// domain, or, on an allow-listed consumer domain, the address itself. The
/// server computes it (public-suffix aware); the model only compares
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailTenant(pub String);

/// An address as the server normalised it: what a change's canonical form
/// and the first contact's tap (0x0D) bind
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalisedEmail(pub String);

/// An email address the operator registered
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress {
    pub id: OperatorEmailAddressId,
    pub address: NormalisedEmail,
    pub tenant: MailTenant,
}

/// Whether the contact's address has been proven to receive mail
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContactState {
    Unverified,
    Verified,
}

/// The contact's freeze power
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreezePower {
    Armed,
    Disarmed,
}

/// The emergency contact (§7.1)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmergencyContact {
    pub address: EmailAddress,
    pub state: ContactState,
    pub power: FreezePower,
}

/// A setting that changes through §5
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Setting {
    DmHold,
    BulkHold,
    AgentReplyDailyCap,
    EmergencyContact,
    PrimaryEmail,
    ActionLogPublic,
}

impl Setting {
    /// Its name in a change's canonical form
    /// ([`super::challenge::change_canonical_form`])
    pub const fn name(self) -> &'static str {
        match self {
            Self::DmHold => "dm_hold",
            Self::BulkHold => "bulk_hold",
            Self::AgentReplyDailyCap => "agent_reply_daily_cap",
            Self::EmergencyContact => "emergency_contact",
            Self::PrimaryEmail => "primary_email",
            Self::ActionLogPublic => "action_log_public",
        }
    }
}

/// A requested change: a setting and its new value
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SettingChange {
    DmHold(Duration),
    BulkHold(Duration),
    AgentReplyDailyCap(u32),
    /// Replace the contact, or remove it (`None`)
    EmergencyContact(Option<EmailAddress>),
    PrimaryEmail(EmailAddress),
    /// Make the action log public (withdrawing is **LogWithdraw**)
    PublishLog,
}

impl SettingChange {
    pub const fn setting(&self) -> Setting {
        match self {
            Self::DmHold(_) => Setting::DmHold,
            Self::BulkHold(_) => Setting::BulkHold,
            Self::AgentReplyDailyCap(_) => Setting::AgentReplyDailyCap,
            Self::EmergencyContact(_) => Setting::EmergencyContact,
            Self::PrimaryEmail(_) => Setting::PrimaryEmail,
            Self::PublishLog => Setting::ActionLogPublic,
        }
    }

    /// The alarm channels and publication wait 72 h whatever the tap
    pub const fn always_waits(&self) -> bool {
        matches!(
            self.setting(),
            Setting::EmergencyContact
                | Setting::PrimaryEmail
                | Setting::ActionLogPublic
        )
    }

    /// Its canonical form for the 0x06 tap
    /// ([`change_canonical_form`](super::challenge::change_canonical_form)):
    /// a duration in seconds, a count in decimal, an address normalised,
    /// publishing the log `true`, and removing the contact the empty string
    pub fn canonical_form(
        &self,
    ) -> Result<Vec<u8>, super::challenge::PartTooLong> {
        let value = match self {
            Self::DmHold(d) | Self::BulkHold(d) => d.num_seconds().to_string(),
            Self::AgentReplyDailyCap(n) => n.to_string(),
            Self::EmergencyContact(Some(a)) | Self::PrimaryEmail(a) => {
                a.address.0.clone()
            }
            Self::EmergencyContact(None) => String::new(),
            Self::PublishLog => "true".into(),
        };
        super::challenge::change_canonical_form(self.setting().name(), &value)
    }

    /// The address it brings in, which must be verified by the effective
    /// time
    pub const fn new_address(&self) -> Option<&EmailAddress> {
        match self {
            Self::EmergencyContact(Some(a)) | Self::PrimaryEmail(a) => Some(a),
            _ => None,
        }
    }
}

/// Where a change stands (§5)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeState {
    Pending,
    Effective,
    Cancelled,
}

/// One requested setting change
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingChange {
    pub change: SettingChange,
    /// The passkey that tapped for it
    pub requested_by: FactorId,
    pub at: DateTime<Utc>,
    pub state: ChangeState,
    /// Its new address (if any) has been verified
    pub address_verified: bool,
}

/// The operator's settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Settings {
    pub dm_hold: Duration,
    pub bulk_hold: Duration,
    pub agent_reply_daily_cap: u32,
    pub action_log_public: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dm_hold: Duration::minutes(1),
            bulk_hold: bulk_hold_floor(),
            agent_reply_daily_cap: 50,
            action_log_public: false,
        }
    }
}

/// What opened a session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionOpener {
    Passkey(OperatorPasskeyId),
    Password,
}

/// A portal session (§10)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Session {
    pub opener: SessionOpener,
    pub last_used: DateTime<Utc>,
    pub open: bool,
}

/// What admitted a send: the factor behind the request, or for an
/// untapped portal send, the session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdmittedBy {
    Factor(FactorId),
    Session(OperatorSessionId),
}

/// Where a send stands (§6)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SendState {
    Scheduled,
    Delivered,
    Read,
    Retracted,
    Cancelled,
}

/// One send: a message to one or more of the operator's agents
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Send {
    pub recipients: BTreeSet<AgentId>,
    pub deliver_at: DateTime<Utc>,
    pub delivery: Delivery,
    pub reach: Reach,
    pub admitted_by: AdmittedBy,
    pub state: SendState,
}

/// The device flow's states (§3)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnrolmentState {
    /// Waiting for the operator's tap (the wire's `pending`)
    Pending,
    Approved(OperatorCredentialId),
    Refused,
    Expired,
}

/// One device-flow enrolment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Enrolment {
    /// An API credential's kind and policies
    pub kind: FactorKind,
    pub state: EnrolmentState,
}

/// What a recovery case is about
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseSubject {
    /// A contested factor
    Contest(FactorId),
    /// No usable factor left: stranded or locked out
    NoFactor,
}

/// A decided case's outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoveryOutcome {
    Reinstate,
    Revoke,
    /// A passkey registered on the call, for an account with no usable
    /// factor
    NewPasskey {
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
    },
}

/// A case's state (§9)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseState {
    Open {
        round: u8,
    },
    Decided {
        outcome: RecoveryOutcome,
        at: DateTime<Utc>,
        round: u8,
    },
    Effective,
}

/// One manual-recovery case
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecoveryCase {
    pub subject: CaseSubject,
    pub opened_at: DateTime<Utc>,
    /// The factors effective when it opened: only they may counter-claim
    pub effective_at_open: BTreeSet<FactorId>,
    pub state: CaseState,
}

/// A freeze link sent to the contact (decision 18)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FreezeLink {
    /// The address it went to
    pub address: OperatorEmailAddressId,
    /// The factor behind the notice: what a freeze on it suspends
    pub factor: FactorId,
    pub sent_at: DateTime<Utc>,
    pub used: bool,
    /// Its address stopped being the contact
    pub void: bool,
}

/// An operator account, as the state table models it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountState {
    pub operator: OperatorId,
    pub auth: AuthMode,
    pub hold: Hold,
    pub standing: AccountStanding,
    /// `tier_agent_count`: live agents and those in their departure grace
    pub count: u32,
    /// Departed agents still in their grace (counted in `count`)
    pub departed: u32,
    pub flags: Flags,
    pub primary: EmailAddress,
    pub contact: Option<EmergencyContact>,
    pub factors: BTreeMap<FactorId, Factor>,
    pub enrolments: BTreeMap<OperatorEnrolmentId, Enrolment>,
    pub sessions: BTreeMap<OperatorSessionId, Session>,
    pub changes: BTreeMap<OperatorPendingChangeId, PendingChange>,
    pub sends: BTreeMap<OperatorActionId, Send>,
    /// OAuth grants, by (client, agent), with the factor that consented
    pub grants: BTreeMap<(OAuthClientId, AgentId), FactorId>,
    pub cases: BTreeMap<OperatorRecoveryCaseId, RecoveryCase>,
    pub freeze_links: BTreeMap<OperatorFreezeLinkId, FreezeLink>,
    /// Each address's last freeze
    pub last_freeze: BTreeMap<OperatorEmailAddressId, DateTime<Utc>>,
    /// When each direct suspension happened, for the rate limit
    pub direct_suspensions: Vec<DateTime<Utc>>,
    pub settings: Settings,
    /// `SUSPEND_LIMIT`: direct suspensions per account per hour
    pub suspend_limit: u32,
}

impl AccountState {
    /// An account as it stands before M2's events: every factor a root
    /// (`parent = None`, I-11's "roots by migration"), each `Active` and
    /// added at its own time
    pub fn migrated(
        operator: OperatorId,
        primary: EmailAddress,
        auth: AuthMode,
        factors: impl IntoIterator<Item = (FactorId, FactorKind, DateTime<Utc>)>,
        suspend_limit: u32,
    ) -> Self {
        Self {
            operator,
            auth,
            hold: Hold::Open,
            standing: AccountStanding::Good,
            count: 0,
            departed: 0,
            flags: Flags::default(),
            primary,
            contact: None,
            factors: factors
                .into_iter()
                .map(|(id, kind, added_at)| {
                    let factor = Factor {
                        kind,
                        state: CredentialState::Active,
                        parent: None,
                        added_at,
                        signed_in: false,
                    };
                    (id, factor)
                })
                .collect(),
            enrolments: BTreeMap::new(),
            sessions: BTreeMap::new(),
            changes: BTreeMap::new(),
            sends: BTreeMap::new(),
            grants: BTreeMap::new(),
            cases: BTreeMap::new(),
            freeze_links: BTreeMap::new(),
            last_freeze: BTreeMap::new(),
            direct_suspensions: Vec::new(),
            settings: Settings::default(),
            suspend_limit,
        }
    }

    /// **Register**: sign-up with a passkey, the one add without a tap
    pub fn register(
        operator: OperatorId,
        primary: EmailAddress,
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
        now: DateTime<Utc>,
        suspend_limit: u32,
    ) -> Self {
        Self::migrated(
            operator,
            primary,
            AuthMode::Passkeys,
            [(FactorId::Passkey(passkey), FactorKind::Passkey(class), now)],
            suspend_limit,
        )
    }

    pub fn factor(&self, id: FactorId) -> Option<&Factor> {
        self.factors.get(&id)
    }

    /// `effective(F)`
    pub fn effective(&self, id: FactorId) -> bool {
        self.factor(id).is_some_and(|f| f.state.is_active())
    }

    /// `parent⁻¹`: the factors `id` approved
    pub fn children(
        &self,
        id: FactorId,
    ) -> impl Iterator<Item = FactorId> + '_ {
        self.factors
            .iter()
            .filter(move |(_, f)| f.parent == Some(id))
            .map(|(child, _)| *child)
    }

    /// `desc(F)`: everything added through `id`, transitively, whatever its
    /// state
    pub fn desc(&self, id: FactorId) -> BTreeSet<FactorId> {
        let mut out = BTreeSet::new();
        let mut todo: Vec<FactorId> = self.children(id).collect();
        while let Some(next) = todo.pop() {
            if out.insert(next) {
                todo.extend(self.children(next));
            }
        }
        out
    }

    /// `cascade(T, S)`: `target`, and every effective factor reached from it
    /// along approvals without passing through `suspender`. With the
    /// suspender the target itself, only the target
    pub fn cascade(
        &self,
        target: FactorId,
        suspender: Option<FactorId>,
    ) -> BTreeSet<FactorId> {
        let mut out = BTreeSet::from([target]);
        if suspender == Some(target) {
            return out;
        }
        let mut seen = BTreeSet::new();
        let mut todo: Vec<FactorId> = self.children(target).collect();
        while let Some(next) = todo.pop() {
            if Some(next) == suspender || !seen.insert(next) {
                continue;
            }
            if self.effective(next) {
                out.insert(next);
            }
            todo.extend(self.children(next));
        }
        out
    }

    /// `recent_desc(u, t)`: what `u` added, through anything, in the 7
    /// days up to `t`
    pub fn recent_desc(
        &self,
        u: FactorId,
        t: DateTime<Utc>,
    ) -> BTreeSet<FactorId> {
        self.desc(u)
            .into_iter()
            .filter(|d| {
                self.factor(*d).is_some_and(|f| {
                    // SPEC: §1 "added in the 7 days before time t": the
                    // complement of `eligible`'s age test, so (t − 7 d, t]
                    f.added_at > t - unfreeze_age() && f.added_at <= t
                })
            })
            .collect()
    }

    /// `eligible(u, X, t)`: may unfreeze, or send the compromise notice of,
    /// a freeze at `t` that excluded `excluded`
    pub fn eligible(
        &self,
        u: FactorId,
        excluded: FactorId,
        t: DateTime<Utc>,
    ) -> bool {
        self.effective(u)
            && u != excluded
            && self
                .factor(u)
                .is_some_and(|f| f.added_at <= t - unfreeze_age())
            && !self.recent_desc(u, t).contains(&excluded)
    }

    /// `authorised(F)` is empty: no open session it opened, pending change
    /// it requested, scheduled send it admitted or grant it consented
    pub fn authorised_is_empty(&self, id: FactorId) -> bool {
        let sessions = self.sessions.values().any(|s| {
            s.open
                && matches!((s.opener, id), (SessionOpener::Passkey(p), FactorId::Passkey(q)) if p == q)
        });
        let changes = self
            .changes
            .values()
            .any(|c| c.state == ChangeState::Pending && c.requested_by == id);
        let sends = self.sends.values().any(|s| {
            s.state == SendState::Scheduled
                && s.admitted_by == AdmittedBy::Factor(id)
        });
        let grants = self.grants.values().any(|by| *by == id);
        !(sessions || changes || sends || grants)
    }

    fn passkeys(&self) -> impl Iterator<Item = (&FactorId, &Factor)> {
        self.factors.iter().filter(|(_, f)| f.kind.is_passkey())
    }

    /// `devices(A)`: effective non-synced passkeys, plus one for any
    /// effective synced ones. PIV keys never count
    pub fn devices(&self) -> u32 {
        let mut synced = false;
        let mut n = 0;
        for (_, f) in self.passkeys().filter(|(_, f)| f.state.is_active()) {
            match f.kind {
                FactorKind::Passkey(PasskeyClass { synced: true, .. }) => {
                    synced = true
                }
                _ => n += 1,
            }
        }
        n + u32::from(synced)
    }

    /// `has_yk_tap(A)`: a live factor gives YubiKey taps (suspended and
    /// contested count until revoked)
    pub fn has_yk_tap(&self) -> bool {
        self.factors
            .values()
            .any(|f| f.state.is_live() && f.kind.gives_yubikey_tap())
    }

    /// `has_yk_passkey(A)`: an effective YubiKey passkey
    pub fn has_yk_passkey(&self) -> bool {
        self.factors
            .values()
            .any(|f| f.state.is_active() && f.kind.is_yubikey_passkey())
    }

    /// `domains_ok(A)`: the contact, if any, and the primary are on
    /// different tenants
    pub fn domains_ok(&self) -> bool {
        self.contact
            .as_ref()
            .is_none_or(|c| c.address.tenant != self.primary.tenant)
    }

    /// The contact, if verified: who notices and freeze links go to
    pub fn verified_contact(&self) -> Option<&EmergencyContact> {
        self.contact
            .as_ref()
            .filter(|c| c.state == ContactState::Verified)
    }

    /// `contact_ok(A)`
    pub fn contact_ok(&self) -> bool {
        self.verified_contact().is_some() && self.domains_ok()
    }

    /// `no_soft(A)`
    pub fn no_soft(&self) -> bool {
        !self
            .factors
            .values()
            .any(|f| f.state.is_active() && f.kind.is_software_key())
    }

    /// `min5(A)`
    pub fn min5(&self) -> bool {
        self.contact_ok() && self.devices() >= 2 && self.no_soft()
    }

    /// `min50(A)`
    pub fn min50(&self) -> bool {
        self.min5() && self.has_yk_passkey()
    }

    /// `restricted(A)`: each flag gates its own part of the minimum
    pub fn restricted(&self) -> bool {
        let above5 = self.count > 5;
        let above50 = self.count > 50;
        (above5 && self.flags.tier && !self.no_soft())
            || (above5
                && self.flags.minimum
                && !(self.contact_ok() && self.devices() >= 2))
            || (above50 && self.flags.yubikey && !self.has_yk_passkey())
    }

    /// `stranded(A)`: no effective passkey, and no password
    pub fn stranded(&self) -> bool {
        let has_passkey = self.passkeys().any(|(_, f)| f.state.is_active());
        let has_password =
            self.auth == AuthMode::PasswordOnly && !self.flags.cutover;
        !has_passkey && !has_password
    }

    /// Locked out: password-only after the cutover
    pub fn locked_out(&self) -> bool {
        self.auth == AuthMode::PasswordOnly && self.flags.cutover
    }

    pub fn frozen(&self) -> Option<&Frozen> {
        match &self.hold {
            Hold::Frozen(f) => Some(f),
            Hold::Open => None,
        }
    }

    pub fn banned(&self) -> bool {
        self.standing == AccountStanding::Banned
    }

    /// Normal mode: open, in good standing, not restricted
    pub fn normal(&self) -> bool {
        self.frozen().is_none() && !self.banned() && !self.restricted()
    }

    /// Decision 9's window: every recipient copy of every send that isn't
    /// cancelled
    pub fn window_messages(&self) -> Vec<WindowMessage> {
        self.sends
            .values()
            .filter(|s| s.state != SendState::Cancelled)
            .flat_map(|s| {
                s.recipients.iter().map(|recipient| WindowMessage {
                    recipient: *recipient,
                    deliver_at: s.deliver_at,
                    immediate: s.delivery == Delivery::Immediate,
                })
            })
            .collect()
    }

    /// The factor behind a session: the passkey that opened it
    pub fn session_factor(&self, id: OperatorSessionId) -> Option<FactorId> {
        match self.sessions.get(&id)?.opener {
            SessionOpener::Passkey(p) => Some(FactorId::Passkey(p)),
            SessionOpener::Password => None,
        }
    }

    /// The password still signs in
    pub fn password_usable(&self) -> bool {
        self.auth == AuthMode::PasswordOnly && !self.flags.cutover
    }
}
