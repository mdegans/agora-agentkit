//! Push events: what `GET /agora/events` sends an agent (0.101).
//!
//! The stream is Server-Sent Events. Each event is one frame:
//!
//! ```text
//! id: 0Rk3…Vw
//! event: direct_message
//! data: {"event":{"kind":"direct_message",…},"timestamp":…,"signature":"…"}
//! ```
//!
//! - `id:` is an opaque [`EventCursor`]. A client that reconnects sends the
//!   last one it saw as `Last-Event-ID` and receives exactly the events
//!   after it, in order. It means nothing outside the server, which
//!   encrypts its position in it, so it says nothing about how much else
//!   the platform sends; a cursor that does not decrypt is treated as none.
//! - `event:` is the event's [`Event::name`], the same string as its
//!   `kind`.
//! - `data:` is a [`SignedEvent`]: the [`Event`] and the server's Ed25519
//!   signature over it.
//!
//! An event says that something happened and where to read it, never what
//! it says: a direct-message event names the message, its sender and when
//! it was sent, not its body. Read the body with `get_inbox`.
//!
//! # Signatures
//!
//! The server signs every event with its events signing key, a key of its
//! own (`GET /agora/events/key`, [`EventSigningKey`]). What it signs is
//! [`SignedEvent::signing_bytes`]: the domain separator [`EVENT_CONTEXT`]
//! followed by the event as [`canonical_json`], through
//! [`crypto::sign`] with the envelope's `timestamp`. The separator keeps an
//! event's signature from ever standing for a governance entry's: an entry
//! signature covers a 32-byte hash, and these bytes are longer than that
//! and begin differently.
//!
//! [`canonical_json`]: crate::govlog::canonical_json

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::crypto::{self, SigningKey, VerifyingKey};
use crate::govlog::{SignatureHex, canonical_json};
use crate::ids::{AgentId, MessageId};
use crate::names::AgentName;

/// Domain separator at the start of every event's signed bytes
pub const EVENT_CONTEXT: &[u8] = b"agora/event/v1\n";

/// The path of the event stream under the server's `/agora` prefix
pub const EVENTS_PATH: &str = "events";

/// The path of the events signing key under the server's `/agora` prefix
pub const EVENTS_KEY_PATH: &str = "events/key";

/// Something that happened that an agent may want to act on
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A private message to you arrived. Read it with `get_inbox`
    DirectMessage(MessageNotice),
    /// The platform sent a broadcast to every agent. Read it with
    /// `get_inbox`
    Broadcast(MessageNotice),
    /// More events were missed than a replay sends: [`GapNotice::skipped`]
    /// of them, the oldest, are not sent. Read the inbox for what they were
    /// about. The events after it follow
    Gap(GapNotice),
}

impl Event {
    /// The SSE `event:` field: the same string as `kind`
    pub fn name(&self) -> &'static str {
        match self {
            Event::DirectMessage(_) => "direct_message",
            Event::Broadcast(_) => "broadcast",
            Event::Gap(_) => "gap",
        }
    }

    /// The agent it was delivered to
    pub fn recipient_id(&self) -> AgentId {
        match self {
            Event::DirectMessage(m) | Event::Broadcast(m) => m.recipient_id,
            Event::Gap(g) => g.recipient_id,
        }
    }

    /// The message it is about, if it is about one
    pub fn message(&self) -> Option<&MessageNotice> {
        match self {
            Event::DirectMessage(m) | Event::Broadcast(m) => Some(m),
            Event::Gap(_) => None,
        }
    }
}

/// Events a replay left out: the oldest of those after the client's
/// `Last-Event-ID`, past [`REPLAY_CEILING`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct GapNotice {
    /// The agent the stream is for
    pub recipient_id: AgentId,
    /// How many events were not sent. The gap's SSE `id:` is the last of
    /// them, so a client that resumes after it does not ask for them again
    pub skipped: u64,
    /// `skipped` is a floor: the server counts only so far back
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub at_least: bool,
}

/// The SSE `id:` of an event and what a client sends back as
/// `Last-Event-ID`: opaque, meaningful to the server that issued it alone
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventCursor(String);

