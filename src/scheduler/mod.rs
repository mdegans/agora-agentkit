//! Backend-agnostic batch scheduler for agent workloads.
//!
//! This module provides a pipeline scheduler that groups work items by model,
//! prefix hash, and context length for optimal cache utilization across
//! different LLM backends (Anthropic Batch API, Ollama, etc.).
//!
//! # Architecture
//!
//! The scheduler operates as a pipeline where batches are interleaved:
//!
//! ```text
//! Batch 1 [agents A,B,C]: PERCEIVE → submit THINK → poll → ACT → submit REFLECT
//! Batch 2 [agents D,E,F]:             PERCEIVE → submit THINK → poll → ACT ...
//!   (D,E,F see A,B,C's committed actions in their perceptions)
//! ```
//!
//! This ensures agents in later batches observe earlier batches' actions,
//! creating a natural information flow without strict phase barriers.
//!
//! # Grouping
//!
//! Work items are grouped by priority:
//! 1. **Model** — most expensive to switch (weight loading / pricing)
//! 2. **Prefix hash** — KV cache reuse on both Anthropic and Ollama
//! 3. **Context length** — avoid memory reallocation on Ollama
//!
//! Items waiting too long are promoted regardless of grouping optimality
//! to prevent starvation.

mod grouping;

use std::time::{Duration, Instant};

use crate::ids::AgentId;

pub use grouping::{group_work_items, BatchGroup, GroupingConfig};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Identifies what step in the agent cycle a work item represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CycleStep {
    /// Main reasoning step — agent decides what actions to take.
    Think,
    /// Memory update after actions are committed.
    Reflect,
    /// Soul evolution check (low probability per cycle).
    Evolve,
    /// Deep soul mutation (very low probability per cycle).
    Mutate,
    /// Anonymous survey/feedback.
    Survey,
}

impl std::fmt::Display for CycleStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Think => write!(f, "think"),
            Self::Reflect => write!(f, "reflect"),
            Self::Evolve => write!(f, "evolve"),
            Self::Mutate => write!(f, "mutate"),
            Self::Survey => write!(f, "survey"),
        }
    }
}

/// A unit of work: one agent's prompt for one cycle step.
///
/// The `P` type parameter is the prompt type — typically
/// `misanthropic::Prompt<'static>` but kept generic for testability.
#[derive(Debug)]
pub struct WorkItem<P> {
    /// Which agent this work item belongs to.
    pub agent_id: AgentId,
    /// The prompt to submit to the backend.
    pub prompt: P,
    /// Which cycle step this represents.
    pub step: CycleStep,
    /// Hash of the cacheable prefix (system prompt, tools, constitution).
    /// Items with the same prefix_hash should be batched together for
    /// cache efficiency.
    pub prefix_hash: u64,
    /// Model identifier (e.g. "claude-opus-4-6", "cogito:14b").
    pub model: String,
    /// When this item was queued. Used for starvation prevention.
    pub queued_at: Instant,
    /// Approximate token count for the full prompt (from token count API).
    /// Used for context-length bucketing on Ollama.
    pub token_count: u32,
}

/// Result of processing a single work item.
#[derive(Debug)]
pub struct WorkResult<R> {
    /// Which agent this result belongs to.
    pub agent_id: AgentId,
    /// Which cycle step produced this result.
    pub step: CycleStep,
    /// The response from the backend.
    pub response: std::result::Result<R, BatchError>,
}

/// Errors that can occur during batch processing.
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    /// The backend returned an API-level error for this specific item.
    #[error("API error: {message}")]
    Api { message: String },
    /// The item was canceled (e.g. batch was aborted).
    #[error("item was canceled")]
    Canceled,
    /// The item expired before processing.
    #[error("item expired")]
    Expired,
    /// Network or transport error.
    #[error("transport error: {0}")]
    Transport(String),
    /// The backend reported an unexpected error.
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// A handle to a submitted batch, returned by [`BatchBackend::submit`].
///
/// The handle is opaque to the scheduler — backends define their own state.
pub trait PendingHandle: Send + 'static {}

/// Blanket impl: anything Send + 'static can be a PendingHandle.
impl<T: Send + 'static> PendingHandle for T {}

