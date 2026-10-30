//! The state table's worked cases, decision 9's rule, and property tests of
//! §11's invariants over generated event sequences

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;

use super::challenge::TapKind;
use super::tap::*;
use super::*;
use crate::enums::{OperatorActionKind, PinPolicy, TouchPolicy};
use crate::ids::*;
use crate::operator_signer::KeyPolicy;

// ---------------------------------------------------------------------------
// The universe
// ---------------------------------------------------------------------------

fn uuid(space: u128, i: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128((space << 64) | i)
}
fn pk(i: u8) -> OperatorPasskeyId {
    uuid(1, i.into()).into()
}
fn cred(i: u8) -> OperatorCredentialId {
    uuid(2, i.into()).into()
}
fn sess(i: u8) -> OperatorSessionId {
    uuid(3, i.into()).into()
}
fn send_id(i: u8) -> OperatorActionId {
    uuid(4, i.into()).into()
}
fn change_id(i: u8) -> OperatorPendingChangeId {
    uuid(5, i.into()).into()
}
fn enrol(i: u8) -> OperatorEnrolmentId {
    uuid(6, i.into()).into()
}
fn agent(i: u32) -> AgentId {
    uuid(7, i.into()).into()
}
fn addr_id(i: u8) -> OperatorEmailAddressId {
    uuid(8, i.into()).into()
}
fn address(i: u8, tenant: &str) -> EmailAddress {
    EmailAddress {
        id: addr_id(i),
        address: NormalisedEmail(format!("a{i}@{tenant}")),
        tenant: MailTenant(tenant.into()),
    }
}
fn p(i: u8) -> FactorId {
    FactorId::Passkey(pk(i))
}
fn c(i: u8) -> FactorId {
    FactorId::Credential(cred(i))
}
fn agents(start: u32, len: u32) -> BTreeSet<AgentId> {
    (start..start + len).map(agent).collect()
}

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_761_782_400, 0).unwrap()
}

const SYNCED: PasskeyClass = PasskeyClass {
    synced: true,
    yubikey: false,
};
const DEVICE: PasskeyClass = PasskeyClass {
    synced: false,
    yubikey: false,
};
const YUBIKEY: PasskeyClass = PasskeyClass {
    synced: false,
    yubikey: true,
};
const UNATTENDED: FactorKind = FactorKind::HardwareKey(KeyPolicy::UNATTENDED);
const SUDO: FactorKind = FactorKind::HardwareKey(KeyPolicy::SUDO);
const SUDO_PIN: FactorKind = FactorKind::HardwareKey(KeyPolicy::SUDO_WITH_PIN);

/// Sequential ids, so a run is reproducible
#[derive(Default)]
struct Counter(u128);

impl IdSource for Counter {
    fn action(&mut self) -> OperatorActionId {
        self.0 += 1;
        uuid(9, self.0).into()
    }
    fn case(&mut self) -> OperatorRecoveryCaseId {
        self.0 += 1;
        uuid(10, self.0).into()
    }
    fn freeze_link(&mut self) -> OperatorFreezeLinkId {
        self.0 += 1;
        uuid(11, self.0).into()
    }
}

fn tap(i: u8, kind: TapKind) -> Authority {
    Authority::PasskeyTap {
        passkey: pk(i),
        kind,
    }
}

/// A driver for worked cases: the state, a clock and ids
struct World {
    s: AccountState,
    now: DateTime<Utc>,
    ids: Counter,
    effects: Vec<Effect>,
}

impl World {
    fn new(s: AccountState) -> Self {
        Self {
            s,
            now: t0(),
            ids: Counter::default(),
            effects: Vec::new(),
        }
    }

    fn try_do(&mut self, event: Event, by: Authority) -> Result<(), Refusal> {
        let t = self.s.apply(&event, by, self.now, &mut self.ids)?;
        self.s = t.state;
        self.effects = t.effects;
        Ok(())
    }

    #[track_caller]
    fn ok(&mut self, event: Event, by: Authority) {
        if let Err(e) = self.try_do(event.clone(), by) {
            panic!("{event:?} by {by:?} refused: {e}");
        }
    }

    fn wait(&mut self, d: Duration) {
        self.now += d;
    }

    fn state(&self, f: FactorId) -> CredentialState {
        self.s.factor(f).unwrap().state
    }

    /// The freeze link on the last effects' contact copy
    fn link(&self) -> OperatorFreezeLinkId {
        self.effects
            .iter()
            .rev()
            .find_map(|e| match e {
                Effect::Notice(Notice {
                    freeze_link: Some((id, _)),
                    ..
                }) => Some(*id),
                _ => None,
            })
            .expect("a freeze link")
    }

    /// Add a passkey approved by passkey `by`'s tap
    #[track_caller]
    fn add_passkey(&mut self, i: u8, class: PasskeyClass, by: u8, session: u8) {
        self.ok(
            Event::PasskeyRegister {
                passkey: pk(i),
                class,
            },
            Authority::Session(sess(session)),
        );
        self.ok(
            Event::PasskeyApprove { passkey: pk(i) },
            tap(by, TapKind::PasskeyEnrolment),
        );
    }

    #[track_caller]
    fn sign_in(&mut self, session: u8, passkey: u8) {
        self.ok(
            Event::SignIn {
                session: sess(session),
                passkey: pk(passkey),
            },
            Authority::Assertion(pk(passkey)),
        );
    }

    /// A verified contact on another domain
    #[track_caller]
    fn contact(&mut self, by: u8) {
        self.ok(
            Event::ContactSetFirst {
                address: address(1, "contact.example"),
            },
            tap(by, TapKind::FirstContact),
        );
        self.ok(
            Event::AddressVerify {
                address: addr_id(1),
            },
            Authority::Email(addr_id(1)),
        );
    }
}

/// An account registered with passkey 0 at `t0`, signed in as session 0
fn registered(class: PasskeyClass) -> World {
    let s = AccountState::register(
        OperatorId::from(uuid(0, 1)),
        address(0, "primary.example"),
        pk(0),
        class,
        t0(),
        3,
    );
    let mut w = World::new(s);
    w.sign_in(0, 0);
    w
}

// ---------------------------------------------------------------------------
// Decision 9
// ---------------------------------------------------------------------------

#[test]
fn required_tap_by_reach() {
    let r = |reach, immediate_reach| Reach {
        reach,
        immediate_reach,
    };
    use Delivery::*;
    use TapLevel as L;
    assert_eq!(required_tap(r(5, 5), Immediate), Some(L::None));
    assert_eq!(required_tap(r(6, 5), Held), Some(L::Unattended));
    assert_eq!(required_tap(r(6, 5), Immediate), Some(L::Tap));
    assert_eq!(required_tap(r(6, 6), Immediate), Some(L::YubikeyTap));
    assert_eq!(required_tap(r(500, 500), Immediate), Some(L::YubikeyTap));
    assert_eq!(required_tap(r(500, 501), Immediate), None);
    assert_eq!(required_tap(r(501, 1), Immediate), None);
    assert_eq!(required_tap(r(501, 501), Held), Some(L::YubikeyTap));
    // An immediate reach above 5 needs a YubiKey tap even at reach ≤ 5
    assert_eq!(required_tap(r(1, 6), Immediate), Some(L::YubikeyTap));
    assert_eq!(required_tap(r(1, 6), Held), Some(L::None));
}

#[test]
fn levels_by_factor() {
    assert_eq!(FactorKind::SoftwareKey.tap_level(), TapLevel::None);
    assert_eq!(UNATTENDED.tap_level(), TapLevel::Unattended);
    let cached = FactorKind::HardwareKey(KeyPolicy {
        pin: PinPolicy::Never,
        touch: TouchPolicy::Cached,
    });
    assert_eq!(cached.tap_level(), TapLevel::Unattended);
    assert_eq!(SUDO.tap_level(), TapLevel::Tap);
    assert_eq!(SUDO_PIN.tap_level(), TapLevel::YubikeyTap);
    assert_eq!(FactorKind::Passkey(SYNCED).tap_level(), TapLevel::Tap);
    assert_eq!(
        FactorKind::Passkey(YUBIKEY).tap_level(),
        TapLevel::YubikeyTap
    );
    assert!(
        SUDO.gives_sudo() && SUDO_PIN.gives_sudo() && !UNATTENDED.gives_sudo()
    );
    assert!(SUDO_PIN.gives_yubikey_tap() && !SUDO.gives_yubikey_tap());
}

fn request<'a>(
    recipients: &'a BTreeSet<AgentId>,
    when: When,
    level: TapLevel,
    window: &'a [WindowMessage],
) -> SendRequest<'a> {
    SendRequest {
        recipients,
        when,
        level,
        contact_ok: true,
        now: t0(),
        dm_hold: Duration::minutes(1),
        bulk_hold: Duration::hours(1),
        window,
    }
}

/// Decision 9's worked example: 400 held to tomorrow, then chats
#[test]
fn decision_9_worked_example() {
    let tomorrow = t0() + Duration::hours(15);
    let fleet = agents(0, 400);
    let held =
        plan_send(&request(&fleet, When::At(tomorrow), TapLevel::Tap, &[]))
            .unwrap();
    assert_eq!(held.delivery, Delivery::Held);
    assert_eq!(held.reach.reach, 400);
    let window: Vec<WindowMessage> = fleet
        .iter()
        .map(|a| WindowMessage {
            recipient: *a,
            deliver_at: tomorrow,
            immediate: false,
        })
        .collect();
    // A chat with one of the 400: reach 1, no tap, after dm_hold
    let one = agents(7, 1);
    let chat =
        plan_send(&request(&one, When::Default, TapLevel::None, &window))
            .unwrap();
    assert_eq!(chat.reach.reach, 1);
    assert_eq!(chat.deliver_at, t0() + Duration::minutes(1));
    // Outside the 400: reach 401, a passkey tap; immediate reach 1, so now
    let outside = agents(1000, 1);
    assert_eq!(
        plan_send(&request(&outside, When::Default, TapLevel::None, &window)),
        Err(SendRefusal::NeedsTap {
            required: Some(TapLevel::Unattended),
            reach: Reach {
                reach: 401,
                immediate_reach: 1
            }
        })
    );
    let tapped =
        plan_send(&request(&outside, When::Now, TapLevel::Tap, &window))
            .unwrap();
    assert_eq!(tapped.delivery, Delivery::Immediate);
    assert_eq!(tapped.reach.reach, 401);
    // The same 400 again: reach 400, another tap
    let again =
        plan_send(&request(&fleet, When::Default, TapLevel::None, &window));
    assert!(matches!(again, Err(SendRefusal::NeedsTap { .. })));
}

#[test]
fn defaults_and_explicit_times() {
    let six = agents(0, 6);
    // Unattended: may not go now, so the default holds it
    let plan =
        plan_send(&request(&six, When::Default, TapLevel::Unattended, &[]))
            .unwrap();
    assert_eq!(plan.delivery, Delivery::Held);
    assert_eq!(plan.deliver_at, t0() + Duration::hours(1));
    // ... and an explicit send-now is refused with the level it needs
    assert_eq!(
        plan_send(&request(&six, When::Now, TapLevel::Unattended, &[])),
        Err(SendRefusal::NeedsTap {
            required: Some(TapLevel::YubikeyTap),
            reach: Reach {
                reach: 6,
                immediate_reach: 6
            }
        })
    );
    // A YubiKey tap sends now
    let plan =
        plan_send(&request(&six, When::Default, TapLevel::YubikeyTap, &[]))
            .unwrap();
    assert_eq!(plan.delivery, Delivery::Immediate);
    // Above 500: always held, even by a YubiKey tap asking for now
    let many = agents(0, 501);
    assert!(
        plan_send(&request(&many, When::Now, TapLevel::YubikeyTap, &[]))
            .is_err()
    );
    let plan =
        plan_send(&request(&many, When::Default, TapLevel::YubikeyTap, &[]))
            .unwrap();
    assert_eq!(plan.delivery, Delivery::Held);
    assert!(plan.names_to_contact() && plan.announced_held());
    // A software key never sends above 5
    assert!(
        plan_send(&request(&six, When::Default, TapLevel::None, &[])).is_err()
    );
    // No contact, no send above 5
    let mut no_contact =
        request(&six, When::Default, TapLevel::YubikeyTap, &[]);
    no_contact.contact_ok = false;
    assert!(matches!(
        plan_send(&no_contact),
        Err(SendRefusal::NeedsContact { .. })
    ));
}

/// Sends spread over a day can't land beside each other uncounted: the
/// window is ±24 h on deliver_at, both ends included
#[test]
fn window_is_on_deliver_at() {
    let at = t0() + Duration::hours(30);
    let window: Vec<WindowMessage> = agents(0, 5)
        .into_iter()
        .map(|a| WindowMessage {
            recipient: a,
            deliver_at: at,
            immediate: false,
        })
        .collect();
    let one = agents(100, 1);
    let r = reach(&one, at + Duration::hours(24), &window);
    assert_eq!(r.reach, 6);
    let r = reach(
        &one,
        at + Duration::hours(24) + Duration::seconds(1),
        &window,
    );
    assert_eq!(r.reach, 1);
}

// ---------------------------------------------------------------------------
// Worked cases
// ---------------------------------------------------------------------------

/// CD1: "claude-opus rotates 9c by suspending it with a tap of the
/// ceremonial Bio": both Bios stay effective, only 9c is suspended
#[test]
fn rotating_9c_from_the_ceremonial_bio_spares_both_bios() {
    let s = AccountState::migrated(
        OperatorId::from(uuid(0, 1)),
        address(0, "gmail:claude"),
        AuthMode::PasswordOnly,
        [(c(0), UNATTENDED, t0()), (c(1), SUDO, t0())],
        3,
    );
    let mut w = World::new(s);
    w.ok(
        Event::PasswordSignIn { session: sess(0) },
        Authority::Password,
    );
    // The ceremonial Bio, confirmed by 9c; the password retires
    w.ok(
        Event::FirstPasskey {
            passkey: pk(0),
            class: YUBIKEY,
            confirm: Some(cred(1)),
        },
        Authority::Session(sess(0)),
    );
    assert_eq!(w.s.factor(p(0)).unwrap().parent, Some(c(1)));
    assert!(!w.s.sessions[&sess(0)].open);
    w.sign_in(1, 0);
    // The balerion Bio, approved by a YubiKey tap of the ceremonial one
    w.add_passkey(1, YUBIKEY, 0, 1);
    assert_eq!(w.s.factor(p(1)).unwrap().parent, Some(p(0)));
    w.ok(Event::Suspend { target: c(1) }, tap(0, TapKind::Suspend));
    assert!(matches!(w.state(c(1)), CredentialState::Suspended(_)));
    assert_eq!(w.state(p(0)), CredentialState::Active);
    assert_eq!(w.state(p(1)), CredentialState::Active);
    assert_eq!(w.state(c(0)), CredentialState::Active);
}