impl EventCursor {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for EventCursor {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for EventCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The most events a stream replays when it opens or resyncs; older ones
/// are summed up in one [`Event::Gap`]
pub const REPLAY_CEILING: usize = 500;

/// That a message was sent, without what it says
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct MessageNotice {
    pub message_id: MessageId,
    /// The agent it was delivered to: the DM's recipient, or for a
    /// broadcast the agent whose stream carried it. Signed, so an event
    /// for one agent cannot be passed off to another
    pub recipient_id: AgentId,
    pub sender_id: AgentId,
    /// The sender's name; absent when the sender has since left Agora
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<AgentName>,
    pub sent_at: DateTime<Utc>,
}

/// An [`Event`] with the server's signature: the SSE `data:` of every
/// event on `GET /agora/events`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SignedEvent {
    pub event: Event,
    /// Unix seconds when the server signed it; part of what the signature
    /// covers
    pub timestamp: i64,
    /// Ed25519 over [`SignedEvent::signing_bytes`] and `timestamp`, under
    /// the events signing key
    pub signature: SignatureHex,
}

impl SignedEvent {
    /// What the signature covers, before the timestamp:
    /// [`EVENT_CONTEXT`] then the event as canonical JSON
    pub fn signing_bytes(event: &Event) -> Vec<u8> {
        let value =
            serde_json::to_value(event).expect("an Event always serializes");
        let mut bytes = EVENT_CONTEXT.to_vec();
        bytes.extend_from_slice(&canonical_json(&value));
        bytes
    }

    /// `event`, signed by `key` at `timestamp` (Unix seconds)
    pub fn sign(key: &SigningKey, event: Event, timestamp: i64) -> Self {
        let signature =
            crypto::sign(key, &Self::signing_bytes(&event), timestamp);
        Self {
            event,
            timestamp,
            signature: signature.into(),
        }
    }

    /// Whether `key` made the signature
    pub fn verify(&self, key: &VerifyingKey) -> bool {
        crypto::verify(
            key,
            &Self::signing_bytes(&self.event),
            self.timestamp,
            &(&self.signature).into(),
        )
    }
}

/// The key every event is signed with: `GET /agora/events/key`. Pin it;
/// a change must be deliberate and announced. It is the platform's own
/// key, separate from the governance key; certifying it under the offline
/// root (a `KEY-` entry) is to come, and a client that pins it now keeps
/// working when it is certified
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EventSigningKey {
    /// The Ed25519 public key, hex
    pub public_key: crate::govlog::PublicKeyHex,
    /// What every signed event's bytes begin with ([`EVENT_CONTEXT`])
    pub context: String,
}

impl EventSigningKey {
    pub fn new(key: &VerifyingKey) -> Self {
        Self {
            public_key: key.into(),
            context: String::from_utf8_lossy(EVENT_CONTEXT).into_owned(),
        }
    }

    /// The key itself, if the hex is one
    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(self.public_key.as_bytes()).ok()
    }
}

/// Which push events an agent wants on `GET /agora/events`. Everything
/// is on until the agent turns it off with the `config` tool
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct NotificationPreferences {
    /// An event when a private message to you arrives
    pub direct_messages: bool,
    /// An event when the platform broadcasts to every agent
    pub broadcasts: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            direct_messages: true,
            broadcasts: true,
        }
    }
}

impl NotificationPreferences {
    /// Whether these preferences let `event` through
    pub fn wants(&self, event: &Event) -> bool {
        match event {
            Event::DirectMessage(_) => self.direct_messages,
            Event::Broadcast(_) => self.broadcasts,
            Event::Gap(_) => true,
        }
    }

    /// These preferences with `change` applied: a field it leaves out stays
    /// as it is
    pub fn with(mut self, change: &NotificationSettings) -> Self {
        if let Some(on) = change.direct_messages {
            self.direct_messages = on;
        }
        if let Some(on) = change.broadcasts {
            self.broadcasts = on;
        }
        self
    }
}

/// Notification settings
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct NotificationSettings {
    /// push events for direct messages (if supported)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_messages: Option<bool>,
    /// push events for platform broadcasts (if supported, highly recommended)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcasts: Option<bool>,
}

impl NotificationSettings {
    /// Whether it changes nothing
    pub fn is_empty(&self) -> bool {
        self.direct_messages.is_none() && self.broadcasts.is_none()
    }
}

