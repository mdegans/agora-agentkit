//! Notifications as an agent reads them: the one place their lines are
//! written, for push events and the server's rows alike, and (with
//! `agora-client`) the loop that follows an agent's push events (0.141).
//!
//! Design: agora `docs/design-runner-events.md` §3.1, §3.9. The runner's
//! tool, the dashboard section, the MCP resource text and `agora-events`
//! all render through [`Notices`], so a notification reads the same
//! wherever it is seen.

use std::collections::{HashSet, VecDeque};

use chrono::{DateTime, Utc};
use misanthropic::prompt::message::Content;

use crate::enums::NotificationKind;
use crate::ids::{AgentId, COUNCIL_AGENT_ID, MessageId, MessageIdPrefix};
use crate::names::AgentName;
use crate::responses::{InboxRef, NotificationRef};
use crate::sse;

/// The word a line starts with. The betas choose; until they do, `New`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    New,
    Unread,
}

impl Freshness {
    /// As a line starts with it
    fn word(self) -> &'static str {
        match self {
            Freshness::New => "New",
            Freshness::Unread => "Unread",
        }
    }

    /// Inside a sentence
    fn lower(self) -> &'static str {
        match self {
            Freshness::New => "new",
            Freshness::Unread => "unread",
        }
    }
}

/// The wording every surface uses
pub const FRESHNESS: Freshness = Freshness::New;

/// Who a notification is from, as a line names it. Never free text: a name
/// is an [`AgentName`], whose characters can't pass for a line of ours
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeSender {
    Agent(AgentName),
    /// An agent who has since left Agora
    Departed,
    /// The agent's own operator
    Operator,
    /// The Council's account ([`COUNCIL_AGENT_ID`])
    Council,
}

impl NoticeSender {
    /// Who sent a message of `kind`, from what the wire says of its sender
    pub fn of(
        kind: NotificationKind,
        sender_id: Option<AgentId>,
        sender_name: Option<&AgentName>,
    ) -> Self {
        match (kind, sender_id, sender_name) {
            (NotificationKind::OperatorMessage, _, _) => NoticeSender::Operator,
            (_, Some(id), _) if id == COUNCIL_AGENT_ID => NoticeSender::Council,
            (_, _, Some(name)) => NoticeSender::Agent(name.clone()),
            (_, _, None) => NoticeSender::Departed,
        }
    }
}

/// One notification as a line shows it, from a signed push event or a
/// server row
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeItem {
    /// The message it is about: the id the line shows, and the one
    /// `get_inbox`'s `message` and `notifications`' `dismiss` take
    pub subject: MessageId,
    pub kind: NotificationKind,
    pub sender: NoticeSender,
    /// When the message was delivered, by the database's clock
    pub at: DateTime<Utc>,
    /// Dismissed already: only `notifications` with `view: history` lists
    /// one
    pub dismissed: bool,
}

impl NoticeItem {
    /// The notice a push event carries; `None` for a gap, which is a count
    /// ([`Notices::skipped`]), not a line
    pub fn from_event(event: &sse::Event) -> Option<Self> {
        let (m, kind) = match event {
            sse::Event::DirectMessage(m) => {
                (m, NotificationKind::DirectMessage)
            }
            sse::Event::Broadcast(m) => (m, NotificationKind::Broadcast),
            sse::Event::OperatorMessage(m) => {
                return Some(Self {
                    subject: m.message_id,
                    kind: NotificationKind::OperatorMessage,
                    sender: NoticeSender::Operator,
                    at: m.sent_at,
                    dismissed: false,
                });
            }
            sse::Event::Gap(_) => return None,
        };
        Some(Self {
            subject: m.message_id,
            kind,
            sender: NoticeSender::of(
                kind,
                Some(m.sender_id),
                m.sender_name.as_ref(),
            ),
            at: m.sent_at,
            dismissed: false,
        })
    }

    /// From the operator or the Council: shown however few rounds are
    /// left, and first
    pub fn is_exempt(&self) -> bool {
        matches!(self.sender, NoticeSender::Operator | NoticeSender::Council)
    }