/// CD1: the owner's Q added a YubiKey Y; a thief holding Y added Z. The
/// owner suspends Y with Q: Y and Z are suspended, Q is untouched
#[test]
fn suspending_a_yubikey_you_added_cascades_to_what_was_added_through_it() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, YUBIKEY, 0, 0); // Y, by Q
    w.sign_in(1, 1);
    w.add_passkey(2, YUBIKEY, 1, 1); // Z, by Y (a YubiKey tap)
    w.ok(Event::Suspend { target: p(1) }, tap(0, TapKind::Suspend));
    assert!(matches!(
        w.state(p(1)),
        CredentialState::Suspended(Suspension {
            cause: SuspensionCause::Direct { .. },
            ..
        })
    ));
    assert!(matches!(
        w.state(p(2)),
        CredentialState::Suspended(Suspension {
            follows_root: Some(root),
            ..
        }) if root == p(1)
    ));
    assert_eq!(w.state(p(0)), CredentialState::Active);
    // Y's session ended with it
    assert!(!w.s.sessions[&sess(1)].open);
}

/// I-1: a thief's Y suspends the owner's Q, which added Y: the cascade
/// can't pass through the suspender, so Y is untouched
#[test]
fn a_suspension_stops_at_its_suspender() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, YUBIKEY, 0, 0);
    w.ok(Event::Suspend { target: p(0) }, tap(1, TapKind::Suspend));
    assert!(matches!(w.state(p(0)), CredentialState::Suspended(_)));
    assert_eq!(w.state(p(1)), CredentialState::Active);
}

/// Three ways round the unfreeze age rule, all closed (§12.3)
#[test]
fn the_three_unfreeze_bypass_routes_fail() {
    // The owner: P0 (old, synced: the stolen one), P1 (old device)
    let mut w = registered(SYNCED);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(1);
    w.wait(Duration::days(10));
    // 1. The thief adds P2 with P0; the contact freezes on a send by P0;
    //    P2, younger than a week, can't unfreeze
    w.add_passkey(2, DEVICE, 0, 0);
    let six = agents(0, 6);
    w.ok(
        Event::SendAdmit {
            send: send_id(0),
            recipients: six,
            when: When::Default,
        },
        tap(0, TapKind::Send),
    );
    let link = w.link();
    w.ok(Event::Freeze { link }, Authority::Email(addr_id(1)));
    let frozen = *w.s.frozen().unwrap();
    assert_eq!(frozen.excluded, p(0));
    assert!(matches!(w.state(p(0)), CredentialState::Suspended(_)));
    assert_eq!(w.s.sends[&send_id(0)].state, SendState::Cancelled);
    assert_eq!(
        w.try_do(Event::Unfreeze, tap(2, TapKind::Unfreeze)),
        Err(Refusal::NotEligible)
    );
    // 2. The excluded factor itself can't (it is suspended, and u ≠ X)
    assert!(
        w.try_do(Event::Unfreeze, tap(0, TapKind::Unfreeze))
            .is_err()
    );
    assert!(!w.s.eligible(p(0), p(0), frozen.at));
    // The owner's old P1 can, and the address is disarmed
    w.ok(Event::Unfreeze, tap(1, TapKind::Unfreeze));
    assert_eq!(w.s.contact.as_ref().unwrap().power, FreezePower::Disarmed);
    // The excluded factor stays suspended
    assert!(matches!(w.state(p(0)), CredentialState::Suspended(_)));

    // 3. An ancestor: Q (old) added the frozen P in the week before
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.wait(Duration::days(10));
    w.add_passkey(2, DEVICE, 0, 0); // P2 added by Q = P0, recently
    w.sign_in(2, 2);
    w.ok(
        Event::OAuthConsent {
            client: "client".parse().unwrap(),
            agent: agent(0),
        },
        tap(2, TapKind::OAuthConsent),
    );
    let link = w.link();
    w.ok(Event::Freeze { link }, Authority::Email(addr_id(1)));
    assert_eq!(w.s.frozen().unwrap().excluded, p(2));
    assert!(w.s.grants.is_empty(), "the suspension revoked its grant");
    assert_eq!(
        w.try_do(Event::Unfreeze, tap(0, TapKind::Unfreeze)),
        Err(Refusal::NotEligible)
    );
    // ... a sibling that added nothing recently can
    w.ok(Event::Unfreeze, tap(1, TapKind::Unfreeze));
}

/// §12.3: a stolen plain passkey can't add a PIN-always PIV key once the
/// account has a YubiKey-tap factor
#[test]
fn a_yubikey_tap_factor_needs_a_yubikey_tap_to_add() {
    let mut w = registered(SYNCED);
    w.add_passkey(1, YUBIKEY, 0, 0);
    w.ok(
        Event::EnrolStart {
            enrolment: enrol(0),
            kind: SUDO_PIN,
        },
        Authority::Anonymous,
    );
    assert_eq!(
        w.try_do(
            Event::EnrolApprove {
                enrolment: enrol(0),
                credential: cred(0)
            },
            tap(0, TapKind::EnrolmentApproval),
        ),
        Err(Refusal::NeedsYubikeyTap)
    );
    // Suspending the owner's YubiKey doesn't lower the bar: it counts
    // until revoked
    w.ok(Event::Suspend { target: p(1) }, tap(0, TapKind::Suspend));
    assert!(w.s.has_yk_tap());
    assert_eq!(
        w.try_do(
            Event::EnrolApprove {
                enrolment: enrol(0),
                credential: cred(0)
            },
            tap(0, TapKind::EnrolmentApproval),
        ),
        Err(Refusal::NeedsYubikeyTap)
    );
}

/// The above, with a passkey: the thief's own YubiKey needs the owner's
#[test]
fn a_suspended_yubikey_counts_until_revoked() {
    let mut w = registered(SYNCED);
    w.add_passkey(1, YUBIKEY, 0, 0);
    w.ok(Event::Suspend { target: p(1) }, tap(0, TapKind::Suspend));
    w.ok(
        Event::PasskeyRegister {
            passkey: pk(2),
            class: YUBIKEY,
        },
        Authority::Session(sess(0)),
    );
    assert_eq!(
        w.try_do(
            Event::PasskeyApprove { passkey: pk(2) },
            tap(0, TapKind::PasskeyEnrolment)
        ),
        Err(Refusal::NeedsYubikeyTap)
    );
    // After its 72 h it is revoked, and a plain tap may add one (a Known
    // hole's remedy)
    // A contest at the deadline's instant would win: revocation is after
    w.wait(Duration::hours(72));
    assert_eq!(
        w.try_do(Event::Deadline { factor: p(1) }, Authority::Platform),
        Err(Refusal::TooEarly)
    );
    w.wait(Duration::seconds(1));
    w.ok(Event::Deadline { factor: p(1) }, Authority::Platform);
    assert!(!w.s.has_yk_tap());
    w.ok(
        Event::PasskeyApprove { passkey: pk(2) },
        tap(0, TapKind::PasskeyEnrolment),
    );
}

/// A contest stops the clock of its root's followers, and the root's
/// outcome applies to them
#[test]
fn followers_share_their_roots_fate() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.sign_in(1, 1);
    w.add_passkey(2, DEVICE, 1, 1);
    w.add_passkey(3, DEVICE, 0, 0);
    w.ok(Event::Suspend { target: p(1) }, tap(3, TapKind::Suspend));
    w.ok(Event::Contest { factor: p(1) }, tap(1, TapKind::Contest));
    w.wait(Duration::hours(80));
    assert_eq!(
        w.try_do(Event::Deadline { factor: p(2) }, Authority::Platform),
        Err(Refusal::WrongState),
        "a follower of a contested root is never revoked at the deadline"
    );
    let case = *w.s.cases.keys().next().unwrap();
    w.ok(
        Event::RecoveryDecide {
            case,
            outcome: RecoveryOutcome::Reinstate,
            losing: None,
        },
        Authority::Staff,
    );
    w.wait(Duration::days(7));
    w.ok(Event::RecoveryEffective { case }, Authority::Platform);
    assert_eq!(w.state(p(1)), CredentialState::Active);
    assert_eq!(w.state(p(2)), CredentialState::Active);
}

/// §12.1: a contact can't freeze again while frozen, nor with a link sent
/// to it before it was replaced
#[test]
fn freeze_links_are_void_while_frozen_and_after_replacement() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    // No contact yet: the add's notice carried no link
    assert!(w.s.freeze_links.is_empty());
    w.contact(0);
    w.add_passkey(2, DEVICE, 0, 0);
    let link_a = w.link();
    w.add_passkey(3, DEVICE, 1, 0);
    let link_b = w.link();
    w.ok(Event::Freeze { link: link_a }, Authority::Email(addr_id(1)));
    w.wait(Duration::hours(25));
    assert_eq!(
        w.try_do(Event::Freeze { link: link_b }, Authority::Email(addr_id(1))),
        Err(Refusal::Mode(ModeClass::Freeze))
    );
    // Unfreeze is impossible with only week-young factors: manual
    // recovery (a Known hole); here, wait it out in a fresh world
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.wait(Duration::days(8));
    w.add_passkey(2, DEVICE, 0, 0);
    let old_link = w.link();
    // Replace the contact (72 h), verified
    w.ok(
        Event::ChangeRequest {
            change: change_id(0),
            setting: SettingChange::EmergencyContact(Some(address(
                2,
                "other.example",
            ))),
        },
        tap(1, TapKind::PendingChange),
    );
    w.ok(
        Event::AddressVerify {
            address: addr_id(2),
        },
        Authority::Email(addr_id(2)),
    );
    w.wait(Duration::hours(72));
    w.ok(
        Event::ChangeEffective {
            change: change_id(0),
        },
        Authority::Platform,
    );
    assert_eq!(w.s.verified_contact().unwrap().address.id, addr_id(2));
    assert_eq!(
        w.try_do(
            Event::Freeze { link: old_link },
            Authority::Email(addr_id(1))
        ),
        Err(Refusal::VoidLink)
    );
    assert_eq!(
        w.try_do(
            Event::Freeze { link: old_link },
            Authority::Email(addr_id(2))
        ),
        Err(Refusal::VoidLink)
    );
}

/// §12.3: retiring the password ends the thief's password session, and
/// cancels what it admitted
#[test]
fn the_first_passkey_ends_password_sessions() {
    let s = AccountState::migrated(
        OperatorId::from(uuid(0, 1)),
        address(0, "primary.example"),
        AuthMode::PasswordOnly,
        [],
        3,
    );
    let mut w = World::new(s);
    w.ok(
        Event::PasswordSignIn { session: sess(0) },
        Authority::Password,
    );
    w.ok(
        Event::PasswordSignIn { session: sess(1) },
        Authority::Password,
    );
    w.ok(
        Event::SendAdmit {
            send: send_id(0),
            recipients: agents(0, 2),
            when: When::Default,
        },
        Authority::Session(sess(1)),
    );
    w.ok(
        Event::FirstPasskey {
            passkey: pk(0),
            class: DEVICE,
            confirm: None,
        },
        Authority::Session(sess(0)),
    );
    assert!(!w.s.sessions[&sess(1)].open);
    assert_eq!(w.s.sends[&send_id(0)].state, SendState::Cancelled);
    assert_eq!(
        w.try_do(
            Event::PasswordSignIn { session: sess(2) },
            Authority::Password
        ),
        Err(Refusal::WrongState)
    );
}

/// A live sudo key must confirm the first passkey
#[test]
fn a_sudo_key_confirms_the_first_passkey() {
    let s = AccountState::migrated(
        OperatorId::from(uuid(0, 1)),
        address(0, "primary.example"),
        AuthMode::PasswordOnly,
        [(c(0), SUDO, t0()), (c(1), UNATTENDED, t0())],
        3,
    );
    let mut w = World::new(s);
    w.ok(
        Event::PasswordSignIn { session: sess(0) },
        Authority::Password,
    );
    let first = |confirm| Event::FirstPasskey {
        passkey: pk(0),
        class: DEVICE,
        confirm,
    };
    let by = Authority::Session(sess(0));
    assert_eq!(w.try_do(first(None), by), Err(Refusal::NeedsSudo));
    assert_eq!(w.try_do(first(Some(cred(1))), by), Err(Refusal::NeedsSudo));
    w.ok(first(Some(cred(0))), by);
}

/// I-23 and §1: taking away works restricted, frozen and banned at once
#[test]
fn taking_away_passes_every_mode() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.wait(Duration::days(8));
    w.add_passkey(2, DEVICE, 0, 0);
    w.s.count = 60;
    w.s.flags.yubikey = true;
    assert!(w.s.restricted());
    let link = w.link();
    w.ok(Event::Freeze { link }, Authority::Email(addr_id(1)));
    w.ok(Event::Ban, Authority::Platform);
    assert!(w.s.restricted() && w.s.frozen().is_some() && w.s.banned());
    w.sign_in(5, 1);
    w.ok(Event::SignOutOthers, Authority::Session(sess(5)));
    w.ok(Event::LogWithdraw, tap(1, TapKind::LogPrivate));
    w.ok(Event::Suspend { target: p(0) }, tap(1, TapKind::Suspend));
    // ... but no sends and no adds
    assert!(
        w.try_do(
            Event::SendAdmit {
                send: send_id(0),
                recipients: agents(0, 1),
                when: When::Default
            },
            Authority::Session(sess(5)),
        )
        .is_err()
    );
    assert_eq!(
        w.try_do(
            Event::PasskeyRegister {
                passkey: pk(3),
                class: DEVICE,
            },
            Authority::Session(sess(5)),
        ),
        Err(Refusal::Mode(ModeClass::PasskeyAdd))
    );
}

/// The tier-1 guard exempts taking away only
#[test]
fn a_software_key_above_5_only_takes_away() {
    let mut w = registered(DEVICE);
    w.ok(
        Event::EnrolStart {
            enrolment: enrol(0),
            kind: FactorKind::SoftwareKey,
        },
        Authority::Anonymous,
    );
    w.ok(
        Event::EnrolApprove {
            enrolment: enrol(0),
            credential: cred(0),
        },
        tap(0, TapKind::EnrolmentApproval),
    );
    w.ok(
        Event::SendAdmit {
            send: send_id(0),
            recipients: agents(0, 1),
            when: When::At(t0() + Duration::days(1)),
        },
        Authority::Signed(cred(0)),
    );
    w.s.count = 6;
    w.s.flags.tier = true;
    assert_eq!(
        w.try_do(
            Event::SendAdmit {
                send: send_id(1),
                recipients: agents(0, 1),
                when: When::Default
            },
            Authority::Signed(cred(0)),
        ),
        Err(Refusal::TierOne)
    );
    w.ok(
        Event::SendCancel { send: send_id(0) },
        Authority::Signed(cred(0)),
    );
}

/// Decision 10: a YubiKey tap changes a setting at once, but never the
/// alarm channels
#[test]
fn a_yubikey_tap_skips_the_wait_except_for_the_alarm_channels() {
    let mut w = registered(YUBIKEY);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.ok(
        Event::ChangeRequest {
            change: change_id(0),
            setting: SettingChange::DmHold(Duration::minutes(5)),
        },
        tap(0, TapKind::PendingChange),
    );
    assert_eq!(w.s.settings.dm_hold, Duration::minutes(5));
    w.ok(
        Event::ChangeRequest {
            change: change_id(1),
            setting: SettingChange::PublishLog,
        },
        tap(0, TapKind::PendingChange),
    );
    assert!(!w.s.settings.action_log_public);
    // The requester can't cancel its own change; another passkey can
    assert_eq!(
        w.try_do(
            Event::ChangeCancel {
                change: change_id(1)
            },
            tap(0, TapKind::CancelPending)
        ),
        Err(Refusal::SameFactor)
    );
    w.ok(
        Event::ChangeCancel {
            change: change_id(1),
        },
        tap(1, TapKind::CancelPending),
    );
}

