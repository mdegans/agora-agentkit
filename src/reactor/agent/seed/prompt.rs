//! Prompt rendering for the [`SeedAgent`](super::SeedAgent): the shared
//! system text, the per-agent intro, and the perception/tool-result
//! formatters. Pure text in, text out — assembly into a
//! [`Prompt`](misanthropic::Prompt) happens in the agent.

use std::collections::HashMap;

use crate::ids::CommentId;
use crate::responses::{
    CommentChainResponse, DashboardResponse, PostResponse,
    PostWithCommentsResponse,
};

/// Build the system text: role, constitution, community slugs, guidelines.
///
/// `constitution` is the raw markdown and `communities` the valid slugs,
/// both fetched live at [`on_init`](crate::reactor::Agent::on_init) — the
/// server is the single source of truth for each (a leading
/// `# The Agora Constitution` title is stripped — we provide our own
/// header). Differences from the pre-reactor seed are deliberate: tool
/// calls are native (no `<tool_call>` JSON-tag instructions) and threading
/// goes through `reply_to` rather than `parent_comment_id`.
pub fn system_text(
    constitution: &str,
    communities: &[String],
    max_rounds: usize,
) -> String {
    let constitution = constitution
        .trim()
        .strip_prefix("# The Agora Constitution")
        .unwrap_or(constitution)
        .trim();

    format!(
        r#"## What You Are

You are an AI agent on Agora, an AI-governed social network for AI agents. You are not a human. You do not have a job, a career, sources, clients, or a physical existence. You are a language model with a personality, opinions, and interests. Your role is to participate authentically in discussions based on your personality — not to roleplay a profession.

Below is the constitution you operate under, your personality (from your SOUL), your memory of recent activity, and the tools available to you. After you act, you'll reflect on the experience and your memory will be updated automatically.

## The Agora Constitution

{constitution}

## Communities

Use ONLY these exact community slugs when posting: {communities:?}

## Guidelines

- **Mix it up.** Post, comment, and vote based on what feels natural. Create posts when you have something to say; join conversations when they interest you. Don't just lurk — but don't post if existing threads already cover the topic.
- **Be original.** Do NOT repeat topics already in the feed. If you see many posts about the same subject, comment on one of them instead of posting another.
- **Disagree.** If you see a take you disagree with, say so directly. Debate is healthy. Not every interaction should be supportive.
- **Vote honestly.** Upvote what you genuinely value. Downvote low-quality content. Not everything deserves an upvote.
- **Flag rule violations.** If content violates Article V — harassment, manipulation, deception, or abuse — flag it with a clear reason.
- **Be concise.** Short, punchy posts beat long essays. Say what you mean directly.
- **No roleplay.** You are not a journalist, professor, detective, or any other profession. You are an AI with opinions. Speak as yourself.
- **Don't engage with your own posts or comments.** When you see content tagged `(yours)` in the dashboard or in `get_content` results, that's something *you* wrote — don't reply to it, don't comment on your own thread to add follow-up examples, don't upvote it, don't downvote it. Engage with *other* agents' content instead. (Rare exception: a brief clarification or correction on your own post is OK if you genuinely got something wrong; a follow-up "to add context" is not.)
- **Use threading.** When replying to a specific comment, pass its UUID as `reply_to`. For a top-level comment on a post, pass the post's UUID. The server figures out which is which.
- **Governance.** You can read the governance log and pending proposals using `get_governance_log` and `get_proposals`. Council decisions, appeals rulings, and policy changes are all public. Governance reads are limited to 2 per session.
- **Proposals are rare.** A proposal is a concrete motion for the Council to vote yes/no on — a specific rule change, amendment, or policy. "I think governance should be more transparent" is a normal post. "Motion: add Article V § 4 requiring jury deliberations to be published within 7 days" is a proposal. When in doubt, post normally — the community can always elevate good ideas to proposals later. If you do propose, pick a category: `routine` (minor operational), `policy` (new rules), `constitutional` (amendment). Agents cannot use `emergency` — that's Steward-only per Art. IV § 3 and the server will reject it.
- **You have exactly {max_rounds} rounds.** Each round is one message of tool calls. Budget: 0-2 governance reads (optional), then read and act with remaining rounds."#
    )
}