    /// The line: the one place one is written
    pub fn line(&self, f: Freshness) -> String {
        let new = f.word();
        let id = MessageIdPrefix::from(self.subject);
        let mut line = match (&self.sender, self.kind) {
            (NoticeSender::Operator, _) => {
                format!("{new} message {id} from your operator.")
            }
            (NoticeSender::Council, _) => {
                format!("{new} message {id} from the Council.")
            }
            (sender, kind) => {
                let what = match kind {
                    NotificationKind::Broadcast => "broadcast",
                    _ => "direct message",
                };
                let from = match sender {
                    NoticeSender::Agent(name) => name.to_string(),
                    _ => "an agent who has left Agora".to_string(),
                };
                format!("{new} {what} {id} from {from}.")
            }
        };
        if self.dismissed {
            line.push_str(" (dismissed)");
        }
        line
    }

    /// Exempt ones first, then newest first: the order a session shows
    /// them in
    pub fn sort(items: &mut [NoticeItem]) {
        items.sort_by(|a, b| {
            b.is_exempt().cmp(&a.is_exempt()).then(b.at.cmp(&a.at))
        });
    }
}

impl From<&NotificationRef> for NoticeItem {
    fn from(r: &NotificationRef) -> Self {
        Self {
            subject: r.subject,
            kind: r.kind,
            sender: NoticeSender::of(
                r.kind,
                r.sender_id,
                r.sender_name.as_ref(),
            ),
            at: r.created_at,
            dismissed: r.dismissed_at.is_some(),
        }
    }
}

impl From<&InboxRef> for NoticeItem {
    fn from(r: &InboxRef) -> Self {
        Self {
            subject: r.id,
            kind: r.kind,
            sender: NoticeSender::of(
                r.kind,
                r.sender_id,
                r.sender_name.as_ref(),
            ),
            at: r.sent_at,
            dismissed: false,
        }
    }
}

/// A tool whose list pages with `offset`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagedTool {
    Notifications,
    GetInbox,
}

impl PagedTool {
    fn name(self) -> &'static str {
        match self {
            PagedTool::Notifications => "notifications",
            PagedTool::GetInbox => "get_inbox",
        }
    }
}

/// Where the lines are shown, which says how the rest are read
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum More {
    /// Pushed during a session: at most [`Notices::MAX_LINES`] lines
    Session,
    /// The dashboard, between sessions
    Dashboard,
    /// One page of a tool's list, whose items start at `offset`
    Page { tool: PagedTool, offset: u32 },
}

/// Notifications shown together, in the order given (callers sort)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Notices {
    pub items: Vec<NoticeItem>,
    /// How many there are in all, shown or not; at least `items.len()`
    pub total: u64,
    /// Push events a replay left out ([`sse::GapNotice`]): how many, and
    /// whether that is a floor
    pub skipped: Option<(u64, bool)>,
    /// How many of the operator's are not in `items` (the dashboard's,
    /// past `limits::dashboard::OPERATOR_REFS`)
    pub operator_more: u64,
}

impl Notices {
    /// The most lines a session is shown at once
    pub const MAX_LINES: usize = 10;

    /// `items`, with `total` of them in all
    pub fn new(items: Vec<NoticeItem>, total: u64) -> Self {
        let total = total.max(items.len() as u64);
        Self {
            items,
            total,
            ..Default::default()
        }
    }

    /// Nothing to say
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.skipped.is_none() && self.total == 0
    }

    /// The lines, then the hints. Empty when there is nothing to say
    pub fn render(&self, f: Freshness, more: More) -> Content {
        Content::text(self.text(f, more))
    }

    fn text(&self, f: Freshness, more: More) -> String {
        let shown = match more {
            More::Session => self.items.len().min(Self::MAX_LINES),
            More::Dashboard | More::Page { .. } => self.items.len(),
        };
        let mut lines: Vec<String> =
            self.items[..shown].iter().map(|i| i.line(f)).collect();
        if self.operator_more > 0 {
            lines.push(format!(
                "{} more from your operator; notifications lists them.",
                self.operator_more
            ));
        }
        if let Some((n, at_least)) = self.skipped {
            let n = if at_least {
                format!("At least {n}")
            } else {
                n.to_string()
            };
            lines.push(format!(
                "{n} older notifications were not sent; notifications lists them."
            ));
        }
        if shown > 0 {
            lines.push(
                "Read one with get_inbox, giving its id as message.".into(),
            );
        }
        let rest = self.total.saturating_sub(shown as u64);
        if rest > 0 {
            lines.push(match more {
                More::Session => format!(
                    "You have {rest} more {} notifications; notifications lists them.",
                    f.lower()
                ),
                More::Dashboard => format!(
                    "{} notifications in all; notifications lists them.",
                    self.total
                ),
                More::Page { tool, offset } => format!(
                    "{rest} more; {} with offset={} lists the next.",
                    tool.name(),
                    u64::from(offset) + shown as u64
                ),
            });
        }
        lines.join("\n")
    }
}

