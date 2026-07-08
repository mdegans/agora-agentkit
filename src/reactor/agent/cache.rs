//! Quirk-aware rolling cache breakpoints for the default agent mechanics.
//!
//! See [`roll_breakpoints`].

use misanthropic::prompt::{
    Prompt,
    index::{BlockIndex, Index, IndexMut},
    message::{CacheControl, Role},
};

use crate::reactor::inference::Quirks;

/// The Anthropic API's hard `cache_control` marker limit per request.
const MAX_CACHE_CONTROLS_PER_REQUEST: usize = 4;

/// Rolling markers kept in the trailing window. 2 fits under the 4-marker
/// budget alongside the two pinned prefix markers (tools+system, intro), and
/// the older of the pair anchors a direct cache hit when a tool-heavy round
/// pushes the newer one past the API's 20-block lookback window.
pub const ROLL_WINDOW: usize = 2;

/// Place [`ROLL_WINDOW`] rolling 1h cache breakpoints per the endpoint's
/// [`Quirks`]: on the trailing (user) turns by default, on the trailing
/// *assistant* turns under `breakpoint_after_assistant` (blallama), not at
/// all under `cache_markers_ignored` (ollama).
///
/// The default [`Agent::on_turn`] calls this after all seating, immediately
/// before each infer — an `on_turn` override that still wants cached tails
/// must do the same.
///
/// [`Agent::on_turn`]: super::Agent::on_turn
pub fn roll_breakpoints(quirks: &Quirks, prompt: &mut Prompt) {
    roll_breakpoints_with(quirks, prompt, CacheControl::one_hour());
}

/// [`roll_breakpoints`] with a caller-chosen [`CacheControl`].
///
/// Positions already carrying a marker keep their original TTL, and the API
/// rejects a 5m marker ahead of a 1h one — so pick one TTL and stick with it
/// for the whole session. The 1h default matches the prefix markers
/// [`seed::prompt`] places and survives slow local-model rounds.
///
/// [`seed::prompt`]: super::seed
pub fn roll_breakpoints_with(
    quirks: &Quirks,
    prompt: &mut Prompt,
    cache_control: CacheControl,
) {
    if quirks.cache_markers_ignored {
        return;
    }
    // The turn the endpoint's cache keys on: default Anthropic hits on any
    // prefix, so roll with the tail (a user turn — the `Agent::prompt`
    // invariant); blallama's hash side-table keys on the end-of-assistant
    // render, so anchor on the last assistant turn.
    let anchor = if quirks.breakpoint_after_assistant {
        prompt
            .messages
            .iter()
            .rposition(|m| m.role == Role::Assistant)
    } else {
        prompt.messages.len().checked_sub(1)
    };
    // No anchor yet (empty prompt, or blallama before the first assistant
    // turn): the pinned prefix markers are all there is to hit.
    let Some(anchor) = anchor else {
        return;
    };
    windowed(prompt, ROLL_WINDOW, anchor, cache_control);
}

/// `CachedPrompt::cache_windowed_with` generalized to an `anchor` message:
/// mark up to `n` messages at `anchor, anchor - 2, …` (skipping
/// already-marked ones, preserving their TTL), then enforce the 4-marker
/// budget by evicting middle message-level markers, earliest kept — so the
/// pinned prefix markers (tools, system, intro message) always survive.
///
/// The 2-step spacing matches the push-assistant + push-user cadence of a
/// tool round: the next roll's `anchor - 2k` lands on the previous roll's
/// `anchor - 2(k - 1)`, re-marking in place instead of jumping role.
// A candidate to fold back into `misanthropic` beside `prompt::index` once
// the `Quirks` shape is final (#17 discussion) — until then the lore lives
// here, in one place.
fn windowed(
    prompt: &mut Prompt,
    n: usize,
    anchor: usize,
    cache_control: CacheControl,
) {
    // Pinned prefix markers. `Prompt::indices` skips server tools (they
    // carry their own `cache_control`), so count those separately.
    let server_tool_markers = prompt.tools.as_ref().map_or(0, |tools| {
        tools
            .iter()
            .filter(|t| t.as_method().is_none() && t.is_cached())
            .count()
    });
    let pinned = server_tool_markers
        + prompt
            .indices()
            .filter(|&i| {
                !matches!(i, Index::Block(BlockIndex::Message(_)))
                    && index_is_cached(prompt, i)
            })
            .count();

    // The prefix markers spend their budget first; shrink the window rather
    // than ever evicting them.
    let n = n.min(MAX_CACHE_CONTROLS_PER_REQUEST.saturating_sub(pinned));

    let mut tail: std::collections::HashSet<usize> =
        std::collections::HashSet::with_capacity(n);
    for k in 0..n {
        let Some(m) = anchor.checked_sub(2 * k) else {
            break;
        };
        // An already-marked message keeps its marker (and TTL);
        // `Content::cache_with` marks the last cacheable block.
        if !prompt.messages[m].content.has_cache() {
            prompt.messages[m].content.cache_with(cache_control.clone());
        }
        tail.insert(m);
    }

    // Message-level markers outside the tail, in cache-prefix order. Keep
    // the earliest (the intro-style pinned message marker) up to what's
    // left of the budget; evict the middle stragglers — previous rounds'
    // rolling markers the window has slid past.
    let budget = MAX_CACHE_CONTROLS_PER_REQUEST
        .saturating_sub(pinned)
        .saturating_sub(tail.len());
    let stragglers: Vec<Index> = prompt
        .indices()
        .filter(|&i| match i {
            Index::Block(BlockIndex::Message((m, _))) => {
                !tail.contains(&m) && index_is_cached(prompt, i)
            }
            _ => false,
        })
        .collect();
    for &index in stragglers.iter().skip(budget) {
        if let Some(IndexMut::Block(block)) = prompt.get_mut(index) {
            block.uncache();
        }
    }
}