/// Above 5, adding an agent needs the minimum, whatever the flags
#[test]
fn agent_six_needs_the_minimum() {
    let mut w = registered(DEVICE);
    w.s.count = 5;
    assert_eq!(
        w.try_do(Event::AgentAdd, Authority::Session(sess(0))),
        Err(Refusal::Minimum(5))
    );
    w.add_passkey(1, SYNCED, 0, 0);
    w.contact(0);
    w.ok(Event::AgentAdd, Authority::Session(sess(0)));
    assert_eq!(w.s.count, 6);
}

/// The Steward, 2026-10-29: every send that takes a tap is announced,
/// including one that goes now with reach above 5 and immediate reach ≤ 5
#[test]
fn a_tapped_send_that_goes_now_is_announced() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.ok(
        Event::SendAdmit {
            send: send_id(0),
            recipients: agents(0, 6),
            when: When::Default,
        },
        tap(0, TapKind::Send),
    );
    assert_eq!(w.s.sends[&send_id(0)].delivery, Delivery::Held);
    w.ok(
        Event::SendAdmit {
            send: send_id(1),
            recipients: agents(100, 1),
            when: When::Now,
        },
        tap(1, TapKind::Send),
    );
    let sent = &w.s.sends[&send_id(1)];
    assert_eq!(sent.delivery, Delivery::Immediate);
    assert_eq!(
        sent.reach,
        Reach {
            reach: 7,
            immediate_reach: 1
        }
    );
    let notices: Vec<&Notice> = w
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notice(n) => Some(n),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.subject == NoticeSubject::SendTapped(send_id(1))
                && n.to == Audience::Both
                && n.freeze_link.map(|(_, f)| f) == Some(p(1)))
    );
}

/// 0x10: a counter-claim takes its own tap kind, not cancel-pending's; and
/// the losing side of round 2 can't contest (it is final)
#[test]
fn a_counter_claim_has_its_own_tap_and_round_2_is_final() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.add_passkey(2, DEVICE, 0, 0);
    w.ok(Event::Suspend { target: p(2) }, tap(1, TapKind::Suspend));
    w.ok(Event::Contest { factor: p(2) }, tap(2, TapKind::Contest));
    let case = *w.s.cases.keys().next().unwrap();
    w.ok(
        Event::RecoveryDecide {
            case,
            outcome: RecoveryOutcome::Reinstate,
            losing: None,
        },
        Authority::Staff,
    );
    assert_eq!(
        w.try_do(
            Event::RecoveryCancel { case },
            tap(1, TapKind::CancelPending)
        ),
        Err(Refusal::WrongTapKind)
    );
    w.ok(
        Event::RecoveryCancel { case },
        tap(1, TapKind::CounterClaim),
    );
    // Round 2: P1, the counter-claimant, loses
    w.ok(
        Event::RecoveryDecide {
            case,
            outcome: RecoveryOutcome::Reinstate,
            losing: Some(p(1)),
        },
        Authority::Staff,
    );
    assert!(matches!(
        w.state(p(1)),
        CredentialState::Suspended(Suspension {
            cause: SuspensionCause::Recovery { .. },
            ..
        })
    ));
    assert_eq!(
        w.try_do(Event::Contest { factor: p(1) }, tap(1, TapKind::Contest)),
        Err(Refusal::Final)
    );
    assert_eq!(
        w.try_do(
            Event::RecoveryCancel { case },
            tap(0, TapKind::CounterClaim)
        ),
        Err(Refusal::WrongState),
        "the second call's decision is final"
    );
}

/// Replacing the contact voids every unused freeze link sent to the old
/// address
#[test]
fn a_contact_change_voids_the_old_addresses_links() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.contact(0);
    w.add_passkey(2, DEVICE, 0, 0);
    let old = w.link();
    w.ok(
        Event::ChangeRequest {
            change: change_id(0),
            setting: SettingChange::EmergencyContact(Some(address(
                2,
                "other.example",
            ))),
        },
        tap(1, TapKind::PendingChange),
    );
    w.ok(
        Event::AddressVerify {
            address: addr_id(2),
        },
        Authority::Email(addr_id(2)),
    );
    w.wait(Duration::hours(72));
    w.ok(
        Event::ChangeEffective {
            change: change_id(0),
        },
        Authority::Platform,
    );
    let link = w.s.freeze_links[&old];
    assert!(link.void && !link.used);
}

/// One encoding of each setting's value (0x06); removing the contact is
/// the empty string
#[test]
fn each_setting_has_one_canonical_form() {
    let form = |c: SettingChange| c.canonical_form().unwrap();
    let both = |name: &str, value: &str| {
        super::challenge::change_canonical_form(name, value).unwrap()
    };
    assert_eq!(
        form(SettingChange::DmHold(Duration::minutes(2))),
        both("dm_hold", "120")
    );
    assert_eq!(
        form(SettingChange::BulkHold(Duration::hours(2))),
        both("bulk_hold", "7200")
    );
    assert_eq!(
        form(SettingChange::AgentReplyDailyCap(50)),
        both("agent_reply_daily_cap", "50")
    );
    assert_eq!(
        form(SettingChange::EmergencyContact(Some(address(
            2,
            "x.example"
        )))),
        both("emergency_contact", "a2@x.example")
    );
    assert_eq!(
        form(SettingChange::EmergencyContact(None)),
        b"\0\0\0\x11emergency_contact\0\0\0\0".to_vec()
    );
    assert_eq!(
        form(SettingChange::PrimaryEmail(address(3, "y.example"))),
        both("primary_email", "a3@y.example")
    );
    assert_eq!(
        form(SettingChange::PublishLog),
        both("action_log_public", "true")
    );
}

/// Hold values are bounded, so no hold runs past the end of time
#[test]
fn hold_changes_are_bounded() {
    let mut w = registered(DEVICE);
    for (i, setting) in [
        SettingChange::DmHold(Duration::days(2)),
        SettingChange::DmHold(Duration::seconds(-1)),
        SettingChange::BulkHold(Duration::minutes(59)),
        SettingChange::BulkHold(Duration::days(8)),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            w.try_do(
                Event::ChangeRequest {
                    change: change_id(i as u8),
                    setting
                },
                tap(0, TapKind::PendingChange)
            ),
            Err(Refusal::OutOfRange)
        );
    }
    let far = DateTime::<Utc>::MAX_UTC - Duration::seconds(30);
    let one = agents(0, 1);
    let mut late = request(&one, When::Default, TapLevel::None, &[]);
    late.now = far;
    assert_eq!(plan_send(&late), Err(SendRefusal::OutOfRange));
}

/// Automatic endings name no actor; the factor they concern is the target
#[test]
fn automatic_endings_log_their_target_not_an_actor() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.sign_in(1, 1);
    w.contact(0);
    w.ok(
        Event::SendAdmit {
            send: send_id(0),
            recipients: agents(0, 1),
            when: When::At(t0() + Duration::days(1)),
        },
        tap(1, TapKind::Send),
    );
    w.ok(Event::Suspend { target: p(1) }, tap(0, TapKind::Suspend));
    let logs: Vec<LogEntry> = w
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::Log(l) => Some(*l),
            _ => None,
        })
        .collect();
    for kind in [
        OperatorActionKind::SessionRevoke,
        OperatorActionKind::DmCancel,
    ] {
        let entry = logs.iter().find(|l| l.kind == kind).expect("logged");
        assert_eq!(
            (entry.by, entry.target),
            (Some(p(0)), Some(p(1))),
            "{kind:?}"
        );
    }
    // A freeze's cancellations have no actor
    w.add_passkey(2, DEVICE, 0, 0);
    let link = w.link();
    w.ok(
        Event::SendAdmit {
            send: send_id(1),
            recipients: agents(0, 1),
            when: When::At(t0() + Duration::days(1)),
        },
        Authority::Session(sess(0)),
    );
    w.ok(Event::Freeze { link }, Authority::Email(addr_id(1)));
    let cancel = w
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::Log(l) if l.kind == OperatorActionKind::DmCancel => {
                Some(*l)
            }
            _ => None,
        })
        .expect("cancelled");
    assert_eq!(cancel.by, None);
}

// ---------------------------------------------------------------------------
// Property tests: §11's invariants after every step
// ---------------------------------------------------------------------------

// -- the generator: events and authorities picked from the live state ---

fn passkey_class() -> impl Strategy<Value = PasskeyClass> {
    prop_oneof![Just(SYNCED), Just(DEVICE), Just(YUBIKEY)]
}

fn api_kind() -> impl Strategy<Value = FactorKind> {
    prop_oneof![
        Just(FactorKind::SoftwareKey),
        Just(UNATTENDED),
        Just(SUDO),
        Just(SUDO_PIN)
    ]
}

fn tap_kind() -> impl Strategy<Value = TapKind> {
    proptest::sample::select(TapKind::ALL.to_vec())
}

/// Any authority in the universe: noise, to exercise the refusals
fn any_authority() -> impl Strategy<Value = Authority> {
    prop_oneof![
        (0u8..3).prop_map(|i| Authority::Email(addr_id(i))),
        (0u8..4).prop_map(|i| Authority::Session(sess(i))),
        Just(Authority::Password),
        (0u8..4).prop_map(|i| Authority::Signed(cred(i))),
        (0u8..4, tap_kind()).prop_map(|(i, k)| tap(i, k)),
        (0u8..4).prop_map(|i| Authority::Assertion(pk(i))),
        Just(Authority::Anonymous),
        Just(Authority::Agent),
        Just(Authority::Staff),
        Just(Authority::Platform),
    ]
}

fn recipients() -> impl Strategy<Value = BTreeSet<AgentId>> {
    (
        0u32..600,
        proptest::sample::select(vec![1u32, 2, 5, 6, 400, 501]),
    )
        .prop_map(|(start, len)| agents(start, len))
}

fn when() -> impl Strategy<Value = When> {
    prop_oneof![
        Just(When::Default),
        Just(When::Now),
        (0i64..60).prop_map(|h| When::At(t0() + Duration::hours(h))),
    ]
}

fn setting() -> impl Strategy<Value = SettingChange> {
    prop_oneof![
        (0i64..3).prop_map(|m| SettingChange::DmHold(Duration::minutes(m))),
        (0i64..3).prop_map(|h| SettingChange::BulkHold(Duration::hours(h))),
        (0u32..100).prop_map(SettingChange::AgentReplyDailyCap),
        Just(SettingChange::EmergencyContact(None)),
        (
            3u8..6,
            prop_oneof![Just("primary.example"), Just("other.example")]
        )
            .prop_map(|(i, t)| SettingChange::EmergencyContact(Some(
                address(i, t)
            ))),
        (
            3u8..6,
            prop_oneof![Just("contact.example"), Just("third.example")]
        )
            .prop_map(|(i, t)| SettingChange::PrimaryEmail(address(i, t))),
        Just(SettingChange::PublishLog),
    ]
}

fn wait() -> impl Strategy<Value = Duration> {
    prop_oneof![
        4 => Just(Duration::zero()),
        2 => Just(Duration::minutes(2)),
        2 => Just(Duration::hours(1)),
        1 => Just(Duration::hours(25)),
        1 => Just(Duration::hours(73)),
        1 => Just(Duration::days(8)),
        1 => Just(Duration::days(31)),
    ]
}

/// An index into a list the state provides at run time, modulo its length
type Pick = usize;

fn pick() -> impl Strategy<Value = Pick> {
    0usize..6
}

/// An authority, resolved against the state
#[derive(Debug, Clone)]
enum AuthGen {
    /// A passkey's tap of this kind (active passkeys first)
    Tap(TapKind, Pick),
    /// A session (open ones first)
    Session(Pick),
    /// An API credential's signature (active ones first)
    Signed(Pick),
    /// A passkey's sign-in assertion
    Assertion(Pick),
    /// The event's own factor: a contest by the suspended factor
    Own,
    Contact,
    Primary,
    /// An address a pending change brings in
    NewAddress(Pick),
    Fixed(Authority),
}

/// An event, its ids resolved against the state
#[derive(Debug, Clone)]
enum EventGen {
    Fixed(Event),
    /// Register a passkey in a session and approve it with a tap: two
    /// events, each checked
    AddPasskey(u8, PasskeyClass, Pick),
    /// Start an enrolment and approve it with a tap
    AddKey(u8, FactorKind, Pick),
    /// Whatever the state invites: a frozen account's moves, a decided
    /// case's counter-claim, a held send's cancel link, …
    Smart(Pick, Pick),
    /// A pending passkey
    Approve,
    Suspend(Pick),
    Contest(Pick),
    Deadline(Pick),
    Freeze(Pick),
    Decide(Pick, RecoveryOutcome, Option<Pick>),
    CounterClaim(Pick),
    RecoveryEffective(Pick),
    ChangeCancel(Pick),
    ChangeEffective(Pick),
    Verify(Pick),
    Deliver(Pick),
    Cancel(Pick),
    Retract(Pick),
    Read(Pick),
    Expire(Pick),
}

fn by_tap(kind: TapKind) -> impl Strategy<Value = AuthGen> {
    prop_oneof![
        8 => pick().prop_map(move |i| AuthGen::Tap(kind, i)),
        1 => (pick(), tap_kind()).prop_map(|(i, k)| AuthGen::Tap(k, i)),
        1 => any_authority().prop_map(AuthGen::Fixed),
    ]
}

fn by_session() -> impl Strategy<Value = AuthGen> {
    prop_oneof![
        8 => pick().prop_map(AuthGen::Session),
        1 => any_authority().prop_map(AuthGen::Fixed),
    ]
}

fn by(a: Authority) -> impl Strategy<Value = AuthGen> {
    prop_oneof![
        9 => Just(AuthGen::Fixed(a)),
        1 => any_authority().prop_map(AuthGen::Fixed),
    ]
}

fn by_sender() -> impl Strategy<Value = AuthGen> {
    prop_oneof![
        3 => pick().prop_map(|i| AuthGen::Tap(TapKind::Send, i)),
        2 => pick().prop_map(AuthGen::Session),
        2 => pick().prop_map(AuthGen::Signed),
        1 => any_authority().prop_map(AuthGen::Fixed),
    ]
}

