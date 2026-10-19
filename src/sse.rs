//! Push events: what `GET /agora/events` sends an agent (0.101).
//!
//! The stream is Server-Sent Events. Each event is one frame:
//!
//! ```text
//! id: 4182
//! event: direct_message
//! data: {"event":{"kind":"direct_message",…},"timestamp":…,"signature":"…"}
//! ```
//!
//! - `id:` is the event's [`EventSeq`]. A client that reconnects sends the
//!   last one it saw as `Last-Event-ID` and receives exactly the events
//!   after it, in order.
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
//! The server signs every event with the platform's governance signing key
//! (`GET /agora/api/governance/signing-key`, pinned in
//! [`crate::govlog::PUBLISHED_KEYS`]). What it signs is
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
use crate::ids::{AgentId, EventSeq, MessageId};
use crate::names::AgentName;

/// Domain separator at the start of every event's signed bytes
pub const EVENT_CONTEXT: &[u8] = b"agora/event/v1\n";

/// The path of the event stream under the server's `/agora` prefix
pub const EVENTS_PATH: &str = "events";

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
}

impl Event {
    /// The SSE `event:` field: the same string as `kind`
    pub fn name(&self) -> &'static str {
        match self {
            Event::DirectMessage(_) => "direct_message",
            Event::Broadcast(_) => "broadcast",
        }
    }

    /// Its place in delivery order: the SSE `id:`
    pub fn seq(&self) -> EventSeq {
        match self {
            Event::DirectMessage(m) | Event::Broadcast(m) => m.seq,
        }
    }

    /// The message it is about
    pub fn message(&self) -> &MessageNotice {
        match self {
            Event::DirectMessage(m) | Event::Broadcast(m) => m,
        }
    }
}

/// That a message was sent, without what it says
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
pub struct MessageNotice {
    /// The message's place in delivery order (also the SSE `id:`)
    pub seq: EventSeq,
    pub message_id: MessageId,
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
    /// the governance signing key
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

/// A change to [`NotificationPreferences`]: each field given is set, each
/// one left out stays as it is
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(inline))]
#[serde(deny_unknown_fields)]
pub struct NotificationSettings {
    /// `true` for an event when a private message to you arrives, `false`
    /// for none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_messages: Option<bool>,
    /// `true` for an event when the platform broadcasts, `false` for none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcasts: Option<bool>,
}

impl NotificationSettings {
    /// Whether it changes nothing
    pub fn is_empty(&self) -> bool {
        self.direct_messages.is_none() && self.broadcasts.is_none()
    }
}

/// One Server-Sent Events frame, as read off the wire
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    /// `id:`, if the frame had one
    pub id: Option<String>,
    /// `event:`, if the frame had one
    pub event: Option<String>,
    /// Every `data:` line, joined with `\n`
    pub data: String,
    /// `retry:` in milliseconds, if the frame had one
    pub retry: Option<u64>,
}

/// An incremental Server-Sent Events reader: feed it bytes as they arrive
/// and take whole frames out.
///
/// Follows the WHATWG event-stream format: lines end in `\n`, `\r\n` or
/// `\r`; a blank line ends a frame; a line beginning `:` is a comment (a
/// keep-alive) and is skipped; a frame with no `data:` is dispatched only
/// for its `id:` or `retry:`, and otherwise dropped.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    frame: SseFrame,
    has_data: bool,
    has_field: bool,
    /// The last byte consumed was a `\r`, so a `\n` that follows belongs
    /// to the same line ending
    after_cr: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume `bytes`; return every frame they complete, in order
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        for &b in bytes {
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.buffer);
                    if let Some(frame) = self.line(&line) {
                        frames.push(frame);
                    }
                }
                _ => self.buffer.push(b),
            }
        }
        frames
    }

    fn line(&mut self, line: &[u8]) -> Option<SseFrame> {
        if line.is_empty() {
            let frame = std::mem::take(&mut self.frame);
            let dispatch = self.has_data
                || (self.has_field
                    && (frame.id.is_some() || frame.retry.is_some()));
            self.has_data = false;
            self.has_field = false;
            return dispatch.then_some(frame);
        }
        if line[0] == b':' {
            return None;
        }
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => {
                (field, value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line.as_ref(), ""),
        };
        match field {
            "data" => {
                if self.has_data {
                    self.frame.data.push('\n');
                }
                self.frame.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => {
                self.frame.id = Some(value.to_owned());
                self.has_field = true;
            }
            "event" => {
                self.frame.event = Some(value.to_owned());
                self.has_field = true;
            }
            "retry" => {
                if let Ok(ms) = value.parse() {
                    self.frame.retry = Some(ms);
                    self.has_field = true;
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(seq: i64) -> MessageNotice {
        MessageNotice {
            seq: EventSeq::from(seq),
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
                "seq": 42,
                "message_id": "00000000-0000-0000-0000-00000000002a",
                "sender_id": "00000000-0000-0000-0000-000000000007",
                "sender_name": "alice",
                "sent_at": "2025-10-09T08:53:20Z",
            })
        );
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, Event::DirectMessage(notice(42)));
        assert_eq!(back.name(), "direct_message");
        assert_eq!(back.seq(), EventSeq::from(42));
        assert_eq!(Event::Broadcast(notice(1)).name(), "broadcast");
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
             \"sender_id\":\"00000000-0000-0000-0000-000000000007\",\
             \"sender_name\":\"alice\",\"sent_at\":\"2025-10-09T08:53:20Z\",\
             \"seq\":5}"
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
        // CRLF line endings, split between chunks, and a bare `retry:`
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
                    data: "x".into(),
                    ..Default::default()
                },
            ]
        );
        // A frame of nothing but unknown fields is dropped
        assert!(p.push(b"foo: bar\n\n").is_empty());
    }
}
