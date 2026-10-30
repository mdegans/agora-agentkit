//! Prompt-cache stability over whole sessions: no model, no network, no spend.
//!
//! A scripted transport drives the real [`Reactor::drive_inner`], default
//! [`Agent::handle`] and [`Agent::on_turn`], `seat_notifications` and
//! `windowed`, and records the JSON [`Inference::infer`] is handed, which is
//! what goes on the wire. [`ApiCache`] and [`common_prefix`] then replay those
//! requests through two cache models.
//!
//! **The API model comes from the docs, not from billing.** It follows
//! "How automatic prefix checking works" in
//! <https://platform.claude.com/docs/en/build-with-claude/prompt-caching>, read
//! 2026-10-30. No request here has been compared with a real `usage`.
//! Assumptions the docs don't state:
//! - `cache_control` isn't part of the hash. The docs say only that the hash
//!   covers "the prefix ending at that block", and moving markers is how their
//!   own multi-turn example works.
//! - A block's hash doesn't depend on whether more blocks follow it in its
//!   message, so an entry survives a merge into that message.
//! - A bare string and a one-text-block array hash the same.
//! - A walk back inside a tool run checks every block's end for one position.
//!   Markers here only sit on a run's end, so nothing depends on it.
//! - A thinking block counts as one position.
//!
//! Not modelled: minimum cacheable sizes, entries shared between sessions or
//! agents, concurrent requests, thinking blocks the API drops (other models',
//! or earlier turns' on last-turn-only models), server tool result blocks, the
//! batch path, and the real `SeedAgent` (the agent here has its prompt shape
//! and the trait's defaults; its phases and `on_truncate` aren't driven).
//! Shares are bytes, not tokens.
//!
//! The prefix model is a stand-in for blallama: ChatML-shaped framing, not a
//! real template, and bytes, not tokens. See [`common_prefix`].

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use misanthropic::prompt::message::Content;
use misanthropic::tool::{Notification, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;
use crate::reactor::MAX_STALLS;
use crate::reactor::agent::cache::{ROLL_WINDOW, divergence, roll_breakpoints};
use crate::reactor::agent::seat_user;

/// The docs' lookback window: positions checked per marker, the marker's own
/// included.
const LOOKBACK: usize = 20;
/// The docs' per-request marker limit.
const MAX_MARKERS: usize = 4;
const ONE_HOUR: u64 = 3600;

// ---------------------------------------------------------------------------
// What a request looks like to a cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ToolUse,
    ToolResult,
    Other,
}

#[derive(Debug, Clone)]
struct WireBlock {
    kind: Kind,
    /// The block's JSON with every `cache_control` removed.
    bytes: String,
    marked: bool,
    /// A marker that isn't the 1h one the runner uses.
    other_ttl: bool,
}

#[derive(Debug, Clone)]
struct Request {
    /// What the docs say the thinking config does, and what a changed
    /// `tool_choice` does: start the prefix over.
    config: String,
    tools: Vec<WireBlock>,
    system: Vec<WireBlock>,
    messages: Vec<(String, Vec<WireBlock>)>,
}

fn strip(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip);
        }
        Value::Array(items) => items.iter_mut().for_each(strip),
        _ => {}
    }
}

fn wire_block(mut value: Value) -> WireBlock {
    let text = value.to_string();
    let marked = text.contains(r#""cache_control":"#);
    let other_ttl = marked && !text.contains(r#""ttl":"1h""#);
    let kind = match value["type"].as_str() {
        Some("tool_use") => Kind::ToolUse,
        Some("tool_result") => Kind::ToolResult,
        _ => Kind::Other,
    };
    strip(&mut value);
    WireBlock {
        kind,
        bytes: value.to_string(),
        marked,
        other_ttl,
    }
}

/// `content` as blocks, a bare string being one text block.
fn wire_blocks(content: &Value) -> Vec<WireBlock> {
    match content {
        Value::Null => Vec::new(),
        Value::String(text) => {
            vec![wire_block(json!({"type": "text", "text": text}))]
        }
        Value::Array(items) => items.iter().cloned().map(wire_block).collect(),
        other => panic!("unexpected content shape: {other}"),
    }
}

impl Request {
    fn parse(wire: &Value) -> Self {
        Self {
            config: json!([
                wire["model"],
                wire["thinking"],
                wire["tool_choice"]
            ])
            .to_string(),
            tools: wire["tools"]
                .as_array()
                .map(|t| t.iter().cloned().map(wire_block).collect())
                .unwrap_or_default(),
            system: wire_blocks(&wire["system"]),
            messages: wire["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|m| {
                    (
                        m["role"].as_str().expect("role").to_owned(),
                        wire_blocks(&m["content"]),
                    )
                })
                .collect(),
        }
    }

    /// Tools, system, messages: the docs' hierarchy.
    fn flat(&self) -> Vec<&WireBlock> {
        self.tools
            .iter()
            .chain(&self.system)
            .chain(self.messages.iter().flat_map(|(_, blocks)| blocks))
            .collect()
    }

    fn markers(&self) -> Vec<usize> {
        self.flat()
            .iter()
            .enumerate()
            .filter(|(_, b)| b.marked)
            .map(|(i, _)| i)
            .collect()
    }

    fn head_len(&self) -> usize {
        self.tools.len() + self.system.len()
    }

    fn total(&self) -> usize {
        self.flat().iter().map(|b| b.bytes.len()).sum()
    }

    /// Bytes up to and including flat block `end`.
    fn bytes_through(&self, end: usize) -> usize {
        self.flat()[..=end].iter().map(|b| b.bytes.len()).sum()
    }

    /// A cumulative hash per flat block. Roles go in, message boundaries
    /// don't: see the module's assumptions.
    fn prefix_hashes(&self) -> Vec<u64> {
        let mut hasher = DefaultHasher::new();
        self.config.hash(&mut hasher);
        let mut hashes = Vec::new();
        let mut push = |role: &str, block: &WireBlock| {
            role.hash(&mut hasher);
            block.bytes.hash(&mut hasher);
            hashes.push(hasher.finish());
        };
        for block in self.tools.iter().chain(&self.system) {
            push("", block);
        }
        for (role, blocks) in &self.messages {
            for block in blocks {
                push(role, block);
            }
        }
        hashes
    }

    /// A chat template's rendering, markers ignored.
    fn render(&self) -> String {
        let mut out = String::new();
        for block in self.tools.iter().chain(&self.system) {
            out.push_str(&block.bytes);
        }
        for (role, blocks) in &self.messages {
            out.push_str("<|im_start|>");
            out.push_str(role);
            out.push('\n');
            for block in blocks {
                out.push_str(&block.bytes);
            }
            out.push_str(CLOSE_MESSAGE);
        }
        out.push_str(GENERATION_PROMPT);
        out
    }
}

const CLOSE_MESSAGE: &str = "<|im_end|>\n";
const GENERATION_PROMPT: &str = "<|im_start|>assistant\n";

/// Longest common byte prefix: what a prefix cache keeps, markers ignored.
///
/// Real blallama compares tokens, and a recurrent or hybrid model can only
/// resume at a checkpoint (a marker or the previous tip), not at any byte. A
/// merge into the tail therefore costs such a model more than this says: back
/// to the last checkpoint, not just the closing bytes (`cache.rs`,
/// `seat_unused_reply`).
fn common_prefix(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // Whole chunks first: a byte loop over 100 KB is slow in a debug build.
    let whole = a
        .chunks(4096)
        .zip(b.chunks(4096))
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len())
        .sum::<usize>();
    whole
        + a[whole..]
            .iter()
            .zip(&b[whole..])
            .take_while(|(x, y)| x == y)
            .count()
}