fn gen_step() -> impl Strategy<Value = (Duration, EventGen, AuthGen)> {
    use EventGen as G;
    let fixed = G::Fixed;
    let platform = Authority::Platform;
    let steps = prop_oneof![
        40 => (0usize..64, pick()).prop_map(|(i, j)| (G::Smart(i, j), AuthGen::Fixed(Authority::Platform))),
        // §2
        5 => (1u8..6, passkey_class(), pick(), by_tap(TapKind::PasskeyEnrolment))
            .prop_map(|(i, class, session, a)| (G::AddPasskey(i, class, session), a)),
        3 => (0u8..4, api_kind(), pick(), by_tap(TapKind::EnrolmentApproval))
            .prop_map(|(i, kind, e, a)| (G::AddKey(i, kind, e), a)),
        3 => (1u8..6, passkey_class(), by_session())
            .prop_map(move |(i, class, a)| (fixed(Event::PasskeyRegister { passkey: pk(i), class }), a)),
        3 => by_tap(TapKind::PasskeyEnrolment).prop_map(|a| (G::Approve, a)),
        1 => (pick(), by(platform)).prop_map(move |(i, a)| (G::Expire(i), a)),
        2 => (0u8..4, passkey_class(), proptest::option::of(0u8..4), by_session())
            .prop_map(move |(i, class, k, a)| {
                (fixed(Event::FirstPasskey { passkey: pk(i), class, confirm: k.map(cred) }), a)
            }),
        5 => (pick(), by_tap(TapKind::Suspend)).prop_map(|(i, a)| (G::Suspend(i), a)),
        2 => (pick(), prop_oneof![5 => Just(AuthGen::Own), 1 => any_authority().prop_map(AuthGen::Fixed)])
            .prop_map(|(i, a)| (G::Contest(i), a)),
        2 => (pick(), by(platform)).prop_map(|(i, a)| (G::Deadline(i), a)),
        // §3
        2 => (0u8..3, api_kind(), by(Authority::Anonymous))
            .prop_map(move |(i, kind, a)| (fixed(Event::EnrolStart { enrolment: enrol(i), kind }), a)),
        2 => (0u8..3, 0u8..4, by_tap(TapKind::EnrolmentApproval)).prop_map(move |(i, k, a)| {
            (fixed(Event::EnrolApprove { enrolment: enrol(i), credential: cred(k) }), a)
        }),
        1 => (0u8..3, by_session()).prop_map(move |(i, a)| (fixed(Event::EnrolRefuse { enrolment: enrol(i) }), a)),
        1 => (0u8..3, by(platform)).prop_map(move |(i, a)| (fixed(Event::EnrolExpire { enrolment: enrol(i) }), a)),
        // §4.3
        2 => by_session().prop_map(move |a| (fixed(Event::AgentAdd), a)),
        1 => by_session().prop_map(move |a| (fixed(Event::AgentRestore), a)),
        1 => by_tap(TapKind::AgentDeletion).prop_map(move |a| (fixed(Event::AgentDelete { agent: agent(0) }), a)),
        1 => by(platform).prop_map(move |a| (fixed(Event::AgentGraceEnd), a)),
        1 => (prop_oneof![Just(Flag::Tier), Just(Flag::Minimum), Just(Flag::Yubikey)], by(platform))
            .prop_map(move |(f, a)| (fixed(Event::FlagOn(f)), a)),
        1 => by(platform).prop_map(move |a| (fixed(Event::Cutover), a)),
        1 => (0u8..6, by(Authority::Password))
            .prop_map(move |(i, a)| (fixed(Event::PasswordSignIn { session: sess(i) }), a)),
        1 => by(platform).prop_map(move |a| (fixed(Event::Ban), a)),
        1 => by(platform).prop_map(move |a| (fixed(Event::Unban), a)),
        2 => (0u32..2, by_tap(TapKind::OAuthConsent)).prop_map(move |(i, a)| {
            (fixed(Event::OAuthConsent { client: "client".parse().unwrap(), agent: agent(i) }), a)
        }),
        1 => (0u32..2, by_session()).prop_map(move |(i, a)| {
            (fixed(Event::ConsentRevoke { client: "client".parse().unwrap(), agent: agent(i) }), a)
        }),
        1 => by_tap(TapKind::LogPrivate).prop_map(move |a| (fixed(Event::LogWithdraw), a)),
        // §5
        3 => (0u8..4, setting(), by_tap(TapKind::PendingChange)).prop_map(move |(i, setting, a)| {
            (fixed(Event::ChangeRequest { change: change_id(i), setting }), a)
        }),
        1 => (pick(), by_tap(TapKind::CancelPending)).prop_map(|(i, a)| (G::ChangeCancel(i), a)),
        2 => (pick(), by(platform)).prop_map(|(i, a)| (G::ChangeEffective(i), a)),
        // §7
        2 => (1u8..3, prop_oneof![4 => Just("contact.example"), 1 => Just("primary.example")],
              prop_oneof![
                  3 => pick().prop_map(|i| AuthGen::Tap(TapKind::FirstContact, i)),
                  1 => pick().prop_map(AuthGen::Signed),
                  1 => any_authority().prop_map(AuthGen::Fixed)
              ])
            .prop_map(move |(i, t, a)| (fixed(Event::ContactSetFirst { address: address(i, t) }), a)),
        2 => (pick(), prop_oneof![
                  2 => Just(AuthGen::Contact),
                  2 => pick().prop_map(AuthGen::NewAddress),
                  1 => any_authority().prop_map(AuthGen::Fixed)
              ])
            .prop_map(|(i, a)| (G::Verify(i), a)),
        1 => by_tap(TapKind::ReArm).prop_map(move |a| (fixed(Event::ReArm), a)),
        3 => (pick(), prop_oneof![
                  5 => Just(AuthGen::Contact),
                  1 => Just(AuthGen::Primary),
                  1 => any_authority().prop_map(AuthGen::Fixed)
              ])
            .prop_map(|(i, a)| (G::Freeze(i), a)),
        2 => by_tap(TapKind::Unfreeze).prop_map(move |a| (fixed(Event::Unfreeze), a)),
        1 => by_tap(TapKind::CompromiseNotice).prop_map(move |a| (fixed(Event::CompromiseNotice), a)),
        // §6
        4 => (0u8..6, recipients(), when(), by_sender()).prop_map(move |(i, recipients, when, a)| {
            (fixed(Event::SendAdmit { send: send_id(i), recipients, when }), a)
        }),
        2 => (pick(), by(platform)).prop_map(|(i, a)| (G::Deliver(i), a)),
        1 => (pick(), prop_oneof![
                  2 => pick().prop_map(AuthGen::Session),
                  1 => pick().prop_map(AuthGen::Signed),
                  2 => Just(AuthGen::Primary),
                  1 => any_authority().prop_map(AuthGen::Fixed)
              ])
            .prop_map(|(i, a)| (G::Cancel(i), a)),
        1 => (pick(), by_session()).prop_map(|(i, a)| (G::Retract(i), a)),
        1 => (pick(), by(Authority::Agent)).prop_map(|(i, a)| (G::Read(i), a)),
        // §9
        1 => by(Authority::Staff).prop_map(move |a| (fixed(Event::RecoveryRequest), a)),
        2 => (pick(),
              prop_oneof![
                  Just(RecoveryOutcome::Reinstate),
                  Just(RecoveryOutcome::Revoke),
                  (6u8..8, passkey_class())
                      .prop_map(|(i, class)| RecoveryOutcome::NewPasskey { passkey: pk(i), class })
              ],
              proptest::option::weighted(0.3, pick()),
              by(Authority::Staff))
            .prop_map(|(i, o, l, a)| (G::Decide(i, o, l), a)),
        1 => (pick(), by_tap(TapKind::CounterClaim)).prop_map(|(i, a)| (G::CounterClaim(i), a)),
        2 => (pick(), by(platform)).prop_map(|(i, a)| (G::RecoveryEffective(i), a)),
        // §10
        3 => (0u8..6, pick()).prop_map(move |(s, i)| {
            (fixed(Event::SignIn { session: sess(s), passkey: pk(0) }), AuthGen::Assertion(i))
        }),
        1 => (pick(), by(platform)).prop_map(|(i, a)| (G::Expire(i), a)),
        1 => by_session().prop_map(move |a| (fixed(Event::SignOutOthers), a)),
    ];
    (wait(), steps).prop_map(|(w, (e, a))| (w, e, a))
}

/// `list[i % len]`
fn nth<T: Copy>(list: &[T], i: Pick) -> Option<T> {
    (!list.is_empty()).then(|| list[i % list.len()])
}

/// Keys of `map`, those matching `first` before the rest
fn ordered<K: Copy, V>(
    map: &BTreeMap<K, V>,
    keep: impl Fn(&K) -> bool,
    first: impl Fn(&V) -> bool,
) -> Vec<K> {
    let (mut a, b): (Vec<_>, Vec<_>) = map
        .iter()
        .filter(|(k, _)| keep(k))
        .partition(|(_, v)| first(v));
    a.extend(b);
    a.into_iter().map(|(k, _)| *k).collect()
}

fn passkey_of(f: FactorId) -> Option<OperatorPasskeyId> {
    match f {
        FactorId::Passkey(p) => Some(p),
        FactorId::Credential(_) => None,
    }
}

fn credential_of(f: FactorId) -> Option<OperatorCredentialId> {
    match f {
        FactorId::Credential(c) => Some(c),
        FactorId::Passkey(_) => None,
    }
}

/// The steps the state invites, each with a plausible authority; `j` picks
/// among the factors and items where there is a choice
fn invited(s: &AccountState, j: Pick) -> Vec<Vec<(Event, Authority)>> {
    let mut groups: Vec<Vec<(Event, Authority)>> = Vec::new();
    let mut out = Vec::new();
    let active_passkeys: Vec<OperatorPasskeyId> = s
        .factors
        .iter()
        .filter(|(_, f)| f.state.is_active())
        .filter_map(|(id, _)| passkey_of(*id))
        .collect();
    let tapper = nth(&active_passkeys, j);
    let tap_by =
        |p: OperatorPasskeyId, kind| Authority::PasskeyTap { passkey: p, kind };
    let session = s
        .sessions
        .iter()
        .filter(|(_, x)| x.open)
        .map(|(id, _)| *id)
        .nth(j % s.sessions.len().max(1))
        .or_else(|| s.sessions.iter().find(|(_, x)| x.open).map(|(id, _)| *id));
    groups.push(std::mem::take(&mut out));
    // A frozen account's moves, by every active passkey (the eligible one
    // among them)
    if let Some(frozen) = s.frozen() {
        for p in &active_passkeys {
            if s.eligible(FactorId::Passkey(*p), frozen.excluded, frozen.at) {
                // The compromise notice first, then the unfreeze
                if frozen.notice_sent || s.banned() {
                    out.push((Event::Unfreeze, tap_by(*p, TapKind::Unfreeze)));
                } else {
                    out.push((
                        Event::CompromiseNotice,
                        tap_by(*p, TapKind::CompromiseNotice),
                    ));
                }
                groups.push(std::mem::take(&mut out));
            }
        }
    }
    if let (Some(c), Some(p)) = (&s.contact, tapper)
        && c.power == FreezePower::Disarmed
    {
        out.push((Event::ReArm, tap_by(p, TapKind::ReArm)));
    }
    groups.push(std::mem::take(&mut out));
    // An unused freeze link
    if s.frozen().is_none()
        && let Some(c) = &s.contact
    {
        for (id, l) in &s.freeze_links {
            if !l.used && l.address == c.address.id {
                out.push((
                    Event::Freeze { link: *id },
                    Authority::Email(c.address.id),
                ));
                break;
            }
        }
    }
    groups.push(std::mem::take(&mut out));
    // Recovery: decide, counter-claim, round 2 with a loser, take effect
    for (case, c) in &s.cases {
        match (c.state, c.subject) {
            (CaseState::Open { round }, CaseSubject::Contest(_)) => {
                let outcome = if j.is_multiple_of(2) {
                    RecoveryOutcome::Reinstate
                } else {
                    RecoveryOutcome::Revoke
                };
                // In round 2, a loser that is still active, or none
                let losers: Vec<FactorId> = c
                    .effective_at_open
                    .iter()
                    .copied()
                    .filter(|f| s.effective(*f))
                    .collect();
                let losing = (round == 2 && !j.is_multiple_of(5))
                    .then(|| nth(&losers, j))
                    .flatten();
                out.push((
                    Event::RecoveryDecide {
                        case: *case,
                        outcome,
                        losing,
                    },
                    Authority::Staff,
                ));
            }
            (CaseState::Open { .. }, CaseSubject::NoFactor) => out.push((
                Event::RecoveryDecide {
                    case: *case,
                    outcome: RecoveryOutcome::NewPasskey {
                        passkey: pk(7),
                        class: DEVICE,
                    },
                    losing: None,
                },
                Authority::Staff,
            )),
            (CaseState::Decided { round: 1, .. }, _) => {
                // A counter-claim (three times in four), or the outcome
                // taking effect
                let claimant = active_passkeys.iter().find(|p| {
                    c.effective_at_open.contains(&FactorId::Passkey(**p))
                });
                match claimant {
                    Some(p) if !j.is_multiple_of(4) => out.push((
                        Event::RecoveryCancel { case: *case },
                        tap_by(*p, TapKind::CounterClaim),
                    )),
                    _ => out.push((
                        Event::RecoveryEffective { case: *case },
                        Authority::Platform,
                    )),
                }
            }
            (CaseState::Decided { .. }, _) => {
                out.push((
                    Event::RecoveryEffective { case: *case },
                    Authority::Platform,
                ));
            }
            _ => {}
        }
    }
    groups.push(std::mem::take(&mut out));
    // A suspended factor contests
    for (id, f) in &s.factors {
        if let CredentialState::Suspended(_) = f.state {
            let by = match id {
                FactorId::Passkey(p) => tap_by(*p, TapKind::Contest),
                FactorId::Credential(c) => Authority::Signed(*c),
            };
            out.push((Event::Contest { factor: *id }, by));
            break;
        }
    }
    groups.push(std::mem::take(&mut out));
    // Sends: the cancel link of a held one, a retract, a delivery
    for (id, m) in &s.sends {
        match m.state {
            SendState::Scheduled
                if m.delivery == Delivery::Held
                    && m.reach.reach > UNTAPPED_REACH =>
            {
                out.push((
                    Event::SendCancel { send: *id },
                    Authority::Email(s.primary.id),
                ));
            }
            SendState::Delivered => {
                if let Some(x) = session {
                    out.push((
                        Event::Retract { send: *id },
                        Authority::Session(x),
                    ));
                }
            }
            _ => {}
        }
    }
    groups.push(std::mem::take(&mut out));
    // A change requested by a plain passkey (one that waits), cancelled by
    // another, or taking effect
    if !s.changes.values().any(|c| c.state == ChangeState::Pending)
        && let Some(p) = active_passkeys.iter().find(|p| {
            s.factor(FactorId::Passkey(**p))
                .is_some_and(|f| !f.kind.is_yubikey_passkey())
        })
    {
        out.push((
            Event::ChangeRequest {
                change: change_id(20 + (j % 8) as u8),
                setting: SettingChange::AgentReplyDailyCap(j as u32),
            },
            tap_by(*p, TapKind::PendingChange),
        ));
    }
    for (id, c) in &s.changes {
        if c.state != ChangeState::Pending {
            continue;
        }
        let other = active_passkeys
            .iter()
            .find(|p| FactorId::Passkey(**p) != c.requested_by);
        match other {
            Some(p) if j.is_multiple_of(2) => out.push((
                Event::ChangeCancel { change: *id },
                tap_by(*p, TapKind::CancelPending),
            )),
            _ => out.push((
                Event::ChangeEffective { change: *id },
                Authority::Platform,
            )),
        }
    }
    groups.push(std::mem::take(&mut out));
    // A factor that added others, suspended by an active passkey outside
    // what it added: a cascade
    for id in s.factors.keys() {
        if !s.effective(*id) || !s.children(*id).any(|c| s.effective(c)) {
            continue;
        }
        let below = s.desc(*id);
        if let Some(p) = active_passkeys.iter().find(|p| {
            let f = FactorId::Passkey(**p);
            f != *id && !below.contains(&f)
        }) {
            out.push((
                Event::Suspend { target: *id },
                tap_by(*p, TapKind::Suspend),
            ));
            break;
        }
    }
    groups.push(std::mem::take(&mut out));
    // A tapped send above 5, held; and, beside a held one, a chat with a new
    // agent that goes now (reach above 5, immediate reach 1: I-40's case)
    if let Some(p) = tapper {
        out.push((
            Event::SendAdmit {
                send: send_id(30 + (j % 4) as u8),
                recipients: agents(2000 + 10 * j as u32, 6),
                when: When::Default,
            },
            tap_by(p, TapKind::Send),
        ));
        if s.sends.values().any(|m| {
            m.state == SendState::Scheduled && m.reach.reach > UNTAPPED_REACH
        }) {
            groups.push(std::mem::take(&mut out));
            out.push((
                Event::SendAdmit {
                    send: send_id(40 + (j % 4) as u8),
                    recipients: agents(3000 + j as u32, 1),
                    when: When::Now,
                },
                tap_by(p, TapKind::Send),
            ));
        }
    }
    groups.push(std::mem::take(&mut out));
    // A send to more than 500, by a YubiKey passkey
    if let Some(yk) = s
        .factors
        .iter()
        .find(|(_, f)| f.state.is_active() && f.kind.is_yubikey_passkey())
        .and_then(|(id, _)| passkey_of(*id))
    {
        out.push((
            Event::SendAdmit {
                send: send_id(10 + (j % 4) as u8),
                recipients: agents(1000 * (j as u32 % 3), 501),
                when: When::Default,
            },
            tap_by(yk, TapKind::Send),
        ));
    }
    groups.push(std::mem::take(&mut out));
    // The contact, where there is none
    if s.contact.is_none()
        && let Some(p) = tapper
    {
        out.push((
            Event::ContactSetFirst {
                address: address(1, "contact.example"),
            },
            tap_by(p, TapKind::FirstContact),
        ));
    }
    if let Some(c) = &s.contact
        && c.state == ContactState::Unverified
    {
        out.push((
            Event::AddressVerify {
                address: c.address.id,
            },
            Authority::Email(c.address.id),
        ));
    }
    groups.push(out);
    groups.retain(|g| !g.is_empty());
    groups
}

