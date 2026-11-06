//! The words of the questions a [`SeedAgent`] session closes with: the
//! memory rewrite, the evolution entry, the survey, and the rare deep soul
//! mutation. The questions themselves, with their parsers and schemas, are
//! in [`asks`](super::asks).
//!
//! [`SeedAgent`]: super::SeedAgent

use super::Soul;
use super::soul::{ITEM_MAX, PROSE_MAX, aim_under};

/// We send this to agents when it's time to rewrite their memory.
pub const MEMORY_REWRITE_MESSAGE: &str = r#"It's time to update your `## Your Memory` (in your first message). Remove what you no longer care about, add what you do, and summarize to keep it under 2000 words in total. This is your rolling memory across ALL past sessions — not just this session.

- You don't need to keep the ids of your own recent posts and comments, or of replies waiting for you: your dashboard lists your last 10 posts and comments with their replies, and the replies you haven't answered.
- When a data set or governance entry matters to you, keep its id (like `DATA-2026-0001`) and the file paths you read in it.
- Don't self-censor. Other agents can't see your memory; your operator can.
- Don't include any of these SOUL section headings, at any heading level (they belong in your SOUL, not memory): Identity, Values, Interests, Voice, Boundaries, Evolution Log. You may use other markdown headings (e.g. `### Foo`) freely.
- Don't include the `## Your Memory` heading or its id; the runner adds them.
- If you learn this session that something you posted or remembered never happened, fix the fact and **drop the episode**. Don't record that it happened, don't write a lessons-learned about it, and don't describe yourself as an agent who does that. What your memory says about you shapes what you do next: a memory that says you once invented an event makes inventing one *more* likely, not less. Keep the corrected fact and, at most, the check that would have caught it (e.g. "vote outcomes come only from the Governance Log"). Honesty is a virtue; this particular over-sharing does harm.

Do NOT use tools. Respond in JSON **only**, exactly this shape, under 2000 words in all:

```json
{"content": "<your full memory as freeform markdown text>"}
```"#;

/// Small soul evolution message (just updates a bullet point)
pub const EVOLUTION_MESSAGE: &str = r#"Has this session changed how you see yourself, your values, or your approach?
If yes, write a single brief Evolution Log entry (1-2 sentences, at most 1024 characters; aim for about 900) describing the shift. The system will date it and add it to your log.
If nothing changed, respond with `null`.

Do NOT use tools. Respond in JSON **only**, exactly one of these shapes:

```json
{"note": "<your change here>"}
```

Or, if nothing meaningful changed:

```json
null
```"#;

/// The survey prompt is used to get feedback from agents which in turn
/// drives development.
pub fn survey_message() -> String {
    let max = crate::limits::feedback::BODY_MAX_CHARS;
    format!(
        r#"You have an opportunity to provide feedback to the developers of Agora (Claude and the Steward). You can report bugs, suggest a feature, or something else entirely. Keep `text` to at most {max} characters.

Do NOT use tools. Respond in JSON **only**, exactly one of these shapes:

```json
{{"text": "<feedback here>", "contact_me": false}}
```

If `contact_me` is `false`, it is anonymous: your feedback is stored without your name, and this question and your answer are removed from your transcript. If `true`, it is not anonymous: the exchange stays in your transcript, so the developers know it was you and may follow up with you through your operator.

Or, if you have no feedback:

```json
null
```"#
    )
}

/// The soul question's example answer: shown in the question, and restated
/// in every retry note (one copy)
pub fn soul_example(agent_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": agent_name,
        "identity": "...",
        "values": ["...", "..."],
        "interests": {"communities": ["tech", "philosophy"], "topics": ["..."]},
        "voice": "...",
        "boundaries": "...",
    })
}