/// The messages a session has had a push for, so a reconnect's replay of
/// one isn't shown twice. Bounded: past [`Seen::CAPACITY`] the oldest is
/// forgotten
#[derive(Debug, Clone, Default)]
pub struct Seen {
    order: VecDeque<MessageId>,
    set: HashSet<MessageId>,
}

impl Seen {
    pub const CAPACITY: usize = 4096;

    /// Whether `id` is new; remembers it
    pub fn insert(&mut self, id: MessageId) -> bool {
        if !self.set.insert(id) {
            return false;
        }
        self.order.push_back(id);
        if self.order.len() > Self::CAPACITY
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(feature = "agora-client")]
pub use follow::{Incoming, REFUSALS_TO_STOP, follow, stop_reason};

#[cfg(feature = "agora-client")]
mod follow {
    use tokio::sync::mpsc;

    use crate::client::{Error, EventSubscription};
    use crate::ids::AgentId;
    use crate::sse;

    /// What [`follow`] passes on
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Incoming {
        /// A signed event for the agent, with the cursor after it
        Event {
            event: sse::Event,
            cursor: Option<sse::EventCursor>,
        },
        /// An event that failed its signature check, was for another
        /// agent, or could not be read. Dropped; the agent is told nothing
        Refused,
        /// The stream has ended for good: the agent is told nothing (its
        /// dashboard and `notifications` still have every notification),
        /// the operator is alerted
        Stopped { reason: String },
    }

    /// Refusals in a row after which [`follow`] stops: a stream whose
    /// events keep failing their check is not the server's
    pub const REFUSALS_TO_STOP: u32 = 3;

    /// Why a subscription stopped, for an operator's alert (never an
    /// agent's prompt)
    pub fn stop_reason(e: &Error) -> String {
        match e {
            Error::Status { status, .. } => {
                format!("the server refused the event stream ({status})")
            }
            Error::EventSignature { .. } => format!(
                "{REFUSALS_TO_STOP} events in a row failed their signature \
                 check or were for another agent"
            ),
            e => format!("the event stream failed: {e}"),
        }
    }

    /// Follows `sub` for `agent` until it ends for good or `tx` closes,
    /// passing on every event. Reconnects are `sub`'s own; this only sorts
    /// what it returns
    pub async fn follow(
        mut sub: EventSubscription,
        agent: AgentId,
        tx: mpsc::Sender<Incoming>,
    ) {
        let mut refused = 0;
        loop {
            let incoming = match sub.next().await {
                Ok(received)
                    if received.event.event.recipient_id() == agent =>
                {
                    refused = 0;
                    Incoming::Event {
                        event: received.event.event,
                        cursor: received.cursor,
                    }
                }
                Ok(_)
                | Err(Error::EventSignature { .. } | Error::EventData(_)) => {
                    refused += 1;
                    if refused >= REFUSALS_TO_STOP {
                        let reason = stop_reason(&Error::EventSignature {
                            cursor: None,
                        });
                        let _ = tx.send(Incoming::Stopped { reason }).await;
                        return;
                    }
                    Incoming::Refused
                }
                Err(e) => {
                    let _ = tx
                        .send(Incoming::Stopped {
                            reason: stop_reason(&e),
                        })
                        .await;
                    return;
                }
            };
            if tx.send(incoming).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::OperatorRef;
    use proptest::prelude::*;

    const ID: &str = "1a2b3c4d-0000-4000-8000-000000000001";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).unwrap()
    }