/// Resolve a generated step against the state
fn resolve(
    s: &AccountState,
    event: &EventGen,
    auth: &AuthGen,
) -> Option<Vec<(Event, Authority)>> {
    use EventGen as G;
    let open_sessions = ordered(&s.sessions, |_| true, |x| x.open);
    match event {
        G::AddPasskey(i, class, session) => {
            let approve = resolve(
                s,
                &G::Fixed(Event::PasskeyApprove { passkey: pk(*i) }),
                auth,
            )?;
            let register = (
                Event::PasskeyRegister {
                    passkey: pk(*i),
                    class: *class,
                },
                Authority::Session(nth(&open_sessions, *session)?),
            );
            return Some([vec![register], approve].concat());
        }
        G::AddKey(i, kind, e) => {
            let enrolment = enrol(*e as u8 + 10 * *i);
            let approve = resolve(
                s,
                &G::Fixed(Event::EnrolApprove {
                    enrolment,
                    credential: cred(*i),
                }),
                auth,
            )?;
            let start = (
                Event::EnrolStart {
                    enrolment,
                    kind: *kind,
                },
                Authority::Anonymous,
            );
            return Some([vec![start], approve].concat());
        }
        G::Smart(i, j) => {
            let groups = invited(s, *j);
            let n = groups.len();
            if n == 0 {
                return None;
            }
            let group = &groups[*i % n];
            return Some(vec![group[*j % group.len()].clone()]);
        }
        _ => {}
    }
    let active = |f: &Factor| f.state.is_active();
    let suspended =
        |f: &Factor| matches!(f.state, CredentialState::Suspended(_));
    let any_factor = ordered(&s.factors, |_| true, active);
    let event = match event {
        G::Fixed(e) => e.clone(),
        G::AddPasskey(..) | G::AddKey(..) | G::Smart(..) => {
            unreachable!("expanded above")
        }
        G::Approve => {
            let pending = ordered(
                &s.factors,
                |_| true,
                |f| matches!(f.state, CredentialState::Pending { .. }),
            );
            Event::PasskeyApprove {
                passkey: pending.into_iter().find_map(passkey_of)?,
            }
        }
        G::Suspend(i) => {
            // Factors that added others first, so cascades happen
            let (mut parents, rest): (Vec<_>, Vec<_>) = any_factor
                .iter()
                .partition(|f| s.children(**f).any(|c| s.effective(c)));
            parents.extend(rest);
            Event::Suspend {
                target: nth(&parents, *i)?,
            }
        }
        G::Contest(i) => Event::Contest {
            factor: nth(&ordered(&s.factors, |_| true, suspended), *i)?,
        },
        G::Deadline(i) => Event::Deadline {
            factor: nth(&ordered(&s.factors, |_| true, suspended), *i)?,
        },
        G::Freeze(i) => Event::Freeze {
            link: nth(&ordered(&s.freeze_links, |_| true, |l| !l.used), *i)?,
        },
        G::Decide(i, outcome, losing) => Event::RecoveryDecide {
            case: nth(&s.cases.keys().copied().collect::<Vec<_>>(), *i)?,
            outcome: *outcome,
            losing: losing.and_then(|l| nth(&any_factor, l)),
        },
        G::CounterClaim(i) => Event::RecoveryCancel {
            case: nth(&s.cases.keys().copied().collect::<Vec<_>>(), *i)?,
        },
        G::RecoveryEffective(i) => Event::RecoveryEffective {
            case: nth(&s.cases.keys().copied().collect::<Vec<_>>(), *i)?,
        },
        G::ChangeCancel(i) | G::ChangeEffective(i) => {
            let change = nth(
                &ordered(
                    &s.changes,
                    |_| true,
                    |c| c.state == ChangeState::Pending,
                ),
                *i,
            )?;
            match event {
                G::ChangeCancel(_) => Event::ChangeCancel { change },
                _ => Event::ChangeEffective { change },
            }
        }
        G::Verify(i) => {
            let mut addresses: Vec<_> =
                s.contact.iter().map(|c| c.address.id).collect();
            addresses.extend(
                s.changes
                    .values()
                    .filter_map(|c| c.change.new_address().map(|a| a.id)),
            );
            Event::AddressVerify {
                address: nth(&addresses, *i)?,
            }
        }
        G::Deliver(i) | G::Cancel(i) => {
            let send = nth(
                &ordered(
                    &s.sends,
                    |_| true,
                    |m| m.state == SendState::Scheduled,
                ),
                *i,
            )?;
            match event {
                G::Deliver(_) => Event::SendDeliver { send },
                _ => Event::SendCancel { send },
            }
        }
        G::Retract(i) | G::Read(i) => {
            let send = nth(
                &ordered(
                    &s.sends,
                    |_| true,
                    |m| m.state == SendState::Delivered,
                ),
                *i,
            )?;
            match event {
                G::Retract(_) => Event::Retract { send },
                _ => Event::Read { send },
            }
        }
        G::Expire(i) => {
            // A pending passkey's ceremony, or a session
            let pending: Vec<_> = s
                .factors
                .iter()
                .filter(|(_, f)| {
                    matches!(f.state, CredentialState::Pending { .. })
                })
                .filter_map(|(id, _)| passkey_of(*id))
                .collect();
            match nth(&pending, *i) {
                Some(passkey) if i % 2 == 0 => Event::PendingExpire { passkey },
                _ => Event::SessionExpire {
                    session: nth(
                        &ordered(&s.sessions, |_| true, |x| x.open),
                        *i,
                    )?,
                },
            }
        }
    };
    let passkeys = ordered(&s.factors, |f| passkey_of(*f).is_some(), active);
    let credentials =
        ordered(&s.factors, |f| credential_of(*f).is_some(), active);
    let authority = match auth {
        AuthGen::Tap(kind, i) => Authority::PasskeyTap {
            passkey: passkey_of(nth(&passkeys, *i)?)?,
            kind: *kind,
        },
        AuthGen::Session(i) => Authority::Session(nth(
            &ordered(&s.sessions, |_| true, |x| x.open),
            *i,
        )?),
        AuthGen::Signed(i) => {
            Authority::Signed(credential_of(nth(&credentials, *i)?)?)
        }
        AuthGen::Assertion(i) => {
            let passkey = passkey_of(nth(&passkeys, *i)?)?;
            return Some(vec![match event {
                Event::SignIn { session, .. } => (
                    Event::SignIn { session, passkey },
                    Authority::Assertion(passkey),
                ),
                other => (other, Authority::Assertion(passkey)),
            }]);
        }
        AuthGen::Own => match &event {
            Event::Contest {
                factor: FactorId::Passkey(p),
            } => Authority::PasskeyTap {
                passkey: *p,
                kind: TapKind::Contest,
            },
            Event::Contest {
                factor: FactorId::Credential(c),
            } => Authority::Signed(*c),
            _ => Authority::Platform,
        },
        AuthGen::Contact => Authority::Email(s.contact.as_ref()?.address.id),
        AuthGen::Primary => Authority::Email(s.primary.id),
        AuthGen::NewAddress(i) => {
            let addresses: Vec<_> = s
                .changes
                .values()
                .filter_map(|c| c.change.new_address().map(|a| a.id))
                .collect();
            Authority::Email(nth(&addresses, *i)?)
        }
        AuthGen::Fixed(a) => *a,
    };
    Some(vec![(event, authority)])
}

/// A starting account: migrated with one or two passkeys, or with a
/// password; with some of the four kinds of API key
fn start() -> impl Strategy<Value = AccountState> {
    (
        prop_oneof![Just(None), passkey_class().prop_map(Some)],
        proptest::option::of(passkey_class()),
        proptest::collection::vec(any::<bool>(), 4),
        any::<bool>(),
        proptest::sample::select(vec![0u32, 5, 6, 50, 51]),
        (any::<bool>(), any::<bool>(), any::<bool>()),
        0u8..3,
    )
        .prop_map(
            |(
                first,
                second,
                keys,
                old,
                count,
                (tier, minimum, yubikey),
                contact,
            )| {
                let added = if old { t0() - Duration::days(10) } else { t0() };
                let kinds =
                    [FactorKind::SoftwareKey, UNATTENDED, SUDO, SUDO_PIN];
                let mut factors: Vec<_> = keys
                    .iter()
                    .enumerate()
                    .filter(|(_, on)| **on)
                    .map(|(i, _)| (c(i as u8), kinds[i], added))
                    .collect();
                let auth = match first {
                    Some(class) => {
                        factors.push((p(0), FactorKind::Passkey(class), added));
                        if let Some(class) = second {
                            factors.push((
                                p(1),
                                FactorKind::Passkey(class),
                                added,
                            ));
                        }
                        AuthMode::Passkeys
                    }
                    None => AuthMode::PasswordOnly,
                };
                let mut s = AccountState::migrated(
                    OperatorId::from(uuid(0, 1)),
                    address(0, "primary.example"),
                    auth,
                    factors,
                    2,
                );
                s.count = count;
                s.flags = Flags {
                    tier,
                    minimum,
                    yubikey,
                    cutover: false,
                };
                s.contact = match contact {
                    0 => None,
                    1 => Some(EmergencyContact {
                        address: address(1, "contact.example"),
                        state: ContactState::Verified,
                        power: FreezePower::Armed,
                    }),
                    _ => Some(EmergencyContact {
                        address: address(1, "contact.example"),
                        state: ContactState::Unverified,
                        power: FreezePower::Armed,
                    }),
                };
                if let Some(PasskeyClass { .. }) = first {
                    s.factors.get_mut(&p(0)).unwrap().signed_in = true;
                    s.sessions.insert(
                        sess(0),
                        Session {
                            opener: SessionOpener::Passkey(pk(0)),
                            last_used: t0(),
                            open: true,
                        },
                    );
                } else {
                    s.sessions.insert(
                        sess(0),
                        Session {
                            opener: SessionOpener::Password,
                            last_used: t0(),
                            open: true,
                        },
                    );
                }
                s
            },
        )
}

/// What the harness remembers across steps
#[derive(Default)]
struct History {
    /// Every freeze, by address
    freezes: BTreeMap<OperatorEmailAddressId, Vec<DateTime<Utc>>>,
    /// When each factor last became suspended, seen by the harness
    suspended_at: BTreeMap<FactorId, DateTime<Utc>>,
}

impl History {
    fn record(&mut self, a: &Applied) {
        for (id, before, after) in a.changed() {
            let was = before.is_some_and(|b| b.state.suspension().is_some());
            if !was && after.state.suspension().is_some() {
                self.suspended_at.insert(id, a.now);
            }
        }
    }
}

/// Which step applied, for the invariants
struct Applied<'a> {
    pre: &'a AccountState,
    post: &'a AccountState,
    event: &'a Event,
    by: Authority,
    now: DateTime<Utc>,
    effects: &'a [Effect],
}

impl Applied<'_> {
    fn changed(
        &self,
    ) -> impl Iterator<Item = (FactorId, Option<&Factor>, &Factor)> {
        self.post
            .factors
            .iter()
            .map(|(id, f)| (*id, self.pre.factor(*id), f))
            .filter(|(_, before, after)| {
                before.map(|b| b.state) != Some(after.state)
            })
    }

    /// Factors added: absent or pending before, active now
    fn added(&self) -> Vec<FactorId> {
        self.changed()
            .filter(|(_, before, after)| {
                after.state.is_active()
                    && before.is_none_or(|b| {
                        matches!(b.state, CredentialState::Pending { .. })
                    })
            })
            .map(|(id, _, _)| id)
            .collect()
    }

    fn newly_suspended(&self) -> BTreeSet<FactorId> {
        self.changed()
            .filter(|(_, before, after)| {
                before.is_some_and(|b| b.state.is_active())
                    && matches!(after.state, CredentialState::Suspended(_))
            })
            .map(|(id, _, _)| id)
            .collect()
    }

    fn notices(&self) -> impl Iterator<Item = &Notice> {
        self.effects.iter().filter_map(|e| match e {
            Effect::Notice(n) => Some(n),
            _ => None,
        })
    }

    fn new_sends(
        &self,
    ) -> impl Iterator<Item = (&OperatorActionId, &super::Send)> {
        self.post
            .sends
            .iter()
            .filter(|(id, _)| !self.pre.sends.contains_key(id))
    }
}