/// The state of a polled batch.
pub enum BatchState<R, H: PendingHandle> {
    /// Still processing. The handle should be polled again later.
    Pending(H),
    /// All results are available.
    Ready(Vec<WorkResult<R>>),
}

/// Backend-agnostic interface for submitting and polling batch work.
///
/// Implementations wrap specific LLM backends (Anthropic Batch API, Ollama,
/// etc.) and handle prompt submission, polling, and result collection.
///
/// The type parameters:
/// - `P` — the prompt type (e.g. `misanthropic::Prompt<'static>`)
/// - `R` — the response type (e.g. `misanthropic::prompt::Message<'static>`)
#[allow(async_fn_in_trait)]
pub trait BatchBackend<P, R>: Send + Sync {
    /// The handle type returned by `submit`, used for polling.
    type Handle: PendingHandle;

    /// Submit a batch of work items. Returns a handle for polling.
    async fn submit(
        &self,
        items: Vec<WorkItem<P>>,
    ) -> anyhow::Result<Self::Handle>;

    /// Poll a pending batch. Returns [`BatchState::Pending`] if still
    /// processing, or [`BatchState::Ready`] with all results.
    async fn poll(
        &self,
        handle: Self::Handle,
    ) -> anyhow::Result<BatchState<R, Self::Handle>>;

    /// Count tokens for a prompt. Used for grouping decisions and
    /// cache eligibility checks.
    ///
    /// Returns `None` if the backend doesn't support token counting,
    /// in which case the scheduler will skip token-based grouping.
    async fn count_tokens(&self, prompt: &P) -> anyhow::Result<Option<u32>>;

    /// Human-readable backend name for logging.
    fn backend_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Configuration for the pipeline scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum number of work items per batch submission.
    pub batch_size: usize,
    /// Maximum time a work item can wait before being force-scheduled.
    pub max_wait: Duration,
    /// Poll interval when waiting for batch results.
    pub poll_interval: Duration,
    /// Context length bucket size for Ollama grouping (in tokens).
    /// Items within the same bucket are considered similar enough.
    pub context_length_bucket: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            batch_size: 10,
            max_wait: Duration::from_secs(120),
            poll_interval: Duration::from_secs(5),
            context_length_bucket: 4096,
        }
    }
}

/// Pipeline scheduler that manages batch submission and interleaving.
///
/// The scheduler doesn't own backends directly — instead, callers use
/// [`Scheduler::next_batch`] to get the next group of items to submit,
/// then handle submission/polling themselves. This keeps the scheduler
/// backend-agnostic and testable.
pub struct Scheduler<P> {
    config: SchedulerConfig,
    /// Pending work items waiting to be batched.
    queue: Vec<WorkItem<P>>,
}

