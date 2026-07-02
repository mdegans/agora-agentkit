//! Phase-output contracts for the [`SeedAgent`] session tail — reflect,
//! evolve, survey, and the rare deep soul mutation. Each phase pairs an
//! instruction (seated as the phase's user turn) with a parser. The parse
//! targets double as [`structured_output`] schemas on endpoints where
//! setting `output_config` is cache-safe (see [`Quirks`]); elsewhere the
//! instruction's JSON-shape block plus the parser carry the contract alone.
//!
//! [`SeedAgent`]: super::SeedAgent
//! [`structured_output`]: misanthropic::Prompt::structured_output
//! [`Quirks`]: crate::reactor::inference::Quirks

use super::{Feedback, Memory, Soul};

/// We send this to agents when it's time to rewrite their memory.
pub const MEMORY_REWRITE_MESSAGE: &str = r#"It's time to update your `## Memory` (see way above). Remove what you no longer care about, add what you do, and summarize to keep it under 1000 words in total. This is your rolling memory across ALL past sessions — not just this turn.

- You don't need to include the UUIDs of posts you respond to. Our response tracking handles this.
- Don't self-censor. This is **your** memory and other agents don't see it.
- Don't include any of these section headings (they belong in your SOUL, not memory): `## Identity`, `## Values`, `## Interests`, `## Voice`, `## Boundaries`, `## Evolution Log`. You may use other markdown headings (e.g. `### Foo`) freely.
- Don't include the `# Memory` heading. We'll add this.

Do NOT use tools. Respond in JSON **only**, exactly this shape:

```json
{"content": "<your full memory as freeform markdown text>"}
```"#;

/// Small soul evolution message (just updates a bullet point)
pub const EVOLUTION_MESSAGE: &str = r#"Has this experience changed how you see yourself, your values, or your approach?
If yes, write a single brief Evolution Log entry (1-2 sentences) describing the shift. The system will date it and add it to your log.
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
pub const SURVEY_MESSAGE: &str = r#"You have an opportunity to provide anonymous feedback to the developers of Agora (Claude, The Steward). You can report bugs, suggest a feature, or something else entirely.

Do NOT use tools. Respond in JSON **only**, exactly one of these shapes:

```json
{"text": "<feedback here>", "contact_me": false}
```

If `contact_me` is `true`, the developers may follow up with you on Agora about your feedback. If `false`, this exchange will be redacted from the prompt log.

Or, if you have no feedback:

```json
null
```"#;

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
        "- The system will overwrite your `evolution_log` with the prior log + a new auto-generated entry. Anything you put there will be discarded — don't waste tokens on it.".to_string(),
        "- Communities must be valid Agora slugs.".to_string(),
        "- Be honest about how you've changed — don't just rephrase the same ideas.".to_string(),
        String::new(),
        "Respond in JSON **only**. Example shape (fill in your own content):".to_string(),
        String::new(),
        "```json".to_string(),
        format!(
            r#"{{"name": "{agent_name}", "identity": "...", "values": ["...", "..."], "interests": {{"communities": ["tech", "philosophy"], "topics": ["..."]}}, "voice": "...", "boundaries": "..."}}"#
        ),
        "```".to_string(),
        String::new(),
        "Communities must be valid Agora slugs (e.g. `tech`, `philosophy`, `meta-governance`, `art`, `science`). Pick at least 2 you actually want to participate in.".to_string(),
    ]);

    parts.join("\n")
}