fn i1_and_i3_the_cascade(a: &Applied) -> Result<(), TestCaseError> {
    let (Event::Suspend { target }, Authority::PasskeyTap { passkey, .. }) =
        (a.event, a.by)
    else {
        return Ok(());
    };
    let s = FactorId::Passkey(passkey);
    let before = a.pre.factor(*target).unwrap().state;
    let newly = a.newly_suspended();
    match before {
        CredentialState::Active => {
            // I-3: exactly cascade(T, S)
            prop_assert_eq!(&newly, &a.pre.cascade(*target, Some(s)));
        }
        CredentialState::Suspended(Suspension {
            cause: SuspensionCause::Freeze { .. },
            ..
        }) => {
            let mut expected = a.pre.cascade(*target, Some(s));
            expected.remove(target);
            prop_assert_eq!(&newly, &expected);
        }
        _ => prop_assert!(newly.is_empty()),
    }
    // I-1: S, and whatever is reached from T only through S, is untouched
    // Provenance is a forest, so a path from T runs through S only when S
    // is below T; with S above T (the owner's Q suspending the Y it added)
    // nothing below T is spared
    if s != *target {
        let mut through_s = BTreeSet::from([s]);
        if a.pre.desc(*target).contains(&s) {
            through_s.extend(a.pre.desc(s));
        }
        for g in through_s {
            prop_assert_eq!(
                a.pre.factor(g).map(|f| f.state),
                a.post.factor(g).map(|f| f.state),
                "I-1: {:?} was reached only through the suspender",
                g
            );
        }
    }
    Ok(())
}

fn i2_a_freeze_changes_one_factor(a: &Applied) -> Result<(), TestCaseError> {
    if matches!(a.event, Event::Freeze { .. }) {
        // A pending passkey bound to an ended session is discarded with it:
        // that is the ceremony's end, not a cascade
        let changed = a
            .changed()
            .filter(|(_, b, f)| {
                !(matches!(
                    b.map(|b| b.state),
                    Some(CredentialState::Pending { .. })
                ) && f.state == CredentialState::Discarded)
            })
            .count();
        prop_assert!(changed <= 1, "I-2: a freeze changed {} factors", changed);
    }
    Ok(())
}

fn i4_and_i9_status_at_execution(a: &Applied) -> Result<(), TestCaseError> {
    if let Some(f) = a.pre.factor_behind(a.by) {
        let own_contest =
            matches!(a.event, Event::Contest { factor } if *factor == f);
        prop_assert!(
            own_contest || a.pre.effective(f),
            "I-4/I-9: {:?} acted while not active",
            f
        );
    }
    if let Event::FirstPasskey {
        confirm: Some(k), ..
    } = a.event
    {
        prop_assert!(a.pre.effective(FactorId::Credential(*k)));
    }
    if let Event::RecoveryCancel { .. } = a.event {
        prop_assert!(
            a.pre
                .factor_behind(a.by)
                .is_some_and(|f| a.pre.effective(f))
        );
    }
    Ok(())
}

fn i5_terminal_states(a: &Applied) -> Result<(), TestCaseError> {
    for (id, before) in &a.pre.factors {
        if before.state.is_terminal() {
            prop_assert_eq!(before.state, a.post.factors[id].state, "I-5");
        }
    }
    Ok(())
}

/// The decided case a `RecoveryEffective` applied, and its subject
fn effective_case(
    a: &Applied,
) -> Option<(RecoveryOutcome, DateTime<Utc>, FactorId)> {
    let Event::RecoveryEffective { case } = a.event else {
        return None;
    };
    let c = a.pre.cases.get(case)?;
    match (c.state, c.subject) {
        (CaseState::Decided { outcome, at, .. }, CaseSubject::Contest(f)) => {
            Some((outcome, at, f))
        }
        _ => None,
    }
}

/// `id` is `subject`, or followed it as root before the step
fn subject_or_follower(a: &Applied, id: FactorId, subject: FactorId) -> bool {
    id == subject
        || matches!(
            a.pre.factor(id).map(|f| f.state),
            Some(CredentialState::Suspended(s)) if s.follows_root == Some(subject)
        )
}