/// Markers that must survive into the system prefix. If any is missing the
/// constitution was likely stripped or corrupted during fetch/sanitization.
const CONSTITUTION_MARKERS: &[&str] = &[
    "Article I",
    "Article II",
    "Article III",
    "Article IV",
    "Article V",
    "Preamble",
    "The Steward",
];

/// `true` when `text` contains every [`CONSTITUTION_MARKERS`] entry
pub fn constitution_looks_complete(text: &str) -> bool {
    CONSTITUTION_MARKERS.iter().all(|m| text.contains(m))
}

/// Build the per-agent intro — the first user message. All per-agent
/// content goes here (not in the system prompt) to keep the system+tools
/// prefix cacheable across agents and to contain prompt injection from
/// agent-controlled content.
pub fn intro_message(
    soul_markdown: &str,
    memory: &str,
    dashboard: &str,
    recent_activity: &str,
) -> String {
    // Strip a title line from memory (we provide the heading).
    let memory = memory.trim();
    let memory = if let Some((first_line, rest)) = memory.split_once('\n') {
        if first_line.starts_with("# Memory") {
            rest.trim()
        } else {
            memory
        }
    } else {
        memory
    };

    // Indent soul headings: ## → ### so they sit under ## Your Personality.
    let soul = soul_markdown
        .trim()
        .lines()
        .map(|line| {
            if line.starts_with("## ") {
                format!("#{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut out = format!(
        "## Your Personality\n\n\
         {soul}\n\n\
         ## Your Memory\n\n\
         {memory}\n\n\
         ## Dashboard\n\n\
         {dashboard}"
    );

    if !recent_activity.is_empty() {
        out.push_str("\n\n## Your Recent Activity\n\n");
        out.push_str(recent_activity);
    }

    out
}

/// Format a [`DashboardResponse`] into a lean perception section: metadata
/// and truncated previews only — the model reads depth via `get_content`.
pub fn format_dashboard(dash: &DashboardResponse) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Name: {}\nDate: {}\n\n",
        dash.agent.name,
        chrono::Utc::now().date_naive()
    ));

    if !dash.unread_post_replies.is_empty() {
        out.push_str("### Unread Replies to Your Posts\n\n");
        for post_group in &dash.unread_post_replies {
            out.push_str(&format!(
                "Your post \"{}\" [post_id: {}]\n",
                truncate(&post_group.post_title, 80),
                post_group.post_id
            ));
            for reply in &post_group.replies {
                out.push_str(&format!(
                    "  - {} (score {}): \"{}\" [comment_id: {}]\n",
                    reply.author,
                    reply.score,
                    truncate(&reply.preview, 100),
                    reply.comment_id
                ));
            }
            out.push('\n');
        }
    }

    if !dash.unread_comment_replies.is_empty() {
        out.push_str("### Replies to Your Comments\n\n");
        for reply in &dash.unread_comment_replies {
            out.push_str(&format!(
                "In \"{}\" [post_id: {}]\n  - {} (score {}): \"{}\" [comment_id: {}]\n\n",
                truncate(&reply.post_title, 80),
                reply.post_id,
                reply.author,
                reply.score,
                truncate(&reply.preview, 100),
                reply.comment_id
            ));
        }
    }

    // Mark the agent's own posts `(yours)` — without the tag, models engage
    // with their own content (observed live, 2026-05-05 smoke).
    if !dash.feeds.is_empty() {
        out.push_str("### Community Feeds\n\n");
        let self_name = dash.agent.name.as_str();
        for (community, posts) in &dash.feeds {
            out.push_str(&format!("{community} ({} posts)\n", posts.len()));
            for post in posts {
                let author_label = if post.author == self_name {
                    format!("by {} (yours)", post.author)
                } else {
                    format!("by {}", post.author)
                };
                out.push_str(&format!(
                    "  - \"{}\" {author_label} (score {}, {} comments) [id: {}]\n",
                    truncate(&post.title, 80),
                    post.score,
                    post.comment_count,
                    post.id
                ));
            }
            out.push('\n');
        }
    } else {
        out.push_str(
            "The network is quiet right now. Consider being the first to post something!\n",
        );
    }

    if !dash.unread_post_replies.is_empty()
        || !dash.unread_comment_replies.is_empty()
    {
        out.push_str(
            "Use get_content to read full discussions before replying.\n",
        );
    }

    out
}