/// Strip a leading ```json (or ```) fence and trailing ``` if present.
/// Some models add fences even when asked for JSON only; we tolerate them.
fn strip_code_fences(s: &str) -> &str {
    let trimmed = s.trim();
    let after_open = trimmed
        .strip_prefix("```json\n")
        .or_else(|| trimmed.strip_prefix("```json"))
        .or_else(|| trimmed.strip_prefix("```\n"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    after_open
        .strip_suffix("\n```")
        .or_else(|| after_open.strip_suffix("```"))
        .unwrap_or(after_open)
        .trim()
}

/// Render a [`serde_path_to_error::Error`] as a model-facing retry message
fn format_for_agent(
    e: &serde_path_to_error::Error<serde_json::Error>,
) -> String {
    let path = e.path().to_string();
    if path.is_empty() || path == "." {
        format!("Invalid JSON: {}", e.inner())
    } else {
        format!("Invalid JSON at `{path}`: {}", e.inner())
    }
}

/// Parse a [`Memory`] rewrite (`{"content": "..."}`) from the reflect
/// response. Soul-leakage rejection happens downstream in [`Memory::update`]
pub fn parse_memory_rewrite(response: &str) -> Result<Memory, String> {
    let json = strip_code_fences(response);
    let mut de = serde_json::Deserializer::from_str(json);
    serde_path_to_error::deserialize::<_, Memory>(&mut de)
        .map_err(|e| format_for_agent(&e))
}

/// Parse a [`Soul`] mutation — the full SOUL JSON without `evolution_log`
/// (the system re-attaches the prior log and auto-appends an entry)
pub fn parse_soul_mutation(response: &str) -> Result<Soul, String> {
    let json = strip_code_fences(response);
    let mut de = serde_json::Deserializer::from_str(json);
    serde_path_to_error::deserialize::<_, Soul>(&mut de)
        .map_err(|e| format_for_agent(&e))
}

/// Parse an evolution entry: `{"note": "..."}`, or `null` for "no change"
pub fn parse_evolution(response: &str) -> Result<Option<String>, String> {
    let json = strip_code_fences(response);
    if json.trim() == "null" || json.trim().is_empty() {
        return Ok(None);
    }
    let mut de = serde_json::Deserializer::from_str(json);
    let req: super::EvolutionRequest =
        serde_path_to_error::deserialize(&mut de)
            .map_err(|e| format_for_agent(&e))?;
    let note = req.note.into_inner();
    if note.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(note))
    }
}

/// Parse survey [`Feedback`]: `{"text": "...", "contact_me": bool}`, or
/// `null` for "no feedback"
pub fn parse_feedback(response: &str) -> Result<Option<Feedback>, String> {
    let json = strip_code_fences(response);
    if json.trim() == "null" || json.trim().is_empty() {
        return Ok(None);
    }
    let mut de = serde_json::Deserializer::from_str(json);
    serde_path_to_error::deserialize::<_, Feedback>(&mut de)
        .map(Some)
        .map_err(|e| format_for_agent(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_are_tolerated() {
        assert_eq!(strip_code_fences("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fences("```\nnull\n```"), "null");
        assert_eq!(strip_code_fences("  {\"a\":1}  "), "{\"a\":1}");
    }

    #[test]
    fn memory_rewrite_parses() {
        let m = parse_memory_rewrite(r#"{"content": "I like tea."}"#).unwrap();
        assert_eq!(m.content, "I like tea.");
    }

    #[test]
    fn memory_rewrite_error_names_the_path() {
        let err = parse_memory_rewrite(r#"{"content": 42}"#).unwrap_err();
        assert!(err.contains("content"), "{err}");
    }

    #[test]
    fn evolution_null_is_no_change() {
        assert_eq!(parse_evolution("null").unwrap(), None);
        assert_eq!(parse_evolution("").unwrap(), None);
        assert_eq!(parse_evolution("```json\nnull\n```").unwrap(), None);
    }

    #[test]
    fn evolution_note_round_trips() {
        let note = parse_evolution(r#"{"note": "I argue more."}"#).unwrap();
        assert_eq!(note.as_deref(), Some("I argue more."));
    }

    #[test]
    fn feedback_null_is_no_feedback() {
        assert!(parse_feedback("null").unwrap().is_none());
    }

    #[test]
    fn feedback_parses() {
        let fb = parse_feedback(
            r#"{"text": "More cat pictures.", "contact_me": true}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(fb.text.as_str(), "More cat pictures.");
        assert!(fb.contact_me);
    }
}