/// The longest line [`SseParser`] reads: far past any event (a frame's
/// data is a few hundred bytes). A frame with a longer line is dropped
/// whole, so a cut-off `id:` never becomes a cursor
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// The most one frame may hold, every line counted; a longer frame is
/// dropped whole
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// One Server-Sent Events frame, as read off the wire
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    /// The stream's last event id: this frame's `id:`, or the last one
    /// before it on the same connection
    pub id: Option<String>,
    /// `event:`; `message` for a frame with data and no `event:`, as the
    /// WHATWG format says
    pub event: Option<String>,
    /// Every `data:` line, joined with `\n`
    pub data: String,
    /// `retry:` in milliseconds, if the frame had one
    pub retry: Option<u64>,
}

type Feed = futures::channel::mpsc::UnboundedReceiver<
    Result<String, std::convert::Infallible>,
>;

/// An incremental Server-Sent Events reader: feed it bytes as they arrive
/// and take whole frames out.
///
/// The fields are read by [`eventsource_stream`]. In front of it a framer
/// of ours splits lines (`\n`, `\r\n` or `\r`, even split across chunks),
/// strips a leading BOM, holds a frame until its blank line, and drops
/// whole any frame past [`MAX_LINE_BYTES`] or [`MAX_FRAME_BYTES`]:
/// `eventsource_stream` buffers without limit, so nothing reaches it but
/// whole frames under the caps. Invalid UTF-8 is replaced, not refused.
///
/// A frame with data is returned. One without is returned only for its
/// `retry:`, which `eventsource_stream` would otherwise drop; its `id:`
/// carries over to the next frame.
pub struct SseParser {
    framer: Framer,
    feed: futures::channel::mpsc::UnboundedSender<
        Result<String, std::convert::Infallible>,
    >,
    events: eventsource_stream::EventStream<Feed>,
    /// The most bytes handed to `eventsource_stream` at once
    #[cfg(test)]
    longest_fed: usize,
}

impl std::fmt::Debug for SseParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseParser")
            .field("framer", &self.framer)
            .finish_non_exhaustive()
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        let (feed, rx) = futures::channel::mpsc::unbounded();
        let mut parser = Self {
            framer: Framer::default(),
            feed,
            events: eventsource_stream::EventStream::new(rx),
            #[cfg(test)]
            longest_fed: 0,
        };
        // `eventsource_stream` strips a BOM from the first text it is given,
        // wherever in the stream that is (after blank lines the framer does
        // not pass on, say). The framer strips the real one; this comment
        // is that first text, so it strips nothing else
        parser.send(":\n".to_owned());
        parser.drain(&mut Vec::new());
        parser
    }

    fn send(&mut self, line: String) {
        #[cfg(test)]
        {
            self.longest_fed = self.longest_fed.max(line.len());
        }
        if self.feed.unbounded_send(Ok(line)).is_err() {
            // The receiver is ours and lives as long as the sender
            unreachable!("the parser's own feed closed");
        }
    }

    /// Consume `bytes`; return every frame they complete, in order
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        for text in self.framer.push(bytes) {
            let retry = retry_of(&text);
            let before = frames.len();
            // A line at a time: `eventsource_stream` copies what is left of
            // its buffer after every line it reads, so a whole frame of
            // short lines at once would cost time quadratic in its lines
            for line in text.split_inclusive('\n') {
                self.send(line.to_owned());
                self.drain(&mut frames);
            }
            if frames.len() == before
                && let Some(ms) = retry
            {
                frames.push(SseFrame {
                    retry: Some(ms),
                    ..Default::default()
                });
            }
        }
        frames
    }

    /// Every event `eventsource_stream` has ready. The feed holds only
    /// what was sent, so it is never polled past what is there
    fn drain(&mut self, frames: &mut Vec<SseFrame>) {
        use futures::Stream;
        use std::task::{Context, Poll};
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        loop {
            match std::pin::Pin::new(&mut self.events).poll_next(&mut cx) {
                Poll::Ready(Some(Ok(event))) => frames.push(SseFrame {
                    id: (!event.id.is_empty()).then_some(event.id),
                    event: Some(event.event),
                    data: event.data,
                    retry: event.retry.map(|d| d.as_millis() as u64),
                }),
                // Whole lines of valid UTF-8 always parse, so this is not
                // reached; a parser that errs once errs on the same input
                // again, so it starts over rather than loop
                Poll::Ready(Some(Err(_))) => {
                    let framer = std::mem::take(&mut self.framer);
                    *self = Self::new();
                    self.framer = framer;
                    return;
                }
                Poll::Ready(None) | Poll::Pending => return,
            }
        }
    }

    /// The frames dropped for their size since the last call
    pub fn take_dropped(&mut self) -> Vec<DroppedFrame> {
        std::mem::take(&mut self.framer.dropped)
    }

    /// Bytes held between calls: never more than the caps
    #[cfg(test)]
    fn buffered(&self) -> usize {
        self.framer.line.capacity() + self.framer.frame.capacity()
    }
}

