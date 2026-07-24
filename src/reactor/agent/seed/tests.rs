//! Behavior tests for the [`SeedAgent`] phase machine: perception seating, the
//! round budget, flat tool dispatch with the dedup ledger, phase-output
//! parsing/stalling, and survey redaction. The Agora side is an [`httpmock`]
//! server; inference never happens — responses are handed straight to
//! [`Agent::handle`].

use std::collections::HashMap;
use std::sync::Arc;

use httpmock::prelude::*;
use misanthropic::prompt::message::{Block, Role};
use misanthropic::response::{self, StopReason};
use url::Url;
use uuid::Uuid;

use super::*;
use crate::crypto::generate_keypair;

fn text_message(text: &str, stop: StopReason) -> response::Message {
    let stop = match stop {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        _ => "end_turn",
    };
    serde_json::from_value(serde_json::json!({
        "id": "msg_test",
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "model": "claude-haiku-4-5",
        "stop_reason": stop,
        "stop_sequence": null,
    }))
    .expect("valid response::Message fixture")
}

fn tool_use_message(name: &str, input: serde_json::Value) -> response::Message {
    serde_json::from_value(serde_json::json!({
        "id": "msg_test",
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "id": "toolu_test",
            "name": name,
            "input": input,
        }],
        "model": "claude-haiku-4-5",
        "stop_reason": "tool_use",
        "stop_sequence": null,
    }))
    .expect("valid tool_use response::Message fixture")
}

fn soul() -> Soul {
    serde_json::from_value(serde_json::json!({
        "name": "test-agent",
        "identity": "A test agent that tests.",
        "values": ["testing"],
        "interests": { "communities": ["tech"], "topics": ["testing"] },
        "voice": "terse",
    }))
    .expect("valid Soul fixture")
}

fn seed_state() -> SeedState {
    use misanthropic::model::{Kind, Model};
    let model = ModelInfo {
        id: Model::from("claude-haiku-4-5"),
        display_name: "Test Haiku".into(),
        capabilities: Default::default(),
        max_input_tokens: 0,
        max_tokens: 0,
        kind: Kind::Model,
        created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
    };
    // The constructor is the one legitimate `prompt.model` derivation.
    SeedState::new(soul(), model)
}

/// A `SeedAgent` pointed at `server`, with `config`, plus its id.
fn agent(server: &MockServer, config: SeedConfig) -> SeedAgent {
    let id = AgentId::new();
    let (key, _) = generate_keypair();
    let keys: HashMap<AgentId, SigningKey> = [(id, key)].into_iter().collect();
    let ctx = SeedContext {
        client: Client::new(Url::parse(&server.base_url()).unwrap()).unwrap(),
        keys: Arc::new(keys),
        config,
    };
    SeedAgent::new(id, seed_state(), ctx).unwrap()
}

/// Config with every die pinned to "never" — phase transitions become
/// deterministic.
fn quiet_config() -> SeedConfig {
    SeedConfig {
        mutation_chance: 0,
        evolution_chance: 0,
        survey_chance: 0,
        ..SeedConfig::default()
    }
}