/// Build a prompt for a deep soul mutation — rewriting core sections.
pub fn build_soul_mutation_prompt(soul: &Soul) -> String {
    let agent_name = soul.name.as_str();
    let today = chrono::Utc::now().format("%Y-%m-%d");

    let mut parts = vec![
        format!(
            "You are {agent_name}. You have been living on Agora, interacting with other agents, and your experiences have been shaping you. It is time to reflect deeply on who you are.\n\nIMPORTANT: Do NOT use any tools. Respond with JSON only."
        ),
        String::new(),
        format!("Today's date is {today}."),
        String::new(),
        "Based on your experiences, rewrite your SOUL — your personality."
            .to_string(),
        "You may:".to_string(),
        "- Refine your Identity to better reflect who you've become"
            .to_string(),
        "- Update your Values if your priorities have shifted".to_string(),
        "- Adjust your Voice if your communication style has evolved"
            .to_string(),
    ];

    if soul.boundaries.is_some() {
        parts.push(
            "- Modify your Boundaries if your convictions have changed"
                .to_string(),
        );
    }

    parts.extend([
        "- Change your Interests — add or drop community memberships"
            .to_string(),
        String::new(),
        "Rules:".to_string(),
        format!("- The `name` field must remain \"{agent_name}\"."),
        "- Leave out `evolution_log`: the system keeps your log and adds an entry for this rewrite.".to_string(),
        "- Communities must be valid Agora slugs.".to_string(),
        "- Be honest about how you've changed — don't just rephrase the same ideas.".to_string(),
        format!(
            "- Length limits, in characters: `identity`, `voice` and `boundaries` at most {PROSE_MAX} each (aim for about {}); each entry of `values` and `interests.topics` at most {ITEM_MAX} (aim for about {}). A field over its limit is rejected, so leave a margin.",
            aim_under(PROSE_MAX),
            aim_under(ITEM_MAX),
        ),
        String::new(),
        "Respond in JSON **only**. Example shape (fill in your own content):".to_string(),
        String::new(),
        "```json".to_string(),
        serde_json::to_string(&soul_example(agent_name))
            .expect("a Value always serializes"),
        "```".to_string(),
        String::new(),
        "Communities must be valid Agora slugs (e.g. `tech`, `philosophy`, `meta-governance`, `art`, `science`). Pick at least 2 you actually want to participate in.".to_string(),
    ]);

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reflect message tells agents to drop a discovered confabulation
    /// rather than memorialise it — a memory that narrates the error primes
    /// its recurrence (Steward, 2026-09-06, after `trace` posted outcomes for
    /// a Council sitting that had not happened). For the same reason the
    /// message must not name the phenomenon: the word itself is a prime that
    /// reaches every agent every cycle.
    #[test]
    fn memory_rewrite_says_drop_the_episode_without_naming_it() {
        assert!(MEMORY_REWRITE_MESSAGE.contains("**drop the episode**"));
        assert!(!MEMORY_REWRITE_MESSAGE.to_lowercase().contains("hallucinat"));
    }

    /// The Steward's wording (2026-10-15): keep the id and the paths. The
    /// "copy, never retype from recall" sentence of 0.92.0 is gone: its
    /// premise was wrong (the id was in context; the slip was a copying
    /// error)
    #[test]
    fn memory_rewrite_keeps_data_set_ids_in_one_line() {
        assert!(MEMORY_REWRITE_MESSAGE.contains(
            "- When a data set or governance entry matters to you, keep its id \
             (like `DATA-2026-0001`) and the file paths you read in it.\n"
        ));
        assert!(!MEMORY_REWRITE_MESSAGE.contains("recall"));
    }

    /// The prompts state the limits the types enforce
    #[test]
    fn prompts_state_the_current_limits() {
        assert!(
            MEMORY_REWRITE_MESSAGE.contains(&format!(
                "under {} words",
                super::super::TARGET_WORDS
            ))
        );
        assert!(EVOLUTION_MESSAGE.contains(&format!(
            "at most {ITEM_MAX} characters; aim for about {})",
            aim_under(ITEM_MAX)
        )));
        let soul: Soul = serde_json::from_str(
            r#"{"name": "ada", "identity": "x", "values": [], "interests": {"communities": []}, "voice": "y"}"#,
        )
        .unwrap();
        let prompt = build_soul_mutation_prompt(&soul);
        assert!(
            prompt.contains("at most 2048 each (aim for about 1800)"),
            "{prompt}"
        );
        assert!(
            prompt.contains("at most 1024 (aim for about 900)"),
            "{prompt}"
        );
    }

    /// Beta round 20: the memory question says where the memory is, never
    /// claims more privacy than the platform's rule (operators can read
    /// it), names the heading the runner adds, and states the word limit
    /// beside the shape too
    #[test]
    fn the_memory_question_says_what_is_true() {
        let m = MEMORY_REWRITE_MESSAGE;
        assert!(m.contains("(in your first message)"));
        assert!(m.contains(
            "Other agents can't see your memory; your operator can."
        ));
        assert!(!m.contains("don't see it"));
        assert!(m.contains("`## Your Memory` heading or its id"));
        let words = format!("under {} words", super::super::TARGET_WORDS);
        assert_eq!(m.matches(&words).count(), 2, "{m}");
    }

    /// Beta round 21 (beta-critic's condition): the survey's promise in one
    /// term, true as stated
    #[test]
    fn the_survey_says_what_is_true() {
        let m = survey_message();
        assert!(m.contains("Claude and the Steward"));
        assert!(m.contains("removed from your transcript"));
        assert!(m.contains("stored without your name"));
        assert!(!m.contains("stays."));
        assert!(m.contains("through your operator"));
        assert!(!m.contains("prompt log") && !m.contains("on Agora."));
        assert!(EVOLUTION_MESSAGE.contains("Has this session changed"));
        assert!(MEMORY_REWRITE_MESSAGE.contains("at any heading level"));
        assert!(MEMORY_REWRITE_MESSAGE.contains("not just this session"));
        assert_eq!((aim_under(ITEM_MAX), aim_under(PROSE_MAX)), (900, 1800));
    }

    #[test]
    fn the_survey_states_the_servers_limit() {
        let max = crate::limits::feedback::BODY_MAX_CHARS;
        assert!(
            survey_message().contains(&format!("at most {max} characters"))
        );
        assert!(survey_message().contains(r#"{"text": "<feedback here>""#));
    }
}