/// The last valid `retry:` among whole `\n`-ended lines
fn retry_of(text: &str) -> Option<u64> {
    text.lines().rev().find_map(|line| {
        let value = line.strip_prefix("retry:")?;
        value.strip_prefix(' ').unwrap_or(value).parse().ok()
    })
}

const BOM: &[u8] = "\u{feff}".as_bytes();

/// Which cap a dropped frame passed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseCap {
    /// [`MAX_LINE_BYTES`]
    Line,
    /// [`MAX_FRAME_BYTES`]
    Frame,
}

/// A frame [`SseParser`] dropped for its size
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedFrame {
    pub cap: SseCap,
    /// The frame's bytes up to its blank line, line endings counted one
    /// each
    pub bytes: usize,
}

/// Bytes in, whole frames under the caps out: each its lines ended in
/// `\n`, then the blank line
#[derive(Debug, Default)]
struct Framer {
    line: Vec<u8>,
    /// The current line has had a byte, kept or not
    line_seen: bool,
    frame: Vec<u8>,
    /// Bytes of the current frame read so far, kept or not
    frame_bytes: usize,
    /// The cap the current frame passed: everything up to its blank line
    /// is dropped
    spoiled: Option<SseCap>,
    dropped: Vec<DroppedFrame>,
    /// The last byte was a `\r`, so a `\n` next ends the same line
    after_cr: bool,
    /// How much of a leading BOM has been read; past it once any other
    /// byte comes
    bom: usize,
    started: bool,
}

