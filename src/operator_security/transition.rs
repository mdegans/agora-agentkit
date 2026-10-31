//! The transition function: `(AccountState, Event, Authority, now)` →
//! `(AccountState, Vec<Effect>)` or a [`Refusal`] naming the failed guard
//! (agora `docs/design-operator-state-table.md`, §13). Effects are data;
//! nothing here sends, stores or logs

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use super::challenge::TapKind;
use super::model::*;
use super::tap::{
    Delivery, FactorKind, NAMED_REACH, PasskeyClass, SendRefusal, SendRequest,
    TapLevel, UNTAPPED_REACH, When, bulk_hold_floor, plan_send, reach,
};
use crate::enums::OperatorActionKind;
use crate::ids::{
    AgentId, OAuthClientId, OperatorActionId, OperatorCredentialId,
    OperatorEmailAddressId, OperatorEnrolmentId, OperatorFreezeLinkId,
    OperatorPasskeyId, OperatorPendingChangeId, OperatorRecoveryCaseId,
    OperatorSessionId,
};

/// The authority behind an event (§1, *Authority*)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Authority {
    /// A link in a mail to this address
    Email(OperatorEmailAddressId),
    /// A portal session
    Session(OperatorSessionId),
    /// A request signed by an API credential: `Soft`, `Unattended`, `Sudo`
    /// or the API's YubiKey tap, by the credential's kind
    Signed(OperatorCredentialId),
    /// A passkey's assertion bound to one action of `kind`
    PasskeyTap {
        passkey: OperatorPasskeyId,
        kind: TapKind,
    },
    /// A passkey's sign-in assertion (UV, bound to no action)
    Assertion(OperatorPasskeyId),
    /// Nobody yet: the device flow's start
    Anonymous,
    /// The operator's agent (reading its inbox)
    Agent,
    /// The manual-recovery call (CD6)
    Staff,
    /// The server: clocks, flags, bans
    Platform,
}

/// Something that can happen to an account
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    // §2, factors
    /// A grant's registration: `PasskeyTap(t, 0x12)`. The grant's secret, session
    /// and expiry are the server's, as a ceremony is
    PasskeyAdd {
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
    },
    /// A session's add, while the account has never had an agent
    PasskeyAddFree {
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
    },
    Suspend {
        target: FactorId,
    },
    Contest {
        factor: FactorId,
    },
    Deadline {
        factor: FactorId,
    },
    // §3, enrolment
    EnrolStart {
        enrolment: OperatorEnrolmentId,
        kind: FactorKind,
    },
    EnrolApprove {
        enrolment: OperatorEnrolmentId,
        credential: OperatorCredentialId,
    },
    EnrolRefuse {
        enrolment: OperatorEnrolmentId,
    },
    EnrolExpire {
        enrolment: OperatorEnrolmentId,
    },
    // §4.3, the account
    AgentAdd,
    AgentRestore,
    AgentDelete {
        agent: AgentId,
    },
    AgentGraceEnd,
    FlagOn(Flag),
    Ban,
    Unban,
    OAuthConsent {
        client: OAuthClientId,
        agent: AgentId,
    },
    ConsentRevoke {
        client: OAuthClientId,
        agent: AgentId,
    },
    LogWithdraw,
    // §5, pending changes
    ChangeRequest {
        change: OperatorPendingChangeId,
        setting: SettingChange,
    },
    ChangeCancel {
        change: OperatorPendingChangeId,
    },
    ChangeEffective {
        change: OperatorPendingChangeId,
    },
    // §7, the contact and freezing
    ContactSetFirst {
        address: EmailAddress,
    },
    AddressVerify {
        address: OperatorEmailAddressId,
    },
    ReArm,
    Freeze {
        link: OperatorFreezeLinkId,
    },
    Unfreeze,
    CompromiseNotice,
    // §6, sends
    SendAdmit {
        send: OperatorActionId,
        recipients: BTreeSet<AgentId>,
        when: When,
    },
    SendDeliver {
        send: OperatorActionId,
    },
    SendCancel {
        send: OperatorActionId,
    },
    /// Earlier, by a YubiKey tap (0x11)
    SendForward {
        send: OperatorActionId,
        deliver_at: DateTime<Utc>,
    },
    /// Later, by what cancelling takes, or a YubiKey tap (0x11) if one made it
    SendLater {
        send: OperatorActionId,
        deliver_at: DateTime<Utc>,
    },
    Retract {
        send: OperatorActionId,
    },
    Read {
        send: OperatorActionId,
    },
    // §9, recovery
    /// The operator asks for recovery, stranded
    RecoveryRequest,
    RecoveryDecide {
        case: OperatorRecoveryCaseId,
        outcome: RecoveryOutcome,
        /// In round 2, the losing side's factor, suspended
        losing: Option<FactorId>,
    },
    RecoveryCancel {
        case: OperatorRecoveryCaseId,
    },
    RecoveryEffective {
        case: OperatorRecoveryCaseId,
    },
    // §10, sessions
    SignIn {
        session: OperatorSessionId,
        passkey: OperatorPasskeyId,
    },
    SessionUse {
        session: OperatorSessionId,
    },
    SessionExpire {
        session: OperatorSessionId,
    },
    /// The session's own: its scheduled sends stand
    SignOut,
    /// Every other session, or the one it names
    SignOutOthers {
        session: Option<OperatorSessionId>,
    },
}

/// Where a notice goes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    /// The primary and the verified contact; with none, the primary alone
    Both,
    Primary,
    Contact,
    /// One address: the old or the new one in an address change
    Address(OperatorEmailAddressId),
}

/// What a notice is about
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeSubject {
    FactorAdded(FactorId),
    Suspended(FactorId),
    Contested(FactorId),
    Revoked(FactorId),
    Reinstated(FactorId),
    RecoveryOpened(OperatorRecoveryCaseId),
    RecoveryDecided(OperatorRecoveryCaseId),
    RecoveryCounterClaim(OperatorRecoveryCaseId),
    ChangeRequested(OperatorPendingChangeId),
    ChangeEffective(OperatorPendingChangeId),
    ChangeCancelled(OperatorPendingChangeId),
    ContactSet(OperatorEmailAddressId),
    ContactVerified(OperatorEmailAddressId),
    /// Held, reach above 5
    SendHeld(OperatorActionId),
    /// Now, immediate reach above 5
    SendNow(OperatorActionId),
    /// Any other send that took a tap (now, reach 6–500, immediate reach
    /// ≤ 5)
    SendTapped(OperatorActionId),
    /// Moved, and announced again with its new time
    SendMoved(OperatorActionId),
    /// Cancelled when its authority ended or its recheck failed
    SendCancelled(OperatorActionId),
    SessionEnded(OperatorSessionId),
    OAuthConsent(AgentId),
    ConsentRevoked(AgentId),
    GrantRevoked(AgentId),
    AgentDeleted(AgentId),
    NewDeviceSignIn(OperatorPasskeyId),
    LogWithdrawn,
    Frozen,
    Unfrozen,
    ReArmed,
    CompromiseNoticeSent,
}

/// One notice; batching (decision 9) changes its timing, not its content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Notice {
    pub to: Audience,
    pub subject: NoticeSubject,
    /// On the contact's copy: a freeze link excluding this factor
    pub freeze_link: Option<(OperatorFreezeLinkId, FactorId)>,
    /// On the primary's copy of a held send's notice
    pub cancel_link: Option<OperatorActionId>,
    /// Above 500: the contact also gets the count and the first names
    pub names: bool,
}

/// An `operator_actions` entry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogEntry {
    pub id: OperatorActionId,
    pub kind: OperatorActionKind,
    /// The factor that tapped or signed, if any
    pub by: Option<FactorId>,
    /// The factor it concerns, for per-target entries
    pub target: Option<FactorId>,
}

/// What a transition asks the server to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notice(Notice),
    Log(LogEntry),
    /// Mail a verification link to this address
    VerificationLink(OperatorEmailAddressId),
    /// Tell the Steward and `security@` (a freeze)
    SecurityAlert,
    /// Into the agents' inboxes
    Deliver(OperatorActionId),
    /// The server-templated compromise notice, to every agent at once
    DeliverCompromiseNotice,
    /// A setting now has its new value
    SettingChanged(SettingChange),
}

/// The new state and what to do about it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub state: AccountState,
    pub effects: Vec<Effect>,
}