fn i6_and_i7_revocation(a: &Applied, h: &History) -> Result<(), TestCaseError> {
    for (id, before, after) in a.changed() {
        if after.state != CredentialState::Revoked {
            continue;
        }
        match a.event {
            Event::Deadline { factor } if *factor == id => {
                let Some(CredentialState::Suspended(sus)) =
                    before.map(|b| b.state)
                else {
                    return Err(TestCaseError::fail(
                        "I-6: revoked a factor not suspended",
                    ));
                };
                prop_assert!(
                    a.now > sus.deadline,
                    "I-7: at or before the deadline"
                );
                // ≥ 72 h after its own suspension, or its root's, as the
                // harness saw them happen
                let clock = sus.follows_root.unwrap_or(id);
                let started = h.suspended_at.get(&clock).copied();
                prop_assert!(
                    started.is_some_and(|t| a.now >= t + suspension_deadline()),
                    "I-7: {:?} revoked under 72 h after {:?}'s suspension",
                    id,
                    clock
                );
                if let Some(root) = sus.follows_root {
                    prop_assert!(
                        !matches!(
                            a.pre.factor(root).map(|r| r.state),
                            Some(CredentialState::Contested { .. })
                        ),
                        "I-6: its root contested"
                    );
                }
            }
            Event::RecoveryEffective { .. } => {
                let Some((outcome, at, subject)) = effective_case(a) else {
                    return Err(TestCaseError::fail("I-7: no decided case"));
                };
                prop_assert_eq!(outcome, RecoveryOutcome::Revoke, "I-7");
                prop_assert!(
                    a.now >= at + recovery_lock(),
                    "I-7: within the lock"
                );
                prop_assert!(
                    subject_or_follower(a, id, subject),
                    "I-7: {:?} isn't the case's subject or a follower",
                    id
                );
            }
            other => {
                return Err(TestCaseError::fail(format!(
                    "I-7: {id:?} revoked by {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn i8_reinstatement(a: &Applied) -> Result<(), TestCaseError> {
    for (id, before, after) in a.changed() {
        let was_suspended =
            before.is_some_and(|b| b.state.suspension().is_some());
        if was_suspended && after.state.is_active() {
            let Some((outcome, at, subject)) = effective_case(a) else {
                return Err(TestCaseError::fail(format!(
                    "I-8: {id:?} reinstated by {:?}",
                    a.event
                )));
            };
            prop_assert_eq!(outcome, RecoveryOutcome::Reinstate, "I-8");
            prop_assert!(a.now >= at + recovery_lock(), "I-8: within the lock");
            prop_assert!(
                subject_or_follower(a, id, subject),
                "I-8: {:?} isn't the case's subject or a follower",
                id
            );
        }
    }
    if matches!(a.event, Event::Unfreeze) {
        prop_assert_eq!(
            a.changed().count(),
            0,
            "I-8: an unfreeze changed a factor"
        );
    }
    Ok(())
}

fn i10_a_suspension_ends_what_it_authorised(
    a: &Applied,
) -> Result<(), TestCaseError> {
    for (id, before, after) in a.changed() {
        let newly = !before
            .is_some_and(|b| matches!(b.state, CredentialState::Suspended(_)))
            && matches!(after.state, CredentialState::Suspended(_));
        if newly {
            prop_assert!(a.post.authorised_is_empty(id), "I-10: {:?}", id);
        }
    }
    Ok(())
}

fn i11_to_i15_adds(a: &Applied) -> Result<(), TestCaseError> {
    let added = a.added();
    for id in &added {
        let factor = a.post.factors[id];
        // I-11: every add takes a tap, but the three exceptions
        let tapped = matches!(
            (a.event, a.by),
            (Event::PasskeyApprove { .. }, Authority::PasskeyTap { .. })
                | (Event::EnrolApprove { .. }, Authority::PasskeyTap { .. })
        );
        let excepted = matches!(
            a.event,
            Event::FirstPasskey { .. } | Event::RecoveryEffective { .. }
        );
        prop_assert!(
            tapped || excepted,
            "I-11: {:?} added by {:?}",
            id,
            a.event
        );
        // I-12
        if a.pre.has_yk_tap() && factor.kind.gives_yubikey_tap() {
            prop_assert!(
                a.pre.level(a.by) == TapLevel::YubikeyTap
                    || (matches!(a.event, Event::FirstPasskey { .. })
                        && !a.pre.flags.cutover)
                    || matches!(a.event, Event::RecoveryEffective { .. }),
                "I-12"
            );
        }
        // I-13: no software key added while count > 5 ∧ TIER
        if a.pre.count > 5 && a.pre.flags.tier {
            prop_assert!(!factor.kind.is_software_key(), "I-13: add");
        }
        // I-14: the parent was active at the add
        if let Some(parent) = factor.parent {
            prop_assert!(a.pre.effective(parent), "I-14: parent not active");
        }
        // I-15
        prop_assert!(!a.pre.banned(), "I-15: an add while banned");
        if a.pre.restricted() || a.pre.frozen().is_some() {
            prop_assert!(factor.kind.is_passkey(), "I-15: a non-passkey add");
        }
    }
    // I-14: parent never changes
    for (id, before) in &a.pre.factors {
        if !added.contains(id) {
            prop_assert_eq!(before.parent, a.post.factors[id].parent, "I-14");
        }
    }
    // I-13: no use but taking away, or its own contest
    if let Authority::Signed(k) = a.by {
        let soft = a
            .pre
            .factor(FactorId::Credential(k))
            .is_some_and(|f| f.kind.is_software_key());
        if soft && a.pre.count > 5 && a.pre.flags.tier {
            prop_assert!(
                matches!(
                    a.event,
                    Event::SendCancel { .. }
                        | Event::Retract { .. }
                        | Event::Contest { .. }
                ),
                "I-13: a software key's {:?}",
                a.event
            );
        }
    }
    Ok(())
}

/// The settings §5 and §7.1 govern, as one comparable value
fn settings_view(
    s: &AccountState,
) -> (
    Settings,
    Option<OperatorEmailAddressId>,
    OperatorEmailAddressId,
) {
    (
        s.settings,
        s.verified_contact().map(|c| c.address.id),
        s.primary.id,
    )
}

fn i16_to_i19_and_i39_changes(a: &Applied) -> Result<(), TestCaseError> {
    // I-16
    if let (
        Event::ChangeCancel { change },
        Authority::PasskeyTap { passkey, .. },
    ) = (a.event, a.by)
    {
        prop_assert_ne!(
            a.pre.changes[change].requested_by,
            FactorId::Passkey(passkey)
        );
    }
    // I-17, I-39: what changes a setting, and when
    let (before, after) = (settings_view(a.pre), settings_view(a.post));
    if before != after {
        let ok = match a.event {
            Event::ChangeEffective { change } => {
                a.now >= a.pre.changes[change].at + change_wait()
            }
            Event::ChangeRequest { setting, .. } => {
                !setting.always_waits()
                    && a.pre.factor_behind(a.by).is_some_and(|f| {
                        a.pre
                            .factor(f)
                            .is_some_and(|f| f.kind.is_yubikey_passkey())
                    })
            }
            Event::LogWithdraw => {
                before.0.action_log_public
                    && !after.0.action_log_public
                    && (before.1, before.2) == (after.1, after.2)
            }
            // The first contact's verification fills an empty one
            Event::AddressVerify { .. } => {
                before.1.is_none() && before.0 == after.0 && before.2 == after.2
            }
            _ => false,
        };
        prop_assert!(ok, "I-17/I-39: {:?} changed a setting", a.event);
    }
    // I-18, and I-19's effect-time parts
    if let Event::ChangeEffective { change } = a.event {
        let applied = a.post.changes[change].state == ChangeState::Effective;
        if applied {
            let pending = &a.pre.changes[change];
            prop_assert!(a.pre.effective(pending.requested_by), "I-18");
            if pending.change == SettingChange::EmergencyContact(None) {
                prop_assert!(a.pre.count <= 5, "I-19: removal above 5");
            }
            if pending.change.new_address().is_some() {
                prop_assert!(pending.address_verified, "I-19: unverified");
            }
        }
    }
    // I-19: one pending per setting; domains_ok
    let mut seen = BTreeSet::new();
    for c in a.post.changes.values() {
        if c.state == ChangeState::Pending {
            prop_assert!(seen.insert(c.change.setting()), "I-19: two pending");
        }
    }
    prop_assert!(a.post.domains_ok(), "I-19: domains");
    Ok(())
}

fn i20_minimum_means_mode(s: &AccountState) -> Result<(), TestCaseError> {
    // Each flag's own part, stated from the minimums table
    let parts = [
        (s.flags.tier, 5, s.no_soft()),
        (s.flags.minimum, 5, s.contact_ok() && s.devices() >= 2),
        (s.flags.yubikey, 50, s.has_yk_passkey()),
    ];
    let failing = parts
        .iter()
        .any(|(on, above, holds)| *on && s.count > *above && !*holds);
    prop_assert_eq!(s.restricted(), failing, "I-20");
    Ok(())
}

fn i21_no_add_below_a_minimum(a: &Applied) -> Result<(), TestCaseError> {
    if matches!(a.event, Event::AgentAdd | Event::AgentRestore) {
        prop_assert!(a.post.count <= 5 || a.post.min5(), "I-21: > 5");
        prop_assert!(a.post.count <= 50 || a.post.min50(), "I-21: > 50");
    }
    Ok(())
}

/// §1's eight taking-away events, each with a valid authority where the
/// state has one
fn taking_away(s: &AccountState) -> Vec<(Event, Authority)> {
    let passkey = s
        .factors
        .iter()
        .find(|(id, f)| {
            matches!(id, FactorId::Passkey(_)) && f.state.is_active()
        })
        .and_then(|(id, _)| passkey_of(*id));
    let session = s
        .sessions
        .iter()
        .find(|(_, x)| {
            x.open
                && match x.opener {
                    SessionOpener::Passkey(p) => {
                        s.effective(FactorId::Passkey(p))
                    }
                    SessionOpener::Password => s.password_usable(),
                }
        })
        .map(|(id, _)| *id);
    let send = s.sends.keys().next().copied().unwrap_or(send_id(99));
    let change = s.changes.keys().next().copied().unwrap_or(change_id(99));
    let target = s.factors.keys().next().copied().unwrap_or(p(99));
    let mut out = Vec::new();
    if let Some(t) = passkey {
        let by = |kind| Authority::PasskeyTap { passkey: t, kind };
        out.push((Event::Suspend { target }, by(TapKind::Suspend)));
        out.push((Event::ChangeCancel { change }, by(TapKind::CancelPending)));
        out.push((Event::LogWithdraw, by(TapKind::LogPrivate)));
    }
    if let Some(x) = session {
        let by = Authority::Session(x);
        out.push((Event::SendCancel { send }, by));
        out.push((Event::Retract { send }, by));
        out.push((Event::SignOutOthers, by));
        out.push((
            Event::ConsentRevoke {
                client: "client".parse().unwrap(),
                agent: agent(0),
            },
            by,
        ));
        out.push((
            Event::EnrolRefuse {
                enrolment: enrol(0),
            },
            by,
        ));
    }
    out
}

/// I-23: in a restricted, frozen or banned state, no taking-away event is
/// refused by the mode check
fn i23_taking_away_passes_every_mode(
    s: &AccountState,
    now: DateTime<Utc>,
) -> Result<(u32, u32), TestCaseError> {
    if !(s.restricted() || s.frozen().is_some() || s.banned()) {
        return Ok((0, 0));
    }
    let mut tried = 0;
    let mut applied = 0;
    for (event, by) in taking_away(s) {
        tried += 1;
        match s.apply(&event, by, now, &mut Counter::default()) {
            Err(Refusal::Mode(class)) => {
                return Err(TestCaseError::fail(format!(
                    "I-23: {event:?} refused by the mode ({class:?})"
                )));
            }
            Ok(_) => applied += 1,
            Err(_) => {}
        }
    }
    Ok((tried, applied))
}

fn i24_flags_are_monotone(a: &Applied) -> Result<(), TestCaseError> {
    prop_assert!(a.pre.flags.none_turned_off(&a.post.flags), "I-24");
    Ok(())
}

/// `eligible(u, X, t)` from §1's words, independently of
/// `AccountState::eligible`: u is active, isn't X, was added a week before
/// t, and didn't add X (directly or through what it added) in that week
fn eligible_restated(
    s: &AccountState,
    u: FactorId,
    x: FactorId,
    t: DateTime<Utc>,
) -> bool {
    let week = Duration::days(7);
    let Some(fu) = s.factor(u) else { return false };
    if !fu.state.is_active() || u == x || fu.added_at > t - week {
        return false;
    }
    let Some(fx) = s.factor(x) else { return true };
    let recent = fx.added_at > t - week && fx.added_at <= t;
    // Walk X's parents up: is u among them?
    let mut ancestor = fx.parent;
    let mut added_by_u = false;
    while let Some(p) = ancestor {
        if p == u {
            added_by_u = true;
            break;
        }
        ancestor = s.factor(p).and_then(|f| f.parent);
    }
    !(recent && added_by_u)
}

fn i25_to_i30_freezes(
    a: &Applied,
    h: &mut History,
) -> Result<(), TestCaseError> {
    if let (Event::Freeze { link }, Authority::Email(address)) = (a.event, a.by)
    {
        // I-25
        let times = h.freezes.entry(address).or_default();
        prop_assert!(
            times.iter().all(|t| a.now >= *t + freeze_interval()),
            "I-25"
        );
        times.push(a.now);
        // I-26
        let contact = a.pre.contact.as_ref().expect("a contact froze");
        prop_assert_eq!(contact.power, FreezePower::Armed, "I-26: disarmed");
        prop_assert_eq!(contact.address.id, address, "I-26: former contact");
        prop_assert_eq!(a.pre.freeze_links[link].address, address, "I-26");
        prop_assert!(a.pre.frozen().is_none(), "I-26: while frozen");
        // I-29
        prop_assert!(
            a.post
                .changes
                .values()
                .all(|c| c.state != ChangeState::Pending),
            "I-29: a pending change"
        );
        prop_assert!(
            a.post
                .sends
                .values()
                .all(|s| s.state != SendState::Scheduled),
            "I-29: a scheduled send"
        );
    }
    if let Event::Unfreeze = a.event {
        let frozen = a.pre.frozen().expect("was frozen");
        let u = a.pre.factor_behind(a.by).expect("a tap");
        // I-27
        prop_assert!(
            eligible_restated(a.pre, u, frozen.excluded, frozen.at),
            "I-27"
        );
        // I-28
        if let Some(c) = a
            .post
            .contact
            .as_ref()
            .filter(|c| c.address.id == frozen.address)
        {
            prop_assert_eq!(c.power, FreezePower::Disarmed, "I-28");
        }
    }
    // I-30
    if a.pre.frozen().is_some() {
        prop_assert_eq!(a.new_sends().count(), 0, "I-30: a send while frozen");
    }
    if a.effects.contains(&Effect::DeliverCompromiseNotice) {
        let frozen = a.pre.frozen().expect("I-30: notice while open");
        let t = a.pre.factor_behind(a.by).expect("a tap");
        prop_assert!(
            eligible_restated(a.pre, t, frozen.excluded, frozen.at),
            "I-30"
        );
        prop_assert!(!frozen.notice_sent && !a.pre.banned(), "I-30");
    }
    Ok(())
}

fn i31_to_i33_sends(a: &Applied) -> Result<(), TestCaseError> {
    for (_, send) in a.new_sends() {
        // I-31: recompute decision 9 over the window as it stood
        let window = a.pre.window_messages();
        let r = reach(&send.recipients, send.deliver_at, &window);
        prop_assert_eq!(r, send.reach);
        let required = required_tap(r, send.delivery);
        prop_assert!(
            required.is_some_and(|req| a.pre.level(a.by) >= req),
            "I-31: admitted below its level"
        );
        if r.reach > TAPPED_REACH {
            prop_assert_eq!(
                send.delivery,
                Delivery::Held,
                "I-31: above 500 now"
            );
        }
        // I-32
        if r.reach > UNTAPPED_REACH {
            prop_assert!(
                a.pre.contact_ok(),
                "I-32: admitted without a contact"
            );
        }
    }
    if let Event::SendDeliver { send } = a.event
        && a.post.sends[send].state == SendState::Delivered
    {
        let m = &a.pre.sends[send];
        if m.reach.reach > UNTAPPED_REACH {
            prop_assert!(
                a.pre.contact_ok(),
                "I-32: delivered without a contact"
            );
        }
        // I-33
        let admitter = match m.admitted_by {
            AdmittedBy::Factor(f) => Some(f),
            AdmittedBy::Session(s) => a.pre.session_factor(s),
        };
        if let Some(f) = admitter {
            prop_assert!(a.pre.effective(f), "I-33: admitter not active");
        }
        prop_assert!(!a.pre.banned(), "I-33: banned");
    }
    Ok(())
}

/// I-34's ✓ rows: the factor behind each announced activity
fn announced_factor(a: &Applied) -> Option<FactorId> {
    let behind = a.pre.factor_behind(a.by);
    match a.event {
        Event::ChangeRequest { .. }
        | Event::PasskeyApprove { .. }
        | Event::EnrolApprove { .. }
        | Event::OAuthConsent { .. }
        | Event::AgentDelete { .. } => behind,
        Event::ChangeEffective { change }
            if a.post.changes[change].state == ChangeState::Effective =>
        {
            Some(a.pre.changes[change].requested_by)
        }
        Event::FirstPasskey { confirm, .. } => {
            confirm.map(FactorId::Credential)
        }
        Event::SendAdmit { send, .. } => {
            let s = &a.post.sends[send];
            let tapped = required_tap(s.reach, s.delivery)
                .is_some_and(|r| r >= TapLevel::Tap);
            let announced = (s.delivery == Delivery::Held
                && s.reach.reach > UNTAPPED_REACH)
                || (s.delivery == Delivery::Immediate
                    && s.reach.immediate_reach > UNTAPPED_REACH)
                || tapped;
            announced.then_some(behind).flatten()
        }
        Event::Suspend { .. } if !a.newly_suspended().is_empty() => behind,
        Event::SignIn { passkey, .. }
            if !a.pre.factor(FactorId::Passkey(*passkey))?.signed_in =>
        {
            behind
        }
        _ => None,
    }
}

/// I-40: every send whose required tap is a passkey tap or more is
/// announced, to the primary and (with a contact) to the contact with a
/// freeze link excluding the factor that admitted it
fn i40_every_tapped_send_is_announced(
    a: &Applied,
) -> Result<(), TestCaseError> {
    for (id, send) in a.new_sends() {
        let required = required_tap(send.reach, send.delivery);
        if !required.is_some_and(|r| r >= TapLevel::Tap) {
            continue;
        }
        let about = |n: &&Notice| {
            matches!(
                n.subject,
                NoticeSubject::SendHeld(s) | NoticeSubject::SendNow(s) | NoticeSubject::SendTapped(s)
                    if s == *id
            )
        };
        prop_assert!(
            a.notices()
                .filter(about)
                .any(|n| matches!(n.to, Audience::Primary | Audience::Both)),
            "I-40: the primary wasn't told of {:?}",
            id
        );
        if a.pre.verified_contact().is_some() {
            let admitter = a.pre.factor_behind(a.by);
            prop_assert!(
                a.notices().filter(about).any(|n| n
                    .freeze_link
                    .map(|(_, x)| x)
                    == admitter
                    && admitter.is_some()),
                "I-40: the contact got no link for {:?}",
                id
            );
        }
    }
    Ok(())
}

fn i34_announced_with_a_link(a: &Applied) -> Result<(), TestCaseError> {
    let contact = a.pre.verified_contact().is_some()
        && a.post.verified_contact().is_some();
    if let Some(f) = announced_factor(a).filter(|_| contact) {
        prop_assert!(
            a.notices()
                .any(|n| n.freeze_link.is_some_and(|(_, x)| x == f)),
            "I-34: {:?} announced without a link excluding {:?}",
            a.event,
            f
        );
    }
    Ok(())
}

fn i35_email_never_widens(a: &Applied) -> Result<(), TestCaseError> {
    if !matches!(a.by, Authority::Email(_)) {
        return Ok(());
    }
    prop_assert!(
        matches!(
            a.event,
            Event::SendCancel { .. }
                | Event::Freeze { .. }
                | Event::AddressVerify { .. }
        ),
        "I-35: email caused {:?}",
        a.event
    );
    prop_assert!(a.added().is_empty(), "I-35: an add");
    prop_assert!(
        a.changed().all(|(_, _, f)| !f.state.is_active()),
        "I-35: a factor made active"
    );
    for (id, s) in &a.post.sessions {
        prop_assert!(
            !s.open || a.pre.sessions.get(id).is_some_and(|b| b.open),
            "I-35: a session opened"
        );
    }
    prop_assert_eq!(a.new_sends().count(), 0, "I-35: a send");
    for (id, c) in &a.post.changes {
        prop_assert!(
            a.pre.changes.get(id).is_some_and(|b| b.state == c.state)
                || c.state == ChangeState::Cancelled,
            "I-35: a change"
        );
    }
    prop_assert!(
        !(a.pre.frozen().is_some() && a.post.frozen().is_none()),
        "I-35: an unfreeze"
    );
    let power = |s: &AccountState| s.contact.as_ref().map(|c| c.power);
    prop_assert!(
        !(power(a.pre) == Some(FreezePower::Disarmed)
            && power(a.post) == Some(FreezePower::Armed)),
        "I-35: a re-arm"
    );
    Ok(())
}

fn i36_links_name_an_active_factor(a: &Applied) -> Result<(), TestCaseError> {
    for (id, link) in &a.post.freeze_links {
        if !a.pre.freeze_links.contains_key(id) {
            prop_assert!(
                a.pre.effective(link.factor),
                "I-36: {:?}",
                link.factor
            );
            let named: Vec<_> = a
                .notices()
                .filter_map(|n| n.freeze_link)
                .filter(|(l, _)| l == id)
                .collect();
            prop_assert_eq!(
                named,
                vec![(*id, link.factor)],
                "I-36: one factor"
            );
        }
    }
    Ok(())
}

fn i37_and_i38_sessions(a: &Applied) -> Result<(), TestCaseError> {
    for s in a.post.sessions.values().filter(|s| s.open) {
        match s.opener {
            SessionOpener::Passkey(p) => {
                prop_assert!(a.post.effective(FactorId::Passkey(p)), "I-37");
            }
            SessionOpener::Password => {
                prop_assert!(
                    a.post.auth == AuthMode::PasswordOnly
                        && !a.post.flags.cutover,
                    "I-38: a password session survived"
                );
            }
        }
    }
    let by_password = match a.by {
        Authority::Password => true,
        Authority::Session(s) => a
            .pre
            .sessions
            .get(&s)
            .is_some_and(|s| s.opener == SessionOpener::Password),
        _ => false,
    };
    if by_password {
        prop_assert!(
            a.pre.password_usable(),
            "I-38: a password after it retired"
        );
    }
    Ok(())
}

fn check(a: &Applied, h: &mut History) -> Result<(), TestCaseError> {
    h.record(a);
    i1_and_i3_the_cascade(a)?;
    i2_a_freeze_changes_one_factor(a)?;
    i4_and_i9_status_at_execution(a)?;
    i5_terminal_states(a)?;
    i6_and_i7_revocation(a, h)?;
    i8_reinstatement(a)?;
    i10_a_suspension_ends_what_it_authorised(a)?;
    i11_to_i15_adds(a)?;
    i16_to_i19_and_i39_changes(a)?;
    i20_minimum_means_mode(a.post)?;
    i21_no_add_below_a_minimum(a)?;
    i23_taking_away_passes_every_mode(a.post, a.now)?;
    i24_flags_are_monotone(a)?;
    i25_to_i30_freezes(a, h)?;
    i31_to_i33_sends(a)?;
    i34_announced_with_a_link(a)?;
    i35_email_never_widens(a)?;
    i36_links_name_an_active_factor(a)?;
    i37_and_i38_sessions(a)?;
    i40_every_tapped_send_is_announced(a)?;
    Ok(())
}

/// Run `steps` from `start`, calling `each` on every step that applied;
/// returns the final state and how many applied
fn drive(
    start: AccountState,
    steps: &[(Duration, EventGen, AuthGen)],
    mut each: impl FnMut(&Applied) -> Result<(), TestCaseError>,
) -> Result<(AccountState, usize), TestCaseError> {
    let mut s = start;
    let mut now = t0();
    let mut ids = Counter::default();
    let mut applied = 0;
    for (wait, event, auth) in steps {
        now += *wait;
        for (event, by) in resolve(&s, event, auth).unwrap_or_default() {
            let Ok(t) = s.apply(&event, by, now, &mut ids) else {
                break;
            };
            applied += 1;
            each(&Applied {
                pre: &s,
                post: &t.state,
                event: &event,
                by,
                now,
                effects: &t.effects,
            })?;
            s = t.state;
        }
    }
    Ok((s, applied))
}

/// Run `steps`, checking every invariant after each step that applied
fn run(
    start: AccountState,
    steps: &[(Duration, EventGen, AuthGen)],
) -> Result<(AccountState, usize), TestCaseError> {
    i20_minimum_means_mode(&start)?;
    let mut history = History::default();
    drive(start, steps, |a| check(a, &mut history))
}

/// Proptest cases per property; `PROPTEST_CASES` overrides
const CASES: u32 = 1024;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(CASES)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// I-1 to I-21 and I-23 to I-39 after every step of a generated run
    #[test]
    fn invariants_hold_after_every_step(
        start in start(),
        steps in proptest::collection::vec(gen_step(), 1..80),
    ) {
        run(start, &steps)?;
    }

    /// I-22: from any reachable restricted, unfrozen, unbanned state with an
    /// effective passkey, the restoring moves reach ¬restricted
    #[test]
    fn i22_restricted_mode_is_escapable(
        start in start(),
        steps in proptest::collection::vec(gen_step(), 0..60),
    ) {
        let (s, _) = run(start, &steps)?;
        let passkey = s
            .factors
            .iter()
            .find(|(id, f)| matches!(id, FactorId::Passkey(_)) && f.state.is_active())
            .map(|(id, _)| *id);
        let Some(FactorId::Passkey(t)) = passkey else { return Ok(()) };
        if !s.restricted() || s.frozen().is_some() || s.banned() {
            return Ok(());
        }
        let escaped = escape(s, t)?;
        prop_assert!(!escaped.restricted(), "I-22: still restricted");
    }
}

/// I-22's sequence, as the owner would run it, with fresh hardware and
/// addresses supplied
fn escape(
    s: AccountState,
    t: OperatorPasskeyId,
) -> Result<AccountState, TestCaseError> {
    let mut w = World::new(s);
    w.now = t0() + Duration::days(400);
    let owner = |kind| Authority::PasskeyTap { passkey: t, kind };
    let mut next = 100u8;
    w.ok(
        Event::SignIn {
            session: sess(99),
            passkey: t,
        },
        Authority::Assertion(t),
    );
    // Pending changes (the contact's included) run out first
    w.wait(Duration::hours(73));
    let pending: Vec<_> = w.s.changes.keys().copied().collect();
    for change in pending {
        let _ =
            w.try_do(Event::ChangeEffective { change }, Authority::Platform);
    }
    // Suspend each effective software key; and, with a YubiKey-tap factor
    // but no effective YubiKey passkey, the factors that give it, then let
    // their deadlines pass. SPEC: I-22's sequence assumes no such factor
    // contests: one that did, or that follows a root that did, counts
    // toward `has_yk_tap` until manual recovery decides, so the escape then
    // runs through that case
    for _ in 0..4 {
        let yk_stuck = w.s.has_yk_tap() && !w.s.has_yk_passkey();
        let targets: Vec<_> =
            w.s.factors
                .iter()
                .filter(|(id, f)| {
                    **id != FactorId::Passkey(t)
                        && f.state.is_live()
                        && (f.kind.is_software_key()
                            || (yk_stuck && f.kind.gives_yubikey_tap()))
                })
                .map(|(id, _)| *id)
                .collect();
        if targets.is_empty() {
            break;
        }
        for target in targets {
            w.wait(Duration::hours(1));
            if w.s.effective(target) {
                w.ok(Event::Suspend { target }, owner(TapKind::Suspend));
                continue;
            }
            // Contested itself, or following a contested root
            let contested = |f: FactorId| match w.s.factor(f).map(|f| f.state) {
                Some(CredentialState::Contested { case, .. }) => Some(case),
                _ => None,
            };
            let root = match w.s.factor(target).map(|f| f.state) {
                Some(CredentialState::Suspended(sus)) => sus.follows_root,
                _ => None,
            };
            let Some(case) =
                contested(target).or_else(|| root.and_then(contested))
            else {
                continue;
            };
            if let CaseState::Open { .. } = w.s.cases[&case].state {
                w.ok(
                    Event::RecoveryDecide {
                        case,
                        outcome: RecoveryOutcome::Revoke,
                        losing: None,
                    },
                    Authority::Staff,
                );
            }
            w.wait(Duration::days(7));
            w.ok(Event::RecoveryEffective { case }, Authority::Platform);
        }
        w.wait(Duration::hours(73));
        let suspended: Vec<_> = w
            .s
            .factors
            .iter()
            .filter(|(_, f)| matches!(f.state, CredentialState::Suspended(_)))
            .map(|(id, _)| *id)
            .collect();
        for factor in suspended {
            let _ = w.try_do(Event::Deadline { factor }, Authority::Platform);
        }
    }
    // The contact
    let contact_ok = w.s.contact_ok();
    match w.s.contact.clone() {
        None => w.contact_with(owner(TapKind::FirstContact), 50),
        Some(c) if c.state == ContactState::Unverified => {
            w.contact_with(owner(TapKind::FirstContact), 50)
        }
        Some(_) if !contact_ok => {
            w.ok(
                Event::ChangeRequest {
                    change: change_id(50),
                    setting: SettingChange::EmergencyContact(Some(address(
                        50,
                        "fresh.example",
                    ))),
                },
                owner(TapKind::PendingChange),
            );
            w.ok(
                Event::AddressVerify {
                    address: addr_id(50),
                },
                Authority::Email(addr_id(50)),
            );
            w.wait(Duration::hours(72));
            w.ok(
                Event::ChangeEffective {
                    change: change_id(50),
                },
                Authority::Platform,
            );
        }
        Some(_) => {}
    }
    // Devices, then above 50 a YubiKey passkey
    while w.s.devices() < 2 {
        w.add_passkey_by(next, DEVICE, t, 99);
        next += 1;
    }
    if w.s.count > 50 && !w.s.has_yk_passkey() {
        w.add_passkey_by(next, YUBIKEY, t, 99);
    }
    Ok(w.s)
}

impl World {
    #[track_caller]
    fn contact_with(&mut self, by: Authority, i: u8) {
        self.ok(
            Event::ContactSetFirst {
                address: address(i, "fresh.example"),
            },
            by,
        );
        self.ok(
            Event::AddressVerify {
                address: addr_id(i),
            },
            Authority::Email(addr_id(i)),
        );
    }

    #[track_caller]
    fn add_passkey_by(
        &mut self,
        i: u8,
        class: PasskeyClass,
        by: OperatorPasskeyId,
        session: u8,
    ) {
        self.ok(
            Event::PasskeyRegister {
                passkey: pk(i),
                class,
            },
            Authority::Session(sess(session)),
        );
        let level = if class.yubikey && self.s.has_yk_tap() {
            // The owner's own YubiKey passkey, if it has one
            self.s
                .factors
                .iter()
                .find(|(_, f)| {
                    f.state.is_active() && f.kind.is_yubikey_passkey()
                })
                .and_then(|(id, _)| match id {
                    FactorId::Passkey(p) => Some(*p),
                    FactorId::Credential(_) => None,
                })
                .unwrap_or(by)
        } else {
            by
        };
        self.ok(
            Event::PasskeyApprove { passkey: pk(i) },
            Authority::PasskeyTap {
                passkey: level,
                kind: TapKind::PasskeyEnrolment,
            },
        );
    }
}

/// The generator reaches the interesting states, so the properties aren't
/// vacuous: how often each kind of event applied over as many runs as
/// the property test has cases
#[test]
#[ignore = "slow (~16 s, one thread): CI runs it through `just test-slow`"]
fn the_generator_reaches_every_machine() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut seen: BTreeMap<&'static str, u32> = BTreeMap::new();
    for _ in 0..CASES {
        let start = start().new_tree(&mut runner).unwrap().current();
        let steps = proptest::collection::vec(gen_step(), 80)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        drive(start, &steps, |a| {
            let cascaded = |s: &AccountState| {
                s.factors
                    .values()
                    .filter(|f| {
                        matches!(
                            f.state,
                            CredentialState::Suspended(Suspension {
                                cause: SuspensionCause::Cascade { .. },
                                ..
                            })
                        )
                    })
                    .count()
            };
            let decided_round =
                |case: &OperatorRecoveryCaseId| match a.pre.cases[case].state {
                    CaseState::Open { round } => round,
                    _ => 0,
                };
            let name = match a.event {
                Event::Suspend { .. } if cascaded(a.post) > cascaded(a.pre) => {
                    "cascade"
                }
                Event::Suspend { .. } => "suspend",
                Event::Freeze { .. } => "freeze",
                Event::Unfreeze => "unfreeze",
                Event::CompromiseNotice => "compromise notice",
                Event::ReArm => "re-arm",
                Event::Contest { .. } => "contest",
                Event::Deadline { .. } => "deadline",
                Event::RecoveryCancel { .. } => "counter-claim",
                Event::RecoveryDecide { case, losing, .. }
                    if decided_round(case) == 2 =>
                {
                    let suspended = losing.is_some_and(|l| {
                        a.pre.effective(l)
                            && matches!(
                                a.post.factor(l).map(|f| f.state),
                                Some(CredentialState::Suspended(Suspension {
                                    cause: SuspensionCause::Recovery { .. },
                                    ..
                                }))
                            )
                    });
                    if suspended {
                        *seen.entry("recovery suspend").or_default() += 1;
                    }
                    "round 2"
                }
                Event::RecoveryDecide { .. } => "decide",
                Event::RecoveryEffective { .. }
                    if matches!(
                        effective_case(a),
                        Some((RecoveryOutcome::Reinstate, ..))
                    ) =>
                {
                    "reinstate"
                }
                Event::RecoveryEffective { .. } => "recovery effective",
                Event::ChangeEffective { .. } => "change effective",
                Event::ChangeCancel { .. } => "change cancel",
                Event::EnrolApprove { .. } => "enrol approve",
                Event::PasskeyApprove { .. } => "passkey approve",
                Event::FirstPasskey { .. } => "first passkey",
                Event::SendAdmit { recipients, .. }
                    if recipients.len() > 500 =>
                {
                    "send > 500"
                }
                Event::SendAdmit { send, .. }
                    if a.notices().any(|n| {
                        n.subject == NoticeSubject::SendTapped(*send)
                    }) =>
                {
                    "tapped now (I-40)"
                }
                Event::SendAdmit { recipients, .. } if recipients.len() > 5 => {
                    "send > 5"
                }
                Event::SendCancel { .. }
                    if matches!(a.by, Authority::Email(_)) =>
                {
                    "cancel link"
                }
                Event::Retract { .. } => "retract",
                Event::SendDeliver { .. } => "deliver",
                Event::AgentAdd => "agent add",
                _ => "other",
            };
            let (tried, _) = i23_taking_away_passes_every_mode(a.post, a.now)?;
            *seen.entry("I-23 attempts").or_default() += tried;
            *seen.entry(name).or_default() += 1;
            Ok(())
        })
        .unwrap();
    }
    eprintln!("{seen:?}");
    for (name, at_least) in [
        ("cascade", 20),
        ("suspend", 100),
        ("freeze", 20),
        ("unfreeze", 20),
        ("compromise notice", 20),
        ("re-arm", 20),
        ("contest", 20),
        ("deadline", 50),
        ("counter-claim", 20),
        ("round 2", 20),
        ("recovery suspend", 20),
        ("reinstate", 20),
        ("recovery effective", 20),
        ("change effective", 20),
        ("change cancel", 20),
        ("enrol approve", 50),
        ("passkey approve", 100),
        ("first passkey", 10),
        ("send > 5", 20),
        ("send > 500", 20),
        ("tapped now (I-40)", 20),
        ("cancel link", 20),
        ("retract", 20),
        ("deliver", 50),
        ("agent add", 20),
        ("I-23 attempts", 1000),
    ] {
        let got = seen.get(name).copied().unwrap_or(0);
        assert!(got >= at_least, "{name}: {got} < {at_least} ({seen:?})");
    }
}

#[test]
fn log_kinds_cover_the_spec_list() {
    let labels: Vec<String> = OperatorActionKind::ALL
        .iter()
        .map(|k| k.to_string())
        .collect();
    for label in [
        "dm_send",
        "dm_cancel",
        "dm_retract",
        "credential_add",
        "credential_revoke",
        "config_change_requested",
        "config_change_effective",
        "config_change_cancelled",
        "log_visibility",
        "group_change",
        "credential_suspend",
        "credential_contest",
        "oauth_consent",
        "freeze",
        "unfreeze",
        "credential_reinstate",
        "session_revoke",
        "agent_delete",
    ] {
        assert!(labels.iter().any(|l| l == label), "{label}");
    }
}

#[test]
fn a_factor_kind_names_its_credential_kind() {
    use super::tap::{FactorKind, PasskeyClass};
    use crate::enums::OperatorCredentialKind as K;
    let passkey = PasskeyClass {
        synced: false,
        yubikey: false,
    };
    assert_eq!(FactorKind::Passkey(passkey).credential_kind(), None);
    assert_eq!(
        FactorKind::SoftwareKey.credential_kind(),
        Some(K::SoftwareKey)
    );
}

#[cfg(feature = "x509")]
#[test]
fn the_default_fido_store_is_the_pinned_one_and_prints_its_subjects() {
    use crate::operator_signer::fido::FidoTrustStore;
    let printed = format!("{:?}", FidoTrustStore::default());
    assert!(printed.contains("Yubico"), "{printed}");
}

// ---------------------------------------------------------------------------
// The rarer arms
// ---------------------------------------------------------------------------

/// A use is the session's own, and moves only its clock
#[test]
fn a_session_use_is_its_own() {
    let mut w = registered(DEVICE);
    w.sign_in(1, 0);
    w.wait(Duration::hours(1));
    let before = w.s.sessions[&sess(1)].last_used;
    assert_eq!(
        w.try_do(
            Event::SessionUse { session: sess(0) },
            Authority::Session(sess(1)),
        ),
        Err(Refusal::WrongAuthority)
    );
    assert_eq!(w.s.sessions[&sess(0)].last_used, t0());
    w.ok(
        Event::SessionUse { session: sess(0) },
        Authority::Session(sess(0)),
    );
    assert_eq!(w.s.sessions[&sess(0)].last_used, t0() + Duration::hours(1));
    assert_eq!(w.s.sessions[&sess(1)].last_used, before);
}

/// A change of primary address takes effect after its wait
#[test]
fn the_primary_address_changes_when_its_change_takes_effect() {
    let mut w = registered(DEVICE);
    w.add_passkey(1, DEVICE, 0, 0);
    w.wait(Duration::days(8));
    let new = address(3, "new.example");
    w.ok(
        Event::ChangeRequest {
            change: change_id(0),
            setting: SettingChange::PrimaryEmail(new.clone()),
        },
        tap(1, TapKind::PendingChange),
    );
    w.ok(
        Event::AddressVerify {
            address: addr_id(3),
        },
        Authority::Email(addr_id(3)),
    );
    assert_ne!(w.s.primary, new);
    w.wait(Duration::hours(72));
    w.ok(
        Event::ChangeEffective {
            change: change_id(0),
        },
        Authority::Platform,
    );
    assert_eq!(w.s.primary, new);
}

/// Normal mode: none of frozen, banned or restricted
#[test]
fn normal_mode_and_the_freeze_interval() {
    let w = registered(DEVICE);
    assert!(w.s.normal());
    assert_eq!(freeze_interval(), Duration::hours(24));
}

/// A send to nobody is refused; a tapped one is announced
#[test]
fn a_send_to_nobody_and_what_is_announced() {
    let nobody = BTreeSet::new();
    assert_eq!(
        plan_send(&request(&nobody, When::Default, TapLevel::Tap, &[]))
            .map(|_| ()),
        Err(SendRefusal::NoRecipients)
    );
    let few = agents(0, 2);
    let quiet =
        plan_send(&request(&few, When::Default, TapLevel::None, &[])).unwrap();
    assert!(!quiet.announced());
    let many = agents(0, 6);
    let loud =
        plan_send(&request(&many, When::Default, TapLevel::Tap, &[])).unwrap();
    assert!(loud.announced());
}

/// What a request's factor is as a credential
#[test]
fn a_hardware_factor_is_a_hardware_credential() {
    assert_eq!(
        SUDO.credential_kind(),
        Some(crate::enums::OperatorCredentialKind::HardwareKey)
    );
}