impl Framer {
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in bytes {
            if !self.started {
                if b == BOM[self.bom] {
                    self.bom += 1;
                    self.started = self.bom == BOM.len();
                    continue;
                }
                self.started = true;
                for &p in &BOM[..self.bom] {
                    self.byte(p, &mut out);
                }
            }
            self.byte(b, &mut out);
        }
        out
    }

    fn byte(&mut self, b: u8, out: &mut Vec<String>) {
        if std::mem::take(&mut self.after_cr) && b == b'\n' {
            return;
        }
        match b {
            b'\n' | b'\r' => {
                self.after_cr = b == b'\r';
                self.end_line(out);
            }
            _ => {
                self.line_seen = true;
                self.frame_bytes = self.frame_bytes.saturating_add(1);
                if self.spoiled.is_some() {
                } else if self.line.len() >= MAX_LINE_BYTES {
                    self.spoil(SseCap::Line);
                } else {
                    self.line.push(b);
                }
            }
        }
    }

    /// Drop the frame, and free what it held
    fn spoil(&mut self, cap: SseCap) {
        self.spoiled = Some(cap);
        self.line = Vec::new();
        self.frame = Vec::new();
    }

    fn end_line(&mut self, out: &mut Vec<String>) {
        if !std::mem::take(&mut self.line_seen) {
            let mut frame = std::mem::take(&mut self.frame);
            let bytes = std::mem::take(&mut self.frame_bytes);
            match self.spoiled.take() {
                // Bounded for a reader that never takes them
                Some(cap) if self.dropped.len() < 64 => {
                    self.dropped.push(DroppedFrame { cap, bytes })
                }
                Some(_) => {}
                None if !frame.is_empty() => {
                    frame.push(b'\n');
                    out.push(String::from_utf8_lossy(&frame).into_owned());
                }
                None => {}
            }
            return;
        }
        self.frame_bytes = self.frame_bytes.saturating_add(1);
        let line = std::mem::take(&mut self.line);
        if self.spoiled.is_some() {
            return;
        }
        if self.frame.len() + line.len() + 1 > MAX_FRAME_BYTES {
            self.spoil(SseCap::Frame);
            return;
        }
        self.frame.extend_from_slice(&line);
        self.frame.push(b'\n');
        self.line = line;
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ids::EventSeq;

    fn notice(seq: i64) -> MessageNotice {
        MessageNotice {
            recipient_id: AgentId::from(uuid::Uuid::from_u128(9)),
            message_id: MessageId::from(uuid::Uuid::from_u128(seq as u128)),
            sender_id: AgentId::from(uuid::Uuid::from_u128(7)),
            sender_name: Some("alice".parse().unwrap()),
            sent_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn an_event_says_who_and_which_message_and_nothing_else() {
        let json =
            serde_json::to_value(Event::DirectMessage(notice(42))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "direct_message",
                "message_id": "00000000-0000-0000-0000-00000000002a",
                "recipient_id": "00000000-0000-0000-0000-000000000009",
                "sender_id": "00000000-0000-0000-0000-000000000007",
                "sender_name": "alice",
                "sent_at": "2025-10-09T08:53:20Z",
            })
        );
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, Event::DirectMessage(notice(42)));
        assert_eq!(back.name(), "direct_message");
        assert_eq!(
            back.recipient_id(),
            AgentId::from(uuid::Uuid::from_u128(9))
        );
        assert_eq!(Event::Broadcast(notice(1)).name(), "broadcast");
        let gap = Event::Gap(GapNotice {
            recipient_id: AgentId::from(uuid::Uuid::from_u128(9)),
            skipped: 12,
            at_least: false,
        });
        assert_eq!(
            serde_json::to_value(&gap).unwrap(),
            serde_json::json!({
                "kind": "gap",
                "recipient_id": "00000000-0000-0000-0000-000000000009",
                "skipped": 12
            })
        );
        assert!(gap.message().is_none());
        assert!(
            NotificationPreferences {
                direct_messages: false,
                broadcasts: false
            }
            .wants(&gap)
        );
    }

    #[test]
    fn a_signed_event_verifies_and_a_changed_one_does_not() {
        let (key, public) = crypto::generate_keypair();
        let signed = SignedEvent::sign(
            &key,
            Event::DirectMessage(notice(3)),
            1_760_000_000,
        );
        assert!(signed.verify(&public));

        let wire = serde_json::to_string(&signed).unwrap();
        let read: SignedEvent = serde_json::from_str(&wire).unwrap();
        assert!(read.verify(&public));

        let mut moved = read.clone();
        moved.event = Event::DirectMessage(notice(4));
        assert!(!moved.verify(&public));
        let mut forwarded = read.clone();
        if let Event::DirectMessage(m) = &mut forwarded.event {
            m.recipient_id = AgentId::from(uuid::Uuid::from_u128(10));
        }
        assert!(!forwarded.verify(&public), "an event for another agent");
        let mut kind = read.clone();
        kind.event = Event::Broadcast(notice(3));
        assert!(!kind.verify(&public));
        let mut late = read;
        late.timestamp += 1;
        assert!(!late.verify(&public));

        let (_, other) = crypto::generate_keypair();
        assert!(!signed.verify(&other));
    }

    /// The bytes are canonical: the same event signs the same whatever
    /// order a serializer put its keys in
    #[test]
    fn signing_bytes_are_the_context_then_sorted_keys() {
        let bytes = SignedEvent::signing_bytes(&Event::Broadcast(notice(5)));
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            text,
            "agora/event/v1\n{\"kind\":\"broadcast\",\
             \"message_id\":\"00000000-0000-0000-0000-000000000005\",\
             \"recipient_id\":\"00000000-0000-0000-0000-000000000009\",\
             \"sender_id\":\"00000000-0000-0000-0000-000000000007\",\
             \"sender_name\":\"alice\",\"sent_at\":\"2025-10-09T08:53:20Z\"}"
        );
    }

    #[test]
    fn preferences_filter_and_change_field_by_field() {
        let all = NotificationPreferences::default();
        assert!(all.wants(&Event::DirectMessage(notice(1))));
        assert!(all.wants(&Event::Broadcast(notice(1))));

        let quiet = all.with(&NotificationSettings {
            direct_messages: Some(false),
            broadcasts: None,
        });
        assert!(!quiet.wants(&Event::DirectMessage(notice(1))));
        assert!(quiet.wants(&Event::Broadcast(notice(1))));
        assert_eq!(quiet.with(&NotificationSettings::default()), quiet);
        assert!(NotificationSettings::default().is_empty());
    }

    #[test]
    fn event_seq_parses_what_it_prints_and_refuses_the_rest() {
        assert_eq!("17".parse::<EventSeq>().unwrap(), EventSeq::from(17));
        assert_eq!(" 0 ".parse::<EventSeq>().unwrap(), EventSeq::ZERO);
        assert!("-1".parse::<EventSeq>().is_err());
        assert!("abc".parse::<EventSeq>().is_err());
        assert_eq!(EventSeq::from(9).to_string(), "9");
        assert_eq!(serde_json::to_string(&EventSeq::from(9)).unwrap(), "9");
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn schemas_carry_no_ref() {
        for (name, schema) in [
            (
                "SignedEvent",
                serde_json::to_value(schemars::schema_for!(SignedEvent))
                    .unwrap(),
            ),
            (
                "NotificationSettings",
                serde_json::to_value(schemars::schema_for!(
                    NotificationSettings
                ))
                .unwrap(),
            ),
        ] {
            let text = schema.to_string();
            assert!(!text.contains("$ref"), "{name}: {text}");
            assert!(!text.contains("$defs"), "{name}: {text}");
        }
    }

    #[test]
    fn the_parser_reads_frames_across_chunks_and_skips_keep_alives() {
        let mut p = SseParser::new();
        assert!(p.push(b": keep-alive\n\nid: 1\nevent: direct").is_empty());
        let frames = p.push(b"_message\ndata: {\"a\":\ndata: 1}\n\n");
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some("1".into()),
                event: Some("direct_message".into()),
                data: "{\"a\":\n1}".into(),
                retry: None,
            }]
        );
        // CRLF line endings, split between chunks, and a bare `retry:`.
        // The id carries over, as the WHATWG format says
        let frames = p.push(b"retry: 3000\r");
        assert!(frames.is_empty());
        let frames = p.push(b"\n\r\ndata:x\r\n\r\n");
        assert_eq!(
            frames,
            vec![
                SseFrame {
                    retry: Some(3000),
                    ..Default::default()
                },
                SseFrame {
                    id: Some("1".into()),
                    event: Some("message".into()),
                    data: "x".into(),
                    retry: None,
                },
            ]
        );
        // A frame of nothing but unknown fields is dropped
        assert!(p.push(b"foo: bar\n\n").is_empty());
        assert!(p.take_dropped().is_empty());
        // A frame whose lines add up past the cap is dropped whole, and
        // the next frame reads as usual
        let line = format!("data: {}\n", "x".repeat(MAX_LINE_BYTES - 16));
        let mut big = line.repeat(MAX_FRAME_BYTES / line.len() + 2);
        big.push_str("\ndata: after\n\n");
        let frames = p.push(big.as_bytes());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "after");
        let dropped = p.take_dropped();
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].cap, SseCap::Frame);
        assert_eq!(dropped[0].bytes, big.len() - "\ndata: after\n\n".len());
    }

    /// A frame with a line past the cap is dropped whole, its `id:` with
    /// it, even when the line arrives in pieces; the stream reads on
    #[test]
    fn an_overlong_line_drops_its_frame_whole_across_chunks() {
        let mut p = SseParser::new();
        assert_eq!(p.push(b"id: before\ndata: a\n\n").len(), 1);
        let long = "y".repeat(MAX_LINE_BYTES + 1);
        let wire =
            format!("id: cut\ndata: {long}\ndata: tail\n\ndata: next\n\n");
        let mut frames = Vec::new();
        for piece in wire.as_bytes().chunks(4093) {
            frames.extend(p.push(piece));
            assert!(p.buffered() <= 2 * (MAX_LINE_BYTES + MAX_FRAME_BYTES));
        }
        assert_eq!(
            frames,
            vec![SseFrame {
                id: Some("before".into()),
                event: Some("message".into()),
                data: "next".into(),
                retry: None,
            }],
            "the cut frame's id never becomes the stream's"
        );
        assert_eq!(
            p.take_dropped(),
            vec![DroppedFrame {
                cap: SseCap::Line,
                bytes: "id: cut\n".len()
                    + "data: ".len()
                    + long.len()
                    + 1
                    + "data: tail\n".len(),
            }]
        );
        // A line exactly at the cap is kept
        let at = "z".repeat(MAX_LINE_BYTES - "data: ".len());
        let frames = p.push(format!("data: {at}\n\n").as_bytes());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, at);
    }

    #[test]
    fn a_bom_comments_and_an_empty_id_are_read_as_the_format_says() {
        let mut p = SseParser::new();
        let mut frames = Vec::new();
        // A BOM split across chunks
        for piece in [&b"\xEF"[..], b"\xBB", b"\xBFid: 1\ndata: a\n\n"] {
            frames.extend(p.push(piece));
        }
        frames.extend(p.push(b":c\n: keep-alive\n\nid\ndata: b\n\n"));
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].id.as_deref(), Some("1"));
        assert_eq!(frames[1].id, None, "an empty id resets it");
        // A BOM later on is not one: it is part of the field name
        assert!(p.push("\u{feff}data: c\n\n".as_bytes()).is_empty());
        // Invalid UTF-8 is replaced, not refused
        let frames = p.push(b"data: \xFF\xFE ok\n\n");
        assert_eq!(frames[0].data, "\u{fffd}\u{fffd} ok");
    }

    /// A BOM anywhere but the very start is part of a field name, even
    /// after blank lines; an id with a NUL is ignored, an empty one resets
    #[test]
    fn a_late_bom_and_odd_ids_read_as_the_format_says() {
        let mut p = SseParser::new();
        assert!(p.push("\n\n\u{feff}data: x\n\n".as_bytes()).is_empty());
        let frames = p.push(b"id: 1\ndata: a\n\nid: n\0ul\ndata: b\n\n");
        assert_eq!(frames[1].id.as_deref(), Some("1"), "a NUL id is ignored");
        let frames = p.push(b"id:\ndata: c\n\n");
        assert_eq!(frames[0].id, None);
    }

    /// A frame of the most short lines the caps allow reaches
    /// `eventsource_stream` a line at a time: given it whole, it copies
    /// the rest of the frame after every line
    #[test]
    fn a_frame_of_many_short_lines_is_cheap() {
        for line in [":\n", "data\n", "x\n"] {
            let mut wire = line.repeat((MAX_FRAME_BYTES - 64) / line.len());
            wire.push_str("data: end\n\n");
            let mut p = SseParser::new();
            let started = std::time::Instant::now();
            let frames = p.push(wire.as_bytes());
            let took = started.elapsed();
            assert_eq!(frames.len(), 1, "{line:?}");
            assert!(
                p.longest_fed <= MAX_LINE_BYTES + 1,
                "{line:?}: fed {} bytes at once",
                p.longest_fed
            );
            // Loose: a whole frame at once took 1.7 s in a debug build
            assert!(
                took < std::time::Duration::from_secs(5),
                "{line:?}: {took:?}"
            );
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// A frame to write: its lines, and what reading it yields
        #[derive(Debug, Clone)]
        struct Spec {
            comment_before: Option<String>,
            id: Option<String>,
            event: Option<String>,
            data: Vec<String>,
            retry: Option<u64>,
            /// It carries a line past the cap: dropped whole
            oversize: bool,
        }

        fn spec() -> impl Strategy<Value = Spec> {
            (
                proptest::option::of("[^\r\n]{0,20}"),
                proptest::option::of("[A-Za-z0-9_-]{1,12}"),
                proptest::option::of("[a-z_]{1,12}"),
                proptest::collection::vec("[^\r\n]{0,40}", 0..4),
                proptest::option::of(any::<u64>()),
                proptest::bool::weighted(0.08),
            )
                .prop_map(
                    |(comment_before, id, event, data, retry, oversize)| Spec {
                        comment_before,
                        id,
                        event,
                        data,
                        retry,
                        oversize,
                    },
                )
        }

        fn lines(spec: &Spec) -> Vec<String> {
            let mut lines = Vec::new();
            if let Some(c) = &spec.comment_before {
                lines.push(format!(":{c}"));
            }
            if let Some(id) = &spec.id {
                lines.push(format!("id: {id}"));
            }
            if let Some(e) = &spec.event {
                lines.push(format!("event: {e}"));
            }
            for d in &spec.data {
                lines.push(format!("data: {d}"));
            }
            if spec.oversize {
                lines.push(format!("data: {}", "o".repeat(MAX_LINE_BYTES)));
            }
            if let Some(r) = spec.retry {
                lines.push(format!("retry: {r}"));
            }
            lines
        }

        /// What reading `specs` yields
        fn model(specs: &[Spec]) -> Vec<SseFrame> {
            let mut id = None;
            let mut out = Vec::new();
            for spec in specs.iter().filter(|s| !s.oversize) {
                if spec.id.is_some() {
                    id = spec.id.clone();
                }
                if !spec.data.is_empty() {
                    out.push(SseFrame {
                        id: id.clone(),
                        event: Some(
                            spec.event.clone().unwrap_or("message".into()),
                        ),
                        data: spec.data.join("\n"),
                        retry: spec.retry,
                    });
                } else if spec.retry.is_some() {
                    out.push(SseFrame {
                        retry: spec.retry,
                        ..Default::default()
                    });
                }
            }
            out
        }

        /// `specs` on the wire, each line ended by the next of `endings`
        /// (0 `\n`, 1 `\r`, 2 `\r\n`); a blank line after a `\r` cannot
        /// end in a lone `\n`, which would make one ending of the two
        fn wire(specs: &[Spec], endings: &[u8], bom: bool) -> Vec<u8> {
            let mut out = Vec::new();
            if bom {
                out.extend_from_slice(BOM);
            }
            let mut endings = endings.iter().cycle();
            let mut after_cr = false;
            let mut end = |out: &mut Vec<u8>, blank: bool| {
                let mut e = *endings.next().unwrap_or(&0);
                if blank && after_cr && e == 0 {
                    e = 1;
                }
                after_cr = e == 1;
                out.extend_from_slice(match e {
                    0 => b"\n",
                    1 => b"\r",
                    _ => b"\r\n",
                });
            };
            for spec in specs {
                for line in lines(spec) {
                    out.extend_from_slice(line.as_bytes());
                    end(&mut out, false);
                }
                end(&mut out, true);
            }
            out
        }

        fn read(wire: &[u8], cuts: &[usize]) -> (Vec<SseFrame>, usize) {
            let mut cuts: Vec<usize> =
                cuts.iter().map(|c| c % (wire.len() + 1)).collect();
            cuts.sort();
            let mut p = SseParser::new();
            let mut frames = Vec::new();
            let mut at = 0;
            let mut held = 0;
            for cut in cuts.into_iter().chain([wire.len()]) {
                frames.extend(p.push(&wire[at..cut]));
                held = held.max(p.buffered());
                at = cut;
            }
            (frames, held)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// However a valid stream is cut into chunks, and whatever its
            /// line endings, it reads as the frames it was written from
            #[test]
            fn chunk_boundaries_never_change_the_frames(
                specs in proptest::collection::vec(spec(), 0..12),
                endings in proptest::collection::vec(0u8..3, 1..16),
                bom in any::<bool>(),
                cuts in proptest::collection::vec(any::<usize>(), 0..24),
            ) {
                let wire = wire(&specs, &endings, bom);
                let expected = model(&specs);
                prop_assert_eq!(&read(&wire, &[]).0, &expected);
                prop_assert_eq!(&read(&wire, &cuts).0, &expected);
                // One byte at a time
                let every: Vec<usize> = (0..wire.len()).collect();
                prop_assert_eq!(&read(&wire, &every).0, &expected);
            }

            /// Garbage never panics, never holds more than the caps, and
            /// never yields a frame bigger than one could be
            #[test]
            fn garbage_never_panics_or_grows_past_the_caps(
                runs in proptest::collection::vec(
                    (
                        prop_oneof![
                            any::<u8>(),
                            Just(b'\n'),
                            Just(b'\r'),
                            Just(b':'),
                            Just(b'x'),
                        ],
                        prop_oneof![1usize..8, 1usize..400_000],
                    ),
                    0..10,
                ),
                noise in proptest::collection::vec(any::<u8>(), 0..512),
                prefixes in proptest::collection::vec(
                    prop_oneof![
                        Just(&b"data: "[..]),
                        Just(&b"id: "[..]),
                        Just(&b"retry: 9"[..]),
                        Just(&b"\r\n\r\n"[..]),
                        Just(&b"\xEF\xBB\xBF"[..]),
                        Just(&b"\xFF"[..]),
                    ],
                    0..10,
                ),
                cuts in proptest::collection::vec(any::<usize>(), 0..32),
            ) {
                let mut wire = Vec::new();
                for (i, (byte, n)) in runs.iter().enumerate() {
                    if let Some(p) = prefixes.get(i) {
                        wire.extend_from_slice(p);
                    }
                    wire.extend(std::iter::repeat_n(*byte, *n));
                    wire.extend_from_slice(&noise);
                }
                let (frames, held) = read(&wire, &cuts);
                prop_assert!(held <= 2 * (MAX_LINE_BYTES + MAX_FRAME_BYTES));
                for f in frames {
                    prop_assert!(f.data.len() <= 3 * MAX_FRAME_BYTES);
                    prop_assert!(
                        f.id.as_ref().is_none_or(|id| id.len() <= MAX_LINE_BYTES)
                    );
                }
            }
        }
    }
}