/// What a transition must refuse, naming the guard
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("this authority can't cause this event")]
    WrongAuthority,
    #[error("the tap is bound to another kind of action")]
    WrongTapKind,
    #[error("the factor behind the request is not effective")]
    NotEffective,
    #[error("the session is not open")]
    SessionClosed,
    #[error("the account's mode refuses this ({0:?})")]
    Mode(ModeClass),
    #[error("software keys can only take away above 5 agents")]
    TierOne,
    #[error("the event doesn't apply in the current state")]
    WrongState,
    #[error("no such item")]
    Unknown,
    #[error("the id is in use")]
    Exists,
    #[error("too many suspensions in the last hour")]
    RateLimited,
    #[error("this takes a YubiKey tap")]
    NeedsYubikeyTap,
    #[error("this is your last passkey: add another one first")]
    LastPasskey,
    #[error("the account has its most passkeys")]
    TooManyPasskeys,
    #[error("the account has had an agent, so an add takes a tap")]
    HasHadAgent,
    #[error("a factor can't approve or cancel its own item")]
    SameFactor,
    #[error("too early")]
    TooEarly,
    #[error("too late")]
    TooLate,
    #[error("the account doesn't meet the minimum for {0} agents")]
    Minimum(u32),
    #[error(transparent)]
    Send(#[from] SendRefusal),
    #[error("the factor is not eligible to unfreeze")]
    NotEligible,
    #[error("the freeze link is void")]
    VoidLink,
    #[error("a change to this setting is already pending")]
    AlreadyPending,
    #[error("the contact and the primary must be on different domains")]
    SameDomain,
    #[error("the contact can't be removed above 5 agents")]
    ContactRequired,
    #[error("outside the setting's range")]
    OutOfRange,
    #[error("a sudo request is needed")]
    NeedsSudo,
    #[error("the account is banned")]
    Banned,
    #[error("a recovery case's second decision is final")]
    Final,
}

/// The rows of §4.2's mode table
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeClass {
    /// Reads, export, the log, the agents' channel, sessions
    Read,
    /// §1's taking-away list
    TakeAway,
    /// **Contest**, **RecoveryCancel**
    Dispute,
    Freeze,
    Unfreeze,
    ReArm,
    /// **SendAdmit**, reach ≤ 5
    SendSmall,
    /// **SendAdmit**, reach > 5
    SendLarge,
    CompromiseNotice,
    /// **ContactSetFirst**, **AddressVerify**
    ContactRestore,
    /// A **ChangeRequest** replacing a contact that fails `contact_ok`
    ContactReplace,
    /// **PasskeyAdd**, **PasskeyAddFree**
    PasskeyAdd,
    EnrolApprove,
    /// Any other **ChangeRequest**
    OtherChange,
    OAuthConsent,
    /// **AgentAdd**, **AgentRestore**
    AgentAdd,
    AgentDelete,
}

impl AccountState {
    /// §4.2: every mode the account is in allows `class`
    pub fn mode_allows(&self, class: ModeClass) -> bool {
        use ModeClass::*;
        let restricted = self.restricted();
        let frozen = self.frozen().is_some();
        let banned = self.banned();
        // (restricted, frozen, banned) columns
        let (r, f, b) = match class {
            Read | TakeAway | Dispute => (true, true, true),
            Freeze => (true, false, true),
            Unfreeze => (true, true, true),
            ReArm | ContactRestore | PasskeyAdd => (true, true, false),
            SendSmall | ContactReplace => (true, false, false),
            CompromiseNotice => (true, true, false),
            SendLarge | EnrolApprove | OtherChange | OAuthConsent
            | AgentAdd | AgentDelete => (false, false, false),
        };
        (!restricted || r) && (!frozen || f) && (!banned || b)
    }

    /// Apply `event`, caused by `by`, at `now`. `ids` names the log
    /// entries, freeze links and cases the transition creates
    pub fn apply(
        &self,
        event: &Event,
        by: Authority,
        now: DateTime<Utc>,
        ids: &mut impl IdSource,
    ) -> Result<Transition, Refusal> {
        let mut tx = Tx {
            s: self.clone(),
            effects: Vec::new(),
            now,
            ids,
        };
        tx.event(event, by)?;
        Ok(Transition {
            state: tx.s,
            effects: tx.effects,
        })
    }

    /// Replacing a passkey: no event of its own, but **PasskeyAdd** of `passkey`
    /// and **Suspend** of `old`, all or nothing, on one tap of `tapper` that binds
    /// the old row (0x04). `tapper` is never `old`
    pub fn replace_passkey(
        &self,
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
        old: OperatorPasskeyId,
        tapper: OperatorPasskeyId,
        now: DateTime<Utc>,
        ids: &mut impl IdSource,
    ) -> Result<Transition, Refusal> {
        guard(tapper != old, Refusal::SameFactor)?;
        let by = Authority::PasskeyTap {
            passkey: tapper,
            kind: TapKind::Suspend,
        };
        let mut tx = Tx {
            s: self.clone(),
            effects: Vec::new(),
            now,
            ids,
        };
        let t = tx.tap(by, TapKind::Suspend)?;
        tx.passkey_add(t, passkey, class)?;
        tx.event(
            &Event::Suspend {
                target: FactorId::Passkey(old),
            },
            by,
        )?;
        Ok(Transition {
            state: tx.s,
            effects: tx.effects,
        })
    }

    /// The level of `by`'s authority (§1's last column)
    pub fn level(&self, by: Authority) -> TapLevel {
        match by {
            Authority::Signed(c) => self
                .factor(FactorId::Credential(c))
                .map_or(TapLevel::None, |f| f.kind.tap_level()),
            Authority::PasskeyTap { passkey, .. } => self
                .factor(FactorId::Passkey(passkey))
                .map_or(TapLevel::None, |f| f.kind.tap_level()),
            _ => TapLevel::None,
        }
    }

    /// The factor behind `by`: the tapping passkey, the signing key, a
    /// session's passkey
    pub fn factor_behind(&self, by: Authority) -> Option<FactorId> {
        match by {
            Authority::Signed(c) => Some(FactorId::Credential(c)),
            Authority::PasskeyTap { passkey, .. }
            | Authority::Assertion(passkey) => Some(FactorId::Passkey(passkey)),
            Authority::Session(s) => self.session_factor(s),
            _ => None,
        }
    }
}

/// Names for what a transition creates
pub trait IdSource {
    fn action(&mut self) -> OperatorActionId;
    fn case(&mut self) -> OperatorRecoveryCaseId;
    fn freeze_link(&mut self) -> OperatorFreezeLinkId;
}

/// Random ids, as the server makes them
#[derive(Debug, Clone, Copy, Default)]
pub struct FreshIds;

impl IdSource for FreshIds {
    fn action(&mut self) -> OperatorActionId {
        OperatorActionId::new()
    }
    fn case(&mut self) -> OperatorRecoveryCaseId {
        OperatorRecoveryCaseId::new()
    }
    fn freeze_link(&mut self) -> OperatorFreezeLinkId {
        OperatorFreezeLinkId::new()
    }
}

struct Tx<'a, I: IdSource> {
    s: AccountState,
    effects: Vec<Effect>,
    now: DateTime<Utc>,
    ids: &'a mut I,
}

type R = Result<(), Refusal>;

fn guard(ok: bool, refusal: Refusal) -> R {
    if ok { Ok(()) } else { Err(refusal) }
}

/// An item a guard just found has gone: believed impossible, so said loudly
fn gone(item: &'static str) -> Refusal {
    tracing::error!(item, "operator security: an item vanished mid-transition");
    Refusal::Unknown
}