// ---------------------------------------------------------------------------
// The API model
// ---------------------------------------------------------------------------

/// Entries by prefix hash, with their expiry.
#[derive(Default)]
struct ApiCache {
    entries: HashMap<u64, u64>,
    now: u64,
}

#[derive(Debug)]
struct ApiOutcome {
    /// Flat index of each marker and of what its walk found.
    hits: Vec<(usize, Option<usize>)>,
    /// The longest hit, as a flat index.
    read_end: Option<usize>,
    read_bytes: usize,
    written_bytes: usize,
}

impl ApiCache {
    fn live(&self, hash: u64) -> bool {
        self.entries.get(&hash).is_some_and(|&t| t > self.now)
    }

    /// One marker's walk: its own position first, then back, at most
    /// [`LOOKBACK`] positions, a run of `tool_use` or of `tool_result` blocks
    /// being one.
    fn walk(
        &self,
        flat: &[&WireBlock],
        hashes: &[u64],
        marker: usize,
    ) -> Option<usize> {
        let mut at = marker as isize;
        for _ in 0..LOOKBACK {
            if at < 0 {
                return None;
            }
            let kind = flat[at as usize].kind;
            loop {
                if self.live(hashes[at as usize]) {
                    return Some(at as usize);
                }
                at -= 1;
                let same_run = kind != Kind::Other
                    && at >= 0
                    && flat[at as usize].kind == kind;
                if !same_run {
                    break;
                }
            }
        }
        None
    }

    fn request(&mut self, request: &Request) -> ApiOutcome {
        let flat = request.flat();
        let hashes = request.prefix_hashes();
        let markers = request.markers();
        let hits: Vec<(usize, Option<usize>)> = markers
            .iter()
            .map(|&m| (m, self.walk(&flat, &hashes, m)))
            .collect();
        let read_end = hits.iter().filter_map(|&(_, hit)| hit).max();
        // A read refreshes what it used; a write refreshes what it wrote.
        for &(_, hit) in &hits {
            if let Some(hit) = hit {
                self.entries.insert(hashes[hit], self.now + ONE_HOUR);
            }
        }
        for &m in &markers {
            self.entries.insert(hashes[m], self.now + ONE_HOUR);
        }
        let read_bytes = read_end.map_or(0, |end| request.bytes_through(end));
        let written_bytes = markers.last().map_or(0, |&last| {
            request.bytes_through(last).saturating_sub(read_bytes)
        });
        ApiOutcome {
            hits,
            read_end,
            read_bytes,
            written_bytes,
        }
    }
}