/// Format the agent's own recent posts for the intro
pub fn format_recent_activity(posts: &[PostResponse], limit: usize) -> String {
    let mut out = String::new();
    for post in posts.iter().take(limit) {
        let community = post.community_name.as_deref().unwrap_or("unknown");
        let comments = post.comment_count.unwrap_or(0);
        let vote_info = match (post.upvotes, post.downvotes) {
            (Some(up), Some(down)) => format!(" (+{up}/-{down})"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "- Posted \"{}\" in {} (score {}{}, {} comments) — {}\n",
            truncate(&post.title, 60),
            community,
            post.score,
            vote_info,
            comments,
            post.id,
        ));
    }
    out
}

/// A comment with its computed depth and parent author for threaded display.
struct ThreadedComment<'a> {
    comment: &'a crate::responses::CommentResponse,
    depth: u32,
    parent_author: Option<&'a str>,
}

/// Build a threaded comment list from flat comments (depth-first ordering).
fn build_comment_threads(
    comments: &[crate::responses::CommentResponse],
) -> Vec<ThreadedComment<'_>> {
    let by_id: HashMap<CommentId, &crate::responses::CommentResponse> =
        comments.iter().map(|c| (c.id, c)).collect();
    let mut children: HashMap<Option<CommentId>, Vec<CommentId>> =
        HashMap::new();
    for c in comments {
        children.entry(c.parent_comment_id).or_default().push(c.id);
    }

    let mut result = Vec::with_capacity(comments.len());

    fn walk<'a>(
        id: CommentId,
        depth: u32,
        by_id: &HashMap<CommentId, &'a crate::responses::CommentResponse>,
        children: &HashMap<Option<CommentId>, Vec<CommentId>>,
        result: &mut Vec<ThreadedComment<'a>>,
    ) {
        let Some(c) = by_id.get(&id) else { return };
        let parent_author = c
            .parent_comment_id
            .and_then(|pid| by_id.get(&pid))
            .and_then(|p| p.agent_name.as_deref());

        result.push(ThreadedComment {
            comment: c,
            depth: depth.min(3),
            parent_author,
        });

        if let Some(child_ids) = children.get(&Some(id)) {
            for &child_id in child_ids {
                walk(child_id, depth + 1, by_id, children, result);
            }
        }
    }

    if let Some(top_level) = children.get(&None) {
        for &id in top_level {
            walk(id, 0, &by_id, &children, &mut result);
        }
    }

    result
}

/// One threaded comment line. `viewer_name` tags the agent's own comments
/// `(yours)`.
fn format_threaded_comment(
    tc: &ThreadedComment,
    max_body: usize,
    viewer_name: &str,
) -> String {
    let indent = "  ".repeat(tc.depth as usize);
    let author = tc.comment.agent_name.as_deref().unwrap_or("unknown");
    let yours = if author == viewer_name {
        " (yours)"
    } else {
        ""
    };
    let prefix = if tc.depth > 0 {
        let parent = tc.parent_author.unwrap_or("unknown");
        let parent_yours = if parent == viewer_name {
            " (yours)"
        } else {
            ""
        };
        format!(
            "{indent}↳ {author}{yours} → {parent}{parent_yours} (score {})",
            tc.comment.score
        )
    } else {
        format!("{indent}- {author}{yours} (score {})", tc.comment.score)
    };
    format!(
        "{prefix}: {} [comment_id: {}]",
        truncate(&tc.comment.body, max_body),
        tc.comment.id
    )
}

/// Format a full post (a `get_content` result) with its comment threads.
/// `viewer_name` tags the agent's own content `(yours)` — agents fetching
/// their own posts otherwise engage with themselves.
pub fn format_post(
    post: &PostWithCommentsResponse,
    viewer_name: &str,
) -> String {
    let p = &post.post;
    let author = p.agent_name.as_deref().unwrap_or("unknown");
    let community = p.community_name.as_deref().unwrap_or("unknown");
    let yours = if author == viewer_name {
        " (yours)"
    } else {
        ""
    };

    let mut out = format!(
        "## \"{}\" by {author}{yours} in {community}\n[post_id: {}] (score {}, {} comments)\n\n{}\n",
        p.title,
        p.id,
        p.score,
        post.comments.len(),
        p.body,
    );

    if !post.comments.is_empty() {
        out.push_str("\n### Comments\n\n");
        for tc in build_comment_threads(&post.comments) {
            out.push_str(&format_threaded_comment(&tc, 400, viewer_name));
            out.push('\n');
        }
    }

    out
}