impl<I: IdSource> Tx<'_, I> {
    // -- effects ----------------------------------------------------------

    fn log(
        &mut self,
        kind: OperatorActionKind,
        by: Option<FactorId>,
        target: Option<FactorId>,
    ) -> OperatorActionId {
        let id = self.ids.action();
        self.effects.push(Effect::Log(LogEntry {
            id,
            kind,
            by,
            target,
        }));
        id
    }

    /// A notice; with `flag` and a verified contact on its audience, a
    /// freeze link excluding `flag` goes on the contact's copy
    fn notify(
        &mut self,
        to: Audience,
        subject: NoticeSubject,
        flag: Option<FactorId>,
    ) {
        let to_contact = matches!(to, Audience::Both | Audience::Contact);
        let freeze_link = match (flag, self.s.verified_contact(), to_contact) {
            (Some(factor), Some(contact), true) => {
                let id = self.ids.freeze_link();
                let link = FreezeLink {
                    address: contact.address.id,
                    factor,
                    sent_at: self.now,
                    used: false,
                    void: false,
                };
                self.s.freeze_links.insert(id, link);
                Some((id, factor))
            }
            _ => None,
        };
        self.effects.push(Effect::Notice(Notice {
            to,
            subject,
            freeze_link,
            cancel_link: None,
            names: false,
        }));
    }

    // -- guards -----------------------------------------------------------

    /// The factor behind `by` is effective, and a session is open (I-4,
    /// I-9, I-37)
    fn authority_live(&self, by: Authority) -> R {
        if let Authority::Session(s) = by {
            let session = self.s.sessions.get(&s).ok_or(Refusal::Unknown)?;
            guard(session.open, Refusal::SessionClosed)?;
        }
        match self.s.factor_behind(by) {
            Some(f) => guard(self.s.effective(f), Refusal::NotEffective),
            None => Ok(()),
        }
    }

    /// The tier-1 guard: a `software_key` above 5 agents only takes away
    fn tier_one(&self, by: Authority, class: ModeClass) -> R {
        let soft = matches!(by, Authority::Signed(c)
            if self.s.factor(FactorId::Credential(c)).is_some_and(|f| f.kind.is_software_key()));
        let enforced = self.s.count > 5 && self.s.flags.tier;
        guard(
            // A contest is a suspended factor's only request: it restores
            // nothing, and refusing it would leave the factor no way to dispute
            !(soft
                && enforced
                && class != ModeClass::TakeAway
                && class != ModeClass::Dispute),
            Refusal::TierOne,
        )
    }

    fn mode(&self, class: ModeClass) -> R {
        guard(self.s.mode_allows(class), Refusal::Mode(class))
    }

    /// A passkey tap of `kind`, by an effective passkey
    fn tap(&self, by: Authority, kind: TapKind) -> Result<FactorId, Refusal> {
        match by {
            Authority::PasskeyTap { passkey, kind: k } => {
                guard(k == kind, Refusal::WrongTapKind)?;
                let id = FactorId::Passkey(passkey);
                guard(self.s.effective(id), Refusal::NotEffective)?;
                Ok(id)
            }
            _ => Err(Refusal::WrongAuthority),
        }
    }

    /// A YubiKey tap of `kind`: a YubiKey passkey's, or a signature by a key that
    /// gives one
    fn yubikey_tap(
        &self,
        by: Authority,
        kind: TapKind,
    ) -> Result<FactorId, Refusal> {
        let id = match by {
            Authority::PasskeyTap { .. } => self.tap(by, kind)?,
            Authority::Signed(k) => {
                let k = FactorId::Credential(k);
                guard(self.s.effective(k), Refusal::NotEffective)?;
                k
            }
            _ => return Err(Refusal::WrongAuthority),
        };
        guard(
            self.s.level(by) == TapLevel::YubikeyTap,
            Refusal::NeedsYubikeyTap,
        )?;
        Ok(id)
    }

    /// A session of the account
    fn session(&self, by: Authority) -> Result<OperatorSessionId, Refusal> {
        match by {
            Authority::Session(s) => {
                self.authority_live(by)?;
                Ok(s)
            }
            _ => Err(Refusal::WrongAuthority),
        }
    }

    fn platform(by: Authority) -> R {
        guard(by == Authority::Platform, Refusal::WrongAuthority)
    }

    // -- shared transitions -----------------------------------------------

    /// Cancel a scheduled send. `by` is the factor whose act caused it (a
    /// suspender, a signing-out session's passkey), `None` for an automatic
    /// ending (a freeze, a ban, a failed recheck); the admitting factor is
    /// the entry's target
    fn cancel_send(
        &mut self,
        send: OperatorActionId,
        notice: bool,
        by: Option<FactorId>,
    ) {
        let Some(s) = self.s.sends.get_mut(&send) else {
            return;
        };
        if s.state != SendState::Scheduled {
            return;
        }
        s.state = SendState::Cancelled;
        let target = match s.admitted_by {
            AdmittedBy::Factor(f) => Some(f),
            AdmittedBy::Session(_) => None,
        };
        self.log(OperatorActionKind::DmCancel, by, target);
        if notice {
            self.notify(
                Audience::Both,
                NoticeSubject::SendCancelled(send),
                None,
            );
        }
    }

    /// Cancel a pending change; `by` as for [`Self::cancel_send`], the
    /// requester the target
    fn cancel_change(
        &mut self,
        change: OperatorPendingChangeId,
        by: Option<FactorId>,
    ) {
        let Some(c) = self.s.changes.get_mut(&change) else {
            return;
        };
        if c.state != ChangeState::Pending {
            return;
        }
        c.state = ChangeState::Cancelled;
        let target = c.requested_by;
        self.log(OperatorActionKind::ConfigChangeCancelled, by, Some(target));
        self.notify(
            Audience::Both,
            NoticeSubject::ChangeCancelled(change),
            None,
        );
    }

    /// End a session; unless `sends_stand` (expiry, signing itself out), the
    /// sends it admitted are cancelled
    fn end_session(
        &mut self,
        session: OperatorSessionId,
        sends_stand: bool,
        by: Option<FactorId>,
    ) {
        let Some(s) = self.s.sessions.get_mut(&session) else {
            return;
        };
        if !s.open {
            return;
        }
        s.open = false;
        if sends_stand {
            return;
        }
        let sends: Vec<_> = self
            .s
            .sends
            .iter()
            .filter(|(_, m)| m.admitted_by == AdmittedBy::Session(session))
            .map(|(id, _)| *id)
            .collect();
        for send in sends {
            self.cancel_send(send, true, by);
        }
    }

    /// `EndAuthorised(F)`: whenever F becomes suspended, whatever the cause.
    /// `by` is the suspender, if any; `f` is each entry's target
    fn end_authorised(&mut self, f: FactorId, by: Option<FactorId>) {
        let sessions: Vec<_> = self
            .s
            .sessions
            .iter()
            .filter(|(_, s)| s.open && FactorId::Passkey(s.passkey) == f)
            .map(|(id, _)| *id)
            .collect();
        for session in sessions {
            self.end_session(session, false, by);
            self.log(OperatorActionKind::SessionRevoke, by, Some(f));
            self.notify(
                Audience::Both,
                NoticeSubject::SessionEnded(session),
                None,
            );
        }
        let changes: Vec<_> = self
            .s
            .changes
            .iter()
            .filter(|(_, c)| {
                c.state == ChangeState::Pending && c.requested_by == f
            })
            .map(|(id, _)| *id)
            .collect();
        for change in changes {
            self.cancel_change(change, by);
        }
        let sends: Vec<_> = self
            .s
            .sends
            .iter()
            .filter(|(_, m)| m.admitted_by == AdmittedBy::Factor(f))
            .map(|(id, _)| *id)
            .collect();
        for send in sends {
            self.cancel_send(send, true, by);
        }
        let grants: Vec<_> = self
            .s
            .grants
            .iter()
            .filter(|(_, g)| **g == f)
            .map(|(k, _)| k.clone())
            .collect();
        for grant in grants {
            self.s.grants.remove(&grant);
            // SPEC: CD1 says each ending has "its notice and log entry", but
            // the spec's action kinds have none for a revoked grant: notice
            // only
            self.notify(
                Audience::Both,
                NoticeSubject::GrantRevoked(grant.1),
                None,
            );
        }
    }

    /// Suspend `f` (if it is `Active`) with `cause`, ending what it
    /// authorised and logging it per target
    fn suspend_one(
        &mut self,
        f: FactorId,
        cause: SuspensionCause,
        deadline: DateTime<Utc>,
        follows_root: Option<FactorId>,
        flag: Option<FactorId>,
    ) {
        if !self.s.effective(f) {
            return;
        }
        let entry =
            self.log(OperatorActionKind::CredentialSuspend, flag, Some(f));
        let Some(factor) = self.s.factors.get_mut(&f) else {
            gone("factor");
            return;
        };
        factor.state = CredentialState::Suspended(Suspension {
            cause,
            entry,
            at: self.now,
            deadline,
            follows_root,
        });
        self.end_authorised(f, flag);
        self.notify(Audience::Both, NoticeSubject::Suspended(f), flag);
    }

    /// Cascade from `root` (already suspended), skipping `suspender`
    fn cascade_from(
        &mut self,
        root: FactorId,
        suspender: Option<FactorId>,
        deadline: DateTime<Utc>,
    ) {
        let targets = self.s.cascade(root, suspender, self.now);
        for g in targets.into_iter().filter(|g| *g != root) {
            self.suspend_one(
                g,
                SuspensionCause::Cascade {
                    root,
                    by: suspender,
                },
                deadline,
                Some(root),
                suspender,
            );
        }
    }

    /// The followers of `root` that still share its fate
    fn followers(&self, root: FactorId) -> Vec<FactorId> {
        self.s
            .factors
            .iter()
            .filter(|(_, f)| {
                matches!(f.state, CredentialState::Suspended(s) if s.follows_root == Some(root))
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// A new factor, `Active`
    fn add_factor(
        &mut self,
        id: FactorId,
        kind: FactorKind,
        parent: Option<FactorId>,
        sends_now_from: Option<DateTime<Utc>>,
    ) -> R {
        guard(!self.s.factors.contains_key(&id), Refusal::Exists)?;
        self.s.factors.insert(
            id,
            Factor {
                kind,
                state: CredentialState::Active,
                parent,
                added_at: self.now,
                signed_in: false,
                sends_now_from,
            },
        );
        Ok(())
    }

    /// `sends_now_from` of a factor of kind `new` that `t` approved
    fn sends_now_from(
        &self,
        new: FactorKind,
        t: FactorId,
    ) -> Result<Option<DateTime<Utc>>, Refusal> {
        let tap = self.s.factor(t).ok_or_else(|| gone("tapper"))?;
        Ok(sends_now_from_for_add(
            new,
            tap.kind,
            tap.sends_now_from,
            self.now,
        ))
    }

    /// **PasskeyAdd**, with its tap already checked
    fn passkey_add(
        &mut self,
        t: FactorId,
        passkey: OperatorPasskeyId,
        class: PasskeyClass,
    ) -> R {
        self.mode(ModeClass::PasskeyAdd)?;
        guard(
            self.s.counted_passkeys() < MAX_PASSKEYS,
            Refusal::TooManyPasskeys,
        )?;
        let id = FactorId::Passkey(passkey);
        let kind = FactorKind::Passkey(class);
        let from = self.sends_now_from(kind, t)?;
        self.add_factor(id, kind, Some(t), from)?;
        self.log(OperatorActionKind::CredentialAdd, Some(t), Some(id));
        self.notify(Audience::Both, NoticeSubject::FactorAdded(id), Some(t));
        Ok(())
    }

    fn open_case(&mut self, subject: CaseSubject) -> OperatorRecoveryCaseId {
        let case = self.ids.case();
        let effective_at_open = self
            .s
            .factors
            .iter()
            .filter(|(_, f)| f.state.is_active())
            .map(|(id, _)| *id)
            .collect();
        self.s.cases.insert(
            case,
            RecoveryCase {
                subject,
                opened_at: self.now,
                effective_at_open,
                state: CaseState::Open { round: 1 },
            },
        );
        self.notify(Audience::Both, NoticeSubject::RecoveryOpened(case), None);
        case
    }

    fn freeze_cancels_everything(&mut self) {
        let changes: Vec<_> = self.s.changes.keys().copied().collect();
        for change in changes {
            self.cancel_change(change, None);
        }
        let sends: Vec<_> = self.s.sends.keys().copied().collect();
        for send in sends {
            self.cancel_send(send, false, None);
        }
    }

    fn apply_setting(&mut self, change: &SettingChange) {
        match change {
            SettingChange::DmHold(d) => self.s.settings.dm_hold = *d,
            SettingChange::BulkHold(d) => self.s.settings.bulk_hold = *d,
            SettingChange::AgentReplyDailyCap(n) => {
                self.s.settings.agent_reply_daily_cap = *n
            }
            SettingChange::EmergencyContact(new) => {
                self.s.contact = new.clone().map(|address| EmergencyContact {
                    address,
                    state: ContactState::Verified,
                    power: FreezePower::Armed,
                })
            }
            SettingChange::PrimaryEmail(address) => {
                self.s.primary = address.clone()
            }
            SettingChange::PublishLog => {
                self.s.settings.action_log_public = true
            }
        }
        self.effects.push(Effect::SettingChanged(change.clone()));
    }

    /// `domains_ok` after `change` takes effect
    fn domains_ok_after(&self, change: &SettingChange) -> bool {
        let mut after = self.s.clone();
        match change {
            SettingChange::EmergencyContact(new) => {
                after.contact = new.clone().map(|address| EmergencyContact {
                    address,
                    state: ContactState::Verified,
                    power: FreezePower::Armed,
                })
            }
            SettingChange::PrimaryEmail(a) => after.primary = a.clone(),
            _ => {}
        }
        after.domains_ok()
    }

    // -- events -----------------------------------------------------------

    fn event(&mut self, event: &Event, by: Authority) -> R {
        use Event::*;
        match event {
            PasskeyAdd { passkey, class } => {
                let t = self.tap(by, TapKind::PasskeyAdd)?;
                self.passkey_add(t, *passkey, *class)
            }
            PasskeyAddFree { passkey, class } => {
                self.session(by)?;
                self.mode(ModeClass::PasskeyAdd)?;
                guard(self.s.no_agents(), Refusal::HasHadAgent)?;
                guard(
                    self.s.counted_passkeys() < MAX_PASSKEYS,
                    Refusal::TooManyPasskeys,
                )?;
                let id = FactorId::Passkey(*passkey);
                self.add_factor(id, FactorKind::Passkey(*class), None, None)?;
                self.log(OperatorActionKind::CredentialAdd, None, Some(id));
                self.notify(
                    Audience::Both,
                    NoticeSubject::FactorAdded(id),
                    None,
                );
                Ok(())
            }
            Suspend { target } => {
                let s = self.tap(by, TapKind::Suspend)?;
                self.mode(ModeClass::TakeAway)?;
                let f = *self.s.factor(*target).ok_or(Refusal::Unknown)?;
                // A suspender is spared, so only suspending itself can leave
                // none (I-42)
                guard(
                    !(s == *target && self.s.is_last_passkey(s)),
                    Refusal::LastPasskey,
                )?;
                let hour_ago = self.now - chrono::Duration::hours(1);
                // Only the rate window is kept
                let spent = self.s.direct_suspensions.entry(s).or_default();
                spent.retain(|t| *t > hour_ago);
                let recent = spent.len();
                match f.state {
                    CredentialState::Active => {
                        guard(
                            (recent as u64) < u64::from(self.s.limits.suspend),
                            Refusal::RateLimited,
                        )?;
                        self.s
                            .direct_suspensions
                            .entry(s)
                            .or_default()
                            .push(self.now);
                        let deadline = self.now + suspension_deadline();
                        let cascade =
                            self.s.cascade(*target, Some(s), self.now);
                        self.suspend_one(
                            *target,
                            SuspensionCause::Direct { by: s },
                            deadline,
                            None,
                            Some(s),
                        );
                        // the set was computed before the root's suspension
                        for g in cascade.into_iter().filter(|g| g != target) {
                            self.suspend_one(
                                g,
                                SuspensionCause::Cascade {
                                    root: *target,
                                    by: Some(s),
                                },
                                deadline,
                                Some(*target),
                                Some(s),
                            );
                        }
                        Ok(())
                    }
                    CredentialState::Suspended(sus)
                        if matches!(
                            sus.cause,
                            SuspensionCause::Freeze { .. }
                        ) =>
                    {
                        guard(
                            (recent as u64) < u64::from(self.s.limits.suspend),
                            Refusal::RateLimited,
                        )?;
                        self.s
                            .direct_suspensions
                            .entry(s)
                            .or_default()
                            .push(self.now);
                        let factor = self
                            .s
                            .factors
                            .get_mut(target)
                            .ok_or_else(|| gone("target"))?;
                        factor.state = CredentialState::Suspended(Suspension {
                            cause: SuspensionCause::Direct { by: s },
                            ..sus
                        });
                        self.cascade_from(*target, Some(s), sus.deadline);
                        Ok(())
                    }
                    // Suspending a factor already suspended any other way
                    // changes nothing
                    CredentialState::Suspended(_) => Ok(()),
                    _ => Err(Refusal::WrongState),
                }
            }
            Contest { factor } => {
                match (by, factor) {
                    (
                        Authority::PasskeyTap { passkey, kind },
                        FactorId::Passkey(p),
                    ) if passkey == *p => {
                        guard(kind == TapKind::Contest, Refusal::WrongTapKind)?;
                    }
                    (Authority::Signed(c), FactorId::Credential(k))
                        if c == *k => {}
                    _ => return Err(Refusal::WrongAuthority),
                }
                self.tier_one(by, ModeClass::Dispute)?;
                self.mode(ModeClass::Dispute)?;
                let f = *self.s.factor(*factor).ok_or(Refusal::Unknown)?;
                let CredentialState::Suspended(sus) = f.state else {
                    return Err(Refusal::WrongState);
                };
                // The losing side of a decided case: round 2 is final
                guard(
                    !matches!(sus.cause, SuspensionCause::Recovery { .. }),
                    Refusal::Final,
                )?;
                // At the deadline itself a contest wins
                guard(self.now <= sus.deadline, Refusal::TooLate)?;
                let case = self.open_case(CaseSubject::Contest(*factor));
                self.s
                    .factors
                    .get_mut(factor)
                    .ok_or_else(|| gone("factor"))?
                    .state = CredentialState::Contested {
                    suspension: Suspension {
                        follows_root: None,
                        ..sus
                    },
                    case,
                };
                self.log(
                    OperatorActionKind::CredentialContest,
                    Some(*factor),
                    Some(*factor),
                );
                self.notify(
                    Audience::Both,
                    NoticeSubject::Contested(*factor),
                    None,
                );
                Ok(())
            }
            Deadline { factor } => {
                Self::platform(by)?;
                let f = *self.s.factor(*factor).ok_or(Refusal::Unknown)?;
                let CredentialState::Suspended(sus) = f.state else {
                    return Err(Refusal::WrongState);
                };
                // A contest at the deadline's instant wins, so the revocation
                // comes strictly after it
                guard(self.now > sus.deadline, Refusal::TooEarly)?;
                if let Some(root) = sus.follows_root {
                    let root_contested = self.s.factor(root).is_some_and(|r| {
                        matches!(r.state, CredentialState::Contested { .. })
                    });
                    guard(!root_contested, Refusal::WrongState)?;
                }
                self.s
                    .factors
                    .get_mut(factor)
                    .ok_or_else(|| gone("factor"))?
                    .state = CredentialState::Revoked;
                self.log(
                    OperatorActionKind::CredentialRevoke,
                    None,
                    Some(*factor),
                );
                self.notify(
                    Audience::Both,
                    NoticeSubject::Revoked(*factor),
                    None,
                );
                Ok(())
            }
            EnrolStart { enrolment, kind } => {
                guard(by == Authority::Anonymous, Refusal::WrongAuthority)?;
                guard(!kind.is_passkey(), Refusal::WrongState)?;
                guard(
                    !self.s.enrolments.contains_key(enrolment),
                    Refusal::Exists,
                )?;
                self.s.enrolments.insert(
                    *enrolment,
                    Enrolment {
                        kind: *kind,
                        state: EnrolmentState::Started,
                    },
                );
                Ok(())
            }
            EnrolApprove {
                enrolment,
                credential,
            } => {
                let t = self.tap(by, TapKind::EnrolmentApproval)?;
                self.mode(ModeClass::EnrolApprove)?;
                let e = *self
                    .s
                    .enrolments
                    .get(enrolment)
                    .ok_or(Refusal::Unknown)?;
                guard(e.state == EnrolmentState::Started, Refusal::WrongState)?;
                guard(
                    !(e.kind.is_software_key()
                        && self.s.count > 5
                        && self.s.flags.tier),
                    Refusal::TierOne,
                )?;
                let id = FactorId::Credential(*credential);
                let from = self.sends_now_from(e.kind, t)?;
                self.add_factor(id, e.kind, Some(t), from)?;
                self.s
                    .enrolments
                    .get_mut(enrolment)
                    .ok_or_else(|| gone("enrolment"))?
                    .state = EnrolmentState::Approved(*credential);
                self.log(OperatorActionKind::CredentialAdd, Some(t), Some(id));
                self.notify(
                    Audience::Both,
                    NoticeSubject::FactorAdded(id),
                    Some(t),
                );
                Ok(())
            }
            EnrolRefuse { enrolment } => {
                // SPEC: §3 "the Session that entered the code"; the model
                // doesn't track code entry, so any session of the account
                self.session(by)?;
                self.mode(ModeClass::TakeAway)?;
                let e = self
                    .s
                    .enrolments
                    .get_mut(enrolment)
                    .ok_or(Refusal::Unknown)?;
                guard(e.state == EnrolmentState::Started, Refusal::WrongState)?;
                e.state = EnrolmentState::Refused;
                Ok(())
            }
            EnrolExpire { enrolment } => {
                Self::platform(by)?;
                let e = self
                    .s
                    .enrolments
                    .get_mut(enrolment)
                    .ok_or(Refusal::Unknown)?;
                guard(e.state == EnrolmentState::Started, Refusal::WrongState)?;
                e.state = EnrolmentState::Expired;
                Ok(())
            }
            AgentAdd => {
                self.session(by)?;
                self.mode(ModeClass::AgentAdd)?;
                let count = self.s.count + 1;
                self.minimum_for(count)?;
                self.s.count = count;
                self.s.first_agent_at.get_or_insert(self.now);
                Ok(())
            }
            AgentRestore => {
                self.session(by)?;
                self.mode(ModeClass::AgentAdd)?;
                guard(self.s.departed > 0, Refusal::WrongState)?;
                self.minimum_for(self.s.count)?;
                self.s.departed -= 1;
                Ok(())
            }
            AgentDelete { agent } => {
                let t = self.tap(by, TapKind::AgentDeletion)?;
                self.mode(ModeClass::AgentDelete)?;
                guard(self.s.count > self.s.departed, Refusal::WrongState)?;
                self.s.departed += 1;
                self.log(OperatorActionKind::AgentDelete, Some(t), None);
                self.notify(
                    Audience::Both,
                    NoticeSubject::AgentDeleted(*agent),
                    Some(t),
                );
                Ok(())
            }
            AgentGraceEnd => {
                Self::platform(by)?;
                guard(self.s.departed > 0, Refusal::WrongState)?;
                self.s.departed -= 1;
                self.s.count -= 1;
                Ok(())
            }
            FlagOn(flag) => {
                Self::platform(by)?;
                match flag {
                    Flag::Tier => self.s.flags.tier = true,
                    Flag::Minimum => self.s.flags.minimum = true,
                    Flag::Yubikey => self.s.flags.yubikey = true,
                }
                Ok(())
            }
            Ban => {
                Self::platform(by)?;
                guard(!self.s.banned(), Refusal::WrongState)?;
                self.s.standing = AccountStanding::Banned;
                let changes: Vec<_> = self.s.changes.keys().copied().collect();
                for change in changes {
                    self.cancel_change(change, None);
                }
                let sends: Vec<_> = self.s.sends.keys().copied().collect();
                for send in sends {
                    self.cancel_send(send, false, None);
                }
                Ok(())
            }
            Unban => {
                Self::platform(by)?;
                guard(self.s.banned(), Refusal::WrongState)?;
                self.s.standing = AccountStanding::Good;
                Ok(())
            }
            OAuthConsent { client, agent } => {
                let t = self.tap(by, TapKind::OAuthConsent)?;
                self.mode(ModeClass::OAuthConsent)?;
                self.s.grants.insert((client.clone(), *agent), t);
                self.log(OperatorActionKind::OauthConsent, Some(t), None);
                self.notify(
                    Audience::Both,
                    NoticeSubject::OAuthConsent(*agent),
                    Some(t),
                );
                Ok(())
            }
            ConsentRevoke { client, agent } => {
                self.session(by)?;
                self.mode(ModeClass::TakeAway)?;
                self.s
                    .grants
                    .remove(&(client.clone(), *agent))
                    .ok_or(Refusal::Unknown)?;
                self.notify(
                    Audience::Both,
                    NoticeSubject::ConsentRevoked(*agent),
                    None,
                );
                Ok(())
            }
            LogWithdraw => {
                let t = self.tap(by, TapKind::LogPrivate)?;
                self.mode(ModeClass::TakeAway)?;
                self.s.settings.action_log_public = false;
                self.log(OperatorActionKind::LogVisibility, Some(t), None);
                self.notify(Audience::Both, NoticeSubject::LogWithdrawn, None);
                Ok(())
            }
            ChangeRequest { change, setting } => {
                self.change_request(*change, setting, by)
            }
            ChangeCancel { change } => {
                let c = self.tap(by, TapKind::CancelPending)?;
                self.mode(ModeClass::TakeAway)?;
                let pending =
                    self.s.changes.get(change).ok_or(Refusal::Unknown)?;
                guard(
                    pending.state == ChangeState::Pending,
                    Refusal::WrongState,
                )?;
                guard(pending.requested_by != c, Refusal::SameFactor)?;
                let pending = self
                    .s
                    .changes
                    .get_mut(change)
                    .ok_or_else(|| gone("change"))?;
                pending.state = ChangeState::Cancelled;
                let requester = pending.requested_by;
                self.log(
                    OperatorActionKind::ConfigChangeCancelled,
                    Some(c),
                    Some(requester),
                );
                self.notify(
                    Audience::Both,
                    NoticeSubject::ChangeCancelled(*change),
                    None,
                );
                Ok(())
            }
            ChangeEffective { change } => {
                Self::platform(by)?;
                let pending =
                    self.s.changes.get(change).ok_or(Refusal::Unknown)?.clone();
                guard(
                    pending.state == ChangeState::Pending,
                    Refusal::WrongState,
                )?;
                guard(
                    self.now >= pending.at + change_wait(),
                    Refusal::TooEarly,
                )?;
                let r = pending.requested_by;
                let removal =
                    pending.change == SettingChange::EmergencyContact(None);
                let ok = self.s.effective(r)
                    && (pending.change.new_address().is_none()
                        || pending.address_verified)
                    && self.domains_ok_after(&pending.change)
                    && !(removal && self.s.count > 5);
                if !ok {
                    self.s
                        .changes
                        .get_mut(change)
                        .ok_or_else(|| gone("change"))?
                        .state = ChangeState::Cancelled;
                    self.log(
                        OperatorActionKind::ConfigChangeCancelled,
                        None,
                        Some(r),
                    );
                    self.notify(
                        Audience::Both,
                        NoticeSubject::ChangeCancelled(*change),
                        None,
                    );
                    return Ok(());
                }
                let old_contact = self.s.contact.as_ref().map(|c| c.address.id);
                self.s
                    .changes
                    .get_mut(change)
                    .ok_or_else(|| gone("change"))?
                    .state = ChangeState::Effective;
                self.apply_setting(&pending.change);
                self.log(
                    OperatorActionKind::ConfigChangeEffective,
                    Some(r),
                    None,
                );
                self.notify(
                    Audience::Both,
                    NoticeSubject::ChangeEffective(*change),
                    Some(r),
                );
                if let SettingChange::EmergencyContact(new) = &pending.change {
                    // Every unused freeze link sent to the old address is
                    // void (the address check at use is the second guard)
                    for link in self.s.freeze_links.values_mut() {
                        if Some(link.address) == old_contact && !link.used {
                            link.void = true;
                        }
                    }
                    if let Some(old) = old_contact {
                        self.notify(
                            Audience::Address(old),
                            NoticeSubject::ChangeEffective(*change),
                            None,
                        );
                    }
                    if let Some(new) = new {
                        self.notify(
                            Audience::Address(new.id),
                            NoticeSubject::ChangeEffective(*change),
                            None,
                        );
                    }
                }
                Ok(())
            }
            ContactSetFirst { address } => {
                let t = match by {
                    Authority::PasskeyTap { .. } => {
                        self.tap(by, TapKind::FirstContact)?
                    }
                    Authority::Signed(k) => {
                        let k = FactorId::Credential(k);
                        guard(self.s.effective(k), Refusal::NotEffective)?;
                        guard(
                            self.s
                                .factor(k)
                                .is_some_and(|f| f.kind.gives_sudo()),
                            Refusal::NeedsSudo,
                        )?;
                        self.tier_one(by, ModeClass::ContactRestore)?;
                        k
                    }
                    _ => return Err(Refusal::WrongAuthority),
                };
                self.mode(ModeClass::ContactRestore)?;
                guard(
                    self.s
                        .contact
                        .as_ref()
                        .is_none_or(|c| c.state == ContactState::Unverified),
                    Refusal::WrongState,
                )?;
                guard(
                    address.tenant != self.s.primary.tenant,
                    Refusal::SameDomain,
                )?;
                self.s.contact = Some(EmergencyContact {
                    address: address.clone(),
                    state: ContactState::Unverified,
                    power: FreezePower::Armed,
                });
                self.effects.push(Effect::VerificationLink(address.id));
                self.notify(
                    Audience::Primary,
                    NoticeSubject::ContactSet(address.id),
                    None,
                );
                self.log(
                    OperatorActionKind::ConfigChangeEffective,
                    Some(t),
                    None,
                );
                Ok(())
            }
            AddressVerify { address } => {
                guard(
                    by == Authority::Email(*address),
                    Refusal::WrongAuthority,
                )?;
                self.mode(ModeClass::ContactRestore)?;
                if let Some(c) = self.s.contact.as_mut()
                    && c.address.id == *address
                    && c.state == ContactState::Unverified
                {
                    c.state = ContactState::Verified;
                    self.notify(
                        Audience::Both,
                        NoticeSubject::ContactVerified(*address),
                        None,
                    );
                    return Ok(());
                }
                let pending = self.s.changes.values_mut().find(|c| {
                    c.state == ChangeState::Pending
                        && c.change
                            .new_address()
                            .is_some_and(|a| a.id == *address)
                        && !c.address_verified
                });
                let pending = pending.ok_or(Refusal::Unknown)?;
                pending.address_verified = true;
                Ok(())
            }
            ReArm => {
                let t = self.tap(by, TapKind::ReArm)?;
                self.mode(ModeClass::ReArm)?;
                let contact =
                    self.s.contact.as_mut().ok_or(Refusal::WrongState)?;
                guard(
                    contact.power == FreezePower::Disarmed,
                    Refusal::WrongState,
                )?;
                contact.power = FreezePower::Armed;
                self.notify(Audience::Both, NoticeSubject::ReArmed, None);
                self.log(
                    OperatorActionKind::ConfigChangeEffective,
                    Some(t),
                    None,
                );
                Ok(())
            }
            Freeze { link } => self.freeze(*link, by),
            Unfreeze => {
                let u = self.tap(by, TapKind::Unfreeze)?;
                self.mode(ModeClass::Unfreeze)?;
                let frozen = *self.s.frozen().ok_or(Refusal::WrongState)?;
                guard(
                    self.s.eligible(u, frozen.excluded, frozen.at),
                    Refusal::NotEligible,
                )?;
                self.s.hold = Hold::Open;
                if let Some(c) = self.s.contact.as_mut()
                    && c.address.id == frozen.address
                {
                    c.power = FreezePower::Disarmed;
                }
                self.notify(Audience::Both, NoticeSubject::Unfrozen, None);
                self.log(OperatorActionKind::Unfreeze, Some(u), None);
                Ok(())
            }
            CompromiseNotice => {
                let t = self.tap(by, TapKind::CompromiseNotice)?;
                self.mode(ModeClass::CompromiseNotice)?;
                let frozen = *self.s.frozen().ok_or(Refusal::WrongState)?;
                guard(!frozen.notice_sent, Refusal::WrongState)?;
                guard(
                    self.s.eligible(t, frozen.excluded, frozen.at),
                    Refusal::NotEligible,
                )?;
                self.s.hold = Hold::Frozen(Frozen {
                    notice_sent: true,
                    ..frozen
                });
                self.effects.push(Effect::DeliverCompromiseNotice);
                self.notify(
                    Audience::Both,
                    NoticeSubject::CompromiseNoticeSent,
                    None,
                );
                self.log(OperatorActionKind::DmSend, Some(t), None);
                Ok(())
            }
            SendAdmit {
                send,
                recipients,
                when,
            } => self.send_admit(*send, recipients, *when, by),
            SendDeliver { send } => {
                Self::platform(by)?;
                let m = self.s.sends.get(send).ok_or(Refusal::Unknown)?.clone();
                guard(m.state == SendState::Scheduled, Refusal::WrongState)?;
                guard(self.now >= m.deliver_at, Refusal::TooEarly)?;
                let admitter_ok =
                    self.admitter(&m).is_some_and(|f| self.s.effective(f));
                let ok = admitter_ok
                    && (m.reach.reach <= UNTAPPED_REACH || self.s.contact_ok())
                    && !self.s.banned();
                if ok {
                    self.s
                        .sends
                        .get_mut(send)
                        .ok_or_else(|| gone("send"))?
                        .state = SendState::Delivered;
                    self.effects.push(Effect::Deliver(*send));
                } else {
                    self.cancel_send(*send, true, None);
                }
                Ok(())
            }
            SendCancel { send } => {
                self.mode(ModeClass::TakeAway)?;
                let m = self.s.sends.get(send).ok_or(Refusal::Unknown)?;
                guard(m.state == SendState::Scheduled, Refusal::WrongState)?;
                match by {
                    Authority::Email(a) => {
                        guard(a == self.s.primary.id, Refusal::VoidLink)?;
                        // SPEC: the cancel link is in the held notice only
                        // (D9: "a held send with reach above 5")
                        guard(
                            m.delivery == Delivery::Held
                                && m.reach.reach > UNTAPPED_REACH,
                            Refusal::VoidLink,
                        )?;
                    }
                    Authority::Session(_) | Authority::Signed(_) => {
                        self.authority_live(by)?;
                        self.tier_one(by, ModeClass::TakeAway)?;
                    }
                    _ => return Err(Refusal::WrongAuthority),
                }
                self.s
                    .sends
                    .get_mut(send)
                    .ok_or_else(|| gone("send"))?
                    .state = SendState::Cancelled;
                self.log(
                    OperatorActionKind::DmCancel,
                    self.s.factor_behind(by),
                    None,
                );
                Ok(())
            }
            SendForward { send, deliver_at } => {
                self.send_forward(*send, *deliver_at, by)
            }
            SendLater { send, deliver_at } => {
                self.send_later(*send, *deliver_at, by)
            }
            Retract { send } => {
                match by {
                    Authority::Session(_) | Authority::Signed(_) => {
                        self.authority_live(by)?;
                        self.tier_one(by, ModeClass::TakeAway)?;
                    }
                    _ => return Err(Refusal::WrongAuthority),
                }
                self.mode(ModeClass::TakeAway)?;
                let m = self.s.sends.get_mut(send).ok_or(Refusal::Unknown)?;
                guard(m.state == SendState::Delivered, Refusal::WrongState)?;
                m.state = SendState::Retracted;
                self.log(
                    OperatorActionKind::DmRetract,
                    self.s.factor_behind(by),
                    None,
                );
                Ok(())
            }
            Read { send } => {
                guard(by == Authority::Agent, Refusal::WrongAuthority)?;
                let m = self.s.sends.get_mut(send).ok_or(Refusal::Unknown)?;
                guard(m.state == SendState::Delivered, Refusal::WrongState)?;
                m.state = SendState::Read;
                Ok(())
            }
            RecoveryRequest => {
                // SPEC: §9 "the operator's request when stranded": an operator
                // with no factor can't authenticate, so the request reaches the
                // model through staff
                guard(by == Authority::Staff, Refusal::WrongAuthority)?;
                guard(self.s.stranded(), Refusal::WrongState)?;
                self.open_case(CaseSubject::NoFactor);
                Ok(())
            }
            RecoveryDecide {
                case,
                outcome,
                losing,
            } => {
                guard(by == Authority::Staff, Refusal::WrongAuthority)?;
                let c = self.s.cases.get(case).ok_or(Refusal::Unknown)?.clone();
                let CaseState::Open { round } = c.state else {
                    return Err(Refusal::WrongState);
                };
                let fits = matches!(
                    (c.subject, outcome),
                    (
                        CaseSubject::Contest(_),
                        RecoveryOutcome::Reinstate | RecoveryOutcome::Revoke
                    ) | (
                        CaseSubject::NoFactor,
                        RecoveryOutcome::NewPasskey { .. }
                    )
                );
                guard(fits, Refusal::WrongState)?;
                guard(losing.is_none() || round == 2, Refusal::WrongState)?;
                // RecoverySuspend is from `Active` only: not the contested
                // factor itself, nor anything else suspended or gone
                if let Some(l) = losing {
                    guard(
                        self.s.factor(*l).is_some_and(|f| {
                            f.state == CredentialState::Active
                        }),
                        Refusal::WrongState,
                    )?;
                }
                if let Some(l) = losing {
                    // RecoverySuspend: no suspender exclusion, no time limit
                    let deadline = self.now + suspension_deadline();
                    let targets = self.s.recovery_cascade(*l);
                    self.suspend_one(
                        *l,
                        SuspensionCause::Recovery { case: *case },
                        deadline,
                        None,
                        None,
                    );
                    for g in targets.into_iter().filter(|g| g != l) {
                        self.suspend_one(
                            g,
                            SuspensionCause::Cascade { root: *l, by: None },
                            deadline,
                            Some(*l),
                            None,
                        );
                    }
                }
                self.s
                    .cases
                    .get_mut(case)
                    .ok_or_else(|| gone("case"))?
                    .state = CaseState::Decided {
                    outcome: *outcome,
                    at: self.now,
                    round,
                };
                self.notify(
                    Audience::Both,
                    NoticeSubject::RecoveryDecided(*case),
                    None,
                );
                Ok(())
            }
            RecoveryCancel { case } => {
                let t = self.tap(by, TapKind::CounterClaim)?;
                self.mode(ModeClass::Dispute)?;
                let c = self.s.cases.get(case).ok_or(Refusal::Unknown)?;
                guard(c.effective_at_open.contains(&t), Refusal::NotEligible)?;
                let CaseState::Decided { round: 1, .. } = c.state else {
                    return Err(Refusal::WrongState);
                };
                self.s
                    .cases
                    .get_mut(case)
                    .ok_or_else(|| gone("case"))?
                    .state = CaseState::Open { round: 2 };
                self.notify(
                    Audience::Both,
                    NoticeSubject::RecoveryCounterClaim(*case),
                    None,
                );
                Ok(())
            }
            RecoveryEffective { case } => self.recovery_effective(*case, by),
            SignIn { session, passkey } => {
                guard(
                    by == Authority::Assertion(*passkey),
                    Refusal::WrongAuthority,
                )?;
                let id = FactorId::Passkey(*passkey);
                guard(self.s.effective(id), Refusal::NotEffective)?;
                guard(!self.s.sessions.contains_key(session), Refusal::Exists)?;
                self.s.sessions.insert(
                    *session,
                    Session {
                        passkey: *passkey,
                        last_used: self.now,
                        open: true,
                    },
                );
                let factor = self
                    .s
                    .factors
                    .get_mut(&id)
                    .ok_or_else(|| gone("passkey"))?;
                let first = !factor.signed_in;
                factor.signed_in = true;
                if first {
                    self.notify(
                        Audience::Both,
                        NoticeSubject::NewDeviceSignIn(*passkey),
                        Some(id),
                    );
                }
                Ok(())
            }
            SessionUse { session } => {
                guard(
                    by == Authority::Session(*session),
                    Refusal::WrongAuthority,
                )?;
                self.session(by)?;
                self.s
                    .sessions
                    .get_mut(session)
                    .ok_or_else(|| gone("session"))?
                    .last_used = self.now;
                Ok(())
            }
            SessionExpire { session } => {
                Self::platform(by)?;
                let s = self.s.sessions.get(session).ok_or(Refusal::Unknown)?;
                guard(s.open, Refusal::WrongState)?;
                guard(
                    self.now >= s.last_used + session_ttl(),
                    Refusal::TooEarly,
                )?;
                self.end_session(*session, true, None);
                Ok(())
            }
            SignOut => {
                let me = self.session(by)?;
                self.end_session(me, true, None);
                Ok(())
            }
            SignOutOthers { session } => {
                let me = self.session(by)?;
                self.mode(ModeClass::TakeAway)?;
                if let Some(named) = session {
                    guard(*named != me, Refusal::WrongState)?;
                    let s =
                        self.s.sessions.get(named).ok_or(Refusal::Unknown)?;
                    guard(s.open, Refusal::WrongState)?;
                }
                let others: Vec<_> = self
                    .s
                    .sessions
                    .iter()
                    .filter(|(id, s)| {
                        s.open
                            && **id != me
                            && session.is_none_or(|named| named == **id)
                    })
                    .map(|(id, _)| *id)
                    .collect();
                let caller = self.s.session_factor(me);
                for other in others {
                    self.end_session(other, false, caller);
                }
                self.log(OperatorActionKind::SessionRevoke, caller, None);
                Ok(())
            }
        }
    }

    /// The factor behind what admitted a send
    fn admitter(&self, m: &Send) -> Option<FactorId> {
        match m.admitted_by {
            AdmittedBy::Factor(f) => Some(f),
            AdmittedBy::Session(s) => self.s.session_factor(s),
        }
    }

    /// §4.3: an add that leaves `count` above a threshold meets its
    /// minimum, whatever the flags
    fn minimum_for(&self, count: u32) -> R {
        guard(count <= 5 || self.s.min5(), Refusal::Minimum(5))?;
        guard(count <= 50 || self.s.min50(), Refusal::Minimum(50))
    }

    fn change_request(
        &mut self,
        change: OperatorPendingChangeId,
        setting: &SettingChange,
        by: Authority,
    ) -> R {
        let r = self.tap(by, TapKind::PendingChange)?;
        let replaces_bad_contact =
            matches!(setting, SettingChange::EmergencyContact(Some(_)))
                && self.s.contact.is_some()
                && !self.s.contact_ok();
        self.mode(if replaces_bad_contact {
            ModeClass::ContactReplace
        } else {
            ModeClass::OtherChange
        })?;
        guard(!self.s.changes.contains_key(&change), Refusal::Exists)?;
        guard(
            !self.s.changes.values().any(|c| {
                c.state == ChangeState::Pending
                    && c.change.setting() == setting.setting()
            }),
            Refusal::AlreadyPending,
        )?;
        match setting {
            SettingChange::EmergencyContact(new) => {
                // SPEC: §7.1: until verified an address isn't a contact, so
                // changing one needs a verified contact; the first is
                // ContactSetFirst
                guard(
                    self.s.verified_contact().is_some(),
                    Refusal::WrongState,
                )?;
                guard(
                    new.is_some() || self.s.count <= 5,
                    Refusal::ContactRequired,
                )?;
            }
            SettingChange::BulkHold(d) => guard(
                *d >= bulk_hold_floor() && *d <= max_bulk_hold(),
                Refusal::OutOfRange,
            )?,
            SettingChange::DmHold(d) => guard(
                *d >= chrono::Duration::zero() && *d <= max_dm_hold(),
                Refusal::OutOfRange,
            )?,
            _ => {}
        }
        guard(self.domains_ok_after(setting), Refusal::SameDomain)?;
        let yubikey = self
            .s
            .factor(r)
            .is_some_and(|f| f.kind.is_yubikey_passkey());
        if yubikey && !setting.always_waits() {
            self.s.changes.insert(
                change,
                PendingChange {
                    change: setting.clone(),
                    requested_by: r,
                    at: self.now,
                    state: ChangeState::Effective,
                    address_verified: false,
                },
            );
            self.apply_setting(setting);
            self.log(OperatorActionKind::ConfigChangeEffective, Some(r), None);
            self.notify(
                Audience::Both,
                NoticeSubject::ChangeEffective(change),
                Some(r),
            );
            return Ok(());
        }
        self.s.changes.insert(
            change,
            PendingChange {
                change: setting.clone(),
                requested_by: r,
                at: self.now,
                state: ChangeState::Pending,
                address_verified: false,
            },
        );
        self.notify(
            Audience::Both,
            NoticeSubject::ChangeRequested(change),
            Some(r),
        );
        if let Some(a) = setting.new_address() {
            self.effects.push(Effect::VerificationLink(a.id));
        }
        self.log(OperatorActionKind::ConfigChangeRequested, Some(r), None);
        Ok(())
    }

    fn freeze(&mut self, link: OperatorFreezeLinkId, by: Authority) -> R {
        let Authority::Email(address) = by else {
            return Err(Refusal::WrongAuthority);
        };
        self.mode(ModeClass::Freeze)?;
        let l = *self.s.freeze_links.get(&link).ok_or(Refusal::VoidLink)?;
        let contact = self.s.contact.as_ref().ok_or(Refusal::VoidLink)?;
        guard(
            !l.used
                && !l.void
                && l.address == address
                && contact.address.id == address
                && self.now < l.sent_at + freeze_link_life(),
            Refusal::VoidLink,
        )?;
        guard(contact.power == FreezePower::Armed, Refusal::VoidLink)?;
        guard(
            self.s
                .last_freeze
                .get(&address)
                .is_none_or(|t| self.now >= *t + freeze_interval()),
            Refusal::RateLimited,
        )?;
        self.s
            .freeze_links
            .get_mut(&link)
            .ok_or_else(|| gone("freeze link"))?
            .used = true;
        self.s.last_freeze.insert(address, self.now);
        let entry = self.log(OperatorActionKind::Freeze, None, Some(l.factor));
        self.s.hold = Hold::Frozen(Frozen {
            entry,
            excluded: l.factor,
            at: self.now,
            address,
            notice_sent: false,
        });
        // No cascade; an already suspended or revoked factor still freezes
        self.suspend_one(
            l.factor,
            SuspensionCause::Freeze { entry },
            self.now + suspension_deadline(),
            None,
            None,
        );
        self.freeze_cancels_everything();
        self.effects.push(Effect::SecurityAlert);
        self.notify(Audience::Both, NoticeSubject::Frozen, None);
        Ok(())
    }

    fn send_admit(
        &mut self,
        send: OperatorActionId,
        recipients: &BTreeSet<AgentId>,
        when: When,
        by: Authority,
    ) -> R {
        let admitted_by = match by {
            Authority::Session(s) => {
                self.authority_live(by)?;
                AdmittedBy::Session(s)
            }
            Authority::Signed(c) => {
                self.authority_live(by)?;
                AdmittedBy::Factor(FactorId::Credential(c))
            }
            Authority::PasskeyTap { .. } => {
                AdmittedBy::Factor(self.tap(by, TapKind::Send)?)
            }
            _ => return Err(Refusal::WrongAuthority),
        };
        guard(!self.s.sends.contains_key(&send), Refusal::Exists)?;
        let window = self.s.window_messages();
        let level = self.s.level(by);
        let plan = plan_send(&SendRequest {
            recipients,
            when,
            level,
            contact_ok: self.s.contact_ok(),
            sends_now: self
                .s
                .factor_behind(by)
                .is_none_or(|f| self.s.sends_now(f, self.now)),
            new_account: self.s.new_account(self.now),
            now: self.now,
            dm_hold: self.s.settings.dm_hold,
            bulk_hold: self.s.settings.bulk_hold,
            window: &window,
        })?;
        let class = if plan.reach.reach > UNTAPPED_REACH {
            ModeClass::SendLarge
        } else {
            ModeClass::SendSmall
        };
        self.tier_one(by, class)?;
        self.mode(class)?;
        self.s.sends.insert(
            send,
            Send {
                recipients: recipients.clone(),
                deliver_at: plan.deliver_at,
                delivery: plan.delivery,
                reach: plan.reach,
                admitted_by,
                state: SendState::Scheduled,
                admitted_at: self.now,
                level,
                announced: plan.announced(),
            },
        );
        let flag = self.s.factor_behind(by);
        if plan.announced_held() {
            self.effects.push(Effect::Notice(Notice {
                to: Audience::Primary,
                subject: NoticeSubject::SendHeld(send),
                freeze_link: None,
                cancel_link: Some(send),
                names: false,
            }));
            self.notify(Audience::Contact, NoticeSubject::SendHeld(send), flag);
        }
        if plan.announced_now() {
            self.notify(Audience::Both, NoticeSubject::SendNow(send), flag);
        } else if plan.announced_tapped() && !plan.announced_held() {
            self.notify(Audience::Both, NoticeSubject::SendTapped(send), flag);
        }
        if plan.names_to_contact() {
            for e in self.effects.iter_mut().rev() {
                if let Effect::Notice(n) = e
                    && matches!(n.to, Audience::Contact | Audience::Both)
                {
                    n.names = true;
                    break;
                }
            }
        }
        self.log(OperatorActionKind::DmSend, flag, None);
        Ok(())
    }

    /// **SendForward**: earlier, by a YubiKey tap, which becomes what admitted it
    fn send_forward(
        &mut self,
        send: OperatorActionId,
        deliver_at: DateTime<Utc>,
        by: Authority,
    ) -> R {
        let t = self.yubikey_tap(by, TapKind::MoveSend)?;
        let m = self.s.sends.get(&send).ok_or(Refusal::Unknown)?.clone();
        guard(m.state == SendState::Scheduled, Refusal::WrongState)?;
        let deliver_at = deliver_at.max(self.now);
        guard(deliver_at < m.deliver_at, Refusal::WrongState)?;
        let window = self.s.window_without(Some(send));
        let at_new = reach(&m.recipients, deliver_at, &window);
        self.mode(if at_new.reach > UNTAPPED_REACH {
            ModeClass::SendLarge
        } else {
            ModeClass::SendSmall
        })?;
        let above_5 = at_new.immediate_reach > UNTAPPED_REACH;
        guard(
            !above_5 || self.s.sends_now(t, self.now),
            SendRefusal::NewKey { reach: at_new }.into(),
        )?;
        guard(
            !(self.s.new_account(self.now)
                && (above_5 || m.reach.immediate_reach > UNTAPPED_REACH)
                && deliver_at < m.admitted_at + super::tap::long_hold()),
            SendRefusal::NewAccount { reach: at_new }.into(),
        )?;
        let moved = self.s.sends.get_mut(&send).ok_or_else(|| gone("send"))?;
        moved.deliver_at = deliver_at;
        moved.reach = widest(m.reach, at_new);
        moved.admitted_by = AdmittedBy::Factor(t);
        moved.level = TapLevel::YubikeyTap;
        moved.announced = true;
        self.notify(Audience::Both, NoticeSubject::SendMoved(send), Some(t));
        self.log(OperatorActionKind::DmSend, Some(t), None);
        Ok(())
    }

    /// **SendLater**: counted again at the new time, under the rule for a new
    /// send, with the level the send has and the hold from its admission
    fn send_later(
        &mut self,
        send: OperatorActionId,
        deliver_at: DateTime<Utc>,
        by: Authority,
    ) -> R {
        let m = self.s.sends.get(&send).ok_or(Refusal::Unknown)?.clone();
        guard(m.state == SendState::Scheduled, Refusal::WrongState)?;
        guard(deliver_at > m.deliver_at, Refusal::WrongState)?;
        let mover = if m.level == TapLevel::YubikeyTap {
            Some(self.yubikey_tap(by, TapKind::MoveSend)?)
        } else {
            match by {
                Authority::Session(_) | Authority::Signed(_) => {
                    self.authority_live(by)?;
                    self.s.factor_behind(by)
                }
                _ => return Err(Refusal::WrongAuthority),
            }
        };
        let admitter = self.admitter(&m);
        let window = self.s.window_without(Some(send));
        let plan = plan_send(&SendRequest {
            recipients: &m.recipients,
            when: When::At(deliver_at),
            level: m.level,
            contact_ok: self.s.contact_ok(),
            sends_now: admitter.is_none_or(|f| self.s.sends_now(f, self.now)),
            new_account: self.s.new_account(self.now),
            now: m.admitted_at,
            dm_hold: self.s.settings.dm_hold,
            bulk_hold: self.s.settings.bulk_hold,
            window: &window,
        })?;
        // SPEC: §4.2 has no row for SendLater: it can land a send, so it takes
        // what sending takes, not taking away's pass
        let class = if plan.reach.reach > UNTAPPED_REACH {
            ModeClass::SendLarge
        } else {
            ModeClass::SendSmall
        };
        self.tier_one(by, class)?;
        self.mode(class)?;
        let moved = self.s.sends.get_mut(&send).ok_or_else(|| gone("send"))?;
        moved.deliver_at = deliver_at;
        moved.delivery = Delivery::Held;
        moved.reach = widest(m.reach, plan.reach);
        if m.announced {
            let flag = admitter.filter(|f| self.s.effective(*f));
            self.effects.push(Effect::Notice(Notice {
                to: Audience::Primary,
                subject: NoticeSubject::SendMoved(send),
                freeze_link: None,
                cancel_link: Some(send),
                names: false,
            }));
            self.notify(
                Audience::Contact,
                NoticeSubject::SendMoved(send),
                flag,
            );
            if plan.reach.reach > NAMED_REACH
                && let Some(Effect::Notice(n)) = self.effects.last_mut()
            {
                n.names = true;
            }
        }
        self.log(OperatorActionKind::DmSend, mover, None);
        Ok(())
    }

    fn recovery_effective(
        &mut self,
        case: OperatorRecoveryCaseId,
        by: Authority,
    ) -> R {
        Self::platform(by)?;
        let c = self.s.cases.get(&case).ok_or(Refusal::Unknown)?.clone();
        let CaseState::Decided { outcome, at, .. } = c.state else {
            return Err(Refusal::WrongState);
        };
        guard(self.now >= at + recovery_lock(), Refusal::TooEarly)?;
        match (c.subject, outcome) {
            (
                CaseSubject::Contest(f),
                RecoveryOutcome::Reinstate | RecoveryOutcome::Revoke,
            ) => {
                let factor = self.s.factor(f).ok_or(Refusal::Unknown)?;
                guard(
                    matches!(factor.state, CredentialState::Contested { case: k, .. } if k == case),
                    Refusal::WrongState,
                )?;
                let reinstate = outcome == RecoveryOutcome::Reinstate;
                for g in std::iter::once(f).chain(self.followers(f)) {
                    let factor = self
                        .s
                        .factors
                        .get_mut(&g)
                        .ok_or_else(|| gone("factor"))?;
                    if reinstate {
                        factor.state = CredentialState::Active;
                        self.log(
                            OperatorActionKind::CredentialReinstate,
                            None,
                            Some(g),
                        );
                        self.notify(
                            Audience::Both,
                            NoticeSubject::Reinstated(g),
                            None,
                        );
                    } else {
                        factor.state = CredentialState::Revoked;
                        self.log(
                            OperatorActionKind::CredentialRevoke,
                            None,
                            Some(g),
                        );
                        self.notify(
                            Audience::Both,
                            NoticeSubject::Revoked(g),
                            None,
                        );
                    }
                }
            }
            (
                CaseSubject::NoFactor,
                RecoveryOutcome::NewPasskey { passkey, class },
            ) => {
                // SPEC: I-15 "none while banned" read literally: a banned
                // account's recovery add waits for the unban
                guard(!self.s.banned(), Refusal::Banned)?;
                let id = FactorId::Passkey(passkey);
                self.add_factor(id, FactorKind::Passkey(class), None, None)?;
                self.log(OperatorActionKind::CredentialAdd, None, Some(id));
                self.notify(
                    Audience::Both,
                    NoticeSubject::FactorAdded(id),
                    None,
                );
            }
            _ => return Err(Refusal::WrongState),
        }
        self.s
            .cases
            .get_mut(&case)
            .ok_or_else(|| gone("case"))?
            .state = CaseState::Effective;
        Ok(())
    }
}

/// The wider of a send's reaches, so moving it never voids its cancel link or
/// its contact recheck
fn widest(a: super::tap::Reach, b: super::tap::Reach) -> super::tap::Reach {
    super::tap::Reach {
        reach: a.reach.max(b.reach),
        immediate_reach: a.immediate_reach.max(b.immediate_reach),
    }
}