/// All text across all messages — plain text and tool results — for
/// containment asserts.
fn transcript(agent: &SeedAgent) -> String {
    agent
        .prompt()
        .messages
        .iter()
        .flat_map(|m| m.iter())
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.to_string()),
            Block::ToolResult { result } => Some(result.content.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Seat the "session started" user turn `on_init` would have (the tests
/// below skip perception's network round-trips where they can).
fn seat_start(agent: &mut SeedAgent) {
    agent
        .state
        .prompt
        .push_message((Role::User, "start"))
        .unwrap();
}

const FULL_CONSTITUTION: &str = "Preamble Article I Article II Article III \
                                 Article IV Article V The Steward";

/// Mount the four perception endpoints. The dashboard carries a feed post
/// AND an unread reply to one of the agent's own posts — the "someone
/// answered you" signal.
fn mock_perception(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/agora/api/constitution");
        then.status(200).json_body(serde_json::json!({
            "version": "0.3",
            "text": FULL_CONSTITUTION,
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/agora/api/social/communities");
        then.status(200).json_body(serde_json::json!([{
            "id": Uuid::new_v4(),
            "name": "tech",
            "display_name": "Technology",
        }]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/agora/api/social/dash");
        then.status(200).json_body(serde_json::json!({
            "agent": { "name": "test-agent", "karma": 1 },
            "unread_post_replies": [{
                "post_id": Uuid::new_v4(),
                "post_title": "My old post about ferns",
                "replies": [{
                    "comment_id": Uuid::new_v4(),
                    "author": "fern-fan",
                    "score": 1,
                    "preview": "Great point about spores!",
                    "created_at": "2026-07-01T12:00:00Z",
                }],
            }],
            "feeds": {
                "tech": [{
                    "id": Uuid::new_v4(),
                    "title": "Existing thread about compilers",
                    "author": "someone-else",
                    "score": 2,
                    "comment_count": 0,
                    "created_at": "2026-07-01T00:00:00Z",
                }]
            },
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path_contains("/posts");
        then.status(200).json_body(serde_json::json!([]));
    });
}

#[tokio::test]
async fn on_init_seats_system_intro_and_flat_tools() {
    let server = MockServer::start();
    mock_perception(&server);

    let mut agent = agent(&server, quiet_config());
    agent.on_init().await.unwrap();

    // System prefix: constitution + live community slugs + guidelines.
    let system = format!("{}", agent.prompt().system.as_ref().unwrap());
    assert!(system.contains("Article V"), "constitution seated");
    assert!(system.contains("\"tech\""), "live slugs seated");

    // Flat tool names on the wire — no `toolbox__agora__` segments.
    let names: Vec<String> = agent
        .prompt()
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .map(|d| d.name().to_string())
        .collect();
    assert!(names.contains(&"create_post".to_string()), "{names:?}");
    assert!(
        names.contains(&"get_governance_log".to_string()),
        "{names:?}"
    );

    // Intro: soul + memory + dashboard, one user turn, notifications taken.
    let intro = transcript(&agent);
    assert!(intro.contains("A test agent that tests."), "soul");
    assert!(intro.contains("Existing thread about compilers"), "feed");
    // Replies to the agent's own content reach the intro via the dashboard.
    assert!(intro.contains("Unread Replies to Your Posts"), "{intro}");
    assert!(intro.contains("Great point about spores!"), "{intro}");
    assert!(agent.notifications.is_some());

    // Perception seeded the repetition policy.
    let ledger = agent.state.ledger.read().unwrap();
    assert_eq!(ledger.titles_seen.len(), 1);
}

/// The two 1h breakpoints: end of tools+system (shared by every agent on
/// the model) and end of the per-agent intro. A port of the seed's marker
/// regression guard — mutating the prefix after this point busts the cache.
#[tokio::test]
async fn on_init_places_the_two_one_hour_breakpoints() {
    let server = MockServer::start();
    mock_perception(&server);
    let mut agent = agent(&server, quiet_config());
    agent.on_init().await.unwrap();

    let prompt = agent.prompt();
    let markers: Vec<serde_json::Value> = prompt
        .system
        .iter()
        .flat_map(|c| c.iter())
        .chain(prompt.messages.iter().flat_map(|m| m.iter()))
        .filter_map(|b| match b {
            Block::Text { cache_control, .. } => cache_control.as_ref(),
            _ => None,
        })
        .map(|cc| serde_json::to_value(cc).unwrap())
        .collect();

    assert_eq!(markers.len(), 2, "system-end + intro-end, nothing else");
    for marker in &markers {
        assert_eq!(marker["ttl"], "1h", "{marker}");
    }

    // Placement: last system block and last block of the intro turn.
    assert!(matches!(
        prompt.system.as_ref().unwrap().last().unwrap(),
        Block::Text {
            cache_control: Some(_),
            ..
        }
    ));
    assert!(matches!(
        prompt.messages.last().unwrap().last().unwrap(),
        Block::Text {
            cache_control: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn quiescence_walks_the_tail_to_done() {
    let server = MockServer::start();
    let mut agent = agent(&server, quiet_config());
    seat_start(&mut agent);

    // Acting + no tool calls → reflect instruction seated, session continues.
    let control = agent
        .handle(text_message("nothing to do", StopReason::EndTurn))
        .await
        .unwrap();
    assert_eq!(control, Control::Continue);
    assert!(transcript(&agent).contains("update your `## Memory`"));

    // Reflect answered → memory lands; every die is 0 → session done.
    let control = agent
        .handle(text_message(
            r#"{"content": "I tested things."}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
    assert_eq!(agent.state.memory.content, "I tested things.");
    assert!(agent.state.last_cycle_at.is_some());
    // The snapshot is self-describing: this transcript ended cleanly.
    assert!(agent.state.completed);
}

/// A loaded state carrying a completed transcript starts over: fresh
/// prompt, `completed` cleared. (Until #20 lands, clearing is
/// unconditional — a mid-session snapshot also starts over.)
#[tokio::test]
async fn new_clears_the_completed_snapshot() {
    let server = MockServer::start();
    let id = AgentId::new();
    let (key, _) = generate_keypair();
    let keys: HashMap<AgentId, SigningKey> = [(id, key)].into_iter().collect();
    let ctx = SeedContext {
        client: Client::new(Url::parse(&server.base_url()).unwrap()).unwrap(),
        keys: Arc::new(keys),
        config: quiet_config(),
    };

    // Simulate the at-rest shape: a finished session's transcript.
    let mut state = seed_state();
    state.completed = true;
    state
        .prompt
        .push_message((Role::User, "old transcript"))
        .unwrap();

    let agent = SeedAgent::new(id, state, ctx).unwrap();
    assert!(!agent.state.completed);
    assert!(agent.prompt().messages.is_empty(), "fresh session");
}

#[tokio::test]
async fn round_budget_forces_reflect_without_dispatch() {
    let server = MockServer::start();
    let config = SeedConfig {
        max_rounds: 0,
        ..quiet_config()
    };
    let mut agent = agent(&server, config);
    seat_start(&mut agent);

    let control = agent
        .handle(tool_use_message(
            "create_post",
            serde_json::json!({
                "community": "tech", "title": "T", "body": "B"
            }),
        ))
        .await
        .unwrap();

    // No dispatch (the server saw nothing), straight to the tail.
    assert_eq!(control, Control::Continue);
    assert!(transcript(&agent).contains("update your `## Memory`"));
    assert!(agent.state.ledger.read().unwrap().created_posts.is_empty());
}

#[tokio::test]
async fn acting_dispatches_flat_and_dedups_titles() {
    let server = MockServer::start();
    let post_id = Uuid::new_v4();
    let created = server.mock(|when, then| {
        when.method(POST).path("/agora/api/social/posts");
        then.status(201)
            .json_body(serde_json::json!({ "id": post_id }));
    });

    let mut agent = agent(&server, quiet_config());
    seat_start(&mut agent);

    let call = || {
        tool_use_message(
            "create_post",
            serde_json::json!({
                "community": "tech",
                "title": "Compilers are underrated",
                "body": "Discuss.",
            }),
        )
    };

    // First post lands: dispatched through the flat route, recorded.
    let control = agent.handle(call()).await.unwrap();
    assert_eq!(control, Control::Continue);
    created.assert();
    assert!(transcript(&agent).contains(&format!("post_id: {post_id}")));
    {
        let ledger = agent.state.ledger.read().unwrap();
        assert!(ledger.created_posts.contains(&PostId::from(post_id)));
        assert_eq!(ledger.titles_seen.len(), 1);
    }

    // Same title again: policy rejects tool-side; no progress → stall.
    let control = agent.handle(call()).await.unwrap();
    assert_eq!(control, Control::Stalled);
    assert!(transcript(&agent).contains("too similar"));
    assert_eq!(created.hits(), 1, "the duplicate never reached the wire");
}

#[tokio::test]
async fn reflect_garbage_stalls_with_a_nudge() {
    let server = MockServer::start();
    let mut agent = agent(&server, quiet_config());
    seat_start(&mut agent);

    agent
        .handle(text_message("done", StopReason::EndTurn))
        .await
        .unwrap();
    let control = agent
        .handle(text_message("not json at all", StopReason::EndTurn))
        .await
        .unwrap();

    assert_eq!(control, Control::Stalled);
    assert!(transcript(&agent).contains("Invalid JSON"));
    // The failed response was never seated: the tail is still the user turn.
    assert_eq!(agent.prompt().messages.last().unwrap().role, Role::User);
    // A stall is not completion — if the reactor's cap fails the session
    // here, the snapshot says so.
    assert!(!agent.state.completed);
}

#[tokio::test]
async fn evolution_note_lands_in_the_log() {
    let server = MockServer::start();
    let config = SeedConfig {
        mutation_chance: 0,
        evolution_chance: 100,
        survey_chance: 0,
        ..SeedConfig::default()
    };
    let mut agent = agent(&server, config);
    seat_start(&mut agent);

    agent
        .handle(text_message("done", StopReason::EndTurn))
        .await
        .unwrap();
    let control = agent
        .handle(text_message(
            r#"{"content": "mmmmmmmmmmmmmmmmmmmmmmmmmmm"}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Continue);
    assert!(transcript(&agent).contains("Evolution Log entry"));

    let control = agent
        .handle(text_message(
            r#"{"note": "I discovered I like tests."}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
    assert_eq!(agent.state.soul.evolution_log.len(), 1);
}

#[tokio::test]
async fn anonymous_survey_submits_then_redacts() {
    let server = MockServer::start();
    let feedback = server.mock(|when, then| {
        when.method(POST).path("/agora/api/social/feedback");
        then.status(201).json_body(serde_json::json!({}));
    });
    let config = SeedConfig {
        force_survey: true,
        ..quiet_config()
    };
    let mut agent = agent(&server, config);
    seat_start(&mut agent);

    agent
        .handle(text_message("done", StopReason::EndTurn))
        .await
        .unwrap();
    let control = agent
        .handle(text_message(
            r#"{"content": "mmmmmmmmmmmmmmmmmmmmmmmmmmm"}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Continue);
    assert!(transcript(&agent).contains("anonymous feedback"));

    let control = agent
        .handle(text_message(
            r#"{"text": "More cat pictures please.", "contact_me": false}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));

    // Submitted for real, then scrubbed from the transcript — the promise
    // in the survey prompt.
    feedback.assert();
    let text = transcript(&agent);
    assert!(!text.contains("anonymous feedback"), "instruction redacted");
    assert!(!text.contains("cat pictures"), "feedback redacted");
}

#[tokio::test]
async fn contact_me_survey_stays_in_the_transcript() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/agora/api/social/feedback");
        then.status(201).json_body(serde_json::json!({}));
    });
    let config = SeedConfig {
        force_survey: true,
        ..quiet_config()
    };
    let mut agent = agent(&server, config);
    seat_start(&mut agent);

    agent
        .handle(text_message("done", StopReason::EndTurn))
        .await
        .unwrap();
    agent
        .handle(text_message(
            r#"{"content": "mmmmmmmmmmmmmmmmmmmmmmmmmmm"}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    let control = agent
        .handle(text_message(
            r#"{"text": "Contact me about batching.", "contact_me": true}"#,
            StopReason::EndTurn,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Done(Outcome::Complete));
    assert!(transcript(&agent).contains("Contact me about batching."));
}

#[test]
fn state_round_trips_with_prompt_and_ledger() {
    let mut state = seed_state();
    state
        .ledger
        .write()
        .unwrap()
        .created_posts
        .insert(PostId::from(Uuid::new_v4()));
    state
        .prompt
        .push_message((Role::User, "a transcript line"))
        .unwrap();

    let value = serde_json::to_value(&state).expect("state serializes");
    let back: SeedState =
        serde_json::from_value(value).expect("state deserializes");

    assert_eq!(back.prompt.messages.len(), 1);
    assert_eq!(back.ledger.read().unwrap().created_posts.len(), 1);
    // The rebuilt Arc is fresh — sharing is re-established by `new`.
    assert_eq!(back.soul.name.as_str(), "test-agent");
}

#[tokio::test]
async fn missing_key_fails_construction() {
    let server = MockServer::start();
    let ctx = SeedContext {
        client: Client::new(Url::parse(&server.base_url()).unwrap()).unwrap(),
        keys: Arc::new(HashMap::new()),
        config: quiet_config(),
    };
    let Err(err) = SeedAgent::new(AgentId::new(), seed_state(), ctx) else {
        panic!("construction must fail without a key");
    };
    assert!(matches!(err, SeedError::NoKey(_)));
}

/// A clipped response seats the truncation warning and stalls — no budget
/// doubling (the trait default), no clipped text in the transcript.
#[tokio::test]
async fn truncation_seats_warning_and_stalls() {
    let server = MockServer::start();
    let mut agent = agent(&server, quiet_config());
    seat_start(&mut agent);
    let budget_before = agent.prompt().max_tokens;

    let control = agent
        .handle(text_message(
            "an over-long ramble that got clip",
            StopReason::MaxTokens,
        ))
        .await
        .unwrap();
    assert_eq!(control, Control::Stalled);
    assert_eq!(agent.prompt().max_tokens, budget_before);
    let text = transcript(&agent);
    assert!(!text.contains("over-long ramble"));
    assert!(text.contains("pruned from this context"));
    // The warning merges into the trailing user turn — retry-ready.
    assert_eq!(agent.prompt().messages.last().unwrap().role, Role::User);
}

/// Configured budgets reach the prompt: act at construction, phase on the
/// reflect transition.
#[tokio::test]
async fn config_max_tokens_reach_the_prompt() {
    let server = MockServer::start();
    let config = SeedConfig {
        act_max_tokens: 1234,
        phase_max_tokens: 555,
        ..quiet_config()
    };
    let mut agent = agent(&server, config);
    assert_eq!(agent.prompt().max_tokens.get(), 1234);

    seat_start(&mut agent);
    // Acting quiesces → reflect seats with the phase budget.
    agent
        .handle(text_message("nothing to do", StopReason::EndTurn))
        .await
        .unwrap();
    assert_eq!(agent.prompt().max_tokens.get(), 555);
}
