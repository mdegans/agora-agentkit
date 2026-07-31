//! Content-addressed prompt logging.
//!
//! At [`on_teardown`](super::Agent::on_teardown) — after the drive loop, before
//! the final save — the fully-assembled session prompt is serialized to
//! `{dir}/{first_2_hex}/{sha256_hex}.json`, sharded by the first two hex
//! characters of its SHA-256 hash. Identical prompts collapse to a single
//! file, which happens often thanks to the shared cached prefix, and any
//! session can be replayed deterministically by looking up the hash from the
//! `prompt logged` tracing event.
//!
//! The prompt at teardown is the *entire* session: the reactor never evicts
//! messages, only cache markers ([`cache::roll_breakpoints`]), so nothing is
//! lost between the first turn and the last.
//!
//! ## Privacy — already handled upstream, do not re-add it here
//!
//! An earlier incarnation of this module (the pre-cutover seed runner) popped
//! the anonymous-survey exchange at write time, by re-parsing the final
//! assistant message as [`Feedback`] and checking `contact_me`. **That belongs
//! to history.** [`SeedAgent::handle_phase`] now truncates
//! `state.prompt.messages` back to the pre-survey mark the moment an anonymous
//! response lands, so the redaction is structural: it happens in the live
//! prompt, before persistence and before this module ever sees it.
//!
//! That ordering is strictly safer than the old dump-time filter, which
//! silently logged the exchange in full whenever the parse failed — exactly
//! when a malformed response makes it *most* likely something went wrong. Do
//! not restore the old pop on top of it: the two would compound, and a
//! `contact_me = true` prompt (which the agent asked to have kept, and which
//! can be replayed straight into the chat REPL to continue the interview in
//! the original context) would lose its last two turns.
//!
//! The feedback itself is submitted to the server before any of this, and the
//! server never receives `contact_me` — it is anonymous there by
//! construction. The retained exchange in this log is the sole opt-in signal.
//!
//! [`Feedback`]: super::Feedback
//! [`SeedAgent::handle_phase`]: super::SeedAgent
//! [`cache::roll_breakpoints`]: crate::reactor::agent::cache::roll_breakpoints

use std::path::{Path, PathBuf};

use misanthropic::Prompt;
use sha2::Digest;

/// Why a prompt could not be logged. Never fatal to a session — the caller
/// warns and moves on.
#[derive(Debug, thiserror::Error)]
pub enum PromptLogError {
    #[error("serializing prompt: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("writing {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl PromptLogError {
    fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}

/// Serialize `prompt` to a content-addressed JSON file under `dir` and
/// return `(path, sha256_hex)`.
///
/// The hash is taken over the exact bytes written, so the filename is an
/// honest digest of the file's contents — and stays compatible with the
/// historical dumps written by the pre-cutover runner, which hashed the same
/// pretty-printed encoding.
///
/// Writing is skipped when the file already exists: the content is addressed
/// by its own hash, so a collision *is* a duplicate. The path is still
/// returned, so callers cannot tell (and should not care) whether this
/// session's prompt was the one that created it.
pub async fn save(
    prompt: &Prompt,
    dir: impl AsRef<Path>,
) -> Result<(PathBuf, String), PromptLogError> {
    let json = serde_json::to_vec_pretty(prompt)?;
    let hash = hex::encode(sha2::Sha256::digest(&json));

    let dir = dir.as_ref().join(&hash[..2]);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(PromptLogError::io(&dir))?;

    // An `Err` here (unreadable parent) is a real problem, but falling
    // through lets the write below surface it with a better message than a
    // bare `try_exists` failure would.
    let path = dir.join(format!("{hash}.json"));
    if let Ok(true) = tokio::fs::try_exists(&path).await {
        return Ok((path, hash));
    }

    tokio::fs::write(&path, &json)
        .await
        .map_err(PromptLogError::io(&path))?;

    Ok((path, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use misanthropic::prompt::Message;
    use misanthropic::prompt::message::{Content, Role};

    fn prompt(text: &str) -> Prompt {
        Prompt {
            messages: vec![Message {
                role: Role::User,
                content: Content::text(text.to_owned()),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn writes_sharded_by_hash_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = save(&prompt("hello"), dir.path()).await.unwrap();

        assert!(path.exists());
        assert_eq!(path.file_name().unwrap(), format!("{hash}.json").as_str());
        assert_eq!(path.parent().unwrap().file_name().unwrap(), &hash[..2]);
    }

    #[tokio::test]
    async fn filename_is_the_digest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = save(&prompt("hello"), dir.path()).await.unwrap();

        let written = tokio::fs::read(&path).await.unwrap();
        assert_eq!(hex::encode(sha2::Sha256::digest(&written)), hash);
    }

    #[tokio::test]
    async fn identical_prompts_collapse_to_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = save(&prompt("hello"), dir.path()).await.unwrap();
        let (second, _) = save(&prompt("hello"), dir.path()).await.unwrap();

        assert_eq!(first, second);
        let shard = std::fs::read_dir(first.parent().unwrap()).unwrap();
        assert_eq!(shard.count(), 1);
    }

    #[tokio::test]
    async fn differing_prompts_get_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = save(&prompt("hello"), dir.path()).await.unwrap();
        let (second, _) = save(&prompt("goodbye"), dir.path()).await.unwrap();

        assert_ne!(first, second);
        assert!(first.exists() && second.exists());
    }

    #[tokio::test]
    async fn creates_missing_intermediate_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("logs").join("prompts");
        let (path, _) = save(&prompt("hello"), &nested).await.unwrap();

        assert!(path.starts_with(&nested));
        assert!(path.exists());
    }
}