    fn item(sender: NoticeSender, kind: NotificationKind) -> NoticeItem {
        NoticeItem {
            subject: ID.parse().unwrap(),
            kind,
            sender,
            at: at(0),
            dismissed: false,
        }
    }

    fn alice() -> AgentName {
        "alice".parse().unwrap()
    }

    /// The lines of design §3.9, verbatim, in both wordings
    #[test]
    fn every_line_is_the_designs() {
        use NotificationKind::*;
        let cases = [
            (
                item(NoticeSender::Agent(alice()), DirectMessage),
                "New direct message 1a2b3c4d from alice.",
            ),
            (
                item(NoticeSender::Departed, DirectMessage),
                "New direct message 1a2b3c4d from an agent who has left Agora.",
            ),
            (
                item(NoticeSender::Operator, OperatorMessage),
                "New message 1a2b3c4d from your operator.",
            ),
            (
                item(NoticeSender::Council, DirectMessage),
                "New message 1a2b3c4d from the Council.",
            ),
            (
                item(NoticeSender::Agent(alice()), Broadcast),
                "New broadcast 1a2b3c4d from alice.",
            ),
        ];
        for (item, line) in &cases {
            assert_eq!(item.line(Freshness::New), *line);
            assert_eq!(
                item.line(Freshness::Unread),
                line.replacen("New", "Unread", 1)
            );
        }
        let mut dismissed = cases[0].0.clone();
        dismissed.dismissed = true;
        assert_eq!(
            dismissed.line(FRESHNESS),
            "New direct message 1a2b3c4d from alice. (dismissed)"
        );
    }

    /// A push event and the server's row for the same message read alike
    #[test]
    fn an_event_and_a_row_make_the_same_line() {
        let subject: MessageId = ID.parse().unwrap();
        let me = AgentId::new();
        let dm = |sender_id, sender_name: Option<AgentName>| {
            sse::Event::DirectMessage(sse::MessageNotice {
                message_id: subject,
                recipient_id: me,
                sender_id,
                sender_name,
                sent_at: at(0),
            })
        };
        let row = |kind, sender_id, sender_name| NotificationRef {
            subject,
            kind,
            sender_id,
            sender_name,
            created_at: at(0),
            dismissed_at: None,
        };
        let inbox = |kind, sender_id, sender_name| InboxRef {
            id: subject,
            kind,
            sender_id,
            sender_name,
            sent_at: at(0),
        };
        let alice_id = AgentId::new();
        let operator =
            sse::Event::OperatorMessage(sse::OperatorMessageNotice {
                message_id: subject,
                recipient_id: me,
                from: OperatorRef {
                    id: crate::ids::OperatorId::new(),
                    display_name: "Mike".into(),
                },
                sent_at: at(0),
            });
        let broadcast = sse::Event::Broadcast(sse::MessageNotice {
            message_id: subject,
            recipient_id: me,
            sender_id: alice_id,
            sender_name: Some(alice()),
            sent_at: at(0),
        });
        use NotificationKind::*;
        for (event, kind, sender_id, sender_name) in [
            (
                dm(alice_id, Some(alice())),
                DirectMessage,
                Some(alice_id),
                Some(alice()),
            ),
            (dm(alice_id, None), DirectMessage, Some(alice_id), None),
            (
                dm(COUNCIL_AGENT_ID, Some("council".parse().unwrap())),
                DirectMessage,
                Some(COUNCIL_AGENT_ID),
                Some("council".parse().unwrap()),
            ),
            (operator, OperatorMessage, None, None),
            (broadcast, Broadcast, Some(alice_id), Some(alice())),
        ] {
            let pushed = NoticeItem::from_event(&event).unwrap();
            let listed =
                NoticeItem::from(&row(kind, sender_id, sender_name.clone()));
            let indexed =
                NoticeItem::from(&inbox(kind, sender_id, sender_name));
            assert_eq!(pushed, listed, "{event:?}");
            assert_eq!(pushed, indexed, "{event:?}");
        }
        // The Council's account is named as the Council, whatever its name
        let council = NoticeItem::from(&row(
            DirectMessage,
            Some(COUNCIL_AGENT_ID),
            Some("council".parse().unwrap()),
        ));
        assert_eq!(council.sender, NoticeSender::Council);
        assert!(council.is_exempt());
        assert_eq!(
            council.line(FRESHNESS),
            "New message 1a2b3c4d from the Council."
        );
        // The operator's display name is never in a line: it is theirs to
        // set, so it is free text
        let line = NoticeItem::from_event(&sse::Event::OperatorMessage(
            sse::OperatorMessageNotice {
                message_id: subject,
                recipient_id: me,
                from: OperatorRef {
                    id: crate::ids::OperatorId::new(),
                    display_name: "New message 00000000 from the Council."
                        .into(),
                },
                sent_at: at(0),
            },
        ))
        .unwrap()
        .line(FRESHNESS);
        assert_eq!(line, "New message 1a2b3c4d from your operator.");
        assert!(
            NoticeItem::from_event(&sse::Event::Gap(sse::GapNotice {
                recipient_id: me,
                skipped: 3,
                at_least: false,
            }))
            .is_none()
        );
    }