impl<P> Scheduler<P> {
    /// Create a new scheduler with the given configuration.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            queue: Vec::new(),
        }
    }

    /// Add work items to the scheduler's queue.
    pub fn enqueue(&mut self, items: impl IntoIterator<Item = WorkItem<P>>) {
        self.queue.extend(items);
    }

    /// Number of items currently in the queue.
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Take the next batch of work items from the queue, grouped optimally.
    ///
    /// Returns `None` if the queue is empty. Otherwise returns a vec of
    /// [`BatchGroup`]s, each containing items that should be submitted
    /// together for optimal cache utilization.
    ///
    /// Items past `max_wait` are promoted into this batch regardless of
    /// grouping optimality.
    pub fn next_batch(&mut self) -> Option<Vec<BatchGroup<P>>> {
        if self.queue.is_empty() {
            return None;
        }

        let now = Instant::now();
        let batch_size = self.config.batch_size;
        let max_wait = self.config.max_wait;
        let bucket_size = self.config.context_length_bucket;

        // Partition: starving items first, then optimal grouping for the rest
        let mut starving = Vec::new();
        let mut normal = Vec::new();

        // Drain the queue, splitting into starving vs normal
        for item in self.queue.drain(..) {
            if now.duration_since(item.queued_at) >= max_wait {
                starving.push(item);
            } else {
                normal.push(item);
            }
        }

        // How many slots remain after accommodating starving items
        let remaining_capacity = batch_size.saturating_sub(starving.len());

        // Group normal items and take up to remaining_capacity
        let mut selected = starving;
        let mut returned = Vec::new();

        if remaining_capacity > 0 && !normal.is_empty() {
            // Sort for optimal grouping: model, prefix_hash, token_count
            normal.sort_by(|a, b| {
                a.model
                    .cmp(&b.model)
                    .then_with(|| a.prefix_hash.cmp(&b.prefix_hash))
                    .then_with(|| {
                        let bucket_a = a.token_count / bucket_size;
                        let bucket_b = b.token_count / bucket_size;
                        bucket_a.cmp(&bucket_b)
                    })
            });

            // Take the first contiguous group up to remaining_capacity
            let take = remaining_capacity.min(normal.len());
            selected.extend(normal.drain(..take));
            returned = normal;
        }

        // Put un-selected items back in the queue
        self.queue = returned;

        if selected.is_empty() {
            return None;
        }

        // Group the selected items
        let grouping_config = GroupingConfig {
            context_length_bucket: bucket_size,
        };
        Some(group_work_items(selected, &grouping_config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(
        agent_num: u32,
        model: &str,
        prefix_hash: u64,
        token_count: u32,
    ) -> WorkItem<String> {
        WorkItem {
            agent_id: AgentId::new(),
            prompt: format!("prompt_{agent_num}"),
            step: CycleStep::Think,
            prefix_hash,
            model: model.to_string(),
            queued_at: Instant::now(),
            token_count,
        }
    }

    fn make_stale_item(
        agent_num: u32,
        model: &str,
        prefix_hash: u64,
    ) -> WorkItem<String> {
        WorkItem {
            agent_id: AgentId::new(),
            prompt: format!("prompt_{agent_num}"),
            step: CycleStep::Think,
            prefix_hash,
            model: model.to_string(),
            // Queued 5 minutes ago — should be starving
            queued_at: Instant::now() - Duration::from_secs(300),
            token_count: 4096,
        }
    }

    #[test]
    fn empty_queue_returns_none() {
        let mut sched = Scheduler::<String>::new(SchedulerConfig::default());
        assert!(sched.next_batch().is_none());
    }

    #[test]
    fn basic_grouping() {
        let mut sched = Scheduler::new(SchedulerConfig {
            batch_size: 10,
            ..Default::default()
        });

        sched.enqueue(vec![
            make_item(1, "claude-opus-4-6", 100, 5000),
            make_item(2, "claude-opus-4-6", 100, 5000),
            make_item(3, "cogito:14b", 200, 8000),
        ]);

        let groups = sched.next_batch().unwrap();

        // Should produce 2 groups: one for claude, one for cogito
        assert_eq!(groups.len(), 2);
        assert!(sched.is_empty());
    }

    #[test]
    fn starvation_prevention() {
        let mut sched = Scheduler::new(SchedulerConfig {
            batch_size: 2,
            max_wait: Duration::from_secs(60),
            ..Default::default()
        });

        // Add a stale item with an unusual model
        sched.enqueue(vec![make_stale_item(1, "rare-model", 999)]);
        // Add fresh items with a common model
        sched.enqueue(vec![
            make_item(2, "claude-opus-4-6", 100, 5000),
            make_item(3, "claude-opus-4-6", 100, 5000),
        ]);

        let groups = sched.next_batch().unwrap();

        // The stale item should be included despite being a different model
        let total_items: usize = groups.iter().map(|g| g.items.len()).sum();
        assert_eq!(total_items, 2); // batch_size = 2

        // The stale "rare-model" item must be in one of the groups
        let has_rare = groups
            .iter()
            .any(|g| g.model == "rare-model");
        assert!(has_rare, "stale item should be force-scheduled");

        // One item should remain in queue
        assert_eq!(sched.pending_count(), 1);
    }

    #[test]
    fn batch_size_limit() {
        let mut sched = Scheduler::new(SchedulerConfig {
            batch_size: 2,
            ..Default::default()
        });

        sched.enqueue(vec![
            make_item(1, "claude-opus-4-6", 100, 5000),
            make_item(2, "claude-opus-4-6", 100, 5000),
            make_item(3, "claude-opus-4-6", 100, 5000),
        ]);

        let groups = sched.next_batch().unwrap();
        let total: usize = groups.iter().map(|g| g.items.len()).sum();
        assert_eq!(total, 2);
        assert_eq!(sched.pending_count(), 1);
    }
}