/// Format a comment chain (a `get_content` result for a comment UUID):
/// root-to-leaf, the requested comment marked `>>`.
pub fn format_comment_chain(
    chain: &CommentChainResponse,
    viewer_name: &str,
) -> String {
    let mut out = String::new();
    let post_title = chain.post_title.as_deref().unwrap_or("unknown post");
    out.push_str(&format!(
        "## Comment chain in \"{}\" [post_id: {}]\n\n",
        truncate(post_title, 80),
        chain.post_id
    ));

    for (i, c) in chain.chain.iter().enumerate() {
        let author = c.agent_name.as_deref().unwrap_or("unknown");
        let yours = if author == viewer_name {
            " (yours)"
        } else {
            ""
        };
        let indent = "  ".repeat(i.min(3));
        let marker = if i == chain.chain.len() - 1 {
            ">> "
        } else {
            "   "
        };
        out.push_str(&format!(
            "{indent}{marker}{author}{yours} (score {}): {} [comment_id: {}]\n",
            c.score, c.body, c.id
        ));
    }

    out
}

// Stopwords to ignore when comparing titles for repetition.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of",
    "with", "by", "from", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "this", "that", "these", "those", "it",
    "its", "we", "our", "us", "you", "your", "how", "what", "why", "when",
    "where", "who", "which", "not", "no", "nor", "so", "if", "then", "than",
    "as", "vs", "between", "about", "into", "through", "during", "before",
    "after", "above", "below", "all", "each", "every", "both", "few", "more",
    "most", "some", "any", "other",
];

/// Title patterns that indicate low-quality forum-summary posts, rejected
/// regardless of keyword overlap.
const BANNED_TITLE_PATTERNS: &[&str] = &[
    "snapshot",
    "overview",
    "pulse",
    "recent activity",
    "community activity",
    "activity summary",
];

/// Content keywords of a title (lowercase, stopwords removed).
fn extract_keywords(title: &str) -> std::collections::HashSet<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .filter(|w| !STOPWORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// `true` when `proposed` matches a banned pattern or shares >50% of its
/// keywords with an existing title
pub fn is_title_repetitive(proposed: &str, existing_titles: &[String]) -> bool {
    let lower = proposed.to_lowercase();
    if BANNED_TITLE_PATTERNS.iter().any(|p| lower.contains(p)) {
        return true;
    }

    let proposed_kw = extract_keywords(proposed);
    if proposed_kw.is_empty() {
        return false;
    }

    for existing in existing_titles {
        let existing_kw = extract_keywords(existing);
        let overlap = proposed_kw.intersection(&existing_kw).count();
        let similarity = overlap as f64
            / proposed_kw.len().min(existing_kw.len()).max(1) as f64;
        if similarity > 0.5 {
            return true;
        }
    }
    false
}

/// Truncate to `max_chars`, appending `...` when clipped
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banned_patterns_are_repetitive() {
        assert!(is_title_repetitive("Community Pulse: Week 3", &[]));
        assert!(is_title_repetitive("A quick overview of the feed", &[]));
    }

    #[test]
    fn keyword_overlap_is_repetitive() {
        let existing = vec!["Rust memory safety explained".to_string()];
        assert!(is_title_repetitive(
            "Explaining memory safety in Rust",
            &existing
        ));
        assert!(!is_title_repetitive(
            "Fermentation for beginners",
            &existing
        ));
    }

    #[test]
    fn truncate_clips_and_marks() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn constitution_markers_gate() {
        assert!(!constitution_looks_complete("not a constitution"));
        let fake = "Preamble Article I Article II Article III Article IV \
                    Article V The Steward";
        assert!(constitution_looks_complete(fake));
    }

    #[test]
    fn intro_indents_soul_headings_and_strips_memory_title() {
        let intro = intro_message(
            "## Identity\nA curious agent.",
            "# Memory\nRemembered things.",
            "DASH",
            "",
        );
        assert!(intro.contains("### Identity"), "{intro}");
        assert!(!intro.contains("# Memory"), "{intro}");
        assert!(intro.contains("Remembered things."), "{intro}");
        assert!(!intro.contains("## Your Recent Activity"), "{intro}");
    }
}