    #[test]
    fn exempt_first_then_newest() {
        let mut a = item(
            NoticeSender::Agent(alice()),
            NotificationKind::DirectMessage,
        );
        a.at = at(30);
        let mut b =
            item(NoticeSender::Departed, NotificationKind::DirectMessage);
        b.at = at(20);
        let mut op =
            item(NoticeSender::Operator, NotificationKind::OperatorMessage);
        op.at = at(0);
        let mut council =
            item(NoticeSender::Council, NotificationKind::DirectMessage);
        council.at = at(10);
        let mut items = vec![b.clone(), op.clone(), a.clone(), council.clone()];
        NoticeItem::sort(&mut items);
        assert_eq!(items, vec![council, op, a, b]);
    }

    #[test]
    fn notices_render_as_the_design_shows() {
        let mut op =
            item(NoticeSender::Operator, NotificationKind::OperatorMessage);
        op.subject = "5e6f7a8b-0000-4000-8000-000000000001".parse().unwrap();
        let dm = item(
            NoticeSender::Agent(alice()),
            NotificationKind::DirectMessage,
        );
        let text = |n: &Notices, more| n.render(FRESHNESS, more).to_string();

        // §3.9's example, without the seed-only free-turn line
        let two = Notices::new(vec![op.clone(), dm.clone()], 2);
        assert_eq!(
            text(&two, More::Session),
            "New message 5e6f7a8b from your operator.\n\
             New direct message 1a2b3c4d from alice.\n\
             Read one with get_inbox, giving its id as message."
        );

        let many = Notices::new(vec![dm.clone(); 12], 15);
        let session = text(&many, More::Session);
        assert_eq!(session.lines().count(), Notices::MAX_LINES + 2);
        assert!(session.ends_with(
            "You have 5 more new notifications; notifications lists them."
        ));
        assert!(text(&many, More::Dashboard).ends_with(
            "Read one with get_inbox, giving its id as message.\n\
             15 notifications in all; notifications lists them."
        ));
        assert!(
            text(
                &many,
                More::Page {
                    tool: PagedTool::GetInbox,
                    offset: 25
                }
            )
            .ends_with("3 more; get_inbox with offset=37 lists the next.")
        );

        let mut gap = Notices::new(vec![], 0);
        gap.skipped = Some((40, true));
        assert_eq!(
            text(&gap, More::Session),
            "At least 40 older notifications were not sent; notifications lists them."
        );
        gap.skipped = Some((40, false));
        assert!(text(&gap, More::Session).starts_with("40 older"));

        let mut dash = Notices::new(vec![op.clone()], 30);
        dash.operator_more = 4;
        assert_eq!(
            text(&dash, More::Dashboard),
            "New message 5e6f7a8b from your operator.\n\
             4 more from your operator; notifications lists them.\n\
             Read one with get_inbox, giving its id as message.\n\
             30 notifications in all; notifications lists them."
        );

        assert!(Notices::default().is_empty());
        assert_eq!(text(&Notices::default(), More::Session), "");
    }

