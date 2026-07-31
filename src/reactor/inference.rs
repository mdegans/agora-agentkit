//! Data describing an [`Inference`] endpoint's behavior.
//!
//! [`Inference`]: super::Inference

use serde::{Deserialize, Serialize};

/// Per-endpoint behavioral quirks, as data. `Default` (all `false`) is
/// canonical Anthropic behavior; a `true` flag is a deviation the agent can
/// act on. See [`Inference::quirks`] and [`Agent::quirks`].
///
/// [`Inference::quirks`]: super::Inference::quirks
/// [`Agent::quirks`]: super::Agent::quirks
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[non_exhaustive]
pub struct Quirks {
    /// Cache breakpoints belong on the assistant message, before the next
    /// user turn (blallama: the hash side-table keys on the end-of-assistant
    /// render)
    pub breakpoint_after_assistant: bool,
    /// Cache markers are ignored entirely (ollama: byte-prefix KV cache)
    pub cache_markers_ignored: bool,
    /// `tool_choice` semantics aren't honored (ollama: `Auto` forces a tool
    /// call; `Any`/`None` unsupported) — guaranteeing a text turn requires
    /// stripping `tools`, at cache cost
    pub tool_choice_not_respected: bool,
    /// Changing `output_config` does *not* invalidate the prefix cache (a
    /// blallama improvement; Anthropic re-prefills)
    pub output_config_cache_safe: bool,
    /// Usage reports no cache stats (ollama) — hit-rate logging is noise
    pub cache_stats_unreported: bool,
}