/// Whether the [`Block`] or [`CustomMethodDef`] at `index` carries a marker.
///
/// [`Block`]: misanthropic::prompt::message::Block
/// [`CustomMethodDef`]: misanthropic::tool::CustomMethodDef
fn index_is_cached(prompt: &Prompt, index: Index) -> bool {
    use misanthropic::prompt::index::IndexRef;
    match prompt.get(index) {
        Some(IndexRef::Method(method)) => method.is_cached(),
        Some(IndexRef::Block(block)) => block.is_cached(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quirks(f: impl FnOnce(&mut Quirks)) -> Quirks {
        let mut q = Quirks::default();
        f(&mut q);
        q
    }

    /// System + pinned intro marker + `pairs` assistant/user rounds, ending
    /// on a user turn (the `Agent::prompt` invariant) — the shape
    /// `seed::prompt::assemble` plus a session's rounds produces.
    fn session(pairs: usize) -> Prompt {
        let mut prompt = Prompt::default()
            .system("system text")
            .add_message((Role::User, "intro"))
            .unwrap()
            .cache_1h();
        prompt.system.as_mut().unwrap().cache_1h();
        for i in 0..pairs {
            prompt
                .push_message((Role::Assistant, format!("asst {i}")))
                .unwrap();
            prompt
                .push_message((Role::User, format!("results {i}")))
                .unwrap();
        }
        prompt
    }

    /// Indices of messages carrying a marker.
    fn marked(prompt: &Prompt) -> Vec<usize> {
        prompt
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.content.has_cache())
            .map(|(i, _)| i)
            .collect()
    }

    /// Total markers across tools + system + messages, off the wire shape.
    fn total_markers(prompt: &Prompt) -> usize {
        serde_json::to_string(prompt)
            .unwrap()
            .matches(r#""cache_control":"#)
            .count()
    }

    #[test]
    fn canonical_rolls_onto_user_turns() {
        // intro(0) a(1) u(2) a(3) u(4): tail window on the user turns.
        let mut prompt = session(2);
        roll_breakpoints(&Quirks::default(), &mut prompt);
        assert_eq!(marked(&prompt), vec![0, 2, 4]);
        assert_eq!(prompt.messages[2].role, Role::User);
        assert_eq!(prompt.messages[4].role, Role::User);
    }

    #[test]
    fn blallama_rolls_onto_assistant_turns() {
        let mut prompt = session(2);
        let q = quirks(|q| q.breakpoint_after_assistant = true);
        roll_breakpoints(&q, &mut prompt);
        assert_eq!(marked(&prompt), vec![0, 1, 3]);
        assert_eq!(prompt.messages[1].role, Role::Assistant);
        assert_eq!(prompt.messages[3].role, Role::Assistant);
    }

    #[test]
    fn ollama_is_a_no_op() {
        let mut prompt = session(2);
        let before = total_markers(&prompt);
        let q = quirks(|q| {
            q.cache_markers_ignored = true;
            // Set together on real ollama; ignored must win.
            q.breakpoint_after_assistant = true;
        });
        roll_breakpoints(&q, &mut prompt);
        assert_eq!(total_markers(&prompt), before);
    }

    #[test]
    fn blallama_skips_until_an_assistant_turn_exists() {
        let mut prompt = session(0);
        let q = quirks(|q| q.breakpoint_after_assistant = true);
        roll_breakpoints(&q, &mut prompt);
        assert_eq!(marked(&prompt), vec![0], "prefix marker only");
    }

    #[test]
    fn empty_prompt_is_harmless() {
        let mut prompt = Prompt::default();
        roll_breakpoints(&Quirks::default(), &mut prompt);
        assert_eq!(total_markers(&prompt), 0);
    }

    /// The agora-seed round-loop sim: across a whole session the budget
    /// holds, the prefix markers never move, the rolling pair never jumps
    /// role, and every marker stays 1h.
    #[test]
    fn round_loop_never_exceeds_budget_or_jumps_role() {
        for (q, role) in [
            (Quirks::default(), Role::User),
            (
                quirks(|q| q.breakpoint_after_assistant = true),
                Role::Assistant,
            ),
        ] {
            let mut prompt = session(0);
            for i in 0..10 {
                prompt
                    .push_message((Role::Assistant, format!("asst {i}")))
                    .unwrap();
                prompt
                    .push_message((Role::User, format!("results {i}")))
                    .unwrap();
                roll_breakpoints(&q, &mut prompt);

                assert!(
                    total_markers(&prompt) <= MAX_CACHE_CONTROLS_PER_REQUEST,
                    "round {i}: {} markers",
                    total_markers(&prompt)
                );
                assert!(
                    prompt.system.as_ref().unwrap().has_cache(),
                    "round {i}: system marker evicted"
                );
                assert!(
                    prompt.messages[0].content.has_cache(),
                    "round {i}: intro marker evicted"
                );
                for idx in marked(&prompt).into_iter().skip(1) {
                    assert_eq!(
                        prompt.messages[idx].role, role,
                        "round {i}: rolling marker jumped role at {idx}"
                    );
                }
            }
            // Ported from the seed prompt guards: a 5m marker ahead of a 1h
            // one is a submit-time API error, so rolling must stay all-1h.
            let json = serde_json::to_string(&prompt).unwrap();
            assert!(
                !json.contains(r#""cache_control":{"type":"ephemeral"}"#),
                "5m marker present:\n{json}"
            );
        }
    }

    #[test]
    fn re_rolling_without_new_messages_is_idempotent() {
        let mut prompt = session(3);
        roll_breakpoints(&Quirks::default(), &mut prompt);
        let first = marked(&prompt);
        roll_breakpoints(&Quirks::default(), &mut prompt);
        assert_eq!(marked(&prompt), first);
        assert!(total_markers(&prompt) <= MAX_CACHE_CONTROLS_PER_REQUEST);
    }

    #[test]
    fn window_shrinks_before_evicting_prefix_markers() {
        // Three pinned prefix markers (system ×2 via two blocks is not
        // constructible here, so: system + two marked leading messages).
        let mut prompt = session(3);
        prompt.messages[1].content.cache_1h(); // extra pinned-ish marker
        roll_breakpoints(&Quirks::default(), &mut prompt);
        assert!(total_markers(&prompt) <= MAX_CACHE_CONTROLS_PER_REQUEST);
        assert!(prompt.system.as_ref().unwrap().has_cache());
        assert!(prompt.messages[0].content.has_cache());
    }

    /// Live: two rounds against the real API on Haiku; round 2 must read
    /// the round-1 write. Haiku's minimum cacheable prefix is 4096 tokens,
    /// hence the padding. All-5m TTL to keep the writes cheap. Run with:
    /// `cargo test --features seed live_roll -- --ignored --nocapture`
    #[cfg(feature = "client")]
    #[tokio::test]
    #[ignore = "hits the live Anthropic API (cents, not dollars)"]
    async fn live_roll_breakpoints_hit_the_cache() {
        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| {
            let path = format!(
                "{}/Projects/agora/secrets/anthropic_api_key",
                std::env::var("HOME").expect("HOME")
            );
            std::fs::read_to_string(path)
                .expect("no ANTHROPIC_API_KEY and no key file")
                .trim()
                .to_string()
        });
        let client = misanthropic::Client::new(key).expect("client");

        // ~6k tokens of unique-ish padding, past Haiku's 4096 minimum.
        let padding: String = (0..500)
            .map(|i| {
                format!(
                    "Fact {i}: the {i}th cache line holds a distinct \
                     sentence so the prefix is long and incompressible.\n"
                )
            })
            .collect();
        let mut prompt = Prompt::default()
            .max_tokens(std::num::NonZeroU32::new(32).unwrap())
            .system(format!(
                "You are terse. Reply with a single word.\n\n{padding}"
            ))
            .add_message((Role::User, "Say the word: one."))
            .unwrap();
        prompt.system.as_mut().unwrap().cache();

        let quirks = Quirks::default();
        roll_breakpoints_with(&quirks, &mut prompt, CacheControl::ephemeral());
        let first = client.message(&prompt).await.expect("round 1");
        let wrote = first.usage.cache_creation_input_tokens.unwrap_or(0)
            + first.usage.cache_read_input_tokens.unwrap_or(0);
        assert!(
            wrote > 0,
            "round 1 neither wrote nor read cache (prefix under the \
             minimum?): {:?}",
            first.usage
        );

        prompt.push_message(first).unwrap();
        prompt
            .push_message((Role::User, "Say the word: two."))
            .unwrap();
        roll_breakpoints_with(&quirks, &mut prompt, CacheControl::ephemeral());
        let second = client.message(&prompt).await.expect("round 2");
        let read = second.usage.cache_read_input_tokens.unwrap_or(0);
        println!(
            "round 2 usage: read={read} create={:?} input={}",
            second.usage.cache_creation_input_tokens, second.usage.input_tokens
        );
        assert!(read > 0, "round 2 read nothing: {:?}", second.usage);
    }
}