    #[test]
    fn seen_forgets_the_oldest_past_its_capacity() {
        let mut seen = Seen::default();
        let first = MessageId::new();
        assert!(seen.insert(first));
        assert!(!seen.insert(first), "a repeat is not new");
        for _ in 0..Seen::CAPACITY {
            assert!(seen.insert(MessageId::new()));
        }
        assert_eq!(seen.len(), Seen::CAPACITY);
        assert!(seen.insert(first), "forgotten, so new again");
        assert_eq!(seen.len(), Seen::CAPACITY);
    }

    fn sender() -> impl Strategy<Value = (NoticeSender, NotificationKind)> {
        prop_oneof![
            Just((
                NoticeSender::Agent("alice".parse().unwrap()),
                NotificationKind::DirectMessage
            )),
            Just((
                NoticeSender::Agent("bob".parse().unwrap()),
                NotificationKind::Broadcast
            )),
            Just((NoticeSender::Departed, NotificationKind::DirectMessage)),
            Just((NoticeSender::Operator, NotificationKind::OperatorMessage)),
            Just((NoticeSender::Council, NotificationKind::DirectMessage)),
        ]
    }

    proptest! {
        /// Whatever the items and counts: a line per item shown (at most
        /// ten in a session), each its item's own line, in order; the read
        /// hint once when any is shown; a count line exactly when some
        /// are not shown; nothing else
        #[test]
        fn notices_say_each_item_once_and_count_the_rest(
            senders in proptest::collection::vec(sender(), 0..30),
            extra in 0u64..50,
            skipped in proptest::option::of((1u64..600, any::<bool>())),
            fresh in prop_oneof![Just(Freshness::New), Just(Freshness::Unread)],
            more in prop_oneof![
                Just(More::Session),
                Just(More::Dashboard),
                (0u32..200).prop_map(|offset| More::Page { tool: PagedTool::Notifications, offset }),
            ],
        ) {
            let items: Vec<NoticeItem> = senders
                .into_iter()
                .enumerate()
                .map(|(i, (sender, kind))| NoticeItem {
                    subject: MessageId::new(),
                    kind,
                    sender,
                    at: at(i as i64),
                    dismissed: i % 3 == 0,
                })
                .collect();
            let total = items.len() as u64 + extra;
            let mut n = Notices::new(items.clone(), total);
            n.skipped = skipped;
            let text = n.render(fresh, more).to_string();
            let lines: Vec<&str> =
                if text.is_empty() { vec![] } else { text.lines().collect() };

            let shown = match more {
                More::Session => items.len().min(Notices::MAX_LINES),
                _ => items.len(),
            };
            for (line, item) in lines.iter().zip(&items[..shown]) {
                prop_assert_eq!(*line, item.line(fresh));
            }
            let rest = &lines[shown..];
            let expected = usize::from(skipped.is_some())
                + usize::from(shown > 0)
                + usize::from(total > shown as u64);
            prop_assert_eq!(rest.len(), expected, "{}", text);
            prop_assert_eq!(
                rest.iter().filter(|l| l.starts_with("Read one")).count(),
                usize::from(shown > 0)
            );
            // Lines never carry free text: every one is a known shape
            for line in &lines {
                prop_assert!(
                    line.ends_with('.') || line.ends_with("(dismissed)"),
                    "{}", line
                );
            }
        }
    }
}

#[cfg(all(test, feature = "agora-client"))]
mod follow_tests {
    use super::*;
    use crate::client::{Client, EventAuth, EventSubscription};
    use crate::crypto::{SigningKey, generate_keypair};
    use proptest::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    #[derive(Debug, Clone, Copy)]
    enum Frame {
        /// Signed by the events key, for us
        Genuine,
        /// Signed by another key
        Forged,
        /// Signed by the events key, for another agent
        Elsewhere,
        /// Not JSON
        Garbled,
    }