/// Positions from flat block `to` back to `from`, the way [`ApiCache::walk`]
/// counts them, `to`'s own included.
fn positions_between(flat: &[&WireBlock], from: usize, to: usize) -> usize {
    let mut count = 1;
    for i in (from + 1)..=to {
        let joined =
            flat[i].kind != Kind::Other && flat[i - 1].kind == flat[i].kind;
        if !joined {
            count += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// The scripted session
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct Echo {
    /// Text to echo back.
    text: String,
}

struct EchoTool;

#[tool(name = "echo")]
impl EchoTool {
    /// Echo the text back.
    #[method]
    async fn echo(&mut self, args: Echo) -> Result<Content, Content> {
        Ok(args.text.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Reply {
    /// Quiescent text; the agent seats the next line.
    Text,
    /// Quiescent thinking only: the placeholder path.
    ThoughtOnly,
    /// A thought, a sentence and this many parallel calls.
    Parallel(usize),
    /// This many (text, call) pairs: each call its own position.
    Interleaved(usize),
    /// Clipped by `max_tokens`: nothing is seated, the same turn goes again.
    Truncated,
    /// This many sentences and a server tool still working: the turn is seated
    /// as the tail, an assistant turn, and the session resumes.
    Paused(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Failure {
    /// A 429 or 529: the retry half of the loop.
    Transient,
    /// A refused connection: the hold half.
    Unreachable,
}

/// One `infer`: how it ends, and the notifications that arrive while it's in
/// flight, as `(source, text)`. Seated at the next `on_turn`.
#[derive(Debug, Clone)]
struct Step {
    outcome: Result<Reply, Failure>,
    notes: Vec<(&'static str, String)>,
}

fn step(reply: Reply) -> Step {
    Step {
        outcome: Ok(reply),
        notes: Vec::new(),
    }
}

fn failed(failure: Failure) -> Step {
    Step {
        outcome: Err(failure),
        notes: Vec::new(),
    }
}

fn with_notes(mut step: Step, sources: &[&'static str], each: usize) -> Step {
    for &source in sources {
        for i in 0..each {
            step.notes.push((source, note_text(source, i)));
        }
    }
    step
}

fn note_text(source: &str, i: usize) -> String {
    format!("New message {i:08x} from {source}.")
}

type Queue = Arc<Mutex<VecDeque<Step>>>;
type Inbox = Arc<Mutex<Vec<Notification>>>;

#[derive(Debug, Clone)]
struct Sent {
    wire: Value,
    failed: bool,
}

/// Plays a [`Step`] per `infer` and records each request as sent.
struct ScriptedInference {
    script: Queue,
    inbox: Inbox,
    sent: Arc<Mutex<Vec<Sent>>>,
    route: String,
    counter: AtomicUsize,
    pad: usize,
}

impl ScriptedInference {
    fn unique(&self) -> usize {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }

    fn reply(&self, reply: Reply) -> response::Message {
        let turn = self.unique();
        let thought = json!({
            "type": "thinking",
            "thinking": format!("thought {turn}: {}", "t".repeat(self.pad)),
            "signature": format!("sig{turn}"),
        });
        let text = |i: usize| {
            json!({
                "type": "text",
                "text": format!("reply {turn}.{i}: {}", "r".repeat(self.pad)),
            })
        };
        let call = |i: usize| {
            json!({
                "type": "tool_use",
                "id": format!("toolu_{turn}_{i}"),
                "name": self.route,
                "input": {"text": format!("result {turn}.{i}: {}", "x".repeat(self.pad))},
            })
        };
        let (content, stop) = match reply {
            Reply::Text => (vec![text(0)], "end_turn"),
            Reply::ThoughtOnly => (vec![thought], "end_turn"),
            Reply::Parallel(n) => {
                let mut blocks = vec![thought, text(0)];
                blocks.extend((0..n).map(call));
                (blocks, "tool_use")
            }
            Reply::Interleaved(n) => (
                (0..n).flat_map(|i| [text(i), call(i)]).collect(),
                "tool_use",
            ),
            Reply::Truncated => (vec![text(0)], "max_tokens"),
            Reply::Paused(n) => {
                let mut blocks: Vec<Value> = (0..n).map(text).collect();
                blocks.push(json!({
                    "type": "server_tool_use",
                    "id": format!("srvtoolu_{turn}"),
                    "name": "web_search",
                    "input": {"query": "q"},
                }));
                (blocks, "pause_turn")
            }
        };
        serde_json::from_value(json!({
            "id": "msg_test",
            "role": "assistant",
            "content": content,
            "model": "test-model",
            "stop_reason": stop,
            "stop_sequence": null,
        }))
        .expect("valid reply fixture")
    }
}

#[async_trait::async_trait]
impl Inference for ScriptedInference {
    type Error = TestError;

    async fn infer<P>(&self, prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        let wire = serde_json::to_value(prompt)?;
        let step = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("the session outran its script");
        self.sent.lock().unwrap().push(Sent {
            wire,
            failed: step.outcome.is_err(),
        });
        self.inbox
            .lock()
            .unwrap()
            .extend(step.notes.into_iter().map(|(source, text)| {
                Notification {
                    source: source.into(),
                    content: text.into(),
                    preferred_roles: vec![Role::User],
                }
            }));
        match step.outcome {
            Ok(reply) => Ok(self.reply(reply)),
            Err(Failure::Transient) => {
                Err(TestError::Transient(Duration::ZERO))
            }
            Err(Failure::Unreachable) => Err(TestError::Unreachable),
        }
    }

    async fn infer_batch<P>(
        &self,
        _: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
        panic!("these sessions take the agent-major path")
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

/// The default mechanics over a seed-shaped prompt. Only the drain is
/// overridden, to hear from several sources at once: a real [`Mailbox`] stamps
/// one. `seat_notifications` is what's under test, and it's the real one.
///
/// [`Mailbox`]: misanthropic::tool::Mailbox
struct CacheAgent {
    id: AgentId,
    state: TestState,
    prompt: Prompt,
    tools: misanthropic::tool::ToolBox,
    inbox: Inbox,
    script: Queue,
    quirks: Quirks,
    lines: usize,
}

#[async_trait::async_trait]
impl Agent for CacheAgent {
    type State = TestState;
    type Context = ();
    type Error = TestError;

    fn new(_: AgentId, _: TestState, _: ()) -> Result<Self, TestError> {
        panic!("built by `run_session`")
    }

    fn id(&self) -> AgentId {
        self.id
    }

    fn state(&self) -> &TestState {
        &self.state
    }

    fn prompt(&self) -> &Prompt {
        &self.prompt
    }

    fn parts(&mut self) -> (&mut misanthropic::tool::ToolBox, &mut Prompt) {
        (&mut self.tools, &mut self.prompt)
    }

    fn drain_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut *self.inbox.lock().unwrap())
    }

    fn model(&self) -> ModelInfo {
        model_info(false)
    }

    fn quirks(&self) -> Option<Quirks> {
        Some(self.quirks)
    }

    async fn on_quiesce(
        &mut self,
        _: &response::Message,
    ) -> Result<Control, TestError> {
        if self.script.lock().unwrap().is_empty() {
            return Ok(Control::Done(Outcome::Complete));
        }
        self.lines += 1;
        let line = format!("Round {}: carry on.", self.lines);
        seat_user(&mut self.prompt, line)
            .map_err(|e| TestError::Msg(e.to_string()))?;
        Ok(Control::Continue)
    }
}

/// The pinned prefix's size, in bytes. Only the shares depend on it.
#[derive(Debug, Clone, Copy)]
struct Size {
    system: usize,
    intro: usize,
    /// Each text and tool result's padding.
    block: usize,
}

const FULL: Size = Size {
    system: 20_000,
    intro: 4_000,
    block: 200,
};
/// For the sweeps, which replay thousands of requests in a debug build.
const SMALL: Size = Size {
    system: 400,
    intro: 100,
    block: 8,
};

/// `seed::prompt::assemble`'s markers (one on the system text, one on the end
/// of the intro; `every_cache_marker_is_1h` pins them), at a size where the
/// shared prefix dominates, as a constitution makes it.
fn seed_shaped_prompt(size: Size) -> Prompt {
    let mut prompt = Prompt::default()
        .system("s".repeat(size.system))
        .add_message((Role::User, "i".repeat(size.intro)))
        .unwrap()
        .cache_1h()
        .unwrap();
    prompt.system.as_mut().unwrap().cache_1h();
    prompt
}

/// The real drive loop over `script`, with a closing quiescent turn added, and
/// `first_notes` waiting before the first turn. Returns every attempt as sent.
async fn run_session_with(
    script: Vec<Step>,
    first_notes: Vec<(&'static str, String)>,
    quirks: Quirks,
    size: Size,
) -> Vec<Sent> {
    let mut tools = misanthropic::tool::ToolBox::new();
    tools.push_typed(EchoTool);
    let route = tools.method_names().next().expect("one method").to_string();
    let mut steps: VecDeque<Step> = script.into();
    steps.push_back(step(Reply::Text));
    let script: Queue = Arc::new(Mutex::new(steps));
    let inbox: Inbox = Arc::new(Mutex::new(
        first_notes
            .into_iter()
            .map(|(source, text)| Notification {
                source: source.into(),
                content: text.into(),
                preferred_roles: vec![Role::User],
            })
            .collect(),
    ));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let inference = ScriptedInference {
        script: script.clone(),
        inbox: inbox.clone(),
        sent: sent.clone(),
        route,
        counter: AtomicUsize::new(0),
        pad: size.block,
    };
    let mut agent = CacheAgent {
        id: AgentId::new(),
        state: TestState {
            behavior: Behavior::Complete,
            turns_left: 1,
            poison: None,
        },
        prompt: seed_shaped_prompt(size),
        tools,
        inbox,
        script: script.clone(),
        quirks,
        lines: 0,
    };
    let outcome =
        Reactor::<ScriptedInference, MemStore, CacheAgent>::drive_inner(
            &inference,
            &mut agent,
            Some(Duration::ZERO),
            &Stop::new(),
            &mut false,
        )
        .await;
    assert!(
        matches!(outcome, Ok(Some(Outcome::Complete))),
        "the session ends cleanly: {:?}",
        outcome.map_err(|e| e.to_string())
    );
    assert!(
        script.lock().unwrap().is_empty(),
        "the script was played out"
    );
    std::mem::take(&mut *sent.lock().unwrap())
}

fn run_session(script: Vec<Step>) -> Vec<Sent> {
    run_session_notes(script, Vec::new())
}

fn run_session_notes(
    script: Vec<Step>,
    first_notes: Vec<(&'static str, String)>,
) -> Vec<Sent> {
    run_sized(script, first_notes, FULL)
}

fn run_sized(
    script: Vec<Step>,
    first_notes: Vec<(&'static str, String)>,
    size: Size,
) -> Vec<Sent> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(run_session_with(
            script,
            first_notes,
            Quirks::default(),
            size,
        ))
}

// ---------------------------------------------------------------------------
// The assertions, for every request after the first
// ---------------------------------------------------------------------------

/// Whether an attempt that failed had written its markers before it did. A 429
/// hasn't; a stream that broke after the headers has. Both are checked.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FailedAttempts {
    WroteNothing,
    Wrote,
}

#[derive(Debug, Default, Clone, Copy)]
struct Shares {
    requests: usize,
    api_read: usize,
    api_written: usize,
    prefix_read: usize,
    /// The rendered length, framing included: what `prefix_read` is out of.
    prefix_total: usize,
    total: usize,
    /// What a perfect cache reads: the whole of the previous request.
    ideal_read: usize,
    /// Requests whose tail marker was past the window from the last write.
    window_fallbacks: usize,
    /// Most positions any request walked from its tip back to the last write.
    widest: usize,
}

impl Shares {
    fn api(&self) -> f64 {
        self.api_read as f64 / self.total as f64
    }

    fn prefix(&self) -> f64 {
        self.prefix_read as f64 / self.prefix_total as f64
    }

    fn ideal(&self) -> f64 {
        self.ideal_read as f64 / self.total as f64
    }
}

/// Assertion 1. `next` extends `prev`, by whole messages or by blocks on
/// `prev`'s last, a user turn. True at the end if blocks were merged.
fn assert_prefix_stable(prev: &Request, next: &Request, at: usize) -> bool {
    let bytes = |blocks: &[WireBlock]| {
        blocks.iter().map(|b| b.bytes.clone()).collect::<Vec<_>>()
    };
    assert_eq!(prev.config, next.config, "request {at}: config changed");
    assert_eq!(
        bytes(&prev.tools),
        bytes(&next.tools),
        "request {at}: tools changed"
    );
    assert_eq!(
        bytes(&prev.system),
        bytes(&next.system),
        "request {at}: system changed"
    );
    assert!(
        prev.messages.len() <= next.messages.len(),
        "request {at}: messages dropped"
    );
    let tail = prev.messages.len() - 1;
    for i in 0..tail {
        assert_eq!(prev.messages[i].0, next.messages[i].0);
        assert_eq!(
            bytes(&prev.messages[i].1),
            bytes(&next.messages[i].1),
            "request {at}: message {i} changed"
        );
    }
    let (old, new) = (&prev.messages[tail], &next.messages[tail]);
    assert_eq!(old.0, new.0);
    assert!(
        new.1.len() >= old.1.len()
            && bytes(&old.1) == bytes(&new.1[..old.1.len()]),
        "request {at}: the tail changed other than by appending"
    );
    let merged = new.1.len() > old.1.len();
    if merged {
        assert_eq!(old.0, "user", "request {at}: merged into a non-user turn");
    }
    merged
}

/// Runs every check over a session, and returns its shares.
#[track_caller]
fn check(sent: &[Sent], failed_attempts: FailedAttempts) -> Shares {
    let requests: Vec<Request> =
        sent.iter().map(|s| Request::parse(&s.wire)).collect();
    check_parsed(sent, &requests, failed_attempts)
}

#[track_caller]
fn check_parsed(
    sent: &[Sent],
    requests: &[Request],
    failed_attempts: FailedAttempts,
) -> Shares {
    let mut cache = ApiCache::default();
    let mut shares = Shares::default();
    // The last request a cache saw, and its markers.
    let mut last_seen: Option<usize> = None;
    let mut last_good_len = 0usize;

    for (i, (request, attempt)) in requests.iter().zip(sent).enumerate() {
        let markers = request.markers();
        let flat = request.flat();
        assert!(
            markers.len() <= MAX_MARKERS,
            "request {i}: {} markers",
            markers.len()
        );
        assert!(
            flat.iter().all(|b| !b.other_ttl),
            "request {i}: a marker that isn't 1h"
        );
        // The write that the next request's read depends on.
        assert_eq!(
            markers.last(),
            Some(&(flat.len() - 1)),
            "request {i}: the last marker isn't on the tip"
        );
        for (m, (_, blocks)) in request.messages.iter().enumerate() {
            let n = blocks.iter().filter(|b| b.marked).count();
            assert!(n <= 1, "request {i}: {n} markers in message {m}");
        }
        assert!(
            request.system.iter().any(|b| b.marked),
            "request {i}: the tools+system marker is gone"
        );
        assert!(
            request.messages[0].1.last().is_some_and(|b| b.marked),
            "request {i}: the intro marker isn't on the intro's end"
        );

        if i > 0 {
            assert_prefix_stable(&requests[i - 1], request, i);
        }

        if attempt.failed && failed_attempts == FailedAttempts::WroteNothing {
            continue;
        }
        let outcome = cache.request(request);

        if let Some(prev) = last_seen.map(|p| &requests[p]) {
            // Assertion 2.
            let prev_markers = prev.markers();
            let last_write = *prev_markers.last().expect("the tip's");
            let system_marker = request.head_len() - 1;
            assert_eq!(
                outcome.hits.iter().find(|h| h.0 == system_marker),
                Some(&(system_marker, Some(system_marker))),
                "request {i}: tools+system missed"
            );
            let intro_marker =
                request.head_len() + request.messages[0].1.len() - 1;
            let intro_moved =
                prev.messages[0].1.len() != request.messages[0].1.len();
            let intro_hit = outcome
                .hits
                .iter()
                .find(|h| h.0 == intro_marker)
                .expect("the intro's marker");
            if intro_moved {
                // Only a merge into the intro itself moves it, while the intro
                // is the tail. Its walk finds where it was.
                assert_eq!(prev.messages.len(), 1, "request {i}: intro moved");
                assert!(
                    intro_hit.1.is_some(),
                    "request {i}: intro walk missed"
                );
            } else {
                assert_eq!(
                    intro_hit.1,
                    Some(intro_marker),
                    "request {i}: the intro missed"
                );
            }
            let read_end = outcome.read_end.expect("something hit");
            let flat = request.flat();
            let tip = flat.len() - 1;
            let widest = positions_between(&flat, last_write, tip);
            shares.widest = shares.widest.max(widest);
            let on_marker = markers.contains(&last_write);
            if on_marker || widest <= LOOKBACK {
                assert!(
                    read_end >= last_write,
                    "request {i}: read to block {read_end}, the last write \
                     was at {last_write} ({widest} positions back, markers \
                     {markers:?})"
                );
            } else {
                // The one case `windowed` doesn't cover: the newest write lost
                // its marker to a merge wider than the window. The older
                // rolling marker reads what it reads.
                shares.window_fallbacks += 1;
                let older = prev_markers[prev_markers.len() - 2];
                assert!(
                    read_end >= older,
                    "request {i}: read to {read_end}, older write at {older}"
                );
            }

            // Assertion 3.
            let (old, new) = (prev.render(), request.render());
            let common = common_prefix(&old, &new);
            let same_tail = prev.messages.len() == request.messages.len()
                && prev.messages.last().map(|m| m.1.len())
                    != request.messages.last().map(|m| m.1.len());
            let expected = if same_tail {
                old.len() - CLOSE_MESSAGE.len() - GENERATION_PROMPT.len()
            } else {
                old.len()
            };
            assert_eq!(
                common,
                expected,
                "request {i}: the common prefix, of {} rendered",
                old.len()
            );

            if !attempt.failed {
                shares.prefix_read += common;
                shares.ideal_read += last_good_len.min(request.total());
            }
        }
        if !attempt.failed {
            shares.requests += 1;
            shares.api_read += outcome.read_bytes;
            shares.api_written += outcome.written_bytes;
            shares.total += request.total();
            shares.prefix_total += request.render().len();
            last_good_len = request.total();
        }
        last_seen = Some(i);
    }
    shares
}

/// The production [`divergence`] agrees: it reports exactly the merges.
#[track_caller]
fn assert_divergence_agrees(sent: &[Sent], requests: &[Request]) {
    let prompts: Vec<Prompt> = sent
        .iter()
        .map(|s| {
            serde_json::from_value(s.wire.clone()).expect("the wire parses")
        })
        .collect();
    for i in 1..sent.len() {
        let merged = assert_prefix_stable(&requests[i - 1], &requests[i], i);
        let why = divergence(&prompts[i - 1], &prompts[i]);
        assert_eq!(
            why.is_some(),
            merged,
            "request {i}: divergence said {why:?}, merged is {merged}"
        );
    }
}

/// Both beliefs about failed attempts.
#[track_caller]
fn check_both(sent: &[Sent]) -> Shares {
    let requests: Vec<Request> =
        sent.iter().map(|s| Request::parse(&s.wire)).collect();
    assert_divergence_agrees(sent, &requests);
    let shares = check_parsed(sent, &requests, FailedAttempts::WroteNothing);
    check_parsed(sent, &requests, FailedAttempts::Wrote);
    shares
}

// ---------------------------------------------------------------------------
// The simulator, checked against the docs by hand
// ---------------------------------------------------------------------------

fn hand_request(blocks: &[Value], marked: &[usize]) -> Request {
    let content: Vec<Value> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut b = b.clone();
            if marked.contains(&i) {
                b["cache_control"] = json!({"type": "ephemeral", "ttl": "1h"});
            }
            b
        })
        .collect();
    Request::parse(&json!({
        "model": "m",
        "messages": [{"role": "user", "content": content}],
    }))
}

fn text(i: usize) -> Value {
    json!({"type": "text", "text": format!("block {i}")})
}

fn texts(n: usize) -> Vec<Value> {
    (0..n).map(text).collect()
}

/// "The system checks at most 20 positions for each breakpoint, counting the
/// breakpoint itself as the first."
#[test]
fn the_model_walks_twenty_positions_counting_the_marker() {
    for (blocks, found) in [(20, true), (21, false)] {
        let all = texts(blocks);
        let mut cache = ApiCache::default();
        cache.request(&hand_request(&all[..1], &[0]));
        let got = cache.request(&hand_request(&all, &[blocks - 1]));
        assert_eq!(got.read_end.is_some(), found, "{blocks} blocks");
    }
}

/// "A run of consecutive `tool_use` blocks counts as one position, and so does
/// a run of consecutive `tool_result` blocks."
#[test]
fn the_model_counts_a_tool_run_as_one_position() {
    let tool_use = |i: usize| json!({"type": "tool_use", "id": format!("t{i}"), "name": "n", "input": {}});
    let tool_result = |i: usize| json!({"type": "tool_result", "tool_use_id": format!("t{i}"), "content": "r"});
    let mut blocks = vec![text(0)];
    blocks.extend((0..30).map(tool_use));
    blocks.extend((0..30).map(tool_result));
    let mut cache = ApiCache::default();
    cache.request(&hand_request(&blocks[..1], &[0]));
    let tip = blocks.len() - 1;
    let got = cache.request(&hand_request(&blocks, &[tip]));
    assert_eq!(got.read_end, Some(0), "three positions, not 61");

    // Thirty text blocks instead: thirty positions, past the window.
    let mut cache = ApiCache::default();
    let all = texts(31);
    cache.request(&hand_request(&all[..1], &[0]));
    assert_eq!(cache.request(&hand_request(&all, &[30])).read_end, None);
}

/// "Cache writes occur only at your breakpoint": a walk finds no entry between
/// markers, and `cache_control` doesn't change a block's hash.
#[test]
fn the_model_writes_at_markers_and_ignores_them_in_the_hash() {
    let all = texts(6);
    let mut cache = ApiCache::default();
    cache.request(&hand_request(&all[..4], &[3]));
    // Block 2 was never a marker; block 3 was.
    assert_eq!(cache.request(&hand_request(&all[..3], &[2])).read_end, None);
    assert_eq!(cache.request(&hand_request(&all, &[5])).read_end, Some(3));
    // A different text before the marker misses.
    let mut other = all.clone();
    other[0] = text(99);
    assert_eq!(cache.request(&hand_request(&other, &[5])).read_end, None);
    // Longest hit across markers, and what's billed.
    let mut cache = ApiCache::default();
    cache.request(&hand_request(&all[..2], &[1]));
    cache.request(&hand_request(&all[..4], &[3]));
    let request = hand_request(&all, &[1, 5]);
    let got = cache.request(&request);
    assert_eq!(got.read_end, Some(3));
    assert_eq!(got.read_bytes, request.bytes_through(3));
    assert_eq!(got.written_bytes, request.total() - got.read_bytes);
}

/// The 1h TTL, refreshed by use.
#[test]
fn the_model_expires_entries_an_hour_after_use() {
    let all = texts(3);
    let mut cache = ApiCache::default();
    cache.request(&hand_request(&all[..1], &[0]));
    cache.now += ONE_HOUR - 1;
    assert_eq!(
        cache.request(&hand_request(&all[..2], &[1])).read_end,
        Some(0)
    );
    // Used just now, so a second near-hour is fine.
    cache.now += ONE_HOUR - 1;
    assert_eq!(
        cache.request(&hand_request(&all[..2], &[1])).read_end,
        Some(1)
    );
    cache.now += ONE_HOUR;
    assert_eq!(cache.request(&hand_request(&all, &[2])).read_end, None);
}

#[test]
fn the_prefix_model_stops_at_the_first_differing_byte() {
    assert_eq!(common_prefix("abc", "abd"), 2);
    assert_eq!(common_prefix("abc", "abcd"), 3);
    assert_eq!(common_prefix("", "a"), 0);
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn plain(rounds: usize) -> Vec<Step> {
    (0..rounds).map(|_| step(Reply::Text)).collect()
}

fn repeat(reply: Reply, rounds: usize) -> Vec<Step> {
    (0..rounds).map(|_| step(reply)).collect()
}

type Scenario = (&'static str, Vec<Step>, Vec<(&'static str, String)>, f64);

/// Every scenario, with the read share it's held to. A request that reads all
/// of the one before it leaves unread only the first request and each round's
/// new blocks, so a session's best share follows from the sizes: Σ previous
/// request / Σ request. Each floor is that figure for [`FULL`], rounded down, so
/// a lost read fails it; a change of fixture sizes must move it.
fn scenarios() -> Vec<Scenario> {
    let none = Vec::new;
    let every_other = |a: Step, b: Step, rounds: usize| -> Vec<Step> {
        (0..rounds)
            .map(|i| if i % 2 == 0 { a.clone() } else { b.clone() })
            .collect()
    };
    let s1 = with_notes(step(Reply::Parallel(1)), &["events"], 1);
    let s2 = |failure| with_notes(failed(failure), &["events"], 1);
    vec![
        ("plain", plain(30), none(), 0.95),
        (
            "parallel x12",
            repeat(Reply::Parallel(12), 30),
            none(),
            0.93,
        ),
        ("sequential", repeat(Reply::Parallel(1), 60), none(), 0.96),
        (
            "past the window",
            repeat(Reply::Interleaved(12), 15),
            none(),
            0.88,
        ),
        (
            "line on the intro",
            plain(30),
            vec![("events", note_text("events", 0))],
            0.95,
        ),
        (
            "notes S1/S3",
            every_other(s1, step(Reply::Text), 40),
            none(),
            0.95,
        ),
        (
            "notes S2 retry",
            every_other(s2(Failure::Transient), step(Reply::Parallel(1)), 40),
            none(),
            0.93,
        ),
        (
            "notes S2 hold",
            every_other(s2(Failure::Unreachable), step(Reply::Parallel(1)), 40),
            none(),
            0.93,
        ),
        (
            "S2 after a clip",
            every_other(
                with_notes(step(Reply::Truncated), &["events"], 1),
                step(Reply::Parallel(1)),
                40,
            ),
            none(),
            0.95,
        ),
        (
            "several notes",
            every_other(
                with_notes(failed(Failure::Transient), &["a", "b", "c"], 2),
                with_notes(step(Reply::Parallel(1)), &["a", "b"], 2),
                40,
            ),
            none(),
            0.92,
        ),
    ]
}

macro_rules! scenario_test {
    ($name:ident, $label:literal) => {
        #[test]
        fn $name() {
            let (_, script, first, _) = scenarios()
                .into_iter()
                .find(|s| s.0 == $label)
                .expect("a scenario by that name");
            let shares = check_both(&run_session_notes(script, first));
            assert!(shares.requests > 1);
        }
    };
}

scenario_test!(plain_rounds_are_stable, "plain");
scenario_test!(many_parallel_calls_are_stable, "parallel x12");
scenario_test!(many_sequential_calls_are_stable, "sequential");
scenario_test!(notes_at_s1_and_s3_are_stable, "notes S1/S3");
scenario_test!(notes_on_a_retry_are_stable, "notes S2 retry");
scenario_test!(notes_on_a_hold_are_stable, "notes S2 hold");
scenario_test!(notes_on_a_clipped_turn_are_stable, "S2 after a clip");
scenario_test!(several_notes_in_one_seat_are_stable, "several notes");

/// Each round is wider than the window, so the tail marker's walk can't reach
/// the last write; the older rolling marker sits on it and reads it directly.
#[test]
fn a_round_past_the_window_is_read_through_the_older_marker() {
    assert_eq!(ROLL_WINDOW, 2, "this test describes a window of 2");
    let sent = run_session(repeat(Reply::Interleaved(12), 15));
    let shares = check_both(&sent);
    assert!(
        shares.widest > LOOKBACK,
        "not past the window: {} positions",
        shares.widest
    );
    assert_eq!(
        shares.window_fallbacks, 0,
        "the older marker covers every one"
    );
}

/// A notification waiting at the start merges into the intro, whose marker
/// moves onto it before anything was sent, so nothing is lost.
#[test]
fn a_line_on_the_intro_before_the_first_turn_takes_its_marker() {
    let sent =
        run_session_notes(plain(5), vec![("events", note_text("events", 0))]);
    let first = Request::parse(&sent[0].wire);
    assert_eq!(first.messages.len(), 1);
    assert_eq!(first.messages[0].1.len(), 3, "intro, label, note");
    assert!(first.messages[0].1[2].marked);
    assert!(!first.messages[0].1[0].marked);
    check_both(&sent);
}

/// The intro is the tail of a failed first attempt too: the note arrives during
/// it, and the marker moves between attempts.
#[test]
fn a_line_on_the_intro_on_a_retried_first_turn_is_stable() {
    let sent = run_session(vec![with_notes(
        failed(Failure::Transient),
        &["events"],
        1,
    )]);
    check_both(&sent);
}

/// `seat_user` onto a rolled tail, as a phase's instruction would be.
#[test]
fn a_line_seated_on_a_marked_tail_moves_the_marker_to_it() {
    let quirks = Quirks::default();
    let mut prompt = seed_shaped_prompt(FULL);
    let mut sent = Vec::new();
    let mut send = |prompt: &mut Prompt| {
        roll_breakpoints(&quirks, prompt);
        sent.push(Sent {
            wire: serde_json::to_value(&*prompt).unwrap(),
            failed: false,
        });
    };
    send(&mut prompt);
    for i in 0..6 {
        prompt
            .push_message((Role::Assistant, format!("a{i}")))
            .unwrap();
        prompt.push_message((Role::User, format!("u{i}"))).unwrap();
        send(&mut prompt);
        seat_user(&mut prompt, format!("line {i}")).unwrap();
        send(&mut prompt);
    }
    check_both(&sent);
}

/// **The exception to "the read reaches the last write".** A merge of 20 or
/// more blocks (a label and 19 notes) onto a tail that had already been written
/// (a clip, or a failed attempt that wrote) leaves that write out of the tail's
/// walk with no marker on it. Reads fall back one round, to the older rolling
/// marker; the tail is written again, and the next request is whole.
#[test]
fn a_merge_wider_than_the_window_falls_back_one_round() {
    // A label and n notes: n + 2 positions from the tip back to the old tail,
    // counting both ends.
    let run = |notes: usize| {
        let sent = run_session(vec![
            step(Reply::Parallel(1)),
            with_notes(step(Reply::Truncated), &["events"], notes),
            step(Reply::Parallel(1)),
        ]);
        check(&sent, FailedAttempts::WroteNothing)
    };
    let inside = run(18);
    assert_eq!(inside.window_fallbacks, 0);
    assert_eq!(inside.api_read, inside.ideal_read);
    let past = run(19);
    assert_eq!(past.window_fallbacks, 1);
    let lost = past.ideal_read - past.api_read;
    println!(
        "19 notes on a clipped tail: {lost} of {} bytes re-read as a write",
        past.total
    );
    assert!(lost > 0 && lost < 2_000, "one round's bytes: {lost}");
}

/// A 429 wrote nothing, so the same merge costs nothing: the last write is the
/// tail of the last success, which the older marker sits on.
#[test]
fn a_wide_merge_after_a_failed_attempt_loses_nothing_if_it_wrote_nothing() {
    let script = vec![
        step(Reply::Parallel(1)),
        with_notes(failed(Failure::Unreachable), &["events"], 40),
        step(Reply::Parallel(1)),
    ];
    let sent = run_session(script);
    assert_eq!(
        check(&sent, FailedAttempts::WroteNothing).window_fallbacks,
        0
    );
    assert_eq!(check(&sent, FailedAttempts::Wrote).window_fallbacks, 1);
}

/// A paused turn's tail is an assistant turn, so the pair rolls onto assistant
/// turns until the session's parity comes back.
#[test]
fn a_paused_turn_stays_stable() {
    let script = vec![
        step(Reply::Parallel(1)),
        step(Reply::Paused(1)),
        step(Reply::Parallel(1)),
        step(Reply::Paused(1)),
        step(Reply::Paused(1)),
        step(Reply::Text),
        step(Reply::Parallel(2)),
    ];
    check_both(&run_session(script));
}

/// The other exception. A paused tail is an assistant turn, so the pair skips
/// the last write's user turn and that write loses its marker; a turn of more
/// than 20 positions before the pause leaves it out of the walk. The older
/// marker reads what it reads, one round back, and the next request is whole.
#[test]
fn a_paused_turn_wider_than_the_window_falls_back_one_round() {
    let run = |sentences: usize| {
        let sent = run_session(vec![
            step(Reply::Parallel(1)),
            step(Reply::Parallel(1)),
            step(Reply::Paused(sentences)),
            step(Reply::Parallel(1)),
        ]);
        check_both(&sent)
    };
    assert_eq!(run(18).window_fallbacks, 0);
    let past = run(25);
    assert_eq!(past.window_fallbacks, 1);
    println!(
        "a 25-sentence paused turn: {} of {} bytes re-read as a write",
        past.ideal_read - past.api_read,
        past.total
    );
}

/// **A real failure, not a cache miss.** A notification that arrives while a
/// paused turn's server tool is open has no legal seat: only an assistant
/// continuation may follow. `on_turn` seats it anyway, `seat_user` refuses, and
/// the error ends the session. It should wait for the next user turn.
#[test]
#[ignore = "real: a notification behind an open paused turn ends the session (on_turn seats it, the turn order refuses)"]
fn a_notification_behind_a_paused_turn_doesnt_end_the_session() {
    let sent = run_session(vec![
        with_notes(step(Reply::Paused(1)), &["events"], 1),
        step(Reply::Parallel(1)),
    ]);
    check_both(&sent);
}

/// Ollama: no rolling markers (the quirk), and its byte-prefix cache still sees
/// a clean extension each round.
#[test]
fn markers_ignored_still_extends_the_prefix() {
    let quirks = Quirks {
        cache_markers_ignored: true,
        ..Quirks::default()
    };
    let sent = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(run_session_with(
            repeat(Reply::Parallel(2), 10),
            Vec::new(),
            quirks,
            FULL,
        ));
    let requests: Vec<Request> =
        sent.iter().map(|s| Request::parse(&s.wire)).collect();
    for pair in requests.windows(2) {
        assert_eq!(pair[1].markers().len(), 2, "the pinned two only");
        let old = pair[0].render();
        assert_eq!(common_prefix(&old, &pair[1].render()), old.len());
    }
}

/// A gap under the hour hits everything; past it, nothing, and then the request
/// after is whole again.
#[test]
fn a_gap_past_the_ttl_misses_once() {
    let sent = run_session(plain(4));
    let requests: Vec<Request> =
        sent.iter().map(|s| Request::parse(&s.wire)).collect();
    let mut cache = ApiCache::default();
    cache.request(&requests[0]);
    cache.now += ONE_HOUR - 1;
    assert!(cache.request(&requests[1]).read_end.is_some());
    cache.now += ONE_HOUR + 1;
    assert_eq!(cache.request(&requests[2]).read_end, None);
    cache.now += ONE_HOUR - 1;
    let hit = cache.request(&requests[3]);
    assert_eq!(hit.read_end, Some(*requests[2].markers().last().unwrap()));
}

/// The whole-session numbers. Run with `--nocapture` to see the table.
///
/// Two things are held: a floor per scenario, from the sizes above, and the
/// reads themselves: each request reading the whole of the one before, which
/// doesn't depend on the sizes. The numbers are for this fixture, byte counts
/// standing in for tokens.
#[test]
fn cache_read_share_over_a_session_meets_its_floor() {
    println!(
        "{:<18} {:>4} {:>7} {:>7} {:>7} {:>6} {:>5}",
        "scenario", "reqs", "api", "prefix", "ideal", "wider", "fall"
    );
    for (name, script, first, floor) in scenarios() {
        let shares = check(
            &run_session_notes(script, first),
            FailedAttempts::WroteNothing,
        );
        println!(
            "{name:<18} {:>4} {:>7.4} {:>7.4} {:>7.4} {:>6} {:>5}",
            shares.requests,
            shares.api(),
            shares.prefix(),
            shares.ideal(),
            shares.widest,
            shares.window_fallbacks,
        );
        assert_eq!(
            shares.api_read, shares.ideal_read,
            "{name}: reads short of the whole previous request"
        );
        assert!(
            shares.api() >= floor,
            "{name}: API read share {:.4} under {floor}",
            shares.api()
        );
        assert!(
            shares.prefix() >= floor,
            "{name}: prefix read share {:.4} under {floor}",
            shares.prefix()
        );
    }
}

// ---------------------------------------------------------------------------
// Random mixes
// ---------------------------------------------------------------------------

/// One round's plan. The stall and retry caps bound what's generated: at most
/// one clip in a row and three failures a turn.
#[derive(Debug, Clone)]
struct Round {
    failures: Vec<(Failure, usize, usize)>,
    clipped: Option<(usize, usize)>,
    reply: Reply,
    notes: (usize, usize),
}

/// Drops the notes that would be seated behind an open paused turn. See
/// [`a_notification_behind_a_paused_turn_doesnt_end_the_session`].
fn quiet_behind_pauses(mut script: Vec<Step>) -> Vec<Step> {
    let mut open = false;
    for step in &mut script {
        match step.outcome {
            Ok(Reply::Paused(_)) => {
                open = true;
                step.notes.clear();
            }
            Ok(Reply::Truncated) | Err(_) => {
                if open {
                    step.notes.clear();
                }
            }
            Ok(_) => open = false,
        }
    }
    script
}

fn flatten(rounds: Vec<Round>) -> Vec<Step> {
    const SOURCES: [&str; 3] = ["a", "b", "c"];
    let notes = |step: Step, (sources, each): (usize, usize)| {
        with_notes(step, &SOURCES[..sources], each)
    };
    rounds
        .into_iter()
        .flat_map(|round| {
            let mut steps: Vec<Step> = round
                .failures
                .iter()
                .map(|&(failure, sources, each)| {
                    notes(failed(failure), (sources, each))
                })
                .collect();
            if let Some(plan) = round.clipped {
                steps.push(notes(step(Reply::Truncated), plan));
            }
            steps.push(notes(step(round.reply), round.notes));
            steps
        })
        .collect()
}

fn round_strategy() -> impl proptest::strategy::Strategy<Value = Round> {
    use proptest::prelude::*;
    let reply = prop_oneof![
        3 => Just(Reply::Text),
        1 => Just(Reply::ThoughtOnly),
        3 => (1usize..16).prop_map(Reply::Parallel),
        1 => (1usize..16).prop_map(Reply::Interleaved),
        1 => (1usize..3).prop_map(Reply::Paused),
    ];
    let failure =
        prop_oneof![Just(Failure::Transient), Just(Failure::Unreachable),];
    let notes = (0usize..3, 0usize..3);
    (
        proptest::collection::vec((failure, 0usize..3, 0usize..3), 0..3),
        proptest::option::weighted(0.1, notes.clone()),
        reply,
        notes,
    )
        .prop_map(|(failures, clipped, reply, notes)| Round {
            failures,
            clipped,
            reply,
            notes,
        })
}

fn random_session(
    rounds: Vec<Round>,
    first: usize,
) -> Result<(), proptest::test_runner::TestCaseError> {
    let first_notes = (0..first)
        .map(|i| ("events", note_text("events", i)))
        .collect();
    let script = quiet_behind_pauses(flatten(rounds));
    let sent = run_sized(script, first_notes, SMALL);
    let shares = check_both(&sent);
    // A fallback needs a merge of 20 blocks onto a written tail. A seat here
    // is at most 6, and only a clip's seat lands on one.
    proptest::prop_assert_eq!(shares.window_fallbacks, 0);
    proptest::prop_assert_eq!(shares.api_read, shares.ideal_read);
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 6,
        failure_persistence: None,
        ..Default::default()
    })]

    /// Every assertion, over 50 to 70 rounds of everything at once.
    #[test]
    fn random_sessions_stay_stable(
        rounds in proptest::collection::vec(round_strategy(), 50..70),
        first in 0usize..3,
    ) {
        random_session(rounds, first)?;
    }
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..Default::default()
    })]

    /// The same, for longer.
    #[test]
    #[ignore = "slow, minutes: 256 random sessions"]
    fn many_random_sessions_stay_stable(
        rounds in proptest::collection::vec(round_strategy(), 50..70),
        first in 0usize..3,
    ) {
        random_session(rounds, first)?;
    }
}

/// Every order of the moves, a few rounds deep.
#[test]
#[ignore = "slow: every sequence of 4 moves"]
fn every_short_sequence_stays_stable() {
    let moves = [
        step(Reply::Text),
        step(Reply::ThoughtOnly),
        step(Reply::Parallel(3)),
        step(Reply::Interleaved(11)),
        step(Reply::Paused(1)),
        with_notes(step(Reply::Parallel(1)), &["a", "b"], 2),
        with_notes(step(Reply::Text), &["a"], 1),
        with_notes(step(Reply::Truncated), &["a"], 1),
        with_notes(failed(Failure::Transient), &["a"], 1),
        with_notes(failed(Failure::Unreachable), &["a", "b"], 1),
    ];
    const DEPTH: u32 = 4;
    let n = moves.len();
    for seq in 0..n.pow(DEPTH) {
        let mut script = Vec::with_capacity(DEPTH as usize);
        let mut left = seq;
        for _ in 0..DEPTH {
            script.push(moves[left % n].clone());
            left /= n;
        }
        // More clips in a row than the stall cap is a session ending, not a
        // cache question.
        let mut stalls = 0;
        let stalled_out = script.iter().any(|s| {
            match s.outcome {
                Ok(Reply::Truncated) => stalls += 1,
                Ok(_) => stalls = 0,
                Err(_) => {}
            }
            stalls >= MAX_STALLS
        });
        if stalled_out {
            continue;
        }
        let sent = run_sized(quiet_behind_pauses(script), Vec::new(), SMALL);
        let shares = check_both(&sent);
        assert_eq!(shares.window_fallbacks, 0, "sequence {seq}");
    }
}