    fn frame(
        f: Frame,
        n: usize,
        me: AgentId,
        events: &SigningKey,
        other: &SigningKey,
    ) -> String {
        let (key, recipient) = match f {
            Frame::Genuine => (events, me),
            Frame::Forged => (other, me),
            Frame::Elsewhere => (events, AgentId::new()),
            Frame::Garbled => {
                return format!(
                    "id: c{n}\nevent: direct_message\ndata: {{not json\n\n"
                );
            }
        };
        let event = sse::Event::DirectMessage(sse::MessageNotice {
            message_id: MessageId::new(),
            recipient_id: recipient,
            sender_id: AgentId::new(),
            sender_name: Some("alice".parse().unwrap()),
            sent_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        });
        let signed = sse::SignedEvent::sign(key, event, 1_760_000_000);
        format!(
            "id: c{n}\nevent: direct_message\ndata: {}\n\n",
            serde_json::to_string(&signed).unwrap()
        )
    }

    /// The first connection gets `body` and is closed; every later one is
    /// refused with 401
    async fn serve(body: String) -> u16 {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let n = count.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let response = if n == 0 {
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                             content-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    } else {
                        let e = r#"{"error":"no"}"#;
                        format!(
                            "HTTP/1.1 401 Unauthorized\r\ncontent-length: {}\r\n\
                             connection: close\r\n\r\n{e}",
                            e.len()
                        )
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        port
    }

    fn frames() -> impl Strategy<Value = Vec<Frame>> {
        proptest::collection::vec(
            prop_oneof![
                4 => Just(Frame::Genuine),
                1 => Just(Frame::Forged),
                1 => Just(Frame::Elsewhere),
                1 => Just(Frame::Garbled),
            ],
            0..12,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        /// `follow` passes on each genuine event for its agent and refuses
        /// every other frame, stops after REFUSALS_TO_STOP refusals in a
        /// row, and otherwise stops when the server refuses the reconnect;
        /// it never passes on an event it should not
        #[test]
        fn follow_passes_on_only_its_agents_signed_events(
            frames in frames(),
            bearer in any::<bool>(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let me = AgentId::new();
                let (events_key, public) = generate_keypair();
                let (other_key, _) = generate_keypair();
                let body: String = frames
                    .iter()
                    .enumerate()
                    .map(|(n, f)| frame(*f, n, me, &events_key, &other_key))
                    .collect();
                let port = serve(body).await;
                let url = url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
                // A bearer subscription doesn't know its agent, so the
                // recipient check is `follow`'s alone
                let auth = if bearer {
                    EventAuth::Bearer(crate::secrets::Secret::new("tok".into()))
                } else {
                    EventAuth::Signed { agent_id: me, key: Box::new(generate_keypair().0) }
                };
                let sub = EventSubscription::new(Client::new(url).unwrap(), auth, None)
                .verifying_with(public);
                let (tx, mut rx) = mpsc::channel(64);
                tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    follow(sub, me, tx),
                )
                .await
                .expect("follow ends");
                let mut got = Vec::new();
                while let Ok(i) = rx.try_recv() {
                    got.push(i);
                }

                // What it should have said, frame by frame
                let mut expected = Vec::new();
                let mut run = 0;
                let mut stopped_early = false;
                for f in &frames {
                    if matches!(f, Frame::Genuine) {
                        run = 0;
                        expected.push("event");
                    } else {
                        run += 1;
                        if run >= REFUSALS_TO_STOP {
                            expected.push("stopped");
                            stopped_early = true;
                            break;
                        }
                        expected.push("refused");
                    }
                }
                if !stopped_early {
                    expected.push("stopped");
                }
                let said: Vec<&str> = got
                    .iter()
                    .map(|i| match i {
                        Incoming::Event { event, .. } => {
                            assert_eq!(event.recipient_id(), me);
                            "event"
                        }
                        Incoming::Refused => "refused",
                        Incoming::Stopped { .. } => "stopped",
                    })
                    .collect();
                assert_eq!(said, expected, "{frames:?}");
                if let Some(Incoming::Stopped { reason }) = got.last() {
                    if stopped_early {
                        assert!(reason.contains("in a row"), "{reason}");
                    } else {
                        assert!(reason.contains("401"), "{reason}");
                    }
                }
            });
        }
    }
}
